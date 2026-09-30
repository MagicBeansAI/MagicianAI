//! `CodeChangeProposal` — approvable shadow-workspace patch output.
//!
//! Pi-backed coding sessions should mutate only a per-task shadow
//! workspace. When the session completes, Magician computes a unified
//! diff between the real workspace and that shadow workspace, stores it
//! as a `CodeChangeProposal`, and emits `diff_approval` with the
//! proposal id as the HITL correlation id. Apply writes the patch to the
//! real workspace through Magician; reject leaves the real workspace
//! untouched.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::diff::diff_stats;
use super::snapshot::{read_text_bounded, SnapshotId, SnapshotStore};
use super::transaction::{
    acquire_record_decision_lock, DecisionConflict, DecisionTransition, DiffApprovalFile,
    RecordDecisionLock, ReviewRevisionConflict, TransactionScope,
};
use crate::magician_v2::artifact_v2::io::write_bytes_durably_sync;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CodeChangeProposalId(String);

impl CodeChangeProposalId {
    pub fn new() -> Self {
        Self(format!("ccp-{}", Uuid::new_v4()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parse an operator-supplied proposal id before using it as a
    /// filename under `<scope>/code_change_proposals/`.
    pub fn parse(raw: &str) -> Result<Self> {
        if raw.is_empty() {
            return Err(anyhow!("code-change proposal id is empty"));
        }
        if raw.len() > 128 {
            return Err(anyhow!(
                "code-change proposal id is {} chars; max 128",
                raw.len()
            ));
        }
        for c in raw.chars() {
            if !matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_') {
                return Err(anyhow!(
                    "code-change proposal id contains disallowed char {:?} (only [A-Za-z0-9_-] allowed)",
                    c
                ));
            }
        }
        Ok(Self(raw.to_string()))
    }
}

impl Default for CodeChangeProposalId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for CodeChangeProposalId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for CodeChangeProposalId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeChangeProposalStatus {
    Pending,
    Applied,
    PartiallyApplied,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TestRunSummary {
    pub command: String,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeChangeProposal {
    pub id: CodeChangeProposalId,
    pub scope: TransactionScope,
    pub summary: String,
    pub patch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apply_root: Option<PathBuf>,
    pub touched_files: Vec<PathBuf>,
    pub files: Vec<DiffApprovalFile>,
    #[serde(default)]
    pub test_evidence: Vec<TestRunSummary>,
    pub source_session_id: String,
    pub status: CodeChangeProposalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decision_claim: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub applied_paths: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped_paths: Vec<PathBuf>,
    /// Run identity, stamped at stage time so the coordinator side can join a
    /// proposal back to the task/child that produced it (the proposal store is the
    /// sole source of truth for `status`, and the parent reconcile path reads it by
    /// `task_id`). Backward-compatible: legacy on-disk proposals (no field) load as
    /// `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
}

/// The cheap first look at a stored proposal: only the fields the store's
/// filters ask about.
///
/// Every other key in the record is skipped by serde rather than allocated —
/// above all `patch` and every `files[].unified_diff`, which together carry
/// the whole unified diff **twice** and routinely reach hundreds of kilobytes.
/// Building a `String` of that to answer "does this record belong to task X"
/// is the cost this shape exists to avoid.
///
/// **Every field is optional, so this shape's requirements are a strict
/// subset of [`CodeChangeProposal`]'s.** That is the entire basis of the
/// pre-filter: any record that deserializes as a full proposal also
/// deserializes here, so a peek answering "not this task" / "not pending" can
/// never be hiding a record the full parse would have accepted. The peek can
/// only ever *over*-admit, and the full parse that follows re-applies the real
/// filter.
///
/// The alternative — searching the raw bytes for the task id — was rejected
/// because it reads the diff TEXT as if it were the field: a patch that merely
/// mentions another run's id matches, and an id serde would escape does not.
/// One of those two errors is silent and drops a card.
#[derive(Deserialize)]
struct ProposalPeek {
    #[serde(default)]
    id: Option<CodeChangeProposalId>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    status: Option<CodeChangeProposalStatus>,
    /// Mirrors the record's private `decision_claim`: `Some` means a terminal
    /// decision is already in flight, so the proposal is no longer waiting on
    /// anyone.
    #[serde(default)]
    decision_claim: Option<String>,
}

/// A proposal reduced to what a *listing* asks about it: identity, ownership,
/// decision state, and how many files it touches.
///
/// Built from a confirmed full parse and kept **instead of** the record, so a
/// caller that needs the answer for every task in a scope holds a few short
/// strings per proposal rather than the scope's diffs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalSummary {
    pub id: CodeChangeProposalId,
    pub task_id: Option<String>,
    pub status: CodeChangeProposalStatus,
    /// `decision_claim.is_some()` — a terminal transition already in flight.
    pub decision_claimed: bool,
    pub file_count: usize,
}

impl From<&CodeChangeProposal> for ProposalSummary {
    fn from(proposal: &CodeChangeProposal) -> Self {
        Self {
            id: proposal.id.clone(),
            task_id: proposal.task_id.clone(),
            status: proposal.status,
            decision_claimed: proposal.decision_claim.is_some(),
            file_count: proposal.files.len(),
        }
    }
}

/// How many proposal-store directory walks have happened since the last reset.
///
/// **Process-global, not thread-local.** It was thread-local, to stop the
/// assertion racing the rest of the test binary — and that silently stopped
/// working the moment a caller moved its scan into `spawn_blocking`. The walk
/// still happened once; it happened on a pool thread, so the test thread's
/// counter read zero and the regression bar reported a saving that was really
/// an unobservable one. A counter that cannot see the thread the work moved to
/// is worse than no counter, because it fails in the direction that looks like
/// success.
///
/// The race the thread-local was avoiding is handled by
/// [`lock_proposal_scan_counter`] instead: the few tests that assert on this
/// hold a guard for their duration.
#[cfg(any(test, feature = "test-fixtures"))]
static DIRECTORY_SCANS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(any(test, feature = "test-fixtures"))]
static SCAN_COUNTER_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Proposal-store directory walks since the last reset. Test-only.
#[cfg(any(test, feature = "test-fixtures"))]
pub fn proposal_directory_scan_count() -> usize {
    DIRECTORY_SCANS.load(std::sync::atomic::Ordering::SeqCst)
}

/// Take exclusive use of the scan counter and zero it. Test-only.
///
/// Hold the returned guard for as long as you intend to count. Poisoning is
/// ignored deliberately — the guard protects a counter, not data, so one
/// panicking test must not fail every later one.
#[cfg(any(test, feature = "test-fixtures"))]
#[must_use = "the counter is only exclusive while the guard is held"]
pub fn lock_proposal_scan_counter() -> std::sync::MutexGuard<'static, ()> {
    let guard = SCAN_COUNTER_GUARD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    DIRECTORY_SCANS.store(0, std::sync::atomic::Ordering::SeqCst);
    guard
}

/// Zero the counter without taking the guard, for a test that measures several
/// calls in sequence. Take [`lock_proposal_scan_counter`] first; this is not
/// re-entrant and re-locking would deadlock.
#[cfg(any(test, feature = "test-fixtures"))]
pub fn reset_proposal_directory_scan_count() {
    DIRECTORY_SCANS.store(0, std::sync::atomic::Ordering::SeqCst);
}

pub struct CodeChangeProposalStore {
    root: PathBuf,
}

impl CodeChangeProposalStore {
    pub fn new(scope_root: impl Into<PathBuf>) -> Self {
        Self {
            root: scope_root.into().join("code_change_proposals"),
        }
    }

    pub fn stage_patch(
        &self,
        scope: TransactionScope,
        summary: impl Into<String>,
        patch: impl Into<String>,
        source_session_id: impl Into<String>,
        test_evidence: Vec<TestRunSummary>,
    ) -> Result<CodeChangeProposal> {
        self.stage_patch_with_apply_root(
            scope,
            summary,
            patch,
            source_session_id,
            test_evidence,
            None,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn stage_patch_with_apply_root(
        &self,
        scope: TransactionScope,
        summary: impl Into<String>,
        patch: impl Into<String>,
        source_session_id: impl Into<String>,
        test_evidence: Vec<TestRunSummary>,
        apply_root: Option<PathBuf>,
        task_id: Option<String>,
        execution_id: Option<String>,
    ) -> Result<CodeChangeProposal> {
        let patch = patch.into();
        let parsed = parse_unified_patch(&patch).context("parse proposal patch")?;
        if parsed.is_empty() {
            return Err(anyhow!(
                "stage code-change proposal: patch did not contain any file hunks"
            ));
        }

        let mut touched = BTreeSet::new();
        let mut files = Vec::with_capacity(parsed.len());
        for file in &parsed {
            let target = file
                .new_path
                .as_ref()
                .or(file.old_path.as_ref())
                .ok_or_else(|| anyhow!("proposal patch file has neither old nor new path"))?;
            touched.insert(target.clone());
            let stats = diff_stats(&file.unified_diff);
            files.push(DiffApprovalFile {
                path: target.to_string_lossy().into_owned(),
                status: file.status_letter().to_string(),
                additions: stats.additions,
                deletions: stats.deletions,
                unified_diff: file.unified_diff.clone(),
            });
        }

        let proposal = CodeChangeProposal {
            id: CodeChangeProposalId::new(),
            scope,
            summary: summary.into(),
            patch,
            apply_root,
            touched_files: touched.into_iter().collect(),
            files,
            test_evidence,
            source_session_id: source_session_id.into(),
            status: CodeChangeProposalStatus::Pending,
            decision_claim: None,
            created_at: Utc::now(),
            resolved_at: None,
            snapshot_id: None,
            applied_paths: Vec::new(),
            skipped_paths: Vec::new(),
            task_id,
            execution_id,
        };
        self.persist(&proposal)?;
        // Trusted-store integrity: bind this staged proposal to the in-process
        // authority so the diff-approval apply takes `apply_root` (+ targets) from
        // process memory, NOT from the shell-writable on-disk JSON. This is the
        // SINGLE choke point every proposal funnels through (the convenience
        // `stage_patch` delegates here, and every production stage site —
        // `coding_engine::stage_shadow_workspace_patch`, `run_coding_task`,
        // `run_project_checks`, `v2_orchestrator` — calls this), so recording here
        // covers EVERY staged proposal. Recorded for every proposal (not just
        // approved out-of-workspace applies) because the decision site fail-closes
        // on an authority miss — so an ordinary in-workspace apply MUST have an
        // entry too, else its own approval would be rejected. The authority is
        // empty after a restart by design (non-terminal records get re-asked, never
        // auto-applied from unverifiable disk).
        //
        // The authority key is `{ CodeChangeProposal, scope.principal,
        // scope.workspace, proposal.id }`. Those three strings are IDENTICAL to the
        // decision site's key: `respond_hitl_handler` derives the proposal store
        // root from `scope_root(request_scope.0, request_scope.1)` and loads by
        // `proposal_id`, so a proposal is only loadable there when its
        // `(principal, workspace)` match — the same pair passed in `scope` here.
        //
        // `content_hash` is over the `patch` — the canonical description of the
        // approved effect (the unified diff whose hunks `apply_proposal`
        // materializes). Computed via `CodeChangeProposal::content_hash` — the SAME
        // method the diff-approval decide site recomputes on the loaded on-disk
        // record and compares against this entry, so a mismatch (JSON `patch`
        // edited out of band between stage and apply) fails the apply closed. It is
        // deterministic and degrades to an empty-input hash on the (unreachable)
        // serialize error rather than failing the stage.
        if let Some(authority) = crate::magician_v2::execution::trusted_store::process_authority() {
            use crate::magician_v2::execution::trusted_store::{
                AuthorityEntry, StoreKind, TrustedRecordKey,
            };
            let _ = authority.record_authenticated_file_edit_stage(
                TrustedRecordKey {
                    kind: StoreKind::CodeChangeProposal,
                    principal: proposal.scope.principal.clone(),
                    workspace: proposal.scope.workspace.clone(),
                    id: proposal.id.as_str().to_string(),
                },
                AuthorityEntry {
                    content_hash: proposal.content_hash(),
                    apply_root: proposal.apply_root.clone(),
                    target_paths: proposal.touched_files.clone(),
                },
                proposal.task_id.clone(),
                proposal.execution_id.clone(),
                proposal.content_hash(),
            );
        }
        Ok(proposal)
    }

    /// One walk of `<scope>/code_change_proposals/`, handing each `.json`
    /// file's bytes to `project`.
    ///
    /// Best-effort throughout: an unreadable file is skipped (never poisons the
    /// scan) and a missing store dir yields an empty list. This is the SINGLE
    /// place the directory is read, which is what makes a scan countable in
    /// tests and what gives callers one thing to hoist out of a per-task loop.
    fn scan_records<T>(&self, mut project: impl FnMut(&[u8]) -> Option<T>) -> Vec<T> {
        // Counted before the `read_dir`, so an absent store still registers the
        // walk: "a listing with no proposals reads the directory once, not
        // once per task" is the regression this proves.
        #[cfg(any(test, feature = "test-fixtures"))]
        DIRECTORY_SCANS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("json"))
            .filter_map(|entry| std::fs::read(entry.path()).ok())
            .filter_map(|bytes| project(&bytes))
            .collect()
    }

    /// Every proposal in this scope stamped with `task_id`. Best-effort: a file
    /// that fails to read/parse is skipped (never poisons the scan), and a missing
    /// store dir yields an empty list. Task-scoped so a concurrent same-scope run
    /// cannot interfere. Used by the parent reconcile (X) + the B14 backstop (Z).
    ///
    /// Ownership is decided off [`ProposalPeek`] **before** the record is
    /// deserialized. Filtering afterwards meant allocating the whole unified
    /// diff — twice, `patch` plus every `files[].unified_diff` — for every
    /// proposal in the scope in order to discard almost all of them on one
    /// short string. The peek's requirements are a strict subset of the full
    /// record's, so it cannot reject something the full parse would accept; the
    /// full parse below re-applies the real filter.
    pub fn list_for_task(&self, task_id: &str) -> Vec<CodeChangeProposal> {
        self.scan_records(|bytes: &[u8]| {
            let peek: ProposalPeek = serde_json::from_slice(bytes).ok()?;
            if peek.task_id.as_deref() != Some(task_id) {
                return None;
            }
            serde_json::from_slice::<CodeChangeProposal>(bytes)
                .ok()
                .filter(|proposal| proposal.task_id.as_deref() == Some(task_id))
        })
    }

    /// Authoritative, resource-bounded task census for lifecycle decisions.
    ///
    /// Unlike [`Self::list_for_task`], this fails closed if any JSON record in
    /// the scope is unreadable or malformed. A caller using absence to advance
    /// or fail a durable workflow must not silently turn a corrupt proposal
    /// into "no proposal". Both directory cardinality and encoded bytes are
    /// bounded before full proposal allocation.
    pub fn try_list_for_task_bounded(
        &self,
        task_id: &str,
        max_records: usize,
        max_record_bytes: usize,
        max_total_bytes: usize,
    ) -> Result<Vec<CodeChangeProposal>> {
        #[cfg(any(test, feature = "test-fixtures"))]
        DIRECTORY_SCANS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "read code-change proposal root {} for strict task census",
                        self.root.display()
                    )
                })
            },
        };
        let mut records_seen = 0usize;
        let mut total_bytes = 0usize;
        let mut proposals = Vec::new();
        for entry in entries {
            let entry = entry.with_context(|| {
                format!(
                    "read code-change proposal directory entry under {}",
                    self.root.display()
                )
            })?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            records_seen = records_seen.saturating_add(1);
            if records_seen > max_records {
                return Err(anyhow!(
                    "strict code-change proposal census exceeded {} records under {}",
                    max_records,
                    self.root.display()
                ));
            }
            let file_type = entry.file_type().with_context(|| {
                format!("inspect code-change proposal record {}", path.display())
            })?;
            if !file_type.is_file() {
                return Err(anyhow!(
                    "strict code-change proposal census rejected non-file record {}",
                    path.display()
                ));
            }
            let file = std::fs::File::open(&path)
                .with_context(|| format!("open code-change proposal {}", path.display()))?;
            let encoded_len = usize::try_from(
                file.metadata()
                    .with_context(|| format!("stat code-change proposal {}", path.display()))?
                    .len(),
            )
            .map_err(|_| anyhow!("code-change proposal {} length overflow", path.display()))?;
            if encoded_len > max_record_bytes {
                return Err(anyhow!(
                    "code-change proposal {} is {} bytes; strict census cap is {}",
                    path.display(),
                    encoded_len,
                    max_record_bytes
                ));
            }
            let read_limit = u64::try_from(max_record_bytes)
                .unwrap_or(u64::MAX)
                .saturating_add(1);
            let mut bytes = Vec::with_capacity(encoded_len);
            file.take(read_limit)
                .read_to_end(&mut bytes)
                .with_context(|| format!("read code-change proposal {}", path.display()))?;
            if bytes.len() > max_record_bytes {
                return Err(anyhow!(
                    "code-change proposal {} grew beyond the {} byte strict census cap",
                    path.display(),
                    max_record_bytes
                ));
            }
            total_bytes = total_bytes
                .checked_add(bytes.len())
                .ok_or_else(|| anyhow!("strict code-change proposal census byte overflow"))?;
            if total_bytes > max_total_bytes {
                return Err(anyhow!(
                    "strict code-change proposal census exceeded {} aggregate bytes under {}",
                    max_total_bytes,
                    self.root.display()
                ));
            }
            let peek: ProposalPeek = serde_json::from_slice(&bytes)
                .with_context(|| format!("parse proposal ownership from {}", path.display()))?;
            if peek.task_id.as_deref() != Some(task_id) {
                continue;
            }
            let proposal: CodeChangeProposal = serde_json::from_slice(&bytes)
                .with_context(|| format!("parse code-change proposal {}", path.display()))?;
            if proposal.task_id.as_deref() != Some(task_id) {
                return Err(anyhow!(
                    "code-change proposal {} changed task ownership during strict census",
                    path.display()
                ));
            }
            proposals.push(proposal);
        }
        Ok(proposals)
    }

    /// Every proposal in this scope whose `apply_root` is `apply_root` (the real repo it
    /// was staged against). Lets a PROJECT-scoped caller (the cockpit `/check` endpoint)
    /// find its own proposals by repo WITHOUT knowing the run's task id — the proposal
    /// carries its task id itself. Same best-effort/skip-on-error semantics as
    /// `list_for_task`.
    ///
    /// No peek pre-filter: this is a cockpit-request path, not a per-card one,
    /// and `apply_root` is not what the listing hot path filters on.
    pub fn list_for_apply_root(&self, apply_root: &std::path::Path) -> Vec<CodeChangeProposal> {
        self.scan_records(|bytes: &[u8]| {
            serde_json::from_slice::<CodeChangeProposal>(bytes)
                .ok()
                .filter(|proposal| proposal.apply_root.as_deref() == Some(apply_root))
        })
    }

    /// Every `Pending` proposal in this scope. Best-effort: a file that fails to
    /// read/parse is skipped (never poisons the scan), and a missing store dir
    /// yields an empty list. Used by the boot re-record (Trusted-Store Integrity
    /// Phase 5) to re-anchor pending proposals into the in-process authority so
    /// their operator approval survives a restart. Same best-effort/skip-on-error
    /// semantics as `list_for_task`.
    pub fn list_pending(&self) -> Vec<CodeChangeProposal> {
        self.scan_records(|bytes: &[u8]| {
            let peek: ProposalPeek = serde_json::from_slice(bytes).ok()?;
            if peek.status != Some(CodeChangeProposalStatus::Pending) {
                return None;
            }
            let proposal: CodeChangeProposal = serde_json::from_slice(bytes).ok()?;
            (matches!(proposal.status, CodeChangeProposalStatus::Pending)
                && proposal.decision_claim.is_none())
            .then_some(proposal)
        })
    }

    /// Summaries of every `Pending` proposal in this scope, from ONE walk of
    /// the directory.
    ///
    /// This is the hoisted form of asking `list_for_task(..)` the same question
    /// once per task: a listing renders M tasks against a store of F files, and
    /// the per-task shape made that M×F reads where this makes it F.
    ///
    /// `Pending` **regardless of `decision_claim`**, because the two callers
    /// want different halves of that — a card still reads "waiting" while a
    /// decision is in flight, an announcement does not — so the claim rides on
    /// the summary and the caller filters.
    ///
    /// Membership is confirmed against the FULL parse, not the peek, so this
    /// set is exactly the one `list_for_task(..).filter(status == Pending)`
    /// produced: a record the full parse rejects is skipped by both.
    pub fn scan_pending_summaries(&self) -> Vec<ProposalSummary> {
        self.scan_records(|bytes: &[u8]| {
            let peek: ProposalPeek = serde_json::from_slice(bytes).ok()?;
            if peek.status != Some(CodeChangeProposalStatus::Pending) {
                return None;
            }
            let proposal: CodeChangeProposal = serde_json::from_slice(bytes).ok()?;
            matches!(proposal.status, CodeChangeProposalStatus::Pending)
                .then(|| ProposalSummary::from(&proposal))
        })
    }

    /// The `Pending` proposal ids stamped with `task_id`, from ONE peek-only
    /// walk of the directory.
    ///
    /// The single-task form of [`Self::scan_pending_summaries`], for the
    /// callers that rebuild **one** card and would otherwise pay for a
    /// scope-wide snapshot to read one row out of it.
    ///
    /// No full parse at all, which is the point: the id comes from the peek
    /// and the two filters are peek fields, so a scope's diffs are never
    /// allocated. `scan_pending_summaries` still needs the full parse because
    /// it reports `file_count`; a card does not ask that.
    ///
    /// Selection matches `scan_pending_summaries(..)` filtered to this task
    /// for every record the store itself wrote. It can differ only for a
    /// record the peek accepts and the full parse rejects — one whose *other*
    /// fields are malformed — where this admits and that skips. Same
    /// over-admission the peek's pre-filter has always had, without the full
    /// parse behind it to narrow the result.
    ///
    /// Sorted, so repeated reads of unchanged state produce an identical list.
    pub fn pending_ids_for_task(&self, task_id: &str) -> Vec<String> {
        let mut ids = self.scan_records(|bytes: &[u8]| {
            let peek: ProposalPeek = serde_json::from_slice(bytes).ok()?;
            if peek.task_id.as_deref() != Some(task_id) {
                return None;
            }
            if peek.status != Some(CodeChangeProposalStatus::Pending) {
                return None;
            }
            peek.id.map(|id| id.as_str().to_string())
        });
        ids.sort();
        ids
    }

    /// Whether this exact proposal is, right now, `Pending` with no terminal
    /// decision in flight.
    ///
    /// One file read, no directory walk, and no diff allocated. Exists for
    /// callers holding a snapshot that may have gone stale and needing to
    /// re-confirm a single row before acting irreversibly on it — the state
    /// this answers is the state a listing snapshot freezes.
    ///
    /// A missing or unreadable record answers `false`: not-known-pending must
    /// never read as pending.
    pub fn is_unclaimed_pending(&self, id: &CodeChangeProposalId) -> bool {
        let Ok(bytes) = std::fs::read(self.proposal_path(id)) else {
            return false;
        };
        let Ok(peek) = serde_json::from_slice::<ProposalPeek>(&bytes) else {
            return false;
        };
        peek.status == Some(CodeChangeProposalStatus::Pending) && peek.decision_claim.is_none()
    }

    pub fn load(&self, id: &CodeChangeProposalId) -> Result<CodeChangeProposal> {
        let path = self.proposal_path(id);
        let bytes = std::fs::read(&path)
            .with_context(|| format!("read code-change proposal {}", path.display()))?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("parse code-change proposal {}", path.display()))
    }

    pub fn exists(&self, id: &CodeChangeProposalId) -> bool {
        self.proposal_path(id).is_file()
    }

    pub fn persist(&self, proposal: &CodeChangeProposal) -> Result<()> {
        std::fs::create_dir_all(&self.root)
            .with_context(|| format!("create code-change proposal root {}", self.root.display()))?;
        let bytes =
            serde_json::to_vec_pretty(proposal).context("serialize code-change proposal")?;
        let path = self.proposal_path(&proposal.id);
        // The shared durable writer, not a fixed `<id>.json.tmp` sibling:
        // persist runs on every state transition of the same proposal id, so
        // that one staging name is shared by overlapping writers and two of
        // them interleaving publish a half-written record over the proposal —
        // which carries the only copy of the patch. The writer also
        // `sync_all`s the record and the proposals directory.
        write_bytes_durably_sync(&path, &bytes)
            .with_context(|| format!("write code-change proposal {}", path.display()))?;
        Ok(())
    }

    pub fn reject(
        &self,
        id: &CodeChangeProposalId,
    ) -> Result<DecisionTransition<CodeChangeProposal>> {
        let _lock = self.acquire_decision_lock(id.as_str())?;
        let mut proposal = self.load(id)?;
        if matches!(proposal.status, CodeChangeProposalStatus::Pending)
            && proposal.decision_claim.is_none()
        {
            proposal.status = CodeChangeProposalStatus::Rejected;
            proposal.resolved_at = Some(Utc::now());
            self.persist(&proposal)?;
            return Ok(DecisionTransition::Transitioned(proposal));
        }
        Ok(DecisionTransition::AlreadyResolved(proposal))
    }

    fn acquire_decision_lock(&self, id: &str) -> Result<RecordDecisionLock> {
        acquire_record_decision_lock(&self.root, id, "code-change proposal")
    }

    fn claim_apply_locked(
        &self,
        id: &CodeChangeProposalId,
        expected_review_revision: Option<blake3::Hash>,
        _lock: &RecordDecisionLock,
    ) -> Result<DecisionTransition<CodeChangeProposal>> {
        let mut proposal = self.load(id)?;
        if expected_review_revision.is_some_and(|expected| proposal.content_hash() != expected) {
            return Err(anyhow::Error::new(ReviewRevisionConflict {
                record_kind: "code-change proposal",
                id: id.to_string(),
            }));
        }
        if !matches!(proposal.status, CodeChangeProposalStatus::Pending)
            || proposal.decision_claim.is_some()
        {
            return Ok(DecisionTransition::AlreadyResolved(proposal));
        }
        proposal.decision_claim = Some(Uuid::new_v4().to_string());
        self.persist(&proposal)?;
        Ok(DecisionTransition::Transitioned(proposal))
    }

    fn reset_pending_locked(
        &self,
        mut proposal: CodeChangeProposal,
        _lock: &RecordDecisionLock,
    ) -> Result<()> {
        proposal.status = CodeChangeProposalStatus::Pending;
        proposal.decision_claim = None;
        proposal.resolved_at = None;
        proposal.snapshot_id = None;
        proposal.applied_paths.clear();
        proposal.skipped_paths.clear();
        self.persist(&proposal)
    }

    fn finish_apply_locked(
        &self,
        id: &CodeChangeProposalId,
        snapshot_id: Option<String>,
        applied_paths: Vec<PathBuf>,
        skipped_paths: Vec<PathBuf>,
        _lock: &RecordDecisionLock,
    ) -> Result<CodeChangeProposal> {
        let mut proposal = self.load(id)?;
        if !matches!(proposal.status, CodeChangeProposalStatus::Pending)
            || proposal.decision_claim.is_none()
        {
            return Err(anyhow!(
                "cannot finish code-change proposal {} apply: status is {:?} and claim_present={} (must be claimed Pending)",
                id,
                proposal.status,
                proposal.decision_claim.is_some()
            ));
        }
        proposal.status = if skipped_paths.is_empty() {
            CodeChangeProposalStatus::Applied
        } else {
            CodeChangeProposalStatus::PartiallyApplied
        };
        proposal.resolved_at = Some(Utc::now());
        proposal.decision_claim = None;
        proposal.snapshot_id = snapshot_id;
        proposal.applied_paths = applied_paths;
        proposal.skipped_paths = skipped_paths;
        self.persist(&proposal)?;
        Ok(proposal)
    }

    pub fn mark_applied(
        &self,
        id: &CodeChangeProposalId,
        snapshot_id: Option<String>,
    ) -> Result<CodeChangeProposal> {
        let _lock = self.acquire_decision_lock(id.as_str())?;
        let mut proposal = self.load(id)?;
        if !matches!(proposal.status, CodeChangeProposalStatus::Pending)
            || proposal.decision_claim.is_some()
        {
            return Err(anyhow!(
                "cannot mark code-change proposal {} as applied: status is {:?} (must be Pending)",
                id,
                proposal.status
            ));
        }
        proposal.status = CodeChangeProposalStatus::Applied;
        proposal.resolved_at = Some(Utc::now());
        proposal.snapshot_id = snapshot_id;
        proposal.applied_paths = proposal.touched_files.clone();
        proposal.skipped_paths.clear();
        self.persist(&proposal)?;
        Ok(proposal)
    }

    pub fn mark_partially_applied(
        &self,
        id: &CodeChangeProposalId,
        snapshot_id: Option<String>,
        applied_paths: Vec<PathBuf>,
        skipped_paths: Vec<PathBuf>,
    ) -> Result<CodeChangeProposal> {
        let _lock = self.acquire_decision_lock(id.as_str())?;
        let mut proposal = self.load(id)?;
        if !matches!(proposal.status, CodeChangeProposalStatus::Pending)
            || proposal.decision_claim.is_some()
        {
            return Err(anyhow!(
                "cannot mark code-change proposal {} as partially applied: status is {:?} (must be Pending)",
                id,
                proposal.status
            ));
        }
        proposal.status = CodeChangeProposalStatus::PartiallyApplied;
        proposal.resolved_at = Some(Utc::now());
        proposal.snapshot_id = snapshot_id;
        proposal.applied_paths = applied_paths;
        proposal.skipped_paths = skipped_paths;
        self.persist(&proposal)?;
        Ok(proposal)
    }

    fn proposal_path(&self, id: &CodeChangeProposalId) -> PathBuf {
        self.root.join(format!("{}.json", id.as_str()))
    }
}

impl CodeChangeProposal {
    pub fn decision_claimed(&self) -> bool {
        self.decision_claim.is_some()
    }

    /// Stable review revision over scope, task/execution provenance, summary,
    /// patch, apply root, displayed files, test evidence, and source session.
    ///
    /// Trusted-store integrity: the stage site records this into the in-process
    /// authority, and the diff-approval decide site recomputes it on the loaded
    /// on-disk record and compares. Any provenance, displayed diff, patch,
    /// destination, or root change therefore fails closed.
    pub fn content_hash(&self) -> blake3::Hash {
        let bytes = serde_json::to_vec(&(
            &self.scope,
            &self.task_id,
            &self.execution_id,
            &self.summary,
            &self.patch,
            &self.apply_root,
            &self.touched_files,
            &self.files,
            &self.test_evidence,
            &self.source_session_id,
        ))
        .unwrap_or_default();
        blake3::hash(&bytes)
    }
}

pub fn apply_proposal(
    proposals: &CodeChangeProposalStore,
    snapshots: &SnapshotStore,
    proposal_id: &CodeChangeProposalId,
    workspace_root: &Path,
) -> Result<()> {
    match apply_proposal_decision(proposals, snapshots, proposal_id, workspace_root)? {
        DecisionTransition::Transitioned(_) => Ok(()),
        DecisionTransition::AlreadyResolved(proposal) => {
            Err(anyhow::Error::new(DecisionConflict {
                record_kind: "code-change proposal",
                id: proposal_id.to_string(),
                status: format!("{:?}", proposal.status),
            }))
        },
    }
}

pub fn apply_proposal_decision(
    proposals: &CodeChangeProposalStore,
    snapshots: &SnapshotStore,
    proposal_id: &CodeChangeProposalId,
    workspace_root: &Path,
) -> Result<DecisionTransition<CodeChangeProposal>> {
    apply_proposal_inner(
        proposals,
        snapshots,
        proposal_id,
        workspace_root,
        None,
        None,
    )
}

pub fn apply_proposal_verified_decision(
    proposals: &CodeChangeProposalStore,
    snapshots: &SnapshotStore,
    proposal_id: &CodeChangeProposalId,
    workspace_root: &Path,
    expected_review_revision: blake3::Hash,
) -> Result<DecisionTransition<CodeChangeProposal>> {
    apply_proposal_inner(
        proposals,
        snapshots,
        proposal_id,
        workspace_root,
        None,
        Some(expected_review_revision),
    )
}

pub fn apply_proposal_selected_paths(
    proposals: &CodeChangeProposalStore,
    snapshots: &SnapshotStore,
    proposal_id: &CodeChangeProposalId,
    workspace_root: &Path,
    selected_paths: &[String],
) -> Result<CodeChangeProposal> {
    match apply_proposal_selected_paths_decision(
        proposals,
        snapshots,
        proposal_id,
        workspace_root,
        selected_paths,
    )? {
        DecisionTransition::Transitioned(proposal) => Ok(proposal),
        DecisionTransition::AlreadyResolved(proposal) => {
            Err(anyhow::Error::new(DecisionConflict {
                record_kind: "code-change proposal",
                id: proposal_id.to_string(),
                status: format!("{:?}", proposal.status),
            }))
        },
    }
}

pub fn apply_proposal_selected_paths_decision(
    proposals: &CodeChangeProposalStore,
    snapshots: &SnapshotStore,
    proposal_id: &CodeChangeProposalId,
    workspace_root: &Path,
    selected_paths: &[String],
) -> Result<DecisionTransition<CodeChangeProposal>> {
    apply_proposal_inner(
        proposals,
        snapshots,
        proposal_id,
        workspace_root,
        Some(selected_paths),
        None,
    )
}

pub fn apply_proposal_selected_paths_verified_decision(
    proposals: &CodeChangeProposalStore,
    snapshots: &SnapshotStore,
    proposal_id: &CodeChangeProposalId,
    workspace_root: &Path,
    selected_paths: &[String],
    expected_review_revision: blake3::Hash,
) -> Result<DecisionTransition<CodeChangeProposal>> {
    apply_proposal_inner(
        proposals,
        snapshots,
        proposal_id,
        workspace_root,
        Some(selected_paths),
        Some(expected_review_revision),
    )
}

fn apply_proposal_inner(
    proposals: &CodeChangeProposalStore,
    snapshots: &SnapshotStore,
    proposal_id: &CodeChangeProposalId,
    workspace_root: &Path,
    selected_paths: Option<&[String]>,
    expected_review_revision: Option<blake3::Hash>,
) -> Result<DecisionTransition<CodeChangeProposal>> {
    let lock = proposals.acquire_decision_lock(proposal_id.as_str())?;
    let proposal =
        match proposals.claim_apply_locked(proposal_id, expected_review_revision, &lock)? {
            DecisionTransition::Transitioned(proposal) => proposal,
            DecisionTransition::AlreadyResolved(proposal) => {
                return Ok(DecisionTransition::AlreadyResolved(proposal));
            },
        };

    let (snapshot_id, applied_paths, skipped_paths) =
        match apply_claimed_proposal(&proposal, snapshots, workspace_root, selected_paths) {
            Ok(applied) => applied,
            Err(apply_err) => {
                proposals
                    .reset_pending_locked(proposal, &lock)
                    .context("proposal apply failed and claim could not be released")?;
                return Err(apply_err);
            },
        };

    match proposals.finish_apply_locked(
        proposal_id,
        Some(snapshot_id.to_string()),
        applied_paths,
        skipped_paths,
        &lock,
    ) {
        Ok(applied) => Ok(DecisionTransition::Transitioned(applied)),
        Err(mark_err) => {
            let rollback = snapshots.restore(&snapshot_id);
            let reset = proposals.reset_pending_locked(proposal, &lock);
            match (rollback, reset) {
                (Ok(()), Ok(())) => Err(mark_err.context(
                    "proposal status commit failed; rolled back while decision lock was held",
                )),
                (rollback, reset) => Err(mark_err.context(format!(
                    "proposal status commit failed; rollback={rollback:?}; claim_reset={reset:?}"
                ))),
            }
        },
    }
}

fn apply_claimed_proposal(
    proposal: &CodeChangeProposal,
    snapshots: &SnapshotStore,
    workspace_root: &Path,
    selected_paths: Option<&[String]>,
) -> Result<(SnapshotId, Vec<PathBuf>, Vec<PathBuf>)> {
    let proposal_id = &proposal.id;

    let workspace_root_canonical = std::fs::canonicalize(workspace_root)
        .with_context(|| format!("canonicalize workspace_root {}", workspace_root.display()))?;
    let parsed = parse_unified_patch(&proposal.patch).context("parse stored proposal patch")?;
    let selected = selected_paths
        .map(normalize_selected_paths)
        .transpose()
        .context("normalize selected proposal paths")?;
    let total_file_count = parsed.len();
    let mut matched_selected_paths = BTreeSet::new();
    let mut applied_paths = Vec::new();
    let mut skipped_paths = Vec::new();
    let mut operations = Vec::with_capacity(total_file_count);
    let mut snapshot_paths = Vec::new();

    for file in parsed {
        let target_path = file
            .target_path()
            .ok_or_else(|| anyhow!("proposal patch file has neither old nor new path"))?
            .clone();
        let should_apply = selected
            .as_ref()
            .map(|selected| selected.contains(&target_path))
            .unwrap_or(true);
        if !should_apply {
            skipped_paths.push(target_path);
            continue;
        }
        matched_selected_paths.insert(target_path.clone());
        let operation = materialize_operation(file, workspace_root, &workspace_root_canonical)?;
        snapshot_paths.extend(operation.affected_absolute_paths());
        applied_paths.push(target_path);
        operations.push(operation);
    }
    if let Some(selected) = selected.as_ref() {
        let unknown: Vec<String> = selected
            .difference(&matched_selected_paths)
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        if !unknown.is_empty() {
            return Err(anyhow!(
                "selected proposal paths are not present in proposal {}: {}",
                proposal_id,
                unknown.join(", ")
            ));
        }
    }
    if operations.is_empty() {
        return Err(anyhow!(
            "apply proposal {} selected no files to apply",
            proposal_id
        ));
    }

    let snapshot_id = snapshots.capture(proposal_id.as_str(), &snapshot_paths)?;
    let mut error = None;
    for operation in &operations {
        if let Err(err) = operation.apply() {
            error = Some(err);
            break;
        }
    }

    if let Some(apply_err) = error {
        return match snapshots.restore(&snapshot_id) {
            Ok(()) => Err(apply_err.context("apply proposal failed; rolled back to snapshot")),
            Err(rollback_err) => Err(apply_err.context(format!(
                "apply proposal failed AND rollback also failed (snapshot {}): {rollback_err:#}",
                snapshot_id
            ))),
        };
    }

    Ok((snapshot_id, applied_paths, skipped_paths))
}

#[derive(Debug, Clone)]
struct ParsedFilePatch {
    old_path: Option<PathBuf>,
    new_path: Option<PathBuf>,
    hunks: Vec<Hunk>,
    unified_diff: String,
}

impl ParsedFilePatch {
    fn target_path(&self) -> Option<&PathBuf> {
        self.new_path.as_ref().or(self.old_path.as_ref())
    }

    fn status_letter(&self) -> &'static str {
        match (&self.old_path, &self.new_path) {
            (None, Some(_)) => "A",
            (Some(_), None) => "D",
            (Some(old), Some(new)) if old != new => "R",
            (Some(_), Some(_)) => "M",
            (None, None) => "M",
        }
    }
}

fn normalize_selected_paths(paths: &[String]) -> Result<BTreeSet<PathBuf>> {
    let mut selected = BTreeSet::new();
    for raw in paths {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let path = Path::new(raw);
        validate_relative_path(path)?;
        selected.insert(normalize_relative_path(path)?);
    }
    if selected.is_empty() {
        return Err(anyhow!(
            "selected_paths did not contain any non-empty paths"
        ));
    }
    Ok(selected)
}

#[derive(Debug, Clone)]
struct Hunk {
    old_start: usize,
    lines: Vec<HunkLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HunkLine {
    Context(String),
    Add(String),
    Remove(String),
}

fn parse_unified_patch(input: &str) -> Result<Vec<ParsedFilePatch>> {
    let mut files = Vec::new();
    let mut current: Option<ParsedFilePatch> = None;
    let mut current_hunk: Option<Hunk> = None;

    for line in input.lines() {
        if line.starts_with("diff --git ") {
            flush_hunk(&mut current, &mut current_hunk)?;
            flush_file(&mut files, &mut current);
            continue;
        }

        if let Some(rest) = line.strip_prefix("--- ") {
            flush_hunk(&mut current, &mut current_hunk)?;
            flush_file(&mut files, &mut current);
            current = Some(ParsedFilePatch {
                old_path: parse_patch_path(rest)?,
                new_path: None,
                hunks: Vec::new(),
                unified_diff: format!("{line}\n"),
            });
            continue;
        }

        if let Some(rest) = line.strip_prefix("+++ ") {
            let file = current
                .as_mut()
                .ok_or_else(|| anyhow!("encountered `+++` before `---`"))?;
            file.new_path = parse_patch_path(rest)?;
            file.unified_diff.push_str(line);
            file.unified_diff.push('\n');
            continue;
        }

        if line.starts_with("@@") {
            flush_hunk(&mut current, &mut current_hunk)?;
            let old_start = parse_hunk_old_start(line)
                .with_context(|| format!("parse hunk header `{line}`"))?;
            current_hunk = Some(Hunk {
                old_start,
                lines: Vec::new(),
            });
            if let Some(file) = current.as_mut() {
                file.unified_diff.push_str(line);
                file.unified_diff.push('\n');
            }
            continue;
        }

        if line.starts_with("\\ No newline at end of file") {
            if let Some(file) = current.as_mut() {
                file.unified_diff.push_str(line);
                file.unified_diff.push('\n');
            }
            continue;
        }

        if let Some(hunk) = current_hunk.as_mut() {
            if line.is_empty() {
                continue;
            }
            let (marker, text) = line.split_at(1);
            match marker {
                " " => hunk.lines.push(HunkLine::Context(text.to_string())),
                "+" => hunk.lines.push(HunkLine::Add(text.to_string())),
                "-" => hunk.lines.push(HunkLine::Remove(text.to_string())),
                _ => {},
            }
            if let Some(file) = current.as_mut() {
                file.unified_diff.push_str(line);
                file.unified_diff.push('\n');
            }
        }
    }

    flush_hunk(&mut current, &mut current_hunk)?;
    flush_file(&mut files, &mut current);
    Ok(files)
}

fn flush_hunk(current: &mut Option<ParsedFilePatch>, hunk: &mut Option<Hunk>) -> Result<()> {
    let Some(hunk) = hunk.take() else {
        return Ok(());
    };
    let file = current
        .as_mut()
        .ok_or_else(|| anyhow!("encountered hunk before file header"))?;
    file.hunks.push(hunk);
    Ok(())
}

fn flush_file(files: &mut Vec<ParsedFilePatch>, current: &mut Option<ParsedFilePatch>) {
    if let Some(file) = current.take() {
        if !file.hunks.is_empty() {
            files.push(file);
        }
    }
}

fn parse_patch_path(raw: &str) -> Result<Option<PathBuf>> {
    let token = raw
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow!("missing patch path"))?;
    if token == "/dev/null" {
        return Ok(None);
    }
    let token = token
        .strip_prefix("a/")
        .or_else(|| token.strip_prefix("b/"))
        .unwrap_or(token);
    let path = Path::new(token);
    validate_relative_path(path)?;
    Ok(Some(normalize_relative_path(path)?))
}

fn parse_hunk_old_start(line: &str) -> Result<usize> {
    let start = line
        .find('-')
        .ok_or_else(|| anyhow!("hunk header missing old range"))?
        + 1;
    let rest = &line[start..];
    let end = rest
        .find(|c: char| c == ',' || c.is_whitespace())
        .ok_or_else(|| anyhow!("hunk header old range is malformed"))?;
    let parsed = rest[..end].parse::<usize>()?;
    Ok(parsed.max(1))
}

#[derive(Debug)]
enum PatchOperation {
    Create {
        path: PathBuf,
        content: String,
    },
    Modify {
        path: PathBuf,
        content: String,
    },
    Delete {
        path: PathBuf,
    },
    Move {
        from: PathBuf,
        to: PathBuf,
        content: String,
    },
}

impl PatchOperation {
    fn affected_absolute_paths(&self) -> Vec<PathBuf> {
        match self {
            Self::Create { path, .. } | Self::Modify { path, .. } | Self::Delete { path } => {
                vec![path.clone()]
            },
            Self::Move { from, to, .. } => vec![from.clone(), to.clone()],
        }
    }

    fn apply(&self) -> Result<()> {
        match self {
            Self::Create { path, content } => {
                if path.exists() {
                    return Err(anyhow!(
                        "create: {} already exists; re-stage proposal from current workspace",
                        path.display()
                    ));
                }
                if let Some(parent) = path.parent() {
                    if !parent.as_os_str().is_empty() {
                        std::fs::create_dir_all(parent)
                            .with_context(|| format!("create parent dir {}", parent.display()))?;
                    }
                }
                std::fs::write(path, content)
                    .with_context(|| format!("create write {}", path.display()))
            },
            Self::Modify { path, content } => std::fs::write(path, content)
                .with_context(|| format!("modify write {}", path.display())),
            Self::Delete { path } => {
                std::fs::remove_file(path).with_context(|| format!("delete {}", path.display()))
            },
            Self::Move { from, to, content } => {
                if from == to {
                    return Err(anyhow!(
                        "move: from == to ({}) — re-stage as Modify if content changed",
                        from.display()
                    ));
                }
                if to.exists() {
                    return Err(anyhow!(
                        "move: destination {} already exists; re-stage proposal from current workspace",
                        to.display()
                    ));
                }
                if let Some(parent) = to.parent() {
                    if !parent.as_os_str().is_empty() {
                        std::fs::create_dir_all(parent)
                            .with_context(|| format!("create parent dir {}", parent.display()))?;
                    }
                }
                std::fs::write(to, content)
                    .with_context(|| format!("move write {}", to.display()))?;
                std::fs::remove_file(from)
                    .with_context(|| format!("move remove src {}", from.display()))
            },
        }
    }
}

fn materialize_operation(
    file: ParsedFilePatch,
    workspace_root: &Path,
    workspace_root_canonical: &Path,
) -> Result<PatchOperation> {
    match (&file.old_path, &file.new_path) {
        (None, Some(new_path)) => {
            let target =
                resolve_patch_path(new_path, workspace_root, workspace_root_canonical, false)?;
            let content = apply_hunks("", &file.hunks)
                .with_context(|| format!("apply create hunk for {}", new_path.display()))?;
            Ok(PatchOperation::Create {
                path: target,
                content,
            })
        },
        (Some(old_path), None) => {
            let target =
                resolve_patch_path(old_path, workspace_root, workspace_root_canonical, true)?;
            let old = read_text_bounded(&target)
                .with_context(|| format!("read delete baseline {}", target.display()))?;
            let content = apply_hunks(&old, &file.hunks)
                .with_context(|| format!("apply delete hunk for {}", old_path.display()))?;
            if !content.is_empty() {
                return Err(anyhow!(
                    "delete patch for {} left {} bytes; full-file delete patches must remove all content",
                    old_path.display(),
                    content.len()
                ));
            }
            Ok(PatchOperation::Delete { path: target })
        },
        (Some(old_path), Some(new_path)) if old_path == new_path => {
            let target =
                resolve_patch_path(old_path, workspace_root, workspace_root_canonical, true)?;
            let old = read_text_bounded(&target)
                .with_context(|| format!("read modify baseline {}", target.display()))?;
            let content = apply_hunks(&old, &file.hunks)
                .with_context(|| format!("apply modify hunk for {}", old_path.display()))?;
            Ok(PatchOperation::Modify {
                path: target,
                content,
            })
        },
        (Some(old_path), Some(new_path)) => {
            let from =
                resolve_patch_path(old_path, workspace_root, workspace_root_canonical, true)?;
            let to = resolve_patch_path(new_path, workspace_root, workspace_root_canonical, false)?;
            let old = read_text_bounded(&from)
                .with_context(|| format!("read move baseline {}", from.display()))?;
            let content = apply_hunks(&old, &file.hunks)
                .with_context(|| format!("apply move hunk for {}", old_path.display()))?;
            Ok(PatchOperation::Move { from, to, content })
        },
        (None, None) => Err(anyhow!("patch file has neither old nor new path")),
    }
}

fn resolve_patch_path(
    rel: &Path,
    workspace_root: &Path,
    workspace_root_canonical: &Path,
    target_must_exist: bool,
) -> Result<PathBuf> {
    validate_relative_path(rel)?;
    let absolute = workspace_root.join(rel);
    assert_resolved_inside_workspace(&absolute, workspace_root_canonical, target_must_exist)
        .with_context(|| format!("symlink escape check for {}", rel.display()))?;
    Ok(absolute)
}

fn apply_hunks(old_content: &str, hunks: &[Hunk]) -> Result<String> {
    let old_lines = split_lines_preserve_endings(old_content);
    let mut output = Vec::new();
    let mut old_index = 0usize;

    for hunk in hunks {
        let target = hunk.old_start.saturating_sub(1);
        if target > old_lines.len() {
            return Err(anyhow!(
                "hunk starts at old line {}, but file has {} lines",
                hunk.old_start,
                old_lines.len()
            ));
        }
        while old_index < target {
            output.push(old_lines[old_index].clone());
            old_index += 1;
        }

        for line in &hunk.lines {
            match line {
                HunkLine::Context(expected) => {
                    assert_old_line(&old_lines, old_index, expected, "context")?;
                    output.push(old_lines[old_index].clone());
                    old_index += 1;
                },
                HunkLine::Remove(expected) => {
                    assert_old_line(&old_lines, old_index, expected, "remove")?;
                    old_index += 1;
                },
                HunkLine::Add(text) => output.push(format!("{text}\n")),
            }
        }
    }

    while old_index < old_lines.len() {
        output.push(old_lines[old_index].clone());
        old_index += 1;
    }

    Ok(output.concat())
}

fn split_lines_preserve_endings(content: &str) -> Vec<String> {
    if content.is_empty() {
        return Vec::new();
    }
    content.split_inclusive('\n').map(str::to_string).collect()
}

fn assert_old_line(lines: &[String], index: usize, expected: &str, kind: &str) -> Result<()> {
    let Some(actual) = lines.get(index) else {
        return Err(anyhow!(
            "{kind} line expected `{expected}`, but file ended at line {}",
            index + 1
        ));
    };
    let actual_body = actual.trim_end_matches(['\r', '\n']);
    if actual_body != expected {
        return Err(anyhow!(
            "{kind} line mismatch at old line {}: expected `{}`, found `{}`",
            index + 1,
            expected,
            actual_body
        ));
    }
    Ok(())
}

fn normalize_relative_path(path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {},
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(anyhow!(
                    "path `{}` is not workspace-relative",
                    path.display()
                ));
            },
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(anyhow!("path cannot resolve to workspace root"));
    }
    Ok(normalized)
}

fn validate_relative_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() {
        return Err(anyhow!("empty path"));
    }
    if path.is_absolute() {
        return Err(anyhow!(
            "path is absolute (only workspace-relative paths allowed): {}",
            path.display()
        ));
    }
    for component in path.components() {
        match component {
            Component::ParentDir => {
                return Err(anyhow!(
                    "path contains `..` component; would escape workspace: {}",
                    path.display()
                ));
            },
            Component::RootDir | Component::Prefix(_) => {
                return Err(anyhow!(
                    "path has root / drive prefix; only workspace-relative paths allowed: {}",
                    path.display()
                ));
            },
            Component::Normal(_) | Component::CurDir => {},
        }
    }
    Ok(())
}

fn assert_resolved_inside_workspace(
    absolute_path: &Path,
    workspace_root_canonical: &Path,
    target_must_exist: bool,
) -> Result<()> {
    let resolved = if target_must_exist {
        std::fs::canonicalize(absolute_path)
            .with_context(|| format!("canonicalize {}", absolute_path.display()))?
    } else {
        let parent = absolute_path
            .parent()
            .ok_or_else(|| anyhow!("cannot derive parent dir of {}", absolute_path.display()))?;
        let parent_canonical = if parent.as_os_str().is_empty() {
            workspace_root_canonical.to_path_buf()
        } else if parent.exists() {
            std::fs::canonicalize(parent)
                .with_context(|| format!("canonicalize parent {}", parent.display()))?
        } else {
            let mut cursor = parent.to_path_buf();
            loop {
                if cursor.exists() {
                    break std::fs::canonicalize(&cursor)
                        .with_context(|| format!("canonicalize ancestor {}", cursor.display()))?;
                }
                let Some(next) = cursor.parent().map(Path::to_path_buf) else {
                    return Err(anyhow!(
                        "no existing ancestor for {}; cannot validate symlink containment",
                        absolute_path.display()
                    ));
                };
                if next == cursor {
                    return Err(anyhow!(
                        "filesystem root reached while validating {}",
                        absolute_path.display()
                    ));
                }
                cursor = next;
            }
        };
        let leaf = absolute_path
            .file_name()
            .ok_or_else(|| anyhow!("path has no filename: {}", absolute_path.display()))?;
        parent_canonical.join(leaf)
    };

    if !resolved.starts_with(workspace_root_canonical) {
        return Err(anyhow!(
            "path {} resolves to {} which is outside workspace root {} (symlink escape)",
            absolute_path.display(),
            resolved.display(),
            workspace_root_canonical.display()
        ));
    }
    Ok(())
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn temp_scope() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("code-change-proposal-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn scope() -> TransactionScope {
        TransactionScope {
            principal: "test".into(),
            workspace: "test".into(),
        }
    }

    #[test]
    fn stage_patch_persists_diff_payload() {
        let root = temp_scope();
        let store = CodeChangeProposalStore::new(&root);
        let proposal = store
            .stage_patch(
                scope(),
                "Update greeting",
                "\
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,1 +1,1 @@
-hello
+hello world
",
                "pi-session-1",
                Vec::new(),
            )
            .unwrap();

        assert_eq!(proposal.status, CodeChangeProposalStatus::Pending);
        assert_eq!(proposal.touched_files, vec![PathBuf::from("src/main.rs")]);
        assert_eq!(proposal.files[0].status, "M");
        assert_eq!(proposal.files[0].additions, 1);
        assert_eq!(proposal.files[0].deletions, 1);
        assert!(store.exists(&proposal.id));

        let loaded = store.load(&proposal.id).unwrap();
        assert_eq!(loaded.summary, "Update greeting");
        assert_eq!(loaded.source_session_id, "pi-session-1");
    }

    /// Persist runs on every state transition of the same proposal id, and the
    /// record carries the only copy of the patch. It must publish in one step
    /// and leave nothing beside itself in `code_change_proposals/`.
    #[test]
    fn persist_publishes_without_leaving_a_staging_sibling() {
        let root = temp_scope();
        let store = CodeChangeProposalStore::new(&root);
        let proposal = store
            .stage_patch(
                scope(),
                "staging check",
                "--- a/f.txt\n+++ b/f.txt\n@@ -1,1 +1,1 @@\n-a\n+b\n",
                "pi-session",
                Vec::new(),
            )
            .unwrap();
        // A second persist of the same id — the transition path.
        store.reject(&proposal.id).unwrap();

        let staging = std::fs::read_dir(root.join("code_change_proposals"))
            .expect("proposals listing")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect::<Vec<_>>();
        assert!(
            staging.is_empty(),
            "persist must leave no staging sibling, found {staging:?}"
        );
        assert_eq!(
            store.load(&proposal.id).unwrap().status,
            CodeChangeProposalStatus::Rejected
        );
    }

    #[test]
    fn stage_with_run_identity_round_trips() {
        let root = temp_scope();
        let store = CodeChangeProposalStore::new(&root);
        let staged = store
            .stage_patch_with_apply_root(
                scope(),
                "stamped",
                "--- a/f.txt\n+++ b/f.txt\n@@ -1,1 +1,1 @@\n-a\n+b\n",
                "pi-session",
                Vec::new(),
                None,
                Some("task-123".to_string()),
                Some("exec-456".to_string()),
            )
            .unwrap();
        assert_eq!(staged.task_id.as_deref(), Some("task-123"));
        assert_eq!(staged.execution_id.as_deref(), Some("exec-456"));
        // Identity survives the persist → load round-trip.
        let loaded = store.load(&staged.id).unwrap();
        assert_eq!(loaded.task_id.as_deref(), Some("task-123"));
        assert_eq!(loaded.execution_id.as_deref(), Some("exec-456"));

        // The convenience wrapper leaves identity unstamped (the legacy shape).
        let plain = store
            .stage_patch(
                scope(),
                "unstamped",
                "--- a/g.txt\n+++ b/g.txt\n@@ -1,1 +1,1 @@\n-x\n+y\n",
                "pi-session",
                Vec::new(),
            )
            .unwrap();
        assert!(plain.task_id.is_none());
        assert!(plain.execution_id.is_none());
    }

    #[test]
    fn list_for_task_filters_and_tolerates_legacy_and_malformed() {
        let root = temp_scope();
        let store = CodeChangeProposalStore::new(&root);
        let mk = |name: &str, task: Option<&str>| {
            store
                .stage_patch_with_apply_root(
                    scope(),
                    name,
                    format!("--- a/{name}.txt\n+++ b/{name}.txt\n@@ -1,1 +1,1 @@\n-a\n+{name}\n"),
                    "pi-session",
                    Vec::new(),
                    None,
                    task.map(|t| t.to_string()),
                    None,
                )
                .unwrap()
        };
        let a = mk("alpha", Some("T1"));
        let b = mk("bravo", Some("T1"));
        let _c = mk("charlie", Some("T2"));
        // Legacy proposal: stage_patch never stamps task_id, so its on-disk JSON has
        // no task_id field — deserializes to None and must be filtered out of "T1".
        let _legacy = store
            .stage_patch(
                scope(),
                "legacy",
                "--- a/legacy.txt\n+++ b/legacy.txt\n@@ -1,1 +1,1 @@\n-a\n+legacy\n",
                "pi-session",
                Vec::new(),
            )
            .unwrap();
        // A malformed .json in the store dir must be skipped, never panic the scan.
        std::fs::write(
            root.join("code_change_proposals")
                .join("not-a-proposal.json"),
            b"{ broken",
        )
        .unwrap();

        let mut got: Vec<String> = store
            .list_for_task("T1")
            .into_iter()
            .map(|p| p.id.to_string())
            .collect();
        got.sort();
        let mut want = vec![a.id.to_string(), b.id.to_string()];
        want.sort();
        assert_eq!(
            got, want,
            "list_for_task returns exactly the T1-stamped proposals"
        );
        assert!(store.list_for_task("T-missing").is_empty());
    }

    #[test]
    fn strict_task_census_fails_closed_on_corrupt_or_oversized_records() {
        let root = temp_scope();
        let store = CodeChangeProposalStore::new(&root);
        store
            .stage_patch_with_apply_root(
                scope(),
                "strict",
                "--- a/strict.txt\n+++ b/strict.txt\n@@ -1,1 +1,1 @@\n-a\n+b\n",
                "pi-session",
                Vec::new(),
                None,
                Some("T-strict".to_string()),
                None,
            )
            .unwrap();
        let proposal_root = root.join("code_change_proposals");
        std::fs::write(proposal_root.join("corrupt.json"), b"{ broken").unwrap();
        assert!(store
            .try_list_for_task_bounded("T-strict", 8, 1024 * 1024, 2 * 1024 * 1024)
            .unwrap_err()
            .to_string()
            .contains("parse proposal ownership"));

        std::fs::remove_file(proposal_root.join("corrupt.json")).unwrap();
        assert!(store
            .try_list_for_task_bounded("T-strict", 8, 8, 2 * 1024 * 1024)
            .unwrap_err()
            .to_string()
            .contains("strict census cap"));
        assert!(store
            .try_list_for_task_bounded("T-strict", 0, 1024 * 1024, 2 * 1024 * 1024)
            .unwrap_err()
            .to_string()
            .contains("exceeded 0 records"));
    }

    /// The way a cheap ownership check goes wrong.
    ///
    /// `list_for_task` no longer deserializes a 700KB record to discard it on
    /// one short string — but the field is the only thing it may read. A
    /// proposal whose *diff text* mentions another run's id belongs to its own
    /// run and nobody else's, and a needle over the raw bytes would hand it to
    /// the wrong task's card.
    #[test]
    fn list_for_task_reads_the_field_not_the_patch_text() {
        let root = temp_scope();
        let store = CodeChangeProposalStore::new(&root);
        let owner = "task-11111111-1111-1111-1111-111111111111";
        let mentioned = "task-22222222-2222-2222-2222-222222222222";

        let staged = store
            .stage_patch_with_apply_root(
                scope(),
                "a diff that talks about another run",
                format!(
                    "--- a/notes.txt\n+++ b/notes.txt\n@@ -1,1 +1,1 @@\n-follow up on {mentioned}\n+follow up on {mentioned} tomorrow\n"
                ),
                "pi-session",
                Vec::new(),
                None,
                Some(owner.to_string()),
                None,
            )
            .unwrap();
        assert!(
            staged.patch.contains(mentioned),
            "the fixture only means something if the id really is in the patch"
        );

        let owned: Vec<String> = store
            .list_for_task(owner)
            .into_iter()
            .map(|proposal| proposal.id.to_string())
            .collect();
        assert_eq!(owned, vec![staged.id.to_string()]);
        assert!(
            store.list_for_task(mentioned).is_empty(),
            "a run mentioned inside the diff does not own the proposal"
        );
    }

    /// The hoist's correctness bar: the ONE scan must answer exactly what the
    /// per-task calls answered, task by task, including for tasks with nothing
    /// staged and for a proposal whose run never stamped an id.
    #[test]
    fn scan_pending_summaries_matches_the_per_task_answer() {
        let root = temp_scope();
        let store = CodeChangeProposalStore::new(&root);
        let mk = |name: &str, task: Option<&str>| {
            store
                .stage_patch_with_apply_root(
                    scope(),
                    name,
                    format!("--- a/{name}.txt\n+++ b/{name}.txt\n@@ -1,1 +1,1 @@\n-a\n+{name}\n"),
                    "pi-session",
                    Vec::new(),
                    None,
                    task.map(|t| t.to_string()),
                    None,
                )
                .unwrap()
        };
        let alpha = mk("alpha", Some("T1"));
        let bravo = mk("bravo", Some("T1"));
        let charlie = mk("charlie", Some("T2"));
        let orphan = mk("orphan", None);
        let rejected = mk("rejected", Some("T3"));
        store.reject(&rejected.id).unwrap();

        let summaries = store.scan_pending_summaries();

        for task_id in ["T1", "T2", "T3", "T-missing"] {
            let mut per_task: Vec<String> = store
                .list_for_task(task_id)
                .into_iter()
                .filter(|proposal| proposal.status == CodeChangeProposalStatus::Pending)
                .map(|proposal| proposal.id.to_string())
                .collect();
            per_task.sort();
            let mut hoisted: Vec<String> = summaries
                .iter()
                .filter(|summary| summary.task_id.as_deref() == Some(task_id))
                .map(|summary| summary.id.to_string())
                .collect();
            hoisted.sort();
            assert_eq!(
                hoisted, per_task,
                "one scan and N scans must agree about {task_id}"
            );
        }

        let mut all: Vec<String> = summaries.iter().map(|s| s.id.to_string()).collect();
        all.sort();
        let mut want = vec![
            alpha.id.to_string(),
            bravo.id.to_string(),
            charlie.id.to_string(),
            orphan.id.to_string(),
        ];
        want.sort();
        assert_eq!(
            all, want,
            "a rejected proposal is not pending; an unstamped one still is"
        );
        assert!(
            summaries
                .iter()
                .all(|summary| summary.status == CodeChangeProposalStatus::Pending),
            "the scan yields Pending records only"
        );
        assert_eq!(
            summaries
                .iter()
                .find(|summary| summary.id == alpha.id)
                .map(|summary| summary.file_count),
            Some(1),
            "the summary carries the file count the announcement reads, \
             without carrying the diff"
        );
    }

    /// One call, one walk — including over a store that does not exist. The
    /// counter this asserts on is what the listing-level regression uses to
    /// prove a request scans once however many tasks it renders.
    #[test]
    fn every_store_query_walks_the_directory_exactly_once() {
        let _scan_guard = lock_proposal_scan_counter();
        let root = temp_scope();
        let store = CodeChangeProposalStore::new(&root);

        reset_proposal_directory_scan_count();
        assert!(store.list_for_task("T1").is_empty());
        assert_eq!(
            proposal_directory_scan_count(),
            1,
            "an absent store still costs exactly one walk"
        );

        store
            .stage_patch_with_apply_root(
                scope(),
                "staged",
                "--- a/f.txt\n+++ b/f.txt\n@@ -1,1 +1,1 @@\n-a\n+b\n",
                "pi-session",
                Vec::new(),
                None,
                Some("T1".to_string()),
                None,
            )
            .unwrap();

        reset_proposal_directory_scan_count();
        assert_eq!(store.scan_pending_summaries().len(), 1);
        assert_eq!(proposal_directory_scan_count(), 1);

        reset_proposal_directory_scan_count();
        assert_eq!(store.list_pending().len(), 1);
        assert_eq!(store.list_for_task("T1").len(), 1);
        assert_eq!(
            proposal_directory_scan_count(),
            2,
            "two queries, two walks — the saving comes from making fewer calls"
        );
    }

    #[test]
    fn apply_modify_proposal_writes_and_marks_applied() {
        let root = temp_scope();
        let workspace = root.join("workspace");
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        std::fs::write(workspace.join("src/main.rs"), "hello\n").unwrap();

        let store = CodeChangeProposalStore::new(&root);
        let snapshots = SnapshotStore::new(&root);
        let proposal = store
            .stage_patch(
                scope(),
                "Update greeting",
                "\
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,1 +1,1 @@
-hello
+hello world
",
                "pi-session-1",
                Vec::new(),
            )
            .unwrap();

        apply_proposal(&store, &snapshots, &proposal.id, &workspace).unwrap();

        assert_eq!(
            std::fs::read_to_string(workspace.join("src/main.rs")).unwrap(),
            "hello world\n"
        );
        let loaded = store.load(&proposal.id).unwrap();
        assert_eq!(loaded.status, CodeChangeProposalStatus::Applied);
        assert!(loaded.snapshot_id.is_some());
    }

    #[test]
    fn concurrent_proposal_apply_and_reject_have_one_terminal_winner() {
        let root = temp_scope();
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let store = CodeChangeProposalStore::new(&root);
        let proposal = store
            .stage_patch(
                scope(),
                "race",
                "--- /dev/null\n+++ b/race.txt\n@@ -0,0 +1,1 @@\n+winner\n",
                "session-race",
                Vec::new(),
            )
            .unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));

        let apply_root = root.clone();
        let apply_workspace = workspace.clone();
        let apply_id = proposal.id.clone();
        let apply_barrier = barrier.clone();
        let apply = std::thread::spawn(move || {
            let store = CodeChangeProposalStore::new(&apply_root);
            let snapshots = SnapshotStore::new(&apply_root);
            apply_barrier.wait();
            apply_proposal_decision(&store, &snapshots, &apply_id, &apply_workspace).unwrap()
        });

        let reject_root = root.clone();
        let reject_id = proposal.id.clone();
        let reject = std::thread::spawn(move || {
            let store = CodeChangeProposalStore::new(&reject_root);
            barrier.wait();
            store.reject(&reject_id).unwrap()
        });

        let apply_outcome = apply.join().unwrap();
        let reject_outcome = reject.join().unwrap();
        assert_ne!(apply_outcome.transitioned(), reject_outcome.transitioned());
        match store.load(&proposal.id).unwrap().status {
            CodeChangeProposalStatus::Applied => {
                assert_eq!(
                    std::fs::read_to_string(workspace.join("race.txt")).unwrap(),
                    "winner\n"
                );
            },
            CodeChangeProposalStatus::Rejected => assert!(!workspace.join("race.txt").exists()),
            status => panic!("unexpected final status: {status:?}"),
        }
    }

    #[test]
    fn apply_selected_paths_writes_subset_and_marks_partially_applied() {
        let root = temp_scope();
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("one.txt"), "one\n").unwrap();
        std::fs::write(workspace.join("two.txt"), "two\n").unwrap();

        let store = CodeChangeProposalStore::new(&root);
        let snapshots = SnapshotStore::new(&root);
        let proposal = store
            .stage_patch(
                scope(),
                "Update two files",
                "\
--- a/one.txt
+++ b/one.txt
@@ -1,1 +1,1 @@
-one
+one updated
--- a/two.txt
+++ b/two.txt
@@ -1,1 +1,1 @@
-two
+two updated
",
                "pi-session-1",
                Vec::new(),
            )
            .unwrap();

        let applied = apply_proposal_selected_paths(
            &store,
            &snapshots,
            &proposal.id,
            &workspace,
            &["one.txt".to_string()],
        )
        .unwrap();

        assert_eq!(applied.status, CodeChangeProposalStatus::PartiallyApplied);
        assert_eq!(applied.applied_paths, vec![PathBuf::from("one.txt")]);
        assert_eq!(applied.skipped_paths, vec![PathBuf::from("two.txt")]);
        assert_eq!(
            std::fs::read_to_string(workspace.join("one.txt")).unwrap(),
            "one updated\n"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("two.txt")).unwrap(),
            "two\n"
        );
    }

    #[test]
    fn apply_selected_paths_rejects_unknown_path() {
        let root = temp_scope();
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("one.txt"), "one\n").unwrap();

        let store = CodeChangeProposalStore::new(&root);
        let snapshots = SnapshotStore::new(&root);
        let proposal = store
            .stage_patch(
                scope(),
                "Update one file",
                "\
--- a/one.txt
+++ b/one.txt
@@ -1,1 +1,1 @@
-one
+one updated
",
                "pi-session-1",
                Vec::new(),
            )
            .unwrap();

        let err = apply_proposal_selected_paths(
            &store,
            &snapshots,
            &proposal.id,
            &workspace,
            &["missing.txt".to_string()],
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("selected proposal paths are not present"));
        assert_eq!(
            std::fs::read_to_string(workspace.join("one.txt")).unwrap(),
            "one\n"
        );
        assert_eq!(
            store.load(&proposal.id).unwrap().status,
            CodeChangeProposalStatus::Pending
        );
    }

    #[test]
    fn reject_proposal_leaves_workspace_untouched() {
        let root = temp_scope();
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("hello.txt"), "hello\n").unwrap();

        let store = CodeChangeProposalStore::new(&root);
        let proposal = store
            .stage_patch(
                scope(),
                "Update greeting",
                "\
--- a/hello.txt
+++ b/hello.txt
@@ -1,1 +1,1 @@
-hello
+hello world
",
                "pi-session-1",
                Vec::new(),
            )
            .unwrap();
        let first = store.reject(&proposal.id).unwrap();
        assert!(matches!(first, DecisionTransition::Transitioned(_)));
        let stale = store.reject(&proposal.id).unwrap();
        assert!(matches!(stale, DecisionTransition::AlreadyResolved(_)));

        assert_eq!(
            std::fs::read_to_string(workspace.join("hello.txt")).unwrap(),
            "hello\n"
        );
        assert_eq!(
            store.load(&proposal.id).unwrap().status,
            CodeChangeProposalStatus::Rejected
        );
    }

    #[test]
    fn apply_create_and_delete_patch() {
        let root = temp_scope();
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("old.txt"), "old\n").unwrap();

        let store = CodeChangeProposalStore::new(&root);
        let snapshots = SnapshotStore::new(&root);
        let proposal = store
            .stage_patch(
                scope(),
                "Create and delete",
                "\
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,1 @@
+new
--- a/old.txt
+++ /dev/null
@@ -1,1 +0,0 @@
-old
",
                "pi-session-1",
                Vec::new(),
            )
            .unwrap();

        apply_proposal(&store, &snapshots, &proposal.id, &workspace).unwrap();
        assert_eq!(
            std::fs::read_to_string(workspace.join("new.txt")).unwrap(),
            "new\n"
        );
        assert!(!workspace.join("old.txt").exists());
    }

    #[test]
    fn stage_rejects_path_traversal() {
        let root = temp_scope();
        let store = CodeChangeProposalStore::new(&root);
        let err = store
            .stage_patch(
                scope(),
                "Bad patch",
                "\
--- a/../secret.txt
+++ b/../secret.txt
@@ -1,1 +1,1 @@
-old
+new
",
                "pi-session-1",
                Vec::new(),
            )
            .unwrap_err();

        assert!(err.to_string().contains("parse proposal patch"));
    }

    #[test]
    fn proposal_id_parse_rejects_path_traversal() {
        let err = CodeChangeProposalId::parse("../ccp-1").unwrap_err();
        assert!(err.to_string().contains("disallowed char"));
    }

    // ── Trusted-store integrity: content-hash sensitivity locks ──────────
    //
    // These are UNIT-level locks of the mechanism's building blocks. The FULL
    // end-to-end HTTP regression (stage a real proposal → POST the diff-approval
    // to `respond_hitl_handler` → assert forged/tampered/restart all refuse;
    // assert a legit approve applies) is an INTEGRATION test that must be written
    // and RUN once the crate compiles.
    //
    // TODO(trusted-store): end-to-end HTTP stage→approve→apply integration
    // regression — write + run when the crate compiles.

    /// `content_hash()` is deterministic — two calls on the same proposal yield
    /// the same hash. The decide site recomputes this on the loaded on-disk
    /// record and compares against the authority entry, so any nondeterminism
    /// would false-fail every legitimate apply.
    #[test]
    fn content_hash_is_deterministic() {
        let root = temp_scope();
        let store = CodeChangeProposalStore::new(&root);
        let proposal = store
            .stage_patch(
                scope(),
                "deterministic",
                "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,1 +1,1 @@\n-hello\n+hello world\n",
                "pi-session-1",
                Vec::new(),
            )
            .unwrap();

        assert_eq!(
            proposal.content_hash(),
            proposal.content_hash(),
            "content_hash must be stable across repeated calls"
        );
    }

    /// Two proposals differing only in `patch` produce different `content_hash`
    /// values. This is the exact property that lets the decide site refuse a
    /// tampered proposal: if the on-disk `patch` JSON is edited out of band
    /// between stage and apply, the recomputed hash no longer matches the
    /// authority entry recorded at stage time, so apply fails closed.
    #[test]
    fn content_hash_detects_forged_patch() {
        let root = temp_scope();
        let store = CodeChangeProposalStore::new(&root);
        let baseline = store
            .stage_patch(
                scope(),
                "baseline",
                "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,1 +1,1 @@\n-hello\n+hello world\n",
                "pi-session-1",
                Vec::new(),
            )
            .unwrap();
        let h1 = baseline.content_hash();

        // Same proposal, only the `patch` differs (a forged replacement line).
        let mut forged = baseline.clone();
        forged.patch =
            "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,1 +1,1 @@\n-hello\n+forged evil\n".into();

        assert_ne!(
            forged.content_hash(),
            h1,
            "changing the patch MUST change content_hash"
        );
    }

    #[test]
    fn content_hash_detects_forged_scope_and_run_provenance() {
        let root = temp_scope();
        let store = CodeChangeProposalStore::new(&root);
        let baseline = store
            .stage_patch_with_apply_root(
                scope(),
                "bound",
                "--- a/f.txt\n+++ b/f.txt\n@@ -1,1 +1,1 @@\n-a\n+b\n",
                "session-1",
                Vec::new(),
                Some(PathBuf::from("/repo")),
                Some("task-1".into()),
                Some("exec-1".into()),
            )
            .unwrap();
        let expected = baseline.content_hash();

        let mut forged = baseline.clone();
        forged.scope.principal = "forged-principal".into();
        assert_ne!(expected, forged.content_hash());
        forged = baseline.clone();
        forged.scope.workspace = "forged-workspace".into();
        assert_ne!(expected, forged.content_hash());
        forged = baseline.clone();
        forged.task_id = Some("forged-task".into());
        assert_ne!(expected, forged.content_hash());
        forged = baseline.clone();
        forged.execution_id = Some("forged-exec".into());
        assert_ne!(expected, forged.content_hash());
        forged = baseline.clone();
        forged.apply_root = Some(PathBuf::from("/forged-root"));
        assert_ne!(expected, forged.content_hash());
    }
}
