//! The loop state store on a filesystem.
//!
//! # What it reuses from `FullPauseStore`, and what it deliberately changes
//!
//! `FullPauseStore` is the durable, crash-recovering store this codebase already
//! has, and the design says to reuse its publish machinery rather than invent a
//! second one. What is reused:
//!
//! - **Bounded reads.** Every *JSON record* — state, lease, key, effect row —
//!   goes through [`read_bounded_json`], which checks the file's size *before*
//!   opening it and then reads through `JsonDepthGuardReader`, the same scanner
//!   pause records use, made `pub(super)` for this rather than copied. Byte
//!   limit, node limit and nesting limit all apply. The journal is the exception
//!   and is bounded differently, because it is a log rather than a document:
//!   [`MAX_JOURNAL_BYTES`] on the file and `MAX_JOURNAL_RECORD_BYTES` on each
//!   line, both enforced by `Journal::parse`.
//! - **The publish sequence.** Write to a uniquely-named temporary, flush,
//!   `sync_all` the file, publish, then **fsync the parent directory**. A rename
//!   or a link that has not been directory-synced is not durable, and skipping
//!   that step is the difference between a store that survives a power cut and
//!   one that appears to. When publication also creates directories, every new
//!   directory name is fsynced into its own parent before the record can report
//!   success; syncing only the leaf would leave a newly-created execution tree
//!   vulnerable to disappearing as a whole. A parent sync that *fails* is
//!   returned to the caller;
//!   the published record is left in place so a retry can reload and resolve the
//!   ambiguously completed write, but the store never reports durable success
//!   for a directory entry it could not sync.
//! - **Bounds on both sides.** A record too large to write is refused at write;
//!   a record too large to read is refused at read. Neither bound is decorative.
//!
//! What changes, and why:
//!
//! - **Publish is `link`, not `rename`.** A pause record is addressed by key and
//!   a new revision replaces the old one, so `rename` is right there. A loop
//!   state is addressed **by revision**, and `commit` is a compare-and-swap:
//!   `rename` would overwrite whatever a racing worker published, which is
//!   exactly the failure CAS exists to prevent. `link(2)` fails with
//!   `AlreadyExists` if the target is taken, and it does so atomically, so the
//!   revision file itself is the compare-and-swap — across processes, not only
//!   within one. The linked name never appears before its content is fully
//!   written and fsynced, so a torn published file is not reachable.
//! - **There is no process-local index.** `FullPauseStore` keeps a `DashMap` and a
//!   startup hydration path. A store whose whole point is that any process can
//!   advance any execution cannot answer from a cache another process cannot
//!   see. The bounded journal tail index is therefore persisted and validated
//!   against the exact on-disk journal generation on every use.
//!
//! # Known characteristics, stated rather than discovered later
//!
//! - **`list_runnable` is a scan**, which the design explicitly permits at
//!   current scale (*"a scan is fine at current scale and lets the queue arrive
//!   with the object-store impl"*). The wake index the design sketches at scope
//!   root is likewise a per-execution marker here, read during a scan that is
//!   already happening.
//! - **The scan is ORDERED, and that is a contract rather than an accident.**
//!   Each level of the walk is sorted, because [`LoopStateStore::scan_runnable`]
//!   resumes *after* a position and a position in `read_dir` order means nothing
//!   the next call can rely on — entries appear and vanish under it constantly.
//!   The sort is windowed ([`ordered_child_dirs`]), so it costs a bounded amount
//!   of memory per level rather than the level.
//! - **A run's ENDING is published, not derived.** A terminal lives in the
//!   journal, and a scan that read each run's journal would be `O(records)` per
//!   key on the poll path. So `commit` writes `ended.json` out of the parse it
//!   was already doing, and the scan reads one small file. It is the only value
//!   in this store that is a *derived* fact about the journal rather than the
//!   journal itself, and the reason it is allowed to be one is written on
//!   [`super::EndedRun`]. Every commit publishes an exact revision binding;
//!   non-terminal revisions publish a `None` tombstone rather than deleting the
//!   file. A derived value without that revision binding would go on answering
//!   for a prefix its journal has moved past and could withhold a live run.
//! - **The marker is a SECOND FILE, so it is revision-bound.** A commit writes a
//!   two-binding marker before publishing its snapshot: one binding preserves
//!   the currently-authoritative revision and one describes the proposed next
//!   revision. A crash or failed snapshot swap therefore leaves the old binding
//!   readable; a successful swap exposes the new one without a second write.
//!   This is deliberately a tiny two-slot transaction record, not a mutable
//!   scalar. Overwriting or retracting a scalar before the snapshot used to hide
//!   an already-committed terminal whenever the later snapshot publish failed.
//! - **The substrate is synchronous; the trait boundary is not.** Every public
//!   operation runs its complete filesystem transaction in a bounded
//!   `spawn_blocking` closure. Fenced operations keep lock, lease validation and
//!   mutation inside that one closure, so moving I/O off Tokio workers does not
//!   split the critical section.
//! - **Journal verification is indexed after one full read.** A bounded
//!   persistent
//!   tail index binds a hash chain, replay cursor, append-batch offsets and
//!   committed watermark to the journal's exact size and modification stamp.
//!   Normal append/commit/verify/outbox work is therefore proportional to the
//!   new boundary, not the lifetime log. Absence, stale metadata, a torn write,
//!   an orphan beyond the indexed tail, or any malformed derived record falls
//!   back to [`FsLoopStateStore::read_journal_file`]; the index never weakens the
//!   parser or becomes required state. The effect-ledger capacity check retains
//!   the older full-scan shape because it runs only while adding new effects.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

use async_trait::async_trait;
use chrono::Utc;
use fs2::FileExt;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::warn;
use uuid::Uuid;

use super::{
    non_resumable_terminal, ChainClosure, CommittedLoopState, EndedRun, ExecutionKey, LastLease,
    Lease, LoopStateStore, ParkedExecution, ParkedListing, Revision, RunnableScan, ScanCursor,
    StoreError, StoreResult, TerminalOutboxScan, MAX_DISCOVERY_VISITS_PER_PAGE,
    MAX_WAKE_RESOLUTIONS,
};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::execution::agentic::executor::JsonDepthGuardReader;
use crate::magician_v2::execution::agentic::run_loop::effects::{
    EffectError, EffectId, EffectLedger, EffectLedgerEntry, EffectOutcome, MAX_LEDGER_ENTRIES,
};
use crate::magician_v2::execution::agentic::run_loop::journal::{
    replay, replay_from, Journal, JournalAppend, JournalBody, JournalError, JournalRecord,
    ProjectorCursor, ReplayedCursor, TerminalKind, MAX_JOURNAL_BYTES, MAX_JOURNAL_RECORDS,
};
use crate::magician_v2::execution::agentic::run_loop::outcome::Phase as LoopPhase;
use crate::magician_v2::execution::agentic::run_loop::state::{LoopState, Placement, WorkerId};

/// The largest a committed state may be.
///
/// Generous next to what the cursor-and-scheduling half of `LoopState` needs
/// today, and deliberately so: the conversation and environment blocks join it by
/// reference rather than by value, so this should stay generous without ever
/// needing to be large.
const MAX_STATE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_STATE_JSON_NODES: usize = 200_000;

const MAX_EFFECT_BYTES: u64 = 256 * 1024;
const MAX_EFFECT_JSON_NODES: usize = 8_192;

const MAX_LEASE_BYTES: u64 = 16 * 1024;
const MAX_LEASE_JSON_NODES: usize = 256;

const MAX_KEY_BYTES: u64 = 16 * 1024;
const MAX_KEY_JSON_NODES: usize = 256;

/// The largest a projector mark may be.
///
/// Sized from what this store can actually write rather than from a round
/// number. An `execution_id` is capped at `MAX_EXECUTION_ID_BYTES` (128) by
/// `ExecutionKey`; content-addressed cursor keys add a 64-character digest and
/// the rest of an address is under a hundred bytes. At
/// `ProjectorCursor::DEFAULT_WINDOW` (1_024) the resulting mark is still well
/// below half this ceiling.
///
/// The number that decides this ceiling is `ProjectorCursor::MAX_WINDOW`, not
/// the default: a cursor restored from a file may carry up to the maximum, and
/// then grows to it. At 2_048 addresses the largest mark this store can produce
/// remains below this one-megabyte ceiling.
/// The margin is stated as a ratio rather than rounded to a comfortable-looking
/// number because a ceiling under the worst case makes a legitimately-written
/// mark unreadable, which is the failure below — and because the pairing is
/// load-bearing in the other direction too: `MAX_WINDOW` was lowered to 2× the
/// default precisely so no window this store accepts on read can grow into a
/// mark it refuses on write.
///
/// # What refusing this read costs, stated because it is not a tidy failure
///
/// A mark too large to read makes `load_projector_cursor` an error, and
/// `driver_worker::project_outbox` responds by **not projecting that run at
/// all** rather than by starting from zero. That is the fail-closed direction —
/// starting from zero re-emits the whole run's timeline — but it means one
/// unreadable file stops an execution's outbox until an operator moves it. The
/// records stay in the journal, so nothing is lost; nothing is delivered either.
const MAX_PROJECTOR_BYTES: u64 = 1024 * 1024;

/// The node ceiling for the same file.
///
/// `JsonDepthGuardReader` counts a node only where a **value** is expected, so
/// an object key costs nothing and one address costs five — the object itself
/// plus its four values. A full `DEFAULT_WINDOW` is therefore 5_120, plus four
/// for the enclosing object, the array and the two scalar fields. This is a
/// little over three times that.
///
/// # It must ADMIT a full `MAX_WINDOW`, and an earlier version deliberately did
/// not
///
/// The earlier note said this ceiling was set far **below** what
/// `ProjectorCursor::MAX_WINDOW` would encode to, on the grounds that a file
/// claiming sixty-five thousand addresses was not written by this store. That
/// reasoning had a hole: `save_projector_cursor` checks **bytes** and not nodes,
/// so once a restored cursor carried a window that large, this store would grow
/// it, write it (under the byte ceiling), and then refuse its own file on the
/// next read — stopping that run's outbox with nothing pointing back here.
///
/// `MAX_WINDOW` is 2× the default now, so a full one is 10_244 nodes and this
/// ceiling is above it with room to spare. Anything this store can write, it can
/// read back; anything past this ceiling is a file it did not write. Keep that
/// relation if either number moves: `5 × MAX_WINDOW + 4` must stay under this.
const MAX_PROJECTOR_JSON_NODES: usize = 16_384;

// The relation above, checked at compile time rather than asked for in prose.
//
// A comment saying "keep this under that" is the kind of protection that is not
// in force. The two constants live in different files and the failure it guards
// is silent: a mark written under the byte ceiling and refused under the node one
// stops a run's outbox at some later boundary with nothing naming the cause.
//
// Five nodes per address and four for the envelope are exact rather than an
// estimate. `JsonDepthGuardReader` counts a node where a value is expected, an
// `EventKey` is one object plus four values, and the file is one object plus two
// scalars plus the array.
const _: () = assert!(
    5 * ProjectorCursor::MAX_WINDOW + 4 <= MAX_PROJECTOR_JSON_NODES,
    "a full ProjectorCursor::MAX_WINDOW must stay readable by this store"
);

/// The byte ceiling on one chain-closure receipt.
///
/// A receipt is one segment id and one timestamp, and the segment id is already
/// bounded by `MAX_EXECUTION_ID_BYTES` — so four kilobytes is two orders of
/// magnitude of headroom rather than a tuned figure. It is here at all for the
/// reason every other bounded read in this file is: a file this store will
/// happily read without a ceiling is a file something else can make it read.
///
/// Refusing this read costs a refusal to prove, never a wrong proof: a receipt
/// that cannot be read is a receipt that does not exist, and the reconciler's
/// answer to an absent receipt is to leave the park alone.
const MAX_CHAIN_CLOSURE_BYTES: u64 = 4 * 1024;

/// The node ceiling for the same file. One object plus two values.
const MAX_CHAIN_CLOSURE_JSON_NODES: usize = 64;

/// The most directory entries one execution directory may hold before a read
/// refuses it.
///
/// Snapshots and leases are pruned, so a directory past this has stopped being
/// pruned — which is a problem to surface rather than to scan through.
const MAX_EXECUTION_DIR_ENTRIES: usize = 8_192;

/// The most **execution** directories one walk over this store will visit.
///
/// Execution directories only: the count is stepped at the leaf of the walk, so
/// the principal and workspace levels are bounded by [`MAX_SCAN_WINDOW`] and not
/// by this.
///
/// It bounds [`FsLoopStateStore::walk_execution_dirs`], so it is a **scheduling**
/// ceiling and a **reconciliation** ceiling at once — `scan_runnable` and
/// `list_parked` share the walk. Tuning it for one moves the other, and what a
/// skipped directory costs is not the same on the two paths: for the scheduler
/// it is work offered late, and for the reconciler it is a parked run never
/// examined at all. That asymmetry is why the walk reports having stopped short
/// rather than returning quietly.
///
/// The two paths now answer that report differently, and the difference is the
/// point. `scan_runnable` turns it into a [`ScanCursor`], so the tail beyond the
/// ceiling is reachable by paging — the ceiling costs a pass, not a population.
/// `list_parked` turns it into `ParkedListing::incomplete`, because a reconciler
/// that stopped short has a coverage hole whether or not somebody pages later.
///
/// `list_runnable` still discards it, which is now the honest description of the
/// **uncursored** call rather than of the walk: it has nowhere to put the fact.
const MAX_EXECUTIONS_SCANNED: usize = 20_000;

const MAX_WAKE_BYTES: u64 = 16 * 1024;
const MAX_WAKE_JSON_NODES: usize = 256;

/// The ending marker is at most two tiny revision bindings. Sized like the lease
/// record rather than snugly, because a file refused for being one byte over its
/// ceiling would withhold a run from every scan for a reason nothing else names.
const MAX_ENDED_BYTES: u64 = 16 * 1024;

/// Typed restart authority for one runtime execution.
///
/// `RecoverableExact` is the only variant that authorizes cold work. Callers
/// must pass its exact segment back through `stateless_resume_source_segment`;
/// `Absent` is only evidence for the narrow pre-seed crash repair, never
/// permission to invent a replacement for an already-seeded run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseExecutionRecoveryAuthority {
    Live {
        exact_segments: Vec<String>,
        until_ms: i64,
    },
    RecoverableExact {
        exact_segment: String,
        revision: Revision,
    },
    SettlementPending {
        exact_segments: Vec<String>,
    },
    Absent,
    Uncertain {
        exact_segments: Vec<String>,
        reason: String,
    },
}

/// One scope-wide recovery classification. Startup obtains this in one bounded
/// compatibility pass instead of rescanning every execution segment for every
/// runtime row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeBaseExecutionRecoveryAuthorities {
    pub legacy_writers_retired: bool,
    pub authorities: BTreeMap<String, BaseExecutionRecoveryAuthority>,
}
const MAX_ENDED_JSON_NODES: usize = 64;
const ENDING_MARKER_SCHEMA_VERSION: u8 = 1;

// `scan_terminal_outbox_debt` is a production cadence, not a maintenance
// command. Walking every execution directory on every tick made the cadence
// proportional to lifetime history even when only a handful of terminal rows
// were pending. This catalog is a bounded, reconstructible priority list. It is
// deliberately never authority: every row is exact-validated against the
// current snapshot/ending/cursor before it is returned, and a periodic bounded
// authoritative walk catches a missing best-effort publication.
const TERMINAL_DEBT_CATALOG_SCHEMA_VERSION: u8 = 1;
const MAX_TERMINAL_DEBT_CATALOG_ENTRIES: usize = 4_096;
const MAX_TERMINAL_DEBT_CATALOG_BYTES: u64 = 4 * 1024 * 1024;
const MAX_TERMINAL_DEBT_CATALOG_JSON_NODES: usize = 50_000;
const MAX_TERMINAL_DEBT_HINT_BYTES: u64 = 4 * 1024;
const MAX_TERMINAL_DEBT_HINT_JSON_NODES: usize = 32;
const TERMINAL_DEBT_AUTHORITATIVE_AUDIT_MS: i64 = 5 * 60 * 1_000;
const TERMINAL_DEBT_FUTURE_MARKER_GRACE_MS: i64 = 5 * 60 * 1_000;
// NUL cannot occur in a filesystem directory name. It therefore distinguishes
// an opaque catalog page cursor from every cursor emitted by the directory
// walker without reserving a user-visible principal/workspace name.
const TERMINAL_DEBT_PRIORITY_CURSOR_SENTINEL: &str = "\0terminal-outbox-priority-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TerminalDebtCatalogEntry {
    principal: String,
    workspace: String,
    execution_id: String,
    observed_revision: Revision,
    observed_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TerminalDebtLocalHint {
    schema_version: u8,
    observed_revision: Revision,
    observed_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TerminalDebtCatalog {
    schema_version: u8,
    /// True only after a complete authoritative directory walk. Publication
    /// failures remain recoverable because this bit ages out into another walk.
    complete: bool,
    /// Once the bounded map fills, omission is possible and priority mode is
    /// forbidden until a later authoritative pass observes a smaller debt set.
    overflowed: bool,
    /// Accumulates unreadable/unclassified directory observations across every
    /// page of one authoritative rebuild. A later page must not erase an
    /// earlier coverage hole by declaring the catalog complete.
    rebuild_had_errors: bool,
    /// Dirty generation observed when the current authoritative rebuild began.
    /// Completion clears only this exact generation; a terminal writer that
    /// failed publication during the walk therefore cannot be erased by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rebuild_dirty_generation: Option<String>,
    last_authoritative_scan_ms: i64,
    entries: BTreeMap<String, TerminalDebtCatalogEntry>,
    integrity_sha256: String,
}

const BASE_BINDING_SCHEMA_VERSION: u8 = 1;
const BASE_BINDING_CATALOG_SCHEMA_VERSION: u8 = 3;
const BASE_BINDING_HEAD_SCHEMA_VERSION: u8 = 1;
const BASE_RECOVERY_INDEX_SCHEMA_VERSION: u8 = 1;
const BASE_PRESEED_ADMISSION_SCHEMA_VERSION: u8 = 2;
const LEGACY_WRITER_CUTOVER_SCHEMA_VERSION: u8 = 1;
const MAX_BASE_BINDING_BYTES: u64 = 64 * 1024;
const MAX_BASE_BINDING_JSON_NODES: usize = 128;
const MAX_BASE_BINDING_HEAD_BYTES: u64 = 4 * 1024 * 1024;
const MAX_BASE_BINDING_HEAD_JSON_NODES: usize = 50_000;
const BASE_BINDING_MIGRATION_PAGE_VISITS: usize = 2_048;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BaseSegmentBindingRecord {
    schema_version: u8,
    principal: String,
    workspace: String,
    base_execution_id: String,
    exact_segment_id: String,
    explicit_binding: bool,
    integrity_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BaseBindingCatalogMarker {
    schema_version: u8,
    #[serde(default)]
    complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    after_execution_dir: Option<String>,
    #[serde(default)]
    covered_execution_dirs: u64,
    #[serde(default)]
    mixed_writer_compat_until_ms: i64,
    /// A cutover seal cannot trust a catalog completion bit written while old
    /// binaries were still live. These fields bind a fresh, resumable
    /// post-drain walk to the deployment that requested the seal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cutover_refresh_deployment_id: Option<String>,
    #[serde(default)]
    cutover_refresh_complete: bool,
}

/// Immutable deployment assertion written only after every pre-reverse-index
/// process has been drained. No clock or lease expiry can substitute for this
/// operator-owned fact because a healthy old writer can renew indefinitely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyWriterCutover {
    schema_version: u8,
    principal: String,
    workspace: String,
    deployment_id: String,
    retired_at_ms: i64,
    integrity_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BaseBindingHead {
    schema_version: u8,
    principal: String,
    workspace: String,
    base_execution_id: String,
    exact_segments: Vec<String>,
    integrity_sha256: String,
}

/// Revision-stamped small projection for O(k) recovery classification. A
/// stale/missing projection is never trusted: the exact bounded snapshot is
/// loaded once and this derivative is repaired.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SegmentRecoveryIndex {
    schema_version: u8,
    revision: Revision,
    snapshot_stamp: SnapshotFileStamp,
    base_execution_id: String,
    exact_segment_id: String,
    explicit_binding: bool,
    has_continuation_checkpoint: bool,
    terminal_settlement_seq: Option<u64>,
    non_resumable_ended: bool,
    placement: Placement,
    integrity_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BasePreseedAdmission {
    schema_version: u8,
    principal: String,
    workspace: String,
    base_execution_id: String,
    token: String,
    runtime_updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exact_segment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exact_revision: Option<Revision>,
    /// Exact membership generation observed while the admission was claimed.
    /// Final validation and first-seed publication reject any sibling added or
    /// removed after classification.
    #[serde(default)]
    base_binding_head_sha256: String,
    expires_at_ms: i64,
    integrity_sha256: String,
}

const JOURNAL_INDEX_SCHEMA_VERSION: u8 = 1;
const MAX_JOURNAL_INDEX_OFFSETS: usize = 256;
const MAX_JOURNAL_INDEX_BYTES: u64 = 128 * 1024;
const MAX_JOURNAL_INDEX_JSON_NODES: usize = 10_000;

const PLACEMENT_INDEX_SCHEMA_VERSION: u8 = 1;
const MAX_PLACEMENT_INDEX_BYTES: u64 = 16 * 1024;
const MAX_PLACEMENT_INDEX_JSON_NODES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalFileStamp {
    bytes: u64,
    modified_secs: u64,
    modified_nanos: u32,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed_secs: i64,
    #[cfg(unix)]
    changed_nanos: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalOffset {
    seq: u64,
    byte_offset: u64,
    iteration: usize,
    phase: LoopPhase,
    ordinal: u32,
    batch_start: bool,
    chain_before: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalIndex {
    schema_version: u8,
    stamp: JournalFileStamp,
    chain_sha256: String,
    last_seq: u64,
    committed_watermark: u64,
    replayed: ReplayedCursor,
    last_event_seq: Option<u64>,
    offsets: Vec<JournalOffset>,
    integrity_sha256: String,
}

/// File-generation stamp for the immutable snapshot named by a placement
/// projection. Revision alone is not sufficient: an operator or damaged
/// substrate can replace a numbered file in place. A stamp mismatch forces the
/// authoritative bounded snapshot reader before ownership can be declared
/// absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotFileStamp {
    bytes: u64,
    modified_secs: u64,
    modified_nanos: u32,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed_secs: i64,
    #[cfg(unix)]
    changed_nanos: i64,
}

/// Small derived projection used only for durable owner/pin admission. It is
/// exact-revision and exact-file-generation bound, and its digest detects
/// partial or accidental semantic edits. Missing, stale, or corrupt projections
/// are never authority: callers fall back to the full LoopState and repair it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlacementIndex {
    schema_version: u8,
    revision: Revision,
    snapshot_stamp: SnapshotFileStamp,
    placement: Placement,
    integrity_sha256: String,
}

struct CommitJournalFacts {
    last_seq: u64,
    current_ending: Option<EndedRun>,
    next_ending: Option<EndedRun>,
    next_index: Option<JournalIndex>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RevisionEndingBinding {
    revision: Revision,
    ending: Option<EndedRun>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RevisionedEndingMarker {
    schema_version: u8,
    bindings: Vec<RevisionEndingBinding>,
}

/// `ended.json` shipped as a bare [`EndedRun`]. Keep reading that exact shape
/// while every new write uses revision bindings. `EndedRun` denies unknown
/// fields, so the untagged alternatives cannot partially accept each other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum StoredEndingMarker {
    Revisioned(RevisionedEndingMarker),
    Legacy(EndedRun),
}

fn validate_revisioned_ending_marker(
    key: &ExecutionKey,
    marker: &RevisionedEndingMarker,
) -> StoreResult<()> {
    if marker.schema_version != ENDING_MARKER_SCHEMA_VERSION {
        return Err(StoreError::Corrupt {
            key: key.clone(),
            detail: format!(
                "unsupported ending marker schema version {}",
                marker.schema_version
            ),
        });
    }
    if marker.bindings.is_empty() || marker.bindings.len() > 2 {
        return Err(StoreError::Corrupt {
            key: key.clone(),
            detail: format!(
                "an ending marker contained {} revision bindings",
                marker.bindings.len()
            ),
        });
    }
    if marker.bindings.len() == 2 && marker.bindings[0].revision == marker.bindings[1].revision {
        return Err(StoreError::Corrupt {
            key: key.clone(),
            detail: format!(
                "the ending marker repeats committed {}",
                marker.bindings[0].revision
            ),
        });
    }
    Ok(())
}

/// The most directory names one level of a walk holds in memory at a time.
///
/// A walk sorts each level so a [`ScanCursor`] means something, and a level that
/// had to be *fully* materialised to be sorted would make one enormous directory
/// a memory failure rather than a slow scan. So each level keeps only the
/// smallest names still ahead of the cursor, and pages through the rest.
///
/// # It DOES interact with [`MAX_EXECUTIONS_SCANNED`], and an earlier note here
/// said it could not
///
/// That note argued the two ceilings were independent because a pass can never
/// visit more entries than the execution ceiling permits. They are not the same
/// count. `MAX_EXECUTIONS_SCANNED` is incremented once per **execution**
/// directory, at the leaf of the walk; this window is applied at **all three**
/// levels. So a principal or workspace level wider than this window is cut with
/// the execution ceiling nowhere near firing — twenty thousand principals
/// holding no executions between them do it — and that is precisely the case
/// [`ChildDirs::truncated`] and [`WalkOutcome::cut_at`] exist for.
///
/// Sizing it at the execution ceiling is therefore a choice about memory, not a
/// proof about interaction: it keeps the common shape — one level, many
/// executions — to a single page. A reader who takes the two constants for
/// independent would delete the flag that makes the cut recoverable.
const MAX_SCAN_WINDOW: usize = MAX_EXECUTIONS_SCANNED;

/// How many superseded revisions stay on disk.
///
/// More than one so an operator can see what the previous state was after a bad
/// commit; few enough that the directory scan stays short. Load reads the newest
/// only, so this is retention rather than correctness.
const RETAINED_SNAPSHOTS: usize = 3;
const RETAINED_LEASES: usize = 3;

const SNAPSHOT_PREFIX: &str = "snapshot-";
const LEASE_PREFIX: &str = "lease-";
const JSON_SUFFIX: &str = ".json";

/// The loop state store, on a scoped V3 root.
#[derive(Debug, Clone)]
pub struct FsLoopStateStore {
    workspace: ArtifactV2Workspace,
    #[cfg(test)]
    full_snapshot_loads: Arc<AtomicUsize>,
}

impl FsLoopStateStore {
    /// Root at a `magician_data_v3`-shaped directory.
    ///
    /// The same root `FullPauseStore::with_scoped_v3_persistence_root` takes, so
    /// an execution's loop state lands beside its pause records rather than in a
    /// parallel tree an operator has to know about separately.
    pub fn new(persistence_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace: ArtifactV2Workspace::new(persistence_root.into()),
            #[cfg(test)]
            full_snapshot_loads: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Latest durable ownership deadline for an exact execution generation.
    ///
    /// A lease covers a phase while its heartbeat is running; a placement pin
    /// covers the inter-phase gap after that lease is released. Wake recovery
    /// may demote `Executing` only after both authorities have lapsed.
    pub async fn durable_owner_until(
        &self,
        key: &ExecutionKey,
        now_ms: i64,
    ) -> StoreResult<Option<i64>> {
        let store = self.clone();
        let key = key.clone();
        run_fs_io(move || store.durable_owner_until_sync(&key, now_ms)).await
    }

    /// Latest live authority held by a worker other than `expected_worker`.
    ///
    /// Exact delegation recovery uses this after a rolling process has rebuilt
    /// an `Executing` runtime row as `Runnable`. A deadline-only lookup cannot
    /// distinguish the newly composed worker's own pin after a provider error
    /// from a peer that is still settling the same successor segment; only the
    /// latter must defer without parking shared runtime state.
    pub async fn durable_foreign_owner_until(
        &self,
        key: &ExecutionKey,
        expected_worker: &WorkerId,
        now_ms: i64,
    ) -> StoreResult<Option<i64>> {
        let store = self.clone();
        let key = key.clone();
        let expected_worker = expected_worker.clone();
        run_fs_io(move || store.durable_foreign_owner_until_sync(&key, &expected_worker, now_ms))
            .await
    }

    /// Exact restart classification for one runtime execution. The reverse
    /// projection makes candidate classification O(k), where `k` is the number
    /// of segments that runtime has produced. Until an explicit rollout
    /// cutover proves legacy writers are gone, a bounded authoritative scope
    /// pass first repairs bindings those writers may have omitted.
    pub async fn base_execution_recovery_authority(
        &self,
        principal: &str,
        workspace: &str,
        base_execution_id: &str,
        now_ms: i64,
    ) -> StoreResult<BaseExecutionRecoveryAuthority> {
        let store = self.clone();
        let principal = principal.to_owned();
        let workspace = workspace.to_owned();
        let base_execution_id = base_execution_id.to_owned();
        run_fs_io(move || {
            store.base_execution_recovery_authority_sync(
                &principal,
                &workspace,
                &base_execution_id,
                now_ms,
            )
        })
        .await
    }

    /// Classify every requested runtime in one scope. Before explicit legacy
    /// writer retirement this performs at most one bounded compatibility walk
    /// for positive discovery, then downgrades every indexed `Absent` or
    /// `RecoverableExact` result to `Uncertain`. After cutover it is O(sum(k))
    /// over the requested bases and does not scan unrelated execution folders.
    pub async fn scope_base_execution_recovery_authorities(
        &self,
        principal: &str,
        workspace: &str,
        base_execution_ids: Vec<String>,
        now_ms: i64,
    ) -> StoreResult<ScopeBaseExecutionRecoveryAuthorities> {
        let store = self.clone();
        let principal = principal.to_owned();
        let workspace = workspace.to_owned();
        run_fs_io(move || {
            store.scope_base_execution_recovery_authorities_sync(
                &principal,
                &workspace,
                base_execution_ids,
                now_ms,
            )
        })
        .await
    }

    /// Whether an operator has durably asserted that every legacy writer for
    /// this scope was drained before the reverse-index protocol became
    /// authoritative.
    pub async fn legacy_writers_retired(
        &self,
        principal: &str,
        workspace: &str,
    ) -> StoreResult<bool> {
        let store = self.clone();
        let principal = principal.to_owned();
        let workspace = workspace.to_owned();
        run_fs_io(move || store.legacy_writers_retired_sync(&principal, &workspace)).await
    }

    /// Irreversibly seal the explicit legacy-writer cutover for one scope.
    /// Callers must establish the operational precondition—every old binary is
    /// stopped—before invoking this. The immutable record is idempotent and a
    /// conflicting deployment id cannot replace it.
    pub async fn seal_legacy_writer_cutover(
        &self,
        principal: &str,
        workspace: &str,
        deployment_id: &str,
        retired_at_ms: i64,
    ) -> StoreResult<()> {
        let store = self.clone();
        let principal = principal.to_owned();
        let workspace = workspace.to_owned();
        let deployment_id = deployment_id.to_owned();
        run_fs_io(move || {
            store.seal_legacy_writer_cutover_sync(
                &principal,
                &workspace,
                &deployment_id,
                retired_at_ms,
            )
        })
        .await
    }

    /// Claim the bounded no-snapshot launch window. This is intentionally a
    /// distinct durable token from the runtime row: process-local launch sets
    /// cannot prevent two rolling peers from re-admitting the same stale
    /// `Executing` row before either reaches its first LoopState commit.
    pub async fn claim_base_execution_preseed_admission(
        &self,
        principal: &str,
        workspace: &str,
        base_execution_id: &str,
        runtime_updated_at: i64,
        now_ms: i64,
        ttl_ms: i64,
    ) -> StoreResult<Option<String>> {
        let store = self.clone();
        let principal = principal.to_owned();
        let workspace = workspace.to_owned();
        let base_execution_id = base_execution_id.to_owned();
        run_fs_io(move || {
            if !matches!(
                store.base_execution_recovery_authority_sync(
                    &principal,
                    &workspace,
                    &base_execution_id,
                    now_ms,
                )?,
                BaseExecutionRecoveryAuthority::Absent
            ) {
                return Ok(None);
            }
            let binding_dir = store.base_binding_dir(&principal, &workspace, &base_execution_id);
            let _catalog = acquire_private_catalog_lock(&binding_dir.join("catalog.lock"))
                .map_err(|error| StoreError::Unavailable {
                    detail: format!("pre-seed admission catalog lock failed: {error}"),
                })?;
            let mut head =
                store.read_base_binding_head_sync(&principal, &workspace, &base_execution_id)?;
            if head
                .as_ref()
                .is_some_and(|head| !head.exact_segments.is_empty())
            {
                return Ok(None);
            }
            if head.is_none() {
                let mut empty = BaseBindingHead {
                    schema_version: BASE_BINDING_HEAD_SCHEMA_VERSION,
                    principal: principal.clone(),
                    workspace: workspace.clone(),
                    base_execution_id: base_execution_id.clone(),
                    exact_segments: Vec::new(),
                    integrity_sha256: String::new(),
                };
                empty.integrity_sha256 = base_binding_head_integrity(&empty);
                store.persist_base_binding_head_locked(&empty)?;
                head = Some(empty);
            }
            let head_sha256 = head
                .as_ref()
                .expect("pre-seed admission persists an empty member head")
                .integrity_sha256
                .clone();
            let path = binding_dir.join("preseed.admission");
            let current: Option<BasePreseedAdmission> =
                read_bounded_json(&path, MAX_BASE_BINDING_BYTES, MAX_BASE_BINDING_JSON_NODES)
                    .map_err(|error| StoreError::Unavailable {
                        detail: format!("pre-seed admission read failed: {error}"),
                    })?;
            if current.as_ref().is_some_and(|current| {
                valid_base_preseed_admission(current, &principal, &workspace, &base_execution_id)
                    && current.expires_at_ms > now_ms
            }) {
                return Ok(None);
            }
            if current.is_some()
                && current.as_ref().is_none_or(|current| {
                    !valid_base_preseed_admission(
                        current,
                        &principal,
                        &workspace,
                        &base_execution_id,
                    )
                })
            {
                return Err(StoreError::Unavailable {
                    detail: "pre-seed admission record is invalid".to_owned(),
                });
            }
            let mut admission = BasePreseedAdmission {
                schema_version: BASE_PRESEED_ADMISSION_SCHEMA_VERSION,
                principal,
                workspace,
                base_execution_id,
                token: Uuid::new_v4().to_string(),
                runtime_updated_at,
                exact_segment: None,
                exact_revision: None,
                base_binding_head_sha256: head_sha256,
                expires_at_ms: now_ms.saturating_add(ttl_ms.max(1)),
                integrity_sha256: String::new(),
            };
            admission.integrity_sha256 = base_preseed_admission_integrity(&admission);
            let encoded =
                serde_json::to_vec(&admission).map_err(|error| StoreError::Unavailable {
                    detail: format!("pre-seed admission encode failed: {error}"),
                })?;
            overwrite_derived_atomically(&path, &encoded).map_err(|error| {
                StoreError::Unavailable {
                    detail: format!("pre-seed admission publication failed: {error}"),
                }
            })?;
            Ok(Some(admission.token))
        })
        .await
    }

    pub async fn validate_base_execution_preseed_admission(
        &self,
        principal: &str,
        workspace: &str,
        base_execution_id: &str,
        token: &str,
        runtime_updated_at: i64,
        now_ms: i64,
    ) -> StoreResult<bool> {
        let store = self.clone();
        let principal = principal.to_owned();
        let workspace = workspace.to_owned();
        let base_execution_id = base_execution_id.to_owned();
        let token = token.to_owned();
        run_fs_io(move || {
            let binding_dir = store.base_binding_dir(&principal, &workspace, &base_execution_id);
            let _catalog = acquire_private_catalog_lock(&binding_dir.join("catalog.lock"))
                .map_err(|error| StoreError::Unavailable {
                    detail: format!("pre-seed validation catalog lock failed: {error}"),
                })?;
            let path = binding_dir.join("preseed.admission");
            let admission: Option<BasePreseedAdmission> =
                read_bounded_json(&path, MAX_BASE_BINDING_BYTES, MAX_BASE_BINDING_JSON_NODES)
                    .map_err(|error| StoreError::Unavailable {
                        detail: format!("pre-seed admission validation read failed: {error}"),
                    })?;
            let Some(head) =
                store.read_base_binding_head_sync(&principal, &workspace, &base_execution_id)?
            else {
                return Ok(false);
            };
            let Some(mut admission) = admission else {
                return Ok(false);
            };
            let valid = head.exact_segments.is_empty()
                && head.integrity_sha256 == admission.base_binding_head_sha256
                && valid_base_preseed_admission(
                    &admission,
                    &principal,
                    &workspace,
                    &base_execution_id,
                )
                && admission.token == token
                && admission.runtime_updated_at == runtime_updated_at
                && admission.exact_segment.is_none()
                && admission.exact_revision.is_none()
                && admission.expires_at_ms > now_ms;
            if !valid {
                return Ok(false);
            }
            renew_base_recovery_admission_locked(&path, &mut admission, now_ms)?;
            Ok(true)
        })
        .await
    }

    pub async fn claim_base_execution_exact_recovery_admission(
        &self,
        principal: &str,
        workspace: &str,
        base_execution_id: &str,
        exact_segment: &str,
        exact_revision: Revision,
        runtime_updated_at: i64,
        now_ms: i64,
        ttl_ms: i64,
    ) -> StoreResult<Option<String>> {
        let store = self.clone();
        let principal = principal.to_owned();
        let workspace = workspace.to_owned();
        let base_execution_id = base_execution_id.to_owned();
        let exact_segment = exact_segment.to_owned();
        run_fs_io(move || {
            if !matches!(
                store.base_execution_recovery_authority_sync(
                    &principal,
                    &workspace,
                    &base_execution_id,
                    now_ms,
                )?,
                BaseExecutionRecoveryAuthority::RecoverableExact {
                    exact_segment: ref candidate,
                    revision,
                } if candidate == &exact_segment && revision == exact_revision
            ) {
                return Ok(None);
            }
            let binding_dir = store.base_binding_dir(&principal, &workspace, &base_execution_id);
            // Capture the exact authoritative member generation, then classify
            // once more. The final catalog-lock comparison closes both sides of
            // classification: a sibling present before this snapshot affects
            // the second authority result, and one published afterward changes
            // the head digest before admission publication.
            let observed_head_sha256 = {
                let _catalog = acquire_private_catalog_lock(&binding_dir.join("catalog.lock"))
                    .map_err(|error| StoreError::Unavailable {
                        detail: format!("exact recovery head snapshot lock failed: {error}"),
                    })?;
                store.reconcile_base_binding_head_locked(
                    &principal,
                    &workspace,
                    &base_execution_id,
                )?;
                store
                    .read_base_binding_head_sync(&principal, &workspace, &base_execution_id)?
                    .ok_or_else(|| StoreError::Unavailable {
                        detail: "exact recovery candidate has no authoritative member head"
                            .to_owned(),
                    })?
                    .integrity_sha256
            };
            if !matches!(
                store.base_execution_recovery_authority_sync(
                    &principal,
                    &workspace,
                    &base_execution_id,
                    now_ms,
                )?,
                BaseExecutionRecoveryAuthority::RecoverableExact {
                    exact_segment: ref candidate,
                    revision,
                } if candidate == &exact_segment && revision == exact_revision
            ) {
                return Ok(None);
            }
            let _catalog = acquire_private_catalog_lock(&binding_dir.join("catalog.lock"))
                .map_err(|error| StoreError::Unavailable {
                    detail: format!("exact recovery admission catalog lock failed: {error}"),
                })?;
            store.reconcile_base_binding_head_locked(
                &principal,
                &workspace,
                &base_execution_id,
            )?;
            let Some(head) = store.read_base_binding_head_sync(
                &principal,
                &workspace,
                &base_execution_id,
            )? else {
                return Ok(None);
            };
            if head.integrity_sha256 != observed_head_sha256
                || !head.exact_segments.iter().any(|segment| segment == &exact_segment)
            {
                return Ok(None);
            }
            let key = ExecutionKey::new(
                principal.clone(),
                workspace.clone(),
                exact_segment.clone(),
            )?;
            if store.latest_revision(&key)? != exact_revision {
                return Ok(None);
            }
            let path = binding_dir.join("preseed.admission");
            let existing: Option<BasePreseedAdmission> = read_bounded_json(
                &path,
                MAX_BASE_BINDING_BYTES,
                MAX_BASE_BINDING_JSON_NODES,
            )
            .map_err(|error| StoreError::Unavailable {
                detail: format!("exact recovery admission read failed: {error}"),
            })?;
            if existing.as_ref().is_some_and(|admission| {
                valid_base_preseed_admission(
                    admission,
                    &principal,
                    &workspace,
                    &base_execution_id,
                ) && admission.expires_at_ms > now_ms
            }) {
                return Ok(None);
            }
            let mut admission = BasePreseedAdmission {
                schema_version: BASE_PRESEED_ADMISSION_SCHEMA_VERSION,
                principal,
                workspace,
                base_execution_id,
                token: Uuid::new_v4().to_string(),
                runtime_updated_at,
                exact_segment: Some(exact_segment),
                exact_revision: Some(exact_revision),
                base_binding_head_sha256: observed_head_sha256,
                expires_at_ms: now_ms.saturating_add(ttl_ms.max(1)),
                integrity_sha256: String::new(),
            };
            admission.integrity_sha256 = base_preseed_admission_integrity(&admission);
            let encoded = serde_json::to_vec(&admission).map_err(|error| {
                StoreError::Unavailable {
                    detail: format!("exact recovery admission encode failed: {error}"),
                }
            })?;
            overwrite_derived_atomically(&path, &encoded).map_err(|error| {
                StoreError::Unavailable {
                    detail: format!("exact recovery admission publication failed: {error}"),
                }
            })?;
            let token = admission.token.clone();
            drop(_catalog);
            let worker = crate::magician_v2::execution::agentic::run_loop::state::recovery_admission_worker_id(&token);
            match store.claim_sync(
                &key,
                &worker,
                Duration::from_millis(
                    crate::magician_v2::execution::agentic::run_loop::state::PIN_TTL_MS as u64,
                ),
            ) {
                Ok(_) => Ok(Some(token)),
                Err(StoreError::LeaseHeld { .. }) => {
                    let _catalog = acquire_private_catalog_lock(&binding_dir.join("catalog.lock"))
                        .map_err(|error| StoreError::Unavailable {
                            detail: format!("failed exact admission rollback lock: {error}"),
                        })?;
                    let current: Option<BasePreseedAdmission> = read_bounded_json(
                        &path,
                        MAX_BASE_BINDING_BYTES,
                        MAX_BASE_BINDING_JSON_NODES,
                    )
                    .map_err(|error| StoreError::Unavailable {
                        detail: format!("failed exact admission rollback read: {error}"),
                    })?;
                    if current
                        .as_ref()
                        .is_some_and(|current| current.token == token)
                    {
                        match std::fs::remove_file(&path) {
                            Ok(()) => sync_parent_directory(&path).map_err(|error| {
                                StoreError::Unavailable {
                                    detail: format!(
                                        "failed exact admission rollback sync: {error}"
                                    ),
                                }
                            })?,
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                            Err(error) => {
                                return Err(StoreError::Unavailable {
                                    detail: format!(
                                        "failed exact admission rollback removal: {error}"
                                    ),
                                })
                            },
                        }
                    }
                    Ok(None)
                },
                Err(error) => Err(error),
            }
        })
        .await
    }

    pub async fn validate_base_execution_exact_recovery_admission(
        &self,
        principal: &str,
        workspace: &str,
        base_execution_id: &str,
        exact_segment: &str,
        exact_revision: Revision,
        token: &str,
        runtime_updated_at: i64,
        now_ms: i64,
    ) -> StoreResult<bool> {
        let store = self.clone();
        let principal = principal.to_owned();
        let workspace = workspace.to_owned();
        let base_execution_id = base_execution_id.to_owned();
        let exact_segment = exact_segment.to_owned();
        let token = token.to_owned();
        run_fs_io(move || {
            let binding_dir = store.base_binding_dir(&principal, &workspace, &base_execution_id);
            let catalog = acquire_private_catalog_lock(&binding_dir.join("catalog.lock"))
                .map_err(|error| StoreError::Unavailable {
                    detail: format!("exact recovery validation catalog lock failed: {error}"),
                })?;
            let path = binding_dir.join("preseed.admission");
            let admission: Option<BasePreseedAdmission> = read_bounded_json(
                &path,
                MAX_BASE_BINDING_BYTES,
                MAX_BASE_BINDING_JSON_NODES,
            )
            .map_err(|error| StoreError::Unavailable {
                detail: format!("exact recovery admission validation read failed: {error}"),
            })?;
            let key = ExecutionKey::new(principal.clone(), workspace.clone(), exact_segment.clone())?;
            store.reconcile_base_binding_head_locked(
                &principal,
                &workspace,
                &base_execution_id,
            )?;
            let Some(head) = store.read_base_binding_head_sync(
                &principal,
                &workspace,
                &base_execution_id,
            )? else {
                return Ok(false);
            };
            let Some(mut admission) = admission else {
                return Ok(false);
            };
            let token_valid = valid_base_preseed_admission(
                    &admission,
                    &principal,
                    &workspace,
                    &base_execution_id,
                ) && admission.schema_version == BASE_PRESEED_ADMISSION_SCHEMA_VERSION
                && admission.token == token
                && admission.runtime_updated_at == runtime_updated_at
                && admission.exact_segment.as_deref() == Some(exact_segment.as_str())
                && admission.exact_revision == Some(exact_revision)
                && admission.expires_at_ms > now_ms
                && head.integrity_sha256 == admission.base_binding_head_sha256
                && head.exact_segments.iter().any(|segment| segment == &exact_segment)
                && store.latest_revision(&key)? == exact_revision;
            if token_valid {
                renew_base_recovery_admission_locked(&path, &mut admission, now_ms)?;
            }
            drop(catalog);
            let expected_worker = crate::magician_v2::execution::agentic::run_loop::state::recovery_admission_worker_id(&token);
            Ok(token_valid
                && store
                    .durable_foreign_owner_until_sync(&key, &expected_worker, now_ms)?
                    .is_none())
        })
        .await
    }

    fn base_execution_recovery_authority_sync(
        &self,
        principal: &str,
        workspace: &str,
        base_execution_id: &str,
        now_ms: i64,
    ) -> StoreResult<BaseExecutionRecoveryAuthority> {
        self.ensure_base_binding_catalog_sync(principal, workspace)?;
        let marker_path = self
            .base_bindings_root(principal, workspace)
            .join("catalog.complete.json");
        read_base_binding_catalog_marker(&marker_path)?.ok_or_else(|| StoreError::Unavailable {
            detail: "base binding catalog completion disappeared after migration".to_owned(),
        })?;
        let legacy_writers_retired = self.legacy_writers_retired_sync(principal, workspace)?;
        let authority = self.base_execution_recovery_authority_indexed_sync(
            principal,
            workspace,
            base_execution_id,
            now_ms,
        )?;
        Ok(restrict_uncutover_authority(
            authority,
            legacy_writers_retired,
        ))
    }

    fn scope_base_execution_recovery_authorities_sync(
        &self,
        principal: &str,
        workspace: &str,
        base_execution_ids: Vec<String>,
        now_ms: i64,
    ) -> StoreResult<ScopeBaseExecutionRecoveryAuthorities> {
        self.ensure_base_binding_catalog_sync(principal, workspace)?;
        let marker_path = self
            .base_bindings_root(principal, workspace)
            .join("catalog.complete.json");
        read_base_binding_catalog_marker(&marker_path)?.ok_or_else(|| StoreError::Unavailable {
            detail: "base binding catalog completion disappeared after migration".to_owned(),
        })?;
        let mut requested = base_execution_ids
            .into_iter()
            .filter(|base| !base.trim().is_empty())
            .collect::<BTreeSet<_>>();
        if requested.len() > MAX_EXECUTIONS_SCANNED {
            return Err(StoreError::Unavailable {
                detail: format!(
                    "scope recovery authority request exceeded {MAX_EXECUTIONS_SCANNED} base executions"
                ),
            });
        }
        let legacy_writers_retired = self.legacy_writers_retired_sync(principal, workspace)?;
        if !legacy_writers_retired && !requested.is_empty() {
            // Discovery only. An old writer does not take our catalog lock, so
            // even a complete scan cannot prove absence or exclusive exact
            // ownership. It can safely add positive members; grant-shaped
            // results below remain downgraded until explicit cutover.
            self.repair_scope_base_bindings_from_compat_scan_sync(
                principal, workspace, &requested,
            )?;
        }
        let mut authorities = BTreeMap::new();
        for base_execution_id in std::mem::take(&mut requested) {
            let authority = self.base_execution_recovery_authority_indexed_sync(
                principal,
                workspace,
                &base_execution_id,
                now_ms,
            )?;
            authorities.insert(
                base_execution_id,
                restrict_uncutover_authority(authority, legacy_writers_retired),
            );
        }
        Ok(ScopeBaseExecutionRecoveryAuthorities {
            legacy_writers_retired,
            authorities,
        })
    }

    fn legacy_writers_retired_sync(&self, principal: &str, workspace: &str) -> StoreResult<bool> {
        Ok(self
            .read_legacy_writer_cutover_sync(principal, workspace)?
            .is_some())
    }

    fn read_legacy_writer_cutover_sync(
        &self,
        principal: &str,
        workspace: &str,
    ) -> StoreResult<Option<LegacyWriterCutover>> {
        let path = self.legacy_writer_cutover_path(principal, workspace);
        let cutover: Option<LegacyWriterCutover> =
            read_bounded_json(&path, MAX_BASE_BINDING_BYTES, MAX_BASE_BINDING_JSON_NODES).map_err(
                |error| StoreError::Unavailable {
                    detail: format!("{}: {error}", path.display()),
                },
            )?;
        let Some(cutover) = cutover else {
            return Ok(None);
        };
        if valid_legacy_writer_cutover(&cutover, principal, workspace) {
            Ok(Some(cutover))
        } else {
            Err(StoreError::Unavailable {
                detail: format!("{} is not a valid legacy-writer cutover", path.display()),
            })
        }
    }

    /// Synchronous seal, for callers already off the async runtime (the test
    /// harness builds its service synchronously).
    pub(crate) fn seal_legacy_writer_cutover_sync(
        &self,
        principal: &str,
        workspace: &str,
        deployment_id: &str,
        retired_at_ms: i64,
    ) -> StoreResult<()> {
        let deployment_id = deployment_id.trim();
        if deployment_id.is_empty()
            || deployment_id.len() > 128
            || deployment_id.chars().any(char::is_control)
            || retired_at_ms <= 0
        {
            return Err(StoreError::Unavailable {
                detail:
                    "legacy-writer cutover requires a bounded deployment id and positive timestamp"
                        .to_owned(),
            });
        }
        let root = self.base_bindings_root(principal, workspace);
        let _migration =
            acquire_private_catalog_lock(&root.join("migration.lock")).map_err(|error| {
                StoreError::Unavailable {
                    detail: format!("legacy-writer cutover lock failed: {error}"),
                }
            })?;
        if let Some(existing) = self.read_legacy_writer_cutover_sync(principal, workspace)? {
            return if existing.deployment_id == deployment_id {
                Ok(())
            } else {
                Err(StoreError::Unavailable {
                    detail: format!(
                        "legacy writers were already retired by deployment {}",
                        existing.deployment_id
                    ),
                })
            };
        }
        let marker_path = root.join("catalog.complete.json");
        let existing_marker = read_base_binding_catalog_marker(&marker_path)?;
        let mut marker = existing_marker.unwrap_or_else(base_binding_catalog_marker_initial);
        if marker.cutover_refresh_deployment_id.as_deref() != Some(deployment_id) {
            // An ordinary completed migration may have run while legacy writers
            // were still active. Start again from the first exact directory
            // after the operator has asserted that those writers are drained.
            marker.complete = false;
            marker.after_execution_dir = None;
            marker.covered_execution_dirs = 0;
            marker.cutover_refresh_deployment_id = Some(deployment_id.to_owned());
            marker.cutover_refresh_complete = false;
        }
        if !marker.cutover_refresh_complete {
            let executions_root = self
                .workspace
                .runtime_root(principal, workspace)
                .join("executions");
            let mut after = marker.after_execution_dir.clone();
            let mut visited = 0usize;
            loop {
                let page = ordered_child_dirs(&executions_root, after.as_deref(), false)?;
                if page.unclassified {
                    return Err(StoreError::Unavailable {
                        detail: "post-drain cutover scan could not classify the complete execution directory"
                            .to_owned(),
                    });
                }
                let remaining = BASE_BINDING_MIGRATION_PAGE_VISITS.saturating_sub(visited);
                let visit_count = page.names.len().min(remaining);
                for name in page.names.iter().take(visit_count) {
                    visited = visited.saturating_add(1);
                    let Some(key) = self.key_at(&executions_root.join(name))? else {
                        continue;
                    };
                    let Some(committed) = self.load_sync(&key)? else {
                        continue;
                    };
                    let Some(record) =
                        self.base_binding_record_for_state(&key, &committed.state)?
                    else {
                        continue;
                    };
                    let binding_dir =
                        self.base_binding_dir(principal, workspace, &record.base_execution_id);
                    let _base = acquire_private_catalog_lock(&binding_dir.join("catalog.lock"))
                        .map_err(|error| StoreError::Unavailable {
                            detail: format!("post-drain cutover catalog lock failed: {error}"),
                        })?;
                    reap_leaked_temporaries(&binding_dir);
                    self.publish_base_binding_record_locked(&record)?;
                }
                after = page
                    .names
                    .get(visit_count.saturating_sub(1))
                    .cloned()
                    .or(after);
                let page_has_more = visit_count < page.names.len() || page.truncated;
                if !page_has_more {
                    marker.complete = true;
                    marker.cutover_refresh_complete = true;
                    marker.after_execution_dir = after;
                    marker.covered_execution_dirs =
                        marker.covered_execution_dirs.saturating_add(visited as u64);
                    break;
                }
                if after.is_none() {
                    return Err(StoreError::Unavailable {
                        detail: "post-drain cutover scan lost its cursor".to_owned(),
                    });
                }
                if visited >= BASE_BINDING_MIGRATION_PAGE_VISITS {
                    marker.after_execution_dir = after;
                    marker.covered_execution_dirs =
                        marker.covered_execution_dirs.saturating_add(visited as u64);
                    persist_base_binding_catalog_marker(&marker_path, &marker)?;
                    return Err(StoreError::Unavailable {
                        detail: "post-drain cutover scan advanced one bounded page; rerun the same deployment cutover until the fresh catalog pass completes"
                            .to_owned(),
                    });
                }
            }
            persist_base_binding_catalog_marker(&marker_path, &marker)?;
        }
        let mut cutover = LegacyWriterCutover {
            schema_version: LEGACY_WRITER_CUTOVER_SCHEMA_VERSION,
            principal: principal.to_owned(),
            workspace: workspace.to_owned(),
            deployment_id: deployment_id.to_owned(),
            retired_at_ms,
            integrity_sha256: String::new(),
        };
        cutover.integrity_sha256 = legacy_writer_cutover_integrity(&cutover);
        let encoded = serde_json::to_vec(&cutover).map_err(|error| StoreError::Unavailable {
            detail: format!("legacy-writer cutover encode failed: {error}"),
        })?;
        match publish_new_file(
            &self.legacy_writer_cutover_path(principal, workspace),
            &encoded,
        ) {
            Ok(Publish::Published) => Ok(()),
            Ok(Publish::AlreadyExists) => {
                let existing = self
                    .read_legacy_writer_cutover_sync(principal, workspace)?
                    .ok_or_else(|| StoreError::Unavailable {
                        detail: "legacy-writer cutover disappeared after publication conflict"
                            .to_owned(),
                    })?;
                if existing.deployment_id == deployment_id {
                    Ok(())
                } else {
                    Err(StoreError::Unavailable {
                        detail: format!(
                            "legacy writers were concurrently retired by deployment {}",
                            existing.deployment_id
                        ),
                    })
                }
            },
            Err(error) => Err(StoreError::Unavailable {
                detail: format!("legacy-writer cutover publication failed: {error}"),
            }),
        }
    }

    fn base_execution_recovery_authority_indexed_sync(
        &self,
        principal: &str,
        workspace: &str,
        base_execution_id: &str,
        now_ms: i64,
    ) -> StoreResult<BaseExecutionRecoveryAuthority> {
        // Membership publication is serialized by this exact base lock and a
        // first segment commit retains it through snapshot CAS. Read names,
        // release before taking per-segment locks, then compare membership
        // under the lock again. This avoids base->segment / segment->base lock
        // inversion without allowing a concurrently seeded segment to vanish
        // between the catalog snapshot and the ownership decision.
        for _ in 0..3 {
            let binding_dir = self.base_binding_dir(principal, workspace, base_execution_id);
            if !binding_dir.exists() {
                return Ok(BaseExecutionRecoveryAuthority::Absent);
            }
            let before = {
                let _catalog = acquire_private_catalog_lock(&binding_dir.join("catalog.lock"))
                    .map_err(|error| StoreError::Unavailable {
                        detail: format!("base execution catalog lock failed: {error}"),
                    })?;
                reap_leaked_temporaries(&binding_dir);
                let admission_path = binding_dir.join("preseed.admission");
                let admission: Option<BasePreseedAdmission> = read_bounded_json(
                    &admission_path,
                    MAX_BASE_BINDING_BYTES,
                    MAX_BASE_BINDING_JSON_NODES,
                )
                .map_err(|error| StoreError::Unavailable {
                    detail: format!("base recovery admission read failed: {error}"),
                })?;
                if let Some(admission) = admission {
                    if !valid_base_preseed_admission(
                        &admission,
                        principal,
                        workspace,
                        base_execution_id,
                    ) {
                        return Err(StoreError::Unavailable {
                            detail: "base recovery admission is invalid".to_owned(),
                        });
                    }
                    if admission.expires_at_ms > now_ms {
                        self.reconcile_base_binding_head_locked(
                            principal,
                            workspace,
                            base_execution_id,
                        )?;
                        if self.base_recovery_admission_consumed_locked(
                            principal,
                            workspace,
                            base_execution_id,
                            &admission,
                        )? {
                            retire_base_recovery_admission_file_locked(&admission_path)?;
                        } else {
                            return Ok(BaseExecutionRecoveryAuthority::Live {
                                exact_segments: admission.exact_segment.into_iter().collect(),
                                until_ms: admission.expires_at_ms,
                            });
                        }
                    } else {
                        retire_base_recovery_admission_file_locked(&admission_path)?;
                    }
                }
                self.reconcile_base_binding_head_locked(principal, workspace, base_execution_id)?;
                self.read_base_binding_records_sync(principal, workspace, base_execution_id)?
            };

            let mut live_until = None::<i64>;
            let mut live_segments = Vec::new();
            let mut recoverable = Vec::<(String, Revision)>::new();
            let mut uncertain = Vec::new();
            let mut orphaned_preseed = Vec::new();
            let mut settlement_pending = Vec::new();
            for record in &before {
                let key = ExecutionKey::new(
                    principal.to_owned(),
                    workspace.to_owned(),
                    record.exact_segment_id.clone(),
                )?;
                let revision = self.latest_revision(&key)?;
                if revision == Revision::INITIAL {
                    // A reverse record is published before the first snapshot.
                    // Seeing it without a snapshot is a concurrent/crashed
                    // pre-seed generation, not permission to start another.
                    orphaned_preseed.push(record.exact_segment_id.clone());
                    continue;
                }
                let index = match self.current_segment_recovery_index(&key, revision, record) {
                    Some(index) => index,
                    None => {
                        let committed = self.load_sync(&key)?.ok_or_else(|| StoreError::Corrupt {
                            key: key.clone(),
                            detail: format!(
                                "latest revision is {revision}, but its committed snapshot is absent"
                            ),
                        })?;
                        let exact_binding =
                            committed
                                .state
                                .segment_binding
                                .as_ref()
                                .is_some_and(|binding| {
                                    binding.base_execution_id == base_execution_id
                                        && binding.exact_segment_id == record.exact_segment_id
                                });
                        let legacy_binding = !record.explicit_binding
                            && committed.state.segment_binding.is_none()
                            && committed.state.identity.execution_id.as_deref()
                                == Some(base_execution_id);
                        if !(exact_binding || legacy_binding) {
                            return Err(StoreError::Corrupt {
                                key: key.clone(),
                                detail: "base execution reverse binding does not match its committed snapshot"
                                    .to_owned(),
                            });
                        }
                        let ended = self.ended_run(&key, revision)?.is_some();
                        self.persist_segment_recovery_index(
                            &key,
                            revision,
                            &committed.state,
                            ended,
                        )?;
                        self.current_segment_recovery_index(&key, revision, record)
                            .ok_or_else(|| StoreError::Unavailable {
                                detail: format!(
                                    "{} recovery index could not be verified after repair",
                                    key
                                ),
                            })?
                    },
                };
                if let Some(terminal_seq) = index.terminal_settlement_seq {
                    let cursor = self.load_projector_cursor_sync(&key)?;
                    let settled = cursor.as_ref().is_some_and(|cursor| {
                        cursor.runtime_settled_terminal_seq() == Some(terminal_seq)
                            && cursor.emitted_through_seq() >= terminal_seq
                    });
                    if !settled {
                        settlement_pending.push(record.exact_segment_id.clone());
                    }
                    continue;
                }
                if index.non_resumable_ended {
                    continue;
                }
                if let Some(until_ms) = self.durable_owner_until_sync(&key, now_ms)? {
                    live_until = Some(live_until.map_or(until_ms, |at| at.max(until_ms)));
                    live_segments.push(record.exact_segment_id.clone());
                    continue;
                }
                if index.explicit_binding && index.has_continuation_checkpoint {
                    recoverable.push((record.exact_segment_id.clone(), revision));
                } else {
                    uncertain.push(record.exact_segment_id.clone());
                }
            }

            let mut removed_orphaned_preseed = false;
            let after = {
                let _catalog = acquire_private_catalog_lock(&binding_dir.join("catalog.lock"))
                    .map_err(|error| StoreError::Unavailable {
                        detail: format!("base execution catalog recheck failed: {error}"),
                    })?;
                self.reconcile_base_binding_head_locked(principal, workspace, base_execution_id)?;
                let after =
                    self.read_base_binding_records_sync(principal, workspace, base_execution_id)?;
                if before == after && !orphaned_preseed.is_empty() {
                    for segment in &orphaned_preseed {
                        let key = ExecutionKey::new(
                            principal.to_owned(),
                            workspace.to_owned(),
                            segment.clone(),
                        )?;
                        // A first commit retains this catalog lock through its
                        // snapshot CAS. Absence while holding the lock proves a
                        // pre-snapshot publisher crashed; its immutable reverse
                        // record is safe to retire and must not defer forever.
                        if self.load_sync(&key)?.is_none() {
                            let record = before
                                .iter()
                                .find(|record| record.exact_segment_id == *segment)
                                .expect("orphan came from the catalog snapshot");
                            self.remove_base_binding_head_member_locked(record)?;
                            let path = self.base_binding_record_path(record);
                            match std::fs::remove_file(&path) {
                                Ok(()) => removed_orphaned_preseed = true,
                                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                                    removed_orphaned_preseed = true;
                                },
                                Err(error) => {
                                    return Err(StoreError::Unavailable {
                                        detail: format!(
                                        "orphaned base binding {} could not be retired: {error}",
                                        path.display()
                                    ),
                                    })
                                },
                            }
                        }
                    }
                    if removed_orphaned_preseed {
                        sync_parent_directory(&binding_dir.join("catalog.lock")).map_err(
                            |error| StoreError::Unavailable {
                                detail: format!(
                                    "orphaned base binding retirement was not synced: {error}"
                                ),
                            },
                        )?;
                    }
                }
                after
            };
            if removed_orphaned_preseed {
                continue;
            }
            if before != after {
                continue;
            }
            live_segments.sort();
            live_segments.dedup();
            uncertain.sort();
            uncertain.dedup();
            recoverable.sort();
            recoverable.dedup();
            if let Some(until_ms) = live_until {
                return Ok(BaseExecutionRecoveryAuthority::Live {
                    exact_segments: live_segments,
                    until_ms,
                });
            }
            settlement_pending.sort();
            settlement_pending.dedup();
            if !settlement_pending.is_empty() {
                return Ok(BaseExecutionRecoveryAuthority::SettlementPending {
                    exact_segments: settlement_pending,
                });
            }
            if uncertain.is_empty() && recoverable.len() == 1 {
                let (exact_segment, revision) = recoverable.pop().expect("one exact candidate");
                return Ok(BaseExecutionRecoveryAuthority::RecoverableExact {
                    exact_segment,
                    revision,
                });
            }
            if uncertain.is_empty() && recoverable.is_empty() {
                return Ok(BaseExecutionRecoveryAuthority::Absent);
            }
            uncertain.extend(recoverable.into_iter().map(|(segment, _)| segment));
            uncertain.sort();
            uncertain.dedup();
            return Ok(BaseExecutionRecoveryAuthority::Uncertain {
                exact_segments: uncertain,
                reason:
                    "base execution has zero or multiple exact recoverable nonterminal segments"
                        .to_owned(),
            });
        }
        Err(StoreError::Unavailable {
            detail:
                "base execution segment membership changed throughout the bounded recovery scan"
                    .to_owned(),
        })
    }

    fn durable_foreign_owner_until_sync(
        &self,
        key: &ExecutionKey,
        expected_worker: &WorkerId,
        now_ms: i64,
    ) -> StoreResult<Option<i64>> {
        let _lock = self.lock_execution(key)?;
        self.ensure_directory_is_not_another_scopes(key)?;
        let lease_until = self
            .blocking_lease(key, now_ms)?
            .and_then(|lease| (&lease.worker != expected_worker).then_some(lease.expires_at_ms));
        let revision = self.latest_revision(key)?;
        let placement = if revision == Revision::INITIAL {
            None
        } else {
            self.ensure_key_record_recheck(key, &self.key_path(key))?;
            match self.current_placement_index(key, revision) {
                Some(placement) => Some(placement),
                None => {
                    let committed = self.load_sync(key)?.ok_or_else(|| StoreError::Corrupt {
                        key: key.clone(),
                        detail: format!(
                            "latest revision is {revision}, but its committed snapshot is absent"
                        ),
                    })?;
                    if committed.revision != revision {
                        return Err(StoreError::Conflict {
                            expected: revision,
                            found: committed.revision,
                        });
                    }
                    if let Err(error) = self.persist_placement_index(
                        key,
                        committed.revision,
                        &committed.state.placement,
                    ) {
                        warn!(
                            execution = %key,
                            %error,
                            "[LOOP-STORE] foreign-owner lookup could not repair the derived placement index"
                        );
                    }
                    Some(committed.state.placement)
                },
            }
        };
        let pin_until = placement.and_then(|placement| match placement {
            Placement::Pinned {
                worker,
                pinned_until_ms,
            } if worker != *expected_worker && pinned_until_ms > now_ms => Some(pinned_until_ms),
            Placement::Portable | Placement::Pinned { .. } => None,
        });
        Ok(lease_until.into_iter().chain(pin_until).max())
    }

    fn durable_owner_until_sync(
        &self,
        key: &ExecutionKey,
        now_ms: i64,
    ) -> StoreResult<Option<i64>> {
        // The absence check belongs under the same cross-process gate as lease
        // and snapshot reads. Reading key.json first allowed a worker to seed
        // immediately afterwards and still receive a stale `None`, which wake
        // recovery interpreted as permission to demote its live generation.
        let _lock = self.lock_execution(key)?;
        self.ensure_directory_is_not_another_scopes(key)?;
        let lease_until = self
            .blocking_lease(key, now_ms)?
            .map(|lease| lease.expires_at_ms);
        let revision = self.latest_revision(key)?;
        let placement = if revision == Revision::INITIAL {
            None
        } else {
            self.ensure_key_record_recheck(key, &self.key_path(key))?;
            match self.current_placement_index(key, revision) {
                Some(placement) => Some(placement),
                None => {
                    let committed = self.load_sync(key)?.ok_or_else(|| StoreError::Corrupt {
                        key: key.clone(),
                        detail: format!(
                            "latest revision is {revision}, but its committed snapshot is absent"
                        ),
                    })?;
                    if committed.revision != revision {
                        return Err(StoreError::Conflict {
                            expected: revision,
                            found: committed.revision,
                        });
                    }
                    if let Err(error) = self.persist_placement_index(
                        key,
                        committed.revision,
                        &committed.state.placement,
                    ) {
                        warn!(
                            execution = %key,
                            %error,
                            "[LOOP-STORE] durable owner lookup could not repair the derived placement index"
                        );
                    }
                    Some(committed.state.placement)
                },
            }
        };
        let pin_until = placement.and_then(|placement| match placement {
            Placement::Pinned {
                pinned_until_ms, ..
            } if pinned_until_ms > now_ms => Some(pinned_until_ms),
            Placement::Portable | Placement::Pinned { .. } => None,
        });
        Ok(lease_until.into_iter().chain(pin_until).max())
    }

    fn execution_dir(&self, key: &ExecutionKey) -> PathBuf {
        self.workspace
            .runtime_root(key.principal(), key.workspace())
            .join("executions")
            .join(key.execution_id())
    }

    fn base_bindings_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.workspace
            .runtime_root(principal, workspace)
            .join("base_execution_bindings_v1")
    }

    fn legacy_writer_cutover_path(&self, principal: &str, workspace: &str) -> PathBuf {
        self.base_bindings_root(principal, workspace)
            .join("legacy-writers-retired.json")
    }

    fn base_binding_dir(
        &self,
        principal: &str,
        workspace: &str,
        base_execution_id: &str,
    ) -> PathBuf {
        self.base_bindings_root(principal, workspace).join(
            blake3::hash(base_execution_id.as_bytes())
                .to_hex()
                .to_string(),
        )
    }

    fn base_binding_record_path(&self, record: &BaseSegmentBindingRecord) -> PathBuf {
        self.base_binding_dir(
            &record.principal,
            &record.workspace,
            &record.base_execution_id,
        )
        .join(format!(
            "{}.json",
            blake3::hash(record.exact_segment_id.as_bytes()).to_hex()
        ))
    }

    fn base_binding_head_path(
        &self,
        principal: &str,
        workspace: &str,
        base_execution_id: &str,
    ) -> PathBuf {
        self.base_binding_dir(principal, workspace, base_execution_id)
            .join("members.head")
    }

    fn segment_recovery_index_path(&self, key: &ExecutionKey) -> PathBuf {
        self.execution_dir(key).join("base-recovery.index.json")
    }

    fn read_base_binding_head_sync(
        &self,
        principal: &str,
        workspace: &str,
        base_execution_id: &str,
    ) -> StoreResult<Option<BaseBindingHead>> {
        let path = self.base_binding_head_path(principal, workspace, base_execution_id);
        let head: Option<BaseBindingHead> = read_bounded_json(
            &path,
            MAX_BASE_BINDING_HEAD_BYTES,
            MAX_BASE_BINDING_HEAD_JSON_NODES,
        )
        .map_err(|error| StoreError::Unavailable {
            detail: format!("{}: {error}", path.display()),
        })?;
        let Some(head) = head else { return Ok(None) };
        let mut canonical = head.exact_segments.clone();
        canonical.sort();
        canonical.dedup();
        let valid = head.schema_version == BASE_BINDING_HEAD_SCHEMA_VERSION
            && head.principal == principal
            && head.workspace == workspace
            && head.base_execution_id == base_execution_id
            && canonical == head.exact_segments
            && head.exact_segments.len() <= MAX_EXECUTIONS_SCANNED
            && valid_journal_digest(&head.integrity_sha256)
            && head.integrity_sha256 == base_binding_head_integrity(&head);
        if !valid {
            return Err(StoreError::Unavailable {
                detail: format!("{} is not a canonical base binding head", path.display()),
            });
        }
        Ok(Some(head))
    }

    fn persist_base_binding_head_locked(&self, head: &BaseBindingHead) -> StoreResult<()> {
        let path =
            self.base_binding_head_path(&head.principal, &head.workspace, &head.base_execution_id);
        let encoded = serde_json::to_vec(head).map_err(|error| StoreError::Unavailable {
            detail: format!("base binding head encode failed: {error}"),
        })?;
        validate_json_bytes(
            &encoded,
            MAX_BASE_BINDING_HEAD_BYTES,
            MAX_BASE_BINDING_HEAD_JSON_NODES,
        )
        .map_err(|error| StoreError::Unavailable {
            detail: format!("base binding head validation failed: {error}"),
        })?;
        overwrite_derived_atomically(&path, &encoded).map_err(|error| StoreError::Unavailable {
            detail: format!("base binding head publication failed: {error}"),
        })
    }

    fn ensure_base_binding_head_member_locked(
        &self,
        record: &BaseSegmentBindingRecord,
    ) -> StoreResult<()> {
        let mut head = self
            .read_base_binding_head_sync(
                &record.principal,
                &record.workspace,
                &record.base_execution_id,
            )?
            .unwrap_or_else(|| BaseBindingHead {
                schema_version: BASE_BINDING_HEAD_SCHEMA_VERSION,
                principal: record.principal.clone(),
                workspace: record.workspace.clone(),
                base_execution_id: record.base_execution_id.clone(),
                exact_segments: Vec::new(),
                integrity_sha256: String::new(),
            });
        if !head
            .exact_segments
            .iter()
            .any(|segment| segment == &record.exact_segment_id)
        {
            if head.exact_segments.len() >= MAX_EXECUTIONS_SCANNED {
                return Err(StoreError::Unavailable {
                    detail: format!(
                        "base execution {} exceeded {MAX_EXECUTIONS_SCANNED} exact segments",
                        record.base_execution_id
                    ),
                });
            }
            head.exact_segments.push(record.exact_segment_id.clone());
            head.exact_segments.sort();
        }
        head.integrity_sha256 = base_binding_head_integrity(&head);
        self.persist_base_binding_head_locked(&head)
    }

    fn remove_base_binding_head_member_locked(
        &self,
        record: &BaseSegmentBindingRecord,
    ) -> StoreResult<()> {
        let mut head = self
            .read_base_binding_head_sync(
                &record.principal,
                &record.workspace,
                &record.base_execution_id,
            )?
            .ok_or_else(|| StoreError::Unavailable {
                detail: "base binding member cannot be retired without its head".to_owned(),
            })?;
        head.exact_segments
            .retain(|segment| segment != &record.exact_segment_id);
        head.integrity_sha256 = base_binding_head_integrity(&head);
        self.persist_base_binding_head_locked(&head)
    }

    /// Repair only crash-shaped disagreement between the mutable member head
    /// and immutable member records. Every repair is proved against the exact
    /// snapshot; committed ambiguity remains fail-closed.
    fn reconcile_base_binding_head_locked(
        &self,
        principal: &str,
        workspace: &str,
        base_execution_id: &str,
    ) -> StoreResult<()> {
        let dir = self.base_binding_dir(principal, workspace, base_execution_id);
        let mut head = self
            .read_base_binding_head_sync(principal, workspace, base_execution_id)?
            .unwrap_or_else(|| BaseBindingHead {
                schema_version: BASE_BINDING_HEAD_SCHEMA_VERSION,
                principal: principal.to_owned(),
                workspace: workspace.to_owned(),
                base_execution_id: base_execution_id.to_owned(),
                exact_segments: Vec::new(),
                integrity_sha256: String::new(),
            });
        let mut changed = false;
        for segment in head.exact_segments.clone() {
            let key =
                ExecutionKey::new(principal.to_owned(), workspace.to_owned(), segment.clone())?;
            let record = match self.load_sync(&key)? {
                Some(committed) => self.base_binding_record_for_state(&key, &committed.state)?,
                None => None,
            };
            let expected = record
                .as_ref()
                .map(|record| self.base_binding_record_path(record));
            if expected.as_ref().is_some_and(|path| path.exists()) {
                continue;
            }
            match record {
                Some(record) if record.base_execution_id == base_execution_id => {
                    self.publish_base_binding_record_locked(&record)?;
                },
                Some(_) => {
                    return Err(StoreError::Unavailable {
                        detail: format!(
                            "head-only member {segment} belongs to a different base execution"
                        ),
                    });
                },
                None => {
                    head.exact_segments
                        .retain(|candidate| candidate != &segment);
                    changed = true;
                },
            }
        }
        let entries = std::fs::read_dir(&dir).map_err(unavailable)?;
        for entry in entries {
            let path = entry.map_err(unavailable)?.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let record: BaseSegmentBindingRecord =
                read_bounded_json(&path, MAX_BASE_BINDING_BYTES, MAX_BASE_BINDING_JSON_NODES)
                    .map_err(|error| StoreError::Unavailable {
                        detail: format!("{}: {error}", path.display()),
                    })?
                    .ok_or_else(|| StoreError::Unavailable {
                        detail: format!(
                            "{} disappeared during head reconciliation",
                            path.display()
                        ),
                    })?;
            self.validate_base_binding_record(
                &record,
                principal,
                workspace,
                base_execution_id,
                &path,
            )?;
            if head.exact_segments.contains(&record.exact_segment_id) {
                continue;
            }
            let key = ExecutionKey::new(
                principal.to_owned(),
                workspace.to_owned(),
                record.exact_segment_id.clone(),
            )?;
            match self.load_sync(&key)? {
                Some(committed)
                    if self.base_binding_record_for_state(&key, &committed.state)?
                        == Some(record.clone()) =>
                {
                    head.exact_segments.push(record.exact_segment_id);
                    changed = true;
                },
                Some(_) => {
                    return Err(StoreError::Corrupt {
                        key,
                        detail: "record-only base binding does not match its committed snapshot"
                            .to_owned(),
                    });
                },
                None => {
                    std::fs::remove_file(&path).map_err(unavailable)?;
                    sync_parent_directory(&path).map_err(unavailable)?;
                },
            }
        }
        if changed
            || !self
                .base_binding_head_path(principal, workspace, base_execution_id)
                .exists()
        {
            head.exact_segments.sort();
            head.exact_segments.dedup();
            head.integrity_sha256 = base_binding_head_integrity(&head);
            self.persist_base_binding_head_locked(&head)?;
        }
        Ok(())
    }

    fn base_binding_record_for_state(
        &self,
        key: &ExecutionKey,
        state: &LoopState,
    ) -> StoreResult<Option<BaseSegmentBindingRecord>> {
        let (base_execution_id, explicit_binding) = match state.segment_binding.as_ref() {
            Some(binding) => {
                if binding.exact_segment_id != key.execution_id() {
                    return Err(StoreError::Corrupt {
                        key: key.clone(),
                        detail: "loop segment binding does not name its exact store key".to_owned(),
                    });
                }
                (binding.base_execution_id.clone(), true)
            },
            None => match state.identity.execution_id.as_deref() {
                Some(base) if !base.trim().is_empty() => (base.to_owned(), false),
                _ => return Ok(None),
            },
        };
        let mut record = BaseSegmentBindingRecord {
            schema_version: BASE_BINDING_SCHEMA_VERSION,
            principal: key.principal().to_owned(),
            workspace: key.workspace().to_owned(),
            base_execution_id,
            exact_segment_id: key.execution_id().to_owned(),
            explicit_binding,
            integrity_sha256: String::new(),
        };
        record.integrity_sha256 = base_binding_integrity(&record);
        Ok(Some(record))
    }

    fn validate_base_binding_record(
        &self,
        record: &BaseSegmentBindingRecord,
        principal: &str,
        workspace: &str,
        base_execution_id: &str,
        path: &Path,
    ) -> StoreResult<()> {
        let expected_name = format!(
            "{}.json",
            blake3::hash(record.exact_segment_id.as_bytes()).to_hex()
        );
        let valid = record.schema_version == BASE_BINDING_SCHEMA_VERSION
            && record.principal == principal
            && record.workspace == workspace
            && record.base_execution_id == base_execution_id
            && !record.exact_segment_id.trim().is_empty()
            && valid_journal_digest(&record.integrity_sha256)
            && record.integrity_sha256 == base_binding_integrity(record)
            && path.file_name().and_then(|name| name.to_str()) == Some(expected_name.as_str());
        if valid {
            Ok(())
        } else {
            Err(StoreError::Unavailable {
                detail: format!(
                    "{} is not an exact base execution binding record",
                    path.display()
                ),
            })
        }
    }

    fn publish_base_binding_record_locked(
        &self,
        record: &BaseSegmentBindingRecord,
    ) -> StoreResult<()> {
        // The authoritative member head moves first while this base lock is
        // held. A crash before the immutable record/snapshot is repairable as
        // an exact pre-snapshot orphan; publishing the snapshot before its
        // membership proof would create an undetectable omission.
        self.ensure_base_binding_head_member_locked(record)?;
        let path = self.base_binding_record_path(record);
        let encoded = serde_json::to_vec(record).map_err(|error| StoreError::Unavailable {
            detail: format!("base execution binding encode failed: {error}"),
        })?;
        validate_json_bytes(
            &encoded,
            MAX_BASE_BINDING_BYTES,
            MAX_BASE_BINDING_JSON_NODES,
        )
        .map_err(|error| StoreError::Unavailable {
            detail: format!("base execution binding validation failed: {error}"),
        })?;
        match publish_new_file(&path, &encoded) {
            Ok(Publish::Published) => Ok(()),
            Ok(Publish::AlreadyExists) => {
                let existing: Option<BaseSegmentBindingRecord> =
                    read_bounded_json(&path, MAX_BASE_BINDING_BYTES, MAX_BASE_BINDING_JSON_NODES)
                        .map_err(|error| StoreError::Unavailable {
                        detail: format!("base execution binding reread failed: {error}"),
                    })?;
                if existing.as_ref() == Some(record) {
                    Ok(())
                } else {
                    Err(StoreError::Unavailable {
                        detail: format!(
                            "{} was replaced by a conflicting base execution binding",
                            path.display()
                        ),
                    })
                }
            },
            Err(error) => Err(StoreError::Unavailable {
                detail: format!("base execution binding publication failed: {error}"),
            }),
        }
    }

    fn read_base_binding_records_sync(
        &self,
        principal: &str,
        workspace: &str,
        base_execution_id: &str,
    ) -> StoreResult<Vec<BaseSegmentBindingRecord>> {
        let dir = self.base_binding_dir(principal, workspace, base_execution_id);
        let head = self
            .read_base_binding_head_sync(principal, workspace, base_execution_id)?
            .ok_or_else(|| StoreError::Unavailable {
                detail: format!(
                    "{} exists without its authoritative member head",
                    dir.display()
                ),
            })?;
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(StoreError::Unavailable {
                    detail: format!("{}: {error}", dir.display()),
                })
            },
        };
        let mut records = Vec::new();
        for entry in entries {
            let entry = entry.map_err(unavailable)?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            if records.len() >= MAX_EXECUTIONS_SCANNED {
                return Err(StoreError::Unavailable {
                    detail: format!(
                        "base execution binding catalog exceeded {MAX_EXECUTIONS_SCANNED} segments"
                    ),
                });
            }
            let record: BaseSegmentBindingRecord =
                read_bounded_json(&path, MAX_BASE_BINDING_BYTES, MAX_BASE_BINDING_JSON_NODES)
                    .map_err(|error| StoreError::Unavailable {
                        detail: format!("{}: {error}", path.display()),
                    })?
                    .ok_or_else(|| StoreError::Unavailable {
                        detail: format!("{} disappeared during catalog read", path.display()),
                    })?;
            self.validate_base_binding_record(
                &record,
                principal,
                workspace,
                base_execution_id,
                &path,
            )?;
            records.push(record);
        }
        records.sort_by(|left, right| left.exact_segment_id.cmp(&right.exact_segment_id));
        let members = records
            .iter()
            .map(|record| record.exact_segment_id.clone())
            .collect::<Vec<_>>();
        if members != head.exact_segments {
            return Err(StoreError::Unavailable {
                detail: format!(
                    "{} does not exactly match its authoritative member head",
                    dir.display()
                ),
            });
        }
        Ok(records)
    }

    fn ensure_base_binding_catalog_sync(
        &self,
        principal: &str,
        workspace: &str,
    ) -> StoreResult<()> {
        let root = self.base_bindings_root(principal, workspace);
        let marker_path = root.join("catalog.complete.json");
        if read_base_binding_catalog_marker(&marker_path)?.is_some_and(|marker| marker.complete) {
            return Ok(());
        }
        let _migration =
            acquire_private_catalog_lock(&root.join("migration.lock")).map_err(|error| {
                StoreError::Unavailable {
                    detail: format!("base execution catalog migration lock failed: {error}"),
                }
            })?;
        let existing_marker = read_base_binding_catalog_marker(&marker_path)?;
        if existing_marker
            .as_ref()
            .is_some_and(|marker| marker.complete)
        {
            return Ok(());
        }
        let executions_root = self
            .workspace
            .runtime_root(principal, workspace)
            .join("executions");
        let mut marker = existing_marker.unwrap_or_else(base_binding_catalog_marker_initial);
        let mut after = marker.after_execution_dir.clone();
        let mut visited = 0usize;
        loop {
            let page = ordered_child_dirs(&executions_root, after.as_deref(), false)?;
            if page.unclassified {
                return Err(StoreError::Unavailable {
                    detail: "base execution catalog migration could not classify the complete execution directory"
                        .to_owned(),
                });
            }
            let remaining = BASE_BINDING_MIGRATION_PAGE_VISITS.saturating_sub(visited);
            let visit_count = page.names.len().min(remaining);
            for name in page.names.iter().take(visit_count) {
                visited = visited.saturating_add(1);
                let Some(key) = self.key_at(&executions_root.join(name))? else {
                    continue;
                };
                let Some(committed) = self.load_sync(&key)? else {
                    continue;
                };
                let Some(record) = self.base_binding_record_for_state(&key, &committed.state)?
                else {
                    continue;
                };
                let binding_dir =
                    self.base_binding_dir(principal, workspace, &record.base_execution_id);
                let _base = acquire_private_catalog_lock(&binding_dir.join("catalog.lock"))
                    .map_err(|error| StoreError::Unavailable {
                        detail: format!("base execution migration catalog lock failed: {error}"),
                    })?;
                reap_leaked_temporaries(&binding_dir);
                self.publish_base_binding_record_locked(&record)?;
            }
            after = page
                .names
                .get(visit_count.saturating_sub(1))
                .cloned()
                .or(after);
            let page_has_more = visit_count < page.names.len() || page.truncated;
            if !page_has_more {
                marker.complete = true;
                marker.after_execution_dir = after;
                marker.covered_execution_dirs =
                    marker.covered_execution_dirs.saturating_add(visited as u64);
                break;
            }
            if after.is_none() {
                return Err(StoreError::Unavailable {
                    detail: "base execution catalog migration lost its scan cursor".to_owned(),
                });
            }
            if visited >= BASE_BINDING_MIGRATION_PAGE_VISITS {
                marker.after_execution_dir = after;
                marker.covered_execution_dirs =
                    marker.covered_execution_dirs.saturating_add(visited as u64);
                let encoded =
                    serde_json::to_vec(&marker).map_err(|error| StoreError::Unavailable {
                        detail: format!("base execution catalog cursor encode failed: {error}"),
                    })?;
                overwrite_derived_atomically(&marker_path, &encoded).map_err(|error| {
                    StoreError::Unavailable {
                        detail: format!(
                            "base execution catalog cursor publication failed: {error}"
                        ),
                    }
                })?;
                return Err(StoreError::Unavailable {
                    detail: "base execution catalog migration advanced one bounded page; recovery remains fail-closed until coverage completes".to_owned(),
                });
            }
        }
        let encoded = serde_json::to_vec(&marker).map_err(|error| StoreError::Unavailable {
            detail: format!("base execution catalog marker encode failed: {error}"),
        })?;
        overwrite_derived_atomically(&marker_path, &encoded).map_err(|error| {
            StoreError::Unavailable {
                detail: format!("base execution catalog marker publication failed: {error}"),
            }
        })
    }

    /// One bounded positive-discovery pass for a set of runtime bases. This can
    /// repair records omitted by old writers, but it is never absence proof:
    /// those writers do not share the catalog lock and can publish behind the
    /// cursor. Authority-granting results stay `Uncertain` until the immutable
    /// deployment cutover exists.
    fn repair_scope_base_bindings_from_compat_scan_sync(
        &self,
        principal: &str,
        workspace: &str,
        requested: &BTreeSet<String>,
    ) -> StoreResult<()> {
        let executions_root = self
            .workspace
            .runtime_root(principal, workspace)
            .join("executions");
        let mut after = None::<String>;
        let mut visited = 0usize;
        loop {
            let page = ordered_child_dirs(&executions_root, after.as_deref(), false)?;
            if page.unclassified {
                return Err(StoreError::Unavailable {
                    detail: "mixed-writer compatibility scan could not classify the complete execution directory"
                        .to_owned(),
                });
            }
            for name in &page.names {
                visited = visited.saturating_add(1);
                if visited > MAX_EXECUTIONS_SCANNED {
                    return Err(StoreError::Unavailable {
                        detail: format!(
                            "mixed-writer compatibility scan exceeded {MAX_EXECUTIONS_SCANNED} segments"
                        ),
                    });
                }
                let Some(key) = self.key_at(&executions_root.join(name))? else {
                    continue;
                };
                let Some(committed) = self.load_sync(&key)? else {
                    continue;
                };
                let Some(record) = self.base_binding_record_for_state(&key, &committed.state)?
                else {
                    continue;
                };
                if !requested.contains(&record.base_execution_id) {
                    continue;
                }
                let binding_dir =
                    self.base_binding_dir(principal, workspace, &record.base_execution_id);
                let _base = acquire_private_catalog_lock(&binding_dir.join("catalog.lock"))
                    .map_err(|error| StoreError::Unavailable {
                        detail: format!("mixed-writer compatibility catalog lock failed: {error}"),
                    })?;
                reap_leaked_temporaries(&binding_dir);
                self.publish_base_binding_record_locked(&record)?;
            }
            if !page.truncated {
                return Ok(());
            }
            after = page.names.last().cloned();
            if after.is_none() {
                return Err(StoreError::Unavailable {
                    detail: "mixed-writer compatibility scan lost its cursor".to_owned(),
                });
            }
        }
    }

    fn lock_execution(&self, key: &ExecutionKey) -> StoreResult<File> {
        create_dir_all_durably(&self.execution_dir(key)).map_err(|error| io_error(key, error))?;
        self.ensure_key_record(key)?;
        let path = self.execution_dir(key).join("mutation.lock");
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|error| io_error(key, error))?;
        file.lock_exclusive()
            .map_err(|error| io_error(key, error))?;
        Ok(file)
    }

    fn validate_current_lease(&self, key: &ExecutionKey, lease: &Lease) -> StoreResult<()> {
        if &lease.key != key {
            return Err(StoreError::LeaseLost { fence: lease.fence });
        }
        let current = self
            .latest_lease(key)?
            .ok_or(StoreError::LeaseLost { fence: lease.fence })?;
        if current.released
            || current.fence != lease.fence
            || current.worker != lease.worker
            || current.expires_at_ms <= Utc::now().timestamp_millis()
        {
            return Err(StoreError::LeaseLost { fence: lease.fence });
        }
        Ok(())
    }

    fn journal_path(&self, key: &ExecutionKey) -> PathBuf {
        self.execution_dir(key).join("journal.jsonl")
    }

    fn journal_index_path(&self, key: &ExecutionKey) -> PathBuf {
        self.execution_dir(key).join("journal.index.json")
    }

    fn swept_journal_path(&self, key: &ExecutionKey) -> PathBuf {
        self.execution_dir(key).join("journal.swept.jsonl")
    }

    fn key_path(&self, key: &ExecutionKey) -> PathBuf {
        self.execution_dir(key).join("key.json")
    }

    /// Where this execution's outbox mark lives.
    ///
    /// Its own file, beside the journal it is a mark into, rather than a field on
    /// a snapshot: snapshots are revision-addressed and compare-and-swapped, and
    /// this value is neither. See
    /// [`LoopStateStore::save_projector_cursor`] for why it must not be a CAS.
    ///
    /// A fixed name, not a numbered one, because there is no history worth
    /// keeping: the previous mark is strictly behind this one and restoring it
    /// would only re-emit.
    fn projector_path(&self, key: &ExecutionKey) -> PathBuf {
        self.execution_dir(key).join("projector.json")
    }

    /// Where this segment's chain-closure receipt lives.
    ///
    /// Its own file beside the journal, for the reason [`Self::projector_path`]
    /// gives and one more: it is written **after** the commit that ended the
    /// segment, when there is no revision left to compare and swap against and
    /// no lease left to present. A fixed name because a receipt has no history
    /// — every writer of one for a given segment writes the same value.
    fn chain_closure_path(&self, key: &ExecutionKey) -> PathBuf {
        self.execution_dir(key).join("chain-closed.json")
    }

    fn terminal_debt_catalog_dir(&self) -> PathBuf {
        self.workspace
            .system_root()
            .join("run_loop")
            .join("terminal_outbox_debt_v1")
    }

    fn terminal_debt_catalog_path(&self) -> PathBuf {
        self.terminal_debt_catalog_dir().join("catalog.json")
    }

    fn terminal_debt_catalog_lock_path(&self) -> PathBuf {
        self.terminal_debt_catalog_dir().join("catalog.lock")
    }

    fn terminal_debt_catalog_dirty_path(&self) -> PathBuf {
        self.terminal_debt_catalog_dir().join("dirty")
    }

    fn terminal_debt_catalog_is_dirty_or_unreadable(&self) -> bool {
        self.terminal_debt_catalog_dirty_path()
            .try_exists()
            .unwrap_or(true)
    }

    fn terminal_debt_dirty_generation(&self) -> Option<String> {
        let file = File::open(self.terminal_debt_catalog_dirty_path()).ok()?;
        if file.metadata().ok()?.len() > 128 {
            return None;
        }
        let mut raw = String::new();
        file.take(129).read_to_string(&mut raw).ok()?;
        if raw.len() > 128 {
            return None;
        }
        let generation = raw.trim();
        (Uuid::parse_str(generation).is_ok()).then(|| generation.to_owned())
    }

    fn mark_terminal_debt_catalog_dirty(&self) -> StoreResult<()> {
        let _catalog = acquire_private_catalog_lock(&self.terminal_debt_catalog_lock_path())
            .map_err(|error| StoreError::Unavailable {
                detail: format!("terminal-debt dirty lock failed: {error}"),
            })?;
        reap_leaked_temporaries(&self.terminal_debt_catalog_dir());
        let generation = Uuid::new_v4().to_string();
        overwrite_atomically(
            &self.terminal_debt_catalog_dirty_path(),
            generation.as_bytes(),
        )
        .map_err(|error| StoreError::Unavailable {
            detail: format!("terminal-debt dirty publication failed: {error}"),
        })
    }

    fn clear_terminal_debt_catalog_dirty_if(&self, expected: Option<&str>) {
        let Ok(_catalog) = acquire_private_catalog_lock(&self.terminal_debt_catalog_lock_path())
        else {
            return;
        };
        if self.terminal_debt_dirty_generation().as_deref() != expected {
            return;
        }
        let path = self.terminal_debt_catalog_dirty_path();
        match std::fs::remove_file(&path) {
            Ok(()) => {
                let _ = sync_parent_directory(&path);
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(_) => {},
        }
    }

    fn terminal_debt_local_hint_path(&self, key: &ExecutionKey) -> PathBuf {
        self.execution_dir(key).join("terminal-debt.hint.json")
    }

    fn publish_terminal_debt_local_hint(
        &self,
        key: &ExecutionKey,
        observed_revision: Revision,
    ) -> StoreResult<()> {
        let hint = TerminalDebtLocalHint {
            schema_version: TERMINAL_DEBT_CATALOG_SCHEMA_VERSION,
            observed_revision,
            observed_at_ms: Utc::now().timestamp_millis(),
        };
        let bytes = serde_json::to_vec(&hint).map_err(|error| StoreError::Unavailable {
            detail: format!("terminal-debt local hint encode failed: {error}"),
        })?;
        overwrite_atomically(&self.terminal_debt_local_hint_path(key), &bytes).map_err(|error| {
            StoreError::Unavailable {
                detail: format!("terminal-debt local hint publication failed: {error}"),
            }
        })
    }

    fn retire_terminal_debt_local_hint_through(
        &self,
        key: &ExecutionKey,
        observed_revision: Revision,
    ) {
        let path = self.terminal_debt_local_hint_path(key);
        let hint: Option<TerminalDebtLocalHint> = match read_bounded_json(
            &path,
            MAX_TERMINAL_DEBT_HINT_BYTES,
            MAX_TERMINAL_DEBT_HINT_JSON_NODES,
        ) {
            Ok(hint) => hint,
            Err(_) => return,
        };
        if !hint.is_some_and(|hint| {
            hint.schema_version == TERMINAL_DEBT_CATALOG_SCHEMA_VERSION
                && hint.observed_at_ms > 0
                && hint.observed_revision <= observed_revision
        }) {
            return;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {
                let _ = sync_parent_directory(&path);
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(_) => {},
        }
    }

    fn retire_terminal_debt_local_hint_if_settled_or_stale(
        &self,
        key: &ExecutionKey,
        current_revision: Revision,
        now_ms: i64,
    ) {
        let path = self.terminal_debt_local_hint_path(key);
        let Ok(Some(hint)) = read_bounded_json::<TerminalDebtLocalHint>(
            &path,
            MAX_TERMINAL_DEBT_HINT_BYTES,
            MAX_TERMINAL_DEBT_HINT_JSON_NODES,
        ) else {
            return;
        };
        if hint.schema_version != TERMINAL_DEBT_CATALOG_SCHEMA_VERSION
            || hint.observed_at_ms <= 0
            || (hint.observed_revision > current_revision
                && now_ms.saturating_sub(hint.observed_at_ms)
                    < TERMINAL_DEBT_FUTURE_MARKER_GRACE_MS)
        {
            return;
        }
        // A concurrent terminal writer also publishes the global entry before
        // committing; deleting this local optimization after reading an older
        // stale generation cannot hide that writer. Its global publication (or
        // dirty generation on failure) remains the discovery contract.
        match std::fs::remove_file(&path) {
            Ok(()) => {
                let _ = sync_parent_directory(&path);
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(_) => {},
        }
    }

    fn load_terminal_debt_catalog(&self, now_ms: i64) -> StoreResult<Option<TerminalDebtCatalog>> {
        let path = self.terminal_debt_catalog_path();
        let catalog: Option<TerminalDebtCatalog> = read_bounded_json(
            &path,
            MAX_TERMINAL_DEBT_CATALOG_BYTES,
            MAX_TERMINAL_DEBT_CATALOG_JSON_NODES,
        )
        .map_err(|error| StoreError::Unavailable {
            detail: format!("{}: {error}", path.display()),
        })?;
        let Some(catalog) = catalog else {
            return Ok(None);
        };
        if !valid_terminal_debt_catalog(&catalog, now_ms) {
            return Err(StoreError::Unavailable {
                detail: format!("{} is not a valid terminal-debt catalog", path.display()),
            });
        }
        Ok(Some(catalog))
    }

    fn mutate_terminal_debt_catalog<F>(&self, mutation: F) -> StoreResult<()>
    where
        F: FnOnce(&mut TerminalDebtCatalog),
    {
        self.mutate_terminal_debt_catalog_with_durability(false, mutation)
    }

    fn mutate_terminal_debt_catalog_durably<F>(&self, mutation: F) -> StoreResult<()>
    where
        F: FnOnce(&mut TerminalDebtCatalog),
    {
        self.mutate_terminal_debt_catalog_with_durability(true, mutation)
    }

    fn mutate_terminal_debt_catalog_with_durability<F>(
        &self,
        durable: bool,
        mutation: F,
    ) -> StoreResult<()>
    where
        F: FnOnce(&mut TerminalDebtCatalog),
    {
        let now_ms = Utc::now().timestamp_millis();
        let _catalog = acquire_private_catalog_lock(&self.terminal_debt_catalog_lock_path())
            .map_err(|error| StoreError::Unavailable {
                detail: format!("terminal-debt catalog lock failed: {error}"),
            })?;
        reap_leaked_temporaries(&self.terminal_debt_catalog_dir());
        // A malformed derivative is replaced by an incomplete catalog. It can
        // never be repaired into `complete` by a writer; only the authoritative
        // walker below may set that bit after reaching the end of the tree.
        let mut catalog = self
            .load_terminal_debt_catalog(now_ms)
            .unwrap_or(None)
            .unwrap_or_else(terminal_debt_catalog_initial);
        mutation(&mut catalog);
        catalog.schema_version = TERMINAL_DEBT_CATALOG_SCHEMA_VERSION;
        catalog.integrity_sha256 = terminal_debt_catalog_integrity(&catalog);
        let bytes = serde_json::to_vec(&catalog).map_err(|error| StoreError::Unavailable {
            detail: format!("terminal-debt catalog encode failed: {error}"),
        })?;
        validate_json_bytes(
            &bytes,
            MAX_TERMINAL_DEBT_CATALOG_BYTES,
            MAX_TERMINAL_DEBT_CATALOG_JSON_NODES,
        )
        .map_err(|error| StoreError::Unavailable {
            detail: format!("terminal-debt catalog exceeded its read bounds: {error}"),
        })?;
        let publication = if durable {
            overwrite_atomically(&self.terminal_debt_catalog_path(), &bytes)
        } else {
            overwrite_derived_atomically(&self.terminal_debt_catalog_path(), &bytes)
        };
        publication.map_err(|error| StoreError::Unavailable {
            detail: format!("terminal-debt catalog publication failed: {error}"),
        })
    }

    fn publish_terminal_debt_candidate(
        &self,
        key: &ExecutionKey,
        observed_revision: Revision,
    ) -> StoreResult<()> {
        if let Err(error) = self.publish_terminal_debt_local_hint(key, observed_revision) {
            warn!(
                execution = %key,
                revision = %observed_revision,
                %error,
                "[LOOP_OUTBOX] terminal debt has no local cursor hint; the global catalog remains the discovery contract"
            );
        }
        let observed_at_ms = Utc::now().timestamp_millis();
        let entry = TerminalDebtCatalogEntry {
            principal: key.principal().to_owned(),
            workspace: key.workspace().to_owned(),
            execution_id: key.execution_id().to_owned(),
            observed_revision,
            observed_at_ms,
        };
        let digest = terminal_debt_entry_digest(&entry);
        let publication = self.mutate_terminal_debt_catalog_durably(move |catalog| {
            if catalog.entries.contains_key(&digest)
                || catalog.entries.len() < MAX_TERMINAL_DEBT_CATALOG_ENTRIES
            {
                catalog.entries.insert(digest, entry);
            } else {
                // Losing a candidate to the bound forbids priority-only scans.
                // The next cadence therefore continues authoritative paging.
                catalog.overflowed = true;
                catalog.complete = false;
            }
        });
        if let Err(error) = publication {
            // A previously complete catalog must not stay usable after it
            // failed to record a new candidate. The dirty generation forces
            // the very next cadence onto authoritative paging.
            self.mark_terminal_debt_catalog_dirty().map_err(|dirty_error| {
                StoreError::Unavailable {
                    detail: format!(
                        "terminal-debt candidate publication failed ({error}); its authoritative-fallback invalidation also failed ({dirty_error})"
                    ),
                }
            })?;
            warn!(
                execution = %key,
                revision = %observed_revision,
                %error,
                "[LOOP_OUTBOX] terminal-debt candidate publication failed; a durable dirty generation forces authoritative discovery"
            );
        }
        Ok(())
    }

    fn remove_terminal_debt_candidate_if_unchanged(
        &self,
        digest: &str,
        observed: &TerminalDebtCatalogEntry,
    ) -> StoreResult<()> {
        let digest = digest.to_owned();
        let observed = observed.clone();
        let mut removed = false;
        self.mutate_terminal_debt_catalog(|catalog| {
            if catalog.entries.get(&digest) == Some(&observed) {
                catalog.entries.remove(&digest);
                removed = true;
            }
        })?;
        if removed {
            let observed_revision = observed.observed_revision;
            let key = ExecutionKey::new(
                observed.principal,
                observed.workspace,
                observed.execution_id,
            )
            .map_err(|error| StoreError::Unavailable {
                detail: format!("terminal-debt local hint key is invalid: {error}"),
            })?;
            self.retire_terminal_debt_local_hint_through(&key, observed_revision);
        }
        Ok(())
    }

    fn refresh_terminal_debt_candidate(
        &self,
        key: &ExecutionKey,
        observed_revision: Revision,
    ) -> StoreResult<()> {
        match self.terminal_outbox_debt_at(&self.execution_dir(key))? {
            Some(_) => self.publish_terminal_debt_candidate(key, observed_revision),
            None => {
                let entry = TerminalDebtCatalogEntry {
                    principal: key.principal().to_owned(),
                    workspace: key.workspace().to_owned(),
                    execution_id: key.execution_id().to_owned(),
                    observed_revision,
                    observed_at_ms: 0,
                };
                let digest = terminal_debt_entry_digest(&entry);
                self.mutate_terminal_debt_catalog(|catalog| {
                    if catalog
                        .entries
                        .get(&digest)
                        .is_some_and(|current| current.observed_revision <= observed_revision)
                    {
                        catalog.entries.remove(&digest);
                    }
                })?;
                // Exact current state proves there is no debt through this
                // revision even when the best-effort global publication never
                // landed. A concurrently newer local hint is revision-guarded
                // and survives.
                self.retire_terminal_debt_local_hint_through(key, observed_revision);
                Ok(())
            },
        }
    }

    /// Where this execution's published ending lives.
    ///
    /// This remains one fixed, bounded file on the scan path, but its contents
    /// are revision-addressed. A commit atomically overwrites it with at most
    /// two bindings (current and proposed) before the snapshot CAS. Readers
    /// select only the binding matching the snapshot they loaded. Thus neither
    /// publishing a future ending nor publishing a future tombstone can erase
    /// the still-authoritative prior ending during the crash window.
    fn ended_path(&self, key: &ExecutionKey) -> PathBuf {
        self.execution_dir(key).join("ended.json")
    }

    fn placement_index_path(&self, key: &ExecutionKey) -> PathBuf {
        self.execution_dir(key).join("placement.index.json")
    }

    fn snapshot_stamp(
        &self,
        key: &ExecutionKey,
        revision: Revision,
    ) -> StoreResult<SnapshotFileStamp> {
        let metadata = std::fs::metadata(self.snapshot_path(key, revision))
            .map_err(|error| io_error(key, error))?;
        let modified = metadata
            .modified()
            .map_err(|error| io_error(key, error))?
            .duration_since(UNIX_EPOCH)
            .map_err(|error| StoreError::Corrupt {
                key: key.clone(),
                detail: format!("the snapshot modification time predates the Unix epoch: {error}"),
            })?;
        Ok(SnapshotFileStamp {
            bytes: metadata.len(),
            modified_secs: modified.as_secs(),
            modified_nanos: modified.subsec_nanos(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(unix)]
            changed_secs: metadata.ctime(),
            #[cfg(unix)]
            changed_nanos: metadata.ctime_nsec(),
        })
    }

    fn current_placement_index(&self, key: &ExecutionKey, revision: Revision) -> Option<Placement> {
        let index = match read_bounded_json::<PlacementIndex>(
            &self.placement_index_path(key),
            MAX_PLACEMENT_INDEX_BYTES,
            MAX_PLACEMENT_INDEX_JSON_NODES,
        ) {
            Ok(Some(index)) => index,
            Ok(None) => return None,
            Err(error) => {
                warn!(
                    execution = %key,
                    %error,
                    "[LOOP-STORE] ignoring an unreadable derived placement index"
                );
                return None;
            },
        };
        let structurally_valid = index.schema_version == PLACEMENT_INDEX_SCHEMA_VERSION
            && index.revision == revision
            && index.snapshot_stamp.bytes > 0
            && index.snapshot_stamp.bytes <= MAX_STATE_BYTES
            && valid_journal_digest(&index.integrity_sha256)
            && index.integrity_sha256 == placement_index_integrity(&index);
        if !structurally_valid {
            warn!(
                execution = %key,
                "[LOOP-STORE] ignoring a stale or structurally invalid derived placement index"
            );
            return None;
        }
        match self.snapshot_stamp(key, revision) {
            Ok(stamp) if stamp == index.snapshot_stamp => Some(index.placement),
            Ok(_) => None,
            Err(error) => {
                warn!(
                    execution = %key,
                    %error,
                    "[LOOP-STORE] could not fingerprint the placement index snapshot; falling back to the full state"
                );
                None
            },
        }
    }

    fn persist_placement_index(
        &self,
        key: &ExecutionKey,
        revision: Revision,
        placement: &Placement,
    ) -> StoreResult<()> {
        let mut index = PlacementIndex {
            schema_version: PLACEMENT_INDEX_SCHEMA_VERSION,
            revision,
            snapshot_stamp: self.snapshot_stamp(key, revision)?,
            placement: placement.clone(),
            integrity_sha256: String::new(),
        };
        index.integrity_sha256 = placement_index_integrity(&index);
        let encoded = serde_json::to_vec(&index).map_err(|error| StoreError::Corrupt {
            key: key.clone(),
            detail: format!("the derived placement index could not be encoded: {error}"),
        })?;
        validate_json_bytes(
            &encoded,
            MAX_PLACEMENT_INDEX_BYTES,
            MAX_PLACEMENT_INDEX_JSON_NODES,
        )
        .map_err(|error| corrupt(key, error))?;
        overwrite_derived_atomically(&self.placement_index_path(key), &encoded)
            .map_err(|error| io_error(key, error))
    }

    fn current_segment_recovery_index(
        &self,
        key: &ExecutionKey,
        revision: Revision,
        record: &BaseSegmentBindingRecord,
    ) -> Option<SegmentRecoveryIndex> {
        let index = match read_bounded_json::<SegmentRecoveryIndex>(
            &self.segment_recovery_index_path(key),
            MAX_BASE_BINDING_BYTES,
            MAX_BASE_BINDING_JSON_NODES,
        ) {
            Ok(Some(index)) => index,
            Ok(None) | Err(_) => return None,
        };
        let valid = index.schema_version == BASE_RECOVERY_INDEX_SCHEMA_VERSION
            && index.revision == revision
            && index.base_execution_id == record.base_execution_id
            && index.exact_segment_id == record.exact_segment_id
            && index.explicit_binding == record.explicit_binding
            && index.snapshot_stamp.bytes > 0
            && index.snapshot_stamp.bytes <= MAX_STATE_BYTES
            && valid_journal_digest(&index.integrity_sha256)
            && index.integrity_sha256 == segment_recovery_index_integrity(&index)
            && self
                .snapshot_stamp(key, revision)
                .is_ok_and(|stamp| stamp == index.snapshot_stamp);
        valid.then_some(index)
    }

    fn persist_segment_recovery_index(
        &self,
        key: &ExecutionKey,
        revision: Revision,
        state: &LoopState,
        non_resumable_ended: bool,
    ) -> StoreResult<()> {
        let Some(binding) = self.base_binding_record_for_state(key, state)? else {
            return Ok(());
        };
        let mut index = SegmentRecoveryIndex {
            schema_version: BASE_RECOVERY_INDEX_SCHEMA_VERSION,
            revision,
            snapshot_stamp: self.snapshot_stamp(key, revision)?,
            base_execution_id: binding.base_execution_id,
            exact_segment_id: binding.exact_segment_id,
            explicit_binding: binding.explicit_binding,
            has_continuation_checkpoint: state.continuation_checkpoint.is_some(),
            terminal_settlement_seq: state
                .terminal_settlement_receipt
                .as_ref()
                .map(|receipt| receipt.descriptor.terminal_seq),
            non_resumable_ended,
            placement: state.placement.clone(),
            integrity_sha256: String::new(),
        };
        index.integrity_sha256 = segment_recovery_index_integrity(&index);
        let encoded = serde_json::to_vec(&index).map_err(|error| StoreError::Unavailable {
            detail: format!("segment recovery index encode failed: {error}"),
        })?;
        validate_json_bytes(
            &encoded,
            MAX_BASE_BINDING_BYTES,
            MAX_BASE_BINDING_JSON_NODES,
        )
        .map_err(|error| StoreError::Unavailable {
            detail: format!("segment recovery index validation failed: {error}"),
        })?;
        overwrite_derived_atomically(&self.segment_recovery_index_path(key), &encoded)
            .map_err(|error| io_error(key, error))
    }

    /// Write the bounded revision bindings that make one snapshot transition
    /// crash-safe. Called under the per-key mutation lock before snapshot CAS.
    fn publish_ending_transition(
        &self,
        key: &ExecutionKey,
        bindings: Vec<RevisionEndingBinding>,
    ) -> StoreResult<()> {
        if bindings.is_empty() || bindings.len() > 2 {
            return Err(StoreError::Corrupt {
                key: key.clone(),
                detail: format!(
                    "an ending transition needs one or two revision bindings, got {}",
                    bindings.len()
                ),
            });
        }
        if bindings.len() == 2 && bindings[0].revision == bindings[1].revision {
            return Err(StoreError::Corrupt {
                key: key.clone(),
                detail: "an ending transition repeated one revision".to_string(),
            });
        }
        let marker = StoredEndingMarker::Revisioned(RevisionedEndingMarker {
            schema_version: ENDING_MARKER_SCHEMA_VERSION,
            bindings,
        });
        let bytes = serde_json::to_vec(&marker).map_err(|error| StoreError::Corrupt {
            key: key.clone(),
            detail: format!("the revisioned run ending could not be encoded: {error}"),
        })?;
        validate_json_bytes(&bytes, MAX_ENDED_BYTES, MAX_ENDED_JSON_NODES)
            .map_err(|error| corrupt(key, error))?;
        overwrite_atomically(&self.ended_path(key), &bytes).map_err(|error| io_error(key, error))
    }

    /// Test helper for the bare marker shape written by older builds.
    #[cfg(test)]
    fn publish_ending(&self, key: &ExecutionKey, ended: EndedRun) -> StoreResult<()> {
        let bytes = serde_json::to_vec(&ended).map_err(|error| StoreError::Corrupt {
            key: key.clone(),
            detail: format!("the run ending could not be encoded: {error}"),
        })?;
        validate_json_bytes(&bytes, MAX_ENDED_BYTES, MAX_ENDED_JSON_NODES)
            .map_err(|error| corrupt(key, error))?;
        overwrite_atomically(&self.ended_path(key), &bytes).map_err(|error| io_error(key, error))
    }

    /// Re-derive this execution's ending from what is committed **now**, and
    /// publish its exact revision binding (including a `None` tombstone).
    ///
    /// # Why a repair exists at all
    ///
    /// Current commits require a marker before publishing a terminal snapshot.
    /// This repair remains for snapshots produced by older builds, and for an
    /// operator who removed a marker after commit. Nothing else rewrites one: a
    /// run that has ended does not commit again.
    ///
    /// An old terminal snapshot with no marker is invisible to the dedicated
    /// terminal-debt scan. This repair cannot discover it globally, but it does
    /// close an exact-key operator/recovery claim and remains the migration path
    /// for those pre-change snapshots.
    ///
    /// # Why here, under the lease the caller just won
    ///
    /// It reads the committed state and the journal and must not race a commit
    /// that is changing both. The lease `claim` publishes immediately before
    /// this is exactly that exclusion — the same licence
    /// [`reap_leaked_temporaries`] is called under on the line above.
    ///
    /// # Best-effort, and it has to be
    ///
    /// The lease is already published when this runs, so returning an error
    /// would leak it. Every failure therefore warns and leaves the marker
    /// exactly as it found it, which is the behaviour this store had before the
    /// repair existed: a repair that cannot run is never worse than no repair.
    ///
    /// # What it costs, said plainly
    ///
    /// Current-format executions pay two bounded JSON reads: the committed
    /// snapshot and the tiny exact-revision marker. Only a legacy, absent or
    /// stale marker pays the whole-journal parse needed to migrate it. That
    /// distinction is load-bearing for long runs: claim is a phase hot path,
    /// while marker repair is a one-time compatibility path.
    fn repair_ending(&self, key: &ExecutionKey) {
        let committed = match self.load_sync(key) {
            Ok(Some(committed)) => committed,
            // No committed state means no watermark, and an ending is only ever
            // a fact about a prefix some watermark vouches for. There is nothing
            // here to repair, and the first commit will publish whatever is true
            // then.
            Ok(None) => return,
            Err(error) => {
                warn!(
                    %key,
                    %error,
                    "[LOOP-STORE] a claim could not read the committed state to check this \
                     run's ending marker, so the marker is left as it was"
                );
                return;
            },
        };

        // Every current producer publishes a bounded marker carrying the exact
        // committed revision before it publishes that revision's snapshot. It
        // is already the durable, cross-process certificate the runnable and
        // terminal-debt scans trust. Replaying the whole journal when that
        // certificate is present did no repair at all; it merely made every
        // successful claim O(journal length), and could replace a sound terminal
        // binding with `None` when the journal had subsequently become damaged.
        //
        // Only the migration shapes continue below: no marker, a legacy bare
        // ending, or a valid revisioned transition that does not cover the
        // currently committed snapshot (for example, a prepublished successor
        // whose snapshot never landed). Corruption/unavailability remains
        // fail-closed and best-effort, exactly like the journal read below.
        match self.ending_marker_covers_revision(key, committed.revision) {
            Ok(true) => return,
            Ok(false) => {},
            Err(error) => {
                warn!(
                    %key,
                    %error,
                    "[LOOP-STORE] a claim could not validate the current ending marker, so the \
                     marker is left as it was"
                );
                return;
            },
        }
        let journal = match self.read_journal_file(key) {
            Ok(journal) => journal,
            Err(error) => {
                warn!(
                    %key,
                    %error,
                    "[LOOP-STORE] a claim could not read the journal to check this run's \
                     ending marker, so the marker is left as it was"
                );
                return;
            },
        };
        // Deliberately the same expression `commit` evaluates, against the same
        // two inputs, so the marker a repair writes and the marker a commit
        // writes cannot come to differ. The explicit `None` binding is a
        // tombstone for this revision; it supersedes a legacy stale ending
        // without creating a pre-snapshot retraction window.
        let binding = RevisionEndingBinding {
            revision: committed.revision,
            ending: non_resumable_terminal(journal.authoritative(committed.state.journal_seq)),
        };
        if let Err(error) = self.publish_ending_transition(key, vec![binding]) {
            warn!(
                %key,
                %error,
                "[LOOP-STORE] a claim could not repair the revision-bound ending marker"
            );
        }
    }

    /// Whether a current-format ending marker already covers `revision`.
    ///
    /// This deliberately does not answer what the ending *is*. Claim repair
    /// needs only proof that a current producer already published the exact
    /// revision, including an explicit non-terminal tombstone. Absence and the
    /// legacy bare shape return `false` so the migration path can derive a
    /// revision-bound replacement from the journal.
    fn ending_marker_covers_revision(
        &self,
        key: &ExecutionKey,
        revision: Revision,
    ) -> StoreResult<bool> {
        let stored: Option<StoredEndingMarker> =
            read_bounded_json(&self.ended_path(key), MAX_ENDED_BYTES, MAX_ENDED_JSON_NODES)
                .map_err(|error| corrupt(key, error))?;
        let Some(StoredEndingMarker::Revisioned(marker)) = stored else {
            return Ok(false);
        };
        validate_revisioned_ending_marker(key, &marker)?;
        Ok(marker
            .bindings
            .iter()
            .any(|binding| binding.revision == revision))
    }

    /// The ending published for this execution, if one has been.
    ///
    /// # Private, and every caller must ALREADY own this directory
    ///
    /// It takes a key and joins it into a path, which is the shape
    /// `a_colliding_scope_is_refused_at_every_door_and_not_only_at_two`
    /// enumerates. It is not a door, because it is unreachable from outside:
    /// `commit` reaches it only after `ensure_key_record` and a won swap, and
    /// `runnable_at` only after [`Self::key_at`] recovered the key **from** the
    /// directory — so `execution_dir(key)` is the directory it was read out of,
    /// by construction — and after `load_sync` re-checked the record. Making it
    /// public would add the door.
    ///
    /// # Absence is `None`; unreadable is an ERROR
    ///
    /// The same distinction [`Self::wake_resolution_paths`] draws, and for the
    /// same reason: the two mean opposite things. Absent is *this run has not
    /// ended*; unreadable is *this store cannot say*. Answering `None` to the
    /// second would offer a finished run — recoverable — but it would do so on
    /// the strength of a read that failed, and the caller quarantines an
    /// unreadable execution from scheduling loudly instead.
    fn ended_run(&self, key: &ExecutionKey, revision: Revision) -> StoreResult<Option<EndedRun>> {
        let stored: Option<StoredEndingMarker> =
            read_bounded_json(&self.ended_path(key), MAX_ENDED_BYTES, MAX_ENDED_JSON_NODES)
                .map_err(|error| corrupt(key, error))?;
        let Some(stored) = stored else {
            return Ok(None);
        };
        match stored {
            StoredEndingMarker::Legacy(ended) => Ok(Some(ended)),
            StoredEndingMarker::Revisioned(marker) => {
                validate_revisioned_ending_marker(key, &marker)?;
                let mut matching = marker
                    .bindings
                    .into_iter()
                    .filter(|binding| binding.revision == revision);
                let Some(binding) = matching.next() else {
                    return Err(StoreError::Corrupt {
                        key: key.clone(),
                        detail: format!(
                            "the ending marker has no binding for committed {revision}"
                        ),
                    });
                };
                debug_assert!(matching.next().is_none());
                Ok(binding.ending)
            },
        }
    }

    fn effects_dir(&self, key: &ExecutionKey) -> PathBuf {
        self.execution_dir(key).join("effects")
    }

    fn effect_path(&self, key: &ExecutionKey, effect_id: &EffectId) -> PathBuf {
        self.effects_dir(key)
            .join(format!("{}{JSON_SUFFIX}", effect_id.local_attempt_key()))
    }

    /// The directory holding one token's outstanding resolutions.
    ///
    /// Digested rather than used raw: a wake token embeds job ids and child
    /// execution ids joined by separators, none of which is guaranteed to be a
    /// safe path component. The digest is not a secret — each resolution file's
    /// content carries the raw token and the raw resolution id, for whoever has
    /// to debug a park that never woke.
    ///
    /// A directory rather than a file because a token can hold more than one
    /// outstanding resolution: a child that reports twice while its parent is
    /// busy is two things to wake for, and a single marker would collapse them
    /// into one and lose the second.
    fn wake_dir(&self, key: &ExecutionKey, wake_token: &str) -> PathBuf {
        self.execution_dir(key)
            .join("wake")
            .join(blake3::hash(wake_token.as_bytes()).to_hex().to_string())
    }

    /// Serialize all mutations of one wake token, including capacity admission.
    ///
    /// Resolution writers do not hold the execution lease, so the execution
    /// mutation lock cannot protect this directory.  A token-local lock keeps
    /// the count-and-publish sequence atomic without coupling independent jobs
    /// or children, and gives readers a safe point at which to reap a writer's
    /// abandoned staging files.
    fn lock_wake_token(&self, key: &ExecutionKey, wake_token: &str) -> StoreResult<File> {
        let dir = self.wake_dir(key, wake_token);
        create_dir_all_durably(&dir).map_err(|error| io_error(key, error))?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(dir.join("mutation.lock"))
            .map_err(|error| io_error(key, error))?;
        file.lock_exclusive()
            .map_err(|error| io_error(key, error))?;
        Ok(file)
    }

    fn wake_resolution_path(
        &self,
        key: &ExecutionKey,
        wake_token: &str,
        resolution_id: &str,
    ) -> PathBuf {
        // Named by the resolution's digest, so publishing is idempotent on
        // `(token, resolution_id)` through `link(2)` alone — the completer that
        // reports the same completion twice takes the `AlreadyExists` arm and
        // never writes a second row.
        self.wake_dir(key, wake_token).join(format!(
            "{}{JSON_SUFFIX}",
            blake3::hash(resolution_id.as_bytes()).to_hex()
        ))
    }

    /// Every outstanding resolution file for one token, by path.
    ///
    /// Names only — no contents read. The scheduler scan asks whether a park is
    /// satisfied on every pass over every parked execution, and that question is
    /// answered by presence; parsing rows to answer it would put a JSON read per
    /// parked run inside `list_runnable`.
    fn wake_resolution_paths_unlocked(
        &self,
        key: &ExecutionKey,
        wake_token: &str,
    ) -> StoreResult<Vec<PathBuf>> {
        let dir = self.wake_dir(key, wake_token);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(io_error(key, error)),
        };
        let mut paths = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| io_error(key, error))?;
            let path = entry.path();
            // A half-written temporary is not a resolution. Its suffix is
            // `.tmp-<uuid>`, so the extension check that keeps it out of the
            // effects listing keeps it out of this one too.
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let regular = entry.file_type().map_err(|error| io_error(key, error))?;
            let valid_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(valid_wake_resolution_filename);
            if !regular.is_file() || !valid_name {
                return Err(StoreError::Corrupt {
                    key: key.clone(),
                    detail: format!(
                        "{} is not a regular wake-resolution row with a canonical digest name",
                        path.display()
                    ),
                });
            }
            if paths.len() >= MAX_WAKE_RESOLUTIONS {
                return Err(StoreError::WakeLedgerFull {
                    wake_token: wake_token.to_string(),
                    limit: MAX_WAKE_RESOLUTIONS,
                });
            }
            paths.push(path);
        }
        // Sorted so two handles onto one substrate answer in the same order; a
        // directory listing has none of its own.
        paths.sort();
        Ok(paths)
    }

    fn read_wake_resolution(
        &self,
        key: &ExecutionKey,
        wake_token: &str,
        path: &Path,
    ) -> StoreResult<Option<StoredWakeResolution>> {
        let record: Option<StoredWakeResolution> =
            read_bounded_json(path, MAX_WAKE_BYTES, MAX_WAKE_JSON_NODES)
                .map_err(|error| corrupt(key, error))?;
        let Some(record) = record else {
            return Ok(None);
        };
        if record.wake_token.as_str() != wake_token
            || self
                .wake_resolution_path(key, wake_token, &record.resolution_id)
                .as_path()
                != path
        {
            return Err(StoreError::Corrupt {
                key: key.clone(),
                detail: format!(
                    "{} does not bind its wake token and resolution id to its directory address",
                    path.display()
                ),
            });
        }
        Ok(Some(record))
    }

    fn wake_resolution_paths(
        &self,
        key: &ExecutionKey,
        wake_token: &str,
    ) -> StoreResult<Vec<PathBuf>> {
        // A read of an unresolved park must stay read-only. Creating and
        // fsyncing an empty token directory for every scheduler probe would
        // turn the wake check into both write amplification and durable litter.
        match std::fs::metadata(self.wake_dir(key, wake_token)) {
            Ok(metadata) if metadata.is_dir() => {},
            Ok(_) => {
                return Err(StoreError::Corrupt {
                    key: key.clone(),
                    detail: "the wake token path is not a directory".to_string(),
                })
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(io_error(key, error)),
        }
        let _lock = self.lock_wake_token(key, wake_token)?;
        // A publisher can die after fsyncing its temporary and before linking
        // it. Under this lock no live publisher can own such a file, so cleanup
        // cannot turn an in-flight completion into a lost wake.
        reap_leaked_temporaries(&self.wake_dir(key, wake_token));
        self.wake_resolution_paths_unlocked(key, wake_token)
    }

    /// Every outstanding resolution id for one token, in a stable order.
    fn read_wake_resolutions(
        &self,
        key: &ExecutionKey,
        wake_token: &str,
    ) -> StoreResult<Vec<String>> {
        let mut ids = Vec::new();
        for path in self.wake_resolution_paths(key, wake_token)? {
            let record = self.read_wake_resolution(key, wake_token, &path)?;
            // `None` is the file vanishing between the listing and the read — a
            // concurrent consume, not an empty row.
            if let Some(record) = record {
                ids.push(record.resolution_id);
            }
        }
        // The paths were sorted by digest, which is not the ids' own order.
        ids.sort();
        Ok(ids)
    }

    /// The highest published revision, and nothing about its content.
    fn latest_revision(&self, key: &ExecutionKey) -> StoreResult<Revision> {
        let dir = self.execution_dir(key);
        let highest = scan_numbered(&dir, SNAPSHOT_PREFIX).map_err(|error| io_error(key, error))?;
        Ok(Revision::from_u64(highest.last().copied().unwrap_or(0)))
    }

    fn snapshot_path(&self, key: &ExecutionKey, revision: Revision) -> PathBuf {
        self.execution_dir(key).join(format!(
            "{SNAPSHOT_PREFIX}{}{JSON_SUFFIX}",
            revision.as_u64()
        ))
    }

    /// Read the journal file, bounded, tolerating a torn tail.
    ///
    /// # A torn tail is not always valid text
    ///
    /// Records carry model and tool output, and `serde_json` emits non-ASCII as
    /// raw UTF-8 rather than as escapes, so an append interrupted part way
    /// through a multi-byte character leaves bytes that are not a string at all.
    /// Reading the file with `read_to_string` refused the **whole** journal in
    /// that case — every append and every commit for that execution, forever —
    /// over a record that was never acknowledged and that `Journal::parse` was
    /// already willing to drop. So the decode separates the two shapes the damage
    /// can take: input that ended mid-character is the torn tail, and an invalid
    /// sequence with bytes after it is damage inside an append-only file, which
    /// is corruption and is reported as such.
    fn read_journal_file(&self, key: &ExecutionKey) -> StoreResult<Journal> {
        Ok(Journal::parse(&self.read_journal_text(key)?)?)
    }

    /// The journal file as text, before it is parsed.
    ///
    /// Separate from [`Self::read_journal_file`] because an append has to see
    /// something the records do not carry: whether the file ends in a finished
    /// line.
    fn read_journal_text(&self, key: &ExecutionKey) -> StoreResult<String> {
        let path = self.journal_path(key);
        match std::fs::metadata(&path) {
            Ok(metadata) => {
                if metadata.len() > MAX_JOURNAL_BYTES {
                    return Err(StoreError::Journal(JournalError::FileTooLarge {
                        bytes: metadata.len(),
                        limit: MAX_JOURNAL_BYTES,
                    }));
                }
                let bytes = std::fs::read(&path).map_err(|error| io_error(key, error))?;
                decode_journal_text(key, bytes)
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(error) => Err(io_error(key, error)),
        }
    }

    fn journal_stamp(&self, key: &ExecutionKey) -> StoreResult<JournalFileStamp> {
        match std::fs::metadata(self.journal_path(key)) {
            Ok(metadata) => {
                let modified = metadata
                    .modified()
                    .map_err(|error| io_error(key, error))?
                    .duration_since(UNIX_EPOCH)
                    .map_err(|error| StoreError::Corrupt {
                        key: key.clone(),
                        detail: format!(
                            "the journal modification time predates the Unix epoch: {error}"
                        ),
                    })?;
                Ok(JournalFileStamp {
                    bytes: metadata.len(),
                    modified_secs: modified.as_secs(),
                    modified_nanos: modified.subsec_nanos(),
                    #[cfg(unix)]
                    device: metadata.dev(),
                    #[cfg(unix)]
                    inode: metadata.ino(),
                    #[cfg(unix)]
                    changed_secs: metadata.ctime(),
                    #[cfg(unix)]
                    changed_nanos: metadata.ctime_nsec(),
                })
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(JournalFileStamp {
                bytes: 0,
                modified_secs: 0,
                modified_nanos: 0,
                #[cfg(unix)]
                device: 0,
                #[cfg(unix)]
                inode: 0,
                #[cfg(unix)]
                changed_secs: 0,
                #[cfg(unix)]
                changed_nanos: 0,
            }),
            Err(error) => Err(io_error(key, error)),
        }
    }

    /// Load the journal accelerator only when it proves it describes the exact
    /// current file generation. It is derived state: malformed, stale or absent
    /// indexes fall back to the full parser and never make an otherwise valid
    /// journal unavailable.
    fn current_journal_index(&self, key: &ExecutionKey) -> Option<JournalIndex> {
        let index = match read_bounded_json::<JournalIndex>(
            &self.journal_index_path(key),
            MAX_JOURNAL_INDEX_BYTES,
            MAX_JOURNAL_INDEX_JSON_NODES,
        ) {
            Ok(Some(index)) => index,
            Ok(None) => return None,
            Err(error) => {
                warn!(
                    execution = %key,
                    %error,
                    "[LOOP-STORE] ignoring an unreadable derived journal index"
                );
                return None;
            },
        };
        let structurally_valid = index.schema_version == JOURNAL_INDEX_SCHEMA_VERSION
            && valid_journal_digest(&index.chain_sha256)
            && valid_journal_digest(&index.integrity_sha256)
            && index.integrity_sha256 == journal_index_integrity(&index)
            && index.offsets.len() <= MAX_JOURNAL_INDEX_OFFSETS
            && index.committed_watermark <= index.last_seq
            && index.replayed.seq == index.committed_watermark
            && (index.last_seq == 0
                || index
                    .offsets
                    .last()
                    .is_some_and(|offset| offset.seq == index.last_seq))
            && index.offsets.iter().all(|offset| {
                offset.seq > 0
                    && offset.byte_offset < index.stamp.bytes
                    && valid_journal_digest(&offset.chain_before)
            })
            && index
                .offsets
                .windows(2)
                .all(|pair| pair[0].seq < pair[1].seq && pair[0].byte_offset < pair[1].byte_offset);
        if !structurally_valid {
            warn!(
                execution = %key,
                "[LOOP-STORE] ignoring a structurally invalid derived journal index"
            );
            return None;
        }
        match self.journal_stamp(key) {
            Ok(stamp) if stamp == index.stamp => Some(index),
            Ok(_) => None,
            Err(error) => {
                warn!(
                    execution = %key,
                    %error,
                    "[LOOP-STORE] could not fingerprint the journal; falling back to a full read"
                );
                None
            },
        }
    }

    fn persist_journal_index(&self, key: &ExecutionKey, index: &JournalIndex) -> StoreResult<()> {
        let mut sealed = index.clone();
        sealed.integrity_sha256 = journal_index_integrity(&sealed);
        let encoded = serde_json::to_vec(&sealed).map_err(|error| StoreError::Corrupt {
            key: key.clone(),
            detail: format!("the derived journal index could not be encoded: {error}"),
        })?;
        validate_json_bytes(
            &encoded,
            MAX_JOURNAL_INDEX_BYTES,
            MAX_JOURNAL_INDEX_JSON_NODES,
        )
        .map_err(|error| corrupt(key, error))?;
        if encoded.len() as u64 > MAX_JOURNAL_INDEX_BYTES {
            return Err(StoreError::Corrupt {
                key: key.clone(),
                detail: format!(
                    "the derived journal index encoded to {} bytes, over its {}-byte limit",
                    encoded.len(),
                    MAX_JOURNAL_INDEX_BYTES
                ),
            });
        }
        overwrite_derived_atomically(&self.journal_index_path(key), &encoded)
            .map_err(|error| io_error(key, error))
    }

    fn build_journal_index(
        &self,
        key: &ExecutionKey,
        raw: &str,
        journal: &Journal,
        watermark: u64,
    ) -> StoreResult<Option<JournalIndex>> {
        if (!raw.is_empty() && !raw.ends_with('\n')) || journal.last_seq() < watermark {
            return Ok(None);
        }
        let replayed = match replay(journal.authoritative(watermark)) {
            Ok(replayed) => replayed,
            // A syntactically valid log may still be semantically impossible
            // (for example, a record after a non-resumable terminal). Commit
            // historically publishes that state so the next phase entry can
            // quarantine it with the precise journal error. A derived index
            // must never turn that into a store-write refusal, so this shape is
            // simply not indexed.
            Err(_) => return Ok(None),
        };
        let last_event_seq = journal
            .authoritative(watermark)
            .iter()
            .rev()
            .find_map(|record| {
                matches!(
                    record.body,
                    JournalBody::Event { .. } | JournalBody::NamedEvent { .. }
                )
                .then_some(record.seq)
            });
        let mut records = journal.all_records().iter();
        let mut offsets = VecDeque::with_capacity(MAX_JOURNAL_INDEX_OFFSETS);
        let mut chain = "0".repeat(64);
        let mut byte_offset = 0u64;
        let mut previous: Option<&JournalRecord> = None;
        for segment in raw.split_inclusive('\n') {
            let chain_before = chain.clone();
            chain = chained_journal_digest(&chain, segment.as_bytes());
            if !segment.trim().is_empty() {
                let Some(record) = records.next() else {
                    return Err(StoreError::Corrupt {
                        key: key.clone(),
                        detail: "the parsed journal and its complete lines disagree".to_string(),
                    });
                };
                let batch_start = previous.map_or(true, |prior| {
                    prior.iteration != record.iteration
                        || prior.phase != record.phase
                        || record.ordinal <= prior.ordinal
                });
                if offsets.len() == MAX_JOURNAL_INDEX_OFFSETS {
                    offsets.pop_front();
                }
                offsets.push_back(JournalOffset {
                    seq: record.seq,
                    byte_offset,
                    iteration: record.iteration,
                    phase: record.phase,
                    ordinal: record.ordinal,
                    batch_start,
                    chain_before,
                });
                previous = Some(record);
            }
            byte_offset = byte_offset.saturating_add(segment.len() as u64);
        }
        if records.next().is_some() {
            return Err(StoreError::Corrupt {
                key: key.clone(),
                detail: "the parsed journal has records with no complete source line".to_string(),
            });
        }
        Ok(Some(JournalIndex {
            schema_version: JOURNAL_INDEX_SCHEMA_VERSION,
            stamp: self.journal_stamp(key)?,
            chain_sha256: chain,
            last_seq: journal.last_seq(),
            committed_watermark: watermark,
            replayed,
            last_event_seq,
            offsets: offsets.into_iter().collect(),
            integrity_sha256: String::new(),
        }))
    }

    fn read_indexed_suffix(
        &self,
        key: &ExecutionKey,
        offset: &JournalOffset,
        expected_chain: &str,
    ) -> StoreResult<Option<Vec<JournalRecord>>> {
        let mut file = File::open(self.journal_path(key)).map_err(|error| io_error(key, error))?;
        file.seek(SeekFrom::Start(offset.byte_offset))
            .map_err(|error| io_error(key, error))?;
        let remaining = self
            .journal_stamp(key)?
            .bytes
            .saturating_sub(offset.byte_offset);
        if remaining > MAX_JOURNAL_BYTES {
            return Err(StoreError::Journal(JournalError::FileTooLarge {
                bytes: remaining,
                limit: MAX_JOURNAL_BYTES,
            }));
        }
        let mut bytes = Vec::with_capacity(remaining as usize);
        file.take(remaining.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| io_error(key, error))?;
        if bytes.len() as u64 != remaining {
            return Err(StoreError::Corrupt {
                key: key.clone(),
                detail: "the journal changed while an indexed suffix was read".to_string(),
            });
        }
        let raw = String::from_utf8(bytes).map_err(|error| StoreError::Corrupt {
            key: key.clone(),
            detail: format!("an indexed journal suffix is not UTF-8: {error}"),
        })?;
        let mut chain = offset.chain_before.clone();
        for segment in raw.split_inclusive('\n') {
            chain = chained_journal_digest(&chain, segment.as_bytes());
        }
        if chain != expected_chain {
            warn!(
                execution = %key,
                from_seq = offset.seq,
                "[LOOP-STORE] an indexed journal suffix failed its hash-chain proof; falling back to the full parser"
            );
            return Ok(None);
        }
        Ok(Some(Journal::parse_suffix(&raw, offset.seq)?))
    }

    /// The committed watermark, or zero when nothing is committed.
    fn watermark(&self, key: &ExecutionKey) -> StoreResult<u64> {
        Ok(self
            .load_sync(key)?
            .map(|committed| committed.state.journal_seq)
            .unwrap_or(0))
    }

    /// Load the newest committed state.
    ///
    /// # Why this retries
    ///
    /// The revision comes from a directory scan and the state from a read, and
    /// [`RETAINED_SNAPSHOTS`] commits can land in between — at which point the
    /// file the scan named has been pruned. That is a healthy store under
    /// contention, not a damaged one, and reporting it as corruption would
    /// quarantine an execution that is merely busy. A re-scan sees the newer
    /// revision; only a name that stays missing across attempts is corruption.
    fn load_sync(&self, key: &ExecutionKey) -> StoreResult<Option<CommittedLoopState>> {
        #[cfg(test)]
        self.full_snapshot_loads
            .fetch_add(1, AtomicOrdering::SeqCst);
        let mut last_missing = None;
        for _ in 0..3 {
            let revision = self.latest_revision(key)?;
            if revision == Revision::INITIAL {
                return Ok(None);
            }
            // Whose state this is. `commit` refuses a directory that already
            // belongs to another scope, but nothing was checking it on the way
            // OUT — so a principal that normalises onto a neighbour's directory
            // was handed the neighbour's committed state, across the scope
            // boundary the key exists to draw. One bounded read of a small file,
            // next to a snapshot read that is already happening.
            self.ensure_key_record_recheck(key, &self.key_path(key))?;
            let path = self.snapshot_path(key, revision);
            let Some(state): Option<LoopState> =
                read_bounded_json(&path, MAX_STATE_BYTES, MAX_STATE_JSON_NODES)
                    .map_err(|error| corrupt(key, error))?
            else {
                last_missing = Some((revision, path));
                continue;
            };
            // The batch bound is enforced here, on the way IN, and not only where
            // a batch is built. A record that grew somewhere else is the one that
            // would otherwise fan a worker out past its own membership.
            if let Some(batch) = &state.pending {
                batch.validate()?;
            }
            return Ok(Some(CommittedLoopState { revision, state }));
        }
        let (revision, path) = last_missing.expect("the loop only exits here after a miss");
        Err(StoreError::Corrupt {
            key: key.clone(),
            detail: format!(
                "{} names revision {revision} but its file is gone, and re-scanning kept naming \
                 a file that is not there",
                path.display()
            ),
        })
    }

    /// Write the immutable key record, or verify the one already there.
    ///
    /// # What this actually guards
    ///
    /// The scope segments of a key go through the workspace layout's own
    /// normalisation, which maps several distinct principals onto one directory
    /// name. Two scopes colliding that way would silently share a journal, a
    /// lease and an effect ledger. This turns that collision into a refusal at
    /// the second scope's first commit, and it is also what lets
    /// [`Self::list_runnable`] recover the exact key from a directory whose name
    /// has already been normalised.
    fn ensure_key_record(&self, key: &ExecutionKey) -> StoreResult<()> {
        let path = self.key_path(key);
        let existing: Option<ExecutionKey> =
            read_bounded_json(&path, MAX_KEY_BYTES, MAX_KEY_JSON_NODES)
                .map_err(|error| corrupt(key, error))?;
        if let Some(existing) = existing {
            if &existing != key {
                return Err(StoreError::Corrupt {
                    key: key.clone(),
                    detail: format!(
                        "this directory already belongs to {existing}; two scopes normalise to \
                         one directory and sharing it would merge two runs' state"
                    ),
                });
            }
            return Ok(());
        }
        let encoded = serde_json::to_vec(key).map_err(|error| StoreError::Corrupt {
            key: key.clone(),
            detail: format!("the execution key could not be encoded: {error}"),
        })?;
        validate_json_bytes(&encoded, MAX_KEY_BYTES, MAX_KEY_JSON_NODES)
            .map_err(|error| corrupt(key, error))?;
        // Same asymmetry as the lease above: the execution id is length-bounded
        // by `ExecutionKey`, but the scope segments are not, and this record is
        // what `list_runnable` reads to recover the exact key. One it cannot read
        // makes the execution invisible to scheduling forever.
        if encoded.len() as u64 > MAX_KEY_BYTES {
            return Err(StoreError::Corrupt {
                key: key.clone(),
                detail: format!(
                    "the execution key encodes to {} bytes, over the {MAX_KEY_BYTES}-byte limit \
                     it would have to be read back within",
                    encoded.len()
                ),
            });
        }
        match publish_new_file(&path, &encoded) {
            // A racing first commit wrote it. Re-read to confirm it is the same
            // key rather than assuming; the whole point of the record is that a
            // different key here is a collision.
            Ok(Publish::AlreadyExists) => self.ensure_key_record_recheck(key, &path),
            Ok(Publish::Published) => Ok(()),
            Err(error) => Err(io_error(key, error)),
        }
    }

    /// Refuse a call whose directory already belongs to somebody else.
    ///
    /// # Why this exists beside [`Self::ensure_key_record`]
    ///
    /// The stamping variant is for the paths that *create* the directory: it
    /// writes the record when there is none and refuses when the one there names
    /// another key. This one only reads, and tolerates absence — because the
    /// read paths must keep answering "nothing here" for an execution that has
    /// never been written, which is a case
    /// `an_execution_that_never_committed_loads_as_nothing` asserts.
    ///
    /// # Why every path needs one or the other
    ///
    /// The collision this guards is not exotic: the scope segments go through
    /// the workspace layout's normalisation, which maps several distinct
    /// principals onto one directory name. Refusing only at `commit` and `load`
    /// left the rest of the surface open, and those are the paths carrying the
    /// most content — `read_journal` hands back every
    /// [`JournalBody::Event`](super::super::journal::JournalBody::Event)
    /// payload the neighbouring run wrote, `load_effects` hands back its whole
    /// ledger, and `claim` takes its lease and reaps its temporaries. A key
    /// record that gates two doors of fourteen is not a scope boundary.
    fn ensure_directory_is_not_another_scopes(&self, key: &ExecutionKey) -> StoreResult<()> {
        let existing: Option<ExecutionKey> =
            read_bounded_json(&self.key_path(key), MAX_KEY_BYTES, MAX_KEY_JSON_NODES)
                .map_err(|error| corrupt(key, error))?;
        match existing {
            Some(existing) if &existing != key => Err(StoreError::Corrupt {
                key: key.clone(),
                detail: format!(
                    "this directory already belongs to {existing}; two scopes normalise to one \
                     directory and sharing it would merge two runs' state"
                ),
            }),
            _ => Ok(()),
        }
    }

    fn ensure_key_record_recheck(&self, key: &ExecutionKey, path: &Path) -> StoreResult<()> {
        let existing: Option<ExecutionKey> =
            read_bounded_json(path, MAX_KEY_BYTES, MAX_KEY_JSON_NODES)
                .map_err(|error| corrupt(key, error))?;
        match existing {
            Some(existing) if &existing == key => Ok(()),
            Some(existing) => Err(StoreError::Corrupt {
                key: key.clone(),
                detail: format!("this directory already belongs to {existing}"),
            }),
            // `read_bounded_json` answers `Ok(None)` for **absence** and `Err`
            // for everything else, so this arm is the record being GONE — not
            // unreadable, which the `?` above already took. An earlier message
            // here said "exists and cannot be read", which is the one state this
            // arm cannot be in. Both callers reach it only where the record must
            // exist: after `Publish::AlreadyExists`, and from `load_sync` where a
            // published revision implies the commit that wrote it. Nothing in
            // this store removes a key record, so absence here is a directory
            // something else is editing.
            None => Err(StoreError::Corrupt {
                key: key.clone(),
                detail: "the execution key record is gone; it is written before the first commit \
                         and nothing here removes it, so this directory is being edited by \
                         something that is not this store"
                    .to_string(),
            }),
        }
    }

    fn latest_lease(&self, key: &ExecutionKey) -> StoreResult<Option<StoredLease>> {
        let dir = self.execution_dir(key);
        let fences = scan_numbered(&dir, LEASE_PREFIX).map_err(|error| io_error(key, error))?;
        let Some(fence) = fences.last().copied() else {
            return Ok(None);
        };
        let path = dir.join(format!("{LEASE_PREFIX}{fence}{JSON_SUFFIX}"));
        read_bounded_json(&path, MAX_LEASE_BYTES, MAX_LEASE_JSON_NODES)
            .map_err(|error| corrupt(key, error))
    }

    fn publish_lease(&self, key: &ExecutionKey, lease: &StoredLease) -> StoreResult<Publish> {
        let path = self
            .execution_dir(key)
            .join(format!("{LEASE_PREFIX}{}{JSON_SUFFIX}", lease.fence));
        let encoded = serde_json::to_vec(lease).map_err(|error| StoreError::Corrupt {
            key: key.clone(),
            detail: format!("a lease could not be encoded: {error}"),
        })?;
        validate_json_bytes(&encoded, MAX_LEASE_BYTES, MAX_LEASE_JSON_NODES)
            .map_err(|error| corrupt(key, error))?;
        // A lease embeds a caller-supplied worker id, which nothing bounds. Left
        // unchecked, a long enough one writes a lease that [`MAX_LEASE_BYTES`]
        // then refuses to read back — and the execution becomes unclaimable by
        // anyone, including the worker that wrote it.
        if encoded.len() as u64 > MAX_LEASE_BYTES {
            return Err(StoreError::Corrupt {
                key: key.clone(),
                detail: format!(
                    "a lease for worker {} encodes to {} bytes, over the {MAX_LEASE_BYTES}-byte \
                     limit it would have to be read back within",
                    lease.worker,
                    encoded.len()
                ),
            });
        }
        let published = publish_new_file(&path, &encoded).map_err(|error| io_error(key, error))?;
        if published == Publish::Published {
            prune_numbered(&self.execution_dir(key), LEASE_PREFIX, RETAINED_LEASES);
        }
        Ok(published)
    }

    /// Whether a live lease bars this execution from a runnable scan right now.
    fn blocking_lease(&self, key: &ExecutionKey, now_ms: i64) -> StoreResult<Option<StoredLease>> {
        let Some(lease) = self.latest_lease(key)? else {
            return Ok(None);
        };
        if lease.released || lease.expires_at_ms <= now_ms {
            return Ok(None);
        }
        Ok(Some(lease))
    }

    fn read_effect(
        &self,
        key: &ExecutionKey,
        effect_id: &EffectId,
    ) -> StoreResult<Option<EffectLedgerEntry>> {
        read_bounded_json(
            &self.effect_path(key, effect_id),
            MAX_EFFECT_BYTES,
            MAX_EFFECT_JSON_NODES,
        )
        .map_err(|error| corrupt(key, error))
    }

    /// Whether this execution already holds `limit` effect rows.
    ///
    /// Counted rather than tracked, which makes a new effect O(rows). That is
    /// the same shape as the journal's whole-file parse and is accepted for the
    /// same reason: the alternative is a second durable counter with a
    /// compare-and-swap of its own, and the cost only becomes interesting at a
    /// row count the execution is being refused for reaching.
    fn effects_at_capacity(&self, key: &ExecutionKey, limit: usize) -> StoreResult<bool> {
        let entries = match std::fs::read_dir(self.effects_dir(key)) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(io_error(key, error)),
        };
        let mut counted = 0usize;
        for entry in entries {
            let entry = entry.map_err(|error| io_error(key, error))?;
            if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            counted += 1;
            if counted >= limit {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn write_effect(&self, key: &ExecutionKey, entry: &EffectLedgerEntry) -> StoreResult<()> {
        let path = self.effect_path(key, &entry.effect_id);
        let encoded = serde_json::to_vec(entry).map_err(|error| StoreError::Corrupt {
            key: key.clone(),
            detail: format!("an effect row could not be encoded: {error}"),
        })?;
        validate_json_bytes(&encoded, MAX_EFFECT_BYTES, MAX_EFFECT_JSON_NODES)
            .map_err(|error| corrupt(key, error))?;
        if encoded.len() as u64 > MAX_EFFECT_BYTES {
            return Err(StoreError::Corrupt {
                key: key.clone(),
                detail: format!(
                    "an effect row of {} bytes could not be read back within its own limit",
                    encoded.len()
                ),
            });
        }
        overwrite_atomically(&path, &encoded).map_err(|error| io_error(key, error))
    }
}

/// A lease as it rests on disk.
///
/// `released` rather than deleting the file, because deleting would let a
/// straggler re-create the same fence and present it as current. Fences only ever
/// go forward.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredLease {
    worker: WorkerId,
    fence: u64,
    expires_at_ms: i64,
    #[serde(default)]
    released: bool,
}

/// One outstanding wake resolution, as it rests on disk.
///
/// The file's *name* is the resolution's digest, which is what makes publishing
/// idempotent; its content carries both raw values, because the person reading
/// this directory is debugging a park that woke when it should not have or did
/// not wake when it should, and a directory of digests answers neither question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredWakeResolution {
    wake_token: String,
    resolution_id: String,
}

impl FsLoopStateStore {
    fn load_store_sync(&self, key: &ExecutionKey) -> StoreResult<Option<CommittedLoopState>> {
        self.load_sync(key)
    }

    fn commit_store_sync(
        &self,
        key: &ExecutionKey,
        state: &LoopState,
        expected: Revision,
    ) -> StoreResult<Revision> {
        let _lock = self.lock_execution(key)?;
        self.commit_store_locked_sync(key, state, expected, None)
    }

    fn commit_journal_facts(
        &self,
        key: &ExecutionKey,
        current_watermark: u64,
        next_watermark: u64,
    ) -> StoreResult<CommitJournalFacts> {
        if let Some(index) = self.current_journal_index(key) {
            if index.committed_watermark == current_watermark
                && index.replayed.seq == current_watermark
                && next_watermark >= current_watermark
                && next_watermark <= index.last_seq
            {
                let mut next_replayed = index.replayed;
                let mut next_last_event_seq = index.last_event_seq;
                if next_watermark > current_watermark {
                    if let Some(offset) = index
                        .offsets
                        .iter()
                        .find(|offset| offset.seq == current_watermark.saturating_add(1))
                    {
                        let Some(mut suffix) =
                            self.read_indexed_suffix(key, offset, &index.chain_sha256)?
                        else {
                            return self.commit_journal_facts_full(
                                key,
                                current_watermark,
                                next_watermark,
                            );
                        };
                        suffix.truncate(
                            suffix.partition_point(|record| record.seq <= next_watermark),
                        );
                        if suffix.last().map_or(current_watermark, |record| record.seq)
                            == next_watermark
                        {
                            next_replayed = match replay_from(index.replayed, &suffix) {
                                Ok(replayed) => replayed,
                                Err(_) => {
                                    return self.commit_journal_facts_full(
                                        key,
                                        current_watermark,
                                        next_watermark,
                                    )
                                },
                            };
                            next_last_event_seq = suffix
                                .iter()
                                .rev()
                                .find_map(|record| {
                                    matches!(
                                        record.body,
                                        JournalBody::Event { .. } | JournalBody::NamedEvent { .. }
                                    )
                                    .then_some(record.seq)
                                })
                                .or(index.last_event_seq);
                        } else {
                            return self.commit_journal_facts_full(
                                key,
                                current_watermark,
                                next_watermark,
                            );
                        }
                    } else {
                        return self.commit_journal_facts_full(
                            key,
                            current_watermark,
                            next_watermark,
                        );
                    }
                }
                let mut next_index = index.clone();
                next_index.committed_watermark = next_watermark;
                next_index.replayed = next_replayed;
                next_index.last_event_seq = next_last_event_seq;
                return Ok(CommitJournalFacts {
                    last_seq: index.last_seq,
                    current_ending: ending_from_index(index.replayed, index.last_event_seq),
                    next_ending: ending_from_index(next_replayed, next_last_event_seq),
                    next_index: Some(next_index),
                });
            }
        }
        self.commit_journal_facts_full(key, current_watermark, next_watermark)
    }

    fn commit_journal_facts_full(
        &self,
        key: &ExecutionKey,
        current_watermark: u64,
        next_watermark: u64,
    ) -> StoreResult<CommitJournalFacts> {
        let raw = self.read_journal_text(key)?;
        let journal = Journal::parse(&raw)?;
        if next_watermark > journal.last_seq() {
            return Err(StoreError::WatermarkAhead {
                watermark: next_watermark,
                last_seq: journal.last_seq(),
            });
        }
        Ok(CommitJournalFacts {
            last_seq: journal.last_seq(),
            current_ending: non_resumable_terminal(journal.authoritative(current_watermark)),
            next_ending: non_resumable_terminal(journal.authoritative(next_watermark)),
            next_index: self.build_journal_index(key, &raw, &journal, next_watermark)?,
        })
    }

    /// Commit while the caller holds this execution's cross-process mutation
    /// guard. Ending publication and snapshot publication must remain inside one
    /// such section so no writer can observe the prepublished marker and race a
    /// different snapshot into the same revision.
    fn commit_store_locked_sync(
        &self,
        key: &ExecutionKey,
        state: &LoopState,
        expected: Revision,
        commit_worker: Option<&WorkerId>,
    ) -> StoreResult<Revision> {
        if let Some(batch) = &state.pending {
            batch.validate()?;
        }

        // Whose directory this is, BEFORE the compare-and-swap.
        //
        // The scope segments go through the workspace layout's normalisation,
        // which maps several distinct principals onto one directory name. Asking
        // the CAS first told a colliding scope "another holder committed in
        // between" — a *retryable* answer, which sends the caller back to `load`,
        // where it would read the other scope's state. The collision is not a
        // conflict and must not be reported as one; `two_scopes_that_normalise_to
        // _one_directory_are_refused` asserts the refusal and was failing on this
        // exact ordering.
        create_dir_all_durably(&self.execution_dir(key)).map_err(|error| io_error(key, error))?;
        self.ensure_key_record(key)?;

        let current = self.latest_revision(key)?;
        if current != expected {
            return Err(StoreError::Conflict {
                expected,
                found: current,
            });
        }

        let current_committed = if expected == Revision::INITIAL {
            None
        } else {
            let Some(committed) = self.load_sync(key)? else {
                return Err(StoreError::Corrupt {
                    key: key.clone(),
                    detail: format!(
                        "latest revision is {expected}, but its committed snapshot is absent"
                    ),
                });
            };
            if committed.revision != expected {
                return Err(StoreError::Conflict {
                    expected,
                    found: committed.revision,
                });
            }
            Some(committed)
        };
        if current_committed.as_ref().is_some_and(|committed| {
            committed.state.segment_binding != state.segment_binding
                || committed.state.identity.execution_id != state.identity.execution_id
        }) {
            return Err(StoreError::Corrupt {
                key: key.clone(),
                detail: "a committed segment cannot change its runtime execution binding"
                    .to_owned(),
            });
        }
        let current_watermark = current_committed
            .as_ref()
            .map_or(0, |committed| committed.state.journal_seq);
        let journal_facts = self.commit_journal_facts(key, current_watermark, state.journal_seq)?;
        if state.journal_seq > journal_facts.last_seq {
            return Err(StoreError::WatermarkAhead {
                watermark: state.journal_seq,
                last_seq: journal_facts.last_seq,
            });
        }

        let encoded = serde_json::to_vec(state).map_err(|error| StoreError::Corrupt {
            key: key.clone(),
            detail: format!("the loop state could not be encoded: {error}"),
        })?;
        validate_json_bytes(&encoded, MAX_STATE_BYTES, MAX_STATE_JSON_NODES)
            .map_err(|error| corrupt(key, error))?;
        // Refused at write, because a state written past this limit is a state
        // no bounded read could ever load again.
        if encoded.len() as u64 > MAX_STATE_BYTES {
            return Err(StoreError::Corrupt {
                key: key.clone(),
                detail: format!(
                    "a loop state of {} bytes is over the {MAX_STATE_BYTES}-byte limit and could \
                     not be read back",
                    encoded.len()
                ),
            });
        }

        // The immutable reverse binding is published before the first snapshot
        // and its exact base catalog lock is retained through snapshot CAS.
        // Recovery takes the same lock around membership snapshots, so it can
        // neither miss a concurrently created segment nor mistake a
        // pre-snapshot crash record for committed work. Existing segments pay
        // only one bounded record validation, not a flock, on ordinary commits.
        let base_binding_catalog_guard = if let Some(record) =
            self.base_binding_record_for_state(key, state)?
        {
            let path = self.base_binding_record_path(&record);
            let existing: Option<BaseSegmentBindingRecord> =
                read_bounded_json(&path, MAX_BASE_BINDING_BYTES, MAX_BASE_BINDING_JSON_NODES)
                    .map_err(|error| StoreError::Unavailable {
                        detail: format!("base execution binding validation failed: {error}"),
                    })?;
            if existing
                .as_ref()
                .is_some_and(|existing| existing != &record)
            {
                return Err(StoreError::Unavailable {
                    detail: format!(
                        "{} conflicts with the segment's immutable runtime binding",
                        path.display()
                    ),
                });
            }
            if expected == Revision::INITIAL || existing.is_none() {
                let binding_dir = self.base_binding_dir(
                    key.principal(),
                    key.workspace(),
                    &record.base_execution_id,
                );
                let guard = acquire_private_catalog_lock(&binding_dir.join("catalog.lock"))
                    .map_err(|error| StoreError::Unavailable {
                        detail: format!("base execution catalog lock failed: {error}"),
                    })?;
                reap_leaked_temporaries(&binding_dir);
                if expected == Revision::INITIAL {
                    let admission_path = binding_dir.join("preseed.admission");
                    let mut admission: Option<BasePreseedAdmission> = read_bounded_json(
                        &admission_path,
                        MAX_BASE_BINDING_BYTES,
                        MAX_BASE_BINDING_JSON_NODES,
                    )
                    .map_err(|error| StoreError::Unavailable {
                        detail: format!("pre-seed admission commit read failed: {error}"),
                    })?;
                    let supplied = state
                        .segment_binding
                        .as_ref()
                        .and_then(|binding| binding.preseed_admission_token.as_deref());
                    let now_ms = Utc::now().timestamp_millis();
                    if admission.as_ref().is_some_and(|admission| {
                        valid_base_preseed_admission(
                            admission,
                            key.principal(),
                            key.workspace(),
                            &record.base_execution_id,
                        ) && admission.expires_at_ms <= now_ms
                    }) {
                        match std::fs::remove_file(&admission_path) {
                                Ok(()) => sync_parent_directory(&admission_path).map_err(
                                    |error| StoreError::Unavailable {
                                        detail: format!(
                                            "expired pre-seed admission retirement was not synced: {error}"
                                        ),
                                    },
                                )?,
                                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                                Err(error) => {
                                    return Err(StoreError::Unavailable {
                                        detail: format!(
                                            "expired pre-seed admission retirement failed: {error}"
                                        ),
                                    })
                                },
                            }
                        admission = None;
                        if supplied.is_some() {
                            return Err(StoreError::Unavailable {
                                detail:
                                    "the supplied pre-seed admission expired before first commit"
                                        .to_owned(),
                            });
                        }
                    }
                    let current_head_sha256 = self
                        .read_base_binding_head_sync(
                            key.principal(),
                            key.workspace(),
                            &record.base_execution_id,
                        )?
                        .map(|head| head.integrity_sha256);
                    match admission {
                        Some(admission)
                            if valid_base_preseed_admission(
                                &admission,
                                key.principal(),
                                key.workspace(),
                                &record.base_execution_id,
                            ) && admission.schema_version
                                == BASE_PRESEED_ADMISSION_SCHEMA_VERSION
                                && admission.exact_segment.is_none()
                                && admission.exact_revision.is_none()
                                && current_head_sha256.as_deref()
                                    == Some(admission.base_binding_head_sha256.as_str())
                                && admission.expires_at_ms > now_ms
                                && supplied == Some(admission.token.as_str()) => {},
                        Some(admission)
                            if valid_base_preseed_admission(
                                &admission,
                                key.principal(),
                                key.workspace(),
                                &record.base_execution_id,
                            ) && admission.expires_at_ms > now_ms =>
                        {
                            return Err(StoreError::Unavailable {
                                detail: "a live pre-seed admission is owned by another recovery"
                                    .to_owned(),
                            });
                        },
                        Some(_) => {
                            return Err(StoreError::Unavailable {
                                detail: "pre-seed admission is corrupt or expired".to_owned(),
                            });
                        },
                        None if supplied.is_some() => {
                            return Err(StoreError::Unavailable {
                                detail: "the supplied pre-seed admission no longer exists"
                                    .to_owned(),
                            });
                        },
                        None => {},
                    }
                }
                self.publish_base_binding_record_locked(&record)?;
                Some(guard)
            } else {
                None
            }
        } else {
            None
        };

        let next = expected.next();
        let may_create_terminal_debt = state.steer_consume_receipt.is_some()
            || state.terminal_settlement_receipt.is_some()
            || journal_facts.next_ending.is_some();
        let may_retire_terminal_debt = current_committed.as_ref().is_some_and(|committed| {
            committed.state.steer_consume_receipt.is_some()
                || committed.state.terminal_settlement_receipt.is_some()
        }) || journal_facts.current_ending.is_some();
        if may_create_terminal_debt {
            // Prepublish before the snapshot CAS. If this process dies after
            // the authoritative snapshot lands, discovery still has a durable
            // candidate. A scanner seeing the marker before the snapshot keeps
            // it as a bounded future revision and exact-validates it later.
            // Either the exact candidate or a durable dirty invalidation must
            // land before the authoritative snapshot. If both publications
            // fail, refusing before CAS is the only way to avoid acknowledging
            // terminal state that a previously complete catalog can hide.
            self.publish_terminal_debt_candidate(key, next)?;
        }
        // Publish a tiny two-revision transaction record BEFORE the snapshot
        // CAS. The current binding is essential: replacing a scalar ending (or
        // retracting it) with only the proposed value hid an already-committed
        // terminal if publication of `next` subsequently failed or the process
        // crashed. Readers load a snapshot first and then select its exact
        // revision binding, so both sides of this two-file transition are safe.
        let mut ending_bindings = Vec::with_capacity(2);
        if current_committed.is_some() {
            ending_bindings.push(RevisionEndingBinding {
                revision: expected,
                ending: journal_facts.current_ending,
            });
        }
        ending_bindings.push(RevisionEndingBinding {
            revision: next,
            ending: journal_facts.next_ending,
        });
        if let Some(index) = journal_facts.next_index.as_ref() {
            if let Err(error) = self.persist_journal_index(key, index) {
                warn!(
                    execution = %key,
                    %error,
                    "[LOOP-STORE] committing without refreshing the derived journal index; the next access will verify the full log"
                );
            }
        }
        self.publish_ending_transition(key, ending_bindings)?;

        let path = self.snapshot_path(key, next);
        match publish_new_file(&path, &encoded).map_err(|error| io_error(key, error))? {
            Publish::Published => {},
            // The link is the compare-and-swap. Somebody published this revision
            // between the scan above and this call. Re-scan rather than reporting
            // `next`: the winner may already have committed again, and a caller
            // told a revision that is no longer current would retry into a second
            // conflict. The same reason `claim` re-reads the holder it names.
            Publish::AlreadyExists => {
                return Err(StoreError::Conflict {
                    expected,
                    found: self.latest_revision(key).unwrap_or(next),
                })
            },
        }
        if let Err(error) = self.persist_placement_index(key, next, &state.placement) {
            warn!(
                execution = %key,
                revision = %next,
                %error,
                "[LOOP-STORE] committed without refreshing the derived placement index; durable owner lookup will fall back to the full state"
            );
        }
        if let Err(error) = self.persist_segment_recovery_index(
            key,
            next,
            state,
            journal_facts.next_ending.is_some(),
        ) {
            warn!(
                execution = %key,
                revision = %next,
                %error,
                "[LOOP-STORE] committed without refreshing the derived recovery index; exact restart discovery will verify the full snapshot"
            );
        }
        if may_create_terminal_debt || may_retire_terminal_debt {
            if let Err(error) = self.refresh_terminal_debt_candidate(key, next) {
                warn!(
                    execution = %key,
                    revision = %next,
                    %error,
                    "[LOOP_OUTBOX] committed snapshot could not refresh its derived terminal-debt candidate; exact authoritative discovery remains available"
                );
            }
        }
        // The snapshot CAS above is the commit point. Admission retirement is
        // derived cleanup and must never turn that committed success into an
        // ordinary execution error. Drop any first-publication catalog guard,
        // reacquire the same base lock in the cleanup helper, reload `next`, and
        // remove only the exact token whose lineage the committed snapshot (and
        // for exact recovery, the fenced worker) proves. A crash or I/O failure
        // is retried by the next claim/classification path.
        drop(base_binding_catalog_guard);
        let may_consume_preseed = expected == Revision::INITIAL
            && state
                .segment_binding
                .as_ref()
                .and_then(|binding| binding.preseed_admission_token.as_ref())
                .is_some();
        let may_consume_exact =
            commit_worker.is_some_and(|worker| worker.as_str().starts_with("recovery-admission-"));
        if may_consume_preseed || may_consume_exact {
            if let Err(error) = self.retire_base_recovery_admission_after_commit_sync(
                key,
                state,
                expected,
                next,
                commit_worker,
            ) {
                warn!(
                    execution = %key,
                    revision = %next,
                    %error,
                    "[LOOP-STORE] committed snapshot; durable recovery-admission retirement will retry"
                );
            }
        }
        prune_numbered(
            &self.execution_dir(key),
            SNAPSHOT_PREFIX,
            RETAINED_SNAPSHOTS,
        );
        Ok(next)
    }

    fn retire_base_recovery_admission_after_commit_sync(
        &self,
        key: &ExecutionKey,
        state: &LoopState,
        expected: Revision,
        committed_revision: Revision,
        commit_worker: Option<&WorkerId>,
    ) -> StoreResult<()> {
        let Some(record) = self.base_binding_record_for_state(key, state)? else {
            return Ok(());
        };
        let binding_dir =
            self.base_binding_dir(key.principal(), key.workspace(), &record.base_execution_id);
        let _catalog =
            acquire_private_catalog_lock(&binding_dir.join("catalog.lock")).map_err(|error| {
                StoreError::Unavailable {
                    detail: format!("post-commit recovery-admission lock failed: {error}"),
                }
            })?;
        let path = binding_dir.join("preseed.admission");
        let admission: Option<BasePreseedAdmission> =
            read_bounded_json(&path, MAX_BASE_BINDING_BYTES, MAX_BASE_BINDING_JSON_NODES).map_err(
                |error| StoreError::Unavailable {
                    detail: format!("post-commit recovery-admission read failed: {error}"),
                },
            )?;
        let Some(admission) = admission else {
            return Ok(());
        };
        if !valid_base_preseed_admission(
            &admission,
            key.principal(),
            key.workspace(),
            &record.base_execution_id,
        ) {
            return Err(StoreError::Unavailable {
                detail: "post-commit recovery admission is invalid".to_owned(),
            });
        }
        let Some(committed) = self.load_sync(key)? else {
            return Err(StoreError::Unavailable {
                detail: "committed snapshot disappeared before recovery-admission retirement"
                    .to_owned(),
            });
        };
        let binding_matches = committed.revision == committed_revision
            && committed
                .state
                .segment_binding
                .as_ref()
                .is_some_and(|binding| {
                    binding.base_execution_id == record.base_execution_id
                        && binding.exact_segment_id == key.execution_id()
                });
        if !binding_matches {
            return Err(StoreError::Unavailable {
                detail: "post-commit snapshot does not prove the recovery-admission lineage"
                    .to_owned(),
            });
        }
        let preseed_consumed = expected == Revision::INITIAL
            && admission.exact_segment.is_none()
            && admission.exact_revision.is_none()
            && committed
                .state
                .segment_binding
                .as_ref()
                .and_then(|binding| binding.preseed_admission_token.as_deref())
                == Some(admission.token.as_str());
        let exact_consumed = expected != Revision::INITIAL
            && admission.exact_segment.as_deref() == Some(key.execution_id())
            && admission.exact_revision == Some(expected)
            && commit_worker.is_some_and(|worker| {
                worker
                    == &crate::magician_v2::execution::agentic::run_loop::state::recovery_admission_worker_id(
                        &admission.token,
                    )
            });
        if preseed_consumed || exact_consumed {
            retire_base_recovery_admission_file_locked(&path)?;
        }
        Ok(())
    }

    /// Crash-retry proof for admission cleanup. The file is derived exclusion,
    /// so deletion is allowed only when a committed snapshot proves the same
    /// opaque preseed token, or the exact admitted revision advanced once under
    /// the token-derived pinned worker.
    fn base_recovery_admission_consumed_locked(
        &self,
        principal: &str,
        workspace: &str,
        base_execution_id: &str,
        admission: &BasePreseedAdmission,
    ) -> StoreResult<bool> {
        if let (Some(exact_segment), Some(exact_revision)) =
            (admission.exact_segment.as_deref(), admission.exact_revision)
        {
            let key = ExecutionKey::new(
                principal.to_owned(),
                workspace.to_owned(),
                exact_segment.to_owned(),
            )?;
            let Some(committed) = self.load_sync(&key)? else {
                return Ok(false);
            };
            let expected_worker =
                crate::magician_v2::execution::agentic::run_loop::state::recovery_admission_worker_id(
                    &admission.token,
                );
            return Ok(committed.revision == exact_revision.next()
                && committed
                    .state
                    .segment_binding
                    .as_ref()
                    .is_some_and(|binding| {
                        binding.base_execution_id == base_execution_id
                            && binding.exact_segment_id == exact_segment
                    })
                && matches!(
                    &committed.state.placement,
                    Placement::Pinned { worker, .. } if worker == &expected_worker
                ));
        }
        if admission.exact_segment.is_some() || admission.exact_revision.is_some() {
            return Ok(false);
        }
        let records =
            self.read_base_binding_records_sync(principal, workspace, base_execution_id)?;
        for record in records {
            let key = ExecutionKey::new(
                principal.to_owned(),
                workspace.to_owned(),
                record.exact_segment_id,
            )?;
            let Some(committed) = self.load_sync(&key)? else {
                continue;
            };
            if committed
                .state
                .segment_binding
                .as_ref()
                .is_some_and(|binding| {
                    binding.base_execution_id == base_execution_id
                        && binding.exact_segment_id == key.execution_id()
                        && binding.preseed_admission_token.as_deref()
                            == Some(admission.token.as_str())
                })
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn commit_fenced_sync(
        &self,
        key: &ExecutionKey,
        state: &LoopState,
        expected: Revision,
        lease: &Lease,
    ) -> StoreResult<Revision> {
        let _lock = self.lock_execution(key)?;
        self.validate_current_lease(key, lease)?;
        self.commit_store_locked_sync(key, state, expected, Some(&lease.worker))
    }

    fn try_append_journal_indexed_locked(
        &self,
        key: &ExecutionKey,
        appends: &[JournalAppend],
        watermark: u64,
    ) -> StoreResult<Option<u64>> {
        let Some(mut index) = self.current_journal_index(key) else {
            return Ok(None);
        };
        if index.committed_watermark != watermark
            || index.replayed.seq != watermark
            || index.last_seq < watermark
        {
            return Ok(None);
        }

        if index.last_seq > watermark {
            let Some(orphan_start) = index
                .offsets
                .iter()
                .find(|offset| offset.seq == watermark.saturating_add(1))
                .cloned()
            else {
                return Ok(None);
            };
            let Some(orphaned) =
                self.read_indexed_suffix(key, &orphan_start, &index.chain_sha256)?
            else {
                return Ok(None);
            };
            let retained = orphaned
                .iter()
                .map(JournalRecord::to_line)
                .collect::<Result<Vec<_>, _>>()?
                .join("\n");
            if !retained.is_empty() {
                let swept_path = self.swept_journal_path(key);
                let swept_bytes = match std::fs::metadata(&swept_path) {
                    Ok(metadata) => metadata.len(),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
                    Err(error) => return Err(io_error(key, error)),
                };
                let projected_swept = swept_bytes
                    .saturating_add(retained.len() as u64)
                    .saturating_add(1);
                if projected_swept <= MAX_JOURNAL_BYTES {
                    append_lines_durably(&swept_path, &retained)
                        .map_err(|error| io_error(key, error))?;
                } else {
                    warn!(
                        path = %swept_path.display(),
                        bytes = swept_bytes,
                        projected_bytes = projected_swept,
                        "[LOOP-STORE] the swept-attempt file is at its ceiling; orphaned records are no longer retained"
                    );
                }
            }
            let file = OpenOptions::new()
                .write(true)
                .open(self.journal_path(key))
                .map_err(|error| io_error(key, error))?;
            file.set_len(orphan_start.byte_offset)
                .map_err(|error| io_error(key, error))?;
            file.sync_all().map_err(|error| io_error(key, error))?;
            index.last_seq = watermark;
            index.chain_sha256 = orphan_start.chain_before;
            index.offsets.retain(|offset| offset.seq <= watermark);
            index.stamp = self.journal_stamp(key)?;
        }

        if watermark.saturating_add(appends.len() as u64) > MAX_JOURNAL_RECORDS as u64 {
            return Err(StoreError::Journal(JournalError::TooManyRecords {
                limit: MAX_JOURNAL_RECORDS,
            }));
        }
        let at_ms = Utc::now().timestamp_millis();
        let mut seq = watermark;
        let mut records = Vec::with_capacity(appends.len());
        let mut lines = Vec::with_capacity(appends.len());
        for append in appends {
            seq = seq.saturating_add(1);
            let record = JournalRecord {
                seq,
                iteration: append.iteration,
                phase: append.phase,
                ordinal: append.ordinal,
                at_ms,
                body: append.body.clone(),
            };
            lines.push(record.to_line()?);
            records.push(record);
        }
        let payload = lines.join("\n");
        let projected = index.stamp.bytes.saturating_add(payload.len() as u64 + 1);
        if projected > MAX_JOURNAL_BYTES {
            return Err(StoreError::Journal(JournalError::FileTooLarge {
                bytes: projected,
                limit: MAX_JOURNAL_BYTES,
            }));
        }
        append_lines_durably(&self.journal_path(key), &payload)
            .map_err(|error| io_error(key, error))?;

        let mut byte_offset = index.stamp.bytes;
        for (position, (record, line)) in records.iter().zip(lines.iter()).enumerate() {
            let mut durable_line = line.as_bytes().to_vec();
            durable_line.push(b'\n');
            let chain_before = index.chain_sha256.clone();
            index.chain_sha256 = chained_journal_digest(&chain_before, &durable_line);
            index.offsets.push(JournalOffset {
                seq: record.seq,
                byte_offset,
                iteration: record.iteration,
                phase: record.phase,
                ordinal: record.ordinal,
                batch_start: position == 0,
                chain_before,
            });
            byte_offset = byte_offset.saturating_add(durable_line.len() as u64);
        }
        if index.offsets.len() > MAX_JOURNAL_INDEX_OFFSETS {
            let excess = index.offsets.len() - MAX_JOURNAL_INDEX_OFFSETS;
            index.offsets.drain(..excess);
        }
        index.last_seq = seq;
        index.stamp = self.journal_stamp(key)?;
        if let Err(error) = self.persist_journal_index(key, &index) {
            warn!(
                execution = %key,
                %error,
                "[LOOP-STORE] the journal append committed without refreshing its derived index; the next access will verify the full log"
            );
        }
        Ok(Some(seq))
    }

    fn append_journal_sync(
        &self,
        key: &ExecutionKey,
        appends: &[JournalAppend],
    ) -> StoreResult<u64> {
        let _lock = self.lock_execution(key)?;
        self.append_journal_locked_sync(key, appends)
    }

    fn append_journal_locked_sync(
        &self,
        key: &ExecutionKey,
        appends: &[JournalAppend],
    ) -> StoreResult<u64> {
        Journal::check_batch_addresses(key.execution_id(), appends)?;
        let watermark = self.watermark(key)?;
        if appends.is_empty() {
            return Ok(watermark);
        }
        if let Some(seq) = self.try_append_journal_indexed_locked(key, appends, watermark)? {
            return Ok(seq);
        }

        create_dir_all_durably(&self.execution_dir(key)).map_err(|error| io_error(key, error))?;
        // Stamped, not merely checked. An append is the first thing written for
        // an execution that has not committed yet, so a directory acquires its
        // owner here rather than waiting for a commit that may never come — and
        // until it has one, two colliding scopes would interleave their seqs in
        // one journal.
        self.ensure_key_record(key)?;
        let raw = self.read_journal_text(key)?;
        // A file that does not end in a newline ends in a record whose write was
        // interrupted. `Journal::parse` drops that stump, but the **bytes** are
        // still on disk, and appending after them splices the next record onto
        // the stump — one unparseable line in the *middle* of the log, which is
        // the single damage nothing here recovers from. So an unterminated tail
        // is rewritten away exactly like an orphaned attempt, whether or not
        // there is one; the committed prefix is what gets written back either
        // way. Without this, a crash mid-append at a moment when the watermark
        // already covered every complete record bricked the execution's journal
        // permanently, and the error surfaced at some later read.
        let tail_is_unterminated = !raw.is_empty() && !raw.ends_with('\n');
        let journal = Journal::parse(&raw)?;

        // The log has to reach at least as far as the commit that vouches for
        // it. One that falls short has lost records a commit claimed, and
        // appending on top of it would place the next seq past a gap — which no
        // later read can parse at all. Refuse here, where the cause is visible,
        // rather than write the file that makes the execution unreadable and
        // surfaces as a seq break somewhere else.
        if journal.last_seq() < watermark {
            return Err(StoreError::Corrupt {
                key: key.clone(),
                detail: format!(
                    "the committed watermark is {watermark} but the log reaches only {}; records \
                     the commit vouches for are missing",
                    journal.last_seq()
                ),
            });
        }

        // Sweep before appending, never after. See the module docs on
        // `store::mod`: appending after an orphan lets the next commit's
        // watermark bury it, and replay then applies work nobody committed.
        if journal.last_seq() > watermark || tail_is_unterminated {
            let orphaned = journal.orphaned(watermark);
            let retained: String = orphaned
                .iter()
                .map(|record| record.to_line())
                .collect::<Result<Vec<_>, _>>()?
                .join("\n");
            // Empty when the rewrite is only clearing an unterminated tail:
            // there is nothing to keep in that case, since a stump is a record
            // that was never finished rather than an attempt that was never
            // committed.
            if !retained.is_empty() {
                // Retained rather than dropped: an operator investigating a
                // crash wants the attempt that did not commit, and the bytes
                // cost nothing next to the question they answer.
                //
                // Retained BEFORE the truncation below, never after. A process
                // that dies between the two leaves the orphans in both files and
                // the next sweep re-retains them, which costs a duplicate line in
                // a file nothing parses. The other order would lose them.
                //
                // This file is written and never read back by the store, so it
                // needs no read bound — but it does need a write one, or a run
                // that crashes repeatedly grows it without limit. Past the cap
                // the sweep stops retaining and says so; it never stops
                // sweeping, because that would be the correctness half.
                let swept_path = self.swept_journal_path(key);
                let swept_bytes = match std::fs::metadata(&swept_path) {
                    Ok(metadata) => metadata.len(),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
                    Err(error) => return Err(io_error(key, error)),
                };
                let projected_swept = swept_bytes
                    .saturating_add(retained.len() as u64)
                    .saturating_add(1);
                if projected_swept > MAX_JOURNAL_BYTES {
                    warn!(
                        path = %swept_path.display(),
                        bytes = swept_bytes,
                        projected_bytes = projected_swept,
                        "[LOOP-STORE] the swept-attempt file is at its ceiling; orphaned records \
                         are still being swept but are no longer being retained"
                    );
                } else {
                    append_lines_durably(&swept_path, &retained)
                        .map_err(|error| io_error(key, error))?;
                }
            }
            let kept: String = journal
                .authoritative(watermark)
                .iter()
                .map(|record| record.to_line())
                .collect::<Result<Vec<_>, _>>()?
                .join("\n");
            let mut bytes = kept.into_bytes();
            if !bytes.is_empty() {
                bytes.push(b'\n');
            }
            overwrite_atomically(&self.journal_path(key), &bytes)
                .map_err(|error| io_error(key, error))?;
        }

        // Both journal ceilings hold on the way IN as well as on the way out.
        // A reader refuses a log past either of them, so an append that crossed
        // one would write the record that makes every later read of this
        // execution fail — the same "written, then unreadable" trap the lease and
        // key records already close, and the same one an operator would meet far
        // from the append that set it.
        if watermark.saturating_add(appends.len() as u64) > MAX_JOURNAL_RECORDS as u64 {
            return Err(StoreError::Journal(JournalError::TooManyRecords {
                limit: MAX_JOURNAL_RECORDS,
            }));
        }

        let mut seq = watermark;
        let mut lines: Vec<String> = Vec::with_capacity(appends.len());
        let at_ms = Utc::now().timestamp_millis();
        for append in appends {
            seq += 1;
            let record = JournalRecord {
                seq,
                iteration: append.iteration,
                phase: append.phase,
                ordinal: append.ordinal,
                at_ms,
                body: append.body.clone(),
            };
            lines.push(record.to_line()?);
        }

        let payload = lines.join("\n");
        let path = self.journal_path(key);
        let existing_bytes = match std::fs::metadata(&path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(io_error(key, error)),
        };
        // `append_lines_durably` writes the payload and one newline.
        let projected = existing_bytes.saturating_add(payload.len() as u64 + 1);
        if projected > MAX_JOURNAL_BYTES {
            return Err(StoreError::Journal(JournalError::FileTooLarge {
                bytes: projected,
                limit: MAX_JOURNAL_BYTES,
            }));
        }
        append_lines_durably(&path, &payload).map_err(|error| io_error(key, error))?;
        match self
            .read_journal_text(key)
            .and_then(|raw| {
                let journal = Journal::parse(&raw)?;
                Ok((raw, journal))
            })
            .and_then(|(raw, journal)| self.build_journal_index(key, &raw, &journal, watermark))
        {
            Ok(Some(index)) => {
                if let Err(error) = self.persist_journal_index(key, &index) {
                    warn!(
                        execution = %key,
                        %error,
                        "[LOOP-STORE] the journal append committed without publishing its derived index"
                    );
                }
            },
            Ok(None) => {},
            Err(error) => warn!(
                execution = %key,
                %error,
                "[LOOP-STORE] the journal append committed but its derived index could not be rebuilt"
            ),
        }
        Ok(seq)
    }

    fn append_journal_fenced_sync(
        &self,
        key: &ExecutionKey,
        appends: &[JournalAppend],
        lease: &Lease,
    ) -> StoreResult<u64> {
        let _lock = self.lock_execution(key)?;
        self.validate_current_lease(key, lease)?;
        self.append_journal_locked_sync(key, appends)
    }

    /// # The `.cloned()` is still a deep copy, and the hot path no longer takes it
    ///
    /// [`Self::read_journal_file`] returns an **owned** [`Journal`] whose records
    /// are already built, so this copies every `JournalBody::Event` payload a
    /// second time. Peak memory is 2× the journal, bounded only by
    /// [`MAX_JOURNAL_BYTES`] (64 MiB), and the payloads are not small: the
    /// journalled `AgenticActionExecuted` carries a tool result up to
    /// `MAX_JOURNAL_RECORD_BYTES` (64 KiB) — see `executor.rs`'s
    /// *WHAT THIS LINE DOES TO JOURNAL VOLUME*.
    ///
    /// **What used to follow that copy no longer does.** An earlier version of
    /// this block said `verify_journal` hands the copy straight back into
    /// `Journal::from_records` for a re-walk. It does not: `verify_journal` and
    /// `project_outbox` — the two callers that wanted a whole checked journal,
    /// and the two that run per phase entry and per committed boundary — go
    /// through [`Self::read_journal_verified`] below, which returns the parse's
    /// own journal with neither the copy nor the second walk.
    ///
    /// # So who is left, and why this method keeps the copy
    ///
    /// Three callers, and none of them is per phase. `reconciler`'s
    /// child-journal scan iterates records and never builds a journal;
    /// `reconciler::retire_under_lease` does still read-then-`from_records`, once
    /// per retirement pass rather than six times an iteration, and moving it to
    /// [`Self::read_journal_verified`] is a change to a file this one does not
    /// own; and the store contract suite exercises `from_seq` itself. That last
    /// one is why the parameter stays: a **suffix** is exactly what a [`Journal`]
    /// cannot be — [`Journal::from_records`] requires a log starting at seq one —
    /// so a `Vec` is the right return here, and the copy is what a `Vec` of owned
    /// records costs.
    ///
    /// It is written with `.cloned()` rather than a move because [`Journal`]
    /// exposes no consuming accessor: its `records` field is private and
    /// `all_records` borrows. `Journal::into_records(self) -> Vec<JournalRecord>`
    /// beside `all_records` would turn this into an
    /// `into_iter().filter().collect()` with no copy at all. That is a change to
    /// `run_loop/journal.rs`, so it is named here rather than worked around here
    /// — a second parse in this file would put the torn-tail and seq-break rules
    /// in two places, which is the one thing `Journal` exists to prevent.
    ///
    /// This is a constant on the quadratic *Known characteristics* already
    /// admits above, not a new term: the parse still dominates.
    fn read_journal_sync(
        &self,
        key: &ExecutionKey,
        from_seq: u64,
    ) -> StoreResult<Vec<JournalRecord>> {
        self.ensure_directory_is_not_another_scopes(key)?;
        Ok(self
            .read_journal_file(key)?
            .all_records()
            .iter()
            .filter(|record| record.seq >= from_seq)
            .cloned()
            .collect())
    }

    /// The whole log as a [`Journal`], with **no copy and no second walk**.
    ///
    /// # What it removes, since the version above is still right there
    ///
    /// [`Self::read_journal_file`] already returns an owned `Journal` whose
    /// records are built. The trait's default then took them out by value,
    /// deep-copied every one, and handed the copy back to
    /// [`Journal::from_records`] — which re-checked the record count, the seq
    /// contiguity and `check_record_bounds` that [`Journal::parse`] had just
    /// checked over the same records, at the same place, against the same
    /// constants. This returns the journal the parse produced.
    ///
    /// The copy was not incidental. A `JournalBody::Event` payload is a
    /// `serde_json::Value`, so cloning it re-allocates every string and every
    /// map in it; the journalled `AgenticActionExecuted` carries a tool result up
    /// to `MAX_JOURNAL_RECORD_BYTES`. Peak memory for the read was two full
    /// copies of a log bounded only by [`MAX_JOURNAL_BYTES`].
    ///
    /// # The dropped check is dropped for THIS store and no other
    ///
    /// [`Journal::parse`] enforces a strict superset of [`Journal::from_records`]
    /// over the records it produces — the same
    /// `records.len() >= MAX_JOURNAL_RECORDS`, the same `seq != index + 1` break,
    /// the same `check_record_bounds` bounds check, plus a per-line byte ceiling
    /// `from_records` has no equivalent for. So for records that came out of
    /// `parse`, `from_records` cannot answer anything `parse` did not already
    /// answer. That is a claim about this store's own read path, which is why it
    /// is made here and why the trait's default still runs the check for
    /// everyone else.
    ///
    /// # It is still scope-checked
    ///
    /// [`Self::ensure_directory_is_not_another_scopes`] first, exactly as the
    /// method above. `read_journal_file` does not check it — it goes straight to
    /// [`Self::journal_path`] — so an override that skipped this line would read
    /// another scope's log through a door the `Vec` path keeps shut.
    fn read_journal_verified_sync(&self, key: &ExecutionKey) -> StoreResult<Journal> {
        self.ensure_directory_is_not_another_scopes(key)?;
        self.read_journal_file(key)
    }

    fn replay_committed_journal_sync(
        &self,
        key: &ExecutionKey,
        watermark: u64,
    ) -> StoreResult<ReplayedCursor> {
        let _lock = self.lock_execution(key)?;
        self.ensure_directory_is_not_another_scopes(key)?;
        if let Some(index) = self.current_journal_index(key) {
            if index.committed_watermark == watermark && index.replayed.seq == watermark {
                return Ok(index.replayed);
            }
        }
        let journal = self.read_journal_file(key)?;
        if journal.last_seq() < watermark {
            return Err(StoreError::WatermarkAhead {
                watermark,
                last_seq: journal.last_seq(),
            });
        }
        Ok(replay(journal.authoritative(watermark))?)
    }

    fn read_journal_projection_sync(
        &self,
        key: &ExecutionKey,
        from_seq: u64,
        watermark: u64,
        require_complete_history: bool,
    ) -> StoreResult<Vec<JournalRecord>> {
        let _lock = self.lock_execution(key)?;
        self.ensure_directory_is_not_another_scopes(key)?;
        if !require_complete_history {
            if let Some(index) = self.current_journal_index(key) {
                if index.committed_watermark == watermark && index.last_seq >= watermark {
                    if from_seq > watermark {
                        return Ok(Vec::new());
                    }
                    if let Some(mut position) = index
                        .offsets
                        .iter()
                        .position(|offset| offset.seq == from_seq)
                    {
                        while position > 0 && !index.offsets[position].batch_start {
                            position -= 1;
                        }
                        if index.offsets[position].batch_start {
                            if let Some(mut records) = self.read_indexed_suffix(
                                key,
                                &index.offsets[position],
                                &index.chain_sha256,
                            )? {
                                if records.last().map_or(0, |record| record.seq) < watermark {
                                    return Err(StoreError::WatermarkAhead {
                                        watermark,
                                        last_seq: records.last().map_or(0, |record| record.seq),
                                    });
                                }
                                records.truncate(
                                    records.partition_point(|record| record.seq <= watermark),
                                );
                                return Ok(records);
                            }
                        }
                    }
                }
            }
        }
        let journal = self.read_journal_file(key)?;
        if journal.last_seq() < watermark {
            return Err(StoreError::WatermarkAhead {
                watermark,
                last_seq: journal.last_seq(),
            });
        }
        let mut records = journal.into_records();
        records.truncate(records.partition_point(|record| record.seq <= watermark));
        Ok(records)
    }

    fn record_effect_intent_sync(
        &self,
        key: &ExecutionKey,
        entry: &EffectLedgerEntry,
    ) -> StoreResult<()> {
        create_dir_all_durably(&self.effects_dir(key)).map_err(|error| io_error(key, error))?;
        // Stamped for the same reason as an append: an intent can be the first
        // thing an execution writes, and a ledger two scopes share is two runs
        // reading each other's dispositions.
        self.ensure_key_record(key)?;
        // The conflict rules live on `EffectLedger` and are applied here rather
        // than restated: a second copy of "may this intent replace that one"
        // would be a second answer to a security-relevant question.
        let existing = self.read_effect(key, &entry.effect_id)?;
        // A NEW row past the limit is a row [`Self::load_effects`] would then
        // refuse for the rest of the execution's life: the ledger is assembled
        // from every file in the directory, so one file too many makes the whole
        // ledger unloadable and every disposition unanswerable. Re-committing an
        // intent that already has a row is not a new row and is not refused.
        if existing.is_none() && self.effects_at_capacity(key, MAX_LEDGER_ENTRIES)? {
            return Err(StoreError::Effect(EffectError::LedgerFull {
                limit: MAX_LEDGER_ENTRIES,
            }));
        }
        let mut ledger = EffectLedger::from_entries(existing)?;
        ledger.record_intent(entry.clone())?;
        let stored = ledger
            .get(&entry.effect_id)
            .expect("record_intent leaves the row present")
            .clone();
        self.write_effect(key, &stored)
    }

    fn record_effect_intent_fenced_sync(
        &self,
        key: &ExecutionKey,
        entry: &EffectLedgerEntry,
        lease: &Lease,
    ) -> StoreResult<()> {
        let _lock = self.lock_execution(key)?;
        self.validate_current_lease(key, lease)?;
        self.record_effect_intent_sync(key, entry)
    }

    fn record_effect_outcome_sync(
        &self,
        key: &ExecutionKey,
        effect_id: &EffectId,
        outcome: EffectOutcome,
    ) -> StoreResult<()> {
        self.ensure_directory_is_not_another_scopes(key)?;
        let existing = self.read_effect(key, effect_id)?;
        let mut ledger = EffectLedger::from_entries(existing)?;
        ledger.record_outcome(effect_id, outcome)?;
        let stored = ledger
            .get(effect_id)
            .expect("record_outcome leaves the row present")
            .clone();
        self.write_effect(key, &stored)
    }

    fn record_effect_outcome_fenced_sync(
        &self,
        key: &ExecutionKey,
        effect_id: &EffectId,
        outcome: EffectOutcome,
        lease: &Lease,
    ) -> StoreResult<()> {
        let _lock = self.lock_execution(key)?;
        self.validate_current_lease(key, lease)?;
        self.record_effect_outcome_sync(key, effect_id, outcome)
    }

    fn load_effects_sync(&self, key: &ExecutionKey) -> StoreResult<EffectLedger> {
        self.ensure_directory_is_not_another_scopes(key)?;
        let dir = self.effects_dir(key);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(EffectLedger::new())
            },
            Err(error) => return Err(io_error(key, error)),
        };

        let mut rows: Vec<EffectLedgerEntry> = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| io_error(key, error))?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            if rows.len() >= MAX_LEDGER_ENTRIES {
                return Err(StoreError::Effect(EffectError::LedgerFull {
                    limit: MAX_LEDGER_ENTRIES,
                }));
            }
            let row: Option<EffectLedgerEntry> =
                read_bounded_json(&path, MAX_EFFECT_BYTES, MAX_EFFECT_JSON_NODES)
                    .map_err(|error| corrupt(key, error))?;
            // `None` here means the file vanished between the directory listing
            // and the read — a concurrent prune, not an empty row. Skipping is
            // correct; treating a read *error* the same way would not be, which
            // is why the two are different arms.
            if let Some(row) = row {
                rows.push(row);
            }
        }
        Ok(EffectLedger::from_entries(rows)?)
    }

    fn claim_sync(
        &self,
        key: &ExecutionKey,
        worker: &WorkerId,
        ttl: Duration,
    ) -> StoreResult<Lease> {
        let _lock = self.lock_execution(key)?;
        // `lock_execution` created the directory durably and stamped its owner
        // before taking the lock. A claim that succeeded in a neighbouring
        // scope's directory would take that run's lease, block its real holder,
        // and reap its temporaries — and `reap_leaked_temporaries` is licensed
        // by exactly this lease.
        let now_ms = Utc::now().timestamp_millis();
        let mut recovery_adoption_authorized = false;
        if let Some(committed) = self.load_sync(key)? {
            if let Some(record) = self.base_binding_record_for_state(key, &committed.state)? {
                let binding_dir = self.base_binding_dir(
                    key.principal(),
                    key.workspace(),
                    &record.base_execution_id,
                );
                if binding_dir.exists() {
                    let _catalog = acquire_private_catalog_lock(&binding_dir.join("catalog.lock"))
                        .map_err(|error| StoreError::Unavailable {
                            detail: format!("base recovery admission claim lock failed: {error}"),
                        })?;
                    let admission_path = binding_dir.join("preseed.admission");
                    let admission: Option<BasePreseedAdmission> = read_bounded_json(
                        &admission_path,
                        MAX_BASE_BINDING_BYTES,
                        MAX_BASE_BINDING_JSON_NODES,
                    )
                    .map_err(|error| StoreError::Unavailable {
                        detail: format!("base recovery admission claim read failed: {error}"),
                    })?;
                    if let Some(admission) = admission {
                        if !valid_base_preseed_admission(
                            &admission,
                            key.principal(),
                            key.workspace(),
                            &record.base_execution_id,
                        ) {
                            return Err(StoreError::Unavailable {
                                detail: "base recovery admission is invalid during claim"
                                    .to_owned(),
                            });
                        }
                        if admission.expires_at_ms > now_ms {
                            self.reconcile_base_binding_head_locked(
                                key.principal(),
                                key.workspace(),
                                &record.base_execution_id,
                            )?;
                            if self.base_recovery_admission_consumed_locked(
                                key.principal(),
                                key.workspace(),
                                &record.base_execution_id,
                                &admission,
                            )? {
                                if let Err(error) =
                                    retire_base_recovery_admission_file_locked(&admission_path)
                                {
                                    warn!(
                                        execution = %key,
                                        %error,
                                        "[LOOP-STORE] consumed recovery admission could not be retired during claim; deferring without advancing"
                                    );
                                    return Err(StoreError::LeaseHeld {
                                        by: crate::magician_v2::execution::agentic::run_loop::state::recovery_admission_worker_id(
                                            &admission.token,
                                        ),
                                        until_ms: admission
                                            .expires_at_ms
                                            .min(now_ms.saturating_add(1_000)),
                                    });
                                }
                            } else {
                                let admitted = crate::magician_v2::execution::agentic::run_loop::state::recovery_admission_worker_id(
                                    &admission.token,
                                );
                                let head_matches = self
                                    .read_base_binding_head_sync(
                                        key.principal(),
                                        key.workspace(),
                                        &record.base_execution_id,
                                    )?
                                    .is_some_and(|head| {
                                        head.integrity_sha256 == admission.base_binding_head_sha256
                                    });
                                let exact_adoption = admission.schema_version
                                    == BASE_PRESEED_ADMISSION_SCHEMA_VERSION
                                    && head_matches
                                    && admission.exact_segment.as_deref()
                                        == Some(key.execution_id())
                                    && admission.exact_revision == Some(committed.revision)
                                    && worker == &admitted;
                                if !exact_adoption {
                                    return Err(StoreError::LeaseHeld {
                                        by: admitted,
                                        until_ms: admission.expires_at_ms,
                                    });
                                }
                                recovery_adoption_authorized = true;
                            }
                        }
                    }
                }
            }
        }
        let latest = self.latest_lease(key)?;
        if let Some(held) = &latest {
            // A live lease excludes every ordinary acquisition, including one
            // presenting the same worker id. The sole exception is the exact
            // recovery worker derived from the still-live, token-bound base
            // admission above: service preclaims that fence before publishing
            // shared controls, and the arm adopts the same fence. Prefix-shaped
            // worker ids without that exact admission proof remain excluded.
            if !held.released && held.expires_at_ms > now_ms {
                if &held.worker == worker && recovery_adoption_authorized {
                    return Ok(Lease {
                        key: key.clone(),
                        worker: worker.clone(),
                        fence: held.fence,
                        expires_at_ms: held.expires_at_ms,
                    });
                }
                return Err(StoreError::LeaseHeld {
                    by: held.worker.clone(),
                    until_ms: held.expires_at_ms,
                });
            }
        }

        let fence = latest.as_ref().map_or(0, |held| held.fence) + 1;
        let stored = StoredLease {
            worker: worker.clone(),
            fence,
            expires_at_ms: now_ms.saturating_add(ttl_millis(ttl)),
            released: false,
        };
        match self.publish_lease(key, &stored)? {
            Publish::Published => {
                // This worker now holds the lease at a fence nobody else has.
                // Everything that writes into this execution's own directory or
                // its effects directory is required to hold it, so any temporary
                // still sitting there belongs to a publish that died between
                // creating one and linking it into place.
                reap_leaked_temporaries(&self.execution_dir(key));
                reap_leaked_temporaries(&self.effects_dir(key));
                // Under the same licence, migrate a legacy terminal snapshot
                // that predates required prepublication of `ended.json`, or
                // repair a marker an operator removed. Current commits cannot
                // create this gap. It never fails this call.
                //
                // AFTER the reaping, not before: the repair publishes through a
                // temporary sibling, and reaping is what clears the ones an
                // earlier death left behind.
                self.repair_ending(key);
                Ok(Lease {
                    key: key.clone(),
                    worker: worker.clone(),
                    fence,
                    expires_at_ms: stored.expires_at_ms,
                })
            },
            // Another worker took this fence between the read and the link. Name
            // whoever actually holds it rather than guessing.
            Publish::AlreadyExists => {
                let holder = self.latest_lease(key)?;
                Err(StoreError::LeaseHeld {
                    by: holder
                        .as_ref()
                        .map(|held| held.worker.clone())
                        .unwrap_or_else(|| WorkerId::new("unknown")),
                    until_ms: holder.map_or(now_ms, |held| held.expires_at_ms),
                })
            },
        }
    }

    fn renew_sync(&self, lease: &Lease, ttl: Duration) -> StoreResult<Lease> {
        let _lock = self.lock_execution(&lease.key)?;
        // A `Lease` has public fields, so holding one is not proof it came from
        // `claim`. The fence and worker checks below make a forgery a guess, but
        // the scope boundary should not rest on that.
        self.ensure_directory_is_not_another_scopes(&lease.key)?;
        let now_ms = Utc::now().timestamp_millis();
        let latest = self.latest_lease(&lease.key)?;
        // Renewing an *expired but uncontested* lease is allowed, and the fence
        // is what makes that safe: any takeover publishes a higher fence, so a
        // holder whose fence is still current is a holder nobody has replaced.
        // Refusing here would push every heartbeat into a claim loop for no
        // additional protection.
        let current = latest.ok_or(StoreError::LeaseLost { fence: lease.fence })?;
        if current.released || current.fence != lease.fence || current.worker != lease.worker {
            return Err(StoreError::LeaseLost { fence: lease.fence });
        }

        let fence = current.fence + 1;
        let stored = StoredLease {
            worker: lease.worker.clone(),
            fence,
            expires_at_ms: now_ms.saturating_add(ttl_millis(ttl)),
            released: false,
        };
        match self.publish_lease(&lease.key, &stored)? {
            Publish::Published => Ok(Lease {
                key: lease.key.clone(),
                worker: lease.worker.clone(),
                fence,
                expires_at_ms: stored.expires_at_ms,
            }),
            Publish::AlreadyExists => Err(StoreError::LeaseLost { fence: lease.fence }),
        }
    }

    fn release_sync(&self, lease: Lease) -> StoreResult<()> {
        let _lock = self.lock_execution(&lease.key)?;
        self.ensure_directory_is_not_another_scopes(&lease.key)?;
        let latest = self.latest_lease(&lease.key)?;
        let current = latest.ok_or(StoreError::LeaseLost { fence: lease.fence })?;
        if current.released || current.fence != lease.fence || current.worker != lease.worker {
            return Err(StoreError::LeaseLost { fence: lease.fence });
        }
        let stored = StoredLease {
            worker: lease.worker.clone(),
            fence: current.fence + 1,
            expires_at_ms: 0,
            released: true,
        };
        match self.publish_lease(&lease.key, &stored)? {
            Publish::Published => Ok(()),
            Publish::AlreadyExists => Err(StoreError::LeaseLost { fence: lease.fence }),
        }
    }

    fn resolve_wake_sync(
        &self,
        key: &ExecutionKey,
        wake_token: &str,
        resolution_id: &str,
    ) -> StoreResult<()> {
        // Checked, never stamped. A completer may resolve BEFORE the parent has
        // written anything at all — `a_resolution_that_arrives_before_the_park
        // _still_satisfies_it` is that case — so an absent key record is normal
        // here and stamping one would make a job runner the owner of a directory
        // it does not run.
        self.ensure_directory_is_not_another_scopes(key)?;
        // Bounded before writing, so a completer reporting without bound is
        // refused rather than absorbed. Checked on the way in because the read
        // is what the bound protects, and a directory already over it would make
        // every later listing fail instead of this call.
        let path = self.wake_resolution_path(key, wake_token, resolution_id);
        // Capacity admission and publication are one token-local critical
        // section. Without it, N concurrent completers can all observe N slots
        // free and publish past the bound that every later read enforces.
        let _lock = self.lock_wake_token(key, wake_token)?;
        reap_leaked_temporaries(&self.wake_dir(key, wake_token));
        let outstanding = self.wake_resolution_paths_unlocked(key, wake_token)?;
        // A re-delivery of something already recorded is admitted whatever the
        // count says: it adds nothing, and refusing it would turn a bound on a
        // misbehaving completer into a failure for a well-behaved one.
        if outstanding.len() >= MAX_WAKE_RESOLUTIONS && !outstanding.contains(&path) {
            return Err(StoreError::WakeLedgerFull {
                wake_token: wake_token.to_string(),
                limit: MAX_WAKE_RESOLUTIONS,
            });
        }

        let encoded = serde_json::to_vec(&StoredWakeResolution {
            wake_token: wake_token.to_string(),
            resolution_id: resolution_id.to_string(),
        })
        .map_err(|error| StoreError::Corrupt {
            key: key.clone(),
            detail: error.to_string(),
        })?;
        validate_json_bytes(&encoded, MAX_WAKE_BYTES, MAX_WAKE_JSON_NODES)
            .map_err(|error| corrupt(key, error))?;
        // Idempotent on `(wake_token, resolution_id)`: a completer that reports
        // the same completion twice — a retried job, a re-delivered signal —
        // takes the `AlreadyExists` arm and adds nothing. Two DIFFERENT
        // completions are two files, which is the case a single marker used to
        // collapse.
        match publish_new_file(&path, &encoded).map_err(|error| io_error(key, error))? {
            Publish::Published => Ok(()),
            Publish::AlreadyExists => match self.read_wake_resolution(key, wake_token, &path)? {
                Some(existing) if existing.resolution_id.as_str() == resolution_id => Ok(()),
                Some(_) | None => Err(StoreError::Corrupt {
                    key: key.clone(),
                    detail: format!(
                        "{} already exists but is not the exact wake resolution being retried",
                        path.display()
                    ),
                }),
            },
        }
    }

    fn wake_resolutions_sync(
        &self,
        key: &ExecutionKey,
        wake_token: &str,
    ) -> StoreResult<Vec<String>> {
        self.ensure_directory_is_not_another_scopes(key)?;
        self.read_wake_resolutions(key, wake_token)
    }

    fn consume_wake_sync(
        &self,
        key: &ExecutionKey,
        wake_token: &str,
        resolution_ids: &[String],
    ) -> StoreResult<usize> {
        self.ensure_directory_is_not_another_scopes(key)?;
        match std::fs::metadata(self.wake_dir(key, wake_token)) {
            Ok(metadata) if metadata.is_dir() => {},
            Ok(_) => {
                return Err(StoreError::Corrupt {
                    key: key.clone(),
                    detail: "the wake token path is not a directory".to_string(),
                })
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(io_error(key, error)),
        }
        let _lock = self.lock_wake_token(key, wake_token)?;
        reap_leaked_temporaries(&self.wake_dir(key, wake_token));
        let mut removed = 0usize;
        for resolution_id in resolution_ids {
            let path = self.wake_resolution_path(key, wake_token, resolution_id);
            match std::fs::remove_file(&path) {
                Ok(()) => removed += 1,
                // Already gone. A retried boundary consuming twice is expected,
                // not an error — which is why this returns a count rather than
                // refusing.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(error) => return Err(io_error(key, error)),
            }
        }
        if removed > 0 {
            // `remove_file` only changes the directory entry. Without syncing
            // the token directory, a power loss after this method returned can
            // resurrect a resolution the caller has already acted on. That is
            // more than harmless litter: a later park on the same token would
            // observe the old completion as fresh and leave early. Publication
            // already fsyncs this directory through `publish_new_file`; consume
            // owes the symmetric durability boundary.
            File::open(self.wake_dir(key, wake_token))
                .and_then(|directory| directory.sync_all())
                .map_err(|error| io_error(key, error))?;
        }
        Ok(removed)
    }

    fn consume_wake_fenced_sync(
        &self,
        key: &ExecutionKey,
        wake_token: &str,
        resolution_ids: &[String],
        lease: &Lease,
    ) -> StoreResult<usize> {
        let _lock = self.lock_execution(key)?;
        self.validate_current_lease(key, lease)?;
        self.consume_wake_sync(key, wake_token, resolution_ids)
    }

    fn list_runnable_sync(
        &self,
        worker: &WorkerId,
        limit: usize,
    ) -> StoreResult<Vec<ExecutionKey>> {
        // One walk, not two. A second copy of the filter is how the paged answer
        // and the unpaged one come to disagree about what "runnable" means, and
        // the disagreement would show as a key one caller can see and another
        // cannot — which no test asks about, because nobody expects two answers
        // to one question.
        Ok(self.scan_runnable_sync(worker, limit, None)?.keys)
    }

    fn scan_runnable_sync(
        &self,
        worker: &WorkerId,
        limit: usize,
        after: Option<&ScanCursor>,
    ) -> StoreResult<RunnableScan> {
        let now_ms = Utc::now().timestamp_millis();
        let mut runnable = Vec::new();
        let mut visits = 0usize;
        // The last execution this page **examined**, offered or not. It has to
        // be that rather than the last key offered, because the scan ceiling can
        // fire having offered nothing at all — and a page that could not name
        // where it stopped would leave everything behind that point unreachable,
        // which is the defect this method exists to remove rather than move.
        let mut last_visited: Option<ScanCursor> = None;
        let walk = self.walk_execution_dirs(after.map(ScanCursor::segments), |dir, at| {
            if runnable.len() >= limit || visits >= MAX_DISCOVERY_VISITS_PER_PAGE {
                // Not examined, so `last_visited` is still the previous entry
                // and resuming after it brings this one back.
                //
                // Unless there IS no previous entry, which happens for exactly
                // one input: a `limit` of zero, which fills the page before the
                // first entry is looked at. A page that named nothing would have
                // to hand back the cursor it arrived with, and a caller paging
                // in a loop would ask the same question forever. So a page that
                // has examined nothing names the entry it stopped ON: the next
                // page resumes past it, which loses nothing because a scan
                // asked for zero keys offers nothing wherever it stops, and the
                // loop terminates.
                if last_visited.is_none() {
                    last_visited = Some(at.to_cursor());
                }
                return Walk::Stop;
            }
            visits += 1;
            // Set BEFORE the read below, so an execution this pass could not
            // read is still a position the next page starts after. A cursor that
            // only advanced past readable entries would stop dead on the first
            // corrupt directory and re-report it forever.
            last_visited = Some(at.to_cursor());
            match self.runnable_at(dir, worker, now_ms) {
                Ok(Some(key)) => runnable.push(key),
                Ok(None) => {},
                // One unreadable execution must not hide every other one, and it
                // must not disappear silently either: it is quarantined from
                // scheduling and said so, loudly.
                Err(error) => warn!(
                    path = %dir.display(),
                    %error,
                    "[LOOP-STORE] an execution is unreadable and is excluded from scheduling \
                     until an operator looks at it"
                ),
            }
            Walk::Continue
        })?;
        // An unclassified skip is a run that may never be offered, and no cursor
        // reaches it: the next page meets the same refusal at the same entry. It
        // is a warning here and a coverage flag on the reconciler's listing,
        // which is the caller that can act on it.
        if walk.unclassified {
            warn!(
                "[LOOP-STORE] a scan skipped an entry it could not classify; any executions \
                 under it are not being scheduled"
            );
        }
        Ok(RunnableScan {
            keys: runnable,
            resume: if !walk.stopped_short {
                None
            } else {
                // The EARLIEST of the two things this pass can name: where it
                // stopped, and where it knows it left a level unfinished. A cut
                // above the leaf sits before the entry the page stopped on, and
                // resuming from the later of the two would step over everything
                // between them.
                //
                // Both are strictly after the cursor this page started from, so
                // the next page is a different page — which is the property that
                // makes a caller's paging loop terminate.
                let earliest = [last_visited, walk.cut_at.map(ScanCursor::from_segments)]
                    .into_iter()
                    .flatten()
                    .min();
                if earliest.is_none() {
                    // Not reachable: every stop above names a position. If a
                    // later edit makes it reachable, this page ENDS rather than
                    // repeating itself — an under-reported pass is recovered by
                    // the next poll, which starts from the beginning again, and
                    // a page that hands back its own cursor is recovered by
                    // nothing.
                    warn!(
                        "[LOOP-STORE] a scan stopped short of the store and could not say \
                         where to carry on, so this page ends here"
                    );
                }
                earliest
            },
        })
    }

    fn scan_terminal_outbox_debt_sync(
        &self,
        max_visits: NonZeroUsize,
        after: Option<&ScanCursor>,
    ) -> StoreResult<TerminalOutboxScan> {
        let now_ms = Utc::now().timestamp_millis();
        let priority_after = after.and_then(terminal_debt_priority_cursor_digest);
        let catalog = self.load_terminal_debt_catalog(now_ms);
        let catalog_ready = catalog
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .is_some_and(|catalog| {
                catalog.complete
                    && !catalog.overflowed
                    && !self.terminal_debt_catalog_is_dirty_or_unreadable()
                    && now_ms.saturating_sub(catalog.last_authoritative_scan_ms)
                        < TERMINAL_DEBT_AUTHORITATIVE_AUDIT_MS
            });

        if priority_after.is_some() || (after.is_none() && catalog_ready) {
            if !self.terminal_debt_catalog_is_dirty_or_unreadable() {
                if let Ok(Some(ref catalog)) = catalog {
                    return self.scan_terminal_debt_catalog_sync(
                        max_visits,
                        priority_after,
                        &catalog,
                        now_ms,
                    );
                }
            }
            // A cursor into a derivative cannot safely be translated after the
            // derivative disappears or becomes corrupt. Restart the bounded
            // authoritative walk; exact projector receipts make duplicates
            // harmless, while continuing from an invented position could hide
            // debt.
            warn!(
                "[LOOP_OUTBOX] the terminal-debt priority catalog became unreadable while paging; restarting authoritative discovery"
            );
            self.prepare_terminal_debt_catalog_rebuild(now_ms);
            return self.scan_terminal_outbox_debt_authoritatively_sync(max_visits, None, now_ms);
        }

        if after.is_none() {
            if let Err(ref error) = catalog {
                warn!(
                    %error,
                    "[LOOP_OUTBOX] terminal-debt priority catalog is missing or invalid; rebuilding from authoritative execution directories"
                );
            }
            self.prepare_terminal_debt_catalog_rebuild(now_ms);
        }
        self.scan_terminal_outbox_debt_authoritatively_sync(max_visits, after, now_ms)
    }

    fn scan_terminal_debt_catalog_sync(
        &self,
        max_visits: NonZeroUsize,
        after_digest: Option<&str>,
        catalog: &TerminalDebtCatalog,
        now_ms: i64,
    ) -> StoreResult<TerminalOutboxScan> {
        let mut keys = Vec::new();
        let mut visits = 0usize;
        let mut last_visited: Option<String> = None;
        let mut has_more = false;
        let mut stale = Vec::new();
        for (digest, entry) in catalog.entries.iter() {
            if after_digest.is_some_and(|after| digest.as_str() <= after) {
                continue;
            }
            if visits >= max_visits.get() {
                has_more = true;
                break;
            }
            visits += 1;
            last_visited = Some(digest.clone());
            let key = ExecutionKey::new(
                entry.principal.clone(),
                entry.workspace.clone(),
                entry.execution_id.clone(),
            )
            .map_err(|error| StoreError::Unavailable {
                detail: format!("terminal-debt catalog key is invalid: {error}"),
            })?;
            let current = match self.latest_revision(&key) {
                Ok(current) => current,
                Err(error) => {
                    warn!(
                        execution = %key,
                        %error,
                        "[LOOP_OUTBOX] a priority candidate is unreadable and remains queued for a later exact validation"
                    );
                    continue;
                },
            };
            if current < entry.observed_revision {
                // A terminal commit publishes before its snapshot CAS so the
                // crash window cannot lose discovery. Do not let a scanner
                // remove that future marker while the commit is in flight.
                if now_ms.saturating_sub(entry.observed_at_ms)
                    >= TERMINAL_DEBT_FUTURE_MARKER_GRACE_MS
                {
                    stale.push((digest.clone(), entry.clone()));
                }
                continue;
            }
            match self.terminal_outbox_debt_at(&self.execution_dir(&key)) {
                Ok(Some(exact)) => keys.push(exact),
                Ok(None) => stale.push((digest.clone(), entry.clone())),
                Err(error) => warn!(
                    execution = %key,
                    %error,
                    "[LOOP_OUTBOX] a priority candidate failed exact validation and remains queued"
                ),
            }
        }
        for (digest, entry) in stale {
            if let Err(error) = self.remove_terminal_debt_candidate_if_unchanged(&digest, &entry) {
                warn!(
                    %error,
                    "[LOOP_OUTBOX] stale terminal-debt priority cleanup will retry"
                );
            }
        }
        Ok(TerminalOutboxScan {
            keys,
            resume: has_more.then(|| {
                terminal_debt_priority_cursor(
                    last_visited.expect("a non-zero page with more catalog rows visited one row"),
                )
            }),
        })
    }

    fn prepare_terminal_debt_catalog_rebuild(&self, now_ms: i64) {
        // First discard exact stale rows from the bounded old priority set. This
        // keeps historical entries from consuming all capacity during the new
        // authoritative pass. Future pre-CAS markers are retained, which closes
        // the only race where a rebuild could otherwise erase a terminal commit
        // between marker publication and snapshot publication.
        if let Ok(Some(catalog)) = self.load_terminal_debt_catalog(now_ms) {
            let mut stale = Vec::new();
            for (digest, entry) in &catalog.entries {
                let Ok(key) = ExecutionKey::new(
                    entry.principal.clone(),
                    entry.workspace.clone(),
                    entry.execution_id.clone(),
                ) else {
                    continue;
                };
                let Ok(current) = self.latest_revision(&key) else {
                    continue;
                };
                if current < entry.observed_revision
                    && now_ms.saturating_sub(entry.observed_at_ms)
                        < TERMINAL_DEBT_FUTURE_MARKER_GRACE_MS
                {
                    continue;
                }
                if self
                    .terminal_outbox_debt_at(&self.execution_dir(&key))
                    .is_ok_and(|debt| debt.is_none())
                {
                    stale.push((digest.clone(), entry.clone()));
                }
            }
            for (digest, entry) in stale {
                let _ = self.remove_terminal_debt_candidate_if_unchanged(&digest, &entry);
            }
        }
        if let Err(error) = self.mutate_terminal_debt_catalog(|catalog| {
            catalog.complete = false;
            catalog.overflowed = false;
            catalog.rebuild_had_errors = false;
            catalog.rebuild_dirty_generation = self.terminal_debt_dirty_generation();
        }) {
            warn!(
                %error,
                "[LOOP_OUTBOX] terminal-debt catalog rebuild could not publish its incomplete marker"
            );
        }
    }

    fn scan_terminal_outbox_debt_authoritatively_sync(
        &self,
        max_visits: NonZeroUsize,
        after: Option<&ScanCursor>,
        now_ms: i64,
    ) -> StoreResult<TerminalOutboxScan> {
        let mut keys = Vec::new();
        let mut visits = 0usize;
        let mut last_visited: Option<ScanCursor> = None;
        let mut observations: Vec<(ExecutionKey, Revision, bool)> = Vec::new();
        let mut unreadable = false;
        let walk = self.walk_execution_dirs(after.map(ScanCursor::segments), |dir, at| {
            if visits >= max_visits.get() {
                return Walk::Stop;
            }
            visits += 1;
            last_visited = Some(at.to_cursor());
            let observed_key = self.key_at(dir);
            match (observed_key, self.terminal_outbox_debt_at(dir)) {
                (Ok(Some(key)), Ok(Some(exact))) => {
                    match self.latest_revision(&key) {
                        Ok(revision) => observations.push((key, revision, true)),
                        Err(_) => unreadable = true,
                    }
                    keys.push(exact);
                },
                (Ok(Some(key)), Ok(None)) => match self.latest_revision(&key) {
                    Ok(revision) => observations.push((key, revision, false)),
                    Err(_) => unreadable = true,
                },
                (Ok(None), Ok(None)) => {},
                (key_result, debt_result) => {
                    unreadable = true;
                    let detail = key_result
                        .err()
                        .map(|error| error.to_string())
                        .or_else(|| debt_result.err().map(|error| error.to_string()))
                        .unwrap_or_else(|| "terminal debt classification was inconsistent".to_owned());
                    warn!(
                        path = %dir.display(),
                        error = %detail,
                        "[LOOP_OUTBOX] an execution is unreadable and is excluded from terminal projection until an operator looks at it"
                    );
                },
            }
            Walk::Continue
        })?;
        if walk.unclassified {
            warn!(
                "[LOOP_OUTBOX] a terminal-debt scan skipped an entry it could not classify; \
                 terminal events under it may remain unprojected"
            );
        }
        let resume = if !walk.stopped_short {
            None
        } else {
            [last_visited, walk.cut_at.map(ScanCursor::from_segments)]
                .into_iter()
                .flatten()
                .min()
        };
        let completed = resume.is_none();
        let continuation_catalog_missing = after.is_some()
            && !matches!(
                self.load_terminal_debt_catalog(now_ms),
                Ok(Some(TerminalDebtCatalog {
                    complete: false,
                    ..
                }))
            );
        let settled_hints = observations
            .iter()
            .filter_map(|(key, revision, has_debt)| (!has_debt).then_some((key.clone(), *revision)))
            .collect::<Vec<_>>();
        let mut catalog_completed = false;
        let mut completed_dirty_generation = None;
        if let Err(error) =
            self.mutate_terminal_debt_catalog_with_durability(completed, |catalog| {
                for (key, revision, has_debt) in observations {
                    let probe = TerminalDebtCatalogEntry {
                        principal: key.principal().to_owned(),
                        workspace: key.workspace().to_owned(),
                        execution_id: key.execution_id().to_owned(),
                        observed_revision: revision,
                        observed_at_ms: now_ms,
                    };
                    let digest = terminal_debt_entry_digest(&probe);
                    if has_debt {
                        if catalog.entries.contains_key(&digest)
                            || catalog.entries.len() < MAX_TERMINAL_DEBT_CATALOG_ENTRIES
                        {
                            catalog.entries.insert(digest, probe);
                        } else {
                            catalog.overflowed = true;
                        }
                    } else if catalog
                        .entries
                        .get(&digest)
                        .is_some_and(|entry| entry.observed_revision <= revision)
                    {
                        catalog.entries.remove(&digest);
                    }
                }
                catalog.rebuild_had_errors |=
                    unreadable || walk.unclassified || continuation_catalog_missing;
                if completed {
                    if self.terminal_debt_dirty_generation() != catalog.rebuild_dirty_generation {
                        catalog.rebuild_had_errors = true;
                    }
                    catalog.last_authoritative_scan_ms = now_ms;
                    catalog.complete = !catalog.overflowed && !catalog.rebuild_had_errors;
                    catalog_completed = catalog.complete;
                    completed_dirty_generation = catalog.rebuild_dirty_generation.clone();
                } else {
                    catalog.complete = false;
                }
            })
        {
            warn!(
                %error,
                "[LOOP_OUTBOX] authoritative terminal-debt results could not repair the derived priority catalog"
            );
            let _ = self.mark_terminal_debt_catalog_dirty();
        }
        if catalog_completed {
            self.clear_terminal_debt_catalog_dirty_if(completed_dirty_generation.as_deref());
        }
        for (key, revision) in settled_hints {
            self.retire_terminal_debt_local_hint_if_settled_or_stale(&key, revision, now_ms);
        }
        Ok(TerminalOutboxScan { keys, resume })
    }

    fn list_parked_sync(&self, limit: usize) -> StoreResult<ParkedListing> {
        self.scan_parked_sync(limit, None)
    }

    fn scan_parked_sync(
        &self,
        limit: usize,
        after: Option<&ScanCursor>,
    ) -> StoreResult<ParkedListing> {
        // A zero-sized page cannot return a cursor for an unvisited key without
        // skipping that key on the next strict-after scan. Preserve boundedness
        // while guaranteeing progress.
        let limit = limit.max(1);
        let mut parked = Vec::new();
        let mut visits = 0usize;
        // An execution this pass could not read might be a parked one, so a skip
        // is a hole in the reconciler's coverage rather than one fewer row. It is
        // folded into the same flag as a filled `limit` because the consequence
        // is identical: something parked may not have been looked at.
        let mut unreadable = false;
        let mut last_visited = None;
        let walk = self.walk_execution_dirs(after.map(ScanCursor::segments), |dir, at| {
            if parked.len() >= limit || visits >= MAX_DISCOVERY_VISITS_PER_PAGE {
                if last_visited.is_none() {
                    last_visited = Some(at.to_cursor());
                }
                return Walk::Stop;
            }
            visits += 1;
            last_visited = Some(at.to_cursor());
            match self.parked_at(dir) {
                Ok(Some(found)) => parked.push(found),
                Ok(None) => {},
                Err(error) => {
                    unreadable = true;
                    warn!(
                        path = %dir.display(),
                        %error,
                        "[LOOP-STORE] an execution is unreadable and was not examined for an \
                         orphaned park"
                    );
                },
            }
            Walk::Continue
        })?;
        Ok(ParkedListing {
            parked,
            resume: if walk.stopped_short {
                [last_visited, walk.cut_at.map(ScanCursor::from_segments)]
                    .into_iter()
                    .flatten()
                    .min()
            } else {
                None
            },
            // `unclassified` belongs here for the same reason `unreadable` does:
            // an entry the walk could not identify may have been an execution
            // directory, and a pass that skipped one has not covered the store.
            // It is the flag rather than an error because the rest of the pass
            // is still worth reporting — and it is not silence, because a
            // reconciler reading `incomplete` is forbidden to call an empty
            // `findings` a clean sweep.
            incomplete: walk.stopped_short || walk.unclassified || unreadable,
        })
    }

    fn load_projector_cursor_sync(
        &self,
        key: &ExecutionKey,
    ) -> StoreResult<Option<ProjectorCursor>> {
        // The same directory-ownership check every other read of this execution
        // makes. A mark read out of a directory that normalises to another
        // scope's would let one run's emitted-addresses suppress another run's
        // events — which is a silent drop rather than a duplicate, and the wrong
        // direction to be wrong in.
        self.ensure_directory_is_not_another_scopes(key)?;
        // Deserializing goes through `ProjectorCursor`'s wire type, so a
        // hand-edited window is clamped and a hand-edited key list is bounded and
        // de-duplicated on the way in. That is the whole reason this returns the
        // parsed value rather than bytes.
        read_bounded_json(
            &self.projector_path(key),
            MAX_PROJECTOR_BYTES,
            MAX_PROJECTOR_JSON_NODES,
        )
        .map_err(|error| corrupt(key, error))
    }

    fn save_projector_cursor_sync(
        &self,
        key: &ExecutionKey,
        cursor: &ProjectorCursor,
    ) -> StoreResult<()> {
        // Stamped before the write, for the same reason an append is: a mark can
        // be the first thing written into a directory that a later commit will
        // claim, and an unstamped directory is one a second scope can normalise
        // onto.
        self.ensure_key_record(key)?;
        let bytes = serde_json::to_vec(cursor).map_err(|error| StoreError::Unavailable {
            detail: format!("{key}: the projector mark did not encode: {error}"),
        })?;
        validate_json_bytes(&bytes, MAX_PROJECTOR_BYTES, MAX_PROJECTOR_JSON_NODES).map_err(
            |error| StoreError::Unavailable {
                detail: format!("{key}: the projector mark exceeds its read limits: {error}"),
            },
        )?;
        // Refused at WRITE as well as at read. A mark this store wrote and cannot
        // read back is worse than one it refused to write: the refusal is visible
        // at the boundary that caused it, while the unreadable file stops the
        // run's outbox at some later boundary with nothing pointing back here.
        if bytes.len() as u64 > MAX_PROJECTOR_BYTES {
            return Err(StoreError::Unavailable {
                detail: format!(
                    "{key}: the projector mark is {} bytes, over the {MAX_PROJECTOR_BYTES}-byte \
                     limit this store can read back",
                    bytes.len()
                ),
            });
        }
        // `overwrite_atomically`, not `publish_new_file`: this is genuinely
        // last-writer-wins under the lease, and a `link(2)` publish would refuse
        // every save after the first.
        overwrite_atomically(&self.projector_path(key), &bytes)
            .map_err(|error| io_error(key, error))?;
        // Ordinary event projection must not serialize on one global catalog.
        // Terminal writers leave this tiny per-execution hint before their CAS,
        // so only a cursor that can retire terminal/receipt debt pays for the
        // exact validation and global catalog update.
        if self.terminal_debt_local_hint_path(key).exists() {
            let revision = self.latest_revision(key)?;
            if let Err(error) = self.refresh_terminal_debt_candidate(key, revision) {
                warn!(
                    execution = %key,
                    %error,
                    "[LOOP_OUTBOX] projector cursor advanced but its derived debt candidate could not be refreshed; exact validation/audit will repair it"
                );
            }
        }
        Ok(())
    }

    fn save_projector_cursor_fenced_sync(
        &self,
        key: &ExecutionKey,
        cursor: &ProjectorCursor,
        lease: &Lease,
    ) -> StoreResult<()> {
        let _lock = self.lock_execution(key)?;
        self.validate_current_lease(key, lease)?;
        self.save_projector_cursor_sync(key, cursor)
    }

    fn load_chain_closure_sync(&self, key: &ExecutionKey) -> StoreResult<Option<ChainClosure>> {
        // The same directory-ownership check every other read of this execution
        // makes, and here it is the one that matters most: a receipt recovered
        // out of a directory that normalises to another scope's would license a
        // permanent terminal on a run nobody asked about. `ChainClosure::closes`
        // is the reader's second check; this is the first.
        self.ensure_directory_is_not_another_scopes(key)?;
        read_bounded_json(
            &self.chain_closure_path(key),
            MAX_CHAIN_CLOSURE_BYTES,
            MAX_CHAIN_CLOSURE_JSON_NODES,
        )
        .map_err(|error| corrupt(key, error))
    }

    fn record_chain_closure_sync(
        &self,
        key: &ExecutionKey,
        closure: &ChainClosure,
    ) -> StoreResult<()> {
        // Stamped for the same reason a mark is: the receipt can be the last
        // thing written into a directory, and an unstamped directory is one a
        // second scope can normalise onto.
        self.ensure_key_record(key)?;
        let bytes = serde_json::to_vec(closure).map_err(|error| StoreError::Unavailable {
            detail: format!("{key}: the chain-closure receipt did not encode: {error}"),
        })?;
        // Refused at WRITE as well as at read, exactly as the projector mark is:
        // a receipt this store wrote and cannot read back would be an absent
        // receipt forever, and the refusal belongs at the boundary that caused
        // it rather than in a reconciler pass months later.
        validate_json_bytes(
            &bytes,
            MAX_CHAIN_CLOSURE_BYTES,
            MAX_CHAIN_CLOSURE_JSON_NODES,
        )
        .map_err(|error| StoreError::Unavailable {
            detail: format!("{key}: the chain-closure receipt exceeds its limits: {error}"),
        })?;
        // `overwrite_atomically`, not `publish_new_file`: a crash-retry
        // republishes the same value, and a `link(2)` publish would refuse it
        // and leave the caller logging a failure about a receipt that is
        // already on disk.
        overwrite_atomically(&self.chain_closure_path(key), &bytes)
            .map_err(|error| io_error(key, error))
    }
}

/// Keep synchronous filesystem work away from Tokio's async workers without
/// allowing an unbounded request burst to fill the blocking pool.
const MAX_BLOCKING_FS_OPERATIONS: usize = 32;
const MAX_BLOCKING_FS_CONTROL_OPERATIONS: usize = 64;

fn fs_io_permits() -> Arc<tokio::sync::Semaphore> {
    static PERMITS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    Arc::clone(
        PERMITS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(MAX_BLOCKING_FS_OPERATIONS))),
    )
}

fn fs_control_io_permits() -> Arc<tokio::sync::Semaphore> {
    static PERMITS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    Arc::clone(PERMITS.get_or_init(|| {
        Arc::new(tokio::sync::Semaphore::new(
            MAX_BLOCKING_FS_CONTROL_OPERATIONS,
        ))
    }))
}

async fn run_fs_io<T, F>(operation: F) -> StoreResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> StoreResult<T> + Send + 'static,
{
    let permit =
        fs_io_permits()
            .acquire_owned()
            .await
            .map_err(|error| StoreError::Unavailable {
                detail: format!("the filesystem operation gate closed: {error}"),
            })?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation()
    })
    .await
    .map_err(|error| StoreError::Unavailable {
        detail: format!("a filesystem operation task failed: {error}"),
    })?
}

/// A reserved lane for lease lifecycle calls. Long store scans must never fill
/// the ordinary I/O gate and prevent a live phase from polling its heartbeat;
/// that would turn local filesystem load into an avoidable lease takeover.
async fn run_fs_control_io<T, F>(operation: F) -> StoreResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> StoreResult<T> + Send + 'static,
{
    let permit = fs_control_io_permits()
        .acquire_owned()
        .await
        .map_err(|error| StoreError::Unavailable {
            detail: format!("the filesystem control-operation gate closed: {error}"),
        })?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation()
    })
    .await
    .map_err(|error| StoreError::Unavailable {
        detail: format!("a filesystem control-operation task failed: {error}"),
    })?
}

#[async_trait]
impl LoopStateStore for FsLoopStateStore {
    async fn load(&self, key: &ExecutionKey) -> StoreResult<Option<CommittedLoopState>> {
        let store = self.clone();
        let key = key.clone();
        run_fs_io(move || store.load_store_sync(&key)).await
    }

    async fn commit(
        &self,
        key: &ExecutionKey,
        state: &LoopState,
        expected: Revision,
    ) -> StoreResult<Revision> {
        let store = self.clone();
        let key = key.clone();
        let state = state.clone();
        run_fs_io(move || store.commit_store_sync(&key, &state, expected)).await
    }

    async fn commit_fenced(
        &self,
        key: &ExecutionKey,
        state: &LoopState,
        expected: Revision,
        lease: &Lease,
    ) -> StoreResult<Revision> {
        let store = self.clone();
        let key = key.clone();
        let state = state.clone();
        let lease = lease.clone();
        // Lock, lease validation and publication all run in this one closure;
        // no async suspension can split the fenced critical section.
        run_fs_io(move || store.commit_fenced_sync(&key, &state, expected, &lease)).await
    }

    async fn append_journal(
        &self,
        key: &ExecutionKey,
        appends: &[JournalAppend],
    ) -> StoreResult<u64> {
        let store = self.clone();
        let key = key.clone();
        let appends = appends.to_vec();
        run_fs_io(move || store.append_journal_sync(&key, &appends)).await
    }

    async fn append_journal_fenced(
        &self,
        key: &ExecutionKey,
        appends: &[JournalAppend],
        lease: &Lease,
    ) -> StoreResult<u64> {
        let store = self.clone();
        let key = key.clone();
        let appends = appends.to_vec();
        let lease = lease.clone();
        run_fs_io(move || store.append_journal_fenced_sync(&key, &appends, &lease)).await
    }

    async fn read_journal(
        &self,
        key: &ExecutionKey,
        from_seq: u64,
    ) -> StoreResult<Vec<JournalRecord>> {
        let store = self.clone();
        let key = key.clone();
        run_fs_io(move || store.read_journal_sync(&key, from_seq)).await
    }

    async fn read_journal_verified(&self, key: &ExecutionKey) -> StoreResult<Journal> {
        let store = self.clone();
        let key = key.clone();
        run_fs_io(move || store.read_journal_verified_sync(&key)).await
    }

    async fn replay_committed_journal(
        &self,
        key: &ExecutionKey,
        watermark: u64,
    ) -> StoreResult<ReplayedCursor> {
        let store = self.clone();
        let key = key.clone();
        run_fs_io(move || store.replay_committed_journal_sync(&key, watermark)).await
    }

    async fn read_journal_projection(
        &self,
        key: &ExecutionKey,
        from_seq: u64,
        watermark: u64,
        require_complete_history: bool,
    ) -> StoreResult<Vec<JournalRecord>> {
        let store = self.clone();
        let key = key.clone();
        run_fs_io(move || {
            store.read_journal_projection_sync(&key, from_seq, watermark, require_complete_history)
        })
        .await
    }

    async fn record_effect_intent(
        &self,
        key: &ExecutionKey,
        entry: &EffectLedgerEntry,
    ) -> StoreResult<()> {
        let store = self.clone();
        let key = key.clone();
        let entry = entry.clone();
        run_fs_io(move || store.record_effect_intent_sync(&key, &entry)).await
    }

    async fn record_effect_intent_fenced(
        &self,
        key: &ExecutionKey,
        entry: &EffectLedgerEntry,
        lease: &Lease,
    ) -> StoreResult<()> {
        let store = self.clone();
        let key = key.clone();
        let entry = entry.clone();
        let lease = lease.clone();
        run_fs_io(move || store.record_effect_intent_fenced_sync(&key, &entry, &lease)).await
    }

    async fn record_effect_outcome(
        &self,
        key: &ExecutionKey,
        effect_id: &EffectId,
        outcome: EffectOutcome,
    ) -> StoreResult<()> {
        let store = self.clone();
        let key = key.clone();
        let effect_id = effect_id.clone();
        run_fs_io(move || store.record_effect_outcome_sync(&key, &effect_id, outcome)).await
    }

    async fn record_effect_outcome_fenced(
        &self,
        key: &ExecutionKey,
        effect_id: &EffectId,
        outcome: EffectOutcome,
        lease: &Lease,
    ) -> StoreResult<()> {
        let store = self.clone();
        let key = key.clone();
        let effect_id = effect_id.clone();
        let lease = lease.clone();
        run_fs_io(move || {
            store.record_effect_outcome_fenced_sync(&key, &effect_id, outcome, &lease)
        })
        .await
    }

    async fn load_effects(&self, key: &ExecutionKey) -> StoreResult<EffectLedger> {
        let store = self.clone();
        let key = key.clone();
        run_fs_io(move || store.load_effects_sync(&key)).await
    }

    async fn claim(
        &self,
        key: &ExecutionKey,
        worker: &WorkerId,
        ttl: Duration,
    ) -> StoreResult<Lease> {
        let store = self.clone();
        let key = key.clone();
        let worker = worker.clone();
        run_fs_control_io(move || store.claim_sync(&key, &worker, ttl)).await
    }

    async fn renew(&self, lease: &Lease, ttl: Duration) -> StoreResult<Lease> {
        let store = self.clone();
        let lease = lease.clone();
        run_fs_control_io(move || store.renew_sync(&lease, ttl)).await
    }

    async fn release(&self, lease: Lease) -> StoreResult<()> {
        let store = self.clone();
        run_fs_control_io(move || store.release_sync(lease)).await
    }

    async fn resolve_wake(
        &self,
        key: &ExecutionKey,
        wake_token: &str,
        resolution_id: &str,
    ) -> StoreResult<()> {
        let store = self.clone();
        let key = key.clone();
        let wake_token = wake_token.to_string();
        let resolution_id = resolution_id.to_string();
        run_fs_io(move || store.resolve_wake_sync(&key, &wake_token, &resolution_id)).await
    }

    async fn wake_resolutions(
        &self,
        key: &ExecutionKey,
        wake_token: &str,
    ) -> StoreResult<Vec<String>> {
        let store = self.clone();
        let key = key.clone();
        let wake_token = wake_token.to_string();
        run_fs_io(move || store.wake_resolutions_sync(&key, &wake_token)).await
    }

    async fn consume_wake(
        &self,
        key: &ExecutionKey,
        wake_token: &str,
        resolution_ids: &[String],
    ) -> StoreResult<usize> {
        let store = self.clone();
        let key = key.clone();
        let wake_token = wake_token.to_string();
        let resolution_ids = resolution_ids.to_vec();
        run_fs_io(move || store.consume_wake_sync(&key, &wake_token, &resolution_ids)).await
    }

    async fn consume_wake_fenced(
        &self,
        key: &ExecutionKey,
        wake_token: &str,
        resolution_ids: &[String],
        lease: &Lease,
    ) -> StoreResult<usize> {
        let store = self.clone();
        let key = key.clone();
        let wake_token = wake_token.to_string();
        let resolution_ids = resolution_ids.to_vec();
        let lease = lease.clone();
        run_fs_io(move || {
            store.consume_wake_fenced_sync(&key, &wake_token, &resolution_ids, &lease)
        })
        .await
    }

    async fn list_runnable(
        &self,
        worker: &WorkerId,
        limit: usize,
    ) -> StoreResult<Vec<ExecutionKey>> {
        let store = self.clone();
        let worker = worker.clone();
        run_fs_io(move || store.list_runnable_sync(&worker, limit)).await
    }

    async fn scan_runnable(
        &self,
        worker: &WorkerId,
        limit: usize,
        after: Option<&ScanCursor>,
    ) -> StoreResult<RunnableScan> {
        let store = self.clone();
        let worker = worker.clone();
        let after = after.cloned();
        run_fs_io(move || store.scan_runnable_sync(&worker, limit, after.as_ref())).await
    }

    async fn scan_terminal_outbox_debt(
        &self,
        max_visits: NonZeroUsize,
        after: Option<&ScanCursor>,
    ) -> StoreResult<TerminalOutboxScan> {
        let store = self.clone();
        let after = after.cloned();
        run_fs_io(move || store.scan_terminal_outbox_debt_sync(max_visits, after.as_ref())).await
    }

    async fn list_parked(&self, limit: usize) -> StoreResult<ParkedListing> {
        let store = self.clone();
        run_fs_io(move || store.list_parked_sync(limit)).await
    }

    async fn scan_parked(
        &self,
        limit: usize,
        after: Option<&ScanCursor>,
    ) -> StoreResult<ParkedListing> {
        let store = self.clone();
        let after = after.cloned();
        run_fs_io(move || store.scan_parked_sync(limit, after.as_ref())).await
    }

    async fn load_projector_cursor(
        &self,
        key: &ExecutionKey,
    ) -> StoreResult<Option<ProjectorCursor>> {
        let store = self.clone();
        let key = key.clone();
        run_fs_io(move || store.load_projector_cursor_sync(&key)).await
    }

    async fn save_projector_cursor(
        &self,
        key: &ExecutionKey,
        cursor: &ProjectorCursor,
    ) -> StoreResult<()> {
        let store = self.clone();
        let key = key.clone();
        let cursor = cursor.clone();
        run_fs_io(move || store.save_projector_cursor_sync(&key, &cursor)).await
    }

    async fn save_projector_cursor_fenced(
        &self,
        key: &ExecutionKey,
        cursor: &ProjectorCursor,
        lease: &Lease,
    ) -> StoreResult<()> {
        let store = self.clone();
        let key = key.clone();
        let cursor = cursor.clone();
        let lease = lease.clone();
        run_fs_io(move || store.save_projector_cursor_fenced_sync(&key, &cursor, &lease)).await
    }

    async fn load_chain_closure(&self, key: &ExecutionKey) -> StoreResult<Option<ChainClosure>> {
        let store = self.clone();
        let key = key.clone();
        run_fs_io(move || store.load_chain_closure_sync(&key)).await
    }

    async fn record_chain_closure(
        &self,
        key: &ExecutionKey,
        closure: &ChainClosure,
    ) -> StoreResult<()> {
        let store = self.clone();
        let key = key.clone();
        let closure = closure.clone();
        run_fs_io(move || store.record_chain_closure_sync(&key, &closure)).await
    }
}

/// Whether a walk over this store's executions should carry on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Walk {
    Continue,
    Stop,
}

/// Where in the walk one visited execution directory sits.
///
/// The **directory** names, which are the scope segments after the workspace
/// layout's normalisation and are therefore not always the key's own segments.
/// That is the point: this is the order the walk runs in, so it is the order a
/// [`ScanCursor`] has to be compared against. Deriving the cursor from the
/// recovered [`ExecutionKey`] instead would compare a walk over directory names
/// against a position in un-normalised key order, and two keys that normalise
/// together would take each other's places in the sequence.
#[derive(Debug, Clone, Copy)]
struct ScanPosition<'a> {
    principal: &'a str,
    workspace: &'a str,
    execution_id: &'a str,
}

impl ScanPosition<'_> {
    fn to_cursor(self) -> ScanCursor {
        ScanCursor::at(self.principal, self.workspace, self.execution_id)
    }
}

/// One level of the walk: the child **directories** of a path, in name order.
///
/// Three fields rather than a `Vec`, because the two ways this answer can be
/// SHORT of the level it describes are not the same thing and the callers above
/// treat them differently. Folding either into the length would make a partial
/// level indistinguishable from a small one.
struct ChildDirs {
    /// The names, sorted, all of them past the cursor the caller asked from.
    names: Vec<String>,
    /// The level held more qualifying names than [`MAX_SCAN_WINDOW`], so the
    /// tail past `names.last()` was not looked at.
    ///
    /// A resume point recovers it, and the walk builds one out of the last name
    /// here — which is why this is a value rather than a warning.
    truncated: bool,
    /// At least one entry was skipped **without the walk being able to say what
    /// it was**: `file_type` refused, or a symlink could not be followed for a
    /// reason other than pointing at nothing.
    ///
    /// It is deliberately NOT set for the two skips whose answer is complete —
    /// a name that is not UTF-8, and a path that is not a directory — because
    /// neither can be a directory this store wrote. Every name it writes comes
    /// from a `String`, and every execution it creates lives under a directory
    /// it created. *Not one of ours* is an answer; *I could not tell* is a hole.
    ///
    /// No resume point recovers this one: paging back to the same entry meets
    /// the same refusal, and a walk that treated it as a stopping point would
    /// hand out the same page forever. So it travels to the caller that can act
    /// on it — the reconciler, whose pass is a coverage claim — and reaches the
    /// scheduler as a warning only.
    unclassified: bool,
}

/// How many names from one ordered window may be descended into now.
///
/// A truncated parent level reserves its last name as an **inclusive** cursor
/// sentinel for the next page. Descending into that same branch on this page is
/// unsafe: if the visitor budget stops part-way through it, the parent sentinel
/// sorts before the visited leaf and wins the resume-point minimum, restarting
/// the branch forever. Principal and workspace cursors are inclusive precisely
/// so reserving the sentinel loses no work.
fn child_dirs_to_visit(children: &ChildDirs) -> usize {
    children
        .names
        .len()
        .saturating_sub(usize::from(children.truncated))
}

/// The child **directories** of `dir`, in name order, starting at `after`.
///
/// `inclusive` decides whether `after` itself is in the answer: the levels above
/// the leaf must be re-entered, because the branch a cursor stopped inside still
/// holds entries after it.
///
/// # Bounded, and completeness survives the bound — but only because it SAYS so
///
/// Only the smallest [`MAX_SCAN_WINDOW`] qualifying names are held, so a
/// directory larger than the window costs one more page rather than the whole
/// level in memory. The next page's cursor sits inside the window this one
/// returned, so the following call selects the names after it.
///
/// That argument is only sound while the caller knows a window was cut, which is
/// why [`ChildDirs::truncated`] is part of the answer. Without it there is a
/// real hole: a level of more than [`MAX_SCAN_WINDOW`] entries that the walk
/// consumes *without* hitting [`MAX_EXECUTIONS_SCANNED`] — twenty thousand
/// principal directories holding no executions between them — would be walked to
/// its window's end, reported as a completed walk, and the entries past the
/// window would be work no caller ever asks for again. The flag turns that into
/// a resume point.
///
/// A truncation that dropped the *largest* names and said nothing is the silent
/// version, and it is exactly what a `take(n)` over an unsorted `read_dir` would
/// have been.
///
/// # Non-directories are skipped rather than descended into
///
/// A file, a socket, a broken symlink — anything this store did not create.
///
/// `file_type` is `lstat` and does not follow a symlink, and **neither does
/// `DirEntry::metadata`**, which std documents as the equivalent of
/// `symlink_metadata` on Unix. An earlier version of this comment claimed that
/// method followed the link. It does not, so every symlinked scope directory
/// answered `is_dir == false` and left the walk — out of `scan_runnable`, out of
/// `list_runnable` and out of `list_parked` at once, with nothing said. The
/// question has to be put to the **path**, which is what `fs::metadata` does.
///
/// A symlink that cannot be followed at all is skipped, because a dangling link
/// is not an outage of the store — but a symlink that could not be *read* is a
/// skip this level cannot account for, and it says so through
/// [`ChildDirs::unclassified`].
///
/// A name that is not UTF-8 is skipped for the plainer reason that this store
/// builds every name it writes from a `String`, so such an entry cannot be one
/// of ours — and admitting it would give the cursor a position it could not
/// round-trip.
///
/// # A path that is not a directory is an EMPTY level, not an outage
///
/// `read_dir` on a file answers `NotADirectory`, and mapping that to
/// [`StoreError::Unavailable`] took every scan and every reconciliation pass
/// down over one stray file. The levels enumerated from a parent are filtered to
/// directories before they are descended into, but `runtime/executions` is
/// **constructed** rather than enumerated, so it is a path nothing above has
/// vouched for — and it was still able to fail the whole walk for every
/// execution in the store. An empty answer is the complete one: this store
/// creates executions only under a directory it made, so a path that is not a
/// directory holds none. The stray is surfaced by the warning rather than by an
/// error, because an error here reads as the store being down and a stray file
/// is not that.
fn ordered_child_dirs(dir: &Path, after: Option<&str>, inclusive: bool) -> StoreResult<ChildDirs> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ChildDirs {
                names: Vec::new(),
                truncated: false,
                unclassified: false,
            })
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotADirectory => {
            warn!(
                path = %dir.display(),
                %error,
                "[LOOP-STORE] a path the scan walks through is not a directory; it can hold no \
                 executions and the walk carries on past it"
            );
            return Ok(ChildDirs {
                names: Vec::new(),
                truncated: false,
                unclassified: false,
            });
        },
        Err(error) => {
            return Err(StoreError::Unavailable {
                detail: format!("{}: {error}", dir.display()),
            })
        },
    };
    let mut window: BTreeSet<String> = BTreeSet::new();
    let mut truncated = false;
    let mut unclassified = false;
    for entry in entries {
        let entry = entry.map_err(unavailable)?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        // The cursor filter FIRST, so the entries a resumed page is skipping
        // past cost a name comparison rather than a `stat` each.
        if let Some(after) = after {
            let past = if inclusive {
                name.as_str() >= after
            } else {
                name.as_str() > after
            };
            if !past {
                continue;
            }
        }
        let kind = match entry.file_type() {
            Ok(kind) => kind,
            // Free from `readdir` on most filesystems and an `lstat` on the
            // rest, which is why it can fail at all: a directory that is
            // readable but not searchable (`r` without `x`) refuses every one of
            // them, and the old code skipped the whole subtree here without a
            // word to anybody.
            Err(error) => {
                unclassified = true;
                warn!(
                    path = %entry.path().display(),
                    %error,
                    "[LOOP-STORE] an entry beside the scope directories could not be \
                     classified, so the walk skipped it without knowing what it was"
                );
                continue;
            },
        };
        let is_dir = if kind.is_dir() {
            true
        } else if kind.is_symlink() {
            // The PATH, not the entry: `DirEntry::metadata` does not follow a
            // symlink, and this question is only ever asked about one.
            match std::fs::metadata(entry.path()) {
                Ok(meta) => meta.is_dir(),
                // Points at nothing. A dangling link is not a directory, is not
                // an outage, and is not a hole in the walk's coverage either —
                // there is nothing on the other end of it to cover.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(error) => {
                    unclassified = true;
                    warn!(
                        path = %entry.path().display(),
                        %error,
                        "[LOOP-STORE] a symlink beside the scope directories could not be \
                         followed, so the walk skipped what may be a scope directory"
                    );
                    false
                },
            }
        } else {
            false
        };
        if !is_dir {
            continue;
        }
        window.insert(name);
        if window.len() > MAX_SCAN_WINDOW {
            // The largest is the one furthest from where the walk is, so it is
            // the one the next page reaches anyway.
            window.pop_last();
            truncated = true;
        }
    }
    Ok(ChildDirs {
        names: window.into_iter().collect(),
        truncated,
        unclassified,
    })
}

/// What one walk over this store's execution directories did.
///
/// # Three answers, because one flag cannot serve both callers
///
/// `stopped_short` alone was enough while the only question was *did you see
/// everything*. A cursored scan asks a second one — *where do I carry on* — and
/// the two are not the same: the walk can stop having visited no execution at
/// all, and a page that could not name a position would either claim the store
/// had been walked to its end or hand the caller back the cursor it arrived
/// with. The second is a spin, and a caller paging in a loop never leaves it.
#[derive(Debug, Default)]
struct WalkOutcome {
    /// The walk did not reach the end of the tree: the visitor said
    /// [`Walk::Stop`], the scan ceiling fired, or a level was wider than
    /// [`MAX_SCAN_WINDOW`].
    stopped_short: bool,
    /// The **earliest** position the walk knows it left unvisited entries after,
    /// across every level it cut.
    ///
    /// Earliest, not latest, and that is the whole of the correctness argument:
    /// a workspace level cut under an early principal sits *before* a principal
    /// level cut, and resuming at the later of the two would step over
    /// everything between them. A caller resumes at the earliest thing anybody
    /// still owes it and re-walks the rest, which costs a repeat and never a
    /// loss.
    ///
    /// Every position here is strictly after the cursor the walk started from —
    /// a level is only cut when it holds more than [`MAX_SCAN_WINDOW`] names
    /// past that cursor, so its last name is past it too — which is what makes
    /// resuming from one progress rather than a repeat.
    ///
    /// A cut above the leaf leaves the segments below it **empty**, and that is
    /// load-bearing in two directions at once: the empty string sorts below
    /// every real name, so nothing under the named branch is skipped, and the
    /// levels above the leaf are re-entered *inclusively*, so the branch itself
    /// is descended into again rather than stepped over. A resume of
    /// `[principal, "", ""]` therefore means *everything from this principal
    /// onwards*, which is exactly what a cut principal level owes its caller.
    cut_at: Option<[String; 3]>,
    /// At least one entry could not be classified as a directory or not, so a
    /// subtree that may hold executions was skipped. See
    /// [`ChildDirs::unclassified`]; no cursor recovers it.
    unclassified: bool,
}

impl WalkOutcome {
    /// Record a level the walk did not see the end of, keeping the earliest.
    fn note_cut(&mut self, at: [String; 3]) {
        if self.cut_at.as_ref().is_none_or(|earliest| at < *earliest) {
            self.cut_at = Some(at);
        }
    }
}

impl FsLoopStateStore {
    /// Visit every execution directory under this store's scope root, in order.
    ///
    /// Shared by [`Self::scan_runnable`] and [`Self::list_parked`] so the two
    /// cannot drift about what an execution directory *is* — the earlier version
    /// had the three-deep walk written out inside `list_runnable`, and a second
    /// copy would have been a second answer to "which directories count",
    /// including the scan ceiling that stops a runaway tree.
    ///
    /// Returns a [`WalkOutcome`] rather than a bare "stopped short", because the
    /// two callers cannot act on one flag: a scheduler needs a POSITION to carry
    /// on from and a reconciler needs to know whether its pass covered the tree.
    ///
    /// # The order is now part of the contract, not a property of `read_dir`
    ///
    /// This used to walk in `read_dir` order, which is whatever the filesystem
    /// hands back and which changes as entries are created and removed —
    /// something this tree does constantly. That was survivable while the only
    /// question was *which `limit` keys do I get*; it is not survivable with a
    /// cursor, because resuming "after" a position in an order that moved is how
    /// a live execution gets skipped silently. So every level is sorted, and the
    /// cursor is compared against that order.
    ///
    /// Sorting does **not** mean materialising a level:
    /// [`ordered_child_dirs`] keeps only the smallest [`MAX_SCAN_WINDOW`] names
    /// still ahead of the cursor, so one enormous directory costs a bounded
    /// window and another page rather than a memory failure.
    ///
    /// # A non-directory beside the scope directories is skipped, not fatal
    ///
    /// It used to be fatal. `read_dir` on a *file* answers `NotADirectory`,
    /// which the old code's `NotFound`-only arm did not match, so one stray file
    /// under `scopes/` — a `.DS_Store`, a lock, anything another subsystem left
    /// — made **every** scan and every reconciliation pass answer
    /// `StoreError::Unavailable`, and a runner reads that as the store being
    /// down. Levels are now filtered to directories before they are descended
    /// into, so the shape cannot arise.
    fn walk_execution_dirs(
        &self,
        after: Option<&[String; 3]>,
        mut visit: impl FnMut(&Path, ScanPosition<'_>) -> Walk,
    ) -> StoreResult<WalkOutcome> {
        let mut visited = 0usize;
        let mut outcome = WalkOutcome::default();
        let scopes_root = self.workspace.scopes_root();

        // At each level the cursor bounds the FIRST branch only, and it is
        // inclusive above the leaf: the principal and workspace the cursor names
        // still have executions after it, so the walk must descend into them
        // again. Only the execution level is exclusive, because that is the
        // entry the caller has already been shown.
        let principals = ordered_child_dirs(&scopes_root, after.map(|at| at[0].as_str()), true)?;
        outcome.unclassified |= principals.unclassified;
        if principals.truncated {
            // The tail this level did not show begins after its last name, and
            // the level above the leaf is re-entered inclusively — so naming
            // that name is naming the cut, not skipping past it.
            if let Some(last) = principals.names.last() {
                outcome.note_cut([last.clone(), String::new(), String::new()]);
            }
        }
        // When this level is truncated, its last name is the inclusive cursor
        // sentinel recorded above. Do not descend into it until the next page;
        // otherwise a visit-budget stop inside that principal would resume at
        // the earlier parent cursor and repeat the same prefix forever.
        let principal_count = child_dirs_to_visit(&principals);
        for principal in principals.names.into_iter().take(principal_count) {
            let resuming_here = after.is_some_and(|at| at[0] == principal);
            let workspaces = ordered_child_dirs(
                &scopes_root.join(&principal),
                after.filter(|_| resuming_here).map(|at| at[1].as_str()),
                true,
            )?;
            outcome.unclassified |= workspaces.unclassified;
            if workspaces.truncated {
                if let Some(last) = workspaces.names.last() {
                    outcome.note_cut([principal.clone(), last.clone(), String::new()]);
                }
            }
            // Same rule one level down. Workspace cursors are inclusive, so the
            // reserved last workspace is the first branch of the next page.
            let workspace_count = child_dirs_to_visit(&workspaces);
            for workspace in workspaces.names.into_iter().take(workspace_count) {
                let resuming_here = resuming_here && after.is_some_and(|at| at[1] == workspace);
                let executions_root = scopes_root
                    .join(&principal)
                    .join(&workspace)
                    .join("runtime")
                    .join("executions");
                let executions = ordered_child_dirs(
                    &executions_root,
                    after.filter(|_| resuming_here).map(|at| at[2].as_str()),
                    false,
                )?;
                outcome.unclassified |= executions.unclassified;
                if executions.truncated {
                    // Exclusive at this level, which is what makes naming the
                    // last execution the right cut: the tail is what sorts
                    // after it.
                    if let Some(last) = executions.names.last() {
                        outcome.note_cut([principal.clone(), workspace.clone(), last.clone()]);
                    }
                }
                for execution in executions.names {
                    visited += 1;
                    if visited > MAX_EXECUTIONS_SCANNED {
                        warn!(
                            scanned = visited,
                            "[LOOP-STORE] a scan over the store hit its ceiling; some \
                             executions were not visited"
                        );
                        outcome.stopped_short = true;
                        return Ok(outcome);
                    }
                    // `as_str`, not `&String`. A struct field IS a coercion
                    // site so both compile, but the explicit form is one less
                    // thing for a reader to have to know.
                    let position = ScanPosition {
                        principal: principal.as_str(),
                        workspace: workspace.as_str(),
                        execution_id: execution.as_str(),
                    };
                    if visit(&executions_root.join(&execution), position) == Walk::Stop {
                        outcome.stopped_short = true;
                        return Ok(outcome);
                    }
                }
            }
        }
        // The walk reached the end of every level it looked at. It still stopped
        // short of the STORE if any of those levels was wider than the window,
        // and that is the one case `MAX_EXECUTIONS_SCANNED` does not cover: a
        // level of more than `MAX_SCAN_WINDOW` entries whose members hold no
        // executions between them is consumed entirely without the ceiling ever
        // firing, and reporting it as a completed walk would leave the tail as
        // work no caller asks for again.
        outcome.stopped_short = outcome.cut_at.is_some();
        Ok(outcome)
    }

    /// The key an execution directory holds, refusing one that names elsewhere.
    ///
    /// The record has to address the directory it was found in. One naming
    /// another execution would have a scan load that execution's state through
    /// this directory and answer about the same run twice, once from each — and a
    /// record naming a directory outside this tree would have it read outside the
    /// scope root entirely. `ExecutionKey`'s deserializer closes the second; this
    /// closes the first, and neither takes the file's word.
    fn key_at(&self, dir: &Path) -> StoreResult<Option<ExecutionKey>> {
        let key_path = dir.join("key.json");
        let Some(key): Option<ExecutionKey> =
            read_bounded_json(&key_path, MAX_KEY_BYTES, MAX_KEY_JSON_NODES).map_err(|error| {
                StoreError::Unavailable {
                    detail: format!("{}: {error}", key_path.display()),
                }
            })?
        else {
            // No key record at all: a directory nothing in this store has
            // written, or one left by a tool that is not this store. A directory
            // that has been *claimed* or *appended to* without committing does
            // carry a record — the write paths stamp one — and falls out at the
            // `load_sync` that finds no committed state instead.
            return Ok(None);
        };
        if self.execution_dir(&key).as_path() != dir {
            return Err(StoreError::Corrupt {
                key: key.clone(),
                detail: format!(
                    "{} holds a key record for {key}, which addresses {}",
                    dir.display(),
                    self.execution_dir(&key).display()
                ),
            });
        }
        Ok(Some(key))
    }

    /// The parked execution at `dir`, or nothing when it is not parked.
    ///
    /// # What this reads, and what it deliberately does not
    ///
    /// What [`Self::runnable_at`] already reads for the scheduler — the key
    /// record and the newest snapshot, through [`Self::load_sync`] — plus one
    /// `stat` on that snapshot and the lease directory.
    ///
    /// It does **not** open the wake ledger and it does not look at any child.
    /// Those are the reconciler's, and doing them here would put a wake read per
    /// parked execution inside a listing, which is the shape
    /// [`LoopStateStore::list_parked`]'s own documentation rules out.
    fn parked_at(&self, dir: &Path) -> StoreResult<Option<ParkedExecution>> {
        let Some(key) = self.key_at(dir)? else {
            return Ok(None);
        };
        let Some(committed) = self.load_sync(&key)? else {
            return Ok(None);
        };
        let Some(wait) = committed.state.wait.clone() else {
            return Ok(None);
        };
        let deadline_at_ms = committed.state.deadline_at_ms;
        let parked_since_ms = self.snapshot_published_at_ms(&key, committed.revision);
        let last_lease = self.latest_lease(&key)?.map(|stored| LastLease {
            worker: stored.worker,
            fence: stored.fence,
            expires_at_ms: stored.expires_at_ms,
            released: stored.released,
        });
        Ok(Some(ParkedExecution {
            key,
            wait,
            parked_since_ms,
            last_lease,
            deadline_at_ms,
        }))
    }

    /// When the snapshot for `revision` was written, in wall-clock milliseconds.
    ///
    /// The snapshot is published by `link(2)` from a fully written and fsynced
    /// temporary, and a hard link carries the inode's mtime, so this is the time
    /// the bytes were written rather than the time the name appeared. The two are
    /// microseconds apart and neither is a clock this store controls.
    ///
    /// `None` on **every** failure, and the absence is deliberately not an error:
    /// a store that could not date a snapshot has still told the truth about the
    /// park, and failing the whole listing over a missing mtime would hide every
    /// other parked run behind one unreadable timestamp. The caller is required
    /// to treat `None` as *"age unknown"* rather than *"new"*.
    fn snapshot_published_at_ms(&self, key: &ExecutionKey, revision: Revision) -> Option<i64> {
        let modified = std::fs::metadata(self.snapshot_path(key, revision))
            .ok()?
            .modified()
            .ok()?;
        // Before the epoch is a clock nobody should be reasoning about, so it
        // reads as unknown rather than as a very old park.
        let since_epoch = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
        i64::try_from(since_epoch.as_millis()).ok()
    }

    /// Whether the execution at `dir` may be picked up by `worker` now.
    ///
    /// Split out so the scan's error handling has one thing to catch: everything
    /// that can fail for one execution fails here, and the caller decides what a
    /// single unreadable execution means for the whole listing.
    fn runnable_at(
        &self,
        dir: &Path,
        worker: &WorkerId,
        now_ms: i64,
    ) -> StoreResult<Option<ExecutionKey>> {
        let Some(key) = self.key_at(dir)? else {
            return Ok(None);
        };
        let Some(committed) = self.load_sync(&key)? else {
            return Ok(None);
        };
        let revision = committed.revision;
        let state = committed.state;
        // `now_ms` is already the instant this whole scan judges against, so an
        // expired pin is offered by the same pass that withholds a live one.
        if !state.claimable_by(worker, now_ms) {
            return Ok(None);
        }
        // An expired *unparked* run is runnable reconciliation work: the driver
        // must claim it and durably commit the canonical timeout terminal. Withholding it here
        // made it invisible to both the scheduler (because of this predicate)
        // and the park reconciler (because it has no wait). An unresolved park
        // remains withheld by the wake check below and is handled by the parked
        // reconciler's deadline ground.
        if state.runnable_at_ms > now_ms && !state.deadline_passed(now_ms) {
            return Ok(None);
        }
        if let Some(wait) = &state.wait {
            // Any unconsumed resolution naming this token satisfies the park.
            //
            // Read rather than probed with `exists()`: that reports a directory
            // it cannot read as one that is not there, and the two mean opposite
            // things — absent is "still waiting", unreadable is "this store
            // cannot answer". A parked execution that silently never wakes is
            // the failure the wake token exists to prevent, so an unreadable
            // ledger raises and quarantines this execution loudly instead.
            if self
                .wake_resolution_paths(&key, &wait.wake_token())?
                .is_empty()
            {
                return Ok(None);
            }
        }
        if state
            .terminal_settlement_receipt
            .as_ref()
            .is_some_and(|receipt| {
                receipt.descriptor.terminal_seq == state.journal_seq
                    && receipt.descriptor.exact_segment_id == key.execution_id()
            })
        {
            // Receipt-backed endings, including resumable HITL, belong only to
            // the independent terminal projector. Re-offering the immutable
            // source segment would rerun its ending phase or churn claims after
            // the successor has already resumed.
            return Ok(None);
        }
        // A run that ended for good. One small read, and only reached by an
        // execution that has passed every free test above — so the walk pays for
        // it on the keys it was about to offer, not on the ones it was going to
        // withhold anyway.
        //
        // Exact watermark equality is what keeps a prepublished or stale marker
        // honest. A committed non-resumable terminal is the final record in its
        // authoritative prefix, so a marker below or above the current
        // watermark describes a different snapshot and is ignored.
        if let Some(ended) = self.ended_run(&key, revision)? {
            if ended.seq == state.journal_seq {
                let Some(projection_through) = ended.projection_through_seq() else {
                    return Ok(None);
                };
                let projected_through = self
                    .load_projector_cursor_sync(&key)?
                    .as_ref()
                    .map(ProjectorCursor::emitted_through_seq)
                    .unwrap_or(0);
                if projected_through >= projection_through {
                    return Ok(None);
                }
            }
        }
        if self.blocking_lease(&key, now_ms)?.is_some() {
            return Ok(None);
        }
        Ok(Some(key))
    }

    /// Whether `dir` names terminal lifecycle debt or any committed operator
    /// steer receipt still awaiting acknowledgement.
    ///
    /// Deliberately does not read placement, runnable time, waits or leases:
    /// this is projection discovery, not authority to execute another phase.
    /// The projector claims and re-verifies the exact key before emitting.
    fn terminal_outbox_debt_at(&self, dir: &Path) -> StoreResult<Option<ExecutionKey>> {
        let Some(key) = self.key_at(dir)? else {
            return Ok(None);
        };
        let Some(committed) = self.load_sync(&key)? else {
            return Ok(None);
        };
        // Receipt retirement is placement- and ending-independent. A runtime
        // cancellation can make this state permanently non-runnable before the
        // terminal boundary is published; requiring ended.json first made that
        // sealed inbox batch undiscoverable forever. The lifecycle claimant
        // leases, reloads and fenced-clears the exact pointer before deciding
        // whether terminal event/runtime settlement also applies.
        if committed.state.steer_consume_receipt.is_some() {
            return Ok(Some(key));
        }
        let cursor = self.load_projector_cursor_sync(&key)?;
        let receipt_lifecycle_debt = committed
            .state
            .terminal_settlement_receipt
            .as_ref()
            .is_some_and(|receipt| {
                receipt.descriptor.terminal_seq == committed.state.journal_seq
                    && (committed.state.identity.task_id.is_some()
                        || committed.state.identity.execution_id.is_some())
                    && (cursor
                        .as_ref()
                        .and_then(ProjectorCursor::runtime_settled_terminal_seq)
                        != Some(receipt.descriptor.terminal_seq)
                        || cursor
                            .as_ref()
                            .map(ProjectorCursor::emitted_through_seq)
                            .unwrap_or(0)
                            < receipt.descriptor.terminal_seq)
            });
        let Some(ended) = self.ended_run(&key, committed.revision)? else {
            return Ok(receipt_lifecycle_debt.then_some(key));
        };
        if ended.seq != committed.state.journal_seq {
            return Ok(receipt_lifecycle_debt.then_some(key));
        }
        let event_debt = ended
            .projection_through_seq()
            .is_some_and(|projection_through| {
                cursor
                    .as_ref()
                    .map(ProjectorCursor::emitted_through_seq)
                    .unwrap_or(0)
                    < projection_through
            });
        // Receipt-less ordinary endings predate cross-layer settlement
        // receipts. Their coarse journal kind cannot safely reconstruct the
        // historical runtime/Artifact outcome, so rolling upgrades project
        // their remaining events but do not create permanent settlement debt.
        // CannotProceed retains its exact reconciler-owned compatibility path.
        let runtime_settlement_debt = ended.terminal != TerminalKind::HandedOff
            && (committed.state.identity.task_id.is_some()
                || committed.state.identity.execution_id.is_some())
            && (ended.terminal == TerminalKind::CannotProceed
                || committed
                    .state
                    .terminal_settlement_receipt
                    .as_ref()
                    .is_some_and(|receipt| receipt.descriptor.terminal_seq == ended.seq))
            && cursor
                .as_ref()
                .and_then(ProjectorCursor::runtime_settled_terminal_seq)
                != Some(ended.seq);
        Ok((event_debt || runtime_settlement_debt || receipt_lifecycle_debt).then_some(key))
    }
}

// ============================================================================
// Durable primitives
// ============================================================================

/// Whether a link-published file was taken by this caller or was already there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Publish {
    Published,
    AlreadyExists,
}

fn valid_wake_resolution_filename(name: &str) -> bool {
    let Some(digest) = name.strip_suffix(JSON_SUFFIX) else {
        return false;
    };
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Publish `bytes` at `path`, failing rather than overwriting.
///
/// The compare-and-swap primitive this whole store rests on. `link(2)` is atomic
/// and refuses an existing target, and the target's content is fully written and
/// fsynced before the link makes the name visible — so no reader can ever observe
/// a torn published file, and no two writers can both believe they published one.
///
/// # The NFS caveat, stated rather than assumed
///
/// On a network filesystem a `link` that actually succeeded can report failure
/// after a retransmit, which this reads as `AlreadyExists` and the caller reads
/// as a lost race. That is the conservative direction — a spurious conflict makes
/// a caller reload and see the state it thought it lost, where the opposite error
/// would let two writers both proceed.
fn publish_new_file(path: &Path, bytes: &[u8]) -> std::io::Result<Publish> {
    if let Some(parent) = path.parent() {
        create_dir_all_durably(parent)?;
    }
    let temp = temp_sibling(path);
    let write = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        std::io::Write::write_all(&mut file, bytes)?;
        std::io::Write::flush(&mut file)?;
        file.sync_all()
    })();
    if let Err(error) = write {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }

    let linked = std::fs::hard_link(&temp, path);
    let _ = std::fs::remove_file(&temp);
    match linked {
        Ok(()) => {
            // The name is not durable until its directory is. A failure here
            // leaves the record in place but returns an error. The caller can
            // reload and observe an ambiguously completed publish; returning
            // success would claim crash durability the store did not establish.
            sync_parent_directory(path)?;
            Ok(Publish::Published)
        },
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Ok(Publish::AlreadyExists)
        },
        // ── A REAPED TEMPORARY IS A LOST RACE, NOT AN UNAVAILABLE STORE ──
        //
        // `NotFound` here means the temporary this function created a few lines
        // above is gone before it could be linked. One thing in this module
        // deletes temporaries — `reap_leaked_temporaries` — and it runs from
        // exactly one place: a `claim` that just WON the lease. So a vanished
        // temporary is evidence that another worker won, which is the same
        // fact `AlreadyExists` reports.
        //
        // It is confirmed rather than inferred. `path.exists()` is the ground
        // truth of "somebody published this record", and only if it holds does
        // this become a lost race; a `NotFound` with no record in place is a
        // genuine failure and is still returned as one. Without the check this
        // would report a lost race whenever a directory went missing under it.
        //
        // Why here and not in the reaper: the reaper cannot tell a live
        // claimant's temporary from a crashed publish's — they differ in no
        // property of the file — and
        // `taking_the_lease_reaps_temporaries_a_crashed_publish_left_behind`
        // requires the crashed one to be reaped by the next claim. Any rule
        // that spared the live temporary would strand the dead one.
        //
        // What this fixes, from `claim`'s side: a loser used to return
        // `StoreError::Unavailable` — "the store is broken" — where the truth
        // was `StoreError::LeaseHeld` — "another worker has it". A scheduler
        // reads those oppositely, and `WorkerRunner::sweep` races claims across
        // runnable keys by design, so the wrong one was on the ordinary path.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && path.exists() => {
            Ok(Publish::AlreadyExists)
        },
        Err(error) => Err(error),
    }
}

/// Atomically replace reconstructible derived state without adding an fsync to
/// every append and commit.
///
/// A crash may lose this rename; that is safe because every consumer validates
/// the checksum and exact journal stamp and falls back to the authoritative log.
/// The temp file is flushed so live rolling processes see complete bytes, and
/// rename prevents either process from observing a partial index.
fn overwrite_derived_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = temp_sibling(path);
    let result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        std::io::Write::write_all(&mut file, bytes)?;
        std::io::Write::flush(&mut file)?;
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Replace `path` wholesale, atomically.
///
/// For the files that are genuinely last-writer-wins under a lease — an effect
/// row, a swept journal — where revision addressing would buy nothing and a
/// rename is the cheaper atomic publish.
///
/// # Why this does NOT call `artifact_v2::io::write_bytes_atomic_sync`
///
/// `check_store_durability_adoption.py` flags the `fs::rename` below, and the
/// rule it enforces is a good one: a hand-rolled atomic write usually misses a
/// unique temp name, a `sync_all`, or a parent-directory sync. This one misses
/// none of those. What it has that the shared helper cannot have is a temporary
/// **this module's reaper can find**.
///
/// [`reap_leaked_temporaries`] identifies a leaked staging file by
/// [`TEMP_INFIX`] — the literal `.tmp-` that [`temp_sibling`] puts in the name.
/// The shared helper stages at `.artifact-write-{uuid}.tmp`, which does not
/// contain that infix, so a crash between its create and its rename would leave
/// a file in an execution directory that **nothing in this store ever cleans**.
/// Adopting the helper here would trade one duplicated ten-line function for a
/// permanent leak in the one directory tree a stateless worker owns.
///
/// If the helper ever grows a caller-supplied staging name, delete this
/// function and use it. Until then the baseline records this hit deliberately.
fn overwrite_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        create_dir_all_durably(parent)?;
    }
    let temp = temp_sibling(path);
    let write = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        std::io::Write::write_all(&mut file, bytes)?;
        std::io::Write::flush(&mut file)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)
    })();
    if let Err(error) = write {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    // Preserve the renamed record, but do not report a durable success until
    // the directory entry is durable too. A retry can reload the record and
    // resolve the ambiguous completion.
    sync_parent_directory(path)?;
    Ok(())
}

fn chained_journal_digest(previous: &str, bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(previous.as_bytes());
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn valid_journal_digest(value: &str) -> bool {
    value.len() == 64 && hex::decode(value).is_ok()
}

fn terminal_debt_catalog_initial() -> TerminalDebtCatalog {
    let mut catalog = TerminalDebtCatalog {
        schema_version: TERMINAL_DEBT_CATALOG_SCHEMA_VERSION,
        complete: false,
        overflowed: false,
        rebuild_had_errors: false,
        rebuild_dirty_generation: None,
        last_authoritative_scan_ms: 0,
        entries: BTreeMap::new(),
        integrity_sha256: String::new(),
    };
    catalog.integrity_sha256 = terminal_debt_catalog_integrity(&catalog);
    catalog
}

fn terminal_debt_entry_digest(entry: &TerminalDebtCatalogEntry) -> String {
    let encoded = serde_json::to_vec(&(
        "magician.loop-terminal-debt-entry.v1",
        &entry.principal,
        &entry.workspace,
        &entry.execution_id,
    ))
    .expect("a terminal-debt key contains only infallibly serializable strings");
    hex::encode(Sha256::digest(encoded))
}

fn terminal_debt_catalog_integrity(catalog: &TerminalDebtCatalog) -> String {
    let encoded = serde_json::to_vec(&(
        "magician.loop-terminal-debt-catalog.v1",
        catalog.schema_version,
        catalog.complete,
        catalog.overflowed,
        catalog.rebuild_had_errors,
        &catalog.rebuild_dirty_generation,
        catalog.last_authoritative_scan_ms,
        &catalog.entries,
    ))
    .expect("the terminal-debt catalog contains only infallibly serializable fields");
    hex::encode(Sha256::digest(encoded))
}

fn valid_terminal_debt_catalog(catalog: &TerminalDebtCatalog, now_ms: i64) -> bool {
    catalog.schema_version == TERMINAL_DEBT_CATALOG_SCHEMA_VERSION
        && catalog.entries.len() <= MAX_TERMINAL_DEBT_CATALOG_ENTRIES
        && (!catalog.complete || (!catalog.overflowed && !catalog.rebuild_had_errors))
        && catalog.rebuild_dirty_generation.as_ref().is_none_or(|generation| {
            Uuid::parse_str(generation).is_ok()
        })
        && catalog.last_authoritative_scan_ms >= 0
        // A backwards wall-clock step cannot extend the period during which a
        // derivative is trusted. Treat a future audit stamp as invalid and
        // rebuild authoritatively.
        && catalog.last_authoritative_scan_ms <= now_ms
        && valid_journal_digest(&catalog.integrity_sha256)
        && catalog.integrity_sha256 == terminal_debt_catalog_integrity(catalog)
        && catalog.entries.iter().all(|(digest, entry)| {
            valid_journal_digest(digest)
                && digest == &terminal_debt_entry_digest(entry)
                && entry.observed_at_ms > 0
                && entry.observed_at_ms
                    <= now_ms.saturating_add(TERMINAL_DEBT_FUTURE_MARKER_GRACE_MS)
                && ExecutionKey::new(
                    entry.principal.clone(),
                    entry.workspace.clone(),
                    entry.execution_id.clone(),
                )
                .is_ok()
        })
}

fn terminal_debt_priority_cursor(digest: String) -> ScanCursor {
    ScanCursor::at(TERMINAL_DEBT_PRIORITY_CURSOR_SENTINEL, "", digest)
}

fn terminal_debt_priority_cursor_digest(cursor: &ScanCursor) -> Option<&str> {
    let segments = cursor.segments();
    (segments[0] == TERMINAL_DEBT_PRIORITY_CURSOR_SENTINEL
        && segments[1].is_empty()
        && valid_journal_digest(&segments[2]))
    .then_some(segments[2].as_str())
}

fn journal_index_integrity(index: &JournalIndex) -> String {
    let encoded = serde_json::to_vec(&(
        "magician.loop-journal-index.v1",
        index.schema_version,
        index.stamp,
        &index.chain_sha256,
        index.last_seq,
        index.committed_watermark,
        index.replayed,
        index.last_event_seq,
        &index.offsets,
    ))
    .expect("the journal index contains only infallibly serializable fields");
    hex::encode(Sha256::digest(encoded))
}

fn placement_index_integrity(index: &PlacementIndex) -> String {
    let encoded = serde_json::to_vec(&(
        "magician.loop-placement-index.v1",
        index.schema_version,
        index.revision,
        index.snapshot_stamp,
        &index.placement,
    ))
    .expect("the placement index contains only infallibly serializable fields");
    hex::encode(Sha256::digest(encoded))
}

fn base_binding_integrity(record: &BaseSegmentBindingRecord) -> String {
    let encoded = serde_json::to_vec(&(
        "magician.loop-base-segment-binding.v1",
        record.schema_version,
        &record.principal,
        &record.workspace,
        &record.base_execution_id,
        &record.exact_segment_id,
        record.explicit_binding,
    ))
    .expect("the base binding contains only infallibly serializable fields");
    hex::encode(Sha256::digest(encoded))
}

fn base_binding_head_integrity(head: &BaseBindingHead) -> String {
    let encoded = serde_json::to_vec(&(
        "magician.loop-base-binding-head.v1",
        head.schema_version,
        &head.principal,
        &head.workspace,
        &head.base_execution_id,
        &head.exact_segments,
    ))
    .expect("the base binding head contains only infallibly serializable fields");
    hex::encode(Sha256::digest(encoded))
}

fn legacy_writer_cutover_integrity(cutover: &LegacyWriterCutover) -> String {
    let encoded = serde_json::to_vec(&(
        "magician.loop-legacy-writer-cutover.v1",
        cutover.schema_version,
        &cutover.principal,
        &cutover.workspace,
        &cutover.deployment_id,
        cutover.retired_at_ms,
    ))
    .expect("the legacy-writer cutover contains only infallibly serializable fields");
    hex::encode(Sha256::digest(encoded))
}

fn valid_legacy_writer_cutover(
    cutover: &LegacyWriterCutover,
    principal: &str,
    workspace: &str,
) -> bool {
    cutover.schema_version == LEGACY_WRITER_CUTOVER_SCHEMA_VERSION
        && cutover.principal == principal
        && cutover.workspace == workspace
        && !cutover.deployment_id.trim().is_empty()
        && cutover.deployment_id.len() <= 128
        && !cutover.deployment_id.chars().any(char::is_control)
        && cutover.retired_at_ms > 0
        && valid_journal_digest(&cutover.integrity_sha256)
        && cutover.integrity_sha256 == legacy_writer_cutover_integrity(cutover)
}

fn restrict_uncutover_authority(
    authority: BaseExecutionRecoveryAuthority,
    legacy_writers_retired: bool,
) -> BaseExecutionRecoveryAuthority {
    if legacy_writers_retired {
        return authority;
    }
    match authority {
        BaseExecutionRecoveryAuthority::RecoverableExact { exact_segment, .. } => {
            BaseExecutionRecoveryAuthority::Uncertain {
                exact_segments: vec![exact_segment],
                reason: "legacy stateless writers have not been explicitly retired; indexed exact recovery is discovery-only"
                    .to_owned(),
            }
        },
        BaseExecutionRecoveryAuthority::Absent => BaseExecutionRecoveryAuthority::Uncertain {
            exact_segments: Vec::new(),
            reason: "legacy stateless writers have not been explicitly retired; indexed absence is not authority"
                .to_owned(),
        },
        positive_or_uncertain => positive_or_uncertain,
    }
}

fn segment_recovery_index_integrity(index: &SegmentRecoveryIndex) -> String {
    let encoded = serde_json::to_vec(&(
        "magician.loop-segment-recovery-index.v1",
        index.schema_version,
        index.revision,
        index.snapshot_stamp,
        &index.base_execution_id,
        &index.exact_segment_id,
        index.explicit_binding,
        index.has_continuation_checkpoint,
        index.terminal_settlement_seq,
        index.non_resumable_ended,
        &index.placement,
    ))
    .expect("the segment recovery index contains only infallibly serializable fields");
    hex::encode(Sha256::digest(encoded))
}

fn base_preseed_admission_integrity(admission: &BasePreseedAdmission) -> String {
    let encoded = serde_json::to_vec(&(
        "magician.loop-base-preseed-admission.v2",
        admission.schema_version,
        &admission.principal,
        &admission.workspace,
        &admission.base_execution_id,
        &admission.token,
        admission.runtime_updated_at,
        &admission.exact_segment,
        admission.exact_revision,
        &admission.base_binding_head_sha256,
        admission.expires_at_ms,
    ))
    .expect("the pre-seed admission contains only infallibly serializable fields");
    hex::encode(Sha256::digest(encoded))
}

fn legacy_base_preseed_admission_integrity(admission: &BasePreseedAdmission) -> String {
    let encoded = serde_json::to_vec(&(
        "magician.loop-base-preseed-admission.v1",
        admission.schema_version,
        &admission.principal,
        &admission.workspace,
        &admission.base_execution_id,
        &admission.token,
        admission.runtime_updated_at,
        &admission.exact_segment,
        admission.exact_revision,
        admission.expires_at_ms,
    ))
    .expect("the legacy pre-seed admission contains only infallibly serializable fields");
    hex::encode(Sha256::digest(encoded))
}

fn valid_base_preseed_admission(
    admission: &BasePreseedAdmission,
    principal: &str,
    workspace: &str,
    base_execution_id: &str,
) -> bool {
    let integrity_valid = match admission.schema_version {
        BASE_PRESEED_ADMISSION_SCHEMA_VERSION => {
            valid_journal_digest(&admission.base_binding_head_sha256)
                && admission.integrity_sha256 == base_preseed_admission_integrity(admission)
        },
        1 => {
            admission.base_binding_head_sha256.is_empty()
                && admission.integrity_sha256 == legacy_base_preseed_admission_integrity(admission)
        },
        _ => false,
    };
    admission.principal == principal
        && admission.workspace == workspace
        && admission.base_execution_id == base_execution_id
        && Uuid::parse_str(&admission.token).is_ok()
        && admission.expires_at_ms > admission.runtime_updated_at
        && valid_journal_digest(&admission.integrity_sha256)
        && integrity_valid
}

fn renew_base_recovery_admission_locked(
    path: &Path,
    admission: &mut BasePreseedAdmission,
    now_ms: i64,
) -> StoreResult<()> {
    if admission.schema_version != BASE_PRESEED_ADMISSION_SCHEMA_VERSION
        || !valid_journal_digest(&admission.base_binding_head_sha256)
    {
        return Err(StoreError::Unavailable {
            detail: "an unstamped legacy admission cannot be renewed".to_owned(),
        });
    }
    admission.expires_at_ms =
        now_ms.saturating_add(crate::magician_v2::execution::agentic::run_loop::state::PIN_TTL_MS);
    admission.integrity_sha256 = base_preseed_admission_integrity(admission);
    let encoded = serde_json::to_vec(admission).map_err(|error| StoreError::Unavailable {
        detail: format!("base recovery admission renewal encode failed: {error}"),
    })?;
    overwrite_derived_atomically(path, &encoded).map_err(|error| StoreError::Unavailable {
        detail: format!("base recovery admission renewal failed: {error}"),
    })
}

fn retire_base_recovery_admission_file_locked(path: &Path) -> StoreResult<()> {
    match std::fs::remove_file(path) {
        Ok(()) => sync_parent_directory(path).map_err(|error| StoreError::Unavailable {
            detail: format!("recovery-admission retirement was not synced: {error}"),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(StoreError::Unavailable {
            detail: format!("recovery-admission retirement failed: {error}"),
        }),
    }
}

fn read_base_binding_catalog_marker(path: &Path) -> StoreResult<Option<BaseBindingCatalogMarker>> {
    let marker: Option<BaseBindingCatalogMarker> =
        read_bounded_json(path, MAX_BASE_BINDING_BYTES, MAX_BASE_BINDING_JSON_NODES).map_err(
            |error| StoreError::Unavailable {
                detail: format!("{}: {error}", path.display()),
            },
        )?;
    match marker {
        None => Ok(None),
        // Schema one contained only the completion bit. Treat it as an
        // unindexed scope and rebuild under the migration lock; defaulted
        // fields above make that older marker readable without trusting it.
        Some(marker) if marker.schema_version != BASE_BINDING_CATALOG_SCHEMA_VERSION => Ok(None),
        Some(marker)
            if marker.mixed_writer_compat_until_ms > 0
                && (!marker.cutover_refresh_complete
                    || (marker.complete
                        && marker
                            .cutover_refresh_deployment_id
                            .as_deref()
                            .is_some_and(|deployment| !deployment.trim().is_empty()))) =>
        {
            Ok(Some(marker))
        },
        Some(_) => Err(StoreError::Unavailable {
            detail: format!(
                "{} is not a valid base binding catalog marker",
                path.display()
            ),
        }),
    }
}

fn base_binding_catalog_marker_initial() -> BaseBindingCatalogMarker {
    BaseBindingCatalogMarker {
        schema_version: BASE_BINDING_CATALOG_SCHEMA_VERSION,
        complete: false,
        after_execution_dir: None,
        covered_execution_dirs: 0,
        // No wall-clock grace proves that an older binary has stopped
        // publishing snapshots without reverse bindings. Only the explicit
        // post-drain cutover scan and immutable seal may retire compatibility.
        mixed_writer_compat_until_ms: i64::MAX,
        cutover_refresh_deployment_id: None,
        cutover_refresh_complete: false,
    }
}

fn persist_base_binding_catalog_marker(
    path: &Path,
    marker: &BaseBindingCatalogMarker,
) -> StoreResult<()> {
    let encoded = serde_json::to_vec(marker).map_err(|error| StoreError::Unavailable {
        detail: format!("base execution catalog marker encode failed: {error}"),
    })?;
    overwrite_derived_atomically(path, &encoded).map_err(|error| StoreError::Unavailable {
        detail: format!("base execution catalog marker publication failed: {error}"),
    })
}

/// Open one catalog lock without following a link, then prove the named path
/// still refers to the same private inode after `flock`. A pathname swap while
/// waiting must never split two writers across different lock objects.
fn acquire_private_catalog_lock(path: &Path) -> std::io::Result<File> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "catalog lock has no parent directory",
        )
    })?;
    create_dir_all_durably(parent)?;
    #[cfg(unix)]
    {
        let metadata = std::fs::symlink_metadata(parent)?;
        if !metadata.file_type().is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() < 1
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "catalog lock directory is not a private owned directory",
            ));
        }
        if metadata.mode() & 0o077 != 0 {
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    options
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let file = options.open(path)?;
    file.lock_exclusive()?;
    #[cfg(unix)]
    {
        let descriptor = file.metadata()?;
        let named = std::fs::symlink_metadata(path)?;
        if !named.file_type().is_file()
            || descriptor.uid() != unsafe { libc::geteuid() }
            || descriptor.nlink() != 1
            || descriptor.mode() & 0o077 != 0
            || descriptor.dev() != named.dev()
            || descriptor.ino() != named.ino()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "catalog lock inode changed or is not private after flock",
            ));
        }
    }
    Ok(file)
}

fn ending_from_index(replayed: ReplayedCursor, last_event_seq: Option<u64>) -> Option<EndedRun> {
    let terminal = replayed.terminal?;
    (!terminal.is_resumable()).then_some(EndedRun {
        seq: replayed.seq,
        terminal,
        last_event_seq,
        has_outbox_metadata: true,
    })
}

/// Append newline-terminated `lines` and make them durable before returning.
///
/// One write for the whole batch rather than one per record, so a crash can tear
/// at most the tail of the batch — and a torn tail is the one damage
/// [`Journal::parse`] tolerates. `sync_data` rather than `sync_all` because the
/// file's metadata does not carry anything a reader depends on; the parent
/// directory is synced only when the file is newly created, which is the only
/// time its *name* is new.
fn append_lines_durably(path: &Path, lines: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        create_dir_all_durably(parent)?;
    }
    let is_new = !path.exists();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let mut payload = String::with_capacity(lines.len() + 1);
    payload.push_str(lines);
    payload.push('\n');
    std::io::Write::write_all(&mut file, payload.as_bytes())?;
    std::io::Write::flush(&mut file)?;
    file.sync_data()?;
    if is_new {
        sync_parent_directory(path)?;
    }
    Ok(())
}

/// Decode journal bytes, keeping a torn tail and refusing damage anywhere else.
///
/// `String::from_utf8` reports the first bad byte and says which kind it is: an
/// `error_len` of `None` means the input ended in the middle of a character,
/// which is an interrupted append and nothing else; `Some` means an invalid
/// sequence with more bytes behind it, which is damage inside a file that is only
/// ever appended to.
fn decode_journal_text(key: &ExecutionKey, bytes: Vec<u8>) -> StoreResult<String> {
    let error = match String::from_utf8(bytes) {
        Ok(text) => return Ok(text),
        Err(error) => error,
    };
    let valid_up_to = error.utf8_error().valid_up_to();
    if error.utf8_error().error_len().is_some() {
        return Err(StoreError::Corrupt {
            key: key.clone(),
            detail: format!(
                "the journal holds an invalid byte sequence at offset {valid_up_to} with bytes \
                 after it, which is damage inside an append-only file rather than an interrupted \
                 append"
            ),
        });
    }
    let mut bytes = error.into_bytes();
    bytes.truncate(valid_up_to);
    String::from_utf8(bytes).map_err(|error| StoreError::Corrupt {
        key: key.clone(),
        detail: format!("the journal's valid prefix is not text: {error}"),
    })
}

fn sync_parent_directory(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

/// Create `path` and durably publish every directory name added on the way.
///
/// Syncing only the directory that receives a file is insufficient when that
/// directory was itself just created: after power loss the file's name may be
/// durable inside a directory whose own name disappeared from its parent. The
/// missing suffix is captured before `create_dir_all`, then every new name is
/// published by syncing its parent from the leaf back toward the first ancestor
/// that was already present.
fn create_dir_all_durably(path: &Path) -> std::io::Result<()> {
    let mut missing = Vec::new();
    let mut cursor = path;
    loop {
        match std::fs::metadata(cursor) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(cursor.to_path_buf());
                let Some(parent) = cursor.parent() else {
                    break;
                };
                cursor = parent;
            },
            Err(error) => return Err(error),
        }
    }

    std::fs::create_dir_all(path)?;
    for directory in missing {
        sync_parent_directory(&directory)?;
    }
    Ok(())
}

fn temp_sibling(path: &Path) -> PathBuf {
    // A sibling, so the link and the rename stay within one filesystem, and
    // uniquely named so two writers never collide on the temporary itself. The
    // suffix deliberately does not end in `.json`, which is what keeps a
    // half-written temporary out of every directory scan in this module.
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("record");
    path.with_file_name(format!(
        "{name}{TEMP_INFIX}{}",
        uuid::Uuid::new_v4().simple()
    ))
}

/// Read a JSON file within byte, node and nesting limits.
///
/// `Ok(None)` means the file is not there. Every other failure — a permission
/// error, a short read, a limit exceeded — is an `Err`, and the distinction is
/// the point: a store that reported an unreadable file as "nothing there" would
/// let a worker start an execution over from a state it simply could not read.
fn read_bounded_json<T: DeserializeOwned>(
    path: &Path,
    max_bytes: u64,
    max_nodes: usize,
) -> std::io::Result<Option<T>> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if metadata.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{} is {} bytes, over the {max_bytes}-byte limit",
                path.display(),
                metadata.len()
            ),
        ));
    }
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let reader = JsonDepthGuardReader::new(std::io::BufReader::new(file), max_bytes, max_nodes);
    serde_json::from_reader(reader)
        .map(Some)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()))
}

/// Apply the exact structural limits used by [`read_bounded_json`] before a
/// record is published.
///
/// A byte-only write check is not symmetric with a depth/node-bounded read: a
/// small JSON array can exceed the node ceiling by orders of magnitude, commit
/// successfully, and make its execution permanently unreadable on the next
/// load. Deserializing into `IgnoredAny` walks the complete document through the
/// same guard without allocating a second copy of the state.
fn validate_json_bytes(bytes: &[u8], max_bytes: u64, max_nodes: usize) -> std::io::Result<()> {
    if bytes.len() as u64 > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "the encoded record is {} bytes, over the {max_bytes}-byte limit",
                bytes.len()
            ),
        ));
    }
    let reader = JsonDepthGuardReader::new(std::io::Cursor::new(bytes), max_bytes, max_nodes);
    serde_json::from_reader::<_, serde::de::IgnoredAny>(reader)
        .map(|_| ())
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()))
}

/// Every `<prefix><n>.json` in `dir`, ascending.
///
/// Names that do not parse are ignored rather than erroring: a temporary that has
/// not been cleaned up, or a file some other tool left, must not make an
/// execution unreadable. What is *not* ignored is the directory being larger than
/// [`MAX_EXECUTION_DIR_ENTRIES`] — which means pruning has stopped running.
///
/// Leaked temporaries used to be the other way to reach this bound, one crash
/// mid-publish at a time, with nothing that ever removed them: an execution
/// became permanently unwritable after enough crashes. [`reap_leaked_temporaries`]
/// now clears them on every claim, so this ceiling is back to meaning what it
/// says.
fn scan_numbered(dir: &Path, prefix: &str) -> std::io::Result<Vec<u64>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut numbers = Vec::new();
    let mut seen = 0usize;
    for entry in entries {
        let entry = entry?;
        seen += 1;
        if seen > MAX_EXECUTION_DIR_ENTRIES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "{} holds more than {MAX_EXECUTION_DIR_ENTRIES} entries; retention has \
                     stopped running",
                    dir.display()
                ),
            ));
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some(rest) = name.strip_prefix(prefix) else {
            continue;
        };
        let Some(digits) = rest.strip_suffix(JSON_SUFFIX) else {
            continue;
        };
        if let Ok(number) = digits.parse::<u64>() {
            numbers.push(number);
        }
    }
    numbers.sort_unstable();
    Ok(numbers)
}

/// Drop all but the newest `retain` numbered files.
///
/// Best effort, and deliberately so: retention failing is not a reason to fail
/// the commit that just succeeded. The bound in [`scan_numbered`] is what turns a
/// persistent failure here into a visible error rather than an unbounded
/// directory.
fn prune_numbered(dir: &Path, prefix: &str, retain: usize) {
    let Ok(numbers) = scan_numbered(dir, prefix) else {
        return;
    };
    if numbers.len() <= retain {
        return;
    }
    for number in &numbers[..numbers.len() - retain] {
        let _ = std::fs::remove_file(dir.join(format!("{prefix}{number}{JSON_SUFFIX}")));
    }
}

/// The suffix [`temp_sibling`] gives every half-written file.
const TEMP_INFIX: &str = ".tmp-";

/// Drop temporaries left by a publish that died between writing one and linking
/// it into place.
///
/// # Why this is not the age-based reap that would race a live writer
///
/// It is keyed on the **lease**, not on a clock. Every writer into an execution's
/// own directory and into its `effects/` directory holds the lease — a commit,
/// an append, an effect row, a lease publish — and this runs at the one moment a
/// caller has just taken that lease at a fence nobody else holds. A temporary
/// still present at that instant belongs to a holder that is no longer one.
///
/// The worst case if that rule is ever broken is bounded and fails in the right
/// direction: a non-holder mid-publish finds its temporary gone, its `link(2)`
/// or `rename(2)` fails, and the call returns `StoreError::Unavailable`. Nothing
/// is corrupted and no acknowledged record is lost — a write that was going to
/// be refused by compare-and-swap anyway simply fails earlier and louder. That
/// asymmetry is the whole argument for reaping here rather than by age.
///
/// # What it deliberately does not touch
///
/// The `wake/` subtree, which is the one place a writer WITHOUT the lease
/// publishes: a job runner or a child execution resolving a token holds nothing.
/// Reaping there could fail a completion, and a completion that fails is a run
/// that never wakes — the failure this store's whole wake mechanism exists to
/// prevent. This scan is shallow and `wake/` is a subdirectory, so it is excluded
/// by construction rather than by a filter someone can delete. Wake operations
/// reap their own temporaries under the token-local mutation lock instead; that
/// lock, rather than the unrelated execution lease, proves no live completer
/// owns the staging file being removed.
///
/// Best effort, like [`prune_numbered`]: a reap that fails must not fail the
/// claim that just succeeded.
/// Delete temporaries left behind by a publish that died between creating one
/// and linking it into place.
///
/// # A lease temporary here may belong to a LIVE claimant
///
/// `claim` calls this immediately on winning, citing the lease as its licence:
/// everything that writes into an execution's directory must hold the lease, so
/// a temporary sitting there must be a dead publish's.
///
/// **That holds for every temporary except a lease's own.** Publishing a lease
/// is precisely what a worker does *before* it holds one, so while the winner
/// reaps, a LOSER may be sitting between `publish_new_file`'s write and its
/// `hard_link` with a live temporary on disk — and this deletes it.
///
/// It is deliberately still deleted. The alternative was an age threshold, and
/// that trades this bug for a worse one: `taking_the_lease_reaps_temporaries_a_
/// crashed_publish_left_behind` requires a crashed publish's lease temporary to
/// be reaped by the very next claim, and no age rule can honour that while also
/// sparing a temporary written microseconds ago. The two are indistinguishable
/// by any property of the file.
///
/// So the ambiguity is resolved where it actually exists — in
/// [`publish_new_file`], which answers a reaped temporary with
/// [`Publish::AlreadyExists`] when the record it was going to publish is now
/// present. See that function's *A REAPED TEMPORARY IS A LOST RACE*. Found
/// 2026-08-28 by `exactly_one_of_eight_racing_claims_takes_the_lease`, which is
/// load- and filesystem-sensitive and had been passing by luck.
fn reap_leaked_temporaries(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut reaped = 0usize;
    let mut seen = 0usize;
    for entry in entries.flatten() {
        seen += 1;
        // The same bound the numbered scans use. A directory already past it is
        // being surfaced by those, and walking it here would pay the cost this
        // reap exists to remove.
        if seen > MAX_EXECUTION_DIR_ENTRIES {
            break;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.contains(TEMP_INFIX) {
            continue;
        }
        // Files only. Nothing in this store publishes a temporary directory, and
        // recursing would put `wake/` back in scope by accident.
        if entry
            .file_type()
            .map(|kind| kind.is_file())
            .unwrap_or(false)
            && std::fs::remove_file(entry.path()).is_ok()
        {
            reaped += 1;
        }
    }
    reaped
}

fn ttl_millis(ttl: Duration) -> i64 {
    i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX)
}

fn io_error(key: &ExecutionKey, error: std::io::Error) -> StoreError {
    StoreError::Unavailable {
        detail: format!("{key}: {error}"),
    }
}

fn unavailable(error: std::io::Error) -> StoreError {
    StoreError::Unavailable {
        detail: error.to_string(),
    }
}

fn corrupt(key: &ExecutionKey, error: std::io::Error) -> StoreError {
    // A read that failed its own limits is corruption; one that failed for any
    // other reason is the substrate being unavailable. Collapsing the two would
    // quarantine an execution because a disk was briefly busy.
    if error.kind() == std::io::ErrorKind::InvalidData {
        StoreError::Corrupt {
            key: key.clone(),
            detail: error.to_string(),
        }
    } else {
        io_error(key, error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::execution::agentic::run_loop::effects::{
        CommittedActRef, PendingEffect, RetrySafety,
    };
    use crate::magician_v2::execution::agentic::run_loop::journal::{
        RecordedStep, TerminalKind, MAX_JOURNAL_RECORD_BYTES,
    };
    use crate::magician_v2::execution::agentic::run_loop::outcome::Phase;
    use crate::magician_v2::execution::agentic::run_loop::state::{
        LoopCursor, LoopSegmentBinding, ResolveCheckpoint,
    };
    use crate::magician_v2::execution::agentic::run_loop::store::contract::{
        self, ContractHarness,
    };
    use crate::magician_v2::execution::agentic::run_loop::store::loop_state_store_contract;

    pub struct FsHarness {
        // Held so the directory outlives the stores that read from it. A harness
        // that returned only the store would drop this first and every case
        // would fail on a path that no longer exists.
        dir: tempfile::TempDir,
        store: FsLoopStateStore,
    }

    impl ContractHarness for FsHarness {
        type Store = FsLoopStateStore;

        fn create() -> Self {
            let dir = tempfile::tempdir().expect("a temporary directory");
            let store = FsLoopStateStore::new(dir.path());
            Self { dir, store }
        }

        fn store(&self) -> &Self::Store {
            &self.store
        }

        fn reopen(&self) -> Self::Store {
            // A fresh store over the same root, holding nothing the first one
            // learned. This is the shape a second worker — or the same worker
            // after a restart — arrives in.
            FsLoopStateStore::new(self.dir.path())
        }
    }

    fn pending_effect(id: &str) -> PendingEffect {
        PendingEffect {
            effect_id: EffectId::parse(id).expect("a fixture id is well-formed"),
            tool: "gmail__send".to_string(),
            arguments_fingerprint: "fp-1".to_string(),
            retry_safety: RetrySafety::NotRetrySafe,
            // An outward send, so it carries the act ref it will reconcile
            // against rather than re-deriving one, bound to the scope that
            // derived it.
            reconcile_ref: Some(
                CommittedActRef::new(
                    format!("act-{}", "0123456789abcdef".repeat(2)),
                    "anonymous",
                    "default",
                )
                .expect("the fixture ref must be the shape derive_act_ref mints"),
            ),
            // A send is not reattachable, so nothing names a job to resume.
            reattach_ref: None,
        }
    }

    loop_state_store_contract!(FsHarness);

    #[tokio::test]
    async fn filesystem_transactions_run_off_the_async_runtime_thread() {
        let runtime_thread = std::thread::current().id();
        let filesystem_thread = run_fs_io(|| Ok(std::thread::current().id()))
            .await
            .expect("blocking task");
        assert_ne!(
            filesystem_thread, runtime_thread,
            "a synchronous filesystem transaction must not pin the async worker"
        );
    }

    #[tokio::test]
    async fn durable_journal_index_bounds_replay_append_commit_and_projection_to_the_tail() {
        let harness = FsHarness::create();
        let key = contract::key("indexed-tail");
        let mut state = contract::fresh_state(&key);
        let mut revision = harness
            .store()
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("seed commit");

        let prepare_seq = harness
            .store()
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Prepare,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("indexed prepare append");
        state.journal_seq = prepare_seq;
        state.cursor = LoopCursor {
            iteration: 1,
            phase: Phase::Observe,
        };
        revision = harness
            .store()
            .commit(&key, &state, revision)
            .await
            .expect("indexed prepare commit");

        let observe_seq = harness
            .store()
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Observe,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("indexed observe append");
        state.journal_seq = observe_seq;
        state.cursor = LoopCursor {
            iteration: 1,
            phase: Phase::Decide,
        };
        harness
            .store()
            .commit(&key, &state, revision)
            .await
            .expect("indexed observe commit");

        let reopened = harness.reopen();
        let index = reopened
            .current_journal_index(&key)
            .expect("a restart sees the durable index");
        assert_eq!(index.committed_watermark, observe_seq);
        assert_eq!(index.replayed.iteration, 1);
        assert_eq!(index.replayed.phase, Phase::Decide);
        assert_ne!(index.chain_sha256, "0".repeat(64));

        let projection = reopened
            .read_journal_projection_sync(&key, observe_seq, observe_seq, false)
            .expect("the exact indexed tail projects");
        assert_eq!(
            projection
                .iter()
                .map(|record| record.seq)
                .collect::<Vec<_>>(),
            vec![observe_seq],
            "the indexed projection must not materialise the committed prefix"
        );
        assert_eq!(
            reopened
                .replay_committed_journal_sync(&key, observe_seq)
                .expect("indexed replay"),
            index.replayed
        );

        let index_path = reopened.journal_index_path(&key);
        let mut altered: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&index_path).expect("read durable index"))
                .expect("index JSON");
        altered["replayed"]["iteration"] = serde_json::json!(99);
        overwrite_atomically(
            &index_path,
            &serde_json::to_vec(&altered).expect("mutated index JSON"),
        )
        .expect("publish a valid-JSON index corruption");
        assert!(
            reopened.current_journal_index(&key).is_none(),
            "a semantic field edit without a matching integrity digest must invalidate the index"
        );
        assert_eq!(
            reopened
                .replay_committed_journal_sync(&key, observe_seq)
                .expect("invalid derived index falls back to the journal")
                .iteration,
            1,
            "derived-state corruption must not change replay semantics"
        );
    }

    #[tokio::test]
    async fn durable_owner_deadline_covers_live_lease_pin_and_expired_authority() {
        let harness = FsHarness::create();
        let now_ms = Utc::now().timestamp_millis();

        let leased = contract::key("durable-owner-live-lease");
        harness
            .store()
            .commit(&leased, &contract::fresh_state(&leased), Revision::INITIAL)
            .await
            .expect("seed leased execution");
        let lease = harness
            .store()
            .claim(&leased, &WorkerId::new("worker-a"), Duration::from_secs(60))
            .await
            .expect("live lease");
        assert_eq!(
            harness
                .store()
                .durable_owner_until(&leased, now_ms)
                .await
                .expect("read live lease"),
            Some(lease.expires_at_ms)
        );

        let pinned = contract::key("durable-owner-live-pin");
        let mut pinned_state = contract::fresh_state(&pinned);
        let pin_until = now_ms.saturating_add(90_000);
        pinned_state.placement = Placement::Pinned {
            worker: WorkerId::new("worker-b"),
            pinned_until_ms: pin_until,
        };
        harness
            .store()
            .commit(&pinned, &pinned_state, Revision::INITIAL)
            .await
            .expect("commit live pin without a lease");
        assert_eq!(
            harness
                .store()
                .durable_owner_until(&pinned, now_ms)
                .await
                .expect("read live pin"),
            Some(pin_until)
        );

        let expired = contract::key("durable-owner-expired");
        let mut expired_state = contract::fresh_state(&expired);
        expired_state.placement = Placement::Pinned {
            worker: WorkerId::new("worker-c"),
            pinned_until_ms: now_ms.saturating_sub(1),
        };
        harness
            .store()
            .commit(&expired, &expired_state, Revision::INITIAL)
            .await
            .expect("commit expired pin");
        let expired_lease = harness
            .store()
            .claim(
                &expired,
                &WorkerId::new("worker-c"),
                Duration::from_secs(60),
            )
            .await
            .expect("claim after pin lapse");
        harness
            .store()
            .release(expired_lease)
            .await
            .expect("release lease");
        assert_eq!(
            harness
                .store()
                .durable_owner_until(&expired, now_ms)
                .await
                .expect("read expired authority"),
            None
        );
    }

    #[tokio::test]
    async fn durable_owner_uses_bounded_placement_index_without_loading_snapshot() {
        let harness = FsHarness::create();
        let key = contract::key("durable-owner-bounded-placement-index");
        let now_ms = Utc::now().timestamp_millis();
        let pin_until = now_ms.saturating_add(90_000);
        let mut state = contract::fresh_state(&key);
        state.placement = Placement::Pinned {
            worker: WorkerId::new("worker-indexed"),
            pinned_until_ms: pin_until,
        };
        harness
            .store()
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit indexed placement");
        harness
            .store()
            .full_snapshot_loads
            .store(0, AtomicOrdering::SeqCst);

        assert_eq!(
            harness
                .store()
                .durable_owner_until(&key, now_ms)
                .await
                .expect("read indexed owner"),
            Some(pin_until)
        );
        assert_eq!(
            harness
                .store()
                .full_snapshot_loads
                .load(AtomicOrdering::SeqCst),
            0,
            "the owner hot path must read only the bounded projection"
        );
    }

    #[tokio::test]
    async fn corrupt_placement_index_falls_back_once_and_repairs() {
        let harness = FsHarness::create();
        let key = contract::key("durable-owner-corrupt-placement-index");
        let now_ms = Utc::now().timestamp_millis();
        let pin_until = now_ms.saturating_add(90_000);
        let mut state = contract::fresh_state(&key);
        state.placement = Placement::Pinned {
            worker: WorkerId::new("worker-corrupt-index"),
            pinned_until_ms: pin_until,
        };
        harness
            .store()
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit indexed placement");
        let index_path = harness.store().placement_index_path(&key);
        let mut altered: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&index_path).expect("read placement index"))
                .expect("placement index JSON");
        altered["placement"]["pinned_until_ms"] = serde_json::json!(now_ms - 1);
        overwrite_derived_atomically(
            &index_path,
            &serde_json::to_vec(&altered).expect("mutated placement index JSON"),
        )
        .expect("publish semantic placement index corruption");
        harness
            .store()
            .full_snapshot_loads
            .store(0, AtomicOrdering::SeqCst);

        assert_eq!(
            harness
                .store()
                .durable_owner_until(&key, now_ms)
                .await
                .expect("corrupt index falls back"),
            Some(pin_until)
        );
        assert_eq!(
            harness
                .store()
                .full_snapshot_loads
                .load(AtomicOrdering::SeqCst),
            1
        );
        harness
            .store()
            .full_snapshot_loads
            .store(0, AtomicOrdering::SeqCst);
        assert_eq!(
            harness
                .store()
                .durable_owner_until(&key, now_ms)
                .await
                .expect("repaired index is reusable"),
            Some(pin_until)
        );
        assert_eq!(
            harness
                .store()
                .full_snapshot_loads
                .load(AtomicOrdering::SeqCst),
            0
        );
    }

    #[tokio::test]
    async fn stale_placement_index_cannot_answer_for_a_newer_snapshot() {
        let harness = FsHarness::create();
        let key = contract::key("durable-owner-stale-placement-index");
        let now_ms = Utc::now().timestamp_millis();
        let first_pin = now_ms.saturating_add(60_000);
        let second_pin = now_ms.saturating_add(120_000);
        let mut state = contract::fresh_state(&key);
        state.placement = Placement::Pinned {
            worker: WorkerId::new("worker-first"),
            pinned_until_ms: first_pin,
        };
        let first_revision = harness
            .store()
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit first placement");
        let stale = std::fs::read(harness.store().placement_index_path(&key))
            .expect("read first placement index");
        state.placement = Placement::Pinned {
            worker: WorkerId::new("worker-second"),
            pinned_until_ms: second_pin,
        };
        harness
            .store()
            .commit(&key, &state, first_revision)
            .await
            .expect("commit second placement");
        overwrite_derived_atomically(&harness.store().placement_index_path(&key), &stale)
            .expect("restore stale derived index");
        harness
            .store()
            .full_snapshot_loads
            .store(0, AtomicOrdering::SeqCst);

        assert_eq!(
            harness
                .store()
                .durable_owner_until(&key, now_ms)
                .await
                .expect("stale index falls back"),
            Some(second_pin),
            "a prior revision's projection must never hide the current pin"
        );
        assert_eq!(
            harness
                .store()
                .full_snapshot_loads
                .load(AtomicOrdering::SeqCst),
            1
        );
    }

    #[tokio::test]
    async fn projection_falls_back_when_the_requested_prefix_predates_the_tail_index() {
        let harness = FsHarness::create();
        let key = contract::key("projection-before-index-window");
        let mut appends = Vec::with_capacity(MAX_JOURNAL_INDEX_OFFSETS + 2);
        for ordinal in 0..=MAX_JOURNAL_INDEX_OFFSETS {
            let mut append = JournalAppend::named_event(
                1,
                Phase::Prepare,
                "agentic.iteration_started",
                "agent-1",
                None,
                None,
                serde_json::json!({"ordinal": ordinal}),
            )
            .expect("bounded named event");
            append.ordinal = ordinal as u32;
            appends.push(append);
        }
        appends.push(JournalAppend::phase_completed(
            1,
            Phase::Prepare,
            RecordedStep::Continued,
        ));
        let watermark = harness
            .store()
            .append_journal(&key, &appends)
            .await
            .expect("append a prefix wider than the bounded index");
        let mut state = contract::fresh_state(&key);
        state.journal_seq = watermark;
        state.cursor = LoopCursor {
            iteration: 1,
            phase: Phase::Observe,
        };
        harness
            .store()
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit wide prefix");

        let index = harness
            .store()
            .current_journal_index(&key)
            .expect("wide journal still has a bounded tail index");
        assert!(index.offsets.first().expect("tail offset").seq > 1);
        let projection = harness
            .store()
            .read_journal_projection_sync(&key, 1, watermark, false)
            .expect("missing indexed coverage falls back to the full parser");
        assert_eq!(projection.first().map(|record| record.seq), Some(1));
        assert_eq!(projection.last().map(|record| record.seq), Some(watermark));
    }

    #[tokio::test]
    async fn orphan_retention_never_projects_the_swept_file_past_its_byte_cap() {
        for (execution_id, keep_index) in
            [("indexed-swept-cap", true), ("full-parse-swept-cap", false)]
        {
            let harness = FsHarness::create();
            let key = contract::key(execution_id);
            harness
                .store()
                .commit(&key, &contract::fresh_state(&key), Revision::INITIAL)
                .await
                .expect("seed snapshot");
            let append = JournalAppend::phase_completed(1, Phase::Prepare, RecordedStep::Continued);
            harness
                .store()
                .append_journal(&key, std::slice::from_ref(&append))
                .await
                .expect("orphan append");
            if !keep_index {
                std::fs::remove_file(harness.store().journal_index_path(&key))
                    .expect("force full-parser recovery path");
            }
            let swept_path = harness.store().swept_journal_path(&key);
            let swept = OpenOptions::new()
                .create(true)
                .write(true)
                .open(&swept_path)
                .expect("create sparse swept ledger");
            swept
                .set_len(MAX_JOURNAL_BYTES - 1)
                .expect("place swept ledger just below cap");
            swept.sync_all().expect("publish sparse length");

            harness
                .store()
                .append_journal(&key, &[append])
                .await
                .expect("orphan is swept even when retention is full");
            assert_eq!(
                std::fs::metadata(&swept_path)
                    .expect("swept ledger metadata")
                    .len(),
                MAX_JOURNAL_BYTES - 1,
                "retention must account for the full payload and delimiter before append"
            );
        }
    }

    #[tokio::test]
    async fn a_committed_state_lands_under_the_scoped_v3_runtime_root() {
        // Not a restatement of the path constant: this asserts the store writes
        // where every other scoped tool looks, so an operator finds a run's loop
        // state beside its pause records rather than in a parallel tree.
        let harness = FsHarness::create();
        let key = contract::key("layout");
        harness
            .store()
            .commit(&key, &contract::fresh_state(&key), Revision::INITIAL)
            .await
            .expect("commit");

        let dir = harness.store().execution_dir(&key);
        assert!(dir.join("snapshot-1.json").is_file(), "{}", dir.display());
        assert!(dir.join("key.json").is_file());
        let rendered = dir.display().to_string();
        assert!(
            rendered.contains("/runtime/executions/layout"),
            "{rendered}"
        );
    }

    /// A park is dated by the commit that made it, not by the pass that reads it.
    ///
    /// The contract case for `parked_since_ms` cannot pin this: it commits and
    /// lists in the same millisecond, so an implementation answering "now" is
    /// indistinguishable from one answering "when the snapshot was written". This
    /// separates them by backdating the snapshot an hour, which is the only thing
    /// the age gate in `run_loop::reconciler` actually depends on — a store that
    /// answered `now` would make every park read as brand new and the reconciler
    /// would never examine one.
    #[tokio::test]
    async fn a_park_is_dated_by_the_commit_that_made_it_rather_than_by_the_read() {
        let harness = FsHarness::create();
        let key = contract::key("dated");
        let mut state = contract::fresh_state(&key);
        state.wait = Some(
            crate::magician_v2::execution::agentic::run_loop::state::WaitReason::Job {
                job_id: "coding-1".to_string(),
            },
        );
        harness
            .store()
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit");

        let snapshot = harness.store().snapshot_path(&key, Revision::from_u64(1));
        let an_hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3_600);
        std::fs::File::options()
            .write(true)
            .open(&snapshot)
            .expect("the snapshot is there to backdate")
            .set_modified(an_hour_ago)
            .expect("the temporary filesystem keeps mtimes");

        let listing = harness.store().list_parked(50).await.expect("list");
        let dated = listing
            .parked
            .iter()
            .find(|row| row.key == key)
            .expect("the park is listed")
            .parked_since_ms
            .expect("and is dated");
        let age_ms = Utc::now().timestamp_millis() - dated;
        assert!(
            (3_500_000..4_100_000).contains(&age_ms),
            "the park should read as about an hour old, and reads as {age_ms}ms — a store \
             answering with the time of the read rather than of the commit would report ~0"
        );
    }

    #[tokio::test]
    async fn a_torn_journal_tail_does_not_stop_the_next_append() {
        // The crash this survives: the process died part way through writing a
        // record. The record was never acknowledged, so the next append takes
        // its seq — and the file must not be unreadable in the meantime.
        let harness = FsHarness::create();
        let key = contract::key("torn");
        harness
            .store()
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Prepare,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("append");

        let path = harness.store().journal_path(&key);
        let mut raw = std::fs::read_to_string(&path).expect("read");
        raw.push_str("{\"seq\":2,\"iterat");
        std::fs::write(&path, raw).expect("write");

        let records = harness.store().read_journal(&key, 0).await.expect("read");
        assert_eq!(records.len(), 1, "the torn tail is dropped, not fatal");

        let seq = harness
            .store()
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Observe,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("append after a torn tail");
        assert_eq!(
            seq, 1,
            "nothing is committed, so the sweep takes it back to zero"
        );
    }

    #[tokio::test]
    async fn a_corrupt_state_is_reported_rather_than_read_as_absent() {
        // The failure this prevents: a store that answered "no state" for a file
        // it could not read would have a worker restart an execution that is
        // half way through.
        let harness = FsHarness::create();
        let key = contract::key("corrupt");
        harness
            .store()
            .commit(&key, &contract::fresh_state(&key), Revision::INITIAL)
            .await
            .expect("commit");

        let snapshot = harness.store().snapshot_path(&key, Revision::from_u64(1));
        std::fs::write(&snapshot, b"{ not json").expect("write");

        let error = harness
            .store()
            .load(&key)
            .await
            .expect_err("an unreadable state must not read as an absent one");
        assert!(matches!(error, StoreError::Corrupt { .. }), "got {error}");
    }

    #[tokio::test]
    async fn a_structurally_oversized_state_is_refused_before_it_is_published() {
        // A flat array can stay far below MAX_STATE_BYTES while crossing the
        // reader's node ceiling. The old byte-only write check accepted this
        // state and the next load quarantined the execution as corrupt.
        let harness = FsHarness::create();
        let key = contract::key("state-node-bound");
        let mut state = contract::fresh_state(&key);
        state.resolve_checkpoint = Some(
            ResolveCheckpoint::try_new(serde_json::Value::Array(vec![
                serde_json::Value::Null;
                MAX_STATE_JSON_NODES
            ]))
            .expect("the checkpoint byte bound is deliberately larger than this fixture"),
        );

        let error = harness
            .store()
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect_err("a state the reader would reject must not be published");
        assert!(matches!(error, StoreError::Corrupt { .. }), "got {error}");
        assert!(
            !harness
                .store()
                .snapshot_path(&key, Revision::INITIAL.next())
                .exists(),
            "refusal must happen before the compare-and-swap publishes a revision"
        );
    }

    #[tokio::test]
    async fn concurrent_wake_publishers_cannot_race_past_the_capacity() {
        let harness = FsHarness::create();
        let key = contract::key("wake-capacity-race");
        let token = "job:coding-race";
        let dir = harness.store().wake_dir(&key, token);
        std::fs::create_dir_all(&dir).expect("fixture directory");
        // Populate names directly: capacity admission counts published JSON
        // rows and deliberately does not parse all of them on the hot path.
        for index in 0..MAX_WAKE_RESOLUTIONS - 1 {
            std::fs::write(dir.join(format!("{index:064x}.json")), b"{}").expect("fixture row");
        }

        let mut publishers = Vec::new();
        for index in 0..8 {
            let store = harness.store().clone();
            let key = key.clone();
            publishers.push(tokio::spawn(async move {
                store
                    .resolve_wake(&key, token, &format!("resolution-{index}"))
                    .await
            }));
        }
        let mut admitted = 0usize;
        for publisher in publishers {
            match publisher.await.expect("publisher task") {
                Ok(()) => admitted += 1,
                Err(StoreError::WakeLedgerFull { .. }) => {},
                Err(error) => panic!("unexpected wake result: {error}"),
            }
        }
        assert_eq!(admitted, 1, "exactly the remaining slot may be claimed");
        assert_eq!(
            harness
                .store()
                .wake_resolution_paths(&key, token)
                .expect("list")
                .len(),
            MAX_WAKE_RESOLUTIONS
        );
    }

    #[tokio::test]
    async fn a_wake_row_must_bind_its_token_and_resolution_to_its_address() {
        let harness = FsHarness::create();
        let key = contract::key("wake-row-address-binding");
        let token = "job:expected";
        let resolution_id = "completion-1";
        let path = harness
            .store()
            .wake_resolution_path(&key, token, resolution_id);
        std::fs::create_dir_all(path.parent().expect("wake directory")).expect("fixture directory");
        std::fs::write(
            &path,
            serde_json::to_vec(&StoredWakeResolution {
                wake_token: "job:another".to_owned(),
                resolution_id: resolution_id.to_owned(),
            })
            .expect("fixture row"),
        )
        .expect("write fixture");

        let error = harness
            .store()
            .wake_resolutions(&key, token)
            .await
            .expect_err("a row from another token must not satisfy this park");
        assert!(matches!(error, StoreError::Corrupt { .. }), "got {error}");
    }

    #[tokio::test]
    async fn an_idempotent_wake_retry_refuses_a_corrupt_existing_row() {
        let harness = FsHarness::create();
        let key = contract::key("wake-retry-address-binding");
        let token = "job:expected";
        let resolution_id = "completion-1";
        let path = harness
            .store()
            .wake_resolution_path(&key, token, resolution_id);
        std::fs::create_dir_all(path.parent().expect("wake directory")).expect("fixture directory");
        std::fs::write(
            &path,
            serde_json::to_vec(&StoredWakeResolution {
                wake_token: token.to_owned(),
                resolution_id: "completion-under-another-address".to_owned(),
            })
            .expect("fixture row"),
        )
        .expect("write fixture");

        let error = harness
            .store()
            .resolve_wake(&key, token, resolution_id)
            .await
            .expect_err("an existing corrupt row is not an idempotent success");
        assert!(matches!(error, StoreError::Corrupt { .. }), "got {error}");
    }

    #[tokio::test]
    async fn reading_a_wake_token_reaps_a_crashed_publish_temporary() {
        let harness = FsHarness::create();
        let key = contract::key("wake-temp-reap");
        let token = "job:coding-temp";
        let dir = harness.store().wake_dir(&key, token);
        std::fs::create_dir_all(&dir).expect("fixture directory");
        let leaked = dir.join("resolution.json.tmp-deadpublisher");
        std::fs::write(&leaked, b"partial").expect("fixture temporary");

        assert_eq!(
            harness
                .store()
                .wake_resolutions(&key, token)
                .await
                .expect("read"),
            Vec::<String>::new()
        );
        assert!(!leaked.exists(), "the token lock makes this reap race-free");
    }

    #[tokio::test]
    async fn a_wake_resolution_too_large_to_read_is_refused_before_publish() {
        let harness = FsHarness::create();
        let key = contract::key("wake-write-bound");
        let token = "job:coding-large-report";
        let resolution_id = "x".repeat(MAX_WAKE_BYTES as usize);

        let error = harness
            .store()
            .resolve_wake(&key, token, &resolution_id)
            .await
            .expect_err("the encoded envelope is larger than the read boundary");
        assert!(matches!(error, StoreError::Corrupt { .. }), "got {error}");
        assert!(
            harness
                .store()
                .wake_resolution_paths(&key, token)
                .expect("list")
                .is_empty(),
            "a refused resolution must not consume a capacity slot"
        );
    }

    #[tokio::test]
    async fn two_scopes_that_normalise_to_one_directory_are_refused() {
        // The scope segments go through the workspace layout's normalisation,
        // which maps several distinct principals onto one directory name. Two
        // runs silently sharing a journal and a lease is the failure; the key
        // record turns it into a refusal.
        let harness = FsHarness::create();
        let first = ExecutionKey::new("own/er", "default", "collide").expect("well-formed");
        let second = ExecutionKey::new("own:er", "default", "collide").expect("well-formed");
        assert_eq!(
            harness.store().execution_dir(&first),
            harness.store().execution_dir(&second),
            "this test is only meaningful if the two normalise together"
        );

        harness
            .store()
            .commit(&first, &contract::fresh_state(&first), Revision::INITIAL)
            .await
            .expect("the first scope commits");
        let error = harness
            .store()
            .commit(&second, &contract::fresh_state(&second), Revision::INITIAL)
            .await
            .expect_err("the second scope must not join the first's directory");
        // Specifically `Corrupt`, not `Conflict`. A conflict is a RETRYABLE
        // answer — it tells the caller to reload and try again — and this caller
        // must never reload, because what it would load is somebody else's run.
        assert!(matches!(error, StoreError::Corrupt { .. }), "got {error}");

        // And the read side refuses too. Refusing only at commit left the leak
        // wide open: the colliding scope simply asks for the state and is handed
        // the first scope's, across the boundary the key record exists to draw.
        let error = harness
            .store()
            .load(&second)
            .await
            .expect_err("a load must not answer with another scope's state");
        assert!(matches!(error, StoreError::Corrupt { .. }), "got {error}");

        // The owning scope still loads its own state, or the refusal above would
        // be satisfied by a store that refused everybody.
        assert!(
            harness
                .store()
                .load(&first)
                .await
                .expect("the owning scope loads")
                .is_some(),
            "the key record must gate the neighbour, not the owner"
        );
    }

    #[tokio::test]
    async fn a_colliding_scope_is_refused_at_every_door_and_not_only_at_two() {
        // Refusing at `commit` and `load` alone left twelve other doors into the
        // same directory. These are the ones that carry content: `read_journal`
        // hands back every event payload the neighbouring run wrote,
        // `load_effects` hands back its whole ledger, and `claim` takes its lease
        // — which is the licence `reap_leaked_temporaries` runs under, so the
        // colliding caller would also delete its in-flight publishes.
        let harness = FsHarness::create();
        let owner = ExecutionKey::new("own/er", "default", "every-door").expect("well-formed");
        let neighbour = ExecutionKey::new("own:er", "default", "every-door").expect("well-formed");
        assert_eq!(
            harness.store().execution_dir(&owner),
            harness.store().execution_dir(&neighbour),
            "this test is only meaningful if the two normalise together"
        );

        let store = harness.store();
        store
            .commit(&owner, &contract::fresh_state(&owner), Revision::INITIAL)
            .await
            .expect("the owning scope commits");
        store
            .append_journal(
                &owner,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Prepare,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("and writes a record the neighbour must not read");
        store
            .record_effect_intent(
                &owner,
                &EffectLedgerEntry::intent(
                    &pending_effect("llm-1:tool:call-1"),
                    1,
                    Phase::Apply,
                    1_000,
                ),
            )
            .await
            .expect("and an effect row");

        let lease = Lease {
            key: neighbour.clone(),
            worker: WorkerId::new("worker-b"),
            fence: 1,
            expires_at_ms: i64::MAX,
        };
        let refusals: Vec<(&str, StoreError)> = vec![
            (
                "read_journal",
                store
                    .read_journal(&neighbour, 0)
                    .await
                    .expect_err("a neighbour must not read this journal"),
            ),
            (
                // A SECOND door onto the same bytes, and it is its own door
                // rather than a wrapper around the one above:
                // `read_journal_verified` is overridden on this store to return
                // `read_journal_file`'s journal directly, and `read_journal_file`
                // goes straight to `journal_path` without a scope check. So the
                // override carries its own `ensure_directory_is_not_another_scopes`
                // — and an override that dropped that line would hand a
                // neighbouring scope every event payload in this log while the
                // entry above still passed.
                "read_journal_verified",
                store
                    .read_journal_verified(&neighbour)
                    .await
                    .expect_err("a neighbour must not read this journal by the checked door"),
            ),
            (
                "load_effects",
                store
                    .load_effects(&neighbour)
                    .await
                    .err()
                    .expect("a neighbour must not read this ledger"),
            ),
            (
                "record_effect_intent",
                store
                    .record_effect_intent(
                        &neighbour,
                        &EffectLedgerEntry::intent(
                            &pending_effect("llm-2:tool:call-1"),
                            1,
                            Phase::Apply,
                            1_000,
                        ),
                    )
                    .await
                    .expect_err("a neighbour must not write into this ledger"),
            ),
            (
                "record_effect_outcome",
                store
                    .record_effect_outcome(
                        &neighbour,
                        &EffectId::parse("llm-1:tool:call-1").expect("well-formed"),
                        EffectOutcome::Succeeded { at_ms: 2_000 },
                    )
                    .await
                    .expect_err("a neighbour must not settle this run's effects"),
            ),
            (
                "append_journal",
                store
                    .append_journal(
                        &neighbour,
                        &[JournalAppend::phase_completed(
                            1,
                            Phase::Observe,
                            RecordedStep::Continued,
                        )],
                    )
                    .await
                    .expect_err("a neighbour must not append to this journal"),
            ),
            (
                "claim",
                store
                    .claim(
                        &neighbour,
                        &WorkerId::new("worker-b"),
                        Duration::from_secs(60),
                    )
                    .await
                    .expect_err("a neighbour must not take this run's lease"),
            ),
            (
                "renew",
                store
                    .renew(&lease, Duration::from_secs(60))
                    .await
                    .expect_err("nor renew one"),
            ),
            (
                "release",
                store
                    .release(lease.clone())
                    .await
                    .expect_err("nor release one"),
            ),
            (
                "resolve_wake",
                store
                    .resolve_wake(&neighbour, "job:coding-7", "report-1")
                    .await
                    .expect_err("nor wake this run"),
            ),
            (
                "wake_resolutions",
                store
                    .wake_resolutions(&neighbour, "job:coding-7")
                    .await
                    .err()
                    .expect("nor read what would wake it"),
            ),
            (
                "consume_wake",
                store
                    .consume_wake(&neighbour, "job:coding-7", &["report-1".to_string()])
                    .await
                    .err()
                    .expect("nor consume it"),
            ),
        ];
        for (door, error) in refusals {
            // `Corrupt`, never `Conflict`. A conflict is RETRYABLE — it tells the
            // caller to reload and try again — and this caller must never
            // reload, because what it would load is somebody else's run.
            assert!(
                matches!(error, StoreError::Corrupt { .. }),
                "{door} let a colliding scope through: {error}"
            );
        }

        // And the owner is untouched at every one of them, or the assertions
        // above would be satisfied by a store that refused everybody.
        assert_eq!(
            store
                .read_journal(&owner, 0)
                .await
                .expect("the owner reads")
                .len(),
            1
        );
        assert_eq!(
            store
                .read_journal_verified(&owner)
                .await
                .expect("the owner reads by the checked door too")
                .all_records()
                .len(),
            1,
            "the scope check must refuse the neighbour without also refusing the owner; a door \
             that answered `Corrupt` to everybody would satisfy the refusal above"
        );
        assert!(store
            .load_effects(&owner)
            .await
            .expect("the owner's ledger")
            .get(&EffectId::parse("llm-1:tool:call-1").expect("well-formed"))
            .is_some());
        store
            .claim(&owner, &WorkerId::new("worker-a"), Duration::from_secs(60))
            .await
            .expect("the owner takes its own lease");
    }

    #[tokio::test]
    async fn the_first_write_stamps_the_directory_even_without_a_commit() {
        // The window the read-only check cannot close: before anybody commits
        // there is no key record, so a colliding scope's append would join the
        // journal and the two would interleave their seqs. The write paths stamp
        // the record, so ownership is settled by the first byte written rather
        // than by the first commit.
        let harness = FsHarness::create();
        let owner = ExecutionKey::new("own/er", "default", "stamped").expect("well-formed");
        let neighbour = ExecutionKey::new("own:er", "default", "stamped").expect("well-formed");

        harness
            .store()
            .append_journal(
                &owner,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Prepare,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("the owner appends first, and commits nothing");

        let error = harness
            .store()
            .append_journal(
                &neighbour,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Prepare,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect_err("a colliding scope must not append into the same log");
        assert!(matches!(error, StoreError::Corrupt { .. }), "got {error}");

        // The owner's log still holds exactly its own record.
        assert_eq!(
            harness
                .store()
                .read_journal(&owner, 0)
                .await
                .expect("read")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn a_swept_attempt_is_retained_for_an_operator() {
        let harness = FsHarness::create();
        let key = contract::key("swept");
        let mut state = contract::fresh_state(&key);

        harness
            .store()
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Prepare,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("append");
        state.journal_seq = 1;
        harness
            .store()
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit");

        // An attempt that appended and never committed.
        harness
            .store()
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Observe,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("append");
        // The next attempt sweeps it.
        harness
            .store()
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Observe,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("append");

        let swept = std::fs::read_to_string(harness.store().swept_journal_path(&key))
            .expect("the swept attempt is retained rather than discarded");
        assert!(swept.contains("\"seq\":2"), "{swept}");
    }

    #[tokio::test]
    async fn superseded_revisions_are_pruned_but_the_newest_is_not() {
        let harness = FsHarness::create();
        let key = contract::key("pruning");
        let state = contract::fresh_state(&key);
        let mut revision = Revision::INITIAL;
        for _ in 0..(RETAINED_SNAPSHOTS + 3) {
            revision = harness
                .store()
                .commit(&key, &state, revision)
                .await
                .expect("commit");
        }

        let kept =
            scan_numbered(&harness.store().execution_dir(&key), SNAPSHOT_PREFIX).expect("scan");
        assert_eq!(kept.len(), RETAINED_SNAPSHOTS);
        assert_eq!(kept.last().copied(), Some(revision.as_u64()));
        assert!(harness.store().load(&key).await.expect("load").is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn exactly_one_of_eight_racing_first_commits_wins() {
        // Every other case in this file is sequential, and a sequential case
        // cannot tell a compare-and-swap from a read followed by a write: the
        // scan that decides the next revision and the publish that takes it are
        // two syscalls with a window between them. Here the window is real, and
        // the only thing closing it is `link(2)` refusing a name that exists.
        let dir = tempfile::tempdir().expect("a temporary directory");
        let store = FsLoopStateStore::new(dir.path());
        let key = contract::key("racing-commits");

        let mut racers = Vec::new();
        for index in 0..8u64 {
            let store = store.clone();
            let key = key.clone();
            racers.push(tokio::spawn(async move {
                let mut state = contract::fresh_state(&key);
                // Distinct per racer, so the state that lands names its winner.
                state.work_budget_consumed_ms = index;
                store.commit(&key, &state, Revision::INITIAL).await
            }));
        }

        let mut winners = 0;
        for racer in racers {
            match racer.await.expect("no racer panicked") {
                Ok(revision) => {
                    winners += 1;
                    assert_eq!(revision, Revision::INITIAL.next());
                },
                Err(StoreError::Conflict { expected, .. }) => {
                    assert_eq!(expected, Revision::INITIAL);
                },
                Err(other) => panic!("a loser must lose by conflict, got {other}"),
            }
        }
        assert_eq!(
            winners, 1,
            "two workers both believing they published one revision is the failure the whole \
             store is built around"
        );

        let loaded = store.load(&key).await.expect("load").expect("present");
        assert_eq!(loaded.revision, Revision::INITIAL.next());
        assert!(
            loaded.state.work_budget_consumed_ms < 8,
            "the published state must be one racer's, not a blend of several"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn exactly_one_of_eight_racing_claims_takes_the_lease() {
        // The same argument for the lease. Every live acquisition now excludes
        // every claimant, regardless of id: two successful claims would be two
        // workers advancing one execution.
        let dir = tempfile::tempdir().expect("a temporary directory");
        let store = FsLoopStateStore::new(dir.path());
        let key = contract::key("racing-claims");

        let mut racers = Vec::new();
        for index in 0..8 {
            let store = store.clone();
            let key = key.clone();
            racers.push(tokio::spawn(async move {
                store
                    .claim(
                        &key,
                        &WorkerId::new(format!("worker-{index}")),
                        Duration::from_secs(300),
                    )
                    .await
            }));
        }

        let mut holders = Vec::new();
        for racer in racers {
            match racer.await.expect("no racer panicked") {
                Ok(lease) => holders.push(lease),
                Err(StoreError::LeaseHeld { .. }) => {},
                Err(other) => panic!("a loser must lose by the lease being held, got {other}"),
            }
        }
        assert_eq!(holders.len(), 1, "got {holders:?}");
    }

    #[test]
    fn a_publish_refuses_an_existing_name_rather_than_overwriting_it() {
        // The compare-and-swap primitive, checked directly: everything the store
        // guarantees about concurrent commits reduces to this.
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("snapshot-1.json");

        assert_eq!(
            publish_new_file(&path, b"first").expect("publish"),
            Publish::Published
        );
        assert_eq!(
            publish_new_file(&path, b"second").expect("publish"),
            Publish::AlreadyExists
        );
        assert_eq!(std::fs::read(&path).expect("read"), b"first");

        // And the temporary it wrote on the way is gone either way.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[tokio::test]
    async fn taking_the_lease_reaps_temporaries_a_crashed_publish_left_behind() {
        // The defect: `publish_new_file` and `overwrite_atomically` remove their
        // temporary on every path they can reach, but a process that dies between
        // creating one and linking it leaves it forever. Nothing removed them, so
        // an execution accumulated one per crash until `scan_numbered` refused
        // the directory at MAX_EXECUTION_DIR_ENTRIES — at which point the
        // execution could no longer be written at all.
        //
        // Age would race a live writer. The lease does not: every writer into
        // these two directories holds it, and this runs the moment a caller takes
        // it at a fence nobody else has.
        let harness = FsHarness::create();
        let key = contract::key("reaped");
        harness
            .store()
            .commit(&key, &contract::fresh_state(&key), Revision::INITIAL)
            .await
            .expect("commit, so the directories exist");
        harness
            .store()
            .record_effect_intent(
                &key,
                &EffectLedgerEntry::intent(
                    &pending_effect("llm-1:tool:call-1"),
                    1,
                    Phase::Apply,
                    1_000,
                ),
            )
            .await
            .expect("an effects directory to reap in");

        let execution_dir = harness.store().execution_dir(&key);
        let effects_dir = harness.store().effects_dir(&key);
        let wake_dir = harness.store().wake_dir(&key, "job:coding-7");
        std::fs::create_dir_all(&wake_dir).expect("wake dir");

        let leaked = [
            execution_dir.join("snapshot-2.json.tmp-deadbeef"),
            execution_dir.join("lease-2.json.tmp-cafe"),
            effects_dir.join("row.json.tmp-1"),
        ];
        for path in &leaked {
            std::fs::write(path, b"half written").expect("leak a temporary");
        }
        // A completer holds no lease, so its temporaries are NOT this reap's to
        // take: removing one mid-publish fails a completion, and a completion
        // that fails is a run that never wakes.
        let completer_temp = wake_dir.join("resolution.json.tmp-inflight");
        std::fs::write(&completer_temp, b"half written").expect("leak a wake temporary");

        harness
            .store()
            .claim(&key, &WorkerId::new("worker-a"), Duration::from_secs(300))
            .await
            .expect("claim");

        for path in &leaked {
            assert!(
                !path.exists(),
                "{} survived the claim that took the lease",
                path.display()
            );
        }
        assert!(
            completer_temp.exists(),
            "a wake temporary belongs to a writer that holds no lease and must \
             not be reaped by whoever takes one"
        );

        // And the reap did not take anything real with it: the committed state
        // and the effect row are still readable. Without this the assertions
        // above would pass on a reaper that emptied the directory.
        assert!(harness.store().load(&key).await.expect("load").is_some());
        assert!(harness
            .store()
            .load_effects(&key)
            .await
            .expect("effects")
            .get(&EffectId::parse("llm-1:tool:call-1").expect("well-formed"))
            .is_some());
    }

    #[test]
    fn a_bounded_read_refuses_an_oversized_file_instead_of_reading_it() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("big.json");
        std::fs::write(&path, format!("{{\"blob\":\"{}\"}}", "x".repeat(4_096))).expect("write");

        let error = read_bounded_json::<serde_json::Value>(&path, 128, 1_000)
            .expect_err("a file over its byte limit must not be read");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

        // And absence is still absence, not an error.
        let missing: Option<serde_json::Value> =
            read_bounded_json(&dir.path().join("nope.json"), 1_024, 100).expect("absent");
        assert!(missing.is_none());
    }

    #[test]
    fn a_bounded_read_refuses_a_node_bomb_within_the_byte_limit() {
        // The byte limit alone does not stop a file that is small but pathological
        // to parse. This is why the node guard is reused rather than a size check
        // being considered sufficient.
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("many.json");
        let payload = format!("[{}]", vec!["1"; 2_000].join(","));
        std::fs::write(&path, &payload).expect("write");

        assert!(
            payload.len() < 16 * 1024,
            "the file is inside its byte limit"
        );
        let error = read_bounded_json::<serde_json::Value>(&path, 16 * 1024, 64)
            .expect_err("a file over its node limit must not be read");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn a_lease_too_large_to_read_back_is_refused_at_write() {
        // The bound was on the read side only. A worker id long enough to push
        // the lease past it would have written a file nothing could load —
        // leaving the execution unclaimable by anyone, including the worker that
        // wrote it, with no error at the moment of the mistake.
        let harness = FsHarness::create();
        let key = contract::key("huge-worker");
        let error = harness
            .store()
            .claim(
                &key,
                &WorkerId::new("w".repeat(MAX_LEASE_BYTES as usize + 1)),
                Duration::from_secs(60),
            )
            .await
            .expect_err("a lease no bounded read could load must not be written");
        assert!(matches!(error, StoreError::Corrupt { .. }), "got {error}");

        // And no half-written lease was left behind for the next claim to trip
        // over.
        let ordinary = harness
            .store()
            .claim(&key, &WorkerId::new("worker-a"), Duration::from_secs(60))
            .await
            .expect("an ordinary claim still works");
        assert_eq!(ordinary.fence, 1);
    }

    #[tokio::test]
    async fn a_journal_torn_inside_a_character_is_still_readable() {
        // `serde_json` writes non-ASCII as raw UTF-8 rather than as escapes, and
        // records carry model and tool text, so an interrupted append can leave
        // bytes that are not a string. Reading the file as text refused the whole
        // journal in that case — every append and every commit for this
        // execution, forever — over a record that was never acknowledged.
        let harness = FsHarness::create();
        let key = contract::key("torn-character");
        harness
            .store()
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Prepare,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("append");

        let path = harness.store().journal_path(&key);
        let mut raw = std::fs::read(&path).expect("read");
        // The first byte of a two-byte character and nothing after it: the file
        // ends in the middle of a character.
        raw.extend_from_slice(b"{\"seq\":2,\"note\":\"caf\xc3");
        std::fs::write(&path, &raw).expect("write");

        let records = harness
            .store()
            .read_journal(&key, 0)
            .await
            .expect("an append torn mid-character is a torn tail, not a corrupt file");
        assert_eq!(records.len(), 1);

        let seq = harness
            .store()
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Observe,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("and the next append still lands");
        assert_eq!(
            seq, 1,
            "nothing is committed, so the sweep takes it back to zero"
        );
    }

    #[tokio::test]
    async fn an_append_after_a_torn_write_is_not_spliced_onto_the_stump() {
        // The torn-tail case the other test misses, and the one that actually
        // happens: the watermark already covers every *complete* record, so
        // nothing looked orphaned and no rewrite ran. The next record was written
        // straight onto the half-finished bytes, and the line it made sits in the
        // middle of the log — where a torn tail is tolerated and a corrupt line is
        // not. The execution never read its own journal again, and the error
        // named a line rather than the append that wrote it.
        let harness = FsHarness::create();
        let key = contract::key("torn-then-committed");
        harness
            .store()
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Prepare,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("append");
        let mut state = contract::fresh_state(&key);
        state.journal_seq = 1;
        harness
            .store()
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit, so the watermark covers every complete record");

        let path = harness.store().journal_path(&key);
        let mut raw = std::fs::read_to_string(&path).expect("read");
        raw.push_str("{\"seq\":2,\"iterat");
        std::fs::write(&path, &raw).expect("write");

        harness
            .store()
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Observe,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("the re-run appends");

        let records = harness
            .store()
            .read_journal(&key, 0)
            .await
            .expect("the log must still be readable");
        assert_eq!(
            records.iter().map(|record| record.seq).collect::<Vec<_>>(),
            vec![1, 2]
        );
        Journal::from_records(records).expect("and gapless, with no spliced line");
    }

    #[tokio::test]
    async fn an_invalid_byte_sequence_is_corruption_rather_than_a_torn_tail() {
        // The other half of the same distinction. A byte that is not the start of
        // a character, with bytes after it, is damage inside a file that is only
        // ever appended to — never an interrupted write — and trimming it would
        // silently discard whatever followed.
        let harness = FsHarness::create();
        let key = contract::key("bad-bytes");
        harness
            .store()
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Prepare,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("append");

        let path = harness.store().journal_path(&key);
        let mut raw = std::fs::read(&path).expect("read");
        raw.extend_from_slice(b"\xc3\x28 not a character\n");
        std::fs::write(&path, &raw).expect("write");

        let error = harness
            .store()
            .read_journal(&key, 0)
            .await
            .expect_err("an invalid sequence must not be trimmed away as a torn tail");
        assert!(matches!(error, StoreError::Corrupt { .. }), "got {error}");

        // Both doors, because they no longer share a body.
        // `read_journal_verified` is overridden on this store to skip the
        // `Journal::from_records` re-check the trait's default runs — on the
        // grounds that `Journal::parse` already applied a superset of it. That
        // grounds is exactly what this asserts: the checked door must refuse the
        // same bytes, or the override traded a copy for a hole.
        let error = harness
            .store()
            .read_journal_verified(&key)
            .await
            .expect_err("nor by the door that skips the second check");
        assert!(matches!(error, StoreError::Corrupt { .. }), "got {error}");
    }

    #[tokio::test]
    async fn the_checked_read_refuses_a_seq_break_the_trait_default_would_have_caught() {
        // The specific check `FsLoopStateStore::read_journal_verified` declines to
        // run twice. The trait's default is
        // `Journal::from_records(read_journal(key, 0))`, and `from_records`'
        // whole job is `record.seq == index + 1`; the override answers with
        // `Journal::parse`'s own journal instead, on the claim that `parse`
        // enforces the same rule at the same place.
        //
        // A claim about a check is worth exactly what happens when the check is
        // needed, so this puts a real seq break on disk and demands the refusal
        // from BOTH doors. An override that returned records without a parse —
        // a cache, a fast path, a `serde_json::from_str` per line — passes every
        // other case in this file and fails here.
        let harness = FsHarness::create();
        let key = contract::key("seq-break");
        harness
            .store()
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Prepare,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect("append");

        // Built by re-serialising the store's OWN first line with its seq moved,
        // rather than by hand-writing a record. A hand-written line would also be
        // testing that this fixture still agrees with `JournalRecord`'s field
        // names, and would start failing for that reason instead of this one.
        let path = harness.store().journal_path(&key);
        let raw = std::fs::read_to_string(&path).expect("read");
        let first = raw.lines().next().expect("the append wrote a line");
        let mut second: serde_json::Value = serde_json::from_str(first).expect("a record parses");
        second["seq"] = serde_json::json!(3);
        let mut with_gap = raw.clone();
        with_gap.push_str(&serde_json::to_string(&second).expect("re-encodes"));
        with_gap.push('\n');
        std::fs::write(&path, with_gap.as_bytes()).expect("write");

        // Not a torn tail: the file ends in a newline, so nothing here is
        // exempt from the parse.
        let raw_door = harness
            .store()
            .read_journal(&key, 0)
            .await
            .expect_err("a log that skips seq 2 is unreadable");
        assert!(
            matches!(raw_door, StoreError::Journal(JournalError::SeqGap { .. })),
            "got {raw_door}"
        );
        let checked_door = harness
            .store()
            .read_journal_verified(&key)
            .await
            .expect_err("and the checked door must not be the lenient one");
        assert!(
            matches!(
                checked_door,
                StoreError::Journal(JournalError::SeqGap { .. })
            ),
            "got {checked_door}"
        );
    }

    #[tokio::test]
    async fn a_log_that_falls_short_of_its_watermark_refuses_the_next_append() {
        // Without this the append placed the next seq past the gap the missing
        // records left, and the file it wrote could never be parsed again: the
        // execution died at its next read, with an error naming a seq break and
        // nothing naming the append that caused it.
        let harness = FsHarness::create();
        let key = contract::key("short-log");
        harness
            .store()
            .append_journal(
                &key,
                &[
                    JournalAppend::phase_completed(1, Phase::Prepare, RecordedStep::Continued),
                    JournalAppend::phase_completed(1, Phase::Observe, RecordedStep::Continued),
                ],
            )
            .await
            .expect("append");
        let mut state = contract::fresh_state(&key);
        state.journal_seq = 2;
        harness
            .store()
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit the watermark");

        // The committed records are no longer all there.
        let path = harness.store().journal_path(&key);
        let raw = std::fs::read_to_string(&path).expect("read");
        let first_line = raw.find('\n').expect("two records, two lines") + 1;
        std::fs::write(&path, &raw[..first_line]).expect("write");

        let error = harness
            .store()
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Decide,
                    RecordedStep::Continued,
                )],
            )
            .await
            .expect_err("appending on top of a log that lost committed records must be refused");
        assert!(matches!(error, StoreError::Corrupt { .. }), "got {error}");
    }

    #[tokio::test]
    async fn effect_rows_are_counted_against_the_limit_a_read_would_apply() {
        // The write side had no ledger bound at all: every effect is its own
        // file, so the in-memory cap never saw more than one row and the
        // directory could pass the count `load_effects` refuses. Past that point
        // the ledger is unloadable and every disposition unanswerable, for the
        // life of the execution.
        //
        // The production limit is a hundred thousand rows, which is not a number
        // a test writes to disk, so the counter is driven at its own boundary and
        // the call site passes `MAX_LEDGER_ENTRIES`.
        let harness = FsHarness::create();
        let key = contract::key("ledger-capacity");
        for index in 0..2 {
            let pending = pending_effect(&format!("llm-1:tool:call-{index}"));
            harness
                .store()
                .record_effect_intent(
                    &key,
                    &EffectLedgerEntry::intent(&pending, 1, Phase::Apply, 1_000),
                )
                .await
                .expect("intent");
        }

        assert!(harness.store().effects_at_capacity(&key, 2).expect("count"));
        assert!(!harness.store().effects_at_capacity(&key, 3).expect("count"));
        assert!(
            !harness
                .store()
                .effects_at_capacity(&contract::key("no-effects"), 1)
                .expect("count"),
            "an execution with no effects directory is not at capacity"
        );

        // A half-written temporary is not a row. Counting it would refuse a write
        // the reader would have accepted, which is the mirror of the bug.
        std::fs::write(
            harness.store().effects_dir(&key).join("row.json.tmp-1"),
            b"{}",
        )
        .expect("write");
        assert!(!harness.store().effects_at_capacity(&key, 3).expect("count"));
    }

    #[tokio::test]
    async fn a_key_record_that_addresses_another_directory_is_excluded_from_the_scan() {
        // The scan recovers a key from each directory's key record and then
        // trusts it. A record naming somebody else's execution had the scan load
        // that execution through this directory and offer the same work twice —
        // two workers racing for one run, from one listing.
        let harness = FsHarness::create();
        let real = contract::key("real");
        harness
            .store()
            .commit(&real, &contract::fresh_state(&real), Revision::INITIAL)
            .await
            .expect("commit");

        let decoy = harness.store().execution_dir(&real).with_file_name("decoy");
        std::fs::create_dir_all(&decoy).expect("mkdir");
        std::fs::write(
            decoy.join("key.json"),
            serde_json::to_vec(&real).expect("encode"),
        )
        .expect("write");

        let runnable = harness
            .store()
            .list_runnable(&WorkerId::new("worker-a"), 50)
            .await
            .expect("list");
        assert_eq!(
            runnable.iter().filter(|key| **key == real).count(),
            1,
            "the decoy directory must not offer the same execution again: {runnable:?}"
        );
    }

    /// End `key`'s run on disk, through the store's own append and commit.
    async fn end_the_run(store: &FsLoopStateStore, key: &ExecutionKey, terminal: TerminalKind) {
        let seq = store
            .append_journal(
                key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Prepare,
                    RecordedStep::RunEnded { terminal },
                )],
            )
            .await
            .expect("append");
        let mut state = contract::fresh_state(key);
        state.journal_seq = seq;
        store
            .commit(key, &state, Revision::INITIAL)
            .await
            .expect("commit");
    }

    #[tokio::test]
    async fn a_published_ending_is_on_disk_and_a_cold_store_reads_it() {
        // The half the contract suite cannot prove. It runs against the
        // in-memory store too, where "the ending survives" is a statement about
        // a field in a map — so a filesystem store that derived the ending,
        // never wrote it, and answered from nothing at all would pass every
        // contract case and would offer every finished run again the moment a
        // worker restarted. Which is the population this whole change is about:
        // a cold-started runner re-discovering every finished run once.
        let harness = FsHarness::create();
        let finished = contract::key("finished");
        end_the_run(harness.store(), &finished, TerminalKind::Success).await;

        assert!(
            harness.store().ended_path(&finished).exists(),
            "the ending must be a durable value, not a derivation this handle happens to hold"
        );

        let cold = harness.reopen();
        let runnable = cold
            .list_runnable(&WorkerId::new("worker-a"), 50)
            .await
            .expect("list");
        assert!(
            !runnable.contains(&finished),
            "a store that has never seen this run before must still withhold it: {runnable:?}"
        );
    }

    /// # This case is deliberately NOT in the shared contract suite
    ///
    /// The defect is a marker that did not land, and `store/memory.rs` writes
    /// its marker as a field of the same slot under the same mutex as the
    /// state — it cannot fail, and it cannot be made to fail from a test. A
    /// contract case would therefore pass against the memory store whether or
    /// not `repair_ending` existed, which is the shape that has already shipped
    /// here three times: a fake whose vocabulary cannot express the defect.
    /// `reconciler`'s
    /// `a_run_the_children_ground_retires_is_withheld_by_the_scan` has exactly
    /// that blind spot and this is what covers behind it.
    #[tokio::test]
    async fn a_lost_ending_marker_is_republished_by_the_next_claim() {
        let harness = FsHarness::create();
        let store = harness.store();
        let worker = WorkerId::new("worker-a");
        let finished = contract::key("finished-marker-lost");
        end_the_run(store, &finished, TerminalKind::Success).await;

        // What the commit published, kept so the repair is measured against the
        // commit's own derivation rather than against a constant this test
        // chose. A repair that re-derived a *different* ending would be a second
        // spelling of the rule, and that is the thing worth catching.
        let marker = store.ended_path(&finished);
        let committed = store
            .load(&finished)
            .await
            .expect("load committed terminal")
            .expect("committed terminal");
        let published = store
            .ended_run(&finished, committed.revision)
            .expect("read published marker")
            .expect("published ending");

        // The window, reproduced by its EFFECT rather than by its cause. The
        // causes are an `overwrite_atomically` that failed and a crash between
        // the snapshot and the marker; both leave precisely this on disk, and
        // neither can be provoked from inside the process.
        std::fs::remove_file(&marker).expect("remove the marker the commit published");

        // The fixture actually reaches the failing state. Without this, the last
        // assertion could be satisfied by a run that was never offered at all.
        let offered = store.list_runnable(&worker, 50).await.expect("list");
        assert!(
            offered.contains(&finished),
            "a run whose marker is gone must be offered again, or this case is not standing \
             where the defect stands: {offered:?}"
        );

        let lease = store
            .claim(&finished, &worker, Duration::from_secs(300))
            .await
            .expect("claim");

        // The direct measurement, and the one nothing else in this file can
        // satisfy: the bytes are back, and they say what the commit said.
        let republished = store
            .ended_run(&finished, committed.revision)
            .expect("read repaired marker")
            .expect("repaired ending");
        assert_eq!(
            republished, published,
            "the repair must re-derive the ending the commit derived, from the same journal \
             prefix and the same watermark"
        );

        // RELEASED before the scan below, and that is not tidiness. `runnable_at`
        // tests the ending BEFORE the lease, so a scan taken while this worker
        // still held one would withhold the key for the lease no matter what the
        // marker said, and the assertion could not fail. Releasing removes the
        // second reason so only the marker is left to answer.
        store.release(lease).await.expect("release");

        let after = store.list_runnable(&worker, 50).await.expect("list");
        assert!(
            !after.contains(&finished),
            "one claim is all the repair gets. After it the key must be out of every scan, or \
             it goes on spending a slot in every page for the life of the store: {after:?}"
        );
    }

    #[tokio::test]
    async fn an_exact_revision_ending_does_not_replay_the_journal_on_claim() {
        let harness = FsHarness::create();
        let store = harness.store();
        let worker = WorkerId::new("worker-a");
        let finished = contract::key("finished-marker-already-current");
        end_the_run(store, &finished, TerminalKind::Success).await;
        let committed = store
            .load(&finished)
            .await
            .expect("load committed terminal")
            .expect("committed terminal");
        let published = store
            .ended_run(&finished, committed.revision)
            .expect("read exact marker")
            .expect("terminal ending");

        // The marker is the revision-bound certificate current commits publish
        // from this journal. Removing the journal after that publication makes
        // any accidental replay observable: an empty replay derives `None` and
        // overwrites the sound terminal binding. The claim path must instead
        // trust the same exact marker every runnable/debt scan already trusts.
        std::fs::remove_file(store.journal_path(&finished)).expect("remove journal");
        let lease = store
            .claim(&finished, &worker, Duration::from_secs(300))
            .await
            .expect("claim with current marker");

        assert_eq!(
            store
                .ended_run(&finished, committed.revision)
                .expect("read marker after claim"),
            Some(published),
            "claim repair must not re-derive or retract an exact current marker"
        );
        store.release(lease).await.expect("release");
    }

    #[tokio::test]
    async fn a_prepublished_terminal_marker_is_invisible_before_its_snapshot_lands() {
        let harness = FsHarness::create();
        let store = harness.store();
        let key = contract::key("prepublished-ending");
        store
            .commit(&key, &contract::fresh_state(&key), Revision::INITIAL)
            .await
            .expect("commit old live snapshot");

        store
            .publish_ending(
                &key,
                EndedRun {
                    seq: 1,
                    terminal: TerminalKind::Success,
                    last_event_seq: Some(1),
                    has_outbox_metadata: true,
                },
            )
            .expect("simulate marker publication immediately before a crash");

        let runnable = store
            .list_runnable(&WorkerId::new("worker-a"), 50)
            .await
            .expect("scan live snapshot");
        assert!(runnable.contains(&key));
        let debt = store
            .scan_terminal_outbox_debt(NonZeroUsize::new(50).expect("non-zero budget"), None)
            .await
            .expect("scan terminal debt");
        assert!(debt.keys.is_empty());
    }

    #[tokio::test]
    async fn a_nonterminal_steer_receipt_is_discovered_without_an_ending_marker() {
        let harness = FsHarness::create();
        let store = harness.store();
        let key = contract::key("cancelled-nonterminal-steer-receipt");
        let mut state = contract::fresh_state(&key);
        state.steer_consume_receipt = Some(
            crate::magician_v2::execution::agentic::run_loop::steer_inbox::SteerConsumeReceipt {
                schema_version: 1,
                execution_id: key.execution_id().to_string(),
                source_segment: key.execution_id().to_string(),
                iteration: 1,
                entries: vec![
                    crate::magician_v2::execution::agentic::run_loop::steer_inbox::SteerReceiptEntry {
                        entry_id: "steer-1".to_string(),
                        generation: 1,
                        control_generation: "controls-1".to_string(),
                        content_digest: "digest-1".to_string(),
                    },
                ],
            },
        );
        store.commit(&key, &state, Revision::INITIAL).await.expect(
            "commit a live state whose runtime can be cancelled before terminal publication",
        );

        assert!(
            store
                .ended_run(&key, Revision::INITIAL.next())
                .expect("ending lookup")
                .is_none(),
            "the regression requires receipt debt without terminal debt"
        );
        let debt = store
            .scan_terminal_outbox_debt(NonZeroUsize::new(8).expect("non-zero budget"), None)
            .await
            .expect("scan receipt debt");
        assert_eq!(debt.keys, vec![key]);
    }

    #[tokio::test]
    async fn terminal_debt_catalog_hot_path_exact_validates_only_priority_candidates() {
        let harness = FsHarness::create();
        let store = harness.store();
        for index in 0..16 {
            let key = contract::key(&format!("ordinary-history-{index:02}"));
            store
                .commit(&key, &contract::fresh_state(&key), Revision::INITIAL)
                .await
                .expect("commit unrelated history");
        }
        let key = contract::key("catalog-priority-terminal");
        let watermark = store
            .append_journal(
                &key,
                &[
                    JournalAppend::named_event(
                        1,
                        Phase::Prepare,
                        "plan.step.finished",
                        "agent-1",
                        None,
                        None,
                        serde_json::json!({"step_id": "last"}),
                    )
                    .expect("bounded event"),
                    JournalAppend::phase_completed(
                        1,
                        Phase::Prepare,
                        RecordedStep::RunEnded {
                            terminal: TerminalKind::Success,
                        },
                    ),
                ],
            )
            .await
            .expect("append terminal prefix");
        let mut state = contract::fresh_state(&key);
        state.journal_seq = watermark;
        store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit terminal");

        // The first pass is the authoritative bootstrap. It is the only pass
        // that may inspect unrelated lifetime history until the bounded audit
        // interval expires.
        let first = store
            .scan_terminal_outbox_debt(NonZeroUsize::new(128).expect("non-zero budget"), None)
            .await
            .expect("bootstrap terminal-debt catalog");
        assert_eq!(first.keys, vec![key.clone()]);
        assert!(first.resume.is_none());

        store.full_snapshot_loads.store(0, AtomicOrdering::SeqCst);
        let hot = store
            .scan_terminal_outbox_debt(NonZeroUsize::new(128).expect("non-zero budget"), None)
            .await
            .expect("scan derived priority catalog");
        assert_eq!(hot.keys, vec![key]);
        assert_eq!(
            store.full_snapshot_loads.load(AtomicOrdering::SeqCst),
            1,
            "the hot cadence exact-validates the debt row, not all historical executions"
        );
    }

    #[tokio::test]
    async fn corrupt_terminal_debt_catalog_falls_back_and_repairs_authoritatively() {
        let harness = FsHarness::create();
        let store = harness.store();
        let key = contract::key("corrupt-catalog-terminal");
        let watermark = store
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Apply,
                    RecordedStep::RunEnded {
                        terminal: TerminalKind::CannotProceed,
                    },
                )],
            )
            .await
            .expect("append terminal prefix");
        let mut state = contract::fresh_state(&key);
        state.journal_seq = watermark;
        store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit terminal");
        overwrite_derived_atomically(&store.terminal_debt_catalog_path(), br#"{}"#)
            .expect("corrupt the derivative only");

        let debt = store
            .scan_terminal_outbox_debt(NonZeroUsize::new(32).expect("non-zero budget"), None)
            .await
            .expect("fall back to authoritative directories");
        assert_eq!(debt.keys, vec![key]);
        let repaired = store
            .load_terminal_debt_catalog(Utc::now().timestamp_millis())
            .expect("read repaired catalog")
            .expect("catalog exists");
        assert!(repaired.complete);
        assert!(!repaired.overflowed);
        assert_eq!(repaired.entries.len(), 1);
    }

    #[tokio::test]
    async fn a_failed_later_commit_cannot_hide_the_prior_terminal_binding() {
        let harness = FsHarness::create();
        let store = harness.store();
        let key = contract::key("terminal-before-failed-successor");
        let watermark = store
            .append_journal(
                &key,
                &[
                    JournalAppend::named_event(
                        1,
                        Phase::Prepare,
                        "plan.step.finished",
                        "agent-1",
                        Some("owner".to_owned()),
                        Some("default".to_owned()),
                        serde_json::json!({"execution_id": key.execution_id()}),
                    )
                    .expect("bounded event"),
                    JournalAppend::phase_completed(
                        1,
                        Phase::Prepare,
                        RecordedStep::RunEnded {
                            terminal: TerminalKind::Success,
                        },
                    ),
                ],
            )
            .await
            .expect("append terminal prefix");
        let mut state = contract::fresh_state(&key);
        state.journal_seq = watermark;
        let current_revision = store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit prior terminal");
        let current_ending = store
            .ended_run(&key, current_revision)
            .expect("read prior marker")
            .expect("prior terminal ending");

        // Exact on-disk state after prepublishing a non-terminal successor and
        // crashing (or failing snapshot CAS) before that successor becomes
        // authoritative. The old scalar scheme removed/overwrote ended.json at
        // this point and made the still-committed terminal debt undiscoverable.
        store
            .publish_ending_transition(
                &key,
                vec![
                    RevisionEndingBinding {
                        revision: current_revision,
                        ending: Some(current_ending),
                    },
                    RevisionEndingBinding {
                        revision: current_revision.next(),
                        ending: None,
                    },
                ],
            )
            .expect("prepublish successor transition");

        let debt = store
            .scan_terminal_outbox_debt(NonZeroUsize::new(50).expect("non-zero budget"), None)
            .await
            .expect("scan authoritative prior terminal");
        assert_eq!(debt.keys, vec![key]);
    }

    #[tokio::test]
    async fn a_cas_losers_stale_ending_cannot_withhold_another_watermark() {
        let harness = FsHarness::create();
        let store = harness.store();
        let key = contract::key("stale-loser-ending");
        let watermark = store
            .append_journal(
                &key,
                &[
                    JournalAppend::phase_completed(1, Phase::Prepare, RecordedStep::Continued),
                    JournalAppend::phase_completed(1, Phase::Observe, RecordedStep::Continued),
                ],
            )
            .await
            .expect("append winner prefix");
        let mut state = contract::fresh_state(&key);
        state.journal_seq = watermark;
        store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("commit winner snapshot");

        store
            .publish_ending(
                &key,
                EndedRun {
                    seq: watermark - 1,
                    terminal: TerminalKind::Success,
                    last_event_seq: Some(watermark - 1),
                    has_outbox_metadata: true,
                },
            )
            .expect("simulate a losing writer's stale marker");

        let runnable = store
            .list_runnable(&WorkerId::new("worker-a"), 50)
            .await
            .expect("scan winner snapshot");
        assert!(runnable.contains(&key));
        let debt = store
            .scan_terminal_outbox_debt(NonZeroUsize::new(50).expect("non-zero budget"), None)
            .await
            .expect("scan terminal debt");
        assert!(debt.keys.is_empty());
    }

    #[tokio::test]
    async fn a_terminal_commit_refuses_before_snapshot_when_its_index_cannot_publish() {
        let harness = FsHarness::create();
        let store = harness.store();
        let key = contract::key("terminal-index-unpublishable");
        let watermark = store
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Apply,
                    RecordedStep::RunEnded {
                        terminal: TerminalKind::Success,
                    },
                )],
            )
            .await
            .expect("append terminal");
        std::fs::create_dir(store.ended_path(&key)).expect("block ending-file publication");
        let mut state = contract::fresh_state(&key);
        state.journal_seq = watermark;

        store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect_err("terminal snapshot must not land without its discovery index");
        assert!(
            store
                .load(&key)
                .await
                .expect("load after refusal")
                .is_none(),
            "marker failure must leave the prior committed snapshot authoritative"
        );
    }

    #[tokio::test]
    async fn terminal_commit_refuses_when_neither_candidate_nor_dirty_fallback_can_publish() {
        let harness = FsHarness::create();
        let store = harness.store();
        let key = contract::key("terminal-debt-publication-refused");
        let watermark = store
            .append_journal(
                &key,
                &[JournalAppend::phase_completed(
                    1,
                    Phase::Apply,
                    RecordedStep::RunEnded {
                        terminal: TerminalKind::Success,
                    },
                )],
            )
            .await
            .expect("append terminal");
        std::fs::create_dir_all(store.terminal_debt_catalog_dir())
            .expect("create catalog directory");
        std::fs::create_dir(store.terminal_debt_catalog_path())
            .expect("block catalog-file publication");
        std::fs::create_dir(store.terminal_debt_catalog_dirty_path())
            .expect("block dirty-generation publication");
        let mut state = contract::fresh_state(&key);
        state.journal_seq = watermark;

        let error = store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect_err("a terminal cannot become committed but undiscoverable");
        assert!(
            matches!(error, StoreError::Unavailable { .. }),
            "got {error}"
        );
        assert!(
            store.load(&key).await.expect("load").is_none(),
            "candidate/dirty failure must occur before the snapshot CAS"
        );
    }

    #[tokio::test]
    async fn a_commit_that_cannot_publish_its_ending_transition_is_refused() {
        let harness = FsHarness::create();
        let store = harness.store();
        let key = contract::key("stale-ending-unremovable");
        store
            .commit(&key, &contract::fresh_state(&key), Revision::INITIAL)
            .await
            .expect("the first commit");
        let before = store.load(&key).await.expect("load").expect("state");

        // New commits publish an explicit non-terminal binding too. Replace it
        // with a directory to model a path that atomic marker replacement
        // cannot update (the same refusal shape as a read-only mount or EIO).
        std::fs::remove_file(store.ended_path(&key)).expect("remove healthy marker");
        std::fs::create_dir(store.ended_path(&key)).expect("an unremovable marker");

        let error = store
            .commit(&key, &contract::fresh_state(&key), before.revision)
            .await
            .expect_err("a commit that cannot publish its transition must be refused");
        assert!(
            matches!(error, StoreError::Unavailable { .. }),
            "got {error}"
        );

        // The refusal must be a NO-OP because it is reported before the snapshot
        // CAS. The prior committed revision remains authoritative.
        let after = store.load(&key).await.expect("load").expect("state");
        assert_eq!(
            after.revision, before.revision,
            "a commit refused over the marker must not have published a snapshot"
        );
    }

    #[tokio::test]
    async fn an_ending_that_cannot_be_read_withholds_the_run_rather_than_offering_it() {
        // Absence and unreadability mean opposite things — *this run has not
        // ended* against *this store cannot say* — and a read that conflated
        // them would offer a run on the strength of a read that failed.
        //
        // The damaged run has NOT ended, and that is the whole design of the
        // fixture. Corrupting the marker of a run that really did end would
        // assert nothing: it is withheld either way, so the assertion could not
        // fire and the test would pass against a store that treated every
        // unreadable marker as absent.
        //
        // The second measurement is a healthy execution in the same tree. A scan
        // that answered the corruption by giving up would withhold that one too,
        // and every other assertion here would look identical.
        let harness = FsHarness::create();
        let damaged = contract::key("aaa-damaged");
        let healthy = contract::key("bbb-healthy");
        for each in [&damaged, &healthy] {
            harness
                .store()
                .commit(each, &contract::fresh_state(each), Revision::INITIAL)
                .await
                .expect("commit");
        }

        let before = harness
            .store()
            .list_runnable(&WorkerId::new("worker-a"), 50)
            .await
            .expect("list");
        assert!(
            before.contains(&damaged),
            "the fixture has to be offerable before the damage, or the assertion below is \
             satisfied by a run that was never offered: {before:?}"
        );

        std::fs::write(harness.store().ended_path(&damaged), b"{\"seq\":").expect("write");

        let runnable = harness
            .store()
            .list_runnable(&WorkerId::new("worker-a"), 50)
            .await
            .expect("one unreadable execution must not fail the whole listing");
        assert!(
            !runnable.contains(&damaged),
            "an execution whose ending cannot be read is quarantined from scheduling, not \
             offered: {runnable:?}"
        );
        assert!(
            runnable.contains(&healthy),
            "and the damage is held against that key alone: {runnable:?}"
        );
    }

    #[tokio::test]
    async fn a_stray_file_beside_the_scope_directories_does_not_take_the_whole_scan_down() {
        // It used to. `read_dir` on a file answers `NotADirectory`, the walk
        // matched only `NotFound`, and everything else became
        // `StoreError::Unavailable` — so one `.DS_Store` under `scopes/`, or any
        // file another subsystem left beside a scope directory, made every scan
        // AND every reconciliation pass report the store as down, for every
        // execution, until somebody deleted it.
        let harness = FsHarness::create();
        let live = contract::key("live");
        harness
            .store()
            .commit(&live, &contract::fresh_state(&live), Revision::INITIAL)
            .await
            .expect("commit");

        let scopes_root = harness.store().workspace.scopes_root();
        std::fs::write(scopes_root.join(".DS_Store"), b"not a scope").expect("write");
        // Four parents up from the execution directory is the PRINCIPAL
        // directory: <id> → executions → runtime → <workspace> → <principal>.
        // The stray has to sit at that level to be an entry the walk's second
        // read_dir hands back, which is the level the old code descended into
        // without asking what it was.
        let principal_dir = harness
            .store()
            .execution_dir(&live)
            .ancestors()
            .nth(4)
            .expect("the principal directory")
            .to_path_buf();
        std::fs::write(principal_dir.join("stray.lock"), b"not a workspace").expect("write");

        // The third stray sits where the other two cannot: `runtime/executions`
        // is CONSTRUCTED by the walk rather than enumerated from a parent, so
        // the directory filter that saves the two levels above never sees it. A
        // workspace whose `runtime` is a file took the whole scan down long
        // after the levels above were fixed, and the case that claimed the shape
        // could not arise did not write one.
        let workspace_with_a_file_for_runtime = principal_dir.join("blocked-workspace");
        std::fs::create_dir_all(&workspace_with_a_file_for_runtime).expect("mkdir");
        std::fs::write(
            workspace_with_a_file_for_runtime.join("runtime"),
            b"not a directory",
        )
        .expect("write");

        let runnable = harness
            .store()
            .list_runnable(&WorkerId::new("worker-a"), 50)
            .await
            .expect("a stray file is not the store being unavailable");
        assert!(runnable.contains(&live), "got {runnable:?}");
        assert!(
            !harness
                .store()
                .list_parked(50)
                .await
                .expect("the reconciler's walk shares the same levels")
                .incomplete,
            "and the shared walk did not report a coverage hole either: a path that is not a \
             directory holds no executions, so an empty answer for it is a COMPLETE one"
        );
    }

    #[tokio::test]
    async fn a_symlinked_execution_directory_is_still_walked() {
        // `DirEntry::file_type` is an `lstat`, so a symlink has to be asked
        // about a second time. The obvious second question — `DirEntry::metadata`
        // — is documented as `symlink_metadata` on Unix and does NOT follow the
        // link, so asking it answered `is_dir == false` for every symlinked
        // scope directory in the store. The entry was then skipped: never
        // scheduled by `scan_runnable`, never offered by `list_runnable`, never
        // examined by `list_parked` — with no warning and without the pass
        // reporting itself incomplete, which is the worst combination available.
        //
        // The execution level is the one under test because it is the cheapest
        // to build, and it is the same arm for all three: a symlinked principal
        // or workspace directory went the same way.
        let harness = FsHarness::create();
        let linked = contract::key("linked");
        harness
            .store()
            .commit(&linked, &contract::fresh_state(&linked), Revision::INITIAL)
            .await
            .expect("commit");

        // Move the real directory out of the walk's reach and leave a link with
        // the name the walk expects. The key record inside still addresses this
        // path — `key_at` compares the path the walk built, not a canonical one
        // — so nothing else about the run changes.
        let in_place = harness.store().execution_dir(&linked);
        let elsewhere = harness.dir.path().join("relocated-execution");
        // Test-only directory relocation for the symlink attack fixture.
        std::fs::rename(&in_place, &elsewhere).expect("move the real directory");
        std::os::unix::fs::symlink(&elsewhere, &in_place).expect("link it back into place");

        let runnable = harness
            .store()
            .list_runnable(&WorkerId::new("worker-a"), 50)
            .await
            .expect("list");
        assert!(
            runnable.contains(&linked),
            "a symlinked execution directory is still an execution directory: {runnable:?}"
        );
    }

    #[tokio::test]
    async fn a_scan_resumed_after_a_position_that_is_gone_carries_on() {
        // A cursor names a POSITION, not a key, and the difference shows exactly
        // here: an execution directory removed between two pages leaves a
        // position nothing sits at. A store that resolved the cursor to a key
        // and looked it up would answer nothing at all from that point on, and
        // every execution after it would be work no caller ever asks for.
        let harness = FsHarness::create();
        let first = contract::key("aaa-first");
        let second = contract::key("bbb-second");
        for each in [&first, &second] {
            harness
                .store()
                .commit(each, &contract::fresh_state(each), Revision::INITIAL)
                .await
                .expect("commit");
        }

        let page = harness
            .store()
            .scan_runnable(&WorkerId::new("worker-a"), 1, None)
            .await
            .expect("a page");
        assert_eq!(page.keys, vec![first.clone()]);
        let resume = page.resume.expect("the page filled, so there is more");

        std::fs::remove_dir_all(harness.store().execution_dir(&first)).expect("rmdir");

        let next = harness
            .store()
            .scan_runnable(&WorkerId::new("worker-a"), 50, Some(&resume))
            .await
            .expect("a page");
        assert_eq!(
            next.keys,
            vec![second],
            "the walk resumes where the position would have been"
        );
        assert!(next.resume.is_none(), "and it reached the end");
    }

    #[test]
    fn a_truncated_parent_window_reserves_its_inclusive_resume_sentinel() {
        // Principal/workspace cursors are inclusive. If the walk also entered
        // `ccc-sentinel` on this page, a visitor-budget stop inside that branch
        // would publish the earlier parent cursor and restart the same branch
        // on every later page. Reserving it makes it the first branch of the
        // next page instead.
        let children = ChildDirs {
            names: vec![
                "aaa".to_owned(),
                "bbb".to_owned(),
                "ccc-sentinel".to_owned(),
            ],
            truncated: true,
            unclassified: false,
        };
        let count = child_dirs_to_visit(&children);
        assert_eq!(count, 2);
        assert_eq!(
            children.names[..count]
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["aaa", "bbb"]
        );
        assert_eq!(
            children.names[count..]
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["ccc-sentinel"]
        );

        let complete = ChildDirs {
            truncated: false,
            ..children
        };
        assert_eq!(child_dirs_to_visit(&complete), complete.names.len());
    }

    #[test]
    fn a_record_line_can_never_exceed_what_the_journal_reader_accepts() {
        // Both sides of the bound, asserted together. A write limit without a
        // matching read limit is a file nobody can load; a read limit without a
        // matching write limit is a file nobody can write.
        assert!(MAX_JOURNAL_RECORD_BYTES as u64 <= MAX_JOURNAL_BYTES);
    }

    #[test]
    fn uncutover_authority_never_grants_absence_or_exact_recovery() {
        let exact = restrict_uncutover_authority(
            BaseExecutionRecoveryAuthority::RecoverableExact {
                exact_segment: "segment-r7".to_owned(),
                revision: Revision::from_u64(7),
            },
            false,
        );
        assert!(matches!(
            exact,
            BaseExecutionRecoveryAuthority::Uncertain { exact_segments, .. }
                if exact_segments == vec!["segment-r7".to_owned()]
        ));
        assert!(matches!(
            restrict_uncutover_authority(BaseExecutionRecoveryAuthority::Absent, false),
            BaseExecutionRecoveryAuthority::Uncertain { exact_segments, .. }
                if exact_segments.is_empty()
        ));
        let live = BaseExecutionRecoveryAuthority::Live {
            exact_segments: vec!["segment-live".to_owned()],
            until_ms: 42,
        };
        assert_eq!(restrict_uncutover_authority(live.clone(), false), live);
        let settlement = BaseExecutionRecoveryAuthority::SettlementPending {
            exact_segments: vec!["segment-terminal".to_owned()],
        };
        assert_eq!(
            restrict_uncutover_authority(settlement.clone(), false),
            settlement
        );
    }

    #[test]
    fn unstamped_v1_admission_remains_a_parseable_blocker_but_not_v2_authority() {
        let mut legacy = BasePreseedAdmission {
            schema_version: 1,
            principal: "owner".to_owned(),
            workspace: "default".to_owned(),
            base_execution_id: "runtime-legacy-admission".to_owned(),
            token: Uuid::new_v4().to_string(),
            runtime_updated_at: 10,
            exact_segment: Some("legacy-exact".to_owned()),
            exact_revision: Some(Revision::from_u64(4)),
            base_binding_head_sha256: String::new(),
            expires_at_ms: 100,
            integrity_sha256: String::new(),
        };
        legacy.integrity_sha256 = legacy_base_preseed_admission_integrity(&legacy);
        let mut encoded = serde_json::to_value(&legacy).expect("legacy admission JSON");
        encoded
            .as_object_mut()
            .expect("admission object")
            .remove("base_binding_head_sha256");
        let parsed: BasePreseedAdmission =
            serde_json::from_value(encoded).expect("v1 admission stays readable");
        assert!(valid_base_preseed_admission(
            &parsed,
            "owner",
            "default",
            "runtime-legacy-admission",
        ));
        assert_ne!(
            parsed.schema_version, BASE_PRESEED_ADMISSION_SCHEMA_VERSION,
            "an unstamped v1 token may defer until expiry but can never validate or renew"
        );
    }

    #[tokio::test]
    async fn ordinary_first_seed_durably_retires_an_expired_v1_admission() {
        let harness = FsHarness::create();
        let store = harness.store();
        let key = contract::key("seed-after-v1-admission");
        let binding_dir = store.base_binding_dir("owner", "default", "runtime-after-v1");
        create_dir_all_durably(&binding_dir).expect("base binding directory");
        let admission_path = binding_dir.join("preseed.admission");
        let mut legacy = BasePreseedAdmission {
            schema_version: 1,
            principal: "owner".to_owned(),
            workspace: "default".to_owned(),
            base_execution_id: "runtime-after-v1".to_owned(),
            token: Uuid::new_v4().to_string(),
            runtime_updated_at: 1,
            exact_segment: None,
            exact_revision: None,
            base_binding_head_sha256: String::new(),
            expires_at_ms: 2,
            integrity_sha256: String::new(),
        };
        legacy.integrity_sha256 = legacy_base_preseed_admission_integrity(&legacy);
        overwrite_derived_atomically(
            &admission_path,
            &serde_json::to_vec(&legacy).expect("legacy admission encoding"),
        )
        .expect("legacy admission publication");
        let mut state = contract::fresh_state(&key);
        state.segment_binding = Some(LoopSegmentBinding {
            base_execution_id: "runtime-after-v1".to_owned(),
            exact_segment_id: key.execution_id().to_owned(),
            preseed_admission_token: None,
        });
        store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("ordinary seed proceeds after retiring the expired blocker");
        assert!(!admission_path.exists());
    }

    #[tokio::test]
    async fn exact_recovery_admission_can_never_authorize_an_initial_snapshot() {
        let harness = FsHarness::create();
        let store = harness.store();
        let key = contract::key("misrouted-exact-first-seed");
        let mut head = BaseBindingHead {
            schema_version: BASE_BINDING_HEAD_SCHEMA_VERSION,
            principal: "owner".to_owned(),
            workspace: "default".to_owned(),
            base_execution_id: "misrouted-runtime".to_owned(),
            exact_segments: Vec::new(),
            integrity_sha256: String::new(),
        };
        head.integrity_sha256 = base_binding_head_integrity(&head);
        let binding_dir = store.base_binding_dir("owner", "default", "misrouted-runtime");
        let _catalog = acquire_private_catalog_lock(&binding_dir.join("catalog.lock"))
            .expect("base catalog lock");
        store
            .persist_base_binding_head_locked(&head)
            .expect("empty member head");
        let mut admission = BasePreseedAdmission {
            schema_version: BASE_PRESEED_ADMISSION_SCHEMA_VERSION,
            principal: "owner".to_owned(),
            workspace: "default".to_owned(),
            base_execution_id: "misrouted-runtime".to_owned(),
            token: Uuid::new_v4().to_string(),
            runtime_updated_at: Utc::now().timestamp_millis().saturating_sub(10),
            exact_segment: Some("some-existing-source".to_owned()),
            exact_revision: Some(Revision::from_u64(9)),
            base_binding_head_sha256: head.integrity_sha256,
            expires_at_ms: Utc::now().timestamp_millis().saturating_add(60_000),
            integrity_sha256: String::new(),
        };
        admission.integrity_sha256 = base_preseed_admission_integrity(&admission);
        let admission_path = binding_dir.join("preseed.admission");
        overwrite_derived_atomically(
            &admission_path,
            &serde_json::to_vec(&admission).expect("exact admission encoding"),
        )
        .expect("exact admission publication");
        drop(_catalog);
        let mut state = contract::fresh_state(&key);
        state.segment_binding = Some(LoopSegmentBinding {
            base_execution_id: "misrouted-runtime".to_owned(),
            exact_segment_id: key.execution_id().to_owned(),
            preseed_admission_token: Some(admission.token),
        });
        assert!(store.commit(&key, &state, Revision::INITIAL).await.is_err());
        assert!(admission_path.exists());
    }

    #[tokio::test]
    async fn immutable_cutover_enables_indexed_absence_and_is_idempotent_per_deployment() {
        let harness = FsHarness::create();
        let store = harness.store();
        let before = store
            .base_execution_recovery_authority("anonymous", "default", "runtime-a", 10)
            .await
            .expect("uncutover authority");
        assert!(matches!(
            before,
            BaseExecutionRecoveryAuthority::Uncertain { .. }
        ));
        assert!(!store
            .legacy_writers_retired("anonymous", "default")
            .await
            .expect("cutover status"));

        store
            .seal_legacy_writer_cutover("anonymous", "default", "deployment-2026-08-30", 11)
            .await
            .expect("seal cutover");
        store
            .seal_legacy_writer_cutover("anonymous", "default", "deployment-2026-08-30", 12)
            .await
            .expect("same deployment is idempotent");
        assert!(store
            .legacy_writers_retired("anonymous", "default")
            .await
            .expect("cutover status"));
        assert!(store
            .seal_legacy_writer_cutover("anonymous", "default", "different-deployment", 13)
            .await
            .is_err());
        assert_eq!(
            store
                .base_execution_recovery_authority("anonymous", "default", "runtime-a", 14)
                .await
                .expect("cutover authority"),
            BaseExecutionRecoveryAuthority::Absent
        );
    }

    #[tokio::test]
    async fn cutover_refresh_repairs_a_binding_published_after_an_old_complete_marker() {
        let harness = FsHarness::create();
        let store = harness.store();
        let _ = store
            .base_execution_recovery_authority("owner", "default", "marker-seed", 1)
            .await
            .expect("ordinary migration publishes its completion marker");

        let key = contract::key("late-legacy-segment");
        let mut state = contract::fresh_state(&key);
        state.segment_binding = Some(LoopSegmentBinding {
            base_execution_id: "late-runtime".to_owned(),
            exact_segment_id: key.execution_id().to_owned(),
            preseed_admission_token: None,
        });
        store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("late legacy-shaped segment snapshot");
        let late_binding_dir = store.base_binding_dir("owner", "default", "late-runtime");
        std::fs::remove_dir_all(&late_binding_dir)
            .expect("simulate an old writer that omitted reverse publication");

        store
            .seal_legacy_writer_cutover("owner", "default", "deployment-refresh", 15)
            .await
            .expect("post-drain seal performs a fresh authoritative pass");
        let repaired = store
            .read_base_binding_head_sync("owner", "default", "late-runtime")
            .expect("repaired head read")
            .expect("fresh cutover scan restored the late binding");
        assert_eq!(repaired.exact_segments, vec![key.execution_id().to_owned()]);
    }

    #[tokio::test]
    async fn exact_admission_validation_rejects_a_changed_member_head() {
        let harness = FsHarness::create();
        let store = harness.store();
        let first = contract::key("head-exact-first");
        let sibling = contract::key("head-exact-sibling");
        let mut first_revision = Revision::INITIAL;
        for key in [&first, &sibling] {
            let mut state = contract::fresh_state(key);
            state.segment_binding = Some(LoopSegmentBinding {
                base_execution_id: "head-runtime".to_owned(),
                exact_segment_id: key.execution_id().to_owned(),
                preseed_admission_token: None,
            });
            let revision = store
                .commit(key, &state, Revision::INITIAL)
                .await
                .expect("bound segment commit");
            if key == &first {
                first_revision = revision;
            }
        }
        let mut observed_head = BaseBindingHead {
            schema_version: BASE_BINDING_HEAD_SCHEMA_VERSION,
            principal: "owner".to_owned(),
            workspace: "default".to_owned(),
            base_execution_id: "head-runtime".to_owned(),
            exact_segments: vec![first.execution_id().to_owned()],
            integrity_sha256: String::new(),
        };
        observed_head.integrity_sha256 = base_binding_head_integrity(&observed_head);
        let mut admission = BasePreseedAdmission {
            schema_version: BASE_PRESEED_ADMISSION_SCHEMA_VERSION,
            principal: "owner".to_owned(),
            workspace: "default".to_owned(),
            base_execution_id: "head-runtime".to_owned(),
            token: Uuid::new_v4().to_string(),
            runtime_updated_at: 20,
            exact_segment: Some(first.execution_id().to_owned()),
            exact_revision: Some(first_revision),
            base_binding_head_sha256: observed_head.integrity_sha256,
            expires_at_ms: 10_000,
            integrity_sha256: String::new(),
        };
        admission.integrity_sha256 = base_preseed_admission_integrity(&admission);
        let admission_path = store
            .base_binding_dir("owner", "default", "head-runtime")
            .join("preseed.admission");
        overwrite_derived_atomically(
            &admission_path,
            &serde_json::to_vec(&admission).expect("admission encoding"),
        )
        .expect("admission publication");

        assert!(!store
            .validate_base_execution_exact_recovery_admission(
                "owner",
                "default",
                "head-runtime",
                first.execution_id(),
                first_revision,
                &admission.token,
                20,
                30,
            )
            .await
            .expect("changed-head validation"));
    }

    #[tokio::test]
    async fn first_fenced_exact_recovery_commit_consumes_its_admission() {
        let harness = FsHarness::create();
        let store = harness.store();
        let key = contract::key("exact-admission-first-commit");
        let mut state = contract::fresh_state(&key);
        state.segment_binding = Some(LoopSegmentBinding {
            base_execution_id: "exact-admission-runtime".to_owned(),
            exact_segment_id: key.execution_id().to_owned(),
            preseed_admission_token: None,
        });
        let revision = store
            .commit(&key, &state, Revision::INITIAL)
            .await
            .expect("source segment seed");
        let head = store
            .read_base_binding_head_sync("owner", "default", "exact-admission-runtime")
            .expect("member head read")
            .expect("member head");
        let token = Uuid::new_v4().to_string();
        let mut admission = BasePreseedAdmission {
            schema_version: BASE_PRESEED_ADMISSION_SCHEMA_VERSION,
            principal: "owner".to_owned(),
            workspace: "default".to_owned(),
            base_execution_id: "exact-admission-runtime".to_owned(),
            token: token.clone(),
            runtime_updated_at: Utc::now().timestamp_millis().saturating_sub(10),
            exact_segment: Some(key.execution_id().to_owned()),
            exact_revision: Some(revision),
            base_binding_head_sha256: head.integrity_sha256,
            expires_at_ms: Utc::now().timestamp_millis().saturating_add(60_000),
            integrity_sha256: String::new(),
        };
        admission.integrity_sha256 = base_preseed_admission_integrity(&admission);
        let admission_path = store
            .base_binding_dir("owner", "default", "exact-admission-runtime")
            .join("preseed.admission");
        overwrite_derived_atomically(
            &admission_path,
            &serde_json::to_vec(&admission).expect("exact admission encoding"),
        )
        .expect("exact admission publication");
        let worker =
            crate::magician_v2::execution::agentic::run_loop::state::recovery_admission_worker_id(
                &token,
            );
        let lease = store
            .claim(&key, &worker, Duration::from_secs(60))
            .await
            .expect("admitted exact lease");
        state.placement = Placement::Pinned {
            worker: worker.clone(),
            pinned_until_ms: Utc::now().timestamp_millis().saturating_add(60_000),
        };
        store
            .commit_fenced(&key, &state, revision, &lease)
            .await
            .expect("first recovered phase commit");
        assert!(
            !admission_path.exists(),
            "the r -> r+1 fenced commit consumes only its exact token"
        );
        overwrite_derived_atomically(
            &admission_path,
            &serde_json::to_vec(&admission).expect("crash-retry admission encoding"),
        )
        .expect("simulate a crash before admission cleanup");
        let _ = store
            .base_execution_recovery_authority_indexed_sync(
                "owner",
                "default",
                "exact-admission-runtime",
                Utc::now().timestamp_millis(),
            )
            .expect("classification retries committed admission cleanup");
        assert!(
            !admission_path.exists(),
            "the r+1 snapshot and token-derived placement retry exact cleanup"
        );
        store.release(lease).await.expect("release recovered phase");
        store
            .claim(&key, &worker, Duration::from_secs(60))
            .await
            .expect("the next phase is not blocked by a stale exact admission");
    }

    #[test]
    fn post_cas_admission_cleanup_cannot_turn_commit_success_into_failure() {
        let source = include_str!("fs.rs");
        let cas = source
            .find("match publish_new_file(&path, &encoded)")
            .expect("snapshot CAS");
        let cleanup = source[cas..]
            .find("if let Err(error) = self.retire_base_recovery_admission_after_commit_sync")
            .expect("best-effort post-CAS cleanup");
        let success = source[cas + cleanup..]
            .find("Ok(next)")
            .expect("committed revision remains success");
        assert!(success > 0);
        assert!(!source[cas..cas + cleanup].contains("pre-seed admission consumption failed"));
    }

    #[tokio::test]
    async fn scope_batch_classifies_each_base_after_one_compatibility_pass() {
        let harness = FsHarness::create();
        let store = harness.store();
        let before = store
            .scope_base_execution_recovery_authorities(
                "anonymous",
                "default",
                vec!["runtime-b".to_owned(), "runtime-a".to_owned()],
                20,
            )
            .await
            .expect("uncutover batch");
        assert!(!before.legacy_writers_retired);
        assert_eq!(before.authorities.len(), 2);
        assert!(before.authorities.values().all(|authority| matches!(
            authority,
            BaseExecutionRecoveryAuthority::Uncertain { .. }
        )));

        store
            .seal_legacy_writer_cutover("anonymous", "default", "deployment-batch", 21)
            .await
            .expect("seal batch cutover");
        let after = store
            .scope_base_execution_recovery_authorities(
                "anonymous",
                "default",
                vec!["runtime-a".to_owned(), "runtime-b".to_owned()],
                22,
            )
            .await
            .expect("cutover batch");
        assert!(after.legacy_writers_retired);
        assert!(after
            .authorities
            .values()
            .all(|authority| *authority == BaseExecutionRecoveryAuthority::Absent));
    }
}
