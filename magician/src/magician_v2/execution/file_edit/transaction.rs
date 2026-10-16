//! `FileEditTransaction` — atomic, approvable multi-file edit unit.
//!
//! ## Lifecycle
//!
//! 1. **Stage** — a tool computes the proposed new content for one or
//!    more files in memory, runs [`super::diff::compute_unified_diff`]
//!    against the existing content, and calls
//!    [`TransactionStore::stage`]. Nothing is written to disk except a
//!    JSON record of the transaction itself
//!    (`<scope>/transactions/<id>.json`).
//! 2. **Request approval** — caller emits a `HitlRequested` event with
//!    `input_type: "diff_approval"` carrying the transaction id + per-file
//!    diff payload. The chat surface renders this via the `DiffStrip`
//!    component (`lifecycle="pending"`, `allowApprove`) and waits for
//!    the operator's response.
//! 3. **Apply** — on approve, [`TransactionStore::apply`] takes a
//!    snapshot of the affected paths (delegated to the `snapshot`
//!    module in a future commit), writes the queued contents
//!    atomically, and transitions the transaction status to
//!    [`FileEditStatus::Applied`].
//! 4. **Reject** — on deny, [`TransactionStore::reject`] transitions
//!    the transaction to [`FileEditStatus::Rejected`] with no disk
//!    mutation.
//! 5. **Revert** — when a snapshot exists, the operator can restore
//!    via the snapshot module; the transaction transitions to
//!    [`FileEditStatus::Reverted`].
//!
//! ## Atomicity guarantee
//!
//! [`TransactionStore::apply`] writes every queued file before
//! flipping the status to `Applied`. If any individual write fails,
//! the function attempts to roll back already-written files using the
//! pre-apply snapshot and surfaces the failure as an `Err`. The
//! transaction record stays in `Pending` so a retry is possible.
//!
//! ## Module independence
//!
//! This module is **purely additive** and does not interfere with any
//! existing file-write path. Tools that want approval-gated edits opt
//! in by calling [`TransactionStore::stage`]; tools that don't (the
//! workspace bootstrap, the chat-store sink, the artifact V2 writer)
//! continue to write directly without touching anything here.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::diff::{compute_unified_diff, diff_stats, DiffStats};
use super::snapshot::MAX_FILE_READ_BYTES;
use crate::magician_v2::artifact_v2::io::write_bytes_durably_sync;

/// Single proposed edit within a transaction.
///
/// `Modify` carries the diff because the frontend renders it directly
/// from the staged transaction; `Create` doesn't need a diff (every
/// line is `+`) but we still compute one (against an empty old content)
/// so the renderer gets a uniform shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FileEdit {
    /// Create a new file at `path` with `content`. Fails apply if the
    /// path already exists (operator can re-stage with `Modify`).
    Create {
        path: PathBuf,
        content: String,
        /// Diff against `/dev/null` → all-add unified-diff. Included so
        /// the frontend `DiffStrip` renders Create entries the same way
        /// as Modify entries.
        unified_diff: String,
        additions: usize,
        deletions: usize,
    },
    /// Replace the contents of `path` with `new_content`. Fails apply
    /// if the on-disk content has changed since the diff was computed
    /// (operator must re-stage from current content).
    Modify {
        path: PathBuf,
        /// Hash of the file's content at stage time. Apply re-reads the
        /// current content and rejects if it doesn't hash to this value.
        /// Prevents the runtime from clobbering external edits made
        /// between stage and apply.
        old_content_hash: String,
        new_content: String,
        unified_diff: String,
        additions: usize,
        deletions: usize,
    },
    /// Delete `path`. Fails apply if the on-disk content has changed
    /// since stage (same hash check as `Modify`).
    Delete {
        path: PathBuf,
        old_content_hash: String,
        /// Diff against `/dev/null` → all-delete unified-diff.
        unified_diff: String,
        additions: usize,
        deletions: usize,
    },
    /// Move `from` → `to`, optionally with content modification. Atomic
    /// rename when possible; falls back to copy + delete when the
    /// platform rejects the rename (e.g. cross-filesystem).
    Move {
        from: PathBuf,
        to: PathBuf,
        old_content_hash: String,
        /// New content; equal to old content when this is a pure rename.
        new_content: String,
        unified_diff: String,
        additions: usize,
        deletions: usize,
    },
}

impl FileEdit {
    /// Path the apply step touches on disk. For `Move`, returns the
    /// destination path (the source is removed).
    pub fn path(&self) -> &Path {
        match self {
            Self::Create { path, .. } | Self::Modify { path, .. } | Self::Delete { path, .. } => {
                path
            },
            Self::Move { to, .. } => to,
        }
    }

    /// All paths affected by this edit. `Move` returns both `from`
    /// and `to` so the snapshot step captures the source content.
    pub fn affected_paths(&self) -> Vec<&Path> {
        match self {
            Self::Create { path, .. } | Self::Modify { path, .. } | Self::Delete { path, .. } => {
                vec![path]
            },
            Self::Move { from, to, .. } => vec![from, to],
        }
    }

    /// Per-edit diff stats (matches the `DiffFile` frontend shape).
    pub fn stats(&self) -> DiffStats {
        match self {
            Self::Create {
                additions,
                deletions,
                ..
            }
            | Self::Modify {
                additions,
                deletions,
                ..
            }
            | Self::Delete {
                additions,
                deletions,
                ..
            }
            | Self::Move {
                additions,
                deletions,
                ..
            } => DiffStats {
                additions: *additions,
                deletions: *deletions,
            },
        }
    }

    /// Status letter for the `DiffFile.status` frontend field.
    pub fn status_letter(&self) -> &'static str {
        match self {
            Self::Create { .. } => "A",
            Self::Modify { .. } => "M",
            Self::Delete { .. } => "D",
            Self::Move { .. } => "R",
        }
    }

    /// Unified-diff string for this edit.
    pub fn unified_diff(&self) -> &str {
        match self {
            Self::Create { unified_diff, .. }
            | Self::Modify { unified_diff, .. }
            | Self::Delete { unified_diff, .. }
            | Self::Move { unified_diff, .. } => unified_diff,
        }
    }
}

/// Lifecycle state of a transaction. State machine:
///
/// ```text
///   Pending ─────approve────► Applied ─────revert────► Reverted
///      │                         │
///      └────reject──► Rejected   └── (terminal until revert)
/// ```
///
/// Rejected is terminal. Reverted is terminal. Applied can only
/// transition to Reverted (no re-apply after revert without re-staging).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileEditStatus {
    Pending,
    Applied,
    Rejected,
    Reverted,
}

/// Result of an idempotent decision request.
#[derive(Debug, Clone)]
pub enum DecisionTransition<T> {
    Transitioned(T),
    AlreadyResolved(T),
}

impl<T> DecisionTransition<T> {
    pub fn record(&self) -> &T {
        match self {
            Self::Transitioned(record) | Self::AlreadyResolved(record) => record,
        }
    }

    pub fn into_record(self) -> T {
        match self {
            Self::Transitioned(record) | Self::AlreadyResolved(record) => record,
        }
    }

    pub fn transitioned(&self) -> bool {
        matches!(self, Self::Transitioned(_))
    }
}

/// Typed stale-decision error returned by compatibility apply entrypoints.
#[derive(Debug, thiserror::Error)]
#[error("{record_kind} {id} decision conflict: already resolved with status {status}")]
pub struct DecisionConflict {
    pub record_kind: &'static str,
    pub id: String,
    pub status: String,
}

#[derive(Debug, thiserror::Error)]
#[error("{record_kind} {id} review revision changed before the decision claim")]
pub struct ReviewRevisionConflict {
    pub record_kind: &'static str,
    pub id: String,
}

/// Held for the complete decision critical section, including rollback.
pub struct RecordDecisionLock {
    file: File,
}

pub fn acquire_record_decision_lock(
    root: &Path,
    id: &str,
    record_kind: &str,
) -> Result<RecordDecisionLock> {
    std::fs::create_dir_all(root)
        .with_context(|| format!("create {record_kind} root {}", root.display()))?;
    let path = root.join(format!("{id}.decision.lock"));
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("open {record_kind} decision lock {}", path.display()))?;
    file.lock_exclusive()
        .with_context(|| format!("lock {record_kind} decision {}", path.display()))?;
    Ok(RecordDecisionLock { file })
}

impl Drop for RecordDecisionLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Persisted transaction record. One JSON file per transaction under
/// `<scope>/transactions/<id>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEditTransaction {
    /// Opaque uuid; used as both the on-disk filename and the
    /// `correlation_id` of the related HitlRequested event.
    pub id: TransactionId,
    /// `(principal, workspace)` the transaction is scoped to. The
    /// store enforces that apply/reject calls match the scope of the
    /// stage call.
    pub scope: TransactionScope,
    /// Owning task/execution captured from compiled-tool provenance. These
    /// identifiers are authoritative when resolving the related HITL request;
    /// a client-supplied execution id may match them but cannot redirect the
    /// terminal transition to another run. Defaults keep legacy records
    /// readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    /// Human-readable rationale rendered in the approval modal so the
    /// operator knows *why* the edits are proposed.
    pub rationale: String,
    /// One or more edits, applied in order. The frontend renders them
    /// as a single approvable card (per-file approve/reject is a
    /// future extension that splits one transaction into N).
    pub edits: Vec<FileEdit>,
    pub status: FileEditStatus,
    /// Durable decision claim written while the per-record lock is held.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decision_claim: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Set when status transitions away from `Pending`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
    /// Snapshot id captured at apply time; required for revert. Set
    /// to None on Pending / Rejected (no snapshot was needed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    /// Apply the edits relative to THIS root instead of the default scoped
    /// workspace root. Populated ONLY when the staged path was under an
    /// operator-approved sandbox root (the sandbox-override HITL added it to
    /// `session_file_sandbox_roots`) — i.e. an approved out-of-workspace write.
    /// `apply_transaction`'s containment guard is run against this root, so an
    /// approved external write lands while a symlink/`..` escape *beyond* the
    /// approved root is still rejected. `None` = the ordinary in-workspace write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apply_root: Option<PathBuf>,
}

/// A field that must be *present* and must be a JSON string, whose bytes are
/// then thrown away.
///
/// The whole basis of [`TransactionPeek`]. `serde::de::IgnoredAny` would also
/// skip the field, but it accepts any JSON value, which would let the peek
/// admit a record the full parse rejects. This accepts exactly what a `String`
/// field accepts and allocates nothing that outlives the call — which is what
/// lets a 10 MiB file body be walked past instead of materialised.
struct SkippedText;

impl<'de> Deserialize<'de> for SkippedText {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct SkippedTextVisitor;
        impl serde::de::Visitor<'_> for SkippedTextVisitor {
            type Value = SkippedText;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a string")
            }

            fn visit_str<E: serde::de::Error>(
                self,
                _: &str,
            ) -> std::result::Result<SkippedText, E> {
                Ok(SkippedText)
            }

            fn visit_string<E: serde::de::Error>(
                self,
                _: String,
            ) -> std::result::Result<SkippedText, E> {
                Ok(SkippedText)
            }
        }
        deserializer.deserialize_str(SkippedTextVisitor)
    }
}

/// [`FileEdit`] with every large field skipped.
///
/// Field-for-field identical to `FileEdit`'s variants — same tag, same
/// required keys, same types — except that `content`, `new_content` and
/// `unified_diff` are validated as strings and discarded. Keeping the shape
/// identical is what makes the peek's accept/reject decision the same one the
/// full parse would make.
// Nothing here is ever read — the variants exist so that deserialization
// *validates* the same shape the full record requires, and then discards it.
// That validation is the whole contribution.
#[allow(dead_code)]
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum FileEditPeek {
    Create {
        path: PathBuf,
        content: SkippedText,
        unified_diff: SkippedText,
        additions: usize,
        deletions: usize,
    },
    Modify {
        path: PathBuf,
        old_content_hash: String,
        new_content: SkippedText,
        unified_diff: SkippedText,
        additions: usize,
        deletions: usize,
    },
    Delete {
        path: PathBuf,
        old_content_hash: String,
        unified_diff: SkippedText,
        additions: usize,
        deletions: usize,
    },
    Move {
        from: PathBuf,
        to: PathBuf,
        old_content_hash: String,
        new_content: SkippedText,
        unified_diff: SkippedText,
        additions: usize,
        deletions: usize,
    },
}

/// [`FileEditTransaction`] read for its identity and decision state only.
///
/// **Why this exists.** `FileEditTransaction.edits` carries `new_content` —
/// the entire proposed body of each file, up to 10 MiB apiece with no cap on
/// the number of edits — plus a unified diff per edit. A hundred-file refactor
/// is therefore hundreds of megabytes of JSON, and
/// [`TransactionStore::list_pending`] deserialised all of it so that its hot
/// caller could keep the id strings and drop everything else. On an attention
/// poll that ran every time.
///
/// **Why it is safe.** Every field `FileEditTransaction` requires is required
/// here too, with the same type, so a record this accepts is a record the full
/// parse accepts and vice versa — with one exception, stated plainly: a value
/// of the wrong JSON *type* inside a skipped string field would be admitted
/// here and rejected there. The store only ever writes these records by
/// serializing the typed struct, so that shape does not occur; a hand-edited
/// file could produce it, and the consequence is one extra id in a list of
/// pending ids.
#[derive(Deserialize)]
struct TransactionPeek {
    id: TransactionId,
    #[allow(dead_code)]
    scope: TransactionScope,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    execution_id: Option<String>,
    #[allow(dead_code)]
    rationale: String,
    #[allow(dead_code)]
    edits: Vec<FileEditPeek>,
    status: FileEditStatus,
    #[serde(default)]
    decision_claim: Option<String>,
    #[allow(dead_code)]
    created_at: DateTime<Utc>,
}

/// A pending transaction reduced to what a *listing* asks about it.
///
/// Built from a peek and kept instead of the record, so a caller answering
/// "which transactions is this task holding?" holds two short strings per
/// transaction rather than the scope's proposed file contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionSummary {
    pub id: TransactionId,
    pub task_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TransactionId(String);

impl TransactionId {
    pub fn new() -> Self {
        Self(format!("txn-{}", Uuid::new_v4()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Construct a TransactionId from an operator-supplied string,
    /// validating it can only ever resolve to a filename in the
    /// scope's `transactions/` directory.
    ///
    /// Critical: the dispatcher uses the HITL response's
    /// `correlation_id` directly as the transaction id, and
    /// [`TransactionStore::load`] builds the file path via
    /// `format!("{}.json", id)`. A malicious id like
    /// `../../scopes/victim/transactions/legit-txn` traverses out of
    /// the scope sandbox and cross-reads / cross-writes other scopes'
    /// transactions.
    ///
    /// Rules (charset whitelist + length cap):
    ///   - alphanumeric (`A-Za-z0-9`), `-`, `_`
    ///   - non-empty
    ///   - <= 128 chars (long enough for `txn-<uuid>` = 40 chars +
    ///     room for future suffix; short enough that a malicious
    ///     attempt would have to fit the exploit in a tiny budget)
    pub fn parse(raw: &str) -> Result<Self> {
        if raw.is_empty() {
            return Err(anyhow!("transaction id is empty"));
        }
        if raw.len() > 128 {
            return Err(anyhow!("transaction id is {} chars; max 128", raw.len()));
        }
        for c in raw.chars() {
            if !matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_') {
                return Err(anyhow!(
                    "transaction id contains disallowed char {:?} (only [A-Za-z0-9_-] allowed)",
                    c
                ));
            }
        }
        Ok(Self(raw.to_string()))
    }
}

impl Default for TransactionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TransactionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Construct a TransactionId from a trusted `String` without
/// validation. Reserved for internal callers that built the string
/// from `TransactionId::new()` (e.g. round-tripping through serde).
/// Untrusted (operator-supplied) input MUST go through
/// [`TransactionId::parse`] instead.
impl From<String> for TransactionId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TransactionScope {
    pub principal: String,
    pub workspace: String,
}

/// Store handle. Constructed against the artifact V2 workspace's
/// `<scope>/transactions/` directory; per-scope so cross-scope
/// confusion is structurally impossible.
///
/// All methods are sync because file I/O happens on the caller's
/// thread; callers running inside async runtimes should wrap in
/// `tokio::task::spawn_blocking` to avoid stalling the executor.
/// Persisted records are small (one JSON file per transaction).
pub struct TransactionStore {
    root: PathBuf,
}

impl TransactionStore {
    /// Build a store rooted at `<scope_root>/transactions/`. The
    /// directory is created lazily on first write.
    pub fn new(scope_root: impl Into<PathBuf>) -> Self {
        Self {
            root: scope_root.into().join("transactions"),
        }
    }

    /// Stage a new transaction. The caller has already computed
    /// `new_content` for each edit; this function computes the
    /// unified diff + stats and persists the transaction record.
    ///
    /// `proposed_edits` mirrors the [`FileEdit`] variants but without
    /// the diff/stats fields — they're filled in here so callers
    /// can't forget. `read_existing` is a callback invoked for
    /// Modify/Delete/Move edits to read the current on-disk content
    /// (string-typed; binary files should be rejected upstream).
    pub fn stage(
        &self,
        scope: TransactionScope,
        rationale: impl Into<String>,
        proposed_edits: Vec<ProposedEdit>,
        read_existing: &mut dyn FnMut(&Path) -> Result<String>,
    ) -> Result<FileEditTransaction> {
        // No-op transactions aren't useful — they'd persist a JSON
        // record, take an empty snapshot at apply time, and surface
        // an empty approval modal. Reject upstream so the operator
        // and the staging code path get a clear error.
        if proposed_edits.is_empty() {
            return Err(anyhow!(
                "stage: empty edits list; transaction must propose at least one file edit"
            ));
        }
        let mut materialized = Vec::with_capacity(proposed_edits.len());
        for proposed in proposed_edits {
            materialized.push(materialize_edit(proposed, read_existing)?);
        }

        let txn = FileEditTransaction {
            id: TransactionId::new(),
            scope,
            task_id: None,
            execution_id: None,
            rationale: rationale.into(),
            edits: materialized,
            status: FileEditStatus::Pending,
            decision_claim: None,
            created_at: Utc::now(),
            resolved_at: None,
            snapshot_id: None,
            apply_root: None,
        };
        self.persist(&txn)?;
        Ok(txn)
    }

    /// Every `Pending` transaction in this scope, **as whole records**.
    ///
    /// Best-effort scan of the `<scope>/transactions/` dir: a file that fails
    /// to read/parse is skipped (never poisons the scan — tolerates
    /// malformed/legacy records), and a missing store dir yields an empty
    /// list. Mirrors `CodeChangeProposalStore::list_for_task`'s best-effort
    /// semantics.
    ///
    /// Used by the boot re-record (Trusted-Store Integrity Phase 5), which
    /// genuinely needs the whole record — it re-anchors `apply_root` and the
    /// target paths into the in-process authority. Runs **once per boot**,
    /// which is what makes materialising every proposed file body acceptable
    /// here and nowhere else.
    ///
    /// Any caller that only wants ids and ownership must use
    /// [`Self::scan_pending_summaries`].
    pub fn list_pending(&self) -> Vec<FileEditTransaction> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
            .filter_map(|e| std::fs::read(e.path()).ok())
            .filter_map(|b| serde_json::from_slice::<FileEditTransaction>(&b).ok())
            .filter(|txn| {
                matches!(txn.status, FileEditStatus::Pending) && txn.decision_claim.is_none()
            })
            .collect()
    }

    /// Every `Pending`, unclaimed transaction in this scope, as identity only.
    ///
    /// Selects exactly what [`Self::list_pending`] selects — same two filters,
    /// on the same two fields — and differs only in what it keeps and how much
    /// it has to hold to decide. See [`TransactionPeek`] for why the peek's
    /// accept/reject decision matches the full parse's.
    ///
    /// **Streamed, not slurped.** `serde_json::from_reader` over a buffered
    /// file, so a transaction record is never resident in full: peak memory is
    /// the read buffer plus the largest single skipped field, not the file. A
    /// `std::fs::read` first would have defeated the point — the bytes *are*
    /// the file contents, and a hundred-file refactor's record is hundreds of
    /// megabytes on disk.
    pub fn scan_pending_summaries(&self) -> Vec<TransactionSummary> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
            .filter_map(|e| File::open(e.path()).ok())
            .filter_map(|file| {
                serde_json::from_reader::<_, TransactionPeek>(std::io::BufReader::new(file)).ok()
            })
            .filter(|peek| {
                matches!(peek.status, FileEditStatus::Pending) && peek.decision_claim.is_none()
            })
            .map(|peek| TransactionSummary {
                id: peek.id,
                task_id: peek.task_id,
            })
            .collect()
    }

    /// Load a transaction by id. Returns `Err` when the file is
    /// missing or malformed.
    pub fn load(&self, id: &TransactionId) -> Result<FileEditTransaction> {
        let path = self.txn_path(id);
        let bytes = std::fs::read(&path)
            .with_context(|| format!("read transaction file {}", path.display()))?;
        let txn: FileEditTransaction = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse transaction file {}", path.display()))?;
        Ok(txn)
    }

    /// Persist a transaction (overwrite). Used after stage + state
    /// transitions.
    pub fn persist(&self, txn: &FileEditTransaction) -> Result<()> {
        std::fs::create_dir_all(&self.root)
            .with_context(|| format!("create transactions root {}", self.root.display()))?;
        let bytes = serde_json::to_vec_pretty(txn).context("serialize transaction")?;
        let path = self.txn_path(&txn.id);
        // Stage-and-rename through the shared durable writer, so a crash
        // mid-write doesn't leave a partial JSON record that subsequent loads
        // would error on. This used to stage through a fixed `<id>.json.tmp`
        // sibling — persist is called on every state transition of the same
        // id, so that one name is shared by overlapping writers and two of
        // them interleaving publish a half-written record. The writer also
        // `sync_all`s the record and the transactions directory.
        write_bytes_durably_sync(&path, &bytes)
            .with_context(|| format!("write transaction {}", path.display()))?;
        Ok(())
    }

    /// Transition Pending to Rejected under the per-record decision lock.
    /// Replays return `AlreadyResolved`, preserving idempotency without hiding
    /// whether this request performed the terminal transition.
    pub fn reject(&self, id: &TransactionId) -> Result<DecisionTransition<FileEditTransaction>> {
        let _lock = self.acquire_decision_lock(id.as_str())?;
        let mut txn = self.load(id)?;
        if matches!(txn.status, FileEditStatus::Pending) && txn.decision_claim.is_none() {
            txn.status = FileEditStatus::Rejected;
            txn.resolved_at = Some(Utc::now());
            self.persist(&txn)?;
            return Ok(DecisionTransition::Transitioned(txn));
        }
        Ok(DecisionTransition::AlreadyResolved(txn))
    }

    pub fn acquire_decision_lock(&self, id: &str) -> Result<RecordDecisionLock> {
        acquire_record_decision_lock(&self.root, id, "transaction")
    }

    pub fn claim_apply_locked(
        &self,
        id: &TransactionId,
        expected_review_revision: Option<blake3::Hash>,
        _lock: &RecordDecisionLock,
    ) -> Result<DecisionTransition<FileEditTransaction>> {
        let mut txn = self.load(id)?;
        if expected_review_revision.is_some_and(|expected| txn.content_hash() != expected) {
            return Err(anyhow::Error::new(ReviewRevisionConflict {
                record_kind: "transaction",
                id: id.to_string(),
            }));
        }
        if !matches!(txn.status, FileEditStatus::Pending) || txn.decision_claim.is_some() {
            return Ok(DecisionTransition::AlreadyResolved(txn));
        }
        txn.decision_claim = Some(Uuid::new_v4().to_string());
        self.persist(&txn)?;
        Ok(DecisionTransition::Transitioned(txn))
    }

    pub fn reset_pending_locked(
        &self,
        mut txn: FileEditTransaction,
        _lock: &RecordDecisionLock,
    ) -> Result<()> {
        txn.status = FileEditStatus::Pending;
        txn.decision_claim = None;
        txn.resolved_at = None;
        txn.snapshot_id = None;
        self.persist(&txn)
    }

    pub fn finish_apply_locked(
        &self,
        id: &TransactionId,
        snapshot_id: Option<String>,
        _lock: &RecordDecisionLock,
    ) -> Result<FileEditTransaction> {
        let mut txn = self.load(id)?;
        if !matches!(txn.status, FileEditStatus::Pending) || txn.decision_claim.is_none() {
            return Err(anyhow!(
                "cannot finish txn {} apply: status is {:?} and claim_present={} (must be claimed Pending)",
                id,
                txn.status,
                txn.decision_claim.is_some()
            ));
        }
        txn.status = FileEditStatus::Applied;
        txn.decision_claim = None;
        txn.resolved_at = Some(Utc::now());
        txn.snapshot_id = snapshot_id;
        self.persist(&txn)?;
        Ok(txn)
    }

    /// Mark a Pending transaction as Applied. The caller is responsible
    /// for the actual file writes + snapshot capture (those are
    /// implemented in a separate `snapshot` module that this file
    /// will depend on once it lands). This method only handles the
    /// status transition + persistence so the snapshot module can
    /// drive the lifecycle.
    pub fn mark_applied(
        &self,
        id: &TransactionId,
        snapshot_id: Option<String>,
    ) -> Result<FileEditTransaction> {
        let _lock = self.acquire_decision_lock(id.as_str())?;
        let mut txn = self.load(id)?;
        if !matches!(txn.status, FileEditStatus::Pending) || txn.decision_claim.is_some() {
            return Err(anyhow!(
                "cannot mark txn {} as applied: status is {:?} (must be Pending)",
                id,
                txn.status
            ));
        }
        txn.status = FileEditStatus::Applied;
        txn.resolved_at = Some(Utc::now());
        txn.snapshot_id = snapshot_id;
        self.persist(&txn)?;
        Ok(txn)
    }

    /// Mark an Applied transaction as Reverted. The caller is
    /// responsible for the actual snapshot restore.
    pub fn mark_reverted(&self, id: &TransactionId) -> Result<FileEditTransaction> {
        let _lock = self.acquire_decision_lock(id.as_str())?;
        let mut txn = self.load(id)?;
        if !matches!(txn.status, FileEditStatus::Applied) {
            return Err(anyhow!(
                "cannot mark txn {} as reverted: status is {:?} (must be Applied)",
                id,
                txn.status
            ));
        }
        txn.status = FileEditStatus::Reverted;
        txn.resolved_at = Some(Utc::now());
        self.persist(&txn)?;
        Ok(txn)
    }

    fn txn_path(&self, id: &TransactionId) -> PathBuf {
        self.root.join(format!("{}.json", id.as_str()))
    }
}

/// Caller-facing edit shape — the proposed change without the
/// computed diff/stats fields. Pass to [`TransactionStore::stage`]
/// which materializes the full [`FileEdit`].
#[derive(Debug, Clone)]
pub enum ProposedEdit {
    Create {
        path: PathBuf,
        content: String,
    },
    Modify {
        path: PathBuf,
        new_content: String,
    },
    Delete {
        path: PathBuf,
    },
    Move {
        from: PathBuf,
        to: PathBuf,
        new_content: String,
    },
}

fn materialize_edit(
    proposed: ProposedEdit,
    read_existing: &mut dyn FnMut(&Path) -> Result<String>,
) -> Result<FileEdit> {
    // Size-guard wrapper around the caller's `read_existing`
    // callback. Callers commonly implement read_existing as
    // `std::fs::read_to_string` which is unbounded; a multi-GB file
    // would OOM the staging process. We re-check the returned string
    // length so misbehaving callbacks (and tests that hand-roll
    // their own readers) can't bypass the cap.
    let mut bounded_read = |path: &Path| -> Result<String> {
        let content = read_existing(path)?;
        if content.len() as u64 > MAX_FILE_READ_BYTES {
            return Err(anyhow!(
                "read_existing returned {} bytes for {}; cap is {} bytes \
                 (file too large to stage; reject at the tool layer or use a streaming surface)",
                content.len(),
                path.display(),
                MAX_FILE_READ_BYTES
            ));
        }
        Ok(content)
    };
    match proposed {
        ProposedEdit::Create { path, content } => {
            if content.len() as u64 > MAX_FILE_READ_BYTES {
                return Err(anyhow!(
                    "Create content for {} is {} bytes; cap is {} bytes",
                    path.display(),
                    content.len(),
                    MAX_FILE_READ_BYTES
                ));
            }
            let unified = compute_unified_diff("", &content, path.to_string_lossy().as_ref());
            let stats = diff_stats(&unified);
            Ok(FileEdit::Create {
                path,
                content,
                unified_diff: unified,
                additions: stats.additions,
                deletions: stats.deletions,
            })
        },
        ProposedEdit::Modify { path, new_content } => {
            if new_content.len() as u64 > MAX_FILE_READ_BYTES {
                return Err(anyhow!(
                    "Modify new_content for {} is {} bytes; cap is {} bytes",
                    path.display(),
                    new_content.len(),
                    MAX_FILE_READ_BYTES
                ));
            }
            let old = bounded_read(&path)
                .with_context(|| format!("read existing content for {}", path.display()))?;
            let unified = compute_unified_diff(&old, &new_content, path.to_string_lossy().as_ref());
            let stats = diff_stats(&unified);
            Ok(FileEdit::Modify {
                old_content_hash: blake3_hex(&old),
                new_content,
                path,
                unified_diff: unified,
                additions: stats.additions,
                deletions: stats.deletions,
            })
        },
        ProposedEdit::Delete { path } => {
            let old = bounded_read(&path)
                .with_context(|| format!("read existing content for {}", path.display()))?;
            let unified = compute_unified_diff(&old, "", path.to_string_lossy().as_ref());
            let stats = diff_stats(&unified);
            Ok(FileEdit::Delete {
                old_content_hash: blake3_hex(&old),
                path,
                unified_diff: unified,
                additions: stats.additions,
                deletions: stats.deletions,
            })
        },
        ProposedEdit::Move {
            from,
            to,
            new_content,
        } => {
            if new_content.len() as u64 > MAX_FILE_READ_BYTES {
                return Err(anyhow!(
                    "Move new_content for {} is {} bytes; cap is {} bytes",
                    to.display(),
                    new_content.len(),
                    MAX_FILE_READ_BYTES
                ));
            }
            let old = bounded_read(&from)
                .with_context(|| format!("read existing content for {}", from.display()))?;
            let unified = compute_unified_diff(&old, &new_content, to.to_string_lossy().as_ref());
            let stats = diff_stats(&unified);
            Ok(FileEdit::Move {
                from,
                to,
                old_content_hash: blake3_hex(&old),
                new_content,
                unified_diff: unified,
                additions: stats.additions,
                deletions: stats.deletions,
            })
        },
    }
}

fn blake3_hex(content: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(content.as_bytes());
    hasher.finalize().to_hex().to_string()
}

/// Per-file payload shape mirroring the frontend `DiffFile`. Used by
/// the HITL emission path to populate `diff_approval` requests
/// directly from a staged transaction without re-reading from disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffApprovalFile {
    pub path: String,
    pub status: String,
    pub additions: usize,
    pub deletions: usize,
    pub unified_diff: String,
}

impl From<&FileEdit> for DiffApprovalFile {
    fn from(edit: &FileEdit) -> Self {
        Self {
            path: edit.path().to_string_lossy().into_owned(),
            status: edit.status_letter().to_string(),
            additions: edit.stats().additions,
            deletions: edit.stats().deletions,
            unified_diff: edit.unified_diff().to_string(),
        }
    }
}

impl FileEditTransaction {
    pub fn decision_claimed(&self) -> bool {
        self.decision_claim.is_some()
    }

    /// Per-file payloads for the `HitlRequested { input_type:
    /// "diff_approval", payload }` event. Stable serialization shape
    /// so the frontend's `DiffStrip` consumes the payload without
    /// custom adapters.
    pub fn diff_approval_files(&self) -> Vec<DiffApprovalFile> {
        self.edits.iter().map(DiffApprovalFile::from).collect()
    }
}

/// Aggregate stats across all edits in a transaction.
impl FileEditTransaction {
    pub fn total_stats(&self) -> DiffStats {
        self.edits.iter().fold(
            DiffStats {
                additions: 0,
                deletions: 0,
            },
            |mut acc, edit| {
                let s = edit.stats();
                acc.additions += s.additions;
                acc.deletions += s.deletions;
                acc
            },
        )
    }

    /// Map of path → edit for callers that need to look up by path
    /// (e.g. the snapshot module deciding which files to capture).
    pub fn edits_by_path(&self) -> HashMap<&Path, &FileEdit> {
        self.edits.iter().map(|e| (e.path(), e)).collect()
    }

    /// Stable review revision over scope, task/execution provenance, rationale,
    /// edits, and apply root. The edits are the exact source of the displayed
    /// diff payload, including destinations and old/new content hashes.
    ///
    /// Trusted-store integrity: the stage site records this into the in-process
    /// authority, and the diff-approval decide site recomputes it on the loaded
    /// on-disk record and compares. Any provenance, review text, destination,
    /// content, or root change therefore fails closed.
    pub fn content_hash(&self) -> blake3::Hash {
        let bytes = serde_json::to_vec(&(
            &self.scope,
            &self.task_id,
            &self.execution_id,
            &self.rationale,
            &self.edits,
            &self.apply_root,
        ))
        .unwrap_or_default();
        blake3::hash(&bytes)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use std::collections::HashMap as StdHashMap;

    fn temp_scope_root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("file-edit-test-{}", Uuid::new_v4()));
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
    fn stage_persists_and_loads_round_trip() {
        let root = temp_scope_root();
        let store = TransactionStore::new(&root);
        let txn = store
            .stage(
                scope(),
                "test rationale",
                vec![ProposedEdit::Create {
                    path: PathBuf::from("foo.txt"),
                    content: "hello\n".into(),
                }],
                &mut |_| unreachable!("Create doesn't read existing"),
            )
            .unwrap();

        assert_eq!(txn.status, FileEditStatus::Pending);
        assert_eq!(txn.edits.len(), 1);
        let loaded = store.load(&txn.id).unwrap();
        assert_eq!(loaded.id, txn.id);
        assert_eq!(loaded.rationale, "test rationale");
        assert_eq!(loaded.edits.len(), 1);
    }

    /// `list_pending` and `scan_pending_summaries` both walk the transactions
    /// directory. Persist runs on every state transition of the same id, so a
    /// staging sibling would accumulate there and be re-read on every poll.
    #[test]
    fn persist_publishes_without_leaving_a_staging_sibling() {
        let root = temp_scope_root();
        let store = TransactionStore::new(&root);
        let txn = store
            .stage(
                scope(),
                "staging check",
                vec![ProposedEdit::Create {
                    path: PathBuf::from("foo.txt"),
                    content: "hello\n".into(),
                }],
                &mut |_| unreachable!("Create doesn't read existing"),
            )
            .unwrap();
        // A second persist of the same id — the transition path.
        store.reject(&txn.id).unwrap();

        let entries = std::fs::read_dir(root.join("transactions"))
            .expect("transactions listing")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect::<Vec<_>>();
        assert!(
            entries.is_empty(),
            "persist must leave no staging sibling, found {entries:?}"
        );
        assert_eq!(
            store.load(&txn.id).unwrap().status,
            FileEditStatus::Rejected
        );
    }

    #[test]
    fn modify_records_old_content_hash() {
        let root = temp_scope_root();
        let store = TransactionStore::new(&root);
        let mut existing: StdHashMap<PathBuf, String> = StdHashMap::new();
        existing.insert(PathBuf::from("src/foo.rs"), "let x = 1;\n".into());

        let txn = store
            .stage(
                scope(),
                "bump x",
                vec![ProposedEdit::Modify {
                    path: PathBuf::from("src/foo.rs"),
                    new_content: "let x = 2;\n".into(),
                }],
                &mut |p| {
                    existing
                        .get(p)
                        .cloned()
                        .ok_or_else(|| anyhow!("not found: {}", p.display()))
                },
            )
            .unwrap();

        let edit = &txn.edits[0];
        match edit {
            FileEdit::Modify {
                old_content_hash,
                unified_diff,
                ..
            } => {
                assert!(!old_content_hash.is_empty());
                assert!(unified_diff.contains("-let x = 1;"));
                assert!(unified_diff.contains("+let x = 2;"));
            },
            other => panic!("expected Modify, got {other:?}"),
        }
        assert_eq!(edit.status_letter(), "M");
    }

    #[test]
    fn reject_transitions_pending_to_rejected() {
        let root = temp_scope_root();
        let store = TransactionStore::new(&root);
        let txn = store
            .stage(
                scope(),
                "",
                vec![ProposedEdit::Create {
                    path: PathBuf::from("a.txt"),
                    content: "x".into(),
                }],
                &mut |_| unreachable!(),
            )
            .unwrap();
        let after = store.reject(&txn.id).unwrap();
        assert!(after.transitioned());
        assert_eq!(after.record().status, FileEditStatus::Rejected);
        assert!(after.record().resolved_at.is_some());

        let after2 = store.reject(&txn.id).unwrap();
        assert!(matches!(after2, DecisionTransition::AlreadyResolved(_)));
        assert_eq!(after2.record().status, FileEditStatus::Rejected);
    }

    #[test]
    fn mark_applied_transitions_pending_to_applied() {
        let root = temp_scope_root();
        let store = TransactionStore::new(&root);
        let txn = store
            .stage(
                scope(),
                "",
                vec![ProposedEdit::Create {
                    path: PathBuf::from("a.txt"),
                    content: "x".into(),
                }],
                &mut |_| unreachable!(),
            )
            .unwrap();
        let after = store
            .mark_applied(&txn.id, Some("snap-123".into()))
            .unwrap();
        assert_eq!(after.status, FileEditStatus::Applied);
        assert_eq!(after.snapshot_id.as_deref(), Some("snap-123"));
    }

    #[test]
    fn mark_applied_rejects_non_pending() {
        let root = temp_scope_root();
        let store = TransactionStore::new(&root);
        let txn = store
            .stage(
                scope(),
                "",
                vec![ProposedEdit::Create {
                    path: PathBuf::from("a.txt"),
                    content: "x".into(),
                }],
                &mut |_| unreachable!(),
            )
            .unwrap();
        store.reject(&txn.id).unwrap();
        let err = store.mark_applied(&txn.id, None).unwrap_err();
        assert!(err.to_string().contains("must be Pending"), "got: {err}");
    }

    #[test]
    fn diff_approval_files_match_frontend_shape() {
        let root = temp_scope_root();
        let store = TransactionStore::new(&root);
        let txn = store
            .stage(
                scope(),
                "",
                vec![ProposedEdit::Create {
                    path: PathBuf::from("alpha.txt"),
                    content: "one\ntwo\n".into(),
                }],
                &mut |_| unreachable!(),
            )
            .unwrap();

        let payloads = txn.diff_approval_files();
        assert_eq!(payloads.len(), 1);
        let p = &payloads[0];
        assert_eq!(p.path, "alpha.txt");
        assert_eq!(p.status, "A");
        assert_eq!(p.additions, 2);
        assert_eq!(p.deletions, 0);
        assert!(p.unified_diff.contains("+one"));
    }

    #[test]
    fn transaction_id_parse_accepts_normal_uuid_shape() {
        let id = TransactionId::parse("txn-f47ac10b-58cc-4372-a567-0e02b2c3d479").unwrap();
        assert_eq!(id.as_str(), "txn-f47ac10b-58cc-4372-a567-0e02b2c3d479");
    }

    #[test]
    fn transaction_id_parse_rejects_path_traversal() {
        // CRITICAL security test — the dispatcher passes the
        // operator-supplied correlation_id through this parser
        // before using it as a transactions/ filename. If `..`
        // slips through, an attacker could cross-read or
        // cross-write other scopes' transaction records.
        let cases = [
            "../../etc/passwd",
            "../../../scopes/victim/workspace/transactions/legit-txn",
            "txn-with/slash",
            "txn\\with\\backslash",
            "txn with space",
            "txn:with:colon",
            "..",
            ".",
            "",
        ];
        for raw in cases {
            assert!(
                TransactionId::parse(raw).is_err(),
                "expected {} to be rejected",
                raw
            );
        }
    }

    #[test]
    fn stage_rejects_oversized_create_content() {
        // 11MB content — over the 10MB MAX_FILE_READ_BYTES cap. Without
        // the guard, the unified-diff computation + JSON persist would
        // hold the whole content in memory; a multi-GB content would
        // OOM the process.
        let root = temp_scope_root();
        let store = TransactionStore::new(&root);
        let huge = "x".repeat(11 * 1024 * 1024);
        let err = store
            .stage(
                scope(),
                "oversized",
                vec![ProposedEdit::Create {
                    path: PathBuf::from("huge.bin"),
                    content: huge,
                }],
                &mut |_| unreachable!(),
            )
            .unwrap_err();
        assert!(
            err.to_string().contains("cap") || err.to_string().contains("bytes"),
            "got: {err}"
        );
    }

    #[test]
    fn stage_rejects_oversized_read_existing_return() {
        // Defensive: even if a caller's `read_existing` callback
        // returns a huge string (e.g. they wrote their own reader
        // without using the bounded helper), the staging guard
        // catches it. Prevents misbehaving callbacks from
        // bypassing the cap.
        let root = temp_scope_root();
        let store = TransactionStore::new(&root);
        let err = store
            .stage(
                scope(),
                "rogue callback",
                vec![ProposedEdit::Modify {
                    path: PathBuf::from("foo.txt"),
                    new_content: "small".into(),
                }],
                &mut |_| Ok("x".repeat(11 * 1024 * 1024)),
            )
            .unwrap_err();
        let chained = format!("{err:#}");
        assert!(
            chained.contains("cap") || chained.contains("bytes"),
            "got: {chained}"
        );
    }

    #[test]
    fn stage_rejects_empty_edits() {
        // No-op transactions waste a JSON record + an empty snapshot
        // + an empty approval modal. Reject at the staging API
        // boundary instead.
        let root = temp_scope_root();
        let store = TransactionStore::new(&root);
        let err = store
            .stage(scope(), "no-op", vec![], &mut |_| unreachable!())
            .unwrap_err();
        assert!(err.to_string().contains("empty edits"), "got: {err}");
    }

    #[test]
    fn transaction_id_parse_rejects_overlong() {
        let raw = "a".repeat(129);
        assert!(TransactionId::parse(&raw).is_err());
        let raw128 = "b".repeat(128);
        assert!(TransactionId::parse(&raw128).is_ok());
    }

    // ── Trusted-store integrity: content-hash sensitivity locks ──────────
    //
    // These are UNIT-level locks of the mechanism's building blocks. The FULL
    // end-to-end HTTP regression (stage a real txn → POST the diff-approval to
    // `respond_hitl_handler` → assert forged/tampered/restart all refuse; assert
    // a legit approve applies) is an INTEGRATION test that must be written and
    // RUN once the crate compiles.
    //
    // TODO(trusted-store): end-to-end HTTP stage→approve→apply integration
    // regression — write + run when the crate compiles.

    /// `content_hash()` is deterministic — two calls on the same transaction
    /// yield the same hash. The decide site recomputes this on the loaded
    /// on-disk record and compares against the authority entry, so any
    /// nondeterminism would false-fail every legitimate apply.
    #[test]
    fn content_hash_is_deterministic() {
        let root = temp_scope_root();
        let store = TransactionStore::new(&root);
        let txn = store
            .stage(
                scope(),
                "deterministic",
                vec![ProposedEdit::Create {
                    path: PathBuf::from("foo.txt"),
                    content: "hello\nworld\n".into(),
                }],
                &mut |_| unreachable!("Create doesn't read existing"),
            )
            .unwrap();

        assert_eq!(
            txn.content_hash(),
            txn.content_hash(),
            "content_hash must be stable across repeated calls"
        );
    }

    /// Forging an edit's CONTENT or destination PATH changes `content_hash()`.
    /// This is the exact property that makes the decide site's
    /// `diff_approval_content_tampered` refusal fire: if the on-disk `edits`
    /// JSON is edited out of band between stage and apply, the recomputed hash
    /// no longer matches the authority entry recorded at stage time, so apply
    /// fails closed.
    #[test]
    fn content_hash_detects_forged_edits() {
        let root = temp_scope_root();
        let store = TransactionStore::new(&root);
        let baseline = store
            .stage(
                scope(),
                "baseline",
                vec![ProposedEdit::Create {
                    path: PathBuf::from("foo.txt"),
                    content: "hello\n".into(),
                }],
                &mut |_| unreachable!(),
            )
            .unwrap();
        let h1 = baseline.content_hash();

        // Second txn identical except the edit's CONTENT differs.
        let mut forged_content = baseline.clone();
        forged_content.edits = vec![match &baseline.edits[0] {
            FileEdit::Create {
                path,
                unified_diff,
                additions,
                deletions,
                ..
            } => FileEdit::Create {
                path: path.clone(),
                content: "forged\n".into(),
                unified_diff: unified_diff.clone(),
                additions: *additions,
                deletions: *deletions,
            },
            other => panic!("expected Create, got {other:?}"),
        }];
        assert_ne!(
            forged_content.content_hash(),
            h1,
            "changing an edit's content MUST change content_hash"
        );

        // Third txn identical except the edit's destination PATH differs.
        let mut forged_path = baseline.clone();
        forged_path.edits = vec![match &baseline.edits[0] {
            FileEdit::Create {
                content,
                unified_diff,
                additions,
                deletions,
                ..
            } => FileEdit::Create {
                path: PathBuf::from("evil.txt"),
                content: content.clone(),
                unified_diff: unified_diff.clone(),
                additions: *additions,
                deletions: *deletions,
            },
            other => panic!("expected Create, got {other:?}"),
        }];
        assert_ne!(
            forged_path.content_hash(),
            h1,
            "changing an edit's destination path MUST change content_hash"
        );
    }

    #[test]
    fn total_stats_sums_across_edits() {
        let root = temp_scope_root();
        let store = TransactionStore::new(&root);
        let txn = store
            .stage(
                scope(),
                "",
                vec![
                    ProposedEdit::Create {
                        path: PathBuf::from("a.txt"),
                        content: "1\n2\n3\n".into(),
                    },
                    ProposedEdit::Create {
                        path: PathBuf::from("b.txt"),
                        content: "x\ny\n".into(),
                    },
                ],
                &mut |_| unreachable!(),
            )
            .unwrap();
        let stats = txn.total_stats();
        assert_eq!(stats.additions, 5);
        assert_eq!(stats.deletions, 0);
    }

    #[test]
    fn content_hash_detects_forged_scope_and_run_provenance() {
        let root = temp_scope_root();
        let store = TransactionStore::new(&root);
        let mut baseline = store
            .stage(
                scope(),
                "provenance",
                vec![ProposedEdit::Create {
                    path: PathBuf::from("foo.txt"),
                    content: "hello\n".into(),
                }],
                &mut |_| unreachable!(),
            )
            .unwrap();
        baseline.task_id = Some("task-1".into());
        baseline.execution_id = Some("exec-1".into());
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
    }

    /// The peek exists so the hot caller never materialises a scope's proposed
    /// file bodies. It is only allowed to exist if it picks the *same*
    /// transactions the full parse picks — so assert that against a store
    /// holding every state and every edit variant, rather than trusting the
    /// two filters look alike.
    #[test]
    fn the_pending_peek_selects_exactly_what_the_full_parse_selects() {
        let root = temp_scope_root();
        let store = TransactionStore::new(&root);
        let mut existing: StdHashMap<PathBuf, String> = StdHashMap::new();
        existing.insert(PathBuf::from("src/a.rs"), "let a = 1;\n".into());
        existing.insert(PathBuf::from("src/b.rs"), "let b = 1;\n".into());
        existing.insert(PathBuf::from("src/c.rs"), "let c = 1;\n".into());
        let mut read = |p: &Path| {
            existing
                .get(p)
                .cloned()
                .ok_or_else(|| anyhow!("not found: {}", p.display()))
        };

        // Every `FileEdit` variant, so the peek's mirrored enum is exercised
        // on each tag rather than only on the one the store happens to write
        // most often.
        let pending = store
            .stage(
                scope(),
                "all four variants",
                vec![
                    ProposedEdit::Create {
                        path: PathBuf::from("src/new.rs"),
                        content: "fn new() {}\n".into(),
                    },
                    ProposedEdit::Modify {
                        path: PathBuf::from("src/a.rs"),
                        new_content: "let a = 2;\n".into(),
                    },
                    ProposedEdit::Delete {
                        path: PathBuf::from("src/b.rs"),
                    },
                    ProposedEdit::Move {
                        from: PathBuf::from("src/c.rs"),
                        to: PathBuf::from("src/d.rs"),
                        new_content: "let c = 2;\n".into(),
                    },
                ],
                &mut read,
            )
            .unwrap();

        // Owned by a task, so the by-task half of the summary is covered too.
        let mut owned = store
            .stage(
                scope(),
                "owned",
                vec![ProposedEdit::Create {
                    path: PathBuf::from("owned.txt"),
                    content: "x\n".into(),
                }],
                &mut |_| unreachable!(),
            )
            .unwrap();
        owned.task_id = Some("task-7".into());
        store.persist(&owned).unwrap();

        // Rejected: not pending, so neither call may return it.
        let rejected = store
            .stage(
                scope(),
                "rejected",
                vec![ProposedEdit::Create {
                    path: PathBuf::from("rejected.txt"),
                    content: "x\n".into(),
                }],
                &mut |_| unreachable!(),
            )
            .unwrap();
        store.reject(&rejected.id).unwrap();

        // Pending but claimed: a terminal decision is already in flight, so
        // neither call may return it either.
        let claimed = store
            .stage(
                scope(),
                "claimed",
                vec![ProposedEdit::Create {
                    path: PathBuf::from("claimed.txt"),
                    content: "x\n".into(),
                }],
                &mut |_| unreachable!(),
            )
            .unwrap();
        {
            let lock = store.acquire_decision_lock(claimed.id.as_str()).unwrap();
            store.claim_apply_locked(&claimed.id, None, &lock).unwrap();
        }

        // Unparseable by either: the best-effort skip must be the same skip.
        std::fs::write(root.join("transactions/not-json.json"), b"{oh no").unwrap();

        let mut from_records: Vec<String> = store
            .list_pending()
            .into_iter()
            .map(|txn| txn.id.as_str().to_string())
            .collect();
        let mut from_peek: Vec<String> = store
            .scan_pending_summaries()
            .into_iter()
            .map(|summary| summary.id.as_str().to_string())
            .collect();
        from_records.sort();
        from_peek.sort();

        assert_eq!(
            from_peek, from_records,
            "the peek must select exactly the transactions the full parse selects"
        );
        let mut expected = vec![
            pending.id.as_str().to_string(),
            owned.id.as_str().to_string(),
        ];
        expected.sort();
        assert_eq!(from_peek, expected);

        // Ownership survives the peek — it is the other field the summary
        // exists to carry.
        let owner = store
            .scan_pending_summaries()
            .into_iter()
            .find(|summary| summary.id == owned.id)
            .expect("the owned transaction is pending");
        assert_eq!(owner.task_id.as_deref(), Some("task-7"));
        let unowned = store
            .scan_pending_summaries()
            .into_iter()
            .find(|summary| summary.id == pending.id)
            .expect("the four-variant transaction is pending");
        assert_eq!(unowned.task_id, None);
    }
}
