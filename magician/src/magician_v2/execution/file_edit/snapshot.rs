//! Pre-apply file content snapshots for transaction revert.
//!
//! When [`crate::magician_v2::execution::file_edit::transaction::TransactionStore::mark_applied`]
//! is about to flip a transaction from `Pending` to `Applied`, the
//! runtime first calls [`SnapshotStore::capture`] to record the
//! current on-disk contents of every affected path. The snapshot id
//! lands on the transaction record. Revert later restores from that
//! snapshot via [`SnapshotStore::restore`].
//!
//! ## Design choices
//!
//! - **One JSON file per snapshot** under `<scope>/snapshots/<id>.json`,
//!   parallel to the transactions layout. Easy to inspect, easy to
//!   prune (just `rm` a file), no external state store needed.
//! - **Full content snapshots, not just diffs.** A diff-based revert
//!   would need the post-apply content to reverse-patch, which we
//!   already have on the transaction record — but storing the
//!   pre-apply content is simpler and more robust to subsequent
//!   external edits. Cost is the extra disk space; bounded by
//!   `MAX_SNAPSHOT_BYTES` (10MB) to prevent runaway growth.
//! - **Missing files captured as `None`.** When the transaction
//!   creates a new file, the snapshot records "this path did not
//!   exist" so revert deletes it instead of writing empty content.
//! - **No git integration in v1.** A future enhancement could detect
//!   when the scope is inside a git repo and use `git stash` for
//!   cheaper snapshots, but v1 stays plain-file-based for portability
//!   (scopes that aren't repos still need revert support).
//!
//! ## Apply + capture flow
//!
//! Atomicity guarantee: [`apply_transaction`] takes the snapshot
//! *before* writing any file, so a crash mid-apply leaves the
//! transaction in `Pending` and the snapshot exists for revert. The
//! snapshot id flips onto the transaction record only after every
//! write succeeds.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::transaction::{
    DecisionConflict, DecisionTransition, FileEdit, FileEditTransaction, TransactionStore,
};
use crate::magician_v2::artifact_v2::io::write_bytes_durably_sync;

/// Hard cap on per-snapshot bytes. Prevents one runaway transaction
/// (e.g. someone proposing a 100MB binary blob diff) from filling
/// the snapshots directory. 10MB is generous for text edits; binary
/// edits are rejected before reaching the snapshot step anyway.
const MAX_SNAPSHOT_BYTES: usize = 10 * 1024 * 1024;

/// Per-file size cap on `read_to_string` calls inside the file_edit
/// module. Without this, a tool proposing a Modify on a multi-GB
/// file (build artifact, dataset, mis-staged binary) would read the
/// whole content into memory before any size check fires — OOMing
/// the process. The cap is checked via file metadata BEFORE reading
/// so a 2GB file errors immediately instead of consuming 2GB of RAM.
///
/// 10MB matches the aggregate snapshot cap and is generous for any
/// real source file. Code edits routinely fit in single-digit KB;
/// rejecting at 10MB is a structural sanity check, not a UX
/// limitation. Operators with legitimate large-file workflows would
/// need a different tool surface (streaming write, chunked apply)
/// that's out of scope for v1.
pub const MAX_FILE_READ_BYTES: u64 = 10 * 1024 * 1024;

/// Read a file with the per-file size guard. Use this anywhere in
/// the file_edit module instead of `std::fs::read_to_string` so the
/// cap is enforced consistently. Returns `Err` immediately when the
/// file metadata reports a size over the cap (no bytes read).
pub(super) fn read_text_bounded(path: &Path) -> Result<String> {
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("stat {} for size check", path.display()))?;
    if metadata.len() > MAX_FILE_READ_BYTES {
        return Err(anyhow!(
            "file {} is {} bytes; refusing to read (cap {} bytes). \
             Reject edits on large files at the tool layer or use a streaming surface.",
            path.display(),
            metadata.len(),
            MAX_FILE_READ_BYTES
        ));
    }
    std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SnapshotId(String);

impl SnapshotId {
    pub fn new() -> Self {
        Self(format!("snap-{}", Uuid::new_v4()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Construct a SnapshotId from an untrusted string with strict
    /// charset validation. Same threat model as
    /// [`super::transaction::TransactionId::parse`]: snapshot_id is
    /// stored on the transaction record, and a tampered transaction
    /// JSON could inject a `..` traversal that
    /// [`SnapshotStore::load`] would dereference outside the scope's
    /// `snapshots/` directory.
    ///
    /// Rules: alphanumeric + `-`/`_`, 1..=128 chars.
    pub fn parse(raw: &str) -> Result<Self> {
        if raw.is_empty() {
            return Err(anyhow!("snapshot id is empty"));
        }
        if raw.len() > 128 {
            return Err(anyhow!("snapshot id is {} chars; max 128", raw.len()));
        }
        for c in raw.chars() {
            if !matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_') {
                return Err(anyhow!("snapshot id contains disallowed char {:?}", c));
            }
        }
        Ok(Self(raw.to_string()))
    }
}

impl Default for SnapshotId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SnapshotId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Trusted-source conversion only; untrusted input MUST go through
/// [`SnapshotId::parse`].
impl From<String> for SnapshotId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// One captured file's pre-apply state. `content: None` means "the
/// file did not exist at capture time" — revert deletes the path in
/// that case rather than writing empty content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotFile {
    pub path: PathBuf,
    /// UTF-8 file content, or `None` for files that didn't exist at
    /// capture time. Binary files should be rejected at the
    /// transaction-stage layer; if one slips through, `read_to_string`
    /// fails and we record the file as missing — revert will then
    /// delete it, which is the right behavior for the "was actually
    /// a binary that we shouldn't have touched" recovery case.
    pub content: Option<String>,
}

/// Persisted snapshot record. One JSON file per snapshot under
/// `<scope>/snapshots/<id>.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: SnapshotId,
    /// Transaction this snapshot was captured for. Mostly informational
    /// — the transaction record also points at the snapshot id, so
    /// the link is bidirectional.
    pub transaction_id: String,
    pub files: Vec<SnapshotFile>,
    pub created_at: DateTime<Utc>,
}

pub struct SnapshotStore {
    root: PathBuf,
}

impl SnapshotStore {
    /// Build a store rooted at `<scope_root>/snapshots/`. Directory
    /// is created lazily on first write.
    pub fn new(scope_root: impl Into<PathBuf>) -> Self {
        Self {
            root: scope_root.into().join("snapshots"),
        }
    }

    /// Capture the current on-disk content for every path in `paths`.
    /// Paths that don't exist are recorded as `content: None` (a
    /// successful capture, not an error — the create case is normal).
    /// Returns the snapshot id; the snapshot itself is persisted on
    /// disk.
    ///
    /// `transaction_id` is recorded on the snapshot so the
    /// snapshot ↔ transaction link is bidirectional.
    pub fn capture(
        &self,
        transaction_id: impl Into<String>,
        paths: &[PathBuf],
    ) -> Result<SnapshotId> {
        let mut files = Vec::with_capacity(paths.len());
        let mut total_bytes = 0usize;
        for path in paths {
            // Use bounded read so a single multi-GB file errors via
            // stat (cheap) instead of OOM-ing the process via
            // unbounded read. Missing files still record `content:
            // None` (correct revert semantic for the create case).
            let content = if !path.exists() {
                None
            } else {
                match read_text_bounded(path) {
                    Ok(content) => Some(content),
                    Err(err) => {
                        return Err(err)
                            .with_context(|| format!("snapshot: capture {}", path.display()));
                    },
                }
            };
            if let Some(c) = content.as_ref() {
                total_bytes = total_bytes.saturating_add(c.len());
                if total_bytes > MAX_SNAPSHOT_BYTES {
                    return Err(anyhow!(
                        "snapshot would exceed {MAX_SNAPSHOT_BYTES} bytes cap; refusing to capture"
                    ));
                }
            }
            files.push(SnapshotFile {
                path: path.clone(),
                content,
            });
        }
        let snap = Snapshot {
            id: SnapshotId::new(),
            transaction_id: transaction_id.into(),
            files,
            created_at: Utc::now(),
        };
        self.persist(&snap)?;
        Ok(snap.id)
    }

    /// Restore every path in the snapshot to its captured state.
    /// `content: None` entries are deleted; `content: Some` entries
    /// are written with parent-directory creation. Errors are
    /// collected and reported with the first failure as the cause —
    /// partial restores are possible but the operator can re-run
    /// once the underlying issue is fixed (snapshots are persistent
    /// until explicitly pruned).
    pub fn restore(&self, id: &SnapshotId) -> Result<()> {
        let snap = self.load(id)?;
        for file in &snap.files {
            match &file.content {
                Some(content) => {
                    if let Some(parent) = file.path.parent() {
                        if !parent.as_os_str().is_empty() {
                            std::fs::create_dir_all(parent).with_context(|| {
                                format!("create parent dir {}", parent.display())
                            })?;
                        }
                    }
                    std::fs::write(&file.path, content)
                        .with_context(|| format!("restore write {}", file.path.display()))?;
                },
                None => match std::fs::remove_file(&file.path) {
                    Ok(()) => {},
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {},
                    Err(err) => {
                        return Err(err)
                            .with_context(|| format!("restore delete {}", file.path.display()));
                    },
                },
            }
        }
        Ok(())
    }

    pub fn load(&self, id: &SnapshotId) -> Result<Snapshot> {
        let path = self.snap_path(id);
        let bytes = std::fs::read(&path)
            .with_context(|| format!("read snapshot file {}", path.display()))?;
        let snap: Snapshot = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse snapshot file {}", path.display()))?;
        Ok(snap)
    }

    fn persist(&self, snap: &Snapshot) -> Result<()> {
        std::fs::create_dir_all(&self.root)
            .with_context(|| format!("create snapshots root {}", self.root.display()))?;
        let bytes = serde_json::to_vec_pretty(snap).context("serialize snapshot")?;
        let path = self.snap_path(&snap.id);
        // The shared durable writer, not a fixed `<id>.json.tmp` sibling. The
        // snapshot is the only copy of the pre-apply content, so a half-written
        // record loses the ability to revert the apply it was captured for; the
        // writer stages through a per-write unique name and `sync_all`s both the
        // record and the snapshots directory.
        write_bytes_durably_sync(&path, &bytes)
            .with_context(|| format!("write snapshot {}", path.display()))?;
        Ok(())
    }

    fn snap_path(&self, id: &SnapshotId) -> PathBuf {
        self.root.join(format!("{}.json", id.as_str()))
    }
}

/// High-level apply: capture snapshot → write every file → mark
/// transaction Applied. The single entrypoint a HITL dispatcher
/// should call on operator approval; encapsulates the
/// atomicity-guarantee rollback logic.
///
/// Path resolution: every edit's path is treated as relative to the
/// `workspace_root` argument so the runtime never writes outside the
/// scope's workspace tree by accident. Absolute paths in edits are
/// rejected as a security guard.
pub fn apply_transaction(
    transactions: &TransactionStore,
    snapshots: &SnapshotStore,
    transaction_id: &super::transaction::TransactionId,
    workspace_root: &Path,
) -> Result<()> {
    match apply_transaction_decision(transactions, snapshots, transaction_id, workspace_root)? {
        DecisionTransition::Transitioned(_) => Ok(()),
        DecisionTransition::AlreadyResolved(txn) => Err(anyhow::Error::new(DecisionConflict {
            record_kind: "transaction",
            id: transaction_id.to_string(),
            status: format!("{:?}", txn.status),
        })),
    }
}

/// Apply with an explicit stale-decision outcome for HTTP decision handlers.
pub fn apply_transaction_decision(
    transactions: &TransactionStore,
    snapshots: &SnapshotStore,
    transaction_id: &super::transaction::TransactionId,
    workspace_root: &Path,
) -> Result<DecisionTransition<FileEditTransaction>> {
    apply_transaction_decision_inner(
        transactions,
        snapshots,
        transaction_id,
        workspace_root,
        None,
    )
}

/// Revision-pinned apply. The revision is checked after taking the record lock
/// and before writing the durable claim, closing the verifier-to-apply gap.
pub fn apply_transaction_verified_decision(
    transactions: &TransactionStore,
    snapshots: &SnapshotStore,
    transaction_id: &super::transaction::TransactionId,
    workspace_root: &Path,
    expected_review_revision: blake3::Hash,
) -> Result<DecisionTransition<FileEditTransaction>> {
    apply_transaction_decision_inner(
        transactions,
        snapshots,
        transaction_id,
        workspace_root,
        Some(expected_review_revision),
    )
}

fn apply_transaction_decision_inner(
    transactions: &TransactionStore,
    snapshots: &SnapshotStore,
    transaction_id: &super::transaction::TransactionId,
    workspace_root: &Path,
    expected_review_revision: Option<blake3::Hash>,
) -> Result<DecisionTransition<FileEditTransaction>> {
    let lock = transactions.acquire_decision_lock(transaction_id.as_str())?;
    let txn =
        match transactions.claim_apply_locked(transaction_id, expected_review_revision, &lock)? {
            DecisionTransition::Transitioned(txn) => txn,
            DecisionTransition::AlreadyResolved(txn) => {
                return Ok(DecisionTransition::AlreadyResolved(txn));
            },
        };

    let snapshot_id = match apply_claimed_transaction(&txn, snapshots, workspace_root) {
        Ok(snapshot_id) => snapshot_id,
        Err(apply_err) => {
            transactions
                .reset_pending_locked(txn, &lock)
                .context("apply failed and transaction claim could not be released")?;
            return Err(apply_err);
        },
    };

    match transactions.finish_apply_locked(transaction_id, Some(snapshot_id.to_string()), &lock) {
        Ok(applied) => Ok(DecisionTransition::Transitioned(applied)),
        Err(mark_err) => {
            let rollback = snapshots.restore(&snapshot_id);
            let reset = transactions.reset_pending_locked(txn, &lock);
            match (rollback, reset) {
                (Ok(()), Ok(())) => Err(mark_err.context(
                    "apply: status commit failed; rolled back while decision lock was held",
                )),
                (rollback, reset) => Err(mark_err.context(format!(
                    "apply: status commit failed; rollback={rollback:?}; claim_reset={reset:?}"
                ))),
            }
        },
    }
}

fn apply_claimed_transaction(
    txn: &FileEditTransaction,
    snapshots: &SnapshotStore,
    workspace_root: &Path,
) -> Result<SnapshotId> {
    let transaction_id = &txn.id;

    // Canonicalize the workspace root once so the per-path symlink
    // containment check can compare prefixes cheaply. If the root
    // itself doesn't exist or canonicalizes outside, that's a
    // structural setup error — surface it loudly instead of papering
    // over.
    let workspace_root_canonical = std::fs::canonicalize(workspace_root)
        .with_context(|| format!("canonicalize workspace_root {}", workspace_root.display()))?;

    // Resolve every edit's path against workspace_root with full
    // traversal validation:
    //   1. static `..` / absolute / prefix rejection
    //   2. runtime symlink-escape guard (canonicalize + prefix check)
    //   3. Move-self-loop check (from == to → would delete the file)
    // The tool layer should also validate (1), but defense in depth.
    let mut resolved_paths: Vec<PathBuf> = Vec::new();
    for edit in &txn.edits {
        // Move-self-loop check: if from == to, the apply walk would
        // write to `to` then delete `from` (same path), losing the
        // file entirely. Tool layer should reject this as malformed
        // input; we catch it here so it can't slip through.
        if let FileEdit::Move { from, to, .. } = edit {
            if from == to {
                return Err(anyhow!(
                    "apply: Move has from == to ({}) — would delete the file; \
                     re-stage as Modify if you only want to change content",
                    from.display()
                ));
            }
        }

        for rel in edit.affected_paths() {
            validate_relative_path(rel)
                .with_context(|| format!("apply: invalid edit path {}", rel.display()))?;
            let absolute = workspace_root.join(rel);
            // Runtime symlink-containment check. `target_must_exist`
            // distinguishes paths the apply will read (Modify source,
            // Delete target, Move source) from paths it will create
            // (Create target, Move target). For the read case, the
            // file should already exist; for the create case, the
            // leaf doesn't exist yet but the parent must canonicalize
            // inside the workspace.
            let target_must_exist = match edit {
                FileEdit::Create { path, .. } => rel != path.as_path(),
                FileEdit::Modify { .. } | FileEdit::Delete { .. } => true,
                FileEdit::Move { to, .. } => rel != to.as_path(),
            };
            assert_resolved_inside_workspace(
                &absolute,
                &workspace_root_canonical,
                target_must_exist,
            )
            .with_context(|| format!("apply: symlink escape check for {}", rel.display()))?;
            resolved_paths.push(absolute);
        }
    }

    // 1. Capture snapshot BEFORE any write. If this fails the
    //    transaction stays Pending and no file is touched.
    let snapshot_id = snapshots.capture(transaction_id.as_str(), &resolved_paths)?;

    // 2. Write every edit. On any failure attempt revert from the
    //    snapshot we just took so the workspace returns to its
    //    pre-apply state. Re-surface the original write error.
    let mut error: Option<anyhow::Error> = None;
    for edit in &txn.edits {
        if let Err(write_err) = apply_one_edit(edit, workspace_root) {
            error = Some(write_err);
            break;
        }
    }

    if let Some(write_err) = error {
        // Attempt rollback. If rollback itself fails, log the
        // rollback failure but propagate the original write error
        // as the primary cause.
        match snapshots.restore(&snapshot_id) {
            Ok(()) => return Err(write_err.context("apply failed; rolled back to snapshot")),
            Err(rollback_err) => {
                return Err(write_err.context(format!(
                    "apply failed AND rollback also failed (snapshot {}): {rollback_err:#}",
                    snapshot_id
                )));
            },
        }
    }

    Ok(snapshot_id)
}

/// Revert a previously-applied transaction. Loads the snapshot from
/// the transaction's `snapshot_id` field and restores every file.
/// Transitions the transaction status to `Reverted` on success.
pub fn revert_transaction(
    transactions: &TransactionStore,
    snapshots: &SnapshotStore,
    transaction_id: &super::transaction::TransactionId,
) -> Result<()> {
    let txn = transactions.load(transaction_id)?;
    let snap_id = txn
        .snapshot_id
        .as_deref()
        .ok_or_else(|| anyhow!("revert: transaction {} has no snapshot_id", transaction_id))?;
    // The snapshot_id is read from the persisted transaction JSON,
    // which could have been tampered with by anything that can write
    // under `<scope>/transactions/`. Validate the charset before
    // using it as a filesystem path — `SnapshotStore::load` does
    // `format!("{}.json", id)`, so a malicious `../../...`
    // snapshot_id would dereference outside the scope's
    // `snapshots/` directory.
    let snap_id = SnapshotId::parse(snap_id).with_context(|| {
        format!(
            "revert: transaction {} has malformed snapshot_id",
            transaction_id
        )
    })?;
    snapshots.restore(&snap_id)?;
    transactions.mark_reverted(transaction_id)?;
    Ok(())
}

/// Reject paths that would escape the workspace root.
///
/// Static rules (no filesystem touch):
///   - reject absolute paths (already caught by upstream tool, but
///     defense in depth)
///   - reject any `..` (`ParentDir`) component — `workspace_root.join("../etc/passwd")`
///     resolves to `<workspace_root>/../etc/passwd` which the kernel
///     normalizes to `/etc/passwd`, escaping the scope sandbox
///   - reject `RootDir` / `Prefix` components (Windows drive letters,
///     UNC paths) for the same reason
///   - empty paths reject as well (callers should send actual filenames)
///
/// `CurDir` (`.`) is allowed because it's a harmless no-op component
/// that `join` collapses. `Normal` components (regular filename
/// segments) are the expected case.
///
/// **Note:** static validation alone cannot catch symlinks pointing
/// outside the workspace — that requires a runtime canonicalization
/// check, performed separately by
/// [`assert_resolved_inside_workspace`] at apply time.
fn validate_relative_path(path: &Path) -> Result<()> {
    use std::path::Component;
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
                    "path contains `..` (parent-dir) component; would escape workspace: {}",
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

/// Runtime symlink-escape guard. Resolves the absolute path
/// (following any symlinks in its parent components) and asserts the
/// real location lives under `workspace_root_canonical`. Without
/// this, a symlink at any level inside the workspace
/// (e.g. `<workspace>/escape → /etc`) lets an otherwise
/// well-formed relative path like `escape/passwd` write to `/etc/passwd`.
///
/// `target_must_exist` is true for paths the operation will READ
/// (Modify / Delete / Move source); false for paths the operation
/// will CREATE (Create destination / Move destination), where the
/// final path component is expected to NOT exist yet. In the create
/// case we canonicalize the parent directory and re-attach the
/// filename, guarding against symlinked parents while accommodating
/// the not-yet-existing leaf.
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
            // Parent doesn't exist yet either — canonicalize the
            // nearest existing ancestor, then validate that. This
            // walks UP until we find an existing dir; if any
            // existing ancestor escapes, we catch it.
            let mut cursor = parent.to_path_buf();
            loop {
                if cursor.exists() {
                    break std::fs::canonicalize(&cursor)
                        .with_context(|| format!("canonicalize ancestor {}", cursor.display()))?;
                }
                let Some(next) = cursor.parent().map(|p| p.to_path_buf()) else {
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

fn apply_one_edit(edit: &FileEdit, workspace_root: &Path) -> Result<()> {
    match edit {
        FileEdit::Create { path, content, .. } => {
            let abs = workspace_root.join(path);
            // Exists-check: a Create on a path that already exists is
            // an error because (a) the tool's pre-stage view of the
            // file is empty (Create's diff is against `/dev/null`), so
            // overwriting silently would clobber unexpected content,
            // and (b) the snapshot recorded the path as missing — a
            // post-apply revert would `delete` the file, losing the
            // pre-existing content entirely. Operator must re-stage
            // as Modify (with the current content as the baseline) or
            // Delete-then-Create to take ownership of the path.
            if abs.exists() {
                return Err(anyhow!(
                    "create: {} already exists; re-stage as Modify or Delete-then-Create to take ownership of the path",
                    abs.display()
                ));
            }
            if let Some(parent) = abs.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("create parent dir {}", parent.display()))?;
                }
            }
            std::fs::write(&abs, content)
                .with_context(|| format!("create write {}", abs.display()))?;
            Ok(())
        },
        FileEdit::Modify {
            path,
            new_content,
            old_content_hash,
            ..
        } => {
            let abs = workspace_root.join(path);
            // Hash check: if the on-disk content has changed since
            // stage, refuse the apply. Operator must re-stage from
            // current content. Bounded read so a multi-GB file
            // errors via stat before we OOM.
            let current = read_text_bounded(&abs)
                .with_context(|| format!("modify hash-check read {}", abs.display()))?;
            let current_hash = blake3_hex(&current);
            if &current_hash != old_content_hash {
                return Err(anyhow!(
                    "modify: on-disk content for {} changed since stage \
                     (expected hash {}, got {}); re-stage from current content",
                    abs.display(),
                    old_content_hash,
                    current_hash
                ));
            }
            std::fs::write(&abs, new_content)
                .with_context(|| format!("modify write {}", abs.display()))?;
            Ok(())
        },
        FileEdit::Delete {
            path,
            old_content_hash,
            ..
        } => {
            let abs = workspace_root.join(path);
            let current = read_text_bounded(&abs)
                .with_context(|| format!("delete hash-check read {}", abs.display()))?;
            let current_hash = blake3_hex(&current);
            if &current_hash != old_content_hash {
                return Err(anyhow!(
                    "delete: on-disk content for {} changed since stage; \
                     re-stage if you still want to delete",
                    abs.display()
                ));
            }
            std::fs::remove_file(&abs).with_context(|| format!("delete {}", abs.display()))?;
            Ok(())
        },
        FileEdit::Move {
            from,
            to,
            new_content,
            old_content_hash,
            ..
        } => {
            let abs_from = workspace_root.join(from);
            let abs_to = workspace_root.join(to);
            let current = read_text_bounded(&abs_from)
                .with_context(|| format!("move hash-check read {}", abs_from.display()))?;
            let current_hash = blake3_hex(&current);
            if &current_hash != old_content_hash {
                return Err(anyhow!(
                    "move: on-disk content for {} changed since stage; re-stage",
                    abs_from.display()
                ));
            }
            if let Some(parent) = abs_to.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("create parent dir {}", parent.display()))?;
                }
            }
            std::fs::write(&abs_to, new_content)
                .with_context(|| format!("move write {}", abs_to.display()))?;
            std::fs::remove_file(&abs_from)
                .with_context(|| format!("move remove src {}", abs_from.display()))?;
            Ok(())
        },
    }
}

fn blake3_hex(content: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(content.as_bytes());
    hasher.finalize().to_hex().to_string()
}

/// Test-time helper for callers that want to introspect a snapshot
/// without loading the file (e.g. for assertions on what was captured).
pub fn snapshot_paths(snap: &Snapshot) -> Vec<&Path> {
    snap.files.iter().map(|f| f.path.as_path()).collect()
}

/// Compatibility re-export — `apply_transaction` and `revert_transaction`
/// are the public entry points, but downstream callers may want a
/// shorter convenience name.
pub use apply_transaction as apply;
pub use revert_transaction as revert;
// Silence the unused-import lint on `FileEditTransaction` in case
// future maintenance refactors away the only doc-comment that
// references it.
#[allow(dead_code)]
fn _retain_transaction_ref(_: &FileEditTransaction) {}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::execution::file_edit::transaction::{
        ProposedEdit, TransactionScope, TransactionStore,
    };

    fn temp_scope() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("file-edit-snap-test-{}", Uuid::new_v4()));
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
    fn capture_then_restore_round_trips_content() {
        let root = temp_scope();
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let file_path = workspace.join("foo.txt");
        std::fs::write(&file_path, "original").unwrap();

        let snapshots = SnapshotStore::new(&root);
        let snap_id = snapshots.capture("txn-1", &[file_path.clone()]).unwrap();

        // Mutate the file.
        std::fs::write(&file_path, "modified").unwrap();
        assert_eq!(std::fs::read_to_string(&file_path).unwrap(), "modified");

        // Restore.
        snapshots.restore(&snap_id).unwrap();
        assert_eq!(std::fs::read_to_string(&file_path).unwrap(), "original");
    }

    #[test]
    fn capture_missing_file_records_none() {
        let root = temp_scope();
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let missing = workspace.join("does-not-exist.txt");

        let snapshots = SnapshotStore::new(&root);
        let snap_id = snapshots.capture("txn-1", &[missing.clone()]).unwrap();

        let loaded = snapshots.load(&snap_id).unwrap();
        assert_eq!(loaded.files.len(), 1);
        assert!(loaded.files[0].content.is_none());

        // Create the file, then restore — should delete it.
        std::fs::write(&missing, "interim").unwrap();
        snapshots.restore(&snap_id).unwrap();
        assert!(
            !missing.exists(),
            "missing file should have been deleted on restore"
        );
    }

    /// The snapshot record is the only copy of the pre-apply content, so it
    /// must land in one step and leave nothing beside itself that a pruner or
    /// a directory walk would mistake for a snapshot.
    #[test]
    fn capture_publishes_without_leaving_a_staging_sibling() {
        let root = temp_scope();
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let file_path = workspace.join("foo.txt");
        std::fs::write(&file_path, "original").unwrap();

        let snapshots = SnapshotStore::new(&root);
        let first = snapshots.capture("txn-1", &[file_path.clone()]).unwrap();
        let second = snapshots.capture("txn-2", &[file_path.clone()]).unwrap();

        // Both records parse back through the store's own reader.
        assert_eq!(snapshots.load(&first).unwrap().files.len(), 1);
        assert_eq!(snapshots.load(&second).unwrap().files.len(), 1);

        let staging_left = std::fs::read_dir(root.join("snapshots"))
            .expect("snapshots listing")
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"));
        assert!(
            !staging_left,
            "snapshot capture must leave no staging sibling in snapshots/"
        );
    }

    #[test]
    fn apply_transaction_writes_then_marks_applied() {
        let root = temp_scope();
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let txns = TransactionStore::new(&root);
        let snaps = SnapshotStore::new(&root);

        let txn = txns
            .stage(
                scope(),
                "create foo",
                vec![ProposedEdit::Create {
                    path: PathBuf::from("foo.txt"),
                    content: "hello\n".into(),
                }],
                &mut |_| unreachable!("Create doesn't read existing"),
            )
            .unwrap();

        apply_transaction(&txns, &snaps, &txn.id, &workspace).unwrap();

        let written = workspace.join("foo.txt");
        assert_eq!(std::fs::read_to_string(&written).unwrap(), "hello\n");

        let loaded = txns.load(&txn.id).unwrap();
        assert!(matches!(
            loaded.status,
            crate::magician_v2::execution::file_edit::transaction::FileEditStatus::Applied
        ));
        assert!(loaded.snapshot_id.is_some());
    }

    #[test]
    fn concurrent_apply_and_reject_have_one_terminal_winner() {
        let root = temp_scope();
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let txns = TransactionStore::new(&root);
        let txn = txns
            .stage(
                scope(),
                "race",
                vec![ProposedEdit::Create {
                    path: PathBuf::from("race.txt"),
                    content: "winner".into(),
                }],
                &mut |_| unreachable!(),
            )
            .unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));

        let apply_root = root.clone();
        let apply_workspace = workspace.clone();
        let apply_id = txn.id.clone();
        let apply_barrier = barrier.clone();
        let apply = std::thread::spawn(move || {
            let txns = TransactionStore::new(&apply_root);
            let snaps = SnapshotStore::new(&apply_root);
            apply_barrier.wait();
            apply_transaction_decision(&txns, &snaps, &apply_id, &apply_workspace).unwrap()
        });

        let reject_root = root.clone();
        let reject_id = txn.id.clone();
        let reject = std::thread::spawn(move || {
            let txns = TransactionStore::new(&reject_root);
            barrier.wait();
            txns.reject(&reject_id).unwrap()
        });

        let apply_outcome = apply.join().unwrap();
        let reject_outcome = reject.join().unwrap();
        assert_ne!(
            apply_outcome.transitioned(),
            reject_outcome.transitioned(),
            "exactly one decision must transition Pending"
        );
        let final_txn = txns.load(&txn.id).unwrap();
        match final_txn.status {
            super::super::transaction::FileEditStatus::Applied => {
                assert_eq!(
                    std::fs::read_to_string(workspace.join("race.txt")).unwrap(),
                    "winner"
                );
            },
            super::super::transaction::FileEditStatus::Rejected => {
                assert!(!workspace.join("race.txt").exists());
            },
            status => panic!("unexpected final status: {status:?}"),
        }
    }

    #[test]
    fn concurrent_apply_loser_never_rolls_back_winner() {
        let root = temp_scope();
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let txns = TransactionStore::new(&root);
        let txn = txns
            .stage(
                scope(),
                "double apply",
                vec![ProposedEdit::Create {
                    path: PathBuf::from("committed.txt"),
                    content: "committed".into(),
                }],
                &mut |_| unreachable!(),
            )
            .unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let root = root.clone();
            let workspace = workspace.clone();
            let id = txn.id.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                let txns = TransactionStore::new(&root);
                let snaps = SnapshotStore::new(&root);
                barrier.wait();
                apply_transaction_decision(&txns, &snaps, &id, &workspace).unwrap()
            }));
        }
        let outcomes: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| outcome.transitioned())
                .count(),
            1
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("committed.txt")).unwrap(),
            "committed"
        );
        assert!(matches!(
            txns.load(&txn.id).unwrap().status,
            super::super::transaction::FileEditStatus::Applied
        ));
    }

    #[test]
    fn verified_apply_rechecks_revision_under_record_lock() {
        let root = temp_scope();
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let txns = TransactionStore::new(&root);
        let snaps = SnapshotStore::new(&root);
        let txn = txns
            .stage(
                scope(),
                "reviewed",
                vec![ProposedEdit::Create {
                    path: PathBuf::from("reviewed.txt"),
                    content: "reviewed".into(),
                }],
                &mut |_| unreachable!(),
            )
            .unwrap();
        let reviewed_revision = txn.content_hash();

        let mut forged = txns.load(&txn.id).unwrap();
        if let FileEdit::Create { content, .. } = &mut forged.edits[0] {
            *content = "forged after verify".into();
        }
        txns.persist(&forged).unwrap();

        let err = apply_transaction_verified_decision(
            &txns,
            &snaps,
            &txn.id,
            &workspace,
            reviewed_revision,
        )
        .unwrap_err();
        assert!(err
            .downcast_ref::<super::super::transaction::ReviewRevisionConflict>()
            .is_some());
        assert!(!workspace.join("reviewed.txt").exists());
        let loaded = txns.load(&txn.id).unwrap();
        assert!(!loaded.decision_claimed());
        assert!(matches!(
            loaded.status,
            super::super::transaction::FileEditStatus::Pending
        ));
    }

    #[test]
    fn revert_transaction_restores_pre_apply_state() {
        let root = temp_scope();
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let target = workspace.join("foo.txt");
        std::fs::write(&target, "original").unwrap();

        let txns = TransactionStore::new(&root);
        let snaps = SnapshotStore::new(&root);

        let mut existing = std::collections::HashMap::new();
        existing.insert(target.clone(), "original".to_string());

        let txn = txns
            .stage(
                scope(),
                "rewrite foo",
                vec![ProposedEdit::Modify {
                    path: PathBuf::from("foo.txt"),
                    new_content: "modified".into(),
                }],
                &mut |p| {
                    let abs = if p.is_absolute() {
                        p.to_path_buf()
                    } else {
                        workspace.join(p)
                    };
                    existing
                        .get(&abs)
                        .cloned()
                        .ok_or_else(|| anyhow!("not staged: {}", abs.display()))
                },
            )
            .unwrap();

        apply_transaction(&txns, &snaps, &txn.id, &workspace).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "modified");

        revert_transaction(&txns, &snaps, &txn.id).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "original");

        let loaded = txns.load(&txn.id).unwrap();
        assert!(matches!(
            loaded.status,
            crate::magician_v2::execution::file_edit::transaction::FileEditStatus::Reverted
        ));
    }

    #[test]
    fn modify_rejects_when_content_changed_between_stage_and_apply() {
        let root = temp_scope();
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let target = workspace.join("foo.txt");
        std::fs::write(&target, "v1").unwrap();

        let txns = TransactionStore::new(&root);
        let snaps = SnapshotStore::new(&root);

        let mut existing = std::collections::HashMap::new();
        existing.insert(target.clone(), "v1".to_string());

        let txn = txns
            .stage(
                scope(),
                "bump",
                vec![ProposedEdit::Modify {
                    path: PathBuf::from("foo.txt"),
                    new_content: "v2".into(),
                }],
                &mut |p| {
                    let abs = if p.is_absolute() {
                        p.to_path_buf()
                    } else {
                        workspace.join(p)
                    };
                    existing
                        .get(&abs)
                        .cloned()
                        .ok_or_else(|| anyhow!("not staged: {}", abs.display()))
                },
            )
            .unwrap();

        // External edit between stage and apply.
        std::fs::write(&target, "external edit").unwrap();

        let err = apply_transaction(&txns, &snaps, &txn.id, &workspace).unwrap_err();
        assert!(
            err.to_string().contains("changed since stage")
                || format!("{err:#}").contains("changed since stage"),
            "got: {err:#}"
        );

        // Workspace should be unchanged (apply failed, rollback ran).
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "external edit");

        // Transaction stays Pending.
        let loaded = txns.load(&txn.id).unwrap();
        assert!(matches!(
            loaded.status,
            crate::magician_v2::execution::file_edit::transaction::FileEditStatus::Pending
        ));
    }

    #[test]
    fn apply_rejects_absolute_paths() {
        let root = temp_scope();
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let txns = TransactionStore::new(&root);
        let snaps = SnapshotStore::new(&root);

        let txn = txns
            .stage(
                scope(),
                "evil",
                vec![ProposedEdit::Create {
                    path: PathBuf::from("/etc/evil.conf"),
                    content: "bad".into(),
                }],
                &mut |_| unreachable!(),
            )
            .unwrap();

        let err = apply_transaction(&txns, &snaps, &txn.id, &workspace).unwrap_err();
        // Inspect the full anyhow error chain — the inner cause
        // carries the precise "is absolute" message; the outer
        // context wraps it with "apply: invalid edit path …".
        let chained = format!("{err:#}");
        assert!(chained.contains("absolute"), "got: {chained}");
    }

    #[test]
    fn snapshot_size_cap_prevents_runaway() {
        let root = temp_scope();
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let first = workspace.join("first.txt");
        let second = workspace.join("second.txt");
        // Two individually valid 6MB files exceed the aggregate
        // MAX_SNAPSHOT_BYTES cap without tripping the per-file read cap.
        let content = "x".repeat(6 * 1024 * 1024);
        std::fs::write(&first, &content).unwrap();
        std::fs::write(&second, &content).unwrap();

        let snaps = SnapshotStore::new(&root);
        let err = snaps.capture("txn-cap", &[first, second]).unwrap_err();
        let chained = format!("{err:#}");
        assert!(chained.contains("exceed"), "got: {chained}");
    }

    #[test]
    fn apply_rejects_parent_dir_traversal() {
        // Critical security test — `../etc/passwd` is not absolute,
        // so the pre-fix `is_absolute()` guard accepted it. The
        // current `validate_relative_path` walker catches the `..`
        // component and refuses to apply.
        let root = temp_scope();
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let txns = TransactionStore::new(&root);
        let snaps = SnapshotStore::new(&root);

        let txn = txns
            .stage(
                scope(),
                "exfiltrate",
                vec![ProposedEdit::Create {
                    path: PathBuf::from("../../etc/evil.conf"),
                    content: "pwned".into(),
                }],
                &mut |_| unreachable!(),
            )
            .unwrap();

        let err = apply_transaction(&txns, &snaps, &txn.id, &workspace).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("..") || msg.contains("parent-dir") || msg.contains("escape"),
            "expected traversal rejection, got: {msg}"
        );

        // Verify no file was written outside the workspace.
        let escaped = root.parent().unwrap().join("etc").join("evil.conf");
        assert!(
            !escaped.exists(),
            "exploit file was created: {}",
            escaped.display()
        );
    }

    #[test]
    fn apply_rejects_deeper_parent_dir_traversal() {
        // `subdir/../../escape.conf` is a more sneaky version — first
        // component is normal, then `..` walks out, then another `..`
        // walks above the workspace root. Must also be rejected.
        let root = temp_scope();
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let txns = TransactionStore::new(&root);
        let snaps = SnapshotStore::new(&root);

        let txn = txns
            .stage(
                scope(),
                "sneaky",
                vec![ProposedEdit::Create {
                    path: PathBuf::from("subdir/../../escape.conf"),
                    content: "pwned".into(),
                }],
                &mut |_| unreachable!(),
            )
            .unwrap();

        let err = apply_transaction(&txns, &snaps, &txn.id, &workspace).unwrap_err();
        assert!(format!("{err:#}").contains(".."), "got: {err:#}");
    }

    #[test]
    fn create_refuses_to_overwrite_existing_file() {
        // Pre-fix, Create blasted whatever existed at the target
        // path. Then revert deleted it (snapshot recorded the file as
        // missing because it was meant to be created), losing the
        // pre-existing content entirely. Current behavior: Create
        // errors out and rollback restores the (still untouched)
        // workspace.
        let root = temp_scope();
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let target = workspace.join("preexisting.txt");
        std::fs::write(&target, "I was here first").unwrap();

        let txns = TransactionStore::new(&root);
        let snaps = SnapshotStore::new(&root);

        let txn = txns
            .stage(
                scope(),
                "create over existing",
                vec![ProposedEdit::Create {
                    path: PathBuf::from("preexisting.txt"),
                    content: "blasted".into(),
                }],
                &mut |_| unreachable!(),
            )
            .unwrap();

        let err = apply_transaction(&txns, &snaps, &txn.id, &workspace).unwrap_err();
        assert!(
            format!("{err:#}").contains("already exists"),
            "got: {err:#}"
        );

        // The pre-existing content survives untouched.
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "I was here first",
            "Create silently overwrote pre-existing content"
        );

        // Transaction stays Pending (operator can re-stage as Modify
        // or Delete-then-Create with the right baseline).
        let loaded = txns.load(&txn.id).unwrap();
        assert!(matches!(
            loaded.status,
            crate::magician_v2::execution::file_edit::transaction::FileEditStatus::Pending
        ));
    }

    #[test]
    #[cfg(unix)]
    fn apply_rejects_symlink_escape() {
        // Critical security test — even with `validate_relative_path`
        // catching `..`, a symlink inside the workspace can still
        // forward writes outside. This test puts an escape symlink
        // in place and verifies `apply_transaction` refuses to
        // follow it.
        let root = temp_scope();
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        // Create a target outside the workspace that we should NEVER
        // be able to write to.
        let outside_root = root.join("outside");
        std::fs::create_dir_all(&outside_root).unwrap();

        // Plant the escape symlink at <workspace>/escape → outside_root.
        let escape_link = workspace.join("escape");
        std::os::unix::fs::symlink(&outside_root, &escape_link).unwrap();

        let txns = TransactionStore::new(&root);
        let snaps = SnapshotStore::new(&root);

        let txn = txns
            .stage(
                scope(),
                "symlink escape",
                vec![ProposedEdit::Create {
                    path: PathBuf::from("escape/evil.conf"),
                    content: "pwned".into(),
                }],
                &mut |_| unreachable!(),
            )
            .unwrap();

        let err = apply_transaction(&txns, &snaps, &txn.id, &workspace).unwrap_err();
        assert!(
            format!("{err:#}").contains("outside") || format!("{err:#}").contains("symlink"),
            "expected symlink-escape rejection, got: {err:#}"
        );

        // Verify the escape target was NOT written.
        let evil = outside_root.join("evil.conf");
        assert!(
            !evil.exists(),
            "symlink escape wrote outside workspace: {}",
            evil.display()
        );
    }

    #[test]
    fn apply_rejects_move_self_loop() {
        // from == to would write content to the path then delete the
        // same path → file vanishes. Caught at apply time so the
        // operator gets a clear error instead of mysterious data loss.
        let root = temp_scope();
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let target = workspace.join("foo.txt");
        std::fs::write(&target, "preserve me").unwrap();

        let txns = TransactionStore::new(&root);
        let snaps = SnapshotStore::new(&root);

        let mut existing = std::collections::HashMap::new();
        existing.insert(target.clone(), "preserve me".to_string());

        let txn = txns
            .stage(
                scope(),
                "self-loop move",
                vec![ProposedEdit::Move {
                    from: PathBuf::from("foo.txt"),
                    to: PathBuf::from("foo.txt"),
                    new_content: "evil overwrite".into(),
                }],
                &mut |p| {
                    let abs = if p.is_absolute() {
                        p.to_path_buf()
                    } else {
                        workspace.join(p)
                    };
                    existing
                        .get(&abs)
                        .cloned()
                        .ok_or_else(|| anyhow!("not staged: {}", abs.display()))
                },
            )
            .unwrap();

        let err = apply_transaction(&txns, &snaps, &txn.id, &workspace).unwrap_err();
        assert!(
            format!("{err:#}").contains("from == to") || format!("{err:#}").contains("delete"),
            "got: {err:#}"
        );

        // File survived.
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "preserve me");
    }

    #[test]
    fn snapshot_paths_helper_returns_all() {
        let root = temp_scope();
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let a = workspace.join("a.txt");
        let b = workspace.join("b.txt");
        std::fs::write(&a, "A").unwrap();
        std::fs::write(&b, "B").unwrap();

        let snaps = SnapshotStore::new(&root);
        let snap_id = snaps.capture("t", &[a.clone(), b.clone()]).unwrap();
        let loaded = snaps.load(&snap_id).unwrap();
        let paths = snapshot_paths(&loaded);
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&a.as_path()));
        assert!(paths.contains(&b.as_path()));
    }
}
