//! A rebuildable SQLite index beside the file store.
//!
//! Every list surface in this system used to materialise its entire result
//! set before taking a page: the tasks handler sliced a fully-loaded `Vec`,
//! monitors sorted in memory and scanned for its cursor row, attention turned
//! a cursor into an index with `position(…)`. Underneath all three, one
//! `read_dir` per scope and one directory per task. The cost of page 50 was
//! the cost of page 1 — invisible at a hundred tasks, and the first thing to
//! break once agents create tasks continuously.
//!
//! **Files stay the source of truth. This index is a rebuildable cache and
//! must never become a second one.** That is the `notmuch` / `mu` split: `mu`
//! keeps maildir files as canonical storage with a Xapian database as a fast
//! index into them. Delete this database, restart, and every list must be
//! identical. Anything that cannot be recovered by walking the disk does not
//! belong in these columns.
//!
//! Two consequences run through the whole module:
//!
//! - **A damaged index is discarded, never propagated.** An unreadable file
//!   or a schema version we do not recognise is deleted and recreated empty
//!   rather than returned as an error, because a cache that fails the caller
//!   has stopped being a cache.
//! - **A half-built index is never served.** `is_ready` reports false from
//!   the moment a rebuild starts until it finishes, including across a crash,
//!   so a reader falls back to the file walk instead of seeing a partial
//!   corpus that looks complete.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, types::Value, Connection, OptionalExtension};
use tracing::warn;

use crate::magician_v2::artifact_v2::models::{TaskLifecycle, TaskManifest, TaskState};
use crate::magician_v2::monitor_support::{parsed_schedule, schedule_is_paused};
use crate::magician_v2::task_lanes::TaskLane;

/// The on-disk schema this build understands, stored in `PRAGMA
/// user_version`. Bump it whenever `BOOTSTRAP_DDL` changes shape or a column's
/// meaning changes; an index written by any other version is dropped and
/// rebuilt from disk rather than migrated. Migrations are for source-of-truth
/// stores, and this is not one.
///
/// `2` added the `paused` column and the second keyset index. Version `3`
/// added `agent_id` and its active-status covering index so low-frequency
/// background admission does not walk every task directory.
/// Version `4` classifies canonical app runs as internal even in the legacy root.
/// Older files are discarded on open and rebuilt from disk — which is the intended
/// path, not a fallback: every column here is recoverable by walking the
/// records, so there is nothing a migration could preserve that a walk cannot.
pub const LIST_INDEX_SCHEMA_VERSION: i32 = 4;

/// The index file, created beside the file store's root.
pub const LIST_INDEX_FILE_NAME: &str = "list_index.db";

/// Meta key holding `in_progress` while a rebuild is running. Persisted
/// rather than in-memory so an interrupted rebuild is still visible as
/// unfinished after a restart.
const META_REBUILD_STATE: &str = "rebuild_state";
const REBUILD_STATE_IN_PROGRESS: &str = "in_progress";
const REBUILD_STATE_COMPLETE: &str = "complete";

/// Meta key prefix for a reconciliation watermark, one per `(kind, scope)`
/// unit: `reconcile:{kind}:{principal}:{workspace}`. Per unit rather than one
/// global mark so a unit that failed retries only its own span, and so a new
/// scope starts at the epoch and reads itself once instead of inheriting a
/// watermark that would skip every record it has.
const META_RECONCILE_PREFIX: &str = "reconcile";

/// One table, indexed for the queries these lists actually make.
///
/// **Two keysets, because the surfaces genuinely have two orders.** Every
/// list sorts `updated_at` descending; they disagree about what comes next
/// when two rows share a millisecond. Tasks, internal tasks and attention
/// break that tie by `id` DESCENDING; `/monitors` has always broken it by
/// `task_id` ASCENDING, and that order is already on the wire under a live
/// cursor. Giving each its own index is what lets monitors seek without
/// reordering same-instant rows underneath a reader — the alternative was to
/// make everything descending, which is a product-visible change to two
/// surfaces and to the three-platform fixture that pins them.
///
/// `list_rebuild_progress` and `list_index_meta` are the lifecycle's own
/// bookkeeping, not record data: they are what makes a rebuild resumable and
/// what tells a reader the index is not yet trustworthy.
const BOOTSTRAP_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS list_entries (
    id           TEXT NOT NULL,
    kind         TEXT NOT NULL,
    principal    TEXT NOT NULL,
    workspace    TEXT NOT NULL,
    updated_at   INTEGER NOT NULL,
    created_at   INTEGER NOT NULL,
    status       TEXT NOT NULL,
    agent_id     TEXT NOT NULL,
    lifecycle    TEXT,
    due_date     TEXT,
    tags         TEXT NOT NULL,
    title        TEXT,
    paused       INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (kind, id)
);
CREATE INDEX IF NOT EXISTS list_keyset
    ON list_entries (kind, principal, workspace, updated_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS list_keyset_id_asc
    ON list_entries (kind, principal, workspace, updated_at DESC, id ASC);
CREATE INDEX IF NOT EXISTS list_status  ON list_entries (kind, principal, workspace, status);
CREATE INDEX IF NOT EXISTS list_active_agent
    ON list_entries (kind, principal, workspace, status, agent_id);
CREATE INDEX IF NOT EXISTS list_due     ON list_entries (kind, principal, workspace, due_date);
CREATE INDEX IF NOT EXISTS list_paused  ON list_entries (kind, principal, workspace, paused);

CREATE TABLE IF NOT EXISTS list_rebuild_progress (
    kind         TEXT NOT NULL,
    principal    TEXT NOT NULL,
    workspace    TEXT NOT NULL,
    completed_at INTEGER NOT NULL,
    PRIMARY KEY (kind, principal, workspace)
);

CREATE TABLE IF NOT EXISTS list_index_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#;

/// What opening the index actually did. Callers use this to decide whether a
/// `rebuild_from_disk` is owed; tests use it to tell a reopen from a rebuild,
/// which is otherwise invisible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListIndexOpen {
    /// No file was there; an empty schema was created. A rebuild is owed.
    Created,
    /// An existing index at the current schema version was reused as-is.
    Reused,
    /// An existing index was discarded and recreated empty. A rebuild is owed.
    Discarded(DiscardReason),
}

impl ListIndexOpen {
    /// Whether the index came up EMPTY and therefore needs populating from
    /// disk before it can answer anything.
    ///
    /// This is not the question a boot gate asks. It is false for `Reused`,
    /// and a reused index can still be mid-rebuild — see
    /// [`ListIndex::rebuild_is_owed`], which is what a caller deciding
    /// whether to walk the disk must consult.
    pub fn needs_rebuild(&self) -> bool {
        !matches!(self, ListIndexOpen::Reused)
    }
}

/// Why an existing index file was thrown away. Recorded rather than
/// summarised so an operator can tell an expected schema bump from a real
/// corruption event in the logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscardReason {
    /// The file's `PRAGMA user_version` was not this build's.
    SchemaVersionChanged { found: i32, expected: i32 },
    /// The file could not be read as a SQLite database carrying our schema.
    Unreadable(String),
    /// An operator asked for a full reindex (`magician --reindex`). Recorded
    /// distinctly from the two failure reasons so a routine recovery cannot be
    /// read as a corruption event in the logs.
    OperatorRequested,
}

/// A rebuildable index into the file store. Cheap to clone — clones share one
/// connection behind a mutex, exactly as the other SQLite-backed stores in
/// this crate do.
#[derive(Clone)]
pub struct ListIndex {
    conn: Arc<Mutex<Connection>>,
    path: PathBuf,
    opened: ListIndexOpen,
    io_gate: Arc<tokio::sync::Mutex<()>>,
}

impl ListIndex {
    /// Compact recovery catalog for TaskPlan owners. Task state is written in
    /// the same recoverable task transaction as a plan entering/leaving
    /// `planning`, and the list index already projects that state. Reading the
    /// indexed ids avoids a boot-time walk of every task directory merely to
    /// discover the usually tiny planning subset.
    pub fn planning_task_ids(&self, scope: &ListScope) -> Result<BTreeSet<String>> {
        let conn = self.lock();
        let mut statement = conn
            .prepare(
                "SELECT DISTINCT id
                 FROM list_entries
                 WHERE kind IN ('task', 'internal')
                   AND principal = ?1 AND workspace = ?2
                   AND status = 'planning'
                 ORDER BY id",
            )
            .context("preparing planning task recovery lookup")?;
        let rows = statement
            .query_map(params![&scope.principal, &scope.workspace], |row| {
                row.get::<_, String>(0)
            })
            .context("reading planning task recovery lookup")?;
        rows.collect::<rusqlite::Result<BTreeSet<_>>>()
            .context("collecting planning task recovery lookup")
    }

    /// Open the index beside a file-store root, creating the directory if it
    /// is not there yet.
    pub fn open(base_root: &Path) -> Result<Self> {
        std::fs::create_dir_all(base_root)
            .with_context(|| format!("creating list index directory: {}", base_root.display()))?;
        Self::open_at(&base_root.join(LIST_INDEX_FILE_NAME))
    }

    /// Open (or recreate) the index at an exact path.
    ///
    /// This never returns an error for a *damaged* index — only for a path it
    /// cannot write to at all. A corrupt cache that errors the caller has
    /// stopped being a cache; the file is deleted and a fresh empty schema
    /// takes its place, and the caller learns what happened from
    /// [`ListIndex::opened`].
    pub fn open_at(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating list index directory: {}", parent.display()))?;
        }

        if std::fs::metadata(db_path).is_ok() {
            match Self::attach(db_path) {
                Ok((conn, version)) if version == LIST_INDEX_SCHEMA_VERSION => {
                    return Ok(Self::assembled(conn, db_path, ListIndexOpen::Reused));
                },
                Ok((conn, version)) => {
                    drop(conn);
                    let reason = DiscardReason::SchemaVersionChanged {
                        found: version,
                        expected: LIST_INDEX_SCHEMA_VERSION,
                    };
                    warn!(
                        path = %db_path.display(),
                        found_version = version,
                        expected_version = LIST_INDEX_SCHEMA_VERSION,
                        "[LIST-INDEX] Discarding list index at a different schema version"
                    );
                    let conn = Self::recreate(db_path)?;
                    return Ok(Self::assembled(
                        conn,
                        db_path,
                        ListIndexOpen::Discarded(reason),
                    ));
                },
                Err(error) => {
                    let detail = format!("{error:#}");
                    warn!(
                        path = %db_path.display(),
                        error = %detail,
                        "[LIST-INDEX] Discarding unreadable list index and rebuilding from disk"
                    );
                    let conn = Self::recreate(db_path)?;
                    return Ok(Self::assembled(
                        conn,
                        db_path,
                        ListIndexOpen::Discarded(DiscardReason::Unreadable(detail)),
                    ));
                },
            }
        }

        let conn = Self::create(db_path)?;
        Ok(Self::assembled(conn, db_path, ListIndexOpen::Created))
    }

    /// Open the index beside a file-store root, throwing away whatever was
    /// there.
    ///
    /// The operator's recovery route, and the whole of what `--reindex` does
    /// before it walks. A boot rebuild RESUMES: it trusts every `(root,
    /// scope)` unit a previous run committed, which is what makes an
    /// interrupted rebuild cheap and what makes it unable to repair a unit
    /// whose rows are merely WRONG rather than missing. Only a discard can, so
    /// the recovery path discards first and re-walks everything.
    pub fn open_discarding(base_root: &Path) -> Result<Self> {
        std::fs::create_dir_all(base_root)
            .with_context(|| format!("creating list index directory: {}", base_root.display()))?;
        let db_path = base_root.join(LIST_INDEX_FILE_NAME);
        let discarded = std::fs::metadata(&db_path).is_ok();
        let conn = Self::recreate(&db_path)?;
        let opened = if discarded {
            ListIndexOpen::Discarded(DiscardReason::OperatorRequested)
        } else {
            ListIndexOpen::Created
        };
        Ok(Self::assembled(conn, &db_path, opened))
    }

    /// What the last open did.
    pub fn opened(&self) -> &ListIndexOpen {
        &self.opened
    }

    /// Where this index lives on disk.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The schema version recorded in the open database.
    pub fn schema_version(&self) -> Result<i32> {
        let conn = self.lock();
        read_user_version(&conn)
    }

    /// Whether a reader may trust this index yet.
    ///
    /// False while a rebuild is in flight — and still false after a crash
    /// mid-rebuild, because the marker is on disk. A caller that sees false
    /// must serve the list from the file walk: correctness never depends on
    /// the cache, and a half-built index looks exactly like a complete one
    /// with fewer tasks in it.
    pub fn is_ready(&self) -> Result<bool> {
        let conn = self.lock();
        Ok(read_meta(&conn, META_REBUILD_STATE)?.as_deref() == Some(REBUILD_STATE_COMPLETE))
    }

    /// Whether this index owes a rebuild before anything may be served from
    /// it. **This is the gate a caller that opened the index must use.**
    ///
    /// Not `opened().needs_rebuild()`, which answers the narrower question of
    /// whether this open came up empty and is therefore FALSE for `Reused` —
    /// including the reused index whose rebuild never finished. That file is
    /// at the current schema version, so it is reused as-is, while its marker
    /// still reads `in_progress` and `is_ready()` stays false. Gated on the
    /// open outcome alone, such an index is never rebuilt on any later boot:
    /// the resume path below is never entered, and every list serves from the
    /// walk this index exists to remove — invisibly, because a fallback that
    /// works looks exactly like a cache that is being used.
    ///
    /// Both terms are kept rather than collapsing to `!is_ready()`. They are
    /// two independent reasons, and a `create` that stopped stamping
    /// `in_progress` would silently turn the second into the only one.
    pub fn rebuild_is_owed(&self) -> Result<bool> {
        Ok(self.opened().needs_rebuild() || !self.is_ready()?)
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn assembled(conn: Connection, db_path: &Path, opened: ListIndexOpen) -> Self {
        Self {
            conn: Arc::new(Mutex::new(conn)),
            path: db_path.to_path_buf(),
            opened,
            io_gate: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Open an existing file and prove it is actually ours: the pragma read
    /// catches "not a database at all", the table probe catches a database
    /// that is readable but does not carry our schema.
    fn attach(db_path: &Path) -> Result<(Connection, i32)> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("opening list index at {}", db_path.display()))?;
        let version = read_user_version(&conn).context("reading list index schema version")?;
        if version == LIST_INDEX_SCHEMA_VERSION {
            conn.query_row("SELECT COUNT(*) FROM list_entries", [], |row| {
                row.get::<_, i64>(0)
            })
            .context("probing list index schema")?;
        }
        Ok((conn, version))
    }

    fn create(db_path: &Path) -> Result<Connection> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("creating list index at {}", db_path.display()))?;
        conn.query_row("PRAGMA journal_mode=WAL", [], |_row| Ok(()))
            .context("enabling WAL journal mode on the list index")?;
        conn.execute_batch(BOOTSTRAP_DDL)
            .context("bootstrapping list index schema")?;
        conn.pragma_update(None, "user_version", LIST_INDEX_SCHEMA_VERSION)
            .context("stamping list index schema version")?;
        // A freshly created index holds nothing, so it is not ready until
        // something populates it. Absent-meaning-not-ready would work too;
        // writing the state explicitly keeps the marker's three values
        // (missing / in_progress / complete) readable in the file itself.
        write_meta(&conn, META_REBUILD_STATE, REBUILD_STATE_IN_PROGRESS)
            .context("marking a fresh list index as awaiting its first rebuild")?;
        Ok(conn)
    }

    /// Delete a damaged or stale index and put an empty schema in its place.
    ///
    /// The `-wal` and `-shm` sidecars go with it. Leaving them behind lets a
    /// stale write-ahead log be replayed into the new file, which is how a
    /// discarded index comes back from the dead carrying the rows that made
    /// it unreadable in the first place.
    fn recreate(db_path: &Path) -> Result<Connection> {
        for suffix in ["", "-wal", "-shm"] {
            let victim = sidecar_path(db_path, suffix);
            match std::fs::remove_file(&victim) {
                Ok(()) => {},
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("discarding list index file {}", victim.display())
                    });
                },
            }
        }
        Self::create(db_path)
    }
}

// ---------------------------------------------------------------------------
// The rows
// ---------------------------------------------------------------------------

/// Which list a row belongs to. One record can appear under more than one
/// kind — a monitor is a task, and shows on both surfaces — which is why the
/// primary key is `(kind, id)` and not `id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ListKind {
    /// The user-visible `tasks/` root.
    Task,
    /// The `internal_tasks/` root behind the `/internal-tasks` surface.
    Internal,
    /// A user-visible task carrying a `monitor_spec`.
    Monitor,
    /// **Unused.** No producer writes an attention row and no read path asks
    /// for one: `rebuild_from_disk` walks `Task` and `Internal` only, and the
    /// only `upsert`s under this kind are in this module's own tests. The
    /// variant is the schema's placeholder for the surface, kept because the
    /// `kind` strings are stable and adding one later is a version bump; it is
    /// not a description of anything the system currently does. `Monitor` was
    /// in exactly this position until `/monitors` started paging from it.
    Attention,
}

impl ListKind {
    pub const ALL: [ListKind; 4] = [
        ListKind::Task,
        ListKind::Internal,
        ListKind::Monitor,
        ListKind::Attention,
    ];

    /// The value stored in the `kind` column. Stable — changing one of these
    /// strings invalidates every row already written under it, so it is a
    /// schema change and needs a version bump.
    pub fn as_str(self) -> &'static str {
        match self {
            ListKind::Task => "task",
            ListKind::Internal => "internal",
            ListKind::Monitor => "monitor",
            ListKind::Attention => "attention",
        }
    }

    /// Exact-match parse. Unknown values are rejected rather than defaulted,
    /// for the same reason `TaskLane::parse` rejects them: silently serving a
    /// different list is worse than refusing.
    pub fn parse(value: &str) -> Option<ListKind> {
        ListKind::ALL
            .into_iter()
            .find(|kind| kind.as_str() == value)
    }
}

/// The principal/workspace pair every list filters by. A column, not a
/// database per scope.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ListScope {
    pub principal: String,
    pub workspace: String,
}

impl ListScope {
    pub fn new(principal: impl Into<String>, workspace: impl Into<String>) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
        }
    }
}

/// One of the two roots a walk visits, paired with the scope it walks in.
///
/// **A separate type from `ListKind` because not every kind names a
/// directory.** `Monitor` rows are produced by the tasks walk and `Attention`
/// has no producer at all, so "which root" and "which rows" are two different
/// questions that a single enum kept answering with a `_ =>` wildcard. Both
/// walkers iterate `UnitKind::ALL`, which means a kind added to the schema
/// cannot silently inherit `tasks/` and a `[Task, Monitor]` sweep — there is
/// nowhere for it to inherit them from.
///
/// The two facts below were each stated twice before, once in the rebuild and
/// once in the reconciler, and the copies were free to drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnitKind {
    Task,
    Internal,
}

impl UnitKind {
    const ALL: [UnitKind; 2] = [UnitKind::Task, UnitKind::Internal];

    /// The row kind a walk of this root produces for each record it reads.
    fn row_kind(self) -> ListKind {
        match self {
            UnitKind::Task => ListKind::Task,
            UnitKind::Internal => ListKind::Internal,
        }
    }

    /// Every row kind this unit's walk is authoritative over — what a rebuild
    /// may clear before re-walking it, and what a reconciliation may sweep.
    ///
    /// **The tasks unit owns `monitor` rows too.** A monitor row is produced
    /// by the tasks walk, so a task that stopped being a monitor, or whose
    /// directory went away, has to lose both of its rows with the one unit.
    fn row_kinds(self) -> &'static [ListKind] {
        match self {
            UnitKind::Task => &[ListKind::Task, ListKind::Monitor],
            UnitKind::Internal => &[ListKind::Internal],
        }
    }

    fn root(self, scopes_root: &Path, scope: &ListScope) -> PathBuf {
        match self {
            UnitKind::Task => tasks_root(scopes_root, scope),
            UnitKind::Internal => internal_tasks_root(scopes_root, scope),
        }
    }

    /// The stored spelling, shared with `list_rebuild_progress` and the
    /// reconciliation watermark keys. Delegated to `ListKind` rather than
    /// spelled again, because both are already on disk under those strings.
    fn as_str(self) -> &'static str {
        self.row_kind().as_str()
    }
}

/// One indexed record — the narrow slice of a task a list page and a lane
/// predicate need, and deliberately nothing more. Everything here is
/// recoverable by re-reading the record's files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListEntry {
    pub kind: ListKind,
    pub id: String,
    pub scope: ListScope,
    /// Epoch milliseconds. The listing sorts on this, so it must be built
    /// from the same field the list item reports: `TaskState.updated_at`.
    pub updated_at: i64,
    /// Epoch milliseconds, from `TaskManifest.created_at`.
    pub created_at: i64,
    pub status: String,
    pub agent_id: String,
    pub lifecycle: Option<String>,
    /// Stored as written, not normalised — the lane predicates compare it as
    /// a string exactly as `TaskLane` does.
    pub due_date: Option<String>,
    /// Tag NAMES, matching what the handler's lane slice passes to
    /// `TaskLane::matches`. Serialised as a JSON array; an empty list is
    /// always the literal `[]`, which is what the `inbox` predicate tests.
    pub tags: Vec<String>,
    pub title: Option<String>,
    /// `schedule.paused == Some(true)`, which is what `/monitors`'
    /// `state=active|paused` filter reads. Held as a column rather than
    /// recomputed per row because a filter that needs the manifest is a
    /// filter the index cannot express, and one it cannot express is one
    /// that drags the whole pool back into memory to apply.
    ///
    /// It is `false` for everything that has no schedule, exactly as
    /// `schedule_is_paused` reports for `None`. Recoverable from disk like
    /// every other column here.
    pub paused: bool,
}

impl ListEntry {
    fn tags_json(&self) -> String {
        serde_json::to_string(&self.tags).unwrap_or_else(|_| "[]".to_string())
    }
}

/// Epoch milliseconds from an RFC3339 timestamp, which is what every task
/// record on disk carries (`Utc::now().to_rfc3339()`).
///
/// `None` rather than a guess when the string is not a timestamp: the caller
/// counts those so an unparseable record is visible in the rebuild report
/// instead of silently sorting to the bottom of every list.
pub fn epoch_millis_from_rfc3339(timestamp: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|value| value.with_timezone(&Utc).timestamp_millis())
}

fn lifecycle_str(lifecycle: &TaskLifecycle) -> &'static str {
    // Exhaustive on purpose: a new lifecycle variant must fail to compile
    // here rather than quietly index as something else.
    match lifecycle {
        TaskLifecycle::Persistent => "persistent",
        TaskLifecycle::Internal => "internal",
    }
}

/// What a single-task reindex did.
///
/// The `rewritten` flag is not decoration: a reindex that declines to write
/// because nothing indexed changed is otherwise indistinguishable from one
/// that wrote the same values back, and the difference is the entire point of
/// the comparison — a test asserting only on `rows` would pass with the
/// comparison deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReindexOutcome {
    /// How many rows the task has after this call — 0 for a record that is
    /// gone from both roots, 2 for a user-visible monitor.
    pub rows: usize,
    /// Whether the index was actually written. False when the record's thirteen
    /// indexed columns already matched the rows on file.
    pub rewritten: bool,
}

/// What a rebuild did. Every field is a count a caller can assert on — a
/// rebuild that reports nothing is indistinguishable from one that indexed
/// nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebuildReport {
    /// `(root, scope)` units walked this run.
    pub units_walked: usize,
    /// `(root, scope)` units skipped because a previous, interrupted run had
    /// already finished them.
    pub units_resumed: usize,
    /// Rows written, counting a monitor's second row separately.
    pub entries_indexed: usize,
    /// Task directories that could not be read as a record — the file
    /// vanished mid-walk, or its JSON is unreadable. Skipped, never fatal.
    pub entries_skipped: usize,
    /// Directories under `tasks/` that the user-visible listing hides, so the
    /// index hides them too.
    pub entries_hidden: usize,
    /// Records whose timestamps were not RFC3339 and were indexed at 0.
    pub unparsed_timestamps: usize,
    /// `(kind, scope)` units skipped because their root could not be read.
    ///
    /// **Non-zero means the index is deliberately left unready**: the walk
    /// never saw those records, so the rows are incomplete, and a reader must
    /// keep using the file walk until a later rebuild finishes the job.
    /// Symmetric with [`ReconcileReport::units_failed`], and for the same
    /// reason — a failure count belongs in the struct that reports what the
    /// run did, not only in a log line a caller may not be collecting.
    pub units_failed: usize,
}

/// What one reconciliation pass did. `records_read` is the design invariant
/// made observable: an unchanged corpus must cost stats, not reads.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// `(kind, scope)` units that finished and advanced their watermark.
    pub units: usize,
    /// `(kind, scope)` units that errored and left their watermark where it
    /// was, owing their span to the next pass.
    ///
    /// A field rather than only a log line: "forty units have failed every
    /// pass for a week" is a number the struct that reports what the pass did
    /// should carry, not something a caller has to reconstruct by grepping
    /// `warn!` output it may not even be collecting.
    pub units_failed: usize,
    /// Task records re-read from disk. The number a healthy idle pass must
    /// report is zero.
    pub records_read: usize,
    /// Records whose indexed columns actually changed as a result — the drift
    /// this pass repaired, straight from `ReindexOutcome::rewritten`.
    pub repaired: usize,
    /// Rows dropped because the record they name is gone from the unit's root.
    /// Counted per ROW, so a monitor that lost its directory counts twice: the
    /// task row and the monitor row it also carried.
    pub removed: usize,
}

// ---------------------------------------------------------------------------
// Populating — the walk, once, and one record at a time thereafter
// ---------------------------------------------------------------------------

impl ListIndex {
    /// One indexed answer for the social worker's busy/idle admission gate.
    /// This intentionally returns only agent IDs; reconstructing task cards
    /// would defeat the purpose of avoiding two complete corpus walks.
    pub fn active_task_agent_ids(&self, scope: &ListScope) -> Result<BTreeSet<String>> {
        let conn = self.lock();
        let mut statement = conn
            .prepare(
                "SELECT DISTINCT agent_id
                 FROM list_entries
                 WHERE kind IN ('task', 'internal')
                   AND principal = ?1 AND workspace = ?2
                   AND status IN (
                     'planning', 'running', 'paused', 'paused_by_user',
                     'waiting_for_user', 'waiting_for_confirmation',
                     'waiting_for_children'
                   )
                   AND agent_id <> ''",
            )
            .context("preparing active task-agent lookup")?;
        let rows = statement
            .query_map(params![&scope.principal, &scope.workspace], |row| {
                row.get::<_, String>(0)
            })
            .context("reading active task-agent lookup")?;
        rows.collect::<rusqlite::Result<BTreeSet<_>>>()
            .context("collecting active task-agent lookup")
    }

    /// One indexed answer for the roster's busy/idle face: every task the
    /// runtime is progressing right now, with the agent that owns it.
    ///
    /// `planning` and `running` only, on purpose. A task parked for a person
    /// (`paused`, `paused_by_user`, `waiting_for_*`) has nothing in flight, and
    /// an agent that has sat behind one for days is idle in every sense that
    /// matters to a caller asking "is this agent busy" — reading it as busy
    /// would silence it indefinitely. Task ids ride along so a caller can look
    /// under each task for the executions it holds; this method stays a
    /// two-column index read and never reconstructs task cards.
    pub fn working_task_refs(&self, scope: &ListScope) -> Result<Vec<(String, String)>> {
        let conn = self.lock();
        let mut statement = conn
            .prepare(
                "SELECT id, agent_id
                 FROM list_entries
                 WHERE kind IN ('task', 'internal')
                   AND principal = ?1 AND workspace = ?2
                   AND status IN ('planning', 'running')
                   AND agent_id <> ''",
            )
            .context("preparing working task lookup")?;
        let rows = statement
            .query_map(params![&scope.principal, &scope.workspace], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .context("reading working task lookup")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("collecting working task lookup")
    }

    /// Insert or replace one row.
    pub fn upsert(&self, entry: &ListEntry) -> Result<()> {
        let conn = self.lock();
        upsert_in(&conn, entry)
    }

    /// Insert or replace a batch in one transaction.
    pub fn upsert_many(&self, entries: &[ListEntry]) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn
            .transaction()
            .context("beginning a list index upsert transaction")?;
        for entry in entries {
            upsert_in(&tx, entry)?;
        }
        tx.commit()
            .context("committing a list index upsert transaction")?;
        Ok(())
    }

    /// Drop one row. Returns whether a row was actually there.
    pub fn remove(&self, kind: ListKind, id: &str) -> Result<bool> {
        let conn = self.lock();
        let removed = conn
            .execute(
                "DELETE FROM list_entries WHERE kind = ?1 AND id = ?2",
                params![kind.as_str(), id],
            )
            .context("deleting a list index row")?;
        Ok(removed > 0)
    }

    /// Drop every row a task can own — the `tasks` row, the `internal` row,
    /// and the `monitor` row a task with a spec also carries. This is what a
    /// delete path calls, and it must not need to know which of the three the
    /// record had.
    pub fn remove_task(&self, task_id: &str) -> Result<usize> {
        let conn = self.lock();
        let removed = conn
            .execute(
                "DELETE FROM list_entries
                 WHERE id = ?1 AND kind IN ('task', 'internal', 'monitor')",
                params![task_id],
            )
            .context("deleting a task's list index rows")?;
        Ok(removed)
    }

    /// Drop every row of one kind in one scope. Used by the rebuild so a
    /// re-walked scope cannot leave behind rows for records that are gone.
    pub fn remove_scope(&self, kind: ListKind, scope: &ListScope) -> Result<usize> {
        let conn = self.lock();
        let removed = conn
            .execute(
                "DELETE FROM list_entries WHERE kind = ?1 AND principal = ?2 AND workspace = ?3",
                params![kind.as_str(), &scope.principal, &scope.workspace],
            )
            .context("deleting a scope's list index rows")?;
        Ok(removed)
    }

    /// Re-read one task from disk and bring its rows in line — the call a
    /// write path makes in the same call that writes the file.
    ///
    /// Re-reading rather than taking the caller's in-memory record is
    /// deliberate: the index then reflects what is actually on disk, which is
    /// the only thing a rebuild could reproduce. A record that is gone from
    /// both roots has its rows removed, so this is also the delete path.
    ///
    /// **Writes nothing when nothing indexed changed.** This runs on every
    /// task-record commit, and a running execution commits constantly — but
    /// most of those commits move fields no column here holds. A step event,
    /// for instance, writes `task_state.json` only to advance
    /// `last_progress_at`, which is not indexed; the thirteen columns come out
    /// identical and the `DELETE`+upsert is pure write amplification.
    ///
    /// The comparison is **provably** safe, not probably: the current rows are
    /// read inside the same transaction as the write they would replace, so
    /// they are exactly the rows a rewrite would have replaced — no window
    /// between the two, and no reliance on the index having been correct
    /// before. Anything different at all falls through and writes, including
    /// a missing row where there should be one, an extra row where there
    /// should not be, and a row that another process left behind.
    ///
    /// Returns what it found and whether it wrote — the flag is what makes
    /// "wrote nothing" distinguishable from "did nothing", which is otherwise
    /// invisible to a caller and to a test.
    pub fn index_task_from_disk(
        &self,
        scopes_root: &Path,
        scope: &ListScope,
        task_id: &str,
    ) -> Result<ReindexOutcome> {
        let mut entries = Vec::new();
        let mut stats = RebuildReport::default();

        // Internal first, mirroring `ArtifactV2Workspace::task_dir`: a task
        // present under both roots is the internal one, and the user-visible
        // listing never shows it.
        let internal_dir = internal_tasks_root(scopes_root, scope).join(task_id);
        if internal_dir.is_dir() {
            read_task_entry(
                &internal_dir,
                ListKind::Internal,
                scope,
                task_id,
                &mut entries,
                &mut stats,
            );
        } else {
            let task_dir = tasks_root(scopes_root, scope).join(task_id);
            if task_dir.is_dir() {
                read_task_entry(
                    &task_dir,
                    ListKind::Task,
                    scope,
                    task_id,
                    &mut entries,
                    &mut stats,
                );
            }
        }

        let mut candidate: Vec<IndexedColumns> = entries.iter().map(entry_columns).collect();
        candidate.sort();

        let mut conn = self.lock();
        let tx = conn
            .transaction()
            .context("beginning a single-task reindex transaction")?;
        if candidate == task_columns_in(&tx, task_id)? {
            // Read-only; dropping the transaction rolls back nothing.
            return Ok(ReindexOutcome {
                rows: entries.len(),
                rewritten: false,
            });
        }
        tx.execute(
            "DELETE FROM list_entries
             WHERE id = ?1 AND kind IN ('task', 'internal', 'monitor')",
            params![task_id],
        )
        .context("clearing a task's list index rows before reindexing")?;
        for entry in &entries {
            upsert_in(&tx, entry)?;
        }
        tx.commit()
            .context("committing a single-task reindex transaction")?;
        Ok(ReindexOutcome {
            rows: entries.len(),
            rewritten: true,
        })
    }

    /// Walk the scoped task roots once and index what is there.
    ///
    /// **Resumable.** Progress is recorded per `(root, scope)` unit inside the
    /// same transaction that writes that unit's rows, so an interrupted
    /// rebuild resumes at the next unindexed unit rather than starting over.
    /// A unit is all-or-nothing: a crash mid-unit leaves no progress row, and
    /// the next run re-walks it.
    ///
    /// **A record whose files vanish mid-walk is skipped, not fatal** — the
    /// same tolerance the existing walk already has, because a task can be
    /// deleted between the `read_dir` and the read of its manifest.
    ///
    /// While this runs, [`ListIndex::is_ready`] is false and readers must use
    /// the file walk.
    pub fn rebuild_from_disk(&self, scopes_root: &Path) -> Result<RebuildReport> {
        {
            let conn = self.lock();
            write_meta(&conn, META_REBUILD_STATE, REBUILD_STATE_IN_PROGRESS)
                .context("marking a list index rebuild in flight")?;
        }

        let mut report = RebuildReport::default();
        for scope in enumerate_scopes(scopes_root)? {
            for kind in UnitKind::ALL {
                if self.unit_already_indexed(kind, &scope)? {
                    report.units_resumed += 1;
                    continue;
                }

                let root = kind.root(scopes_root, &scope);
                let mut entries = Vec::new();
                let mut unit = RebuildReport::default();
                // A root that cannot be READ skips its unit. It must not
                // commit: `commit_unit` clears the unit's rows before
                // inserting and then records progress, so an empty walk would
                // write an empty scope and mark it DONE — and since only a
                // FINISHED rebuild clears the reconciler's watermarks, nothing
                // would ever re-read it.
                //
                // Skipped, not fatal. One unstat-able inode must not turn a
                // working index permanently unready, which is what aborting
                // the whole walk did: the readiness marker is stamped before
                // the walk begins, so every list would fall back to the file
                // walk on every boot, forever. This is the same per-unit
                // granularity the reconciler uses — one bad scope costs its
                // own rows, not the corpus.
                if let Err(error) =
                    walk_task_root(scopes_root, &root, kind, &scope, &mut entries, &mut unit)
                {
                    report.units_failed += 1;
                    warn!(
                        kind = kind.as_str(),
                        principal = %scope.principal,
                        workspace = %scope.workspace,
                        error = %format!("{error:#}"),
                        "[LIST-INDEX] a rebuild unit could not be walked; it keeps no progress row and the index stays unready"
                    );
                    continue;
                }

                self.commit_unit(kind, &scope, &entries)?;

                report.units_walked += 1;
                report.entries_indexed += entries.len();
                report.entries_skipped += unit.entries_skipped;
                report.entries_hidden += unit.entries_hidden;
                report.unparsed_timestamps += unit.unparsed_timestamps;
            }
        }

        if report.units_failed > 0 {
            // Everything that DID walk keeps its rows and its progress row, so
            // a later rebuild resumes at the units this one could not read
            // rather than starting over. What it must not do is claim to be
            // complete: the marker stays `in_progress`, `is_ready()` stays
            // false, and readers keep serving from the file walk — the same
            // answer they get during any unfinished rebuild.
            return Ok(report);
        }

        {
            let conn = self.lock();
            conn.execute("DELETE FROM list_rebuild_progress", [])
                .context("clearing list index rebuild progress")?;
            // The reconciler's watermarks describe a scan of the rows this
            // rebuild has just replaced, so they are stale by construction —
            // and a rebuild is precisely what an operator runs when the index
            // was WRONG. A surviving watermark would tell the next pass to
            // skip exactly the records the rebuild was run to fix, which is
            // how a repaired index quietly stops being repaired. One pass
            // re-reading every record once is the whole cost of dropping them.
            // A prefix comparison rather than `LIKE`, which would read `_`
            // and `%` in the prefix as wildcards. Harmless for today's
            // constant and silently wrong the day it gains an underscore.
            conn.execute(
                "DELETE FROM list_index_meta WHERE substr(key, 1, length(?1)) = ?1",
                params![format!("{META_RECONCILE_PREFIX}:")],
            )
            .context("clearing list index reconciliation watermarks")?;
            write_meta(&conn, META_REBUILD_STATE, REBUILD_STATE_COMPLETE)
                .context("marking the list index ready")?;
        }
        Ok(report)
    }

    async fn spawn_index_blocking<T, F>(&self, label: &'static str, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        let _gate = self.io_gate.lock().await;
        magician_core::blocking_admission::spawn_blocking_admitted(f)
            .await
            .with_context(|| format!("list index {label} task panicked"))?
    }

    /// `rebuild_from_disk` off the async runtime's worker threads. The walk is
    /// disk-bound and unbounded in size; it must not run on a reactor thread.
    pub async fn rebuild_from_disk_async(&self, scopes_root: PathBuf) -> Result<RebuildReport> {
        let index = self.clone();
        self.spawn_index_blocking("rebuild", move || index.rebuild_from_disk(&scopes_root))
            .await
    }

    /// SQLite reindex of one task, admitted on the async side first so the
    /// connection mutex wait cannot occupy a process-wide blocking permit.
    pub async fn index_task_from_disk_admitted(
        &self,
        scopes_root: PathBuf,
        scope: ListScope,
        task_id: String,
    ) -> Result<ReindexOutcome> {
        let index = self.clone();
        self.spawn_index_blocking("reindex", move || {
            index.index_task_from_disk(&scopes_root, &scope, &task_id)
        })
        .await
    }

    /// Drop a deleted task's index rows through the same admitted funnel.
    pub async fn remove_task_admitted(&self, task_id: String) -> Result<usize> {
        let index = self.clone();
        self.spawn_index_blocking("deindex", move || index.remove_task(&task_id))
            .await
    }

    /// Ready-or-none agent-id lookup through the same admitted funnel.
    pub async fn active_task_agent_ids_admitted(
        &self,
        scope: ListScope,
    ) -> Result<Option<BTreeSet<String>>> {
        let index = self.clone();
        self.spawn_index_blocking("active_task_agent_ids", move || {
            if !index.is_ready()? {
                return Ok(None);
            }
            index.active_task_agent_ids(&scope).map(Some)
        })
        .await
    }

    /// Ready-or-none working-task lookup through the same admitted funnel.
    pub async fn working_task_refs_admitted(
        &self,
        scope: ListScope,
    ) -> Result<Option<Vec<(String, String)>>> {
        let index = self.clone();
        self.spawn_index_blocking("working_task_refs", move || {
            if !index.is_ready()? {
                return Ok(None);
            }
            index.working_task_refs(&scope).map(Some)
        })
        .await
    }

    pub async fn planning_task_ids_admitted(
        &self,
        scope: ListScope,
    ) -> Result<Option<Vec<String>>> {
        let index = self.clone();
        self.spawn_index_blocking("planning_task_ids", move || {
            if !index.is_ready()? {
                return Ok(None);
            }
            index
                .planning_task_ids(&scope)
                .map(|ids| Some(ids.into_iter().collect()))
        })
        .await
    }

    /// How many `(root, scope)` units a previous interrupted rebuild finished.
    /// Non-zero means a rebuild is unfinished.
    pub fn rebuild_progress_units(&self) -> Result<usize> {
        let conn = self.lock();
        let count = conn
            .query_row("SELECT COUNT(*) FROM list_rebuild_progress", [], |row| {
                row.get::<_, i64>(0)
            })
            .context("reading list index rebuild progress")?;
        Ok(count as usize)
    }

    fn unit_already_indexed(&self, kind: UnitKind, scope: &ListScope) -> Result<bool> {
        let conn = self.lock();
        let found = conn
            .query_row(
                "SELECT 1 FROM list_rebuild_progress
                 WHERE kind = ?1 AND principal = ?2 AND workspace = ?3",
                params![kind.as_str(), &scope.principal, &scope.workspace],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .context("reading a list index rebuild progress row")?;
        Ok(found.is_some())
    }

    /// One unit's rows and its progress marker land together or not at all.
    /// Split across two transactions, a crash between them would mark a unit
    /// done that holds half its rows — the exact failure a resumable rebuild
    /// exists to avoid.
    fn commit_unit(&self, kind: UnitKind, scope: &ListScope, entries: &[ListEntry]) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn
            .transaction()
            .context("beginning a list index rebuild unit transaction")?;

        // The same `row_kinds` the reconciler sweeps by, so "what this unit
        // owns" is one fact rather than a `kind == Task` special case here and
        // a slice literal over there.
        for row_kind in kind.row_kinds() {
            tx.execute(
                "DELETE FROM list_entries WHERE kind = ?1 AND principal = ?2 AND workspace = ?3",
                params![row_kind.as_str(), &scope.principal, &scope.workspace],
            )
            .context("clearing a unit's rows before re-walking it")?;
        }
        for entry in entries {
            upsert_in(&tx, entry)?;
        }
        tx.execute(
            "INSERT INTO list_rebuild_progress (kind, principal, workspace, completed_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(kind, principal, workspace)
                 DO UPDATE SET completed_at = excluded.completed_at",
            params![
                kind.as_str(),
                &scope.principal,
                &scope.workspace,
                Utc::now().timestamp_millis()
            ],
        )
        .context("recording list index rebuild progress")?;

        tx.commit()
            .context("committing a list index rebuild unit transaction")?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Reconciling — the periodic pass that repairs whatever a writer missed
// ---------------------------------------------------------------------------

impl ListIndex {
    /// One reconciliation pass: repair whatever disagrees with disk, reading
    /// only what has changed since the last pass.
    ///
    /// **This exists because the write hooks are a static argument, and that
    /// argument has been wrong twice.** Three separate correctness bugs were
    /// the same invariant escaping through a different door — a task record
    /// landing on disk without the index being told. Each fix moved the hook
    /// to a chokepoint; each time another path existed that bypassed it. A
    /// periodic pass does not care which door: a writer nobody hooked costs
    /// one interval of wrongness instead of an unbounded wrong answer.
    ///
    /// **Declines a half-built index.** Such an index is not WRONG, it is
    /// INCOMPLETE — it looks exactly like a complete one with fewer tasks in
    /// it — and a pass over it would advance watermarks past records the
    /// rebuild has yet to reach, turning a temporary gap into a permanent one.
    ///
    /// One `(kind, scope)` unit failing logs and leaves that unit's watermark
    /// where it was, so the next pass retries the span. It never aborts the
    /// others: a single unreadable scope must not stop the rest of the corpus
    /// from being repaired.
    pub fn reconcile_from_disk(&self, scopes_root: &Path) -> Result<ReconcileReport> {
        let mut report = ReconcileReport::default();
        if !self.is_ready()? {
            return Ok(report);
        }

        for scope in enumerate_scopes(scopes_root)? {
            for kind in UnitKind::ALL {
                match self.reconcile_unit(scopes_root, kind, &scope, &mut report) {
                    Ok(()) => report.units += 1,
                    Err(error) => {
                        report.units_failed += 1;
                        // `{:#}` rather than `%error`: `Display` on an
                        // `anyhow::Error` prints only the outermost context,
                        // so the log would say which root failed and never
                        // whether it was EACCES, EMFILE or ENOTDIR — the
                        // three cases the policy is written around, and the
                        // only part an operator can act on.
                        warn!(
                            kind = kind.as_str(),
                            principal = %scope.principal,
                            workspace = %scope.workspace,
                            error = %format!("{error:#}"),
                            "[LIST-INDEX] a reconciliation unit failed; its watermark stays put so the next pass retries the span"
                        );
                    },
                }
            }
        }

        Ok(report)
    }

    fn reconcile_unit(
        &self,
        scopes_root: &Path,
        kind: UnitKind,
        scope: &ListScope,
        report: &mut ReconcileReport,
    ) -> Result<()> {
        // Sampled here rather than passed in, so the contract — before the
        // readdir, so a write landing mid-pass is caught by the NEXT pass
        // instead of missed by this one — sits against the readdir it
        // constrains and cannot be broken by a caller.
        let started_at = SystemTime::now();
        let root = kind.root(scopes_root, scope);

        // The scan runs with no lock held; `index_task_from_disk` takes the
        // connection per call as it already does. Holding it across a
        // directory walk would block every list the index exists to serve.
        let watermark = self.reconcile_watermark(kind, scope)?;
        let mut scan = scan_unit_root(&root, watermark)?;
        if kind == UnitKind::Internal {
            let legacy = scan_unit_root(&tasks_root(scopes_root, scope), watermark)?;
            for task_id in legacy.named {
                if crate::magician_v2::artifact_v2::service::is_canonical_app_workflow_task_id(
                    &task_id,
                ) {
                    scan.named.insert(task_id);
                }
            }
            for task_id in legacy.changed {
                if crate::magician_v2::artifact_v2::service::is_canonical_app_workflow_task_id(
                    &task_id,
                ) && !scan.changed.contains(&task_id)
                {
                    scan.changed.push(task_id);
                }
            }
        }

        for task_id in &scan.changed {
            let outcome = self.index_task_from_disk(scopes_root, scope, task_id)?;
            report.records_read += 1;
            report.repaired += usize::from(outcome.rewritten);
        }

        // An mtime cannot report a deletion — a directory that is gone has no
        // mtime to read — so removal is driven by the readdir, which runs
        // every pass regardless of the watermark.
        for row_kind in kind.row_kinds().iter().copied() {
            for id in self.scope_ids(row_kind, scope)? {
                if !scan.named.contains(&id) && self.remove(row_kind, &id)? {
                    report.removed += 1;
                }
            }
        }

        // Success only. An error above returns before here, which is what
        // makes a failed unit retry its whole span next pass.
        let conn = self.lock();
        write_meta(
            &conn,
            &reconcile_watermark_key(kind, scope),
            &nanos_since_epoch(started_at).to_string(),
        )
        .context("advancing a list index reconciliation watermark")
    }

    /// The unit's watermark in nanoseconds since the epoch.
    ///
    /// Absent means this unit has never been reconciled, so its first pass
    /// reads every record once. A value we cannot parse means the same thing:
    /// a watermark we cannot read is not one we may trust to skip work.
    ///
    /// **Unparseable warns, because falling back to the epoch is invisible
    /// otherwise.** A unit stuck there reads every record on every pass
    /// forever while reporting `repaired: 0` and `removed: 0` — a full walk
    /// that looks exactly like a healthy idle pass to anything watching the
    /// counts, and quieter than one, since a caller that logs only when it
    /// repaired something never says a word about the pass that is degraded.
    fn reconcile_watermark(&self, kind: UnitKind, scope: &ListScope) -> Result<u128> {
        let conn = self.lock();
        let key = reconcile_watermark_key(kind, scope);
        let Some(raw) = read_meta(&conn, &key)? else {
            return Ok(0);
        };
        match raw.parse::<u128>() {
            Ok(watermark) => Ok(watermark),
            Err(error) => {
                warn!(
                    key = %key,
                    value = %raw,
                    error = %error,
                    "[LIST-INDEX] unreadable reconciliation watermark; this unit re-reads every record until the pass rewrites it"
                );
                Ok(0)
            },
        }
    }

    /// Every id the index holds for one kind in one scope.
    fn scope_ids(&self, kind: ListKind, scope: &ListScope) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut statement = conn
            .prepare(
                "SELECT id FROM list_entries
                 WHERE kind = ?1 AND principal = ?2 AND workspace = ?3",
            )
            .context("preparing a scoped list index id read")?;
        let ids = statement
            .query_map(
                params![kind.as_str(), &scope.principal, &scope.workspace],
                |row| row.get::<_, String>(0),
            )
            .context("reading a scope's indexed ids")?
            .collect::<Result<Vec<String>, _>>()
            .context("collecting a scope's indexed ids")?;
        Ok(ids)
    }
}

fn reconcile_watermark_key(kind: UnitKind, scope: &ListScope) -> String {
    format!(
        "{META_RECONCILE_PREFIX}:{}:{}:{}",
        kind.as_str(),
        scope.principal,
        scope.workspace
    )
}

/// What one readdir of a unit root found.
struct UnitScan {
    /// Every task id the root names, so a row the index holds under an id
    /// that is absent here can be recognised as an orphan.
    named: BTreeSet<String>,
    /// The subset worth re-reading: records whose files have moved since
    /// the watermark, and records missing one of those files entirely, which
    /// `file_touched_since` also reports as touched because absence is itself
    /// a change the index has to reflect.
    changed: Vec<String>,
}

/// One readdir, and per entry the stats that decide whether its record is
/// worth re-reading. No record is opened here — that is the whole point.
///
/// **`NotFound` is the only error this interprets.** Everything else fails
/// the unit, because `scan.named` drives the orphan sweep and a scan that
/// guesses is a scan that deletes. Absent means gone; unreadable means
/// unknown, and the two must not collapse into the same empty set.
fn scan_unit_root(root: &Path, watermark: u128) -> Result<UnitScan> {
    let mut scan = UnitScan {
        named: BTreeSet::new(),
        changed: Vec::new(),
    };

    let dir = match std::fs::read_dir(root) {
        Ok(dir) => dir,
        // A scope with no tasks of this kind simply has no directory, exactly
        // as `walk_task_root` treats it.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(scan),
        // EACCES, EMFILE, ENOTDIR, a root that is momentarily unreadable —
        // none of these mean "empty". Read as empty, this unit would delete
        // every `task` and `monitor` row in the scope, count them in
        // `removed`, and then ADVANCE THE WATERMARK past the very files it
        // just orphaned, so the mtime gate would never re-read them and the
        // scope would stay missing until the next full rebuild. Failing the
        // unit costs one interval; guessing costs the scope.
        Err(error) => {
            return Err(error)
                .with_context(|| format!("reading list index unit root {}", root.display()));
        },
    };

    for entry in dir {
        let entry = entry.with_context(|| {
            format!(
                "reading an entry of list index unit root {}",
                root.display()
            )
        })?;
        let path = entry.path();

        // `is_dir()` swallows a failed stat as "not a directory", which would
        // drop the entry from `named` and sweep its rows on an EACCES.
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            // Gone between the readdir and the stat. That IS a deletion, and
            // leaving it unnamed is how a deletion reaches the index.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("reading list index task directory {}", path.display())
                });
            },
        };
        if !metadata.is_dir() {
            continue;
        }
        // A name that is not UTF-8 can never have produced a row — the walk
        // skips it on the same test — so there is nothing to sweep.
        let Some(task_id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };

        if record_touched_since(&path, watermark)? {
            scan.changed.push(task_id.clone());
        }
        scan.named.insert(task_id);
    }

    Ok(scan)
}

/// Whether either file `read_task_entry` reads has moved since `watermark`.
///
/// **The record FILES, not the directory that holds them.**
/// `write_bytes_atomic` renames its temp file into the target's own
/// directory, so a rename moves that file's IMMEDIATE parent — which for
/// `state/task_state.json` is `<task>/state/`, leaving `<task>/` untouched.
/// `status` and `updated_at` come only from `task_state.json` and are the two
/// columns that drift most, so a gate on the task directory would have been
/// blind to exactly the change this pass exists to repair.
///
/// Stat'ing the files is also one assumption lighter: the directory reading
/// is sound only while every writer renames, and a writer that truncated
/// `task_state.json` in place would move the file's mtime but not its
/// parent's. Same two stats either way.
fn record_touched_since(task_dir: &Path, watermark: u128) -> Result<bool> {
    for path in [manifest_path(task_dir), task_state_path(task_dir)] {
        if file_touched_since(&path, watermark)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether one record file is worth a read, and the one place that decides
/// what an unreadable record means.
///
/// **Absent and unreadable are opposite answers, deliberately.** Both used to
/// return `true` here, which routed both into `index_task_from_disk` — and
/// its unreadable-record path builds an empty candidate, so it DELETES the
/// task's rows and reports the deletion as `repaired`. Conservative in the
/// read direction, destructive in the write direction.
///
/// - **Absent** is a state change. `read_task_entry` cannot build a row
///   without this file, so a record missing one genuinely has no rows and the
///   rebuild would index none either. Re-reading is how a row left behind is
///   removed, and it is free of consequence when there is no row: candidate
///   and current are both empty, so `index_task_from_disk` declines to write.
/// - **Unreadable** is not a state change and is not evidence of one. "I
///   could not read it" is not "it is gone". Skipping it silently would be
///   just as bad as deleting, because the watermark would then advance past a
///   record that was never checked and the gate would never look again.
///   Failing the unit does neither: nothing is written, and the next pass
///   retries the whole span.
fn file_touched_since(path: &Path, watermark: u128) -> Result<bool> {
    match std::fs::metadata(path).and_then(|metadata| metadata.modified()) {
        // `>=` rather than `>`: a file written in the same tick as the pass
        // start would otherwise be skipped forever — every later watermark is
        // larger still — and the cost of the tie is one redundant read.
        // Pinned by `the_gate_re_reads_a_file_written_in_the_watermarks_own_tick`.
        Ok(modified) => Ok(nanos_since_epoch(modified) >= watermark),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error)
            .with_context(|| format!("stat'ing list index record file {}", path.display())),
    }
}

/// Nanoseconds rather than milliseconds, so a watermark is never truncated
/// toward the epoch: a millisecond mark would re-read every record written in
/// the same millisecond as the pass that set it, on every later pass.
///
/// **Not pinned by any test.** A sub-millisecond tie is not reachable
/// deterministically from one, and `an_idle_pass_reads_no_records` would only
/// catch a coarser watermark when its fixtures happened to land in the same
/// millisecond as the first pass — timing, not a guard. The reasoning is what
/// holds this. (The `>=` in `file_touched_since` IS pinned; the two choices
/// are often discussed together and only one of them is covered.)
///
/// A time before the epoch reads as 0, which re-reads rather than skips.
fn nanos_since_epoch(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos())
}

// ---------------------------------------------------------------------------
// Reading — a keyset page, a total, and six counts
// ---------------------------------------------------------------------------

/// The keyset position a page resumes from.
///
/// The wire form is `"{updated_at}:{url-encoded id}"` — byte-for-byte what
/// attention's cursor already carries, which is why moving that surface onto
/// the index changes no contract. `updated_at` is epoch milliseconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListCursor {
    pub updated_at: i64,
    pub id: String,
}

impl ListCursor {
    pub fn new(updated_at: i64, id: impl Into<String>) -> Self {
        Self {
            updated_at,
            id: id.into(),
        }
    }

    pub fn encode(&self) -> String {
        format!("{}:{}", self.updated_at, urlencoding::encode(&self.id))
    }

    pub fn decode(raw: &str) -> Result<ListCursor> {
        let (updated_at, encoded_id) = raw
            .split_once(':')
            .ok_or_else(|| anyhow!("invalid list cursor"))?;
        let updated_at = updated_at
            .parse::<i64>()
            .context("invalid list cursor timestamp")?;
        let id = urlencoding::decode(encoded_id)
            .context("invalid list cursor id")?
            .into_owned();
        if id.trim().is_empty() {
            return Err(anyhow!("invalid list cursor id"));
        }
        Ok(ListCursor { updated_at, id })
    }
}

/// How same-instant rows break their tie.
///
/// `updated_at` descending orders every list here; the surfaces disagree
/// about what comes next when two rows share a millisecond, and BOTH answers
/// are already on the wire under live cursors. This enum is that
/// disagreement written down once, so a query, its `COUNT(*)` and the Rust
/// predicate a fallback walk uses cannot each pick a different one.
///
/// Do not "unify" these by making everything descending. It reorders
/// same-instant rows on `/monitors` for every reader mid-page, and the
/// three-platform monitors fixture pins the ascending order it would change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListTieBreak {
    /// `id DESC` — tasks, internal tasks, and attention's cursor.
    #[default]
    IdDescending,
    /// `id ASC` — what `/monitors` has sorted and paged by since it shipped.
    IdAscending,
}

impl ListTieBreak {
    /// The `ORDER BY` this tiebreak produces, whole, so the clause and the
    /// keyset `WHERE` below can never be written for different orders.
    fn order_by(self) -> &'static str {
        match self {
            ListTieBreak::IdDescending => " ORDER BY updated_at DESC, id DESC",
            ListTieBreak::IdAscending => " ORDER BY updated_at DESC, id ASC",
        }
    }

    /// The `WHERE` fragment selecting rows strictly after a cursor, with
    /// placeholders for `updated_at`, `updated_at`, `id` in that order.
    fn follows_clause(self) -> &'static str {
        match self {
            ListTieBreak::IdDescending => " AND (updated_at < ? OR (updated_at = ? AND id < ?))",
            ListTieBreak::IdAscending => " AND (updated_at < ? OR (updated_at = ? AND id > ?))",
        }
    }

    /// The exact complement of `follows_clause` — rows at or before the
    /// cursor — with the same placeholder order, so counting them gives the
    /// reader's true position in the corpus.
    ///
    /// Written as the negation rather than derived from it because every
    /// column here is `NOT NULL`; if that ever stops being true, `NOT (…)`
    /// and this clause stop agreeing on the null rows and the two sides of
    /// the page silently disagree about where the reader is.
    fn precedes_clause(self) -> &'static str {
        match self {
            ListTieBreak::IdDescending => " AND (updated_at > ? OR (updated_at = ? AND id >= ?))",
            ListTieBreak::IdAscending => " AND (updated_at > ? OR (updated_at = ? AND id <= ?))",
        }
    }
}

/// The keyset predicate `page` puts in its `WHERE` clause, in Rust.
///
/// True when a row at `(updated_at, id)` sorts strictly AFTER `cursor` under
/// `updated_at DESC` plus `tie_break` — which is exactly the set of rows the
/// next page may contain.
///
/// It lives here, beside the SQL, because a list handler must honour a cursor
/// while the index is rebuilding and would otherwise invent a second
/// definition of "after". Two definitions is how a page comes to skip or
/// repeat a row depending on which path served it, and neither page would
/// look wrong on its own. `keyset_predicate_matches_the_page_sql` runs both
/// over one corpus and asserts identical ids, for both tiebreaks.
pub fn row_follows_cursor_with(
    tie_break: ListTieBreak,
    cursor: &ListCursor,
    updated_at: i64,
    id: &str,
) -> bool {
    if updated_at != cursor.updated_at {
        return updated_at < cursor.updated_at;
    }
    match tie_break {
        ListTieBreak::IdDescending => id < cursor.id.as_str(),
        ListTieBreak::IdAscending => id > cursor.id.as_str(),
    }
}

/// [`row_follows_cursor_with`] under the default descending tiebreak — the
/// order tasks, internal tasks and attention all page in.
pub fn row_follows_cursor(cursor: &ListCursor, updated_at: i64, id: &str) -> bool {
    row_follows_cursor_with(ListTieBreak::IdDescending, cursor, updated_at, id)
}

/// One page request. A struct rather than eight positional arguments, and
/// because a page is a single coherent question: this lane, of this kind, in
/// this scope, from here.
#[derive(Debug, Clone)]
pub struct ListPageQuery {
    pub kind: ListKind,
    pub scope: ListScope,
    /// `None` is the whole pool — no lane filter at all.
    pub lane: Option<TaskLane>,
    /// The READER's local date, `YYYY-MM-DD`. The server cannot know the
    /// reader's timezone, so the two date lanes take it explicitly rather
    /// than inventing one from a UTC clock — same rule as `TaskLane::matches`.
    pub today: String,
    /// When set, the page is a keyset seek from this position and `offset` is
    /// ignored. When absent, the page is an `OFFSET`.
    pub cursor: Option<ListCursor>,
    pub limit: usize,
    pub offset: usize,
    /// Which of the two keysets this page walks. Defaults to the descending
    /// tiebreak every task surface uses; `/monitors` asks for the ascending
    /// one because that is the order its cursors were minted under.
    pub tie_break: ListTieBreak,
}

impl ListPageQuery {
    pub fn new(kind: ListKind, scope: ListScope, limit: usize) -> Self {
        Self {
            kind,
            scope,
            lane: None,
            today: String::new(),
            cursor: None,
            limit,
            offset: 0,
            tie_break: ListTieBreak::IdDescending,
        }
    }

    pub fn tie_break(mut self, tie_break: ListTieBreak) -> Self {
        self.tie_break = tie_break;
        self
    }

    pub fn lane(mut self, lane: TaskLane, today: impl Into<String>) -> Self {
        self.lane = Some(lane);
        self.today = today.into();
        self
    }

    pub fn cursor(mut self, cursor: ListCursor) -> Self {
        self.cursor = Some(cursor);
        self
    }

    pub fn offset(mut self, offset: usize) -> Self {
        self.offset = offset;
        self
    }
}

/// One page, plus everything the envelope needs beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListPage {
    pub items: Vec<ListEntry>,
    /// The lane's total over the whole scoped pool, not the page.
    pub total: usize,
    pub limit: usize,
    pub offset: usize,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

const ENTRY_COLUMNS: &str =
    "id, updated_at, created_at, status, agent_id, lifecycle, due_date, tags, title, paused";

/// The SQL for one lane, with the values its placeholders bind to, returned
/// **together** so the two can never drift out of step.
///
/// These must be the predicates `TaskLane::matches` already defines. Two
/// definitions of `overdue` is how the index and the file walk come to
/// disagree, and the disagreement would be invisible — so the match here is
/// exhaustive (a new lane fails to compile until it has SQL) and
/// `every_lane_agrees_with_task_lanes_over_one_fixture` runs both sides over
/// one corpus and asserts identical ids, lane by lane.
///
/// Two details that look like noise and are not:
///
/// - `due_date <> ''` in `overdue`. An empty string sorts before every real
///   date, so without the guard every untouched row reads as overdue. It is
///   the `filter(|due| !due.is_empty())` in `TaskLane::Overdue`.
/// - `substr(due_date, 1, length(?)) = ?` in `today`, not `LIKE`. It is
///   `starts_with`, so a stored full timestamp still counts as due today, and
///   it cannot be knocked sideways by a wildcard in the date.
///
/// Every comparison here relies on SQLite's default BINARY collation, which
/// is byte-wise exactly as Rust's `&str` ordering is. No column may ever be
/// declared `COLLATE NOCASE`.
fn lane_clause(lane: TaskLane, today: &str) -> (&'static str, Vec<Value>) {
    let today_value = || Value::Text(today.to_string());
    match lane {
        TaskLane::All => ("status <> 'completed'", Vec::new()),
        // `tags` is written as a JSON array by one code path, so an empty tag
        // list is always the literal `[]`.
        TaskLane::Inbox => ("tags = '[]' AND status = 'pending'", Vec::new()),
        TaskLane::Today => (
            "due_date IS NOT NULL AND substr(due_date, 1, length(?)) = ?",
            vec![today_value(), today_value()],
        ),
        TaskLane::Overdue => (
            "due_date IS NOT NULL AND due_date <> '' AND due_date < ? \
             AND status <> 'completed'",
            vec![today_value()],
        ),
        TaskLane::Running => ("status IN ('running', 'paused')", Vec::new()),
        TaskLane::Completed => ("status = 'completed'", Vec::new()),
    }
}

/// Scope predicate and its binds. Every list filters by principal/workspace,
/// so this is the head of every statement and of every index.
fn scope_clause(kind: ListKind, scope: &ListScope) -> (&'static str, Vec<Value>) {
    (
        "kind = ? AND principal = ? AND workspace = ?",
        vec![
            Value::Text(kind.as_str().to_string()),
            Value::Text(scope.principal.clone()),
            Value::Text(scope.workspace.clone()),
        ],
    )
}

impl ListIndex {
    /// One page: a keyset seek when a cursor is given, an `OFFSET` when a page
    /// number is. Both are served by `list_keyset`.
    ///
    /// Ordering is `updated_at DESC` plus the query's tiebreak, and each of
    /// the two tiebreaks has its own covering index (`list_keyset`,
    /// `list_keyset_id_asc`) so neither order pays for a temporary sort.
    /// The descending one is the sort key and tiebreaker attention's cursor
    /// already uses, which is why its wire contract did not change when its
    /// implementation did.
    ///
    /// `total` is the lane's total over the whole scoped pool, not the page —
    /// a pager needs the corpus size, and a total that shrank to the page
    /// would report one page of results however many there are.
    pub fn page(&self, query: &ListPageQuery) -> Result<ListPage> {
        let (scope_sql, scope_binds) = scope_clause(query.kind, &query.scope);
        let mut filter = scope_sql.to_string();
        let mut filter_binds = scope_binds;

        if let Some(lane) = query.lane {
            let (lane_sql, lane_binds) = lane_clause(lane, &query.today);
            filter.push_str(" AND (");
            filter.push_str(lane_sql);
            filter.push(')');
            filter_binds.extend(lane_binds);
        }

        let mut sql = format!("SELECT {ENTRY_COLUMNS} FROM list_entries WHERE {filter}");
        let mut binds = filter_binds.clone();

        // The SQL `OFFSET` and the offset the envelope REPORTS are two
        // different numbers wearing one name, and conflating them is how a
        // pager comes to render page two's rows while showing page one as
        // current. A keyset seek must start at SQL offset 0 — it has already
        // skipped by cursor — but the reader is genuinely some rows into the
        // corpus, and only a count can say how many. `/monitors` resolves it
        // the same way; tasks reported the request's 0 until a test compared
        // an indexed page against the walk's and found the walk right.
        let sql_offset = if query.cursor.is_some() {
            0
        } else {
            query.offset
        };
        if let Some(cursor) = query.cursor.as_ref() {
            sql.push_str(query.tie_break.follows_clause());
            binds.push(Value::Integer(cursor.updated_at));
            binds.push(Value::Integer(cursor.updated_at));
            binds.push(Value::Text(cursor.id.clone()));
        }

        sql.push_str(query.tie_break.order_by());
        sql.push_str(" LIMIT ? OFFSET ?");
        // One row past the page, so `has_more` is an observation rather than
        // arithmetic against a total that a concurrent write may have moved.
        binds.push(Value::Integer(query.limit.saturating_add(1) as i64));
        binds.push(Value::Integer(sql_offset as i64));

        let mut items = {
            let conn = self.lock();
            let mut statement = conn.prepare(&sql).context("preparing a list index page")?;
            let rows = statement
                .query_map(rusqlite::params_from_iter(binds), |row| {
                    row_to_entry(row, query.kind, &query.scope)
                })
                .context("reading a list index page")?;
            rows.collect::<rusqlite::Result<Vec<ListEntry>>>()
                .context("collecting a list index page")?
        };

        let has_more = items.len() > query.limit;
        items.truncate(query.limit);
        let next_cursor = has_more.then(|| {
            items
                .last()
                .map(|entry| ListCursor::new(entry.updated_at, entry.id.clone()).encode())
        });

        // Counted AFTER the page is read rather than folded into it: the seek
        // and the count answer different questions, and a cursor that names a
        // row which has since been deleted must still yield the position the
        // reader is at, not an error.
        let offset = match query.cursor.as_ref() {
            Some(cursor) => {
                let mut count_binds = filter_binds;
                count_binds.push(Value::Integer(cursor.updated_at));
                count_binds.push(Value::Integer(cursor.updated_at));
                count_binds.push(Value::Text(cursor.id.clone()));
                let counted: i64 = self
                    .lock()
                    .query_row(
                        &format!(
                            "SELECT COUNT(*) FROM list_entries WHERE {filter}{}",
                            query.tie_break.precedes_clause()
                        ),
                        rusqlite::params_from_iter(count_binds),
                        |row| row.get(0),
                    )
                    .context("counting the rows a cursor has already paged past")?;
                counted as usize
            },
            None => query.offset,
        };

        Ok(ListPage {
            total: self.total(query.kind, &query.scope, query.lane, &query.today)?,
            items,
            limit: query.limit,
            offset,
            next_cursor: next_cursor.flatten(),
            has_more,
        })
    }

    /// One `COUNT(*)` over the lane, for the envelope's `total`.
    pub fn total(
        &self,
        kind: ListKind,
        scope: &ListScope,
        lane: Option<TaskLane>,
        today: &str,
    ) -> Result<usize> {
        let mut sql = String::from("SELECT COUNT(*) FROM list_entries WHERE ");
        let (scope_sql, mut binds) = scope_clause(kind, scope);
        sql.push_str(scope_sql);
        if let Some(lane) = lane {
            let (lane_sql, lane_binds) = lane_clause(lane, today);
            sql.push_str(" AND (");
            sql.push_str(lane_sql);
            sql.push(')');
            binds.extend(lane_binds);
        }

        let conn = self.lock();
        let total: i64 = conn
            .query_row(&sql, rusqlite::params_from_iter(binds), |row| row.get(0))
            .context("counting a list index lane")?;
        Ok(total as usize)
    }

    /// All six lane counts in one statement.
    ///
    /// Every lane is present even at zero, exactly as `task_lanes::lane_counts`
    /// guarantees: a missing key makes a client render nothing where it should
    /// render `0`, and "no badge" and "zero" are different claims about the
    /// reader's work. `BTreeMap` for the stable key order that keeps the
    /// payload diffable.
    ///
    /// Counts run over the whole scoped pool, never over a lane already
    /// filtered — counting afterwards would make every badge report the lane
    /// the reader is already looking at.
    pub fn lane_counts(
        &self,
        kind: ListKind,
        scope: &ListScope,
        today: &str,
    ) -> Result<BTreeMap<&'static str, usize>> {
        // Built in one pass so the SQL text and the bind order cannot drift:
        // anonymous placeholders bind by position, and the SELECT list comes
        // before the WHERE clause.
        let mut projections = Vec::new();
        let mut binds = Vec::new();
        for lane in TaskLane::ALL {
            let (lane_sql, lane_binds) = lane_clause(lane, today);
            projections.push(format!(
                "COALESCE(SUM(CASE WHEN {lane_sql} THEN 1 ELSE 0 END), 0)"
            ));
            binds.extend(lane_binds);
        }
        let (scope_sql, scope_binds) = scope_clause(kind, scope);
        binds.extend(scope_binds);

        let sql = format!(
            "SELECT {} FROM list_entries WHERE {scope_sql}",
            projections.join(", ")
        );

        let conn = self.lock();
        let values: Vec<i64> = conn
            .query_row(&sql, rusqlite::params_from_iter(binds), |row| {
                (0..TaskLane::ALL.len())
                    .map(|column| row.get::<_, i64>(column))
                    .collect::<rusqlite::Result<Vec<i64>>>()
            })
            .context("counting list index lanes")?;

        Ok(TaskLane::ALL
            .into_iter()
            .map(TaskLane::wire_name)
            .zip(values.into_iter().map(|value| value as usize))
            .collect())
    }

    /// **Every** row this scope holds for `kind`, unpaged and unordered.
    ///
    /// For the readers that want the scope's *membership* rather than a page
    /// of it: `/today` and `/feed` filter feed rows against "which tasks still
    /// exist", and before this they answered it by walking the corpus and
    /// throwing away everything but the ids — 137 record triples read, 137
    /// `TaskListItemV3`s built (each carrying a cloned `description`, 18KB on
    /// average here), one `HashSet<String>` kept.
    ///
    /// Deliberately **not** [`Self::page`] with a huge limit. `page` adds a
    /// `LIMIT ?/OFFSET ?` and a separate `COUNT(*)` for the envelope's
    /// `total`, and `usize::MAX.saturating_add(1) as i64` is `-1`, which
    /// SQLite reads as "no limit" — correct by accident, and one refactor away
    /// from silently truncating a membership set that the caller then treats
    /// as complete. A reader that wants no page gets no pager.
    ///
    /// Ordering is left to SQLite: a membership set has none, and asking for
    /// one would cost the sort. Callers that need order want [`Self::page`].
    ///
    /// The rows carry `status` and `lifecycle` because the second caller
    /// filters on them — an internal task that is already terminal can no
    /// longer accept a HITL submission, so its prompt is dropped rather than
    /// stranded. Reading the two columns the filter needs out of a row it is
    /// already reading is free; going back to disk for them is the cost this
    /// removes.
    pub fn scope_entries(&self, kind: ListKind, scope: &ListScope) -> Result<Vec<ListEntry>> {
        let (scope_sql, binds) = scope_clause(kind, scope);
        let sql = format!("SELECT {ENTRY_COLUMNS} FROM list_entries WHERE {scope_sql}");
        let conn = self.lock();
        let mut statement = conn
            .prepare(&sql)
            .context("preparing a list index scope membership read")?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(binds), |row| {
                row_to_entry(row, kind, scope)
            })
            .context("reading a list index scope's membership")?;
        rows.collect::<rusqlite::Result<Vec<ListEntry>>>()
            .context("collecting a list index scope's membership")
    }
}

/// One `/monitors` page request.
///
/// `/monitors` gets its own query rather than more optional fields on
/// [`ListPageQuery`], because three things about it are genuinely different
/// and all three are contract: it filters on `paused`, it orders with the
/// ASCENDING tiebreak, and **its cursor is a bare `task_id`** rather than
/// `{updated_at}:{id}`. That last one is why this cannot be a
/// `ListPageQuery` with a different tiebreak — the seek has to look the
/// cursor's row up before it has a position to seek from.
#[derive(Debug, Clone)]
pub struct MonitorPageQuery {
    pub scope: ListScope,
    /// `Some(true)` = only paused, `Some(false)` = only active, `None` = both.
    /// The `state=` filter, which is why `paused` is a column.
    pub paused: Option<bool>,
    /// The previous page's last `task_id`, with its wire prefix already
    /// stripped.
    pub cursor_task_id: Option<String>,
    pub limit: usize,
}

/// One `/monitors` page: the ids, and the two numbers a pager needs beside
/// them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorPage {
    /// Task ids in `updated_at DESC, task_id ASC`, at most `limit` of them.
    pub ids: Vec<String>,
    /// Monitors in the scope after the `paused` filter — the size of the
    /// corpus being paged, never the size of the page.
    pub total: usize,
    /// Where this page starts in that corpus. It is the position the cursor
    /// RESOLVED to, not a number the caller sent, and a stale cursor
    /// resolves to `total` — an empty last page rather than an error.
    pub offset: usize,
}

impl ListIndex {
    /// Serve one `/monitors` page as a seek instead of a scan of the pool.
    ///
    /// The ordering, the tiebreak and the stale-cursor behaviour are the
    /// ones that surface already had: `updated_at` descending, `task_id`
    /// ascending inside a tie, and a cursor whose row is no longer in the
    /// filtered corpus ends pagination with an empty page. None of the three
    /// is negotiable — every one of them is already on the wire under live
    /// cursors and pinned by the three-platform list fixture.
    pub fn monitor_page(&self, query: &MonitorPageQuery) -> Result<MonitorPage> {
        let (scope_sql, scope_binds) = scope_clause(ListKind::Monitor, &query.scope);
        let filter = match query.paused {
            Some(_) => format!("{scope_sql} AND paused = ?"),
            None => scope_sql.to_string(),
        };
        // Rebuilt per statement rather than shared: anonymous placeholders
        // bind by position, so each statement needs its own run of values in
        // its own order.
        let filter_binds = || {
            let mut binds = scope_binds.clone();
            if let Some(paused) = query.paused {
                binds.push(Value::Integer(paused as i64));
            }
            binds
        };

        let conn = self.lock();

        let total: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM list_entries WHERE {filter}"),
                rusqlite::params_from_iter(filter_binds()),
                |row| row.get(0),
            )
            .context("counting the monitor corpus")?;
        let total = total as usize;

        // A monitor cursor names its row by id ALONE, so the keyset needs
        // that row's instant before it can compare anything.
        //
        // Looked up under the SAME filter the page uses, not by id alone: a
        // cursor whose monitor has since been paused is stale for
        // `state=active`, and stale must end pagination — which is what the
        // scan's `position(…) -> None` did. Resolving it against the
        // unfiltered table would instead resume in the middle of a list the
        // reader is not looking at.
        let cursor = match query.cursor_task_id.as_deref() {
            None => None,
            Some(task_id) => {
                let mut binds = filter_binds();
                binds.push(Value::Text(task_id.to_string()));
                let found: Option<i64> = conn
                    .query_row(
                        &format!("SELECT updated_at FROM list_entries WHERE {filter} AND id = ?"),
                        rusqlite::params_from_iter(binds),
                        |row| row.get(0),
                    )
                    .optional()
                    .context("locating a monitor cursor row")?;
                match found {
                    Some(updated_at) => Some(ListCursor::new(updated_at, task_id.to_string())),
                    None => {
                        return Ok(MonitorPage {
                            ids: Vec::new(),
                            total,
                            offset: total,
                        });
                    },
                }
            },
        };

        let mut sql = format!("SELECT id FROM list_entries WHERE {filter}");
        let mut binds = filter_binds();
        let mut offset = 0usize;

        if let Some(cursor) = cursor.as_ref() {
            // Rows at or before the cursor's row are the ones the reader has
            // already been handed, so their count IS this page's offset.
            // Counted in the index rather than derived from the page, because
            // a reader who arrived on a cursor never sent an offset and a
            // pager still has to say which page they are on.
            let mut position_binds = filter_binds();
            position_binds.push(Value::Integer(cursor.updated_at));
            position_binds.push(Value::Integer(cursor.updated_at));
            position_binds.push(Value::Text(cursor.id.clone()));
            let seen: i64 = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM list_entries WHERE {filter}{precedes}",
                        precedes = ListTieBreak::IdAscending.precedes_clause()
                    ),
                    rusqlite::params_from_iter(position_binds),
                    |row| row.get(0),
                )
                .context("resolving a monitor cursor's position")?;
            offset = (seen as usize).min(total);

            sql.push_str(ListTieBreak::IdAscending.follows_clause());
            binds.push(Value::Integer(cursor.updated_at));
            binds.push(Value::Integer(cursor.updated_at));
            binds.push(Value::Text(cursor.id.clone()));
        }

        sql.push_str(ListTieBreak::IdAscending.order_by());
        sql.push_str(" LIMIT ?");
        binds.push(Value::Integer(query.limit as i64));

        let ids = {
            let mut statement = conn.prepare(&sql).context("preparing a monitor page")?;
            let rows = statement
                .query_map(rusqlite::params_from_iter(binds), |row| {
                    row.get::<_, String>(0)
                })
                .context("reading a monitor page")?;
            rows.collect::<rusqlite::Result<Vec<String>>>()
                .context("collecting a monitor page")?
        };

        Ok(MonitorPage { ids, total, offset })
    }
}

fn row_to_entry(
    row: &rusqlite::Row<'_>,
    kind: ListKind,
    scope: &ListScope,
) -> rusqlite::Result<ListEntry> {
    let tags: String = row.get(7)?;
    Ok(ListEntry {
        kind,
        id: row.get(0)?,
        scope: scope.clone(),
        updated_at: row.get(1)?,
        created_at: row.get(2)?,
        status: row.get(3)?,
        agent_id: row.get(4)?,
        lifecycle: row.get(5)?,
        due_date: row.get(6)?,
        // A row this module wrote always holds a JSON array. An empty vec for
        // anything else keeps a single damaged row from failing the page —
        // it is a cache, and the record itself is still on disk.
        tags: serde_json::from_str(&tags).unwrap_or_default(),
        title: row.get(8)?,
        paused: row.get::<_, i64>(9)? != 0,
    })
}

/// The thirteen columns `upsert_in` writes, in the types SQLite stores them as.
///
/// Compared rather than the `ListEntry`s themselves so the check is against
/// what is literally in the table — the tags JSON exactly as encoded, `paused`
/// as the integer it is stored as — and so it needs no `kind` parse that could
/// fail on a row some future schema wrote. A mismatch there must read as
/// "different", and a comparison that cannot fail is how that stays true.
/// A struct rather than the 13-tuple this used to be.
///
/// The standard library implements `Ord`/`PartialEq` for tuples only up to
/// twelve elements, so the thirteenth field made `candidate.sort()` and the
/// `==` against the stored rows fail to compile. Field order is unchanged, and
/// a derived `Ord` compares fields in declaration order — so this is the same
/// comparison the tuple was asking for, not a new one. Adding a field here is
/// now free; adding a fourteenth tuple element would not have been.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct IndexedColumns {
    id: String,
    kind: String,
    principal: String,
    workspace: String,
    updated_at: i64,
    created_at: i64,
    status: String,
    agent_id: String,
    lifecycle: Option<String>,
    due_date: Option<String>,
    tags: String,
    title: Option<String>,
    paused: i64,
}

fn entry_columns(entry: &ListEntry) -> IndexedColumns {
    IndexedColumns {
        id: entry.id.clone(),
        kind: entry.kind.as_str().to_string(),
        principal: entry.scope.principal.clone(),
        workspace: entry.scope.workspace.clone(),
        updated_at: entry.updated_at,
        created_at: entry.created_at,
        status: entry.status.clone(),
        agent_id: entry.agent_id.clone(),
        lifecycle: entry.lifecycle.clone(),
        due_date: entry.due_date.clone(),
        tags: entry.tags_json(),
        title: entry.title.clone(),
        paused: entry.paused as i64,
    }
}

/// Every row a task currently owns, sorted so it can be compared directly with
/// the sorted candidate set a reindex just read off disk.
///
/// Takes the connection rather than `self` so the caller can pass its open
/// transaction: the comparison is only sound if it sees the rows the write it
/// replaces would have replaced.
fn task_columns_in(conn: &Connection, task_id: &str) -> Result<Vec<IndexedColumns>> {
    let mut statement = conn
        .prepare(
            "SELECT id, kind, principal, workspace, updated_at, created_at,
                    status, agent_id, lifecycle, due_date, tags, title, paused
               FROM list_entries
              WHERE id = ?1 AND kind IN ('task', 'internal', 'monitor')",
        )
        .context("preparing a task's current index rows")?;
    let rows = statement
        .query_map(params![task_id], |row| {
            Ok(IndexedColumns {
                id: row.get(0)?,
                kind: row.get(1)?,
                principal: row.get(2)?,
                workspace: row.get(3)?,
                updated_at: row.get(4)?,
                created_at: row.get(5)?,
                status: row.get(6)?,
                agent_id: row.get(7)?,
                lifecycle: row.get(8)?,
                due_date: row.get(9)?,
                tags: row.get(10)?,
                title: row.get(11)?,
                paused: row.get(12)?,
            })
        })
        .context("reading a task's current index rows")?;
    let mut columns = rows
        .collect::<rusqlite::Result<Vec<IndexedColumns>>>()
        .context("collecting a task's current index rows")?;
    columns.sort();
    Ok(columns)
}

fn upsert_in(conn: &Connection, entry: &ListEntry) -> Result<()> {
    conn.execute(
        "INSERT INTO list_entries
            (id, kind, principal, workspace, updated_at, created_at,
             status, agent_id, lifecycle, due_date, tags, title, paused)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(kind, id) DO UPDATE SET
             principal  = excluded.principal,
             workspace  = excluded.workspace,
             updated_at = excluded.updated_at,
             created_at = excluded.created_at,
             status     = excluded.status,
             agent_id   = excluded.agent_id,
             lifecycle  = excluded.lifecycle,
             due_date   = excluded.due_date,
             tags       = excluded.tags,
             title      = excluded.title,
             paused     = excluded.paused",
        params![
            &entry.id,
            entry.kind.as_str(),
            &entry.scope.principal,
            &entry.scope.workspace,
            entry.updated_at,
            entry.created_at,
            &entry.status,
            &entry.agent_id,
            &entry.lifecycle,
            &entry.due_date,
            entry.tags_json(),
            &entry.title,
            entry.paused as i64,
        ],
    )
    .context("writing a list index row")?;
    Ok(())
}

fn tasks_root(scopes_root: &Path, scope: &ListScope) -> PathBuf {
    scopes_root
        .join(&scope.principal)
        .join(&scope.workspace)
        .join("tasks")
}

fn internal_tasks_root(scopes_root: &Path, scope: &ListScope) -> PathBuf {
    scopes_root
        .join(&scope.principal)
        .join(&scope.workspace)
        .join("internal_tasks")
}

/// `<scopes_root>/<principal>/<workspace>`, the same directory-name-is-the-name
/// convention `ArtifactV2Workspace::list_scope_segments` uses. Sorted, so a
/// rebuild visits units in a stable order and its resume points are
/// predictable.
fn enumerate_scopes(scopes_root: &Path) -> Result<Vec<ListScope>> {
    let mut scopes = Vec::new();
    let principals = match std::fs::read_dir(scopes_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(scopes),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("reading scopes root {}", scopes_root.display()));
        },
    };

    for principal_entry in principals.flatten() {
        if !principal_entry.path().is_dir() {
            continue;
        }
        let Some(principal) = principal_entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let workspaces = match std::fs::read_dir(principal_entry.path()) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for workspace_entry in workspaces.flatten() {
            if !workspace_entry.path().is_dir() {
                continue;
            }
            let Some(workspace) = workspace_entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            scopes.push(ListScope::new(principal.clone(), workspace));
        }
    }

    scopes.sort();
    Ok(scopes)
}

/// **`NotFound` is the only error this interprets**, for the same reason
/// `scan_unit_root` draws the line there: `commit_unit` clears the unit's
/// rows before inserting what this collected and then records progress, so a
/// root that read as empty would write an empty scope and mark it done. That
/// does not self-heal — a completed rebuild is also what clears the
/// reconciler's watermarks, and one that wrote an empty scope has no reason
/// to run again. Absent means the scope has no tasks of this kind;
/// unreadable means we do not know, and the two must not share an answer.
///
/// The error is the CALLER's to place: `rebuild_from_disk` skips this unit
/// and keeps walking, because one unreadable directory must cost its own
/// rows and not the corpus. A broken scopes root is different and still
/// aborts everything — that failure is `enumerate_scopes`', and there is no
/// unit to scope it to.
fn walk_task_root(
    scopes_root: &Path,
    root: &Path,
    kind: UnitKind,
    scope: &ListScope,
    entries: &mut Vec<ListEntry>,
    stats: &mut RebuildReport,
) -> Result<()> {
    let legacy_root = tasks_root(scopes_root, scope);
    if kind == UnitKind::Internal && root != legacy_root {
        // Internal owns these rows in both rebuild and orphan reconciliation.
        walk_task_root(scopes_root, &legacy_root, kind, scope, entries, stats)?;
    }
    let dir = match std::fs::read_dir(root) {
        Ok(dir) => dir,
        // A scope with no tasks yet has no `tasks/` directory.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("reading list index unit root {}", root.display()));
        },
    };

    for entry in dir {
        let entry = entry.with_context(|| {
            format!(
                "reading an entry of list index unit root {}",
                root.display()
            )
        })?;
        let path = entry.path();
        // `metadata` rather than `entry.metadata()`, which would not follow
        // symlinks: a symlinked task directory is walked, matching
        // `path.is_dir()`'s reading and `scan_unit_root`'s. Changing to the
        // cheaper call would silently change which tasks are indexed.
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            // Deleted between the readdir and the stat — a task can vanish
            // mid-walk, and that has always been tolerated here.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("reading list index task directory {}", path.display())
                });
            },
        };
        if !metadata.is_dir() {
            continue;
        }
        let Some(task_id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };

        let app_run =
            crate::magician_v2::artifact_v2::service::is_canonical_app_workflow_task_id(&task_id);
        if (kind == UnitKind::Task && app_run)
            || (kind == UnitKind::Internal && root == legacy_root && !app_run)
        {
            continue;
        }
        if root == legacy_root
            && internal_tasks_root(scopes_root, scope)
                .join(&task_id)
                .is_dir()
        {
            // `ArtifactV2Workspace::task_dir` probes `internal_tasks/` first,
            // so a task present under both roots reads as the internal one
            // and the user-visible listing never shows it. Indexing the
            // `tasks/` copy here would surface a task the file walk hides.
            stats.entries_hidden += 1;
            continue;
        }

        read_task_entry(&path, kind.row_kind(), scope, &task_id, entries, stats);
    }

    Ok(())
}

/// Read one task directory into zero, one, or two rows.
///
/// Zero when the record cannot be read (skipped, counted, never fatal) or
/// when the user-visible listing hides it. Two when a user-visible task
/// carries a `monitor_spec`: it is both a task and a monitor, and both
/// surfaces list it.
fn read_task_entry(
    task_dir: &Path,
    kind: ListKind,
    scope: &ListScope,
    task_id: &str,
    entries: &mut Vec<ListEntry>,
    stats: &mut RebuildReport,
) {
    let Some(manifest) = read_json::<TaskManifest>(&manifest_path(task_dir)) else {
        stats.entries_skipped += 1;
        return;
    };
    let Some(state) = read_json::<TaskState>(&task_state_path(task_dir)) else {
        stats.entries_skipped += 1;
        return;
    };

    // App workflow identity is server-owned. Older runs remain in tasks/;
    // classify their projection without relocating immutable run evidence.
    let kind =
        if crate::magician_v2::artifact_v2::service::is_canonical_app_workflow_task_id(task_id) {
            ListKind::Internal
        } else {
            kind
        };
    if kind == ListKind::Task && !task_is_user_visible(&manifest) {
        stats.entries_hidden += 1;
        return;
    }

    // The list item reports `TaskState.updated_at` and
    // `TaskManifest.created_at`. The index sorts on the same two fields, or
    // it would order the page differently from the record it is indexing.
    let updated_at = match epoch_millis_from_rfc3339(&state.updated_at) {
        Some(value) => value,
        None => {
            stats.unparsed_timestamps += 1;
            0
        },
    };
    let created_at = epoch_millis_from_rfc3339(&manifest.created_at).unwrap_or(0);

    let entry = ListEntry {
        kind,
        id: task_id.to_string(),
        scope: scope.clone(),
        updated_at,
        created_at,
        status: state.status.clone(),
        agent_id: manifest.agent_id.clone(),
        lifecycle: Some(lifecycle_str(&manifest.lifecycle).to_string()),
        due_date: manifest.due_date.clone(),
        // Tag NAMES, matching the handler's lane slice.
        tags: manifest.tags.iter().map(|tag| tag.name.clone()).collect(),
        title: Some(manifest.title.clone()),
        // Read through the monitors API's OWN two functions rather than
        // reimplemented here. `/monitors?state=` decides active-vs-paused
        // with these; a second definition in this module would let the
        // index and the handler disagree about which monitors are running,
        // and the disagreement would look like a correct list.
        paused: schedule_is_paused(parsed_schedule(manifest.schedule.as_ref()).as_ref()),
    };

    // A monitor is a user-visible task with a spec, minus the archived ones —
    // the same gate the monitors listing applies.
    if kind == ListKind::Task && manifest.monitor_spec.is_some() && state.status != "archived" {
        entries.push(ListEntry {
            kind: ListKind::Monitor,
            ..entry.clone()
        });
    }
    entries.push(entry);
}

/// The user-visible gate, mirroring `task_is_user_visible` in the artifact
/// service. Held to one definition by
/// `index_hides_exactly_what_the_user_visible_listing_hides`, which walks the
/// four exclusions one at a time — two definitions of "visible" is how the
/// index comes to list a task the file walk does not.
fn task_is_user_visible(manifest: &TaskManifest) -> bool {
    !matches!(manifest.lifecycle, TaskLifecycle::Internal)
        && !crate::magician_v2::artifact_v2::service::is_canonical_app_workflow_task_id(
            &manifest.task_id,
        )
        && !manifest.task_id.starts_with("system:")
        && manifest.agent_id != "__system__"
        && manifest.created_by != "__system__"
}

/// The two files a task record is read from, and therefore the exact two the
/// reconciler's mtime gate stats.
///
/// **They are functions so the coupling runs both ways.** `read_task_entry`
/// decides what a row is built from and `record_touched_since` decides when to
/// rebuild one; naming the paths in both places let a third input be added to
/// the first without the second hearing about it, after which the gate would
/// stop noticing that field changing — a green pass, quietly under-reporting.
/// A new input has to come through here, where both callers see it.
fn manifest_path(task_dir: &Path) -> PathBuf {
    task_dir.join("manifest.json")
}

fn task_state_path(task_dir: &Path) -> PathBuf {
    task_dir.join("state").join("task_state.json")
}

/// `None` for a file that is missing, unreadable, or not the shape we expect.
/// All three mean the same thing to a rebuild: skip this record, keep going.
///
/// **This is where the reconciler's absent/unreadable distinction stops.**
/// `file_touched_since` separates a missing record file from an unreadable
/// one and fails the unit on the second; this collapses missing, unreadable
/// and malformed back into one `None`. So a record that STATS cleanly but
/// will not OPEN still passes the gate, reaches `index_task_from_disk`, and
/// has its rows deleted as though it were gone.
///
/// Left collapsed deliberately, and the tolerance is the rebuild's: a record
/// that cannot be parsed is skipped rather than fatal. A rebuild and a
/// reconcile agree such a record is worth no rows — but **they do not agree
/// how long that decision sticks**, and that is the part worth knowing. A
/// rebuild's skip self-heals: the next rebuild re-reads the record, and since
/// it also clears the watermarks, a reconcile follows it. A reconcile's
/// delete does not: it advances the watermark, so the rows stay gone until
/// the file's mtime moves or someone rebuilds.
///
/// Propagating IO errors out would change the rebuild's contract and the
/// write path's behaviour to buy that back. The narrower guarantee is the
/// honest one: the gate refuses to guess, this does not, and the boundary is
/// here.
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    if suffix.is_empty() {
        return db_path.to_path_buf();
    }
    let mut name = db_path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn read_user_version(conn: &Connection) -> Result<i32> {
    let version = conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i32>(0))?;
    Ok(version)
}

pub(super) fn read_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    let value = conn
        .query_row(
            "SELECT value FROM list_index_meta WHERE key = ?1",
            [key],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .with_context(|| format!("reading list index meta key {key}"))?;
    Ok(value)
}

pub(super) fn write_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO list_index_meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [key, value],
    )
    .with_context(|| format!("writing list index meta key {key}"))?;
    Ok(())
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    use std::io::Write;

    use tempfile::TempDir;

    fn index_path(dir: &TempDir) -> PathBuf {
        dir.path().join(LIST_INDEX_FILE_NAME)
    }

    /// Insert a row through raw SQL. Task 1 owns the lifecycle, not the
    /// writers, so the lifecycle tests need their own way to prove a
    /// database's contents survived (or did not survive) an open.
    fn seed_row(index: &ListIndex, id: &str) {
        let conn = index.lock();
        conn.execute(
            "INSERT INTO list_entries
                (id, kind, principal, workspace, updated_at, created_at, status, agent_id, lifecycle, due_date, tags, title)
             VALUES (?1, 'task', 'alpha', 'prod', 10, 5, 'pending', 'planner', NULL, NULL, '[]', 'seeded')",
            [id],
        )
        .expect("seeding a list index row");
    }

    fn row_ids(index: &ListIndex) -> Vec<String> {
        let conn = index.lock();
        let mut statement = conn
            .prepare("SELECT id FROM list_entries ORDER BY id")
            .expect("preparing row id read");
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))
            .expect("reading row ids")
            .collect::<Result<Vec<_>, _>>()
            .expect("collecting row ids");
        ids
    }

    // -----------------------------------------------------------------
    // Fixture: real `manifest.json` + `state/task_state.json` on disk, so
    // the walk is exercised against the shapes it will actually meet.
    // -----------------------------------------------------------------

    struct TaskFixture {
        id: String,
        title: String,
        status: String,
        created_at: String,
        updated_at: String,
        due_date: Option<String>,
        tags: Vec<String>,
        lifecycle: &'static str,
        agent_id: String,
        created_by: String,
        monitor: bool,
        /// The raw `manifest.schedule` value, exactly as a task record
        /// carries it. Held untyped so the `paused` column is proved against
        /// the JSON the monitors API actually parses, not against a struct
        /// the test built by hand.
        schedule: Option<serde_json::Value>,
    }

    impl TaskFixture {
        fn new(id: &str) -> Self {
            Self {
                id: id.to_string(),
                title: format!("title for {id}"),
                status: "pending".to_string(),
                created_at: "2026-07-30T09:00:00+00:00".to_string(),
                updated_at: "2026-07-30T10:00:00+00:00".to_string(),
                due_date: None,
                tags: Vec::new(),
                lifecycle: "persistent",
                agent_id: "planner".to_string(),
                created_by: "user".to_string(),
                monitor: false,
                schedule: None,
            }
        }

        fn status(mut self, status: &str) -> Self {
            self.status = status.to_string();
            self
        }

        fn updated_at(mut self, updated_at: &str) -> Self {
            self.updated_at = updated_at.to_string();
            self
        }

        fn due_date(mut self, due_date: &str) -> Self {
            self.due_date = Some(due_date.to_string());
            self
        }

        fn tags(mut self, tags: &[&str]) -> Self {
            self.tags = tags.iter().map(|tag| (*tag).to_string()).collect();
            self
        }

        fn lifecycle(mut self, lifecycle: &'static str) -> Self {
            self.lifecycle = lifecycle;
            self
        }

        fn agent_id(mut self, agent_id: &str) -> Self {
            self.agent_id = agent_id.to_string();
            self
        }

        fn created_by(mut self, created_by: &str) -> Self {
            self.created_by = created_by.to_string();
            self
        }

        fn monitor(mut self) -> Self {
            self.monitor = true;
            self
        }

        /// An hourly schedule, paused or not. `paused: false` is written
        /// explicitly rather than omitted, so the fixture exercises the
        /// difference between "the key says false" and "there is no key" —
        /// which `schedule_is_paused` treats identically and a hand-rolled
        /// predicate might not.
        fn scheduled(mut self, paused: bool) -> Self {
            self.schedule = Some(serde_json::json!({
                "kind": { "Interval": { "seconds": 3600, "jitter_seconds": null } },
                "timezone": null,
                "paused": paused,
                "missed_fire_policy": "skip",
                "concurrent_execution_policy": "skip"
            }));
            self
        }

        /// A schedule this build cannot parse. `parsed_schedule` returns
        /// `None` for it, and an unparseable schedule must read as ACTIVE —
        /// the same answer `/monitors` gives — rather than as paused.
        fn unparseable_schedule(mut self) -> Self {
            self.schedule = Some(serde_json::json!("every hour or so"));
            self
        }

        /// The one shape on which the two definitions of "paused" disagree.
        ///
        /// It is an OBJECT carrying `"paused": true`, so a direct
        /// `schedule["paused"]` read calls it paused — but `TaskSchedule.kind`
        /// has no `#[serde(default)]`, so it is not a `TaskSchedule` and
        /// `parsed_schedule` returns `None`, which `schedule_is_paused` reports
        /// as ACTIVE. Every other fixture here agrees under both readings,
        /// which is what would let the drift this module forbids go unnoticed:
        /// the index and `/monitors` would sort one monitor into different
        /// lists, and neither list would look wrong.
        fn paused_but_not_a_task_schedule(mut self) -> Self {
            self.schedule = Some(serde_json::json!({ "paused": true }));
            self
        }
    }

    fn monitor_spec_json() -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "objective": "watch the release notes",
            "query_seeds": ["release notes"],
            "sources": {
                "urls": [],
                "domains": ["example.test"],
                "authenticated_sources": []
            },
            "include_rules": [],
            "exclude_rules": [],
            "match_mode": "balanced",
            "notification_policy": "material_changes",
            "notify_initial_baseline": false
        })
    }

    fn write_task(root: &Path, scope: &ListScope, fixture: &TaskFixture) {
        let task_dir = root.join(&fixture.id);
        std::fs::create_dir_all(task_dir.join("state")).expect("creating a fixture task dir");

        let mut manifest = serde_json::json!({
            "task_id": fixture.id,
            "principal": scope.principal,
            "workspace": scope.workspace,
            "title": fixture.title,
            "description": "",
            "agent_id": fixture.agent_id,
            "created_by": fixture.created_by,
            "lifecycle": fixture.lifecycle,
            "tags": fixture.tags
                .iter()
                .map(|tag| serde_json::json!({ "id": tag, "name": tag }))
                .collect::<Vec<_>>(),
            "created_at": fixture.created_at,
            "updated_at": fixture.updated_at,
        });
        if let Some(due_date) = fixture.due_date.as_deref() {
            manifest["due_date"] = serde_json::json!(due_date);
        }
        if fixture.monitor {
            manifest["monitor_spec"] = monitor_spec_json();
            manifest["monitor_revision"] = serde_json::json!(1);
        }
        if let Some(schedule) = fixture.schedule.as_ref() {
            manifest["schedule"] = schedule.clone();
        }
        std::fs::write(
            task_dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).expect("serializing a fixture manifest"),
        )
        .expect("writing a fixture manifest");

        let state = serde_json::json!({
            "task_id": fixture.id,
            "status": fixture.status,
            "active_root_execution_id": null,
            "latest_root_execution_id": null,
            "last_completed_root_execution_id": null,
            "default_task_agent_output_id": null,
            "primary_user_output_id": null,
            "updated_at": fixture.updated_at,
        });
        std::fs::write(
            task_dir.join("state").join("task_state.json"),
            serde_json::to_string_pretty(&state).expect("serializing fixture task state"),
        )
        .expect("writing fixture task state");
    }

    fn scopes_root(dir: &TempDir) -> PathBuf {
        dir.path().join("scopes")
    }

    fn seed_scope(dir: &TempDir, scope: &ListScope, tasks: &[TaskFixture]) {
        let root = tasks_root(&scopes_root(dir), scope);
        std::fs::create_dir_all(&root).expect("creating a fixture tasks root");
        for fixture in tasks {
            write_task(&root, scope, fixture);
        }
    }

    fn seed_internal_scope(dir: &TempDir, scope: &ListScope, tasks: &[TaskFixture]) {
        let root = internal_tasks_root(&scopes_root(dir), scope);
        std::fs::create_dir_all(&root).expect("creating a fixture internal tasks root");
        for fixture in tasks {
            write_task(&root, scope, fixture);
        }
    }

    /// Ids in the index for one kind, sorted — the shape assertions read best in.
    fn indexed_ids(index: &ListIndex, kind: ListKind) -> Vec<String> {
        let conn = index.lock();
        let mut statement = conn
            .prepare("SELECT id FROM list_entries WHERE kind = ?1 ORDER BY id")
            .expect("preparing kind read");
        statement
            .query_map([kind.as_str()], |row| row.get::<_, String>(0))
            .expect("reading ids for a kind")
            .collect::<Result<Vec<_>, _>>()
            .expect("collecting ids for a kind")
    }

    fn indexed_column(index: &ListIndex, kind: ListKind, id: &str, column: &str) -> String {
        let conn = index.lock();
        conn.query_row(
            &format!("SELECT {column} FROM list_entries WHERE kind = ?1 AND id = ?2"),
            [kind.as_str(), id],
            |row| row.get::<_, Option<String>>(0),
        )
        .expect("reading an indexed column")
        .unwrap_or_default()
    }

    fn indexed_int(index: &ListIndex, kind: ListKind, id: &str, column: &str) -> i64 {
        let conn = index.lock();
        conn.query_row(
            &format!("SELECT {column} FROM list_entries WHERE kind = ?1 AND id = ?2"),
            [kind.as_str(), id],
            |row| row.get::<_, i64>(0),
        )
        .expect("reading an indexed integer column")
    }

    fn alpha() -> ListScope {
        ListScope::new("alpha", "prod")
    }

    fn beta() -> ListScope {
        ListScope::new("beta", "prod")
    }

    #[test]
    fn active_task_agent_lookup_is_scoped_status_aware_and_card_free() {
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening the index");
        let entry =
            |kind: ListKind, id: &str, scope: ListScope, status: &str, agent_id: &str| ListEntry {
                kind,
                id: id.to_string(),
                scope,
                updated_at: 20,
                created_at: 10,
                status: status.to_string(),
                agent_id: agent_id.to_string(),
                lifecycle: None,
                due_date: None,
                tags: Vec::new(),
                title: None,
                paused: false,
            };
        index
            .upsert_many(&[
                entry(ListKind::Task, "running", alpha(), "running", "agent-a"),
                entry(
                    ListKind::Internal,
                    "waiting",
                    alpha(),
                    "waiting_for_user",
                    "agent-b",
                ),
                entry(ListKind::Task, "complete", alpha(), "completed", "agent-c"),
                entry(ListKind::Task, "other-scope", beta(), "running", "agent-d"),
                entry(
                    ListKind::Monitor,
                    "monitor-only",
                    alpha(),
                    "running",
                    "agent-e",
                ),
                entry(ListKind::Task, "planning-a", alpha(), "planning", ""),
                entry(ListKind::Internal, "planning-a", alpha(), "planning", ""),
                entry(ListKind::Internal, "planning-b", alpha(), "planning", ""),
                entry(ListKind::Task, "planning-other", beta(), "planning", ""),
            ])
            .expect("writing active lookup fixtures");

        assert_eq!(
            index.active_task_agent_ids(&alpha()).unwrap(),
            BTreeSet::from(["agent-a".to_string(), "agent-b".to_string()])
        );
        assert_eq!(
            index.planning_task_ids(&alpha()).unwrap(),
            BTreeSet::from(["planning-a".to_string(), "planning-b".to_string()]),
            "the compact catalog is scoped and deduplicates task/internal rows"
        );
    }

    #[test]
    fn a_fresh_open_creates_the_schema_and_stamps_its_version() {
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening a fresh list index");

        assert_eq!(index.opened(), &ListIndexOpen::Created);
        assert_eq!(
            index.schema_version().expect("schema version"),
            LIST_INDEX_SCHEMA_VERSION,
            "a fresh index must carry this build's version, or the next open discards it"
        );
        assert!(
            index_path(&dir).exists(),
            "the database file must exist on disk after open"
        );

        // Every table and index the DDL promises, by name — a partially
        // applied `execute_batch` would still leave `list_entries` behind.
        let names = {
            let conn = index.lock();
            let mut statement = conn
                .prepare(
                    "SELECT name FROM sqlite_master
                     WHERE type IN ('table', 'index') AND name NOT LIKE 'sqlite_%'
                     ORDER BY name",
                )
                .expect("preparing schema read");
            statement
                .query_map([], |row| row.get::<_, String>(0))
                .expect("reading schema")
                .collect::<Result<Vec<String>, _>>()
                .expect("collecting schema")
        };
        assert_eq!(
            names,
            vec![
                "list_active_agent".to_string(),
                "list_due".to_string(),
                "list_entries".to_string(),
                "list_index_meta".to_string(),
                // Two keysets, one per tiebreak. An order without a covering
                // index still answers, in a temporary sort over the whole
                // scoped pool — which is the cost this index exists to remove.
                "list_keyset".to_string(),
                "list_keyset_id_asc".to_string(),
                "list_paused".to_string(),
                "list_rebuild_progress".to_string(),
                "list_status".to_string(),
            ]
        );

        assert!(
            !index.is_ready().expect("readiness"),
            "an empty index has indexed nothing, so no reader may trust it yet"
        );
        assert!(index.opened().needs_rebuild());
    }

    #[test]
    fn reopening_an_existing_index_keeps_its_rows() {
        let dir = TempDir::new().expect("temp dir");
        let path = index_path(&dir);

        let first = ListIndex::open_at(&path).expect("first open");
        seed_row(&first, "task-kept");
        drop(first);

        let second = ListIndex::open_at(&path).expect("second open");
        assert_eq!(
            second.opened(),
            &ListIndexOpen::Reused,
            "an index at the current version must be reused, not recreated"
        );
        assert_eq!(
            row_ids(&second),
            vec!["task-kept".to_string()],
            "a reopen that dropped the row would have silently thrown away the whole index"
        );
    }

    /// A version that is not this build's is discarded in BOTH directions.
    ///
    /// Older is the direction that actually happens — a running install meets
    /// a newer binary, never the reverse — and it is the dangerous one:
    /// version `1` has no `paused` column, so an adopted v1 file would fail
    /// every `monitor_page` on `no such column: paused` and degrade that
    /// surface to the walk permanently and silently. `0` is what an
    /// unstamped file reads as. A test that only forces a HIGHER version
    /// leaves a `version <= LIST_INDEX_SCHEMA_VERSION` comparison passing.
    #[test]
    fn a_different_schema_version_discards_the_index() {
        for found in [
            LIST_INDEX_SCHEMA_VERSION - 1,
            0,
            LIST_INDEX_SCHEMA_VERSION + 7,
        ] {
            let dir = TempDir::new().expect("temp dir");
            let path = index_path(&dir);

            let first = ListIndex::open_at(&path).expect("first open");
            seed_row(&first, "task-stale");
            {
                // Stand in for an index written by another build.
                let conn = first.lock();
                conn.pragma_update(None, "user_version", found)
                    .expect("forcing a foreign schema version");
            }
            drop(first);

            let second = ListIndex::open_at(&path).expect("reopen after a version change");
            assert_eq!(
                second.opened(),
                &ListIndexOpen::Discarded(DiscardReason::SchemaVersionChanged {
                    found,
                    expected: LIST_INDEX_SCHEMA_VERSION,
                }),
                "version {found} must be discarded, not adopted"
            );
            assert!(
                row_ids(&second).is_empty(),
                "rows written under schema version {found} must not survive into this one"
            );
            assert_eq!(
                second.schema_version().expect("schema version"),
                LIST_INDEX_SCHEMA_VERSION
            );
            assert!(!second.is_ready().expect("readiness"));
        }
    }

    #[test]
    fn a_corrupt_file_is_discarded_rather_than_erroring_the_caller() {
        let dir = TempDir::new().expect("temp dir");
        let path = index_path(&dir);

        {
            let mut file = std::fs::File::create(&path).expect("creating a corrupt index file");
            file.write_all(b"this is not a sqlite database, it is a plain sentence")
                .expect("writing corruption");
        }

        let index = ListIndex::open_at(&path).expect(
            "a corrupt cache must be discarded, not propagated — the caller asked for a list, \
             not for a storage error",
        );
        assert!(
            matches!(
                index.opened(),
                ListIndexOpen::Discarded(DiscardReason::Unreadable(_))
            ),
            "expected an Unreadable discard, got {:?}",
            index.opened()
        );
        assert_eq!(
            index.schema_version().expect("schema version"),
            LIST_INDEX_SCHEMA_VERSION
        );

        // The replacement is a working database, not just a deleted file.
        seed_row(&index, "task-after-corruption");
        assert_eq!(row_ids(&index), vec!["task-after-corruption".to_string()]);
    }

    #[test]
    fn a_readable_database_without_our_schema_is_discarded_too() {
        let dir = TempDir::new().expect("temp dir");
        let path = index_path(&dir);

        {
            // Valid SQLite, right user_version, wrong contents — the shape a
            // rename or a stray file would take. The pragma alone would
            // wave this through.
            let conn = Connection::open(&path).expect("creating a foreign database");
            conn.execute_batch("CREATE TABLE something_else (id TEXT PRIMARY KEY)")
                .expect("creating a foreign table");
            conn.pragma_update(None, "user_version", LIST_INDEX_SCHEMA_VERSION)
                .expect("stamping a matching version onto a foreign database");
        }

        let index = ListIndex::open_at(&path).expect("opening over a foreign database");
        assert!(
            matches!(
                index.opened(),
                ListIndexOpen::Discarded(DiscardReason::Unreadable(_))
            ),
            "expected an Unreadable discard, got {:?}",
            index.opened()
        );
        seed_row(&index, "task-after-foreign-schema");
        assert_eq!(
            row_ids(&index),
            vec!["task-after-foreign-schema".to_string()]
        );
    }

    /// A discarded index must not come back out of its write-ahead log.
    ///
    /// The fixture is the whole difficulty. Dropping the connection first —
    /// which this test used to do — CHECKPOINTS the log into the main file and
    /// unlinks both sidecars, so corrupting the main file afterwards destroys
    /// the only copy of the row and leaves nothing to replay. Every assertion
    /// then held trivially of a freshly created database.
    ///
    /// So the live triple is copied aside while the connection is still open
    /// and the rows are still only in the log. What that copy then shows is how
    /// completely a log can carry an index: the main file is empty, page one
    /// included, and an open over it reads the whole database — schema, rows
    /// and `user_version` — out of the log. Corrupting the main file is not
    /// even enough to make such a copy unreadable, which is why the discard
    /// here is triggered by a foreign schema version instead.
    #[test]
    fn discarding_removes_the_write_ahead_sidecars() {
        let dir = TempDir::new().expect("temp dir");
        let live_dir = dir.path().join("live");
        std::fs::create_dir_all(&live_dir).expect("creating the live index dir");
        let path = live_dir.join(LIST_INDEX_FILE_NAME);

        // Held OPEN for the rest of the setup: an uncheckpointed log is the
        // only state in which this test is about anything.
        let first = ListIndex::open_at(&path).expect("first open");
        seed_row(&first, "task-in-the-wal");
        {
            // A foreign schema version, so the copy is DISCARDED on open
            // rather than merely reused. Corruption cannot be what triggers
            // that discard here: the main file is empty and the whole database
            // — page one included — lives in the log, so an open over a
            // corrupt main file reads straight past it and succeeds.
            let conn = first.lock();
            conn.pragma_update(None, "user_version", LIST_INDEX_SCHEMA_VERSION + 7)
                .expect("forcing a foreign schema version");
        }
        assert!(
            sidecar_path(&path, "-wal").exists(),
            "WAL mode should have produced a sidecar for this test to be about anything"
        );

        // The main file ALONE does not carry the row. That is what makes the
        // sidecar load-bearing rather than incidental — without this, a
        // discard that left the log behind could still look correct because
        // the row was in the main file all along.
        let orphan_dir = dir.path().join("main-file-only");
        std::fs::create_dir_all(&orphan_dir).expect("creating the main-file-only dir");
        let orphan = orphan_dir.join(LIST_INDEX_FILE_NAME);
        std::fs::copy(&path, &orphan).expect("copying the main file without its sidecars");
        {
            let conn = Connection::open(&orphan).expect("opening the main file alone");
            // The schema went through the same log the row did, so this file
            // may not carry the table at all yet. "No such table" and "zero
            // rows" are the same answer to the only question being asked.
            let rows = conn
                .query_row("SELECT COUNT(*) FROM list_entries", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap_or(0);
            assert_eq!(
                rows, 0,
                "the seeded row must live in the write-ahead log rather than the main \
                 file, or nothing below is about a replay"
            );
        }

        // A copy of the LIVE triple, exactly as it is on disk right now.
        let stale_dir = dir.path().join("stale");
        std::fs::create_dir_all(&stale_dir).expect("creating the stale index dir");
        let stale = stale_dir.join(LIST_INDEX_FILE_NAME);
        for suffix in ["", "-wal", "-shm"] {
            let source = sidecar_path(&path, suffix);
            if source.exists() {
                std::fs::copy(&source, sidecar_path(&stale, suffix))
                    .expect("copying a live index file");
            }
        }
        drop(first);

        assert!(
            sidecar_path(&stale, "-wal").exists(),
            "the write-ahead log must be beside the copy when the discard runs, or \
             there is nothing for the discard to have to remove"
        );

        let second = ListIndex::open_at(&stale).expect("reopening the copied triple");
        assert_eq!(
            second.opened(),
            &ListIndexOpen::Discarded(DiscardReason::SchemaVersionChanged {
                found: LIST_INDEX_SCHEMA_VERSION + 7,
                expected: LIST_INDEX_SCHEMA_VERSION,
            }),
            "the copy's main file holds no header at all, so reading a foreign version \
             out of it is itself the proof that the open went through the log"
        );
        assert!(
            row_ids(&second).is_empty(),
            "a stale write-ahead log must not replay the discarded index back into existence"
        );
        assert_eq!(
            second.schema_version().expect("schema version"),
            LIST_INDEX_SCHEMA_VERSION,
            "and the version it carries must be this build's, not the one the log held"
        );
    }

    #[test]
    fn the_readiness_marker_survives_a_reopen() {
        let dir = TempDir::new().expect("temp dir");
        let path = index_path(&dir);

        let first = ListIndex::open_at(&path).expect("first open");
        {
            let conn = first.lock();
            write_meta(&conn, META_REBUILD_STATE, REBUILD_STATE_COMPLETE)
                .expect("marking the index ready");
        }
        assert!(first.is_ready().expect("readiness"));
        drop(first);

        let second = ListIndex::open_at(&path).expect("second open");
        assert!(
            second.is_ready().expect("readiness"),
            "readiness is a property of the file, not of the process that opened it"
        );

        // And an interrupted rebuild stays visibly unfinished across a restart.
        {
            let conn = second.lock();
            write_meta(&conn, META_REBUILD_STATE, REBUILD_STATE_IN_PROGRESS)
                .expect("marking a rebuild in flight");
        }
        drop(second);

        let third = ListIndex::open_at(&path).expect("third open");
        assert_eq!(third.opened(), &ListIndexOpen::Reused);
        assert!(
            !third.is_ready().expect("readiness"),
            "a rebuild interrupted by a crash must still read as unfinished after a restart"
        );
    }

    /// **The boot gate.** Every open outcome, against what the caller must do
    /// about it.
    ///
    /// The `Reused` + unfinished row is the one that matters and the one a
    /// `needs_rebuild()` gate gets wrong: that index is at the current schema
    /// version, so it is reused as-is, and its rebuild never finished. Gated
    /// on the open outcome, it is never rebuilt on any later boot — the resume
    /// path is unreachable in production, `is_ready()` is false forever, and
    /// every list quietly serves from the walk the index exists to remove.
    #[test]
    fn a_reused_but_unfinished_index_still_owes_a_rebuild() {
        let dir = TempDir::new().expect("temp dir");
        let path = index_path(&dir);

        let created = ListIndex::open_at(&path).expect("first open");
        assert_eq!(created.opened(), &ListIndexOpen::Created);
        assert!(
            created.rebuild_is_owed().expect("rebuild owed"),
            "a fresh index holds nothing and owes its first walk"
        );

        // A rebuild that started and died. `rebuild_from_disk` writes exactly
        // this marker before it walks anything, and a crash leaves it.
        {
            let conn = created.lock();
            write_meta(&conn, META_REBUILD_STATE, REBUILD_STATE_IN_PROGRESS)
                .expect("marking a rebuild in flight");
        }
        drop(created);

        let restarted = ListIndex::open_at(&path).expect("reopen after a crash mid-rebuild");
        assert_eq!(
            restarted.opened(),
            &ListIndexOpen::Reused,
            "the file is at this build's schema version, so it IS reused — which is \
             precisely why the open outcome cannot be the gate"
        );
        assert!(
            !restarted.opened().needs_rebuild(),
            "and `needs_rebuild()` says no rebuild is needed, which is the bug"
        );
        assert!(
            restarted.rebuild_is_owed().expect("rebuild owed"),
            "an interrupted rebuild must be resumed at the next boot, or it never is"
        );

        // Finished: the marker is the only thing that changed, and the gate
        // closes. Without this, a gate hardwired to `true` would pass above.
        {
            let conn = restarted.lock();
            write_meta(&conn, META_REBUILD_STATE, REBUILD_STATE_COMPLETE)
                .expect("marking the index ready");
        }
        drop(restarted);

        let ready = ListIndex::open_at(&path).expect("reopen after a finished rebuild");
        assert_eq!(ready.opened(), &ListIndexOpen::Reused);
        assert!(
            !ready.rebuild_is_owed().expect("rebuild owed"),
            "a complete index must not be re-walked on every boot"
        );
    }

    /// `--reindex` throws the file away rather than resuming into it.
    ///
    /// Resume trusts any unit a previous run committed, so it cannot repair a
    /// unit whose rows are WRONG — only one that was never walked. A recovery
    /// route built on resume would leave exactly the corruption an operator
    /// ran it to clear.
    #[test]
    fn an_operator_reindex_discards_rows_a_resume_would_have_kept() {
        let dir = TempDir::new().expect("temp dir");
        let base = dir.path().to_path_buf();

        // A finished unit — which a resume trusts and skips — holding a row no
        // walk of the disk would ever produce. Seeded after the unit commits,
        // because committing a unit clears its scope's rows first.
        let first = ListIndex::open_at(&base.join(LIST_INDEX_FILE_NAME)).expect("first open");
        first
            .commit_unit(UnitKind::Task, &alpha(), &[])
            .expect("committing a unit a resume would trust");
        seed_row(&first, "task-that-is-not-on-disk");
        {
            let conn = first.lock();
            write_meta(&conn, META_REBUILD_STATE, REBUILD_STATE_COMPLETE)
                .expect("marking the index ready");
        }
        drop(first);

        let reindexed = ListIndex::open_discarding(&base).expect("discarding on operator request");
        assert_eq!(
            reindexed.opened(),
            &ListIndexOpen::Discarded(DiscardReason::OperatorRequested),
            "an operator's discard must not be logged as a corruption event"
        );
        assert!(
            row_ids(&reindexed).is_empty(),
            "a row no walk would produce is exactly what a reindex exists to remove"
        );
        assert_eq!(
            reindexed.rebuild_progress_units().expect("progress"),
            0,
            "and no resume point may survive, or the walk that follows would skip a unit"
        );
        assert!(
            reindexed.rebuild_is_owed().expect("rebuild owed"),
            "a discarded index holds nothing until the walk repopulates it"
        );

        // The walk that `--reindex` runs next repopulates it from the records.
        seed_scope(&dir, &alpha(), &[TaskFixture::new("task-on-disk")]);
        let report = reindexed
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("rebuilding after the discard");
        assert_eq!(
            report.units_resumed, 0,
            "there is nothing left to resume from, so every unit is walked"
        );
        assert_eq!(
            indexed_ids(&reindexed, ListKind::Task),
            vec!["task-on-disk".to_string()]
        );
        assert!(reindexed.is_ready().expect("readiness"));
    }

    /// A fresh root has no file to discard, and `--reindex` on one is not an
    /// error — it is the ordinary first build, reported as `Created` so the
    /// log does not claim an index was thrown away.
    #[test]
    fn an_operator_reindex_of_a_missing_index_creates_one() {
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_discarding(dir.path()).expect("reindexing a fresh root");

        assert_eq!(index.opened(), &ListIndexOpen::Created);
        assert_eq!(
            index.schema_version().expect("schema version"),
            LIST_INDEX_SCHEMA_VERSION
        );
        assert!(index.rebuild_is_owed().expect("rebuild owed"));
    }

    // -----------------------------------------------------------------
    // Task 2 — populating from disk, and staying current on write
    // -----------------------------------------------------------------

    #[test]
    fn legacy_app_runs_remain_internal_after_rebuild_and_reconcile() {
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("index");
        let app_id = format!("task_app_{}", "a".repeat(64));
        seed_scope(
            &dir,
            &alpha(),
            &[TaskFixture::new(&app_id), TaskFixture::new("ordinary-task")],
        );
        index
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("rebuild");
        assert_eq!(
            indexed_ids(&index, ListKind::Task),
            vec!["ordinary-task".to_string()]
        );
        assert_eq!(
            indexed_ids(&index, ListKind::Internal),
            vec![app_id.clone()]
        );
        for _ in 0..2 {
            let report = index
                .reconcile_from_disk(&scopes_root(&dir))
                .expect("reconcile");
            assert_eq!(report.units_failed, 0);
            assert_eq!(
                indexed_ids(&index, ListKind::Internal),
                vec![app_id.clone()]
            );
        }
        assert!(tasks_root(&scopes_root(&dir), &alpha())
            .join(&app_id)
            .exists());
    }

    #[test]
    fn a_rebuild_indexes_every_record_under_both_roots() {
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening the index");

        seed_scope(
            &dir,
            &alpha(),
            &[
                TaskFixture::new("task-one").tags(&["work"]),
                TaskFixture::new("task-two").status("running"),
                TaskFixture::new("task-monitor").monitor(),
            ],
        );
        seed_internal_scope(
            &dir,
            &alpha(),
            &[TaskFixture::new("internal-one").lifecycle("internal")],
        );
        seed_scope(&dir, &beta(), &[TaskFixture::new("task-beta")]);

        let report = index
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("rebuilding from disk");

        assert_eq!(
            report.units_walked, 4,
            "two scopes times two roots, every one of them walked"
        );
        assert_eq!(report.units_resumed, 0);
        assert_eq!(
            report.entries_skipped, 0,
            "every fixture record is readable"
        );
        assert_eq!(report.entries_hidden, 0);

        assert_eq!(
            indexed_ids(&index, ListKind::Task),
            vec![
                "task-beta".to_string(),
                "task-monitor".to_string(),
                "task-one".to_string(),
                "task-two".to_string(),
            ]
        );
        assert_eq!(
            indexed_ids(&index, ListKind::Internal),
            vec!["internal-one".to_string()]
        );
        assert_eq!(
            indexed_ids(&index, ListKind::Monitor),
            vec!["task-monitor".to_string()],
            "a task carrying a monitor spec lists on both surfaces, so it holds two rows"
        );
        assert_eq!(
            report.entries_indexed, 6,
            "four tasks, one internal task, and the monitor's second row"
        );

        // The columns the lanes read, carried through from the two files.
        assert_eq!(
            indexed_column(&index, ListKind::Task, "task-two", "status"),
            "running"
        );
        assert_eq!(
            indexed_column(&index, ListKind::Task, "task-one", "tags"),
            r#"["work"]"#
        );
        assert_eq!(
            indexed_column(&index, ListKind::Task, "task-two", "tags"),
            "[]",
            "an untagged task must store the literal empty array the inbox predicate tests"
        );
        assert_eq!(
            indexed_column(&index, ListKind::Task, "task-one", "title"),
            "title for task-one"
        );
        assert_eq!(
            indexed_column(&index, ListKind::Internal, "internal-one", "lifecycle"),
            "internal"
        );
        assert_eq!(
            indexed_int(&index, ListKind::Task, "task-one", "updated_at"),
            DateTime::parse_from_rfc3339("2026-07-30T10:00:00+00:00")
                .expect("fixture timestamp")
                .timestamp_millis(),
            "the index sorts on the same instant the list item reports"
        );

        assert!(
            index.is_ready().expect("readiness"),
            "a completed rebuild makes the index trustworthy"
        );
        assert_eq!(
            index.rebuild_progress_units().expect("progress"),
            0,
            "a finished rebuild leaves no resume points behind"
        );
    }

    /// `paused` is the one column `/monitors` needs that a lane predicate
    /// does not, and the reason that surface could not seek before. It must
    /// come out of the manifest's schedule by the SAME two functions the
    /// handler uses, or the index and the handler would disagree about which
    /// monitors are running — and a wrong `state=active` list looks exactly
    /// like a right one.
    #[test]
    fn paused_is_read_from_the_schedule_the_monitors_api_parses() {
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening the index");

        let fixtures = vec![
            TaskFixture::new("mon-active").monitor().scheduled(false),
            TaskFixture::new("mon-paused").monitor().scheduled(true),
            TaskFixture::new("mon-unscheduled").monitor(),
            TaskFixture::new("mon-junk-schedule")
                .monitor()
                .unparseable_schedule(),
            TaskFixture::new("mon-paused-key-without-a-schedule")
                .monitor()
                .paused_but_not_a_task_schedule(),
            TaskFixture::new("plain-paused").scheduled(true),
        ];
        seed_scope(&dir, &alpha(), &fixtures);
        index
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("rebuilding from disk");

        for fixture in &fixtures {
            // The handler's own answer, from the same JSON on disk.
            let expected =
                schedule_is_paused(parsed_schedule(fixture.schedule.as_ref()).as_ref()) as i64;
            assert_eq!(
                indexed_int(&index, ListKind::Task, &fixture.id, "paused"),
                expected,
                "`{}` is indexed as paused={} but `schedule_is_paused` says {}",
                fixture.id,
                1 - expected,
                expected == 1
            );
        }

        // Named individually too, so a `schedule_is_paused` that started
        // returning a constant could not make the loop above pass.
        assert_eq!(
            indexed_int(&index, ListKind::Monitor, "mon-paused", "paused"),
            1
        );
        assert_eq!(
            indexed_int(&index, ListKind::Monitor, "mon-active", "paused"),
            0,
            "`paused: false` written explicitly is active, not paused"
        );
        assert_eq!(
            indexed_int(&index, ListKind::Monitor, "mon-unscheduled", "paused"),
            0,
            "a monitor with no schedule at all is active — `/monitors?state=active` includes it"
        );
        assert_eq!(
            indexed_int(&index, ListKind::Monitor, "mon-junk-schedule", "paused"),
            0,
            "a schedule this build cannot parse must read as active, never as paused: \
             defaulting the other way would hide a running monitor from its own list"
        );
        assert_eq!(
            indexed_int(
                &index,
                ListKind::Monitor,
                "mon-paused-key-without-a-schedule",
                "paused"
            ),
            0,
            "`{{\"paused\": true}}` with no schedule kind is not a `TaskSchedule`, so \
             `/monitors` calls it ACTIVE — the column must be read through \
             `schedule_is_paused(parsed_schedule(…))` and never off the raw JSON key, \
             which is the only fixture here on which the two answers differ"
        );
        assert_eq!(
            indexed_int(&index, ListKind::Task, "plain-paused", "paused"),
            1,
            "the column is a property of the schedule, not of being a monitor"
        );
    }

    /// A write path re-reads the record, so pausing a monitor must move the
    /// column on BOTH of its rows. A monitor whose `task` row said paused and
    /// whose `monitor` row said active would serve two different answers to
    /// two surfaces reading one record.
    #[test]
    fn reindexing_moves_paused_on_every_row_a_record_owns() {
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening the index");
        let root = tasks_root(&scopes_root(&dir), &alpha());

        seed_scope(
            &dir,
            &alpha(),
            &[TaskFixture::new("mon-toggle").monitor().scheduled(false)],
        );
        index
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("rebuilding from disk");
        assert_eq!(
            indexed_int(&index, ListKind::Task, "mon-toggle", "paused"),
            0
        );
        assert_eq!(
            indexed_int(&index, ListKind::Monitor, "mon-toggle", "paused"),
            0
        );

        write_task(
            &root,
            &alpha(),
            &TaskFixture::new("mon-toggle").monitor().scheduled(true),
        );
        assert_eq!(
            index
                .index_task_from_disk(&scopes_root(&dir), &alpha(), "mon-toggle")
                .expect("reindexing after a pause")
                .rows,
            2
        );

        assert_eq!(
            indexed_int(&index, ListKind::Task, "mon-toggle", "paused"),
            1
        );
        assert_eq!(
            indexed_int(&index, ListKind::Monitor, "mon-toggle", "paused"),
            1,
            "the monitor row is the one `/monitors?state=paused` reads; it must move too"
        );
    }

    #[test]
    fn a_rebuild_hides_exactly_what_the_user_visible_listing_hides() {
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening the index");

        seed_scope(
            &dir,
            &alpha(),
            &[
                TaskFixture::new("task-visible"),
                // The four exclusions, one per row, so a dropped clause fails
                // on exactly the row it stopped excluding.
                TaskFixture::new("task-internal-lifecycle").lifecycle("internal"),
                TaskFixture::new("system:seed"),
                TaskFixture::new("task-system-agent").agent_id("__system__"),
                TaskFixture::new("task-system-author").created_by("__system__"),
            ],
        );

        let report = index
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("rebuilding from disk");

        assert_eq!(
            indexed_ids(&index, ListKind::Task),
            vec!["task-visible".to_string()],
            "the index must list exactly what the user-visible walk lists — no more"
        );
        assert_eq!(
            report.entries_hidden, 4,
            "each exclusion must be counted, not silently dropped"
        );
        assert_eq!(
            report.entries_skipped, 0,
            "hidden is not the same as unreadable"
        );
    }

    #[test]
    fn a_task_present_under_both_roots_indexes_as_internal_only() {
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening the index");

        // `ArtifactV2Workspace::task_dir` probes `internal_tasks/` first, so
        // this record reads as internal everywhere else in the system.
        seed_scope(&dir, &alpha(), &[TaskFixture::new("task-both")]);
        seed_internal_scope(
            &dir,
            &alpha(),
            &[TaskFixture::new("task-both").lifecycle("internal")],
        );

        index
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("rebuilding from disk");

        assert!(
            indexed_ids(&index, ListKind::Task).is_empty(),
            "indexing the tasks/ copy would surface a task the file walk hides"
        );
        assert_eq!(
            indexed_ids(&index, ListKind::Internal),
            vec!["task-both".to_string()]
        );
    }

    #[test]
    fn a_record_whose_files_vanished_is_skipped_rather_than_fatal() {
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening the index");

        seed_scope(
            &dir,
            &alpha(),
            &[
                TaskFixture::new("task-intact"),
                TaskFixture::new("task-losing-its-manifest"),
                TaskFixture::new("task-losing-its-state"),
            ],
        );
        let root = tasks_root(&scopes_root(&dir), &alpha());
        // A task deleted between the read_dir and the read of its files, and
        // one whose write was interrupted between its two files.
        std::fs::remove_file(root.join("task-losing-its-manifest").join("manifest.json"))
            .expect("removing a manifest mid-walk");
        std::fs::remove_file(
            root.join("task-losing-its-state")
                .join("state")
                .join("task_state.json"),
        )
        .expect("removing task state mid-walk");
        // And a directory that was never a task at all.
        std::fs::create_dir_all(root.join("not-a-task")).expect("creating a stray directory");

        let report = index
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("a vanished record must not fail the rebuild");

        assert_eq!(
            indexed_ids(&index, ListKind::Task),
            vec!["task-intact".to_string()]
        );
        assert_eq!(report.entries_skipped, 3);
        assert!(
            index.is_ready().expect("readiness"),
            "skipping unreadable records still finishes the rebuild"
        );
    }

    #[test]
    fn an_interrupted_rebuild_resumes_instead_of_starting_over() {
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening the index");

        seed_scope(
            &dir,
            &alpha(),
            &[
                TaskFixture::new("task-alpha-one"),
                TaskFixture::new("task-alpha-two"),
            ],
        );
        seed_scope(&dir, &beta(), &[TaskFixture::new("task-beta-one")]);

        // Stand in for a rebuild that got through alpha and then died: alpha's
        // two units are committed with their progress markers, beta's are not.
        let alpha_tasks: Vec<ListEntry> = {
            let mut entries = Vec::new();
            let mut stats = RebuildReport::default();
            walk_task_root(
                &scopes_root(&dir),
                &tasks_root(&scopes_root(&dir), &alpha()),
                UnitKind::Task,
                &alpha(),
                &mut entries,
                &mut stats,
            )
            .expect("walking alpha's tasks root");
            entries
        };
        index
            .commit_unit(UnitKind::Task, &alpha(), &alpha_tasks)
            .expect("committing alpha's tasks unit");
        index
            .commit_unit(UnitKind::Internal, &alpha(), &[])
            .expect("committing alpha's internal unit");
        assert_eq!(index.rebuild_progress_units().expect("progress"), 2);

        // Now delete alpha's files. A rebuild that RESTARTED would re-walk
        // alpha, find nothing, and drop its rows; one that RESUMES keeps them.
        std::fs::remove_dir_all(tasks_root(&scopes_root(&dir), &alpha()))
            .expect("removing alpha's tasks root");

        let report = index
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("resuming the rebuild");

        assert_eq!(
            report.units_resumed, 2,
            "alpha's two finished units must be skipped, not re-walked"
        );
        assert_eq!(report.units_walked, 2, "only beta's two units remain");
        assert_eq!(
            indexed_ids(&index, ListKind::Task),
            vec![
                "task-alpha-one".to_string(),
                "task-alpha-two".to_string(),
                "task-beta-one".to_string(),
            ],
            "alpha's rows survived a rebuild that never re-read alpha's (now deleted) files"
        );
        assert_eq!(
            index.rebuild_progress_units().expect("progress"),
            0,
            "a finished rebuild clears its resume points, so the next one is a full walk"
        );

        // Proof the resume points really were cleared: this run re-walks
        // alpha, whose files are gone, and its rows go with them.
        let second = index
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("a second, full rebuild");
        assert_eq!(second.units_resumed, 0);
        assert_eq!(second.units_walked, 4);
        assert_eq!(
            indexed_ids(&index, ListKind::Task),
            vec!["task-beta-one".to_string()]
        );
    }

    #[test]
    fn a_rebuild_is_unreadable_until_it_finishes() {
        let dir = TempDir::new().expect("temp dir");
        let path = index_path(&dir);
        let index = ListIndex::open_at(&path).expect("opening the index");
        seed_scope(&dir, &alpha(), &[TaskFixture::new("task-one")]);

        assert!(
            !index.is_ready().expect("readiness"),
            "a fresh index has indexed nothing"
        );
        // A unit lands, but the rebuild has not finished.
        index
            .commit_unit(UnitKind::Task, &alpha(), &[])
            .expect("committing one unit");
        assert!(
            !index.is_ready().expect("readiness"),
            "a partly-built index must not be served: it looks like a complete one \
             with fewer tasks in it"
        );
        drop(index);

        let reopened = ListIndex::open_at(&path).expect("reopening after an interrupted rebuild");
        assert!(
            !reopened.is_ready().expect("readiness"),
            "the unfinished marker is on disk, so a crash does not launder it into readiness"
        );
        reopened
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("finishing the rebuild");
        assert!(reopened.is_ready().expect("readiness"));
    }

    #[test]
    fn a_write_path_reindexes_one_record_from_disk() {
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening the index");
        seed_scope(&dir, &alpha(), &[TaskFixture::new("task-one")]);
        index
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("rebuilding from disk");

        // A create: the file appears, the write path reindexes it.
        seed_scope(
            &dir,
            &alpha(),
            &[TaskFixture::new("task-new").status("running")],
        );
        assert_eq!(
            index
                .index_task_from_disk(&scopes_root(&dir), &alpha(), "task-new")
                .expect("indexing a new task")
                .rows,
            1
        );
        assert_eq!(
            indexed_ids(&index, ListKind::Task),
            vec!["task-new".to_string(), "task-one".to_string()]
        );
        assert_eq!(
            indexed_column(&index, ListKind::Task, "task-new", "status"),
            "running"
        );

        // An update: the file changes, the same call brings the row along.
        seed_scope(
            &dir,
            &alpha(),
            &[TaskFixture::new("task-new")
                .status("completed")
                .updated_at("2026-07-30T18:30:00+00:00")],
        );
        index
            .index_task_from_disk(&scopes_root(&dir), &alpha(), "task-new")
            .expect("reindexing an updated task");
        assert_eq!(
            indexed_column(&index, ListKind::Task, "task-new", "status"),
            "completed"
        );
        assert_eq!(
            indexed_int(&index, ListKind::Task, "task-new", "updated_at"),
            DateTime::parse_from_rfc3339("2026-07-30T18:30:00+00:00")
                .expect("fixture timestamp")
                .timestamp_millis()
        );
        assert_eq!(
            indexed_ids(&index, ListKind::Task).len(),
            2,
            "an update must move the row, not add a second one"
        );

        // A delete: the files go, and the same call removes the row.
        std::fs::remove_dir_all(tasks_root(&scopes_root(&dir), &alpha()).join("task-new"))
            .expect("deleting a task directory");
        assert_eq!(
            index
                .index_task_from_disk(&scopes_root(&dir), &alpha(), "task-new")
                .expect("reindexing a deleted task")
                .rows,
            0
        );
        assert_eq!(
            indexed_ids(&index, ListKind::Task),
            vec!["task-one".to_string()]
        );
    }

    /// **A reindex that would write the same thirteen columns writes nothing.**
    ///
    /// This call runs on every task-record commit, and a running execution
    /// commits constantly. `reduce_step_event` is the shape that matters: it
    /// writes `task_state.json` only to advance `last_progress_at`, which no
    /// column here holds. The row it would write is the row already there, and
    /// the `DELETE`+upsert underneath it is pure write amplification.
    ///
    /// **The rows decide, not the record.** The last case is the proof: the
    /// disk does not change at all and the row is simply missing, and the
    /// reindex writes anyway. A skip that reasoned "the record did not change,
    /// so the index is fine" would leave it missing — which is the class of
    /// bug this whole seam exists to stop.
    #[test]
    fn a_reindex_that_would_change_no_column_writes_nothing() {
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening the index");
        seed_scope(&dir, &alpha(), &[TaskFixture::new("task-step")]);
        assert!(
            index
                .index_task_from_disk(&scopes_root(&dir), &alpha(), "task-step")
                .expect("the first index writes")
                .rewritten,
            "the first index has no row to compare against, so it has to write"
        );

        // The step-event write: `last_progress_at` moves and nothing else
        // does. The record on disk is genuinely different; not one indexed
        // column is.
        let state_path = tasks_root(&scopes_root(&dir), &alpha())
            .join("task-step")
            .join("state")
            .join("task_state.json");
        let mut state: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&state_path).expect("the fixture state reads"),
        )
        .expect("the fixture state parses");
        state["last_progress_at"] = serde_json::json!("2026-07-30T11:00:00+00:00");
        std::fs::write(
            &state_path,
            serde_json::to_string_pretty(&state).expect("re-serializing the fixture state"),
        )
        .expect("writing the fixture state back");

        assert!(
            !index
                .index_task_from_disk(&scopes_root(&dir), &alpha(), "task-step")
                .expect("the second index compares")
                .rewritten,
            "a record write that moves no indexed column owes no index write"
        );
        assert_eq!(
            indexed_column(&index, ListKind::Task, "task-step", "status"),
            "pending",
            "and the row it declined to rewrite is still the right row"
        );

        // An indexed column moves: the write is owed, and happens.
        seed_scope(
            &dir,
            &alpha(),
            &[TaskFixture::new("task-step").status("running")],
        );
        assert!(
            index
                .index_task_from_disk(&scopes_root(&dir), &alpha(), "task-step")
                .expect("the third index writes")
                .rewritten,
            "`status` is indexed, so moving it owes the write"
        );
        assert_eq!(
            indexed_column(&index, ListKind::Task, "task-step", "status"),
            "running"
        );

        // Nothing on disk changes from here. The row is simply gone.
        assert!(
            index
                .remove(ListKind::Task, "task-step")
                .expect("the index removes"),
            "the row has to have been there for its absence to prove anything"
        );
        assert!(
            index
                .index_task_from_disk(&scopes_root(&dir), &alpha(), "task-step")
                .expect("the fourth index writes")
                .rewritten,
            "a missing row is a difference, and every difference is written"
        );
        assert_eq!(
            indexed_column(&index, ListKind::Task, "task-step", "status"),
            "running",
            "the row a reindex restores is the one the disk describes"
        );
    }

    #[test]
    fn reindexing_drops_the_monitor_row_when_a_task_stops_being_one() {
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening the index");
        seed_scope(&dir, &alpha(), &[TaskFixture::new("task-watch").monitor()]);
        index
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("rebuilding from disk");
        assert_eq!(
            indexed_ids(&index, ListKind::Monitor),
            vec!["task-watch".to_string()]
        );

        // The spec is removed from the manifest.
        seed_scope(&dir, &alpha(), &[TaskFixture::new("task-watch")]);
        index
            .index_task_from_disk(&scopes_root(&dir), &alpha(), "task-watch")
            .expect("reindexing after the spec was removed");
        assert!(
            indexed_ids(&index, ListKind::Monitor).is_empty(),
            "a stale monitor row would keep listing a task that is no longer a monitor"
        );
        assert_eq!(
            indexed_ids(&index, ListKind::Task),
            vec!["task-watch".to_string()]
        );

        // Archiving has the same effect — the monitors listing drops archived
        // monitors, so the index must too.
        seed_scope(
            &dir,
            &alpha(),
            &[TaskFixture::new("task-watch").monitor().status("archived")],
        );
        index
            .index_task_from_disk(&scopes_root(&dir), &alpha(), "task-watch")
            .expect("reindexing an archived monitor");
        assert!(indexed_ids(&index, ListKind::Monitor).is_empty());
    }

    #[test]
    fn removing_a_task_clears_every_kind_it_could_hold() {
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening the index");
        seed_scope(&dir, &alpha(), &[TaskFixture::new("task-watch").monitor()]);
        index
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("rebuilding from disk");

        assert_eq!(
            index.remove_task("task-watch").expect("removing a task"),
            2,
            "the delete path must not need to know the record had a monitor row too"
        );
        assert!(row_ids(&index).is_empty());
        assert_eq!(
            index.remove_task("task-watch").expect("removing again"),
            0,
            "a second delete is a no-op, not an error"
        );
    }

    #[test]
    fn a_rebuild_is_idempotent_and_reflects_deletions() {
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening the index");
        seed_scope(
            &dir,
            &alpha(),
            &[TaskFixture::new("task-one"), TaskFixture::new("task-two")],
        );

        let first = index
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("first rebuild");
        let second = index
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("second rebuild");
        assert_eq!(first.entries_indexed, second.entries_indexed);
        assert_eq!(
            indexed_ids(&index, ListKind::Task),
            vec!["task-one".to_string(), "task-two".to_string()],
            "re-running the walk must not duplicate rows"
        );

        std::fs::remove_dir_all(tasks_root(&scopes_root(&dir), &alpha()).join("task-two"))
            .expect("deleting a task directory");
        index
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("third rebuild");
        assert_eq!(
            indexed_ids(&index, ListKind::Task),
            vec!["task-one".to_string()],
            "a re-walked scope must not leave rows behind for records that are gone"
        );
    }

    #[test]
    fn an_unparseable_timestamp_is_counted_rather_than_guessed() {
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening the index");
        seed_scope(
            &dir,
            &alpha(),
            &[TaskFixture::new("task-broken-clock").updated_at("not a timestamp")],
        );

        let report = index
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("rebuilding from disk");

        assert_eq!(
            report.unparsed_timestamps, 1,
            "a record the index cannot order must be visible in the report, \
             not silently sorted to the bottom of every list"
        );
        assert_eq!(
            indexed_int(&index, ListKind::Task, "task-broken-clock", "updated_at"),
            0
        );
        assert_eq!(
            indexed_ids(&index, ListKind::Task),
            vec!["task-broken-clock".to_string()],
            "it is still indexed — dropping it would make the list disagree with the disk"
        );
    }

    #[test]
    fn epoch_millis_parses_the_timestamps_task_records_actually_carry() {
        assert_eq!(
            epoch_millis_from_rfc3339("1970-01-01T00:00:01+00:00"),
            Some(1_000)
        );
        assert_eq!(
            epoch_millis_from_rfc3339("2026-07-30T10:00:00.500+00:00"),
            Some(
                DateTime::parse_from_rfc3339("2026-07-30T10:00:00+00:00")
                    .expect("base")
                    .timestamp_millis()
                    + 500
            ),
            "sub-second precision must survive, or same-second records tie arbitrarily"
        );
        assert_eq!(
            epoch_millis_from_rfc3339("2026-07-30T15:30:00+05:30"),
            epoch_millis_from_rfc3339("2026-07-30T10:00:00+00:00"),
            "an offset timestamp is the same instant as its UTC equivalent"
        );
        for junk in ["", "not a timestamp", "2026-07-30", "1730000000"] {
            assert_eq!(
                epoch_millis_from_rfc3339(junk),
                None,
                "{junk} is not RFC3339 and must not parse to a plausible-looking instant"
            );
        }
    }

    #[test]
    fn every_kind_round_trips_through_its_wire_name_and_nothing_else() {
        assert_eq!(ListKind::parse("task"), Some(ListKind::Task));
        assert_eq!(ListKind::parse("internal"), Some(ListKind::Internal));
        assert_eq!(ListKind::parse("monitor"), Some(ListKind::Monitor));
        assert_eq!(ListKind::parse("attention"), Some(ListKind::Attention));
        for kind in ListKind::ALL {
            assert_eq!(ListKind::parse(kind.as_str()), Some(kind));
        }
        for junk in ["", " ", "Task", "tasks", "internal_tasks", "monitors"] {
            assert_eq!(ListKind::parse(junk), None, "{junk} must not parse");
        }
    }

    #[test]
    fn attention_rows_share_the_table_without_a_task_root() {
        // Attention items do not live under a scope's task roots, so they are
        // fed by their own surface. The table must carry them all the same.
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening the index");

        index
            .upsert_many(&[
                ListEntry {
                    kind: ListKind::Attention,
                    id: "annotation-1".to_string(),
                    scope: alpha(),
                    updated_at: 20,
                    created_at: 10,
                    status: "needs_approval".to_string(),
                    agent_id: "approvals-agent".to_string(),
                    lifecycle: None,
                    due_date: None,
                    tags: Vec::new(),
                    title: Some("approve the reply".to_string()),
                    paused: false,
                },
                ListEntry {
                    kind: ListKind::Attention,
                    id: "annotation-2".to_string(),
                    scope: alpha(),
                    updated_at: 30,
                    created_at: 11,
                    status: "needs_approval".to_string(),
                    agent_id: "approvals-agent".to_string(),
                    lifecycle: None,
                    due_date: None,
                    tags: Vec::new(),
                    title: None,
                    paused: false,
                },
            ])
            .expect("writing attention rows");

        assert_eq!(
            indexed_ids(&index, ListKind::Attention),
            vec!["annotation-1".to_string(), "annotation-2".to_string()]
        );
        assert!(index
            .remove(ListKind::Attention, "annotation-1")
            .expect("removing an attention row"),);
        assert_eq!(
            indexed_ids(&index, ListKind::Attention),
            vec!["annotation-2".to_string()]
        );
        assert!(
            !index
                .remove(ListKind::Attention, "annotation-1")
                .expect("removing it again"),
            "removing a row that is not there reports false rather than erroring"
        );
    }

    // -----------------------------------------------------------------
    // Task 3 — the page query
    // -----------------------------------------------------------------

    const TODAY: &str = "2026-07-30";
    const YESTERDAY: &str = "2026-07-29";
    const TOMORROW: &str = "2026-07-31";

    /// A corpus that exercises every lane and every edge the predicates were
    /// written for: an empty due date (which sorts before every real date), a
    /// full timestamp due today, completed work that is also past due, and a
    /// paused run.
    fn lane_fixtures() -> Vec<TaskFixture> {
        vec![
            // untagged + pending -> all, inbox
            TaskFixture::new("row-a").updated_at("2026-07-30T10:00:09+00:00"),
            // tagged, due today as a full timestamp, running -> all, today, running
            TaskFixture::new("row-b")
                .tags(&["work"])
                .status("running")
                .due_date("2026-07-30T09:00:00Z")
                .updated_at("2026-07-30T10:00:08+00:00"),
            // tagged, due yesterday, unfinished -> all, overdue
            TaskFixture::new("row-c")
                .tags(&["home"])
                .due_date(YESTERDAY)
                .updated_at("2026-07-30T10:00:07+00:00"),
            // finished, due yesterday -> completed only; NOT all, NOT overdue
            TaskFixture::new("row-d")
                .status("completed")
                .due_date(YESTERDAY)
                .updated_at("2026-07-30T10:00:06+00:00"),
            // paused with an EMPTY due date -> all, running; no date lane
            TaskFixture::new("row-e")
                .status("paused")
                .due_date("")
                .updated_at("2026-07-30T10:00:05+00:00"),
            // due tomorrow -> all, inbox; neither date lane
            TaskFixture::new("row-f")
                .due_date(TOMORROW)
                .updated_at("2026-07-30T10:00:04+00:00"),
            // due today exactly -> all, inbox, today; NOT overdue
            TaskFixture::new("row-g")
                .due_date(TODAY)
                .updated_at("2026-07-30T10:00:03+00:00"),
            // failed and overdue -> all, overdue
            TaskFixture::new("row-h")
                .status("failed")
                .tags(&["work"])
                .due_date(YESTERDAY)
                .updated_at("2026-07-30T10:00:02+00:00"),
        ]
    }

    /// The same narrow slice `task_api_v3`'s `lane_task_slice` builds from a
    /// list item — status, tag NAMES, due date.
    fn fixture_lane_task(fixture: &TaskFixture) -> crate::magician_v2::task_lanes::LaneTask {
        crate::magician_v2::task_lanes::LaneTask {
            status: fixture.status.clone(),
            tags: fixture.tags.clone(),
            due_date: fixture.due_date.clone(),
        }
    }

    /// A scope seeded with `fixtures` and fully indexed — the setup every
    /// reconciliation test starts from.
    fn indexed_with(fixtures: &[TaskFixture]) -> (TempDir, ListIndex) {
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening the index");
        seed_scope(&dir, &alpha(), fixtures);
        index
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("rebuilding from disk");
        (dir, index)
    }

    fn indexed_with_lane_fixtures() -> (TempDir, ListIndex) {
        indexed_with(&lane_fixtures())
    }

    fn page_ids(page: &ListPage) -> Vec<String> {
        page.items.iter().map(|entry| entry.id.clone()).collect()
    }

    /// Every id the index returns for a lane, in one page big enough to hold
    /// the corpus, sorted so the comparison is about membership.
    fn sorted_lane_ids(index: &ListIndex, lane: TaskLane) -> Vec<String> {
        let query = ListPageQuery::new(ListKind::Task, alpha(), 500).lane(lane, TODAY);
        let mut ids = page_ids(&index.page(&query).expect("paging a lane"));
        ids.sort();
        ids
    }

    /// **This test is the point of the task.** Two definitions of `overdue` is
    /// exactly how the index and the file walk come to disagree, and the
    /// disagreement would be invisible: the list would simply be wrong, and
    /// confidently so.
    #[test]
    fn every_lane_agrees_with_task_lanes_over_one_fixture() {
        let (_dir, index) = indexed_with_lane_fixtures();
        let fixtures = lane_fixtures();

        for lane in TaskLane::ALL {
            let mut expected: Vec<String> = fixtures
                .iter()
                .filter(|fixture| lane.matches(&fixture_lane_task(fixture), TODAY))
                .map(|fixture| fixture.id.clone())
                .collect();
            expected.sort();

            assert_eq!(
                sorted_lane_ids(&index, lane),
                expected,
                "the index and `TaskLane::{:?}` disagree about which rows are in `{}`",
                lane,
                lane.wire_name()
            );

            // A lane that matched nothing on both sides proves nothing.
            assert!(
                !expected.is_empty(),
                "the fixture must exercise `{}`, or this comparison is vacuous",
                lane.wire_name()
            );
            assert!(
                expected.len() < fixtures.len(),
                "`{}` must exclude something, or a predicate of `1 = 1` would pass",
                lane.wire_name()
            );
        }
    }

    #[test]
    fn the_lane_edges_are_the_ones_the_predicates_were_written_for() {
        // Spelled out rather than derived, so a fixture edited out of the
        // corpus fails here instead of quietly agreeing with itself.
        let (_dir, index) = indexed_with_lane_fixtures();

        assert_eq!(
            sorted_lane_ids(&index, TaskLane::All),
            vec!["row-a", "row-b", "row-c", "row-e", "row-f", "row-g", "row-h"],
            "`all` keeps unfinished work of every status and drops only completed"
        );
        assert_eq!(
            sorted_lane_ids(&index, TaskLane::Inbox),
            vec!["row-a", "row-f", "row-g"],
            "`inbox` needs untagged AND pending together"
        );
        assert_eq!(
            sorted_lane_ids(&index, TaskLane::Today),
            vec!["row-b", "row-g"],
            "a stored full timestamp is still due today; an empty due date is not"
        );
        assert_eq!(
            sorted_lane_ids(&index, TaskLane::Overdue),
            vec!["row-c", "row-h"],
            "the completed row and the empty-due-date row must not read as late"
        );
        assert_eq!(
            sorted_lane_ids(&index, TaskLane::Running),
            vec!["row-b", "row-e"],
            "`running` folds paused in"
        );
        assert_eq!(sorted_lane_ids(&index, TaskLane::Completed), vec!["row-d"]);
    }

    #[test]
    fn an_empty_due_date_does_not_make_every_row_overdue() {
        // The guard this test exists for is one `due_date <> ''`. Without it
        // an empty string sorts before every real date and every untouched
        // row reads as owed — a whole task list quietly moving into the
        // overdue lane.
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening the index");
        seed_scope(
            &dir,
            &alpha(),
            &[
                TaskFixture::new("row-empty-due").due_date(""),
                TaskFixture::new("row-no-due"),
                TaskFixture::new("row-really-late").due_date(YESTERDAY),
            ],
        );
        index
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("rebuilding from disk");

        assert_eq!(
            sorted_lane_ids(&index, TaskLane::Overdue),
            vec!["row-really-late"]
        );
    }

    #[test]
    fn lane_counts_report_every_lane_over_the_whole_pool() {
        let (_dir, index) = indexed_with_lane_fixtures();

        let counts = index
            .lane_counts(ListKind::Task, &alpha(), TODAY)
            .expect("counting lanes");

        assert_eq!(counts.get("all"), Some(&7));
        assert_eq!(counts.get("inbox"), Some(&3));
        assert_eq!(counts.get("today"), Some(&2));
        assert_eq!(counts.get("overdue"), Some(&2));
        assert_eq!(counts.get("running"), Some(&2));
        assert_eq!(counts.get("completed"), Some(&1));

        // Stable, sorted key order — the payload stays diffable.
        assert_eq!(
            counts.keys().copied().collect::<Vec<_>>(),
            vec!["all", "completed", "inbox", "overdue", "running", "today"]
        );

        // Counts are the corpus, not the page: they must not move when the
        // reader narrows to a lane or walks to page two.
        for lane in TaskLane::ALL {
            assert_eq!(
                index
                    .total(ListKind::Task, &alpha(), Some(lane), TODAY)
                    .expect("lane total"),
                *counts
                    .get(lane.wire_name())
                    .expect("every lane is present in the counts"),
                "`total` and `lane_counts` must be the same number for `{}`",
                lane.wire_name()
            );
        }
    }

    #[test]
    fn an_empty_scope_reports_every_lane_at_zero_rather_than_omitting_it() {
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening the index");

        let counts = index
            .lane_counts(ListKind::Task, &alpha(), TODAY)
            .expect("counting lanes over nothing");

        assert_eq!(counts.len(), 6, "all six lanes must be present");
        for lane in TaskLane::ALL {
            assert_eq!(
                counts.get(lane.wire_name()),
                Some(&0),
                "{} must report 0, not go missing — a client renders nothing for a \
                 missing key where it should render a zero",
                lane.wire_name()
            );
        }
    }

    #[test]
    fn counts_and_pages_stay_inside_their_scope_and_kind() {
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening the index");
        seed_scope(&dir, &alpha(), &[TaskFixture::new("alpha-task")]);
        seed_scope(&dir, &beta(), &[TaskFixture::new("beta-task")]);
        seed_internal_scope(
            &dir,
            &alpha(),
            &[TaskFixture::new("alpha-internal").lifecycle("internal")],
        );
        index
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("rebuilding from disk");

        let alpha_tasks = index
            .page(&ListPageQuery::new(ListKind::Task, alpha(), 50))
            .expect("paging alpha's tasks");
        assert_eq!(page_ids(&alpha_tasks), vec!["alpha-task"]);
        assert_eq!(alpha_tasks.total, 1);

        let beta_tasks = index
            .page(&ListPageQuery::new(ListKind::Task, beta(), 50))
            .expect("paging beta's tasks");
        assert_eq!(page_ids(&beta_tasks), vec!["beta-task"]);

        let alpha_internal = index
            .page(&ListPageQuery::new(ListKind::Internal, alpha(), 50))
            .expect("paging alpha's internal tasks");
        assert_eq!(page_ids(&alpha_internal), vec!["alpha-internal"]);

        assert_eq!(
            index
                .lane_counts(ListKind::Task, &beta(), TODAY)
                .expect("counting beta's lanes")
                .get("all")
                .copied(),
            Some(1),
            "one scope's counts must not include another's rows"
        );
    }

    #[test]
    fn a_page_is_newest_first_and_offsets_walk_the_whole_corpus() {
        let (_dir, index) = indexed_with_lane_fixtures();

        let first = index
            .page(&ListPageQuery::new(ListKind::Task, alpha(), 3))
            .expect("first page");
        assert_eq!(
            page_ids(&first),
            vec!["row-a", "row-b", "row-c"],
            "newest updated_at first"
        );
        assert_eq!(first.total, 8, "the total is the corpus, not the page");
        assert_eq!(first.limit, 3);
        assert_eq!(first.offset, 0);
        assert!(first.has_more);

        let second = index
            .page(&ListPageQuery::new(ListKind::Task, alpha(), 3).offset(3))
            .expect("second page");
        assert_eq!(page_ids(&second), vec!["row-d", "row-e", "row-f"]);
        assert_eq!(second.offset, 3);
        assert_eq!(second.total, 8);
        assert!(second.has_more);

        let last = index
            .page(&ListPageQuery::new(ListKind::Task, alpha(), 3).offset(6))
            .expect("last page");
        assert_eq!(page_ids(&last), vec!["row-g", "row-h"]);
        assert!(
            !last.has_more,
            "the final page must not claim another follows"
        );
        assert_eq!(last.next_cursor, None);

        let past_the_end = index
            .page(&ListPageQuery::new(ListKind::Task, alpha(), 3).offset(99))
            .expect("a page past the end");
        assert!(past_the_end.items.is_empty());
        assert_eq!(
            past_the_end.total, 8,
            "walking off the end does not change how much there is"
        );
        assert!(!past_the_end.has_more);
    }

    #[test]
    fn a_cursor_walks_the_same_corpus_as_offsets_do() {
        let (_dir, index) = indexed_with_lane_fixtures();

        let mut walked: Vec<String> = Vec::new();
        let mut cursor: Option<ListCursor> = None;
        for _ in 0..10 {
            let mut query = ListPageQuery::new(ListKind::Task, alpha(), 3);
            if let Some(cursor) = cursor.clone() {
                query = query.cursor(cursor);
            }
            let page = index.page(&query).expect("paging by cursor");
            walked.extend(page_ids(&page));
            match page.next_cursor.as_deref() {
                Some(raw) => cursor = Some(ListCursor::decode(raw).expect("decoding next_cursor")),
                None => break,
            }
        }

        assert_eq!(
            walked,
            vec!["row-a", "row-b", "row-c", "row-d", "row-e", "row-f", "row-g", "row-h"],
            "a cursor walk must visit every row exactly once, in the same order \
             an offset walk does"
        );
    }

    #[test]
    fn a_cursor_page_carries_the_lane_total_not_the_remainder() {
        let (_dir, index) = indexed_with_lane_fixtures();

        let first = index
            .page(&ListPageQuery::new(ListKind::Task, alpha(), 2).lane(TaskLane::All, TODAY))
            .expect("first lane page");
        assert_eq!(first.total, 7);
        let cursor = ListCursor::decode(first.next_cursor.as_deref().expect("a next cursor"))
            .expect("decoding the cursor");

        let second = index
            .page(
                &ListPageQuery::new(ListKind::Task, alpha(), 2)
                    .lane(TaskLane::All, TODAY)
                    .cursor(cursor),
            )
            .expect("second lane page");
        assert_eq!(page_ids(&second), vec!["row-c", "row-e"]);
        assert_eq!(
            second.total, 7,
            "a total that shrank with each page would report one page however \
             many there are"
        );
        // Two rows of the lane precede this page, and the envelope must say so.
        //
        // This assertion used to read `0`, on the reasoning that a keyset seek
        // needs no SQL `OFFSET`. True of the SQL and false of the envelope:
        // the walk this index replaced resolved the cursor to a real position,
        // and `an_indexed_page_is_the_page_the_walk_serves` caught the two
        // disagreeing. A pager fed `0` renders page two's rows while showing
        // page one as current.
        assert_eq!(
            second.offset, 2,
            "a cursor page reports where the reader IS, not the seek's own offset"
        );
    }

    #[test]
    fn a_cursor_supersedes_an_offset_rather_than_compounding_with_it() {
        let (_dir, index) = indexed_with_lane_fixtures();

        let first = index
            .page(&ListPageQuery::new(ListKind::Task, alpha(), 2))
            .expect("first page");
        let cursor = ListCursor::decode(first.next_cursor.as_deref().expect("a next cursor"))
            .expect("decoding the cursor");

        let seeked = index
            .page(
                &ListPageQuery::new(ListKind::Task, alpha(), 2)
                    .offset(4)
                    .cursor(cursor),
            )
            .expect("a page with both a cursor and an offset");
        assert_eq!(
            page_ids(&seeked),
            vec!["row-c", "row-d"],
            "seeking and counting to the same place at once would page twice"
        );
    }

    #[test]
    fn same_millisecond_rows_break_their_tie_by_id_descending() {
        // The keyset's tiebreaker, which is what stops a cursor walk from
        // looping on, or skipping over, rows written in the same millisecond.
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening the index");
        let same_instant = "2026-07-30T10:00:00+00:00";
        seed_scope(
            &dir,
            &alpha(),
            &[
                TaskFixture::new("tie-a").updated_at(same_instant),
                TaskFixture::new("tie-b").updated_at(same_instant),
                TaskFixture::new("tie-c").updated_at(same_instant),
            ],
        );
        index
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("rebuilding from disk");

        let first = index
            .page(&ListPageQuery::new(ListKind::Task, alpha(), 2))
            .expect("first page");
        assert_eq!(page_ids(&first), vec!["tie-c", "tie-b"]);

        let cursor = ListCursor::decode(first.next_cursor.as_deref().expect("a next cursor"))
            .expect("decoding the cursor");
        let second = index
            .page(&ListPageQuery::new(ListKind::Task, alpha(), 2).cursor(cursor))
            .expect("second page");
        assert_eq!(
            page_ids(&second),
            vec!["tie-a"],
            "the tiebreaker must carry the walk past a whole page of identical timestamps"
        );
    }

    #[test]
    fn a_cursor_is_the_wire_form_attention_already_carries() {
        // `{updated_at}:{url-encoded id}` — byte-for-byte
        // `encode_attention_lane_cursor`. Spelled out rather than round-tripped,
        // because a round trip agrees with itself in any format at all.
        assert_eq!(
            ListCursor::new(1_730_000_000_000, "task-a").encode(),
            "1730000000000:task-a"
        );
        assert_eq!(
            ListCursor::new(20, "a:b").encode(),
            "20:a%3Ab",
            "an id containing the separator must survive the round trip"
        );
        assert_eq!(
            ListCursor::decode("20:a%3Ab").expect("decoding"),
            ListCursor::new(20, "a:b")
        );
        assert_eq!(
            ListCursor::decode("-1:x").expect("decoding a negative instant"),
            ListCursor::new(-1, "x")
        );

        for junk in ["", "nocolon", "notanumber:x", "20:", "20:%20"] {
            assert!(
                ListCursor::decode(junk).is_err(),
                "{junk} must not decode into a position the page would then seek to"
            );
        }
    }

    /// **This test is the point of `row_follows_cursor_with`.** A list handler
    /// honours a cursor from Rust while the index is rebuilding and from SQL
    /// once it is ready. Two definitions of "after the cursor" would make the
    /// same page skip or repeat a row depending on which path served it, and
    /// neither page would look wrong on its own.
    ///
    /// Run for BOTH tiebreaks. There are two orders on the wire, so there are
    /// two chances for the SQL and the Rust to drift, and the ascending one
    /// is the newer of the pair.
    #[test]
    fn keyset_predicate_matches_the_page_sql() {
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening the index");
        let same_instant = "2026-07-30T10:00:00+00:00";
        // Deliberately three rows at ONE instant, so the tiebreaker — not the
        // timestamp — is what decides most of these comparisons.
        let fixtures = vec![
            TaskFixture::new("row-a").updated_at(same_instant),
            TaskFixture::new("row-b").updated_at(same_instant),
            TaskFixture::new("row-c").updated_at(same_instant),
            TaskFixture::new("row-d").updated_at("2026-07-30T09:00:00+00:00"),
            TaskFixture::new("row-e").updated_at("2026-07-30T11:00:00+00:00"),
        ];
        seed_scope(&dir, &alpha(), &fixtures);
        index
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("rebuilding from disk");

        for tie_break in [ListTieBreak::IdDescending, ListTieBreak::IdAscending] {
            let whole = ListPageQuery::new(ListKind::Task, alpha(), 500).tie_break(tie_break);
            let all = index.page(&whole).expect("the whole corpus");
            assert_eq!(all.items.len(), fixtures.len(), "every fixture is indexed");

            let mut agreed_true = 0usize;
            let mut agreed_false = 0usize;
            for anchor in &all.items {
                let cursor = ListCursor::new(anchor.updated_at, anchor.id.clone());
                // What the SQL says follows this cursor.
                let from_sql: Vec<String> = page_ids(
                    &index
                        .page(&whole.clone().cursor(cursor.clone()))
                        .expect("a cursor page"),
                );
                // What the Rust predicate says follows it, over the same corpus
                // in the same order.
                let from_rust: Vec<String> = all
                    .items
                    .iter()
                    .filter(|entry| {
                        row_follows_cursor_with(tie_break, &cursor, entry.updated_at, &entry.id)
                    })
                    .map(|entry| entry.id.clone())
                    .collect();
                assert_eq!(
                    from_rust, from_sql,
                    "Rust and SQL disagree about what follows {} under {tie_break:?}",
                    anchor.id
                );
                for entry in &all.items {
                    if row_follows_cursor_with(tie_break, &cursor, entry.updated_at, &entry.id) {
                        agreed_true += 1;
                    } else {
                        agreed_false += 1;
                    }
                }
            }
            // A predicate that answered `true` for everything, or `false` for
            // everything, would satisfy the equality above just as well.
            assert!(
                agreed_true > 0 && agreed_false > 0,
                "the predicate must discriminate under {tie_break:?}: \
                 {agreed_true} after, {agreed_false} not"
            );
            // And it must be strict at the cursor's own row — a cursor that
            // includes itself pages the same row forever.
            let anchor = all.items.first().expect("a row");
            assert!(!row_follows_cursor_with(
                tie_break,
                &ListCursor::new(anchor.updated_at, anchor.id.clone()),
                anchor.updated_at,
                &anchor.id
            ));
        }
    }

    /// The two tiebreaks must actually differ, and differ only inside a tie
    /// group. If `ListTieBreak` were ignored somewhere in the SQL assembly
    /// both orders would still be internally consistent — and every test
    /// above would still pass while `/monitors` silently reordered.
    #[test]
    fn the_two_tiebreaks_agree_on_everything_except_a_tie() {
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening the index");
        let same_instant = "2026-07-30T10:00:00+00:00";
        seed_scope(
            &dir,
            &alpha(),
            &[
                TaskFixture::new("tie-a").updated_at(same_instant),
                TaskFixture::new("tie-b").updated_at(same_instant),
                TaskFixture::new("tie-c").updated_at(same_instant),
                TaskFixture::new("older").updated_at("2026-07-30T09:00:00+00:00"),
                TaskFixture::new("newer").updated_at("2026-07-30T11:00:00+00:00"),
            ],
        );
        index
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("rebuilding from disk");

        let whole = |tie_break| {
            page_ids(
                &index
                    .page(&ListPageQuery::new(ListKind::Task, alpha(), 500).tie_break(tie_break))
                    .expect("the whole corpus"),
            )
        };
        assert_eq!(
            whole(ListTieBreak::IdDescending),
            vec!["newer", "tie-c", "tie-b", "tie-a", "older"]
        );
        assert_eq!(
            whole(ListTieBreak::IdAscending),
            vec!["newer", "tie-a", "tie-b", "tie-c", "older"],
            "the ascending keyset is `/monitors`' order: the timestamp still leads, \
             only the tie runs the other way"
        );

        // And a cursor walk under the ascending tiebreak visits every row
        // exactly once — the property a reordered tie group would break.
        let mut seen = Vec::new();
        let mut cursor = None;
        loop {
            let mut query =
                ListPageQuery::new(ListKind::Task, alpha(), 2).tie_break(ListTieBreak::IdAscending);
            if let Some(cursor) = cursor.take() {
                query = query.cursor(cursor);
            }
            let page = index.page(&query).expect("an ascending cursor page");
            seen.extend(page_ids(&page));
            match page.next_cursor.as_deref() {
                Some(raw) => cursor = Some(ListCursor::decode(raw).expect("decoding")),
                None => break,
            }
        }
        assert_eq!(
            seen,
            vec!["newer", "tie-a", "tie-b", "tie-c", "older"],
            "a two-row page walked across a three-row tie group must neither \
             repeat nor skip inside it"
        );
    }

    // -----------------------------------------------------------------
    // `/monitors` — the ascending keyset, the paused filter, the bare cursor
    // -----------------------------------------------------------------

    /// A tie group at one instant, so the ascending `task_id` tiebreak is
    /// what carries the walk rather than the timestamp; a mix of paused,
    /// active and unscheduled, so `state=` has something to exclude in both
    /// directions; and one plain task, which must never reach this surface.
    fn monitor_fixtures() -> Vec<TaskFixture> {
        let tie = "2026-07-30T10:00:00+00:00";
        vec![
            TaskFixture::new("mon-newest")
                .monitor()
                .scheduled(false)
                .updated_at("2026-07-30T11:00:00+00:00"),
            TaskFixture::new("mon-tie-c")
                .monitor()
                .scheduled(true)
                .updated_at(tie),
            TaskFixture::new("mon-tie-a")
                .monitor()
                .scheduled(false)
                .updated_at(tie),
            TaskFixture::new("mon-tie-b").monitor().updated_at(tie),
            // A third paused monitor, inside the tie group. Without it
            // `state=paused` is two rows, a two-row page covers the lane in
            // one hop, and the walk's own `pages > 1` guard correctly refuses
            // to call that a proof of anything.
            TaskFixture::new("mon-tie-d")
                .monitor()
                .scheduled(true)
                .updated_at(tie),
            TaskFixture::new("mon-oldest")
                .monitor()
                .scheduled(true)
                .updated_at("2026-07-30T09:00:00+00:00"),
            TaskFixture::new("plain-task")
                .scheduled(true)
                .updated_at(tie),
        ]
    }

    fn indexed_with_monitor_fixtures() -> (TempDir, ListIndex) {
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening the index");
        seed_scope(&dir, &alpha(), &monitor_fixtures());
        index
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("rebuilding from disk");
        (dir, index)
    }

    fn monitor_query(
        paused: Option<bool>,
        cursor_task_id: Option<String>,
        limit: usize,
    ) -> MonitorPageQuery {
        MonitorPageQuery {
            scope: alpha(),
            paused,
            cursor_task_id,
            limit,
        }
    }

    /// The scan `/monitors` used to run, kept here as the thing the seek has
    /// to agree with: filter the pool, sort `updated_at` desc then `task_id`
    /// ascending, find the cursor's row by id, take a slice.
    ///
    /// Written against the FIXTURES rather than against the index, so a bug
    /// in the SQL cannot also be the oracle's bug.
    fn scanned_monitor_page(
        fixtures: &[TaskFixture],
        paused: Option<bool>,
        cursor_task_id: Option<&str>,
        limit: usize,
    ) -> (Vec<String>, usize, usize) {
        let mut pool: Vec<&TaskFixture> = fixtures
            .iter()
            .filter(|fixture| fixture.monitor && fixture.status != "archived")
            .filter(|fixture| match paused {
                None => true,
                Some(want) => {
                    schedule_is_paused(parsed_schedule(fixture.schedule.as_ref()).as_ref()) == want
                },
            })
            .collect();
        pool.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.id.cmp(&right.id))
        });

        let total = pool.len();
        let start = match cursor_task_id {
            None => 0,
            Some(cursor) => pool
                .iter()
                .position(|fixture| fixture.id == cursor)
                .map(|position| position + 1)
                .unwrap_or(total),
        };
        let offset = start.min(total);
        let ids = pool[offset..]
            .iter()
            .take(limit)
            .map(|fixture| fixture.id.clone())
            .collect();
        (ids, total, offset)
    }

    /// Spelled out rather than compared to the oracle, because an oracle that
    /// broke the tie the wrong way would agree with an index that did too.
    #[test]
    fn a_monitor_page_is_newest_first_then_task_id_ascending() {
        let (_dir, index) = indexed_with_monitor_fixtures();

        let page = index
            .monitor_page(&monitor_query(None, None, 50))
            .expect("the whole monitor corpus");
        assert_eq!(
            page.ids,
            vec![
                "mon-newest",
                "mon-tie-a",
                "mon-tie-b",
                "mon-tie-c",
                "mon-tie-d",
                "mon-oldest"
            ],
            "`/monitors` breaks a same-instant tie by task_id ASCENDING — the \
             descending keyset would have handed the reader tie-d, tie-c, \
             tie-b, tie-a"
        );
        assert_eq!(page.total, 6, "the plain task is not a monitor");
        assert_eq!(page.offset, 0);
        assert!(
            !page.ids.iter().any(|id| id == "plain-task"),
            "a task without a monitor_spec has no `monitor` row to page"
        );
    }

    #[test]
    fn the_state_filter_is_a_where_clause_rather_than_a_pass_over_the_pool() {
        let (_dir, index) = indexed_with_monitor_fixtures();

        let active = index
            .monitor_page(&monitor_query(Some(false), None, 50))
            .expect("the active monitors");
        assert_eq!(
            active.ids,
            vec!["mon-newest", "mon-tie-a", "mon-tie-b"],
            "unscheduled counts as active, exactly as `schedule_is_paused` reports"
        );
        assert_eq!(active.total, 3);

        let paused = index
            .monitor_page(&monitor_query(Some(true), None, 50))
            .expect("the paused monitors");
        assert_eq!(paused.ids, vec!["mon-tie-c", "mon-tie-d", "mon-oldest"]);
        assert_eq!(
            paused.total, 3,
            "`total` is the size of the FILTERED corpus — a pager on `state=paused` \
             that reported the whole scope would offer pages that do not exist"
        );

        let unfiltered = index
            .monitor_page(&monitor_query(None, None, 50))
            .expect("every monitor");
        assert_eq!(
            active.total + paused.total,
            unfiltered.total,
            "every monitor is in exactly one of the two states"
        );
    }

    /// **The property the whole change turns on.** Every page, at every
    /// cursor, under every filter, must be byte-identical to what the scan
    /// produced — including `total` and `offset`, which a pager renders as
    /// "page 3 of 9".
    #[test]
    fn a_monitor_cursor_walk_matches_the_scan_it_replaced() {
        let (_dir, index) = indexed_with_monitor_fixtures();
        let fixtures = monitor_fixtures();

        for paused in [None, Some(false), Some(true)] {
            let mut cursor: Option<String> = None;
            let mut visited: Vec<String> = Vec::new();
            let mut pages = 0usize;

            loop {
                let page = index
                    .monitor_page(&monitor_query(paused, cursor.clone(), 2))
                    .expect("a monitor page");
                let (expected_ids, expected_total, expected_offset) =
                    scanned_monitor_page(&fixtures, paused, cursor.as_deref(), 2);

                assert_eq!(
                    page.ids, expected_ids,
                    "seek and scan disagree at cursor {cursor:?} under state={paused:?}"
                );
                assert_eq!(page.total, expected_total, "total disagrees at {cursor:?}");
                assert_eq!(
                    page.offset, expected_offset,
                    "offset disagrees at {cursor:?} — a pager would report the wrong page"
                );

                visited.extend(page.ids.iter().cloned());
                pages += 1;
                assert!(pages < 20, "the cursor walk is not terminating");

                if page.offset + page.ids.len() >= page.total {
                    break;
                }
                cursor = page.ids.last().cloned();
            }

            let (whole, total, _) = scanned_monitor_page(&fixtures, paused, None, 50);
            assert_eq!(
                visited, whole,
                "a two-row walk across a three-row tie group must visit every \
                 monitor exactly once under state={paused:?}"
            );
            assert_eq!(visited.len(), total);
            assert!(
                pages > 1,
                "state={paused:?} must take more than one page, or this proves nothing"
            );
        }
    }

    #[test]
    fn a_stale_monitor_cursor_ends_pagination_instead_of_erroring() {
        let (_dir, index) = indexed_with_monitor_fixtures();

        let gone = index
            .monitor_page(&monitor_query(None, Some("mon-deleted".to_string()), 50))
            .expect("a stale cursor must not error");
        assert!(gone.ids.is_empty());
        assert_eq!(gone.total, 6, "the corpus is still the corpus");
        assert_eq!(
            gone.offset, gone.total,
            "a stale cursor resolves to the end — an empty LAST page, which is what \
             the scan's `position(…) -> None` produced"
        );

        // A cursor that names a row which EXISTS but is outside this filter is
        // stale for this filter. Resolving it against the unfiltered table
        // would resume in the middle of a list the reader is not looking at.
        let filtered_out = index
            .monitor_page(&monitor_query(
                Some(false),
                Some("mon-tie-c".to_string()),
                50,
            ))
            .expect("a cursor naming a paused monitor while filtering to active");
        assert!(filtered_out.ids.is_empty());
        assert_eq!(filtered_out.total, 3);
        assert_eq!(filtered_out.offset, 3);
    }

    #[test]
    fn a_monitor_page_stays_inside_its_scope() {
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening the index");
        seed_scope(&dir, &alpha(), &[TaskFixture::new("mon-alpha").monitor()]);
        seed_scope(&dir, &beta(), &[TaskFixture::new("mon-beta").monitor()]);
        index
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("rebuilding from disk");

        let page = index
            .monitor_page(&monitor_query(None, None, 50))
            .expect("alpha's monitors");
        assert_eq!(page.ids, vec!["mon-alpha"]);
        assert_eq!(
            page.total, 1,
            "a total that counted another principal's monitors would leak their \
             corpus size through a pager"
        );

        // And another scope's id is a stale cursor here, not a seek into it.
        let cross = index
            .monitor_page(&monitor_query(None, Some("mon-beta".to_string()), 50))
            .expect("a cursor from another scope");
        assert!(cross.ids.is_empty());
        assert_eq!(cross.offset, cross.total);
    }

    #[test]
    fn a_lane_page_and_its_total_agree_with_each_other() {
        let (_dir, index) = indexed_with_lane_fixtures();

        for lane in TaskLane::ALL {
            let page = index
                .page(&ListPageQuery::new(ListKind::Task, alpha(), 500).lane(lane, TODAY))
                .expect("paging a lane");
            assert_eq!(
                page.items.len(),
                page.total,
                "a page big enough to hold `{}` must hold exactly `total` rows",
                lane.wire_name()
            );
            assert!(!page.has_more);
            assert_eq!(page.next_cursor, None);
        }
    }

    #[test]
    fn a_page_carries_the_columns_a_lane_reads() {
        let (_dir, index) = indexed_with_lane_fixtures();

        let page = index
            .page(&ListPageQuery::new(ListKind::Task, alpha(), 1).offset(1))
            .expect("paging one row");
        let entry = page.items.first().expect("one row");

        assert_eq!(entry.id, "row-b");
        assert_eq!(entry.kind, ListKind::Task);
        assert_eq!(entry.scope, alpha());
        assert_eq!(entry.status, "running");
        assert_eq!(entry.tags, vec!["work".to_string()]);
        assert_eq!(entry.due_date.as_deref(), Some("2026-07-30T09:00:00Z"));
        assert_eq!(entry.title.as_deref(), Some("title for row-b"));
        assert_eq!(entry.lifecycle.as_deref(), Some("persistent"));
        assert!(
            !entry.paused,
            "an unscheduled row reads as active, and the column must survive the SELECT"
        );
        assert_eq!(
            entry.updated_at,
            DateTime::parse_from_rfc3339("2026-07-30T10:00:08+00:00")
                .expect("fixture timestamp")
                .timestamp_millis()
        );
    }

    // -----------------------------------------------------------------
    // Task 1 — the reconciliation pass
    //
    // Every test here introduces drift DELIBERATELY. A reconciler run
    // against a corpus that already agrees with its index proves only that
    // it did no harm, which `an_idle_pass_reads_no_records` says better.
    // -----------------------------------------------------------------

    /// The watermark a unit has stored, or `None` if it has never finished a
    /// pass.
    fn stored_watermark(index: &ListIndex, kind: UnitKind) -> Option<String> {
        let conn = index.lock();
        read_meta(&conn, &reconcile_watermark_key(kind, &alpha()))
            .expect("reading a reconciliation watermark")
    }

    /// Rewrite `state/task_state.json` and nothing else — the shape a status
    /// change actually takes on disk. The manifest is untouched, so the task
    /// directory's own mtime never moves; only the record file's does.
    ///
    /// Written through a temp file and a rename, because that is what
    /// `write_bytes_atomic` does and the gate must be tested against the
    /// writer it will really see.
    fn rewrite_task_status(dir: &TempDir, scope: &ListScope, task_id: &str, status: &str) {
        let state_path = tasks_root(&scopes_root(dir), scope)
            .join(task_id)
            .join("state")
            .join("task_state.json");
        let mut state: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&state_path).expect("reading a fixture task state"),
        )
        .expect("parsing a fixture task state");
        state["status"] = serde_json::json!(status);

        let tmp_path = state_path.with_file_name(".task_state.json.tmp");
        std::fs::write(
            &tmp_path,
            serde_json::to_string_pretty(&state).expect("serializing a fixture task state"),
        )
        .expect("writing a fixture task state");
        std::fs::rename(&tmp_path, &state_path).expect("renaming a fixture task state into place");
    }

    #[test]
    fn a_pass_restores_a_row_whose_record_is_still_on_disk() {
        let (dir, index) = indexed_with_lane_fixtures();

        // Proven, not assumed. Every assertion below is a tautology over a
        // row that was never in the index to begin with.
        assert!(
            index
                .remove(ListKind::Task, "row-c")
                .expect("removing an indexed row"),
            "the fixture must have indexed `row-c`, or this test asserts nothing"
        );
        assert!(!indexed_ids(&index, ListKind::Task).contains(&"row-c".to_string()));

        let report = index
            .reconcile_from_disk(&scopes_root(&dir))
            .expect("reconciling");

        assert_eq!(
            report.repaired, 1,
            "exactly the missing row is rewritten; the other seven already agreed with disk"
        );
        assert_eq!(report.removed, 0, "nothing left the index");
        assert!(indexed_ids(&index, ListKind::Task).contains(&"row-c".to_string()));
        assert_eq!(
            indexed_column(&index, ListKind::Task, "row-c", "title"),
            "title for row-c",
            "the restored row carries the record's columns, not an empty placeholder"
        );
    }

    /// **The bug class the whole pass exists for.** The id is unchanged and
    /// the status is stale, which an id-set comparison could never see.
    ///
    /// The first pass here is load-bearing: it sets the watermark, so the
    /// correction has to be found by the mtime gate rather than by a read of
    /// everything. Without it the test would pass against a gate that only
    /// stats the task directory — which a status write never touches.
    #[test]
    fn a_pass_corrects_a_status_changed_behind_the_indexs_back() {
        let (dir, index) = indexed_with_lane_fixtures();
        assert_eq!(
            indexed_column(&index, ListKind::Task, "row-b", "status"),
            "running"
        );

        index
            .reconcile_from_disk(&scopes_root(&dir))
            .expect("a first pass, to set the watermark");
        rewrite_task_status(&dir, &alpha(), "row-b", "completed");

        let report = index
            .reconcile_from_disk(&scopes_root(&dir))
            .expect("reconciling");

        assert_eq!(
            report.records_read, 1,
            "only the record whose file moved is re-read"
        );
        assert_eq!(report.repaired, 1);
        assert_eq!(
            indexed_column(&index, ListKind::Task, "row-b", "status"),
            "completed",
            "a status that changed under a stable id must reach the index"
        );
    }

    #[test]
    fn a_pass_drops_a_row_whose_directory_is_gone() {
        let (dir, index) = indexed_with_lane_fixtures();
        assert!(indexed_ids(&index, ListKind::Task).contains(&"row-e".to_string()));

        std::fs::remove_dir_all(tasks_root(&scopes_root(&dir), &alpha()).join("row-e"))
            .expect("removing a fixture task directory");

        let report = index
            .reconcile_from_disk(&scopes_root(&dir))
            .expect("reconciling");

        assert_eq!(report.removed, 1);
        assert_eq!(
            report.repaired, 0,
            "a record that is merely gone is removed, never rewritten"
        );
        assert!(!indexed_ids(&index, ListKind::Task).contains(&"row-e".to_string()));
        // A sweep that emptied the unit would satisfy the assertion above.
        assert_eq!(
            indexed_ids(&index, ListKind::Task).len(),
            lane_fixtures().len() - 1,
            "only the row whose directory went away leaves"
        );
    }

    /// **The headline case: a record that appears AFTER a watermark.** A
    /// writer nobody hooked lands a task record and the index never hears —
    /// the entire reason this pass exists.
    ///
    /// The first pass is what makes it a test of the gate rather than of the
    /// walk. Without it the watermark is 0, everything is re-read, and an
    /// implementation that stats only the entries it already has rows for
    /// would still pass.
    #[test]
    fn a_pass_sees_a_record_created_after_the_watermark() {
        let (dir, index) = indexed_with_lane_fixtures();

        index
            .reconcile_from_disk(&scopes_root(&dir))
            .expect("a first pass, to set the watermark");
        assert!(!indexed_ids(&index, ListKind::Task).contains(&"row-new".to_string()));

        // Straight to disk, with nothing telling the index about it.
        write_task(
            &tasks_root(&scopes_root(&dir), &alpha()),
            &alpha(),
            &TaskFixture::new("row-new").status("running"),
        );

        let report = index
            .reconcile_from_disk(&scopes_root(&dir))
            .expect("reconciling");

        assert_eq!(
            report.records_read, 1,
            "only the new record is read; the rest are older than the watermark"
        );
        assert_eq!(report.repaired, 1);
        assert_eq!(report.removed, 0);
        assert!(
            indexed_ids(&index, ListKind::Task).contains(&"row-new".to_string()),
            "a record no writer announced must still reach the index"
        );
        assert_eq!(
            indexed_column(&index, ListKind::Task, "row-new", "status"),
            "running",
            "and it must arrive with its columns, not as a bare id"
        );
    }

    /// The `>=` in `file_touched_since`, pinned.
    ///
    /// A file written in the same tick as the pass that set the watermark
    /// lands with an mtime EQUAL to it. Under `>` that record is skipped —
    /// and skipped forever, because every later watermark is larger still.
    /// Setting the watermark to the record's own mtime is how that tie is
    /// reached deterministically; waiting for one to happen is not.
    ///
    /// **The NEWER of the two record files, which is the whole test.** Anchor
    /// on `manifest.json` and the tie is irrelevant: `task_state.json` is
    /// written after it, so the record reads as touched through the other
    /// file and the assertion holds under `>` as well. Verified by mutation —
    /// the first version of this test passed against `>`.
    #[test]
    fn the_gate_re_reads_a_file_written_in_the_watermarks_own_tick() {
        let (dir, index) = indexed_with(&[TaskFixture::new("row-tie")]);
        let task_dir = tasks_root(&scopes_root(&dir), &alpha()).join("row-tie");

        let file_mtime = |path: PathBuf| {
            std::fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .expect("reading a fixture record file's mtime")
        };
        let mtime =
            file_mtime(manifest_path(&task_dir)).max(file_mtime(task_state_path(&task_dir)));
        {
            let conn = index.lock();
            write_meta(
                &conn,
                &reconcile_watermark_key(UnitKind::Task, &alpha()),
                &nanos_since_epoch(mtime).to_string(),
            )
            .expect("planting a watermark at the record's own mtime");
        }

        let report = index
            .reconcile_from_disk(&scopes_root(&dir))
            .expect("reconciling");

        assert_eq!(
            report.records_read, 1,
            "a record whose file shares the watermark's tick must be re-read, or it never is again"
        );
    }

    /// **A rebuild invalidates every watermark, and must say so.** A rebuild
    /// is what an operator runs when the index was wrong; a watermark that
    /// survived it would tell the next pass to skip exactly the records the
    /// rebuild was run to fix.
    #[test]
    fn a_completed_rebuild_clears_the_watermarks_it_invalidates() {
        let (dir, index) = indexed_with_lane_fixtures();

        index
            .reconcile_from_disk(&scopes_root(&dir))
            .expect("a pass, to set watermarks");
        assert!(
            stored_watermark(&index, UnitKind::Task).is_some(),
            "the pass must have stored one, or the assertion below is vacuous"
        );

        index
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("rebuilding from disk");

        assert_eq!(
            stored_watermark(&index, UnitKind::Task),
            None,
            "a completed rebuild drops the watermarks that describe the rows it replaced"
        );
        assert_eq!(
            index
                .reconcile_from_disk(&scopes_root(&dir))
                .expect("a pass after the rebuild")
                .records_read,
            lane_fixtures().len(),
            "so the next pass reads the corpus once rather than trusting a stale mark"
        );
    }

    /// **`UnitKind::row_kinds`, pinned through the rebuild.** Narrowing the
    /// tasks unit to `&[ListKind::Task]` compiles fine — the return type does
    /// not change — and every other rebuild test stays green, because they run
    /// one rebuild against an empty index where `commit_unit`'s
    /// DELETE-before-insert is a no-op. This is the one that needs the second
    /// rebuild to have something to clear.
    #[test]
    fn a_rebuild_clears_the_monitor_row_of_a_task_that_stopped_being_one() {
        let (dir, index) = indexed_with(&[TaskFixture::new("row-watch").monitor()]);
        assert_eq!(
            indexed_ids(&index, ListKind::Monitor),
            vec!["row-watch".to_string()],
            "the first rebuild must have produced a monitor row, or there is nothing to clear"
        );

        // Same task, same directory, manifest rewritten without a
        // `monitor_spec`. Only the monitor row is now wrong.
        write_task(
            &tasks_root(&scopes_root(&dir), &alpha()),
            &alpha(),
            &TaskFixture::new("row-watch"),
        );
        index
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("a second rebuild");

        assert!(
            indexed_ids(&index, ListKind::Monitor).is_empty(),
            "a task that stopped being a monitor must stop listing as one"
        );
        assert_eq!(
            indexed_ids(&index, ListKind::Task),
            vec!["row-watch".to_string()],
            "while the task itself stays indexed"
        );
    }

    /// **The property the skip policy rests on.** A unit that could not be
    /// walked must cost only its own rows: everything already finished keeps
    /// its progress row, so the retry resumes instead of starting over.
    ///
    /// `enumerate_scopes` sorts, so locking `beta`'s tasks root deterministically
    /// puts the failure after `alpha`'s two units have committed. The internal
    /// unit also reads legacy App tasks from the tasks root. Locking the FIRST unit's root — which this test
    /// used to do — proves nothing: no unit has finished yet, so the progress
    /// count is 0 whether the policy resumes or restarts.
    #[cfg(unix)]
    #[test]
    fn a_rebuild_that_skipped_a_unit_resumes_the_rest_instead_of_restarting() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening the index");
        seed_scope(&dir, &alpha(), &[TaskFixture::new("task-alpha")]);
        seed_scope(&dir, &beta(), &[TaskFixture::new("task-beta")]);

        let root = tasks_root(&scopes_root(&dir), &beta());
        let restore = std::fs::metadata(&root)
            .expect("reading beta's tasks root mode")
            .permissions();
        let mut locked = restore.clone();
        locked.set_mode(0o000);
        std::fs::set_permissions(&root, locked).expect("locking beta's tasks root");
        let readable_anyway = std::fs::read_dir(&root).is_ok();

        let first = index.rebuild_from_disk(&scopes_root(&dir));
        std::fs::set_permissions(&root, restore).expect("restoring beta's tasks root");
        let first = first.expect("one unwalkable unit must not fail the whole rebuild");

        assert!(
            !readable_anyway,
            "mode 0o000 did not deny this process — running as root defeats the whole test"
        );
        assert_eq!(
            first.units_failed, 2,
            "both beta units depend on its tasks root"
        );
        assert_eq!(first.units_walked, 2, "alpha's two units still walked");
        assert!(
            !index.is_ready().expect("readiness"),
            "an incomplete corpus must not be served, however much of it landed"
        );
        assert_eq!(
            index.rebuild_progress_units().expect("progress"),
            2,
            "every unit that finished keeps its progress row — this is what the retry resumes from"
        );
        assert_eq!(
            indexed_ids(&index, ListKind::Task),
            vec!["task-alpha".to_string()],
            "and the work that succeeded is committed, not discarded"
        );

        let second = index
            .rebuild_from_disk(&scopes_root(&dir))
            .expect("the retry");

        assert_eq!(
            second.units_resumed, 2,
            "the retry trusts the two finished units rather than re-walking them"
        );
        assert_eq!(second.units_walked, 2, "and walks only the two that failed");
        assert_eq!(second.units_failed, 0);
        assert!(
            index.is_ready().expect("readiness"),
            "with nothing skipped, the retry completes and the index becomes servable"
        );
        assert_eq!(
            indexed_ids(&index, ListKind::Task),
            vec!["task-alpha".to_string(), "task-beta".to_string()]
        );
    }

    /// The rebuild's half of the unreadable-root policy, in the case where
    /// nothing else succeeded. `commit_unit` clears a unit's rows before
    /// inserting and then records progress, so a walk that read an unreadable
    /// root as empty would write an EMPTY scope and mark it done — and a
    /// completed rebuild is also what clears the reconciler's watermarks, so
    /// nothing would ever re-read it.
    #[cfg(unix)]
    #[test]
    fn a_rebuild_over_an_unreadable_root_skips_it_rather_than_indexing_an_empty_scope() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening the index");
        seed_scope(&dir, &alpha(), &lane_fixtures());
        let root = tasks_root(&scopes_root(&dir), &alpha());

        let restore = std::fs::metadata(&root)
            .expect("reading the fixture root's mode")
            .permissions();
        let mut locked = restore.clone();
        locked.set_mode(0o000);
        std::fs::set_permissions(&root, locked).expect("locking the fixture root");
        let readable_anyway = std::fs::read_dir(&root).is_ok();

        let outcome = index.rebuild_from_disk(&scopes_root(&dir));
        std::fs::set_permissions(&root, restore).expect("restoring the fixture root");

        assert!(
            !readable_anyway,
            "mode 0o000 did not deny this process — running as root defeats the whole test"
        );
        let outcome = outcome.expect("an unwalkable unit is skipped, not fatal");
        assert_eq!(outcome.units_failed, 2);
        assert!(
            indexed_ids(&index, ListKind::Task).is_empty(),
            "the unit must not have committed a scope's worth of nothing"
        );
        assert_eq!(
            index.rebuild_progress_units().expect("progress"),
            0,
            "neither unit can finish while the shared legacy tasks root is unreadable"
        );
        assert!(
            !index.is_ready().expect("readiness"),
            "the index stays unready, so readers keep using the file walk"
        );
    }

    /// **An unreadable root is not an empty one.** `scan.named` drives the
    /// orphan sweep, so a root that momentarily cannot be read would, if it
    /// read as empty, delete every row in the scope AND advance the watermark
    /// past the files it just orphaned — after which the mtime gate never
    /// looks at them again and the scope stays missing until a full rebuild.
    /// That is the `is_ready()` guard's failure arriving through another door,
    /// and it is worse than the drift this pass exists to repair.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_unit_root_neither_sweeps_nor_advances_its_watermark() {
        use std::os::unix::fs::PermissionsExt;

        let (dir, index) = indexed_with_lane_fixtures();
        let root = tasks_root(&scopes_root(&dir), &alpha());
        let watermark = || stored_watermark(&index, UnitKind::Task);

        index
            .reconcile_from_disk(&scopes_root(&dir))
            .expect("a first pass, to set the watermark");
        let before = watermark();
        assert!(
            before.is_some(),
            "the first pass must have set a watermark, or the comparison below is vacuous"
        );

        // Drift introduced BEFORE the lock, so the unmoved watermark has a
        // consequence to demonstrate. Asserting the stored value alone proves
        // little: it is already newer than every fixture mtime, so a later
        // pass would read nothing whether the watermark moved or not.
        rewrite_task_status(&dir, &alpha(), "row-b", "completed");

        let restore = std::fs::metadata(&root)
            .expect("reading the fixture root's mode")
            .permissions();
        let mut locked = restore.clone();
        locked.set_mode(0o000);
        std::fs::set_permissions(&root, locked).expect("locking the fixture root");
        let readable_anyway = std::fs::read_dir(&root).is_ok();

        let report = index.reconcile_from_disk(&scopes_root(&dir));

        // Before any assertion, or a failure leaves a directory the temp dir
        // cannot clean up.
        std::fs::set_permissions(&root, restore).expect("restoring the fixture root");
        let report = report.expect("a failing unit must not fail the pass");

        assert!(
            !readable_anyway,
            "mode 0o000 did not deny this process — running as root defeats the whole test"
        );
        assert_eq!(
            report.removed, 0,
            "a root the pass could not read must not be treated as a root with nothing in it"
        );
        assert_eq!(
            indexed_ids(&index, ListKind::Task).len(),
            lane_fixtures().len(),
            "every row survives"
        );
        assert_eq!(
            report.units, 0,
            "both units depend on the unreadable legacy tasks root"
        );
        assert_eq!(
            report.units_failed, 2,
            "and the failure is a number the report carries, not only a log line"
        );
        assert_eq!(
            watermark(),
            before,
            "a failed unit leaves its watermark where it was, so the next pass retries the span"
        );

        // The consequence, which is the whole point of not advancing it.
        let retry = index
            .reconcile_from_disk(&scopes_root(&dir))
            .expect("the pass after the root became readable again");
        assert_eq!(
            retry.records_read, 1,
            "the span the failed pass never scanned is re-scanned, and the drift inside it found"
        );
        assert_eq!(retry.repaired, 1);
        assert_eq!(
            indexed_column(&index, ListKind::Task, "row-b", "status"),
            "completed",
            "a change made while the root was unreadable still reaches the index"
        );
    }

    /// A monitor owns TWO rows and both belong to the tasks unit, so the
    /// sweep has to reach past its own `kind`.
    ///
    /// Its own corpus rather than a fixture added to `lane_fixtures()`, whose
    /// membership several lane tests spell out by id. Without this test,
    /// narrowing `unit_row_kinds` to `&[ListKind::Task]` leaves the whole
    /// suite green and `/monitors` lists a task whose directory is gone.
    #[test]
    fn a_pass_drops_both_rows_of_a_monitor_whose_directory_is_gone() {
        let (dir, index) = indexed_with(&[
            TaskFixture::new("row-watch").monitor(),
            TaskFixture::new("row-plain"),
        ]);

        assert_eq!(
            indexed_ids(&index, ListKind::Monitor),
            vec!["row-watch".to_string()],
            "the fixture must carry a monitor row, or the pairing this test exists for is absent"
        );

        std::fs::remove_dir_all(tasks_root(&scopes_root(&dir), &alpha()).join("row-watch"))
            .expect("removing the monitor's task directory");

        let report = index
            .reconcile_from_disk(&scopes_root(&dir))
            .expect("reconciling");

        assert_eq!(
            report.removed, 2,
            "both rows go, and `removed` counts ROWS rather than records"
        );
        assert!(
            indexed_ids(&index, ListKind::Monitor).is_empty(),
            "a monitor row outliving its task is a `/monitors` entry for a task that is gone"
        );
        assert_eq!(
            indexed_ids(&index, ListKind::Task),
            vec!["row-plain".to_string()],
            "the surviving task keeps its row"
        );
    }

    /// **The design invariant, not a performance test.** A reconciler that
    /// quietly degrades into a full walk is still perfectly correct, and this
    /// is the exact failure mode the watermark exists to avoid — nothing else
    /// in this suite would ever notice it.
    #[test]
    fn an_idle_pass_reads_no_records() {
        let (dir, index) = indexed_with_lane_fixtures();

        let first = index
            .reconcile_from_disk(&scopes_root(&dir))
            .expect("a first pass");
        assert_eq!(
            first.records_read,
            lane_fixtures().len(),
            "a unit with no watermark reads every record once"
        );
        assert_eq!(
            first.repaired, 0,
            "a corpus the rebuild just indexed has nothing to repair"
        );
        assert_eq!(
            first.units, 2,
            "one scope, its `tasks/` root and its absent `internal_tasks/` root"
        );

        let second = index
            .reconcile_from_disk(&scopes_root(&dir))
            .expect("a second pass");

        assert_eq!(
            second,
            ReconcileReport {
                units: 2,
                units_failed: 0,
                records_read: 0,
                repaired: 0,
                removed: 0,
            },
            "an unchanged corpus must cost stats, not reads"
        );
    }

    #[test]
    fn a_pass_declines_to_reconcile_a_half_built_index() {
        let dir = TempDir::new().expect("temp dir");
        let index = ListIndex::open_at(&index_path(&dir)).expect("opening the index");
        seed_scope(&dir, &alpha(), &lane_fixtures());

        // Never rebuilt, so this index holds nothing and `is_ready` is false.
        // Its rows are not WRONG, they are ABSENT — and a pass that reconciled
        // against them would advance a watermark past records the rebuild has
        // yet to reach, which is how a half-built index becomes a permanently
        // incomplete one.
        assert!(!index.is_ready().expect("readiness"));

        let report = index
            .reconcile_from_disk(&scopes_root(&dir))
            .expect("reconciling");

        assert_eq!(report, ReconcileReport::default());
        assert!(
            indexed_ids(&index, ListKind::Task).is_empty(),
            "the pass must not have written rows either"
        );
    }
}
