//! SQLite persistence for the Proactive Resurfacing Engine.
//!
//! [`ResurfacingStore`] mirrors the `mail_assist` store's conventions
//! (`docs/plans/2026-07-07-proactive-resurfacing-implementation.md`, Task 2):
//!
//! * **Async surface** — like the comms crate's channel-assist store, every
//!   method is `pub async fn` and runs its blocking SQL on `spawn_blocking`,
//!   so a poll loop never stalls the tokio runtime. `open`/`open_in_temp`
//!   stay sync, exactly as the mail store's `open` does.
//! * **Column scoping** — the mail store scopes rows by `principal`/`workspace`
//!   columns with composite primary keys; this store carries the same scope
//!   columns on every table so a single db file holds all scopes. (The mail
//!   store ALSO keeps one DuckDB file per scope with cross-process file locks;
//!   that machinery is DuckDB-concurrency-specific and out of scope for the
//!   four methods here, so this store uses a single rusqlite file.)
//! * **JSON payloads** — the signal bundle is stored as a serde_json string,
//!   the same way the mail store persists its JSON columns.
//! * **Shared column list** — one `CANDIDATE_COLUMNS` constant feeds both the
//!   reader and the positional decoder, so their orders can't drift (mirrors
//!   `THREAD_COLUMNS`/`MESSAGE_COLUMNS`).
//!
//! Scope discipline: Task 2 landed `open`/`open_in_temp`/`upsert_candidate`/
//! `get_candidate`; Task 3 adds the `resurfacing_watermarks` accessors
//! (`get_watermark`/`set_watermark`), the top-candidate query
//! (`list_top_candidates`), and `decay_all`. Task 7 adds the surfacing +
//! feedback state-machine accessors (`mark_surfaced`, `record_action`) that
//! write the `resurfacing_feedback` table created here. Phase 3 / P3a adds the
//! durable `resurfacing_dismissed_signals` table + `record_dismissed_signal`/
//! `list_dismissed_signals` — a cross-session record of dismissed items'
//! embeddings that the scorer penalizes new candidates against (the durable
//! successor to P2g's in-session `suppress_neighbors`). Phase 3 / P3b adds
//! [`retention_sweep`](ResurfacingStore::retention_sweep) — a per-scope
//! housekeeping prune (age-out terminal candidates, cap scope size, age-out
//! dismissed AND affinity signals, clear orphaned embedding/phrasing rows) that
//! keeps the store bounded over time. Phase 4 / P4a adds the symmetric POSITIVE
//! mirror: a durable `resurfacing_affinity_signals` table +
//! `record_affinity_signal`/`list_affinity_signals` — a cross-session record of
//! OPENED/ACKNOWLEDGED items' embeddings that the scorer BOOSTS new candidates
//! against (the positive twin of the dismissed-signal penalty). The
//! `Open`/`Acknowledge` arm of [`record_action`](ResurfacingStore::record_action)
//! captures the acted item's embedding into it, exactly as `Dismiss` captures a
//! dismissed signal.
//!
//! Phase 4 / P4c adds a per-lane **utility signal**: a
//! `resurfacing_kind_engagement` counter table (positive/negative tallies keyed
//! by `source_kind`) that [`record_action`](ResurfacingStore::record_action)
//! bumps inside its transaction — `Open` → `positive`, `Acknowledge` → neutral
//! ("seen", no signal), `Dismiss` → `negative` unless a "done" reason
//! (already_handled/duplicate/delegated) keeps it neutral.
//! [`kind_engagement`](ResurfacingStore::kind_engagement) reads it
//! back so the scorer can down-weight lanes the owner keeps dismissing and
//! up-weight lanes they engage with (Laplace-smoothed so a cold-start lane is
//! neutral). Unlike the embedding-keyed dismissed/affinity signals, this counter
//! is lane-based and embedding-independent, and it lives in its own table so it
//! survives the acted candidate being pruned.
//!
//! Future legibility (NOT built here): a `user.resurfacing_feedback` text tier
//! that LLM-distills these dismissed signals into a human-readable "you tend to
//! dismiss X" note. The durable embeddings + score penalty ARE the functional
//! learning; the tier text is a separate legibility concern.

use std::{
    path::Path,
    str::FromStr,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};

use super::centrality::{cosine, normalize};
use super::interaction::{
    ResurfacingActionKind, ResurfacingRecommendation, ResurfacingRecommendationSource,
};
use super::types::{Candidate, CandidateState, DismissReason, FeedbackAction, SourceKind};

#[path = "store_connections.rs"]
mod connections;

/// Cosine similarity at/above which a still-`candidate` row is treated as a
/// near-duplicate of a just-dismissed item and down-weighted. High enough
/// (0.82) that only genuinely close semantic neighbors are suppressed, so a
/// dismiss teaches "not this, and not the ones just like it" without silencing
/// merely-adjacent context. Tunable; a later config layer can override it.
const SUPPRESS_THRESHOLD: f32 = 0.82;

/// Multiplicative salience penalty applied to each near-neighbor of a dismissed
/// item (`salience_score *= SUPPRESS_FACTOR`). Halving (0.5) makes a neighbor
/// meaningfully less likely to be surfaced by the curator's top-`cap` pick
/// WITHOUT changing its state or cooldown — it stays a live candidate that can
/// re-earn salience on a future scan.
const SUPPRESS_FACTOR: f32 = 0.5;

/// Max durable dismissed-signal rows retained PER SCOPE in
/// `resurfacing_dismissed_signals`. P2g's `suppress_neighbors` only teaches the
/// *currently loaded* candidates and forgets once they churn; P3a persists each
/// dismissed item's embedding so future NEW candidates resembling a past
/// dismissal are penalized at score time (cross-session learning). This table is
/// append-only, so we bound it to the newest `DISMISSED_SIGNAL_CAP` rows per
/// scope — a large-enough recent window to represent "the kinds of things you
/// keep dismissing" without unbounded growth for a heavy dismisser. Tunable; a
/// later config layer can override it.
const DISMISSED_SIGNAL_CAP: usize = 200;

/// Max durable affinity-signal rows retained PER SCOPE in
/// `resurfacing_affinity_signals`. The positive mirror of
/// [`DISMISSED_SIGNAL_CAP`]: P4a persists each opened/acknowledged item's
/// embedding so future NEW candidates resembling a past POSITIVE action are
/// boosted at score time (cross-session learning of "more of this kind"). Like
/// the dismissed table it's append-only, so we bound it to the newest
/// `AFFINITY_SIGNAL_CAP` rows per scope — a large-enough recent window to
/// represent "the kinds of things you keep engaging with" without unbounded
/// growth. Tunable; a later config layer can override it.
const AFFINITY_SIGNAL_CAP: usize = 200;
const ATTENTION_FEEDBACK_SEMANTIC_TEXT_MAX_CHARS: usize = 8_192;

/// Provenance-preserving semantic snapshot written atomically with an exact
/// owner action. Historical attention bootstrap consumes these rows without
/// attempting to reconstruct a deleted candidate or a lost dismiss reason.
#[derive(Debug, Clone, PartialEq)]
pub struct ResurfacingAttentionSignal {
    pub id: i64,
    pub embedding_contract: String,
    pub embedding: Vec<f32>,
    /// Epoch seconds, matching the resurfacing feedback clock.
    pub occurred_at: i64,
    /// Exact post-activation source event, when this signal was atomically
    /// emitted with a live feedback outbox row.
    pub live_event_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResurfacingFeedbackRepairRow {
    pub id: i64,
    pub event_id: String,
    pub candidate_id: String,
    /// None preserves a malformed durable row so the repair worker can record
    /// a skip and advance its cursor instead of stalling the entire tail.
    pub action: Option<FeedbackAction>,
    pub reason: Option<DismissReason>,
    pub semantic_text: String,
    pub embedding_contract: Option<String>,
    pub embedding: Option<Vec<f32>>,
    pub occurred_at: i64,
}

/// Max run-history rows retained PER SCOPE in `resurfacing_runs` (O1
/// observability). Every background pass (scorer / curator / retention) appends
/// one record so we can see what the engine did, when, how long it took, and
/// whether it succeeded; the table is append-only, so we bound it to the newest
/// `RUN_HISTORY_CAP` rows per scope — a large-enough recent window to inspect
/// engine health without unbounded growth over a long uptime. Tunable; a later
/// config layer can override it.
const RUN_HISTORY_CAP: usize = 500;
/// Maximum number of digest/contract-qualified embeddings that may exist
/// before their candidate row is committed. Cancellation can legitimately
/// leave this cache ahead of the scorer, but it must never grow without bound.
const UNATTACHED_EMBEDDING_CACHE_CAP: usize = 512;
const EMBEDDING_SNAPSHOT_SQL_CHUNK_SIZE: usize = 128;

/// Bootstrap DDL for the three resurfacing tables. Everything is
/// `IF NOT EXISTS` so re-opening an existing db is idempotent.
const BOOTSTRAP_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS resurfacing_memory_connections (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    candidate_id TEXT NOT NULL,
    attempted_at INTEGER NOT NULL,
    state TEXT NOT NULL,
    record_json TEXT NOT NULL,
    PRIMARY KEY (principal, workspace, candidate_id)
);
CREATE INDEX IF NOT EXISTS idx_memory_connections_active
    ON resurfacing_memory_connections (principal, workspace, state, attempted_at);
CREATE TABLE IF NOT EXISTS resurfacing_candidates (
    candidate_id TEXT NOT NULL,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    source_ref TEXT NOT NULL,
    title TEXT NOT NULL,
    content_digest TEXT NOT NULL,
    content_details_json TEXT,
    content_revision TEXT,
    semantic_features_json TEXT,
    salience_score REAL NOT NULL,
    signals TEXT NOT NULL,
    temporal_anchor_at INTEGER,
    embedding_id TEXT,
    state TEXT NOT NULL,
    first_seen_at INTEGER NOT NULL,
    last_scored_at INTEGER NOT NULL,
    last_surfaced_at INTEGER,
    cooldown_until INTEGER NOT NULL,
    surface_count INTEGER NOT NULL,
    dismiss_count INTEGER NOT NULL,
    PRIMARY KEY (principal, workspace, candidate_id)
);
CREATE TABLE IF NOT EXISTS resurfacing_watermarks (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    corpus_kind TEXT NOT NULL,
    cursor INTEGER NOT NULL,
    PRIMARY KEY (principal, workspace, corpus_kind)
);
CREATE TABLE IF NOT EXISTS resurfacing_feedback (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    candidate_id TEXT NOT NULL,
    action TEXT NOT NULL,
    event_id TEXT,
    reason TEXT,
    semantic_text TEXT,
    embedding_contract TEXT,
    vec TEXT,
    at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS resurfacing_phrasing (
    candidate_id TEXT NOT NULL,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    line TEXT NOT NULL,
    why TEXT NOT NULL,
    content_revision TEXT,
    at INTEGER NOT NULL,
    PRIMARY KEY (principal, workspace, candidate_id)
);
CREATE TABLE IF NOT EXISTS resurfacing_recommendations (
    candidate_id TEXT NOT NULL,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    action_kind TEXT NOT NULL,
    label TEXT NOT NULL,
    rationale TEXT NOT NULL,
    confidence REAL NOT NULL,
    source TEXT NOT NULL,
    content_revision TEXT,
    shown_at INTEGER,
    at INTEGER NOT NULL,
    PRIMARY KEY (principal, workspace, candidate_id)
);
CREATE TABLE IF NOT EXISTS resurfacing_embeddings (
    candidate_id TEXT NOT NULL,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    content_digest TEXT,
    embedding_contract TEXT,
    vec TEXT NOT NULL,
    updated_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (principal, workspace, candidate_id)
);
CREATE TABLE IF NOT EXISTS resurfacing_embedding_cache (
    candidate_id TEXT NOT NULL,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    content_digest TEXT NOT NULL,
    embedding_contract TEXT NOT NULL,
    vec TEXT NOT NULL,
    updated_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (principal, workspace, candidate_id, content_digest, embedding_contract)
);
CREATE INDEX IF NOT EXISTS idx_resurfacing_embedding_cache_updated
    ON resurfacing_embedding_cache(principal, workspace, updated_at DESC);
CREATE TABLE IF NOT EXISTS resurfacing_dismissed_signals (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    event_id TEXT,
    embedding_contract TEXT,
    vec TEXT NOT NULL,
    at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS resurfacing_affinity_signals (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    event_id TEXT,
    embedding_contract TEXT,
    vec TEXT NOT NULL,
    at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS resurfacing_kind_engagement (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    source_kind TEXT NOT NULL,
    positive INTEGER NOT NULL DEFAULT 0,
    negative INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (principal, workspace, source_kind)
);
CREATE TABLE IF NOT EXISTS resurfacing_runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    kind TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    duration_ms INTEGER NOT NULL,
    produced INTEGER NOT NULL,
    success INTEGER NOT NULL,
    error TEXT,
    at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS resurfacing_action_claims (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    candidate_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    action_kind TEXT NOT NULL,
    input_hash TEXT NOT NULL,
    content_revision TEXT,
    -- Which lane `candidate_id` names. Existing rows predate the follow-up lane
    -- and are all resurfacing candidates, which is why the default backfills
    -- them correctly.
    target_kind TEXT NOT NULL DEFAULT 'resurfacing_candidate',
    state TEXT NOT NULL,
    result_ref TEXT NOT NULL,
    result_json TEXT,
    error_class TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (principal, workspace, candidate_id, idempotency_key)
);
CREATE TABLE IF NOT EXISTS resurfacing_action_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    candidate_id TEXT NOT NULL,
    action_kind TEXT NOT NULL,
    recommendation_kind TEXT,
    content_revision TEXT,
    event_type TEXT NOT NULL,
    result_ref TEXT,
    error_class TEXT,
    at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_resurfacing_action_events_scope_at
    ON resurfacing_action_events (principal, workspace, at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_resurfacing_recommendations_scope_kind
    ON resurfacing_recommendations (principal, workspace, action_kind);
CREATE TABLE IF NOT EXISTS resurfacing_memory_applications (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    candidate_id TEXT NOT NULL,
    content_revision TEXT NOT NULL,
    memory_revision TEXT NOT NULL,
    judgement_json TEXT NOT NULL,
    recorded_at INTEGER NOT NULL,
    PRIMARY KEY (principal, workspace, candidate_id, content_revision, memory_revision)
);
CREATE INDEX IF NOT EXISTS idx_resurfacing_memory_applications_scope_at
    ON resurfacing_memory_applications (principal, workspace, recorded_at DESC);
CREATE TABLE IF NOT EXISTS resurfacing_routing_repairs (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    candidate_id TEXT NOT NULL,
    content_revision TEXT NOT NULL,
    outcome TEXT NOT NULL,
    repaired_at INTEGER NOT NULL,
    PRIMARY KEY (principal, workspace, candidate_id, content_revision)
);
CREATE INDEX IF NOT EXISTS idx_resurfacing_routing_repairs_scope_at
    ON resurfacing_routing_repairs (principal, workspace, repaired_at DESC);
-- The surfaced lane's exact read order, so its page is a seek instead of a
-- sort. Without it the query plan is SCAN + USE TEMP B-TREE FOR ORDER BY: the
-- whole scope is read and sorted to return eight rows, and every later page
-- pays for it again. The column order is the ORDER BY verbatim, expression and
-- directions included, because an index that differs from it in any of those
-- cannot serve it. Applied by BOOTSTRAP_DDL on open, so existing databases
-- pick it up with no migration.
CREATE INDEX IF NOT EXISTS idx_resurfacing_candidates_surfaced_keyset
    ON resurfacing_candidates (
        principal, workspace, state,
        COALESCE(last_surfaced_at, 0) DESC, salience_score DESC, candidate_id ASC
    );
"#;

/// Shared SELECT list — one column order for every candidate reader so the
/// positional [`map_candidate_row`] decoder can't drift. New columns append at
/// the end (and get a matching `row.get` index).
const CANDIDATE_COLUMNS: &str = "candidate_id, source_kind, source_ref, title, \
     content_digest, salience_score, signals, temporal_anchor_at, embedding_id, \
     state, first_seen_at, last_scored_at, last_surfaced_at, cooldown_until, \
     surface_count, dismiss_count, content_details_json, content_revision, \
     semantic_features_json";

fn update_generation_token_value(
    digest: &mut blake3::Hasher,
    value: rusqlite::types::ValueRef<'_>,
) {
    use rusqlite::types::ValueRef;

    match value {
        ValueRef::Null => {
            digest.update(&[0]);
        },
        ValueRef::Integer(value) => {
            digest.update(&[1]);
            digest.update(&value.to_le_bytes());
        },
        ValueRef::Real(value) => {
            digest.update(&[2]);
            digest.update(&value.to_bits().to_le_bytes());
        },
        ValueRef::Text(value) => {
            digest.update(&[3]);
            digest.update(&(value.len() as u64).to_le_bytes());
            digest.update(value);
        },
        ValueRef::Blob(value) => {
            digest.update(&[4]);
            digest.update(&(value.len() as u64).to_le_bytes());
            digest.update(value);
        },
    };
}

/// Per-table row counts for a scope (O2 observability), so the observability
/// endpoint can show how big each resurfacing data plane is. Every field is a
/// scoped `COUNT(*)`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ResurfacingTableSizes {
    pub candidates: u64,
    pub embeddings: u64,
    pub embedding_cache: u64,
    pub phrasing: u64,
    pub recommendations: u64,
    pub dismissed_signals: u64,
    pub affinity_signals: u64,
    pub runs: u64,
    pub action_claims: u64,
    pub action_events: u64,
    pub routing_repairs: u64,
}

/// Digest-validated durable embedding snapshot used by background centrality.
/// The vector is reusable only when the source's current `content_digest`
/// still matches this value.
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateEmbeddingSnapshot {
    pub candidate_id: String,
    pub content_digest: String,
    pub embedding_contract: String,
    pub embedding: Vec<f32>,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct ResurfacingRecommendationEventAggregate {
    pub recommendation_kind: String,
    pub event_type: String,
    pub count: u64,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct ResurfacingActionEventAggregate {
    pub action_kind: String,
    pub event_type: String,
    pub error_class: Option<String>,
    pub count: u64,
}

#[derive(Debug, Clone, Default, serde::Serialize, PartialEq, Eq)]
pub struct ResurfacingBriefCoverage {
    pub comm_total: u64,
    pub comm_surfaced: u64,
    pub with_brief: u64,
    pub legacy: u64,
    pub complete: u64,
    pub partial: u64,
    pub source_omits_details: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResurfacingRecommendationInteractionResult {
    Recorded,
    Duplicate,
    NotShown,
    OutOfOrder,
}

/// Stored action result used to replay a completed idempotent request without
/// repeating its downstream side effect.
#[derive(Debug, Clone, PartialEq)]
pub struct ResurfacingStoredActionResult {
    pub result_ref: String,
    pub result: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResurfacingActionClaimLookup {
    Missing,
    Completed(ResurfacingStoredActionResult),
    InProgress,
    Retryable,
    Conflict,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResurfacingActionClaimBegin {
    Claimed { result_ref: String },
    Completed(ResurfacingStoredActionResult),
    InProgress,
    Conflict,
    NotFound,
    NotActionable { state: CandidateState },
    StaleRevision { current_revision: Option<String> },
}

#[derive(Debug, Clone)]
struct PersistedResurfacingActionClaim {
    action_kind: String,
    input_hash: String,
    content_revision: Option<String>,
    state: String,
    result_ref: String,
    result_json: Option<String>,
    updated_at: i64,
}

/// Live queue snapshot for a scope. Unlike the older funnel headline,
/// `pending`/`eligible` means candidates the curator can act on **now**:
/// candidate-state rows whose cooldown has elapsed. `candidate_pool` is the raw
/// candidate-state pool, and `cooling` is the candidate-state subset temporarily
/// held back by cooldown.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ResurfacingQueueStats {
    pub candidate_pool: u64,
    pub pending: u64,
    pub eligible: u64,
    pub cooling: u64,
    pub surfaced: u64,
    pub acted: u64,
    pub dismissed: u64,
    pub snoozed: u64,
}

/// Cursor for stable server-side pagination over surfaced rows.
///
/// The surfaced list is ordered by `(last_surfaced_at DESC, salience_score DESC,
/// candidate_id ASC)`. Carrying the last row's sort tuple lets the UI request
/// the next page without offset drift when a user opens/dismisses already loaded
/// cards between page fetches.
#[derive(Debug, Clone, PartialEq)]
pub struct SurfacedCursor {
    pub surfaced_at: i64,
    /// Exact SQLite REAL sort value. Candidates expose a compact `f32` score,
    /// but using that lossy projection as a cursor can round the boundary up
    /// and return the last row of the previous page again.
    pub salience_score: f64,
    pub candidate_id: String,
}

#[derive(Debug, Clone)]
pub struct SurfacedPage {
    pub candidates: Vec<Candidate>,
    pub total: u64,
    pub limit: usize,
    pub offset: usize,
    pub has_more: bool,
    pub next_cursor: Option<SurfacedCursor>,
}

/// The surfaced lane's size and the only column that can hide a row from it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SurfacedCommSourceRefs {
    /// Every surfaced candidate in the scope, communication or not.
    pub total: u64,
    /// The `source_ref` of each surfaced `comm` candidate, in no order.
    pub comm_source_refs: Vec<String>,
}

/// One background-pass run record shaped for the observability read (O2). The
/// serializable public twin of the O1 test reader's tuple; newest-first ordering
/// is applied by [`ResurfacingStore::recent_runs`].
#[derive(Debug, Clone, serde::Serialize)]
pub struct ResurfacingRun {
    pub kind: String,
    pub started_at: i64,
    pub duration_ms: i64,
    pub produced: i64,
    pub success: bool,
    pub error: Option<String>,
}

/// Per-`kind` run aggregate for the observability pipeline block (O2): how many
/// times each background pass (`scorer`/`curator`/`retention`) ran, how many
/// succeeded/failed, how much it produced in total, its average duration, when it
/// last started, and the error string of its most recent FAILING run (`None`
/// when the kind has never failed).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ResurfacingRunAggregate {
    pub kind: String,
    pub total: u64,
    pub successes: u64,
    pub failures: u64,
    pub total_produced: i64,
    pub avg_duration_ms: f64,
    pub last_started_at: i64,
    pub last_error: Option<String>,
}

/// A single-file SQLite store for resurfacing candidates, scoped by
/// `principal`/`workspace` columns. Cheap to clone (shares one connection
/// behind an `Arc<Mutex<_>>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryConflictSurface {
    pub memory_key: String,
    pub rationale: String,
    pub agree_count: u32,
    pub disagree_count: u32,
}

#[derive(Clone)]
pub struct ResurfacingStore {
    conn: Arc<Mutex<Connection>>,
}

impl ResurfacingStore {
    /// Open (creating if absent) the resurfacing db under `base_root`,
    /// enabling WAL and bootstrapping the schema. Same base-path shape as
    /// `ChannelAssistStore::open`.
    pub fn open(base_root: &Path) -> Result<Self> {
        std::fs::create_dir_all(base_root).with_context(|| {
            format!(
                "creating resurfacing store directory: {}",
                base_root.display()
            )
        })?;
        let db_path = crate::magician_v2::database_owners::host_database_path(
            base_root,
            crate::magician_v2::database_owners::DatabaseOwner::Resurfacing,
        );
        let conn = Connection::open(&db_path)
            .with_context(|| format!("opening resurfacing store at {}", db_path.display()))?;
        // journal_mode=WAL returns the resulting mode as a row; read it via
        // query_row and discard so rusqlite doesn't treat the result set as an
        // error (as a plain `execute` would).
        conn.query_row("PRAGMA journal_mode=WAL", [], |_row| Ok(()))
            .context("enabling WAL journal mode on resurfacing store")?;
        conn.execute_batch(BOOTSTRAP_DDL)
            .context("bootstrapping resurfacing store schema")?;
        ensure_column(
            &conn,
            "resurfacing_candidates",
            "content_details_json",
            "TEXT",
        )?;
        ensure_column(
            &conn,
            "resurfacing_dismissed_signals",
            "embedding_contract",
            "TEXT",
        )?;
        ensure_column(&conn, "resurfacing_dismissed_signals", "event_id", "TEXT")?;
        // Rows written before the follow-up lane existed are all resurfacing
        // candidates, so the default is a correct backfill rather than a guess.
        ensure_column(
            &conn,
            "resurfacing_action_claims",
            "target_kind",
            "TEXT NOT NULL DEFAULT 'resurfacing_candidate'",
        )?;
        ensure_column(
            &conn,
            "resurfacing_affinity_signals",
            "embedding_contract",
            "TEXT",
        )?;
        ensure_column(&conn, "resurfacing_affinity_signals", "event_id", "TEXT")?;
        ensure_column(&conn, "resurfacing_candidates", "content_revision", "TEXT")?;
        ensure_column(
            &conn,
            "resurfacing_candidates",
            "semantic_features_json",
            "TEXT",
        )?;
        ensure_column(&conn, "resurfacing_phrasing", "content_revision", "TEXT")?;
        ensure_column(&conn, "resurfacing_embeddings", "content_digest", "TEXT")?;
        ensure_column(
            &conn,
            "resurfacing_embeddings",
            "embedding_contract",
            "TEXT",
        )?;
        ensure_column(
            &conn,
            "resurfacing_embeddings",
            "updated_at",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(&conn, "resurfacing_feedback", "event_id", "TEXT")?;
        ensure_column(&conn, "resurfacing_feedback", "reason", "TEXT")?;
        ensure_column(&conn, "resurfacing_feedback", "semantic_text", "TEXT")?;
        ensure_column(&conn, "resurfacing_feedback", "embedding_contract", "TEXT")?;
        ensure_column(&conn, "resurfacing_feedback", "vec", "TEXT")?;
        conn.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS resurfacing_feedback_scope_event_idx \
             ON resurfacing_feedback(principal, workspace, event_id) \
             WHERE event_id IS NOT NULL; \
             CREATE UNIQUE INDEX IF NOT EXISTS resurfacing_dismissed_signal_scope_event_idx \
             ON resurfacing_dismissed_signals(principal, workspace, event_id) \
             WHERE event_id IS NOT NULL; \
             CREATE UNIQUE INDEX IF NOT EXISTS resurfacing_affinity_signal_scope_event_idx \
             ON resurfacing_affinity_signals(principal, workspace, event_id) \
             WHERE event_id IS NOT NULL",
        )
        .context("indexing resurfacing feedback repair events")?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Remove every row this database holds for one scope. Counterpart to the
    /// attention learning store's retirement: both are enumerated by scope
    /// discovery, so both have to forget a deleted workspace or it returns.
    /// Tables are read from the schema, not listed literally.
    pub async fn retire_scope(&self, principal: &str, workspace: &str) -> Result<u64> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let mut connection = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut names = connection
                .prepare(
                    "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
                )?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<String>, _>>()?;
            names.retain(|table| {
                let Ok(mut columns) = connection.prepare(&format!("PRAGMA table_info(\"{table}\")"))
                else {
                    return false;
                };
                let Ok(rows) = columns.query_map([], |row| row.get::<_, String>(1)) else {
                    return false;
                };
                let columns: Vec<String> = rows.flatten().collect();
                columns.iter().any(|column| column == "principal")
                    && columns.iter().any(|column| column == "workspace")
            });
            names.sort();
            let transaction = connection
                .transaction()
                .context("opening resurfacing scope retirement transaction")?;
            let mut removed = 0u64;
            for table in &names {
                removed += transaction
                    .execute(
                        &format!(
                            "DELETE FROM \"{table}\" WHERE principal = ?1 AND workspace = ?2"
                        ),
                        params![principal, workspace],
                    )
                    .with_context(|| format!("retiring scope rows from {table}"))?
                    as u64;
            }
            transaction
                .commit()
                .context("committing resurfacing scope retirement")?;
            Ok(removed)
        })
        .await
        .context("resurfacing scope retirement task panicked")?
    }

    pub async fn list_scopes(&self) -> Result<Vec<(String, String)>> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut statement = conn.prepare(
                "SELECT principal, workspace FROM resurfacing_candidates \
                 UNION SELECT principal, workspace FROM resurfacing_feedback \
                 UNION SELECT principal, workspace FROM resurfacing_dismissed_signals \
                 UNION SELECT principal, workspace FROM resurfacing_affinity_signals \
                 ORDER BY principal, workspace",
            )?;
            let scopes = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(scopes)
        })
        .await
        .context("resurfacing scope list task panicked")?
    }

    /// Insert or update a candidate keyed on its scoped
    /// `(principal, workspace, candidate_id)`. A brand-new row (no conflict)
    /// takes its full state — including lifecycle columns — from the INSERT
    /// values. On conflict, ONLY the scoring/content columns are overwritten;
    /// the lifecycle columns (`state`, `cooldown_until`, `surface_count`,
    /// `dismiss_count`, `first_seen_at`, `last_surfaced_at`) are deliberately
    /// left untouched so a concurrent `record_action` (dismiss/ack) that landed
    /// between a scorer's read and this write is never resurrected/clobbered —
    /// this is what makes concurrent feedback safe (SQLite keeps the existing
    /// value for any column absent from the `DO UPDATE SET` list).
    pub async fn upsert_candidate(
        &self,
        principal: &str,
        workspace: &str,
        candidate: &Candidate,
    ) -> Result<bool> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate = candidate.clone();
        tokio::task::spawn_blocking(move || {
            let signals_json = serde_json::to_string(&candidate.signals)
                .context("serializing resurfacing candidate signals")?;
            let content_details_json = candidate
                .content_details
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .context("serializing resurfacing candidate content details")?;
            let semantic_features_json = candidate
                .semantic_features
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .context("serializing resurfacing semantic features")?;
            // Defensively clamp a non-finite incoming score to 0.0 so an
            // inf/NaN can never enter the persisted `salience_score` and become
            // permanently top-ranked by any `ORDER BY salience_score`.
            let salience_score = if candidate.salience_score.is_finite() {
                candidate.salience_score
            } else {
                0.0
            };
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // On conflict we update ONLY the scoring/content columns. The
            // lifecycle columns (state, cooldown_until, surface_count,
            // dismiss_count, first_seen_at, last_surfaced_at) are intentionally
            // omitted from the SET list — SQLite keeps their existing values —
            // so a scorer pass that read a row BEFORE a concurrent
            // record_action(Dismiss) can't write back the stale pre-dismiss
            // state and resurrect it. This is what makes concurrent feedback
            // safe. A brand-new row still gets its lifecycle from the INSERT.
            let changed = conn
                .execute(
                    "INSERT INTO resurfacing_candidates (
                    candidate_id, principal, workspace, source_kind, source_ref,
                    title, content_digest, salience_score, signals,
                    temporal_anchor_at, embedding_id, state, first_seen_at,
                    last_scored_at, last_surfaced_at, cooldown_until,
                    surface_count, dismiss_count, content_details_json,
                    content_revision, semantic_features_json
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(principal, workspace, candidate_id) DO UPDATE SET
                    source_kind = excluded.source_kind,
                    source_ref = excluded.source_ref,
                    title = excluded.title,
                    content_digest = excluded.content_digest,
                    salience_score = excluded.salience_score,
                    signals = excluded.signals,
                    temporal_anchor_at = excluded.temporal_anchor_at,
                    embedding_id = excluded.embedding_id,
                    content_details_json = excluded.content_details_json,
                    semantic_features_json = CASE
                        WHEN resurfacing_candidates.content_revision IS excluded.content_revision
                        THEN resurfacing_candidates.semantic_features_json
                        ELSE excluded.semantic_features_json
                    END,
                    content_revision = excluded.content_revision,
                    last_scored_at = excluded.last_scored_at
                WHERE resurfacing_candidates.content_revision IS NULL
                   OR NOT (
                       resurfacing_candidates.content_revision GLOB '[0-9]*'
                       AND resurfacing_candidates.content_revision NOT GLOB '*[^0-9]*'
                       AND excluded.content_revision GLOB '[0-9]*'
                       AND excluded.content_revision NOT GLOB '*[^0-9]*'
                   )
                   OR CAST(excluded.content_revision AS INTEGER) >=
                      CAST(resurfacing_candidates.content_revision AS INTEGER)",
                    params![
                        candidate.candidate_id,
                        principal,
                        workspace,
                        candidate.source_kind.as_str(),
                        candidate.source_ref,
                        candidate.title,
                        candidate.content_digest,
                        salience_score,
                        signals_json,
                        candidate.temporal_anchor_at,
                        candidate.embedding_id,
                        candidate.state.as_str(),
                        candidate.first_seen_at,
                        candidate.last_scored_at,
                        candidate.last_surfaced_at,
                        candidate.cooldown_until,
                        candidate.surface_count,
                        candidate.dismiss_count,
                        content_details_json,
                        candidate.content_revision,
                        semantic_features_json,
                    ],
                )
                .context("upserting resurfacing candidate")?;
            Ok(changed > 0)
        })
        .await
        .context("resurfacing upsert_candidate task panicked")?
    }

    /// Fetch a candidate by id within a scope, or `None` if absent.
    pub async fn get_candidate(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
    ) -> Result<Option<Candidate>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate_id = candidate_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut stmt = conn.prepare(&format!(
                "SELECT {CANDIDATE_COLUMNS} FROM resurfacing_candidates \
                 WHERE candidate_id = ? AND principal = ? AND workspace = ?",
            ))?;
            let mut rows = stmt.query(params![candidate_id, principal, workspace])?;
            match rows.next()? {
                Some(row) => Ok(Some(map_candidate_row(row)?)),
                None => Ok(None),
            }
        })
        .await
        .context("resurfacing get_candidate task panicked")?
    }

    /// Semantics-only compare-and-set. No salience, lifecycle, source, title,
    /// or routing field can be rewritten by an extraction completion.
    pub async fn update_semantic_features_if_revision(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        expected_content_revision: &str,
        semantic_envelope: &serde_json::Value,
    ) -> Result<bool> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate_id = candidate_id.to_string();
        let expected_content_revision = expected_content_revision.to_string();
        let semantic_envelope = serde_json::to_string(semantic_envelope)
            .context("serializing resurfacing semantic extraction envelope")?;
        tokio::task::spawn_blocking(move || {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            Ok(conn.execute(
                "UPDATE resurfacing_candidates SET semantic_features_json = ?
                 WHERE principal = ? AND workspace = ? AND candidate_id = ?
                   AND content_revision = ? AND state IN ('candidate', 'surfaced')",
                params![
                    semantic_envelope,
                    principal,
                    workspace,
                    candidate_id,
                    expected_content_revision,
                ],
            )? > 0)
        })
        .await
        .context("resurfacing semantic extraction compare-and-set task panicked")?
    }

    /// Inspect an idempotency key before resolving live source metadata. A
    /// completed request replays even after the candidate has transitioned to
    /// `acted`; incompatible reuse of the same key is rejected.
    pub async fn lookup_contextual_action_claim(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        idempotency_key: &str,
        action_kind: &str,
        input_hash: &str,
        now: i64,
        stale_after_secs: i64,
    ) -> Result<ResurfacingActionClaimLookup> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate_id = candidate_id.to_string();
        let idempotency_key = idempotency_key.to_string();
        let action_kind = action_kind.to_string();
        let input_hash = input_hash.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(claim) = query_contextual_action_claim(
                &conn,
                &principal,
                &workspace,
                &candidate_id,
                &idempotency_key,
            )?
            else {
                return Ok(ResurfacingActionClaimLookup::Missing);
            };
            contextual_action_lookup(&claim, &action_kind, &input_hash, now, stale_after_secs)
        })
        .await
        .context("resurfacing lookup_contextual_action_claim task panicked")?
    }

    /// Claim one validated contextual action. Failed claims and abandoned
    /// in-progress claims may be retried with the same payload; completed
    /// claims replay their stored bounded result.
    #[allow(clippy::too_many_arguments)]
    pub async fn begin_contextual_action_claim(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        idempotency_key: &str,
        action_kind: &str,
        input_hash: &str,
        content_revision: Option<&str>,
        result_ref: &str,
        now: i64,
        stale_after_secs: i64,
        target_kind: &str,
    ) -> Result<ResurfacingActionClaimBegin> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate_id = candidate_id.to_string();
        let target_kind = target_kind.to_string();
        let idempotency_key = idempotency_key.to_string();
        let action_kind = action_kind.to_string();
        let input_hash = input_hash.to_string();
        let content_revision = content_revision.map(str::to_string);
        let result_ref = result_ref.to_string();
        tokio::task::spawn_blocking(move || {
            let mut conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tx = conn
                .transaction()
                .context("opening resurfacing contextual-action claim transaction")?;

            let existing = query_contextual_action_claim(
                &tx,
                &principal,
                &workspace,
                &candidate_id,
                &idempotency_key,
            )?;
            if let Some(claim) = existing.as_ref() {
                if claim.action_kind != action_kind || claim.input_hash != input_hash {
                    return Ok(ResurfacingActionClaimBegin::Conflict);
                }
                if claim.state == "completed" {
                    return Ok(ResurfacingActionClaimBegin::Completed(
                        stored_contextual_action_result(claim)?,
                    ));
                }
                if claim.state == "started"
                    && now.saturating_sub(claim.updated_at) < stale_after_secs.max(1)
                {
                    return Ok(ResurfacingActionClaimBegin::InProgress);
                }
                if claim.state != "started" && claim.state != "failed" {
                    anyhow::bail!("invalid resurfacing action claim state: {}", claim.state);
                }
            }

            // A resurfacing candidate is re-read inside this transaction so
            // actionability and the claim are decided atomically. Other lanes
            // are stored in a separate database, so that read cannot join here;
            // their caller validates immediately before claiming instead. The
            // idempotency and replay guarantees below are unaffected either
            // way — only the actionability re-check moves earlier.
            if target_kind == TARGET_KIND_RESURFACING_CANDIDATE {
                let Some((candidate_state, current_revision)) =
                    read_candidate_action_state(&tx, &principal, &workspace, &candidate_id)?
                else {
                    return Ok(ResurfacingActionClaimBegin::NotFound);
                };
                if current_revision.as_deref() != content_revision.as_deref() {
                    return Ok(ResurfacingActionClaimBegin::StaleRevision { current_revision });
                }
                if candidate_state != CandidateState::Surfaced {
                    return Ok(ResurfacingActionClaimBegin::NotActionable {
                        state: candidate_state,
                    });
                }
            }

            if existing.is_some() {
                tx.execute(
                    "UPDATE resurfacing_action_claims \
                     SET state = 'started', result_ref = ?, result_json = NULL, \
                         error_class = NULL, updated_at = ? \
                     WHERE principal = ? AND workspace = ? AND candidate_id = ? \
                       AND idempotency_key = ? AND action_kind = ? AND input_hash = ?",
                    params![
                        result_ref,
                        now,
                        principal,
                        workspace,
                        candidate_id,
                        idempotency_key,
                        action_kind,
                        input_hash,
                    ],
                )
                .context("retrying resurfacing contextual-action claim")?;
            } else {
                tx.execute(
                    "INSERT INTO resurfacing_action_claims ( \
                        principal, workspace, candidate_id, idempotency_key, \
                        action_kind, input_hash, content_revision, target_kind, state, \
                        result_ref, result_json, error_class, created_at, updated_at \
                     ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'started', ?, NULL, NULL, ?, ?)",
                    params![
                        principal,
                        workspace,
                        candidate_id,
                        idempotency_key,
                        action_kind,
                        input_hash,
                        content_revision,
                        target_kind,
                        result_ref,
                        now,
                        now,
                    ],
                )
                .context("inserting resurfacing contextual-action claim")?;
                insert_contextual_action_event(
                    &tx,
                    &principal,
                    &workspace,
                    &candidate_id,
                    &action_kind,
                    content_revision.as_deref(),
                    "selected",
                    None,
                    None,
                    now,
                )?;
            }
            insert_contextual_action_event(
                &tx,
                &principal,
                &workspace,
                &candidate_id,
                &action_kind,
                content_revision.as_deref(),
                "started",
                Some(&result_ref),
                None,
                now,
            )?;
            tx.commit()
                .context("committing resurfacing contextual-action claim")?;
            Ok(ResurfacingActionClaimBegin::Claimed { result_ref })
        })
        .await
        .context("resurfacing begin_contextual_action_claim task panicked")?
    }

    /// Mark a downstream action failure without changing candidate feedback.
    /// The same idempotency key remains retryable with the same input hash.
    #[allow(clippy::too_many_arguments)]
    pub async fn fail_contextual_action_claim(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        idempotency_key: &str,
        action_kind: &str,
        input_hash: &str,
        content_revision: Option<&str>,
        claim_started_at: i64,
        error_class: &str,
        now: i64,
    ) -> Result<()> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate_id = candidate_id.to_string();
        let idempotency_key = idempotency_key.to_string();
        let action_kind = action_kind.to_string();
        let input_hash = input_hash.to_string();
        let content_revision = content_revision.map(str::to_string);
        let error_class = bounded_action_field(error_class, 80);
        tokio::task::spawn_blocking(move || {
            let mut conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tx = conn
                .transaction()
                .context("opening resurfacing contextual-action failure transaction")?;
            let changed = tx
                .execute(
                    "UPDATE resurfacing_action_claims \
                     SET state = 'failed', error_class = ?, updated_at = ? \
                     WHERE principal = ? AND workspace = ? AND candidate_id = ? \
                       AND idempotency_key = ? AND action_kind = ? AND input_hash = ? \
                       AND state = 'started' AND updated_at = ?",
                    params![
                        error_class,
                        now,
                        principal,
                        workspace,
                        candidate_id,
                        idempotency_key,
                        action_kind,
                        input_hash,
                        claim_started_at,
                    ],
                )
                .context("failing resurfacing contextual-action claim")?;
            if changed > 0 {
                insert_contextual_action_event(
                    &tx,
                    &principal,
                    &workspace,
                    &candidate_id,
                    &action_kind,
                    content_revision.as_deref(),
                    "failed",
                    None,
                    Some(&error_class),
                    now,
                )?;
            }
            tx.commit()
                .context("committing resurfacing contextual-action failure")?;
            Ok(())
        })
        .await
        .context("resurfacing fail_contextual_action_claim task panicked")?
    }

    /// Complete a claim and, for durable owner actions, record positive
    /// engagement in the same SQLite transaction. The result is bounded JSON
    /// suitable for idempotent replay; raw source content must never be passed.
    #[allow(clippy::too_many_arguments)]
    pub async fn complete_contextual_action_claim(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        idempotency_key: &str,
        action_kind: &str,
        input_hash: &str,
        content_revision: Option<&str>,
        claim_started_at: i64,
        result_ref: &str,
        result: &serde_json::Value,
        mark_acted: bool,
        now: i64,
        acted_cooldown_secs: i64,
    ) -> Result<ResurfacingStoredActionResult> {
        let result_json = serde_json::to_string(result)
            .context("serializing resurfacing contextual-action result")?;
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate_id = candidate_id.to_string();
        let idempotency_key = idempotency_key.to_string();
        let action_kind = action_kind.to_string();
        let input_hash = input_hash.to_string();
        let content_revision = content_revision.map(str::to_string);
        let result_ref = result_ref.to_string();
        tokio::task::spawn_blocking(move || {
            let mut conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tx = conn
                .transaction()
                .context("opening resurfacing contextual-action completion transaction")?;
            let claim = query_contextual_action_claim(
                &tx,
                &principal,
                &workspace,
                &candidate_id,
                &idempotency_key,
            )?
            .context("resurfacing contextual-action claim disappeared")?;
            if claim.action_kind != action_kind || claim.input_hash != input_hash {
                anyhow::bail!("resurfacing contextual-action claim payload mismatch");
            }
            if claim.state == "completed" {
                return stored_contextual_action_result(&claim);
            }
            if claim.state != "started" {
                anyhow::bail!(
                    "resurfacing contextual-action claim is not active: {}",
                    claim.state
                );
            }
            if claim.updated_at != claim_started_at {
                anyhow::bail!("resurfacing contextual-action claim attempt was superseded");
            }
            if claim.result_ref != result_ref {
                anyhow::bail!("resurfacing contextual-action result reference mismatch");
            }
            if claim.content_revision != content_revision {
                anyhow::bail!("resurfacing contextual-action claim revision mismatch");
            }

            let changed = tx
                .execute(
                    "UPDATE resurfacing_action_claims \
                 SET state = 'completed', result_json = ?, error_class = NULL, updated_at = ? \
                 WHERE principal = ? AND workspace = ? AND candidate_id = ? \
                   AND idempotency_key = ? AND action_kind = ? AND input_hash = ? \
                   AND state = 'started' AND updated_at = ?",
                    params![
                        result_json,
                        now,
                        principal,
                        workspace,
                        candidate_id,
                        idempotency_key,
                        action_kind,
                        input_hash,
                        claim_started_at,
                    ],
                )
                .context("completing resurfacing contextual-action claim")?;
            if changed != 1 {
                anyhow::bail!("resurfacing contextual-action claim attempt was superseded");
            }
            insert_contextual_action_event(
                &tx,
                &principal,
                &workspace,
                &candidate_id,
                &action_kind,
                content_revision.as_deref(),
                "completed",
                Some(&result_ref),
                None,
                now,
            )?;
            if mark_acted {
                record_contextual_action_positive(
                    &tx,
                    &principal,
                    &workspace,
                    &candidate_id,
                    claim.content_revision.as_deref(),
                    now,
                    acted_cooldown_secs,
                )?;
            }
            tx.commit()
                .context("committing resurfacing contextual-action completion")?;
            Ok(ResurfacingStoredActionResult {
                result_ref,
                result: serde_json::from_str(&result_json)
                    .context("decoding committed resurfacing action result")?,
            })
        })
        .await
        .context("resurfacing complete_contextual_action_claim task panicked")?
    }

    /// Record a structural action event that does not own a claim, such as a
    /// stale-revision rejection. Free-form inputs are deliberately excluded.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_contextual_action_event(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        action_kind: &str,
        content_revision: Option<&str>,
        event_type: &str,
        error_class: Option<&str>,
        now: i64,
    ) -> Result<()> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate_id = candidate_id.to_string();
        let action_kind = action_kind.to_string();
        let content_revision = content_revision.map(str::to_string);
        let event_type = bounded_action_field(event_type, 80);
        let error_class = error_class.map(|value| bounded_action_field(value, 80));
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            insert_contextual_action_event(
                &conn,
                &principal,
                &workspace,
                &candidate_id,
                &action_kind,
                content_revision.as_deref(),
                &event_type,
                None,
                error_class.as_deref(),
                now,
            )
        })
        .await
        .context("resurfacing record_contextual_action_event task panicked")?
    }

    /// Recent source references in one lifecycle state. Historical comms
    /// repair uses this bounded read to prioritize cards currently visible to
    /// the owner without coupling the channel store to the resurfacing DB.
    pub async fn list_source_refs_by_state(
        &self,
        principal: &str,
        workspace: &str,
        source_kind: SourceKind,
        state: CandidateState,
        limit: usize,
    ) -> Result<Vec<String>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut stmt = conn.prepare(
                "SELECT source_ref FROM resurfacing_candidates \
                 WHERE principal = ? AND workspace = ? \
                   AND source_kind = ? AND state = ? \
                 ORDER BY last_surfaced_at DESC, candidate_id ASC LIMIT ?",
            )?;
            let rows = stmt.query_map(
                params![
                    principal,
                    workspace,
                    source_kind.as_str(),
                    state.as_str(),
                    limit as i64
                ],
                |row| row.get::<_, String>(0),
            )?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .context("reading resurfacing source refs")
        })
        .await
        .context("resurfacing list_source_refs_by_state task panicked")?
    }

    /// Surfaced communication cards whose current content revision has not yet
    /// passed active routing repair. A durable per-revision receipt makes the
    /// pass idempotent while still allowing a freshly distilled revision to be
    /// checked again.
    pub async fn list_active_repair_candidates(
        &self,
        principal: &str,
        workspace: &str,
        limit: usize,
    ) -> Result<Vec<Candidate>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut stmt = conn.prepare(&format!(
                "SELECT {CANDIDATE_COLUMNS} FROM resurfacing_candidates c \
                 WHERE principal = ? AND workspace = ? AND source_kind = ? AND state = ? \
                   AND NOT EXISTS (SELECT 1 FROM resurfacing_routing_repairs r \
                       WHERE r.principal = c.principal AND r.workspace = c.workspace \
                         AND r.candidate_id = c.candidate_id \
                         AND r.content_revision = COALESCE(c.content_revision, '')) \
                 ORDER BY last_surfaced_at DESC, candidate_id ASC LIMIT ?"
            ))?;
            let mut rows = stmt.query(params![
                principal,
                workspace,
                SourceKind::Comm.as_str(),
                CandidateState::Surfaced.as_str(),
                limit as i64
            ])?;
            let mut candidates = Vec::new();
            while let Some(row) = rows.next()? {
                match map_candidate_row(row) {
                    Ok(candidate) => candidates.push(candidate),
                    Err(error) => tracing::warn!(
                        error = %error,
                        "skipping undecodable resurfacing active-repair candidate"
                    ),
                }
            }
            Ok(candidates)
        })
        .await
        .context("resurfacing list_active_repair_candidates task panicked")?
    }

    /// Stable candidate-id scan for semantic coverage. It includes the whole
    /// active universe (not just the visible page) and returns only durable,
    /// source-sanitized candidate records.
    pub async fn list_active_semantic_candidates(
        &self,
        principal: &str,
        workspace: &str,
        after_candidate_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<Candidate>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let after_candidate_id = after_candidate_id.unwrap_or_default().to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            let mut stmt = conn.prepare(&format!(
                "SELECT {CANDIDATE_COLUMNS} FROM resurfacing_candidates
                 WHERE principal = ? AND workspace = ?
                   AND state IN ('candidate', 'surfaced')
                   AND candidate_id > ?
                 ORDER BY candidate_id ASC LIMIT ?"
            ))?;
            let mut rows = stmt.query(params![
                principal,
                workspace,
                after_candidate_id,
                limit as i64
            ])?;
            let mut candidates = Vec::new();
            while let Some(row) = rows.next()? {
                candidates.push(map_candidate_row(row)?);
            }
            Ok(candidates)
        })
        .await
        .context("resurfacing active semantic candidate scan task panicked")?
    }

    pub async fn count_active_semantic_candidates(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<u64> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM resurfacing_candidates
                 WHERE principal = ? AND workspace = ?
                   AND state IN ('candidate', 'surfaced')
                 ",
                params![principal, workspace],
                |row| row.get(0),
            )?;
            Ok(count.max(0) as u64)
        })
        .await
        .context("resurfacing active semantic candidate count task panicked")?
    }

    /// Retract already-surfaced memory cards whose tier is no longer eligible.
    ///
    /// Filtering the memory source governs ingestion only, so narrowing
    /// `resurfacing.memory_tiers` leaves whatever the previous policy already
    /// surfaced sitting in the owner's lane until it ages out. This is the
    /// repair half: bounded, idempotent, and driven by the same allowlist, so a
    /// tier change takes effect on the existing lane rather than only on the
    /// next thing ingested.
    ///
    /// Retracted rows move to `dismissed`, which the curator and scorer both
    /// treat as ineligible. Neither `dismiss_count` nor
    /// `resurfacing_dismissed_signals` is touched: the owner did not dismiss
    /// these, policy did, and recording it as feedback would teach the ranker
    /// from an action nobody took.
    ///
    /// An empty allowlist retracts every memory card, matching the source's own
    /// fail-closed reading of an unconfigured list.
    pub async fn retract_ineligible_memory_candidates(
        &self,
        principal: &str,
        workspace: &str,
        allowed_tiers: &[String],
        limit: usize,
        apply: bool,
    ) -> Result<u64> {
        if limit == 0 {
            return Ok(0);
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let allowed: Vec<String> = allowed_tiers.to_vec();
        tokio::task::spawn_blocking(move || {
            let mut conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tx = conn
                .transaction()
                .context("opening resurfacing memory-tier retraction transaction")?;
            // `source_ref` is `<tier>#<key>` by construction, so the tier is the
            // prefix. A row without the separator yields an empty tier, matches
            // no allowlist, and is retracted — fail-closed, like the source.
            let tier_expr = "substr(c.source_ref, 1, instr(c.source_ref, '#') - 1)";
            let mut sql = format!(
                "SELECT c.candidate_id FROM resurfacing_candidates c \
                 WHERE c.principal = ? AND c.workspace = ? \
                   AND c.source_kind = ? AND c.state = ?"
            );
            let mut values: Vec<Box<dyn rusqlite::ToSql>> = vec![
                Box::new(principal.clone()),
                Box::new(workspace.clone()),
                Box::new(SourceKind::Memory.as_str().to_string()),
                Box::new(CandidateState::Surfaced.as_str().to_string()),
            ];
            if !allowed.is_empty() {
                let placeholders = vec!["?"; allowed.len()].join(", ");
                sql.push_str(&format!(" AND {tier_expr} NOT IN ({placeholders})"));
                for tier in &allowed {
                    values.push(Box::new(tier.clone()));
                }
            }
            sql.push_str(" ORDER BY c.candidate_id LIMIT ?");
            values.push(Box::new(limit as i64));

            let params: Vec<&dyn rusqlite::ToSql> =
                values.iter().map(|value| value.as_ref()).collect();
            let mut statement = tx.prepare(&sql)?;
            let ids = statement
                .query_map(params.as_slice(), |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            drop(statement);
            if apply {
                for candidate_id in &ids {
                    tx.execute(
                        "UPDATE resurfacing_candidates SET state = ? \
                         WHERE principal = ? AND workspace = ? AND candidate_id = ? \
                           AND state = ?",
                        params![
                            CandidateState::Dismissed.as_str(),
                            principal,
                            workspace,
                            candidate_id,
                            CandidateState::Surfaced.as_str(),
                        ],
                    )?;
                }
            }
            let retracted = ids.len() as u64;
            tx.commit()
                .context("committing resurfacing memory-tier retraction")?;
            Ok(retracted)
        })
        .await
        .context("resurfacing memory-tier retraction task panicked")?
    }

    /// Atomically record one routing-repair receipt and, when the item no
    /// longer belongs in Worth a look, withhold it from the surfaced lane. The
    /// state and revision guards preserve concurrent owner feedback and make a
    /// repeated pass a no-op.
    #[allow(clippy::too_many_arguments)]
    pub async fn complete_active_repair(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        content_revision: Option<&str>,
        outcome: &str,
        withhold: bool,
        cooldown_until: i64,
        now: i64,
    ) -> Result<bool> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate_id = candidate_id.to_string();
        let content_revision = content_revision.unwrap_or_default().to_string();
        let outcome = bounded_action_field(outcome, 80);
        tokio::task::spawn_blocking(move || {
            let mut conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tx = conn
                .transaction()
                .context("opening resurfacing active-repair transaction")?;
            let eligible: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM resurfacing_candidates \
                 WHERE principal = ? AND workspace = ? AND candidate_id = ? \
                   AND source_kind = ? AND state = ? \
                   AND COALESCE(content_revision, '') = ?)",
                params![
                    principal,
                    workspace,
                    candidate_id,
                    SourceKind::Comm.as_str(),
                    CandidateState::Surfaced.as_str(),
                    content_revision,
                ],
                |row| row.get(0),
            )?;
            if !eligible {
                return Ok(false);
            }
            if withhold {
                tx.execute(
                    "UPDATE resurfacing_candidates SET state = ?, cooldown_until = ? \
                     WHERE principal = ? AND workspace = ? AND candidate_id = ? \
                       AND state = ? AND COALESCE(content_revision, '') = ?",
                    params![
                        CandidateState::Candidate.as_str(),
                        cooldown_until,
                        principal,
                        workspace,
                        candidate_id,
                        CandidateState::Surfaced.as_str(),
                        content_revision,
                    ],
                )?;
            }
            tx.execute(
                "INSERT OR IGNORE INTO resurfacing_routing_repairs \
                 (principal, workspace, candidate_id, content_revision, outcome, repaired_at) \
                 VALUES (?, ?, ?, ?, ?, ?)",
                params![
                    principal,
                    workspace,
                    candidate_id,
                    content_revision,
                    outcome,
                    now
                ],
            )?;
            tx.commit()
                .context("committing resurfacing active-repair transaction")?;
            Ok(true)
        })
        .await
        .context("resurfacing complete_active_repair task panicked")?
    }

    /// Read the corpus scan cursor for a scope, or `0` when no scan has
    /// recorded a watermark yet (i.e. "start from the beginning").
    pub async fn get_watermark(
        &self,
        principal: &str,
        workspace: &str,
        corpus_kind: &str,
    ) -> Result<i64> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let corpus_kind = corpus_kind.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut stmt = conn.prepare(
                "SELECT cursor FROM resurfacing_watermarks \
                 WHERE principal = ? AND workspace = ? AND corpus_kind = ?",
            )?;
            let mut rows = stmt.query(params![principal, workspace, corpus_kind])?;
            match rows.next()? {
                Some(row) => {
                    let cursor: i64 = row.get(0)?;
                    Ok(cursor)
                },
                None => Ok(0_i64),
            }
        })
        .await
        .context("resurfacing get_watermark task panicked")?
    }

    /// Advance (or insert) the corpus scan cursor for a scope. Idempotent on
    /// the `(principal, workspace, corpus_kind)` primary key.
    pub async fn set_watermark(
        &self,
        principal: &str,
        workspace: &str,
        corpus_kind: &str,
        cursor: i64,
    ) -> Result<()> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let corpus_kind = corpus_kind.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            conn.execute(
                "INSERT INTO resurfacing_watermarks (
                    principal, workspace, corpus_kind, cursor
                ) VALUES (?, ?, ?, ?)
                ON CONFLICT(principal, workspace, corpus_kind) DO UPDATE SET
                    cursor = MAX(resurfacing_watermarks.cursor, excluded.cursor)",
                params![principal, workspace, corpus_kind, cursor],
            )
            .context("upserting resurfacing watermark")?;
            Ok(())
        })
        .await
        .context("resurfacing set_watermark task panicked")?
    }

    /// Return up to `limit` scorable candidates in a scope, highest salience
    /// first. Only rows still in the `candidate` state whose cooldown has
    /// elapsed (`cooldown_until <= now`) are eligible; surfaced/acted/dismissed
    /// rows and cooling-down rows are excluded.
    pub async fn list_top_candidates(
        &self,
        principal: &str,
        workspace: &str,
        now: i64,
        limit: usize,
    ) -> Result<Vec<Candidate>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let requested = limit;
            let page_size = requested.saturating_mul(2).clamp(50, 500);
            let max_raw_scan = requested.saturating_mul(10).max(page_size).min(10_000);
            let mut out = Vec::new();
            let mut offset = 0usize;
            while out.len() < requested && offset < max_raw_scan {
                let raw_limit = page_size.min(max_raw_scan - offset);
                let mut stmt = conn.prepare(&format!(
                    "SELECT {CANDIDATE_COLUMNS} FROM resurfacing_candidates \
                     WHERE principal = ? AND workspace = ? AND state = ? \
                     AND cooldown_until <= ? \
                     ORDER BY salience_score DESC LIMIT ? OFFSET ?",
                ))?;
                let mut rows = stmt.query(params![
                    principal.as_str(),
                    workspace.as_str(),
                    CandidateState::Candidate.as_str(),
                    now,
                    raw_limit as i64,
                    offset as i64,
                ])?;
                let mut raw_seen = 0usize;
                while let Some(row) = rows.next()? {
                    raw_seen += 1;
                    // Skip a single undecodable row (unknown state/source_kind or
                    // corrupt signals JSON) rather than `?`-failing the ENTIRE read
                    // and darkening the Today surface — mirrors the skip-bad-row
                    // policy of candidate_funnel/read_kind_engagement.
                    match map_candidate_row(row) {
                        Ok(candidate) => {
                            out.push(candidate);
                            if out.len() >= requested {
                                break;
                            }
                        },
                        Err(err) => {
                            let cid = row
                                .get::<_, String>(0)
                                .unwrap_or_else(|_| "<unknown>".to_string());
                            tracing::warn!(
                                candidate_id = %cid,
                                error = %err,
                                "skipping undecodable resurfacing candidate row in list_top_candidates",
                            );
                        },
                    }
                }
                if raw_seen < raw_limit {
                    break;
                }
                offset += raw_seen;
            }
            out.truncate(requested);
            Ok(out)
        })
        .await
        .context("resurfacing list_top_candidates task panicked")?
    }

    /// Apply exponential time-decay to every candidate's salience in a scope:
    /// `new = salience * 0.5^((now - last_scored_at) / halflife_secs)`, computed
    /// in Rust because the bundled SQLite may not have math functions (`pow`)
    /// enabled. Each decayed row's `last_scored_at` is advanced to `now` so
    /// repeated passes decay from the fresh baseline rather than compounding.
    /// A non-positive (or NaN) `halflife_days` is a no-op, and any non-finite
    /// decayed score is skipped rather than persisted.
    pub async fn decay_all(
        &self,
        principal: &str,
        workspace: &str,
        halflife_days: f64,
        now: i64,
    ) -> Result<()> {
        // A non-positive or NaN half-life would blow up / poison the exponent;
        // treat it as "no decay this pass" so we never corrupt scores.
        if !(halflife_days > 0.0) {
            return Ok(());
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let halflife_secs = halflife_days * 86_400.0;
            let mut conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // Read (id, score, last_scored_at), compute the decayed score in
            // Rust, then write it back. The read statement is scoped to this
            // block so it stops borrowing the connection before the updates.
            let updates: Vec<(String, f64)> = {
                let mut stmt = conn.prepare(
                    "SELECT candidate_id, salience_score, last_scored_at \
                     FROM resurfacing_candidates \
                     WHERE principal = ? AND workspace = ?",
                )?;
                let mapped = stmt.query_map(params![principal, workspace], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, f64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })?;
                let mut acc = Vec::new();
                for triple in mapped {
                    let (candidate_id, score, last_scored_at) = triple?;
                    // Floor elapsed at 0 so a clock regression (a future
                    // `last_scored_at`) can't yield negative elapsed →
                    // `0.5^negative > 1` = score amplification.
                    let elapsed = (now - last_scored_at).max(0) as f64;
                    let decayed = score * 0.5_f64.powf(elapsed / halflife_secs);
                    if decayed.is_finite() {
                        acc.push((candidate_id, decayed));
                    }
                }
                acc
            };
            // Apply every decay write atomically in one transaction.
            let tx = conn
                .transaction()
                .context("opening resurfacing decay transaction")?;
            for (candidate_id, decayed) in updates {
                tx.execute(
                    "UPDATE resurfacing_candidates \
                     SET salience_score = ?, last_scored_at = ? \
                     WHERE candidate_id = ? AND principal = ? AND workspace = ?",
                    params![decayed, now, candidate_id, principal, workspace],
                )
                .context("updating decayed resurfacing candidate")?;
            }
            tx.commit()
                .context("committing resurfacing decay transaction")?;
            Ok(())
        })
        .await
        .context("resurfacing decay_all task panicked")?
    }

    /// Mark the given candidates (by id, scoped to `principal`/`workspace`) as
    /// surfaced: transition each to `surfaced`, stamp `last_surfaced_at`, and
    /// bump `surface_count`. The curator calls this after picking the top-`cap`
    /// eligible rows so they drop out of the `state='candidate'` eligibility
    /// filter and won't be re-surfaced until feedback or a fresh scan moves
    /// them back. Ids not present in the scope are simply no-ops.
    pub async fn mark_surfaced(
        &self,
        principal: &str,
        workspace: &str,
        candidate_ids: &[String],
        now: i64,
    ) -> Result<Vec<String>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate_ids = candidate_ids.to_vec();
        tokio::task::spawn_blocking(move || {
            let mut conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // One atomic transaction for the whole batch. Each UPDATE is guarded
            // by `AND state = 'candidate'` so a row the owner dismissed/acted
            // between the curator's list_top_candidates and this call is NOT
            // flipped back to `surfaced` (a 0-row no-op for such a row).
            let tx = conn
                .transaction()
                .context("opening resurfacing mark_surfaced transaction")?;
            let mut updated_ids = Vec::new();
            for id in &candidate_ids {
                let updated = tx
                    .execute(
                        "UPDATE resurfacing_candidates \
                     SET state = ?, last_surfaced_at = ?, surface_count = surface_count + 1 \
                     WHERE candidate_id = ? AND principal = ? AND workspace = ? \
                     AND state = ?",
                        params![
                            CandidateState::Surfaced.as_str(),
                            now,
                            id,
                            principal,
                            workspace,
                            CandidateState::Candidate.as_str(),
                        ],
                    )
                    .context("marking resurfacing candidate surfaced")?;
                if updated > 0 {
                    updated_ids.push(id.clone());
                }
            }
            tx.commit()
                .context("committing resurfacing mark_surfaced transaction")?;
            Ok(updated_ids)
        })
        .await
        .context("resurfacing mark_surfaced task panicked")?
    }

    /// Temporarily remove reviewed-but-not-selected candidates from the active
    /// curation queue by pushing `cooldown_until` forward while keeping them in
    /// `state='candidate'`. The scorer's upsert path preserves cooldowns, so a
    /// fresh scan cannot immediately re-queue a candidate the curator just passed
    /// over. Rows already surfaced/acted/dismissed/snoozed, absent from the scope,
    /// or already cooled beyond `cooldown_until` are no-ops.
    pub async fn defer_candidates(
        &self,
        principal: &str,
        workspace: &str,
        candidate_ids: &[String],
        cooldown_until: i64,
    ) -> Result<Vec<String>> {
        if candidate_ids.is_empty() {
            return Ok(Vec::new());
        }
        if candidate_ids.len() > 512 {
            anyhow::bail!("resurfacing defer candidate request exceeds 512 ids");
        }

        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate_ids = candidate_ids.to_vec();
        tokio::task::spawn_blocking(move || {
            let mut conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tx = conn
                .transaction()
                .context("opening resurfacing defer_candidates transaction")?;
            let mut updated_ids = Vec::new();
            for id in &candidate_ids {
                let updated = tx
                    .execute(
                        "UPDATE resurfacing_candidates \
                         SET cooldown_until = ? \
                         WHERE candidate_id = ? AND principal = ? AND workspace = ? \
                         AND state = ? AND cooldown_until < ?",
                        params![
                            cooldown_until,
                            id,
                            principal,
                            workspace,
                            CandidateState::Candidate.as_str(),
                            cooldown_until,
                        ],
                    )
                    .context("deferring resurfacing candidate")?;
                if updated > 0 {
                    updated_ids.push(id.clone());
                }
            }
            tx.commit()
                .context("committing resurfacing defer_candidates transaction")?;
            Ok(updated_ids)
        })
        .await
        .context("resurfacing defer_candidates task panicked")?
    }

    /// Record owner feedback on a surfaced candidate and apply its lifecycle
    /// transition, in one transaction so the feedback log and the candidate
    /// update commit together.
    ///
    /// * `Dismiss` → `state='dismissed'`, `cooldown_until = now +
    ///   dismiss_cooldown_secs`, `dismiss_count += 1`.
    /// * `Open` | `Acknowledge` → `state='acted'`, `cooldown_until = now +
    ///   ack_cooldown_secs` (the caller passes a longer ack cooldown than the
    ///   dismiss cooldown).
    ///
    /// Because [`list_top_candidates`](Self::list_top_candidates) only returns
    /// `state='candidate'` rows whose cooldown has elapsed, a dismissed/acted
    /// row is doubly excluded from re-surfacing (terminal state *and* cooldown).
    /// Record an action with no dismiss reason (a plain dismiss penalizes similar,
    /// the default). Thin wrapper over [`record_action_with_reason`] kept so the
    /// many existing callers/tests need no change.
    pub async fn record_action(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        action: FeedbackAction,
        now: i64,
        dismiss_cooldown_secs: i64,
        ack_cooldown_secs: i64,
    ) -> Result<()> {
        self.record_action_with_reason(
            principal,
            workspace,
            candidate_id,
            action,
            None,
            now,
            dismiss_cooldown_secs,
            ack_cooldown_secs,
        )
        .await
    }

    pub async fn record_action_with_reason(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        action: FeedbackAction,
        // Dismiss reason (ignored for Open/Acknowledge). Tunes suppression:
        // `None` or a `penalizes_similar()` reason teaches "less like this"; a
        // "done" reason (already_handled/duplicate/delegated) dismisses this card
        // without penalizing its neighbors.
        reason: Option<DismissReason>,
        now: i64,
        dismiss_cooldown_secs: i64,
        ack_cooldown_secs: i64,
    ) -> Result<()> {
        self.record_action_with_reason_event(
            principal,
            workspace,
            candidate_id,
            action,
            reason,
            None,
            now,
            dismiss_cooldown_secs,
            ack_cooldown_secs,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn record_action_with_reason_event(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        action: FeedbackAction,
        reason: Option<DismissReason>,
        event_id: Option<&str>,
        now: i64,
        dismiss_cooldown_secs: i64,
        ack_cooldown_secs: i64,
    ) -> Result<()> {
        if let Some(event_id) = event_id {
            anyhow::ensure!(
                !event_id.trim().is_empty() && event_id.chars().count() <= 200,
                "resurfacing feedback event_id must contain 1..=200 characters"
            );
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate_id = candidate_id.to_string();
        let event_id = event_id.map(str::to_string);
        tokio::task::spawn_blocking(move || {
            let mut conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tx = conn
                .transaction()
                .context("opening resurfacing feedback transaction")?;

            if let Some(event_id) = event_id.as_deref() {
                let existing = tx
                    .query_row(
                        "SELECT candidate_id, action, reason FROM resurfacing_feedback \
                         WHERE principal = ? AND workspace = ? AND event_id = ?",
                        params![principal, workspace, event_id],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, Option<String>>(2)?,
                            ))
                        },
                    )
                    .optional()?;
                if let Some((existing_candidate, existing_action, existing_reason)) = existing {
                    anyhow::ensure!(
                        existing_candidate == candidate_id
                            && existing_action == action.as_str()
                            && existing_reason.as_deref()
                                == reason.as_ref().map(DismissReason::as_str),
                        "resurfacing feedback event_id collision with a different payload"
                    );
                    tx.commit()
                        .context("committing replayed resurfacing feedback transaction")?;
                    return Ok(());
                }
            }

            // Read the acted candidate's lane ONCE up front. This doubles as an
            // in-scope existence check: `None` means the candidate_id does not
            // exist in this scope (a foreign/guessed id), so we must NOT append
            // an unbounded feedback-log row for it, nor bump any lane counter.
            // The state UPDATE below is then a harmless 0-row no-op.
            let source_kind =
                read_candidate_source_kind(&tx, &principal, &workspace, &candidate_id)?;
            let semantic_text = tx
                .query_row(
                    "SELECT title, content_digest, content_details_json \
                     FROM resurfacing_candidates \
                     WHERE principal = ? AND workspace = ? AND candidate_id = ?",
                    params![principal, workspace, candidate_id],
                    |row| {
                        let title: String = row.get(0)?;
                        let digest: String = row.get(1)?;
                        let details: Option<String> = row.get(2)?;
                        Ok([
                            title.as_str(),
                            digest.as_str(),
                            details.as_deref().unwrap_or(""),
                        ]
                        .join("\n"))
                    },
                )
                .optional()?
                .map(|text| {
                    text.trim()
                        .chars()
                        .take(ATTENTION_FEEDBACK_SEMANTIC_TEXT_MAX_CHARS)
                        .collect::<String>()
                });
            let feedback_embedding = read_current_candidate_embedding_with_contract(
                &tx,
                &principal,
                &workspace,
                &candidate_id,
            )?;

            // 1. Append the immutable feedback-log row — ONLY for a candidate
            //    that actually exists in-scope, so an action on a foreign/guessed
            //    id can't grow the append-only feedback log without bound.
            if source_kind.is_some() {
                tx.execute(
                    "INSERT INTO resurfacing_feedback \
                     (principal, workspace, candidate_id, action, event_id, reason, \
                      semantic_text, embedding_contract, vec, at) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    params![
                        principal,
                        workspace,
                        candidate_id,
                        action.as_str(),
                        event_id,
                        reason.as_ref().map(DismissReason::as_str),
                        semantic_text,
                        feedback_embedding
                            .as_ref()
                            .and_then(|(_, contract)| contract.as_deref()),
                        feedback_embedding
                            .as_ref()
                            .map(|(vector, _)| serde_json::to_string(vector))
                            .transpose()?,
                        now,
                    ],
                )
                .context("appending resurfacing feedback row")?;
            }

            // 2. Transition the candidate's state + cooldown.
            match action {
                FeedbackAction::Dismiss => {
                    tx.execute(
                        "UPDATE resurfacing_candidates \
                         SET state = ?, cooldown_until = ?, \
                             dismiss_count = dismiss_count + 1 \
                         WHERE candidate_id = ? AND principal = ? AND workspace = ?",
                        params![
                            CandidateState::Dismissed.as_str(),
                            now + dismiss_cooldown_secs,
                            candidate_id,
                            principal,
                            workspace,
                        ],
                    )
                    .context("updating dismissed resurfacing candidate")?;
                    // Dismiss teaches "not the ones just like it" — now on TWO
                    // horizons. Read the dismissed item's stored embedding ONCE
                    // (no double read) and reuse it for both:
                    //   * PHASE 2 (in-session): down-weight the dismissed item's
                    //     currently-loaded semantic neighbors so the curator is
                    //     less likely to re-surface near-duplicates THIS pass.
                    //   * PHASE 3 / P3a (cross-session): append the embedding to
                    //     the durable `resurfacing_dismissed_signals` table so
                    //     FUTURE new candidates resembling it are penalized at
                    //     score time — the lesson survives candidate churn and a
                    //     fresh session, where in-session suppression forgets.
                    // Both run inside this transaction so the log/state/
                    // suppression/durable-signal commit together. A no-op when
                    // the dismissed item has no stored embedding.
                    //
                    // Reason-tuned: only penalize similar items for a reasonless
                    // dismiss or a `penalizes_similar()` reason (not_relevant /
                    // spam). "This instance is done" reasons (already_handled /
                    // duplicate / delegated) still dismiss THIS card (state +
                    // cooldown above) but leave its neighbors alone — the category
                    // is fine, so we don't teach the ranker "less like this".
                    let penalize_similar = reason.map(|r| r.penalizes_similar()).unwrap_or(true);
                    if penalize_similar {
                        if let Some((dismissed_vec, Some(contract))) =
                            read_current_candidate_embedding_with_contract(
                                &tx,
                                &principal,
                                &workspace,
                                &candidate_id,
                            )?
                        {
                            suppress_neighbors(
                                &tx,
                                &principal,
                                &workspace,
                                &candidate_id,
                                &contract,
                                &dismissed_vec,
                            )
                            .context("suppressing dismissed candidate's neighbors")?;
                            write_dismissed_signal(
                                &tx,
                                &principal,
                                &workspace,
                                event_id.as_deref(),
                                Some(&contract),
                                &dismissed_vec,
                                now,
                            )
                            .context("recording durable dismissed signal")?;
                        }
                    }
                },
                FeedbackAction::Open | FeedbackAction::Acknowledge | FeedbackAction::OwnerWork => {
                    tx.execute(
                        "UPDATE resurfacing_candidates \
                         SET state = ?, cooldown_until = ? \
                         WHERE candidate_id = ? AND principal = ? AND workspace = ?",
                        params![
                            CandidateState::Acted.as_str(),
                            now + ack_cooldown_secs,
                            candidate_id,
                            principal,
                            workspace,
                        ],
                    )
                    .context("updating acted resurfacing candidate")?;
                    // "Mark useful" (`Open`) teaches "more of this kind" — the
                    // positive mirror of Dismiss (P4a): read the acted item's stored
                    // embedding ONCE and, when present, append it to the durable
                    // `resurfacing_affinity_signals` table so FUTURE new candidates
                    // resembling it are BOOSTED at score time. `Acknowledge` is a
                    // NEUTRAL "seen, no action needed": the shared UPDATE above stops
                    // it resurfacing, but it does NOT boost similar items (no affinity
                    // signal here, and no positive lane engagement below).
                    if action == FeedbackAction::Open {
                        if let Some((acted_vec, Some(contract))) =
                            read_current_candidate_embedding_with_contract(
                                &tx,
                                &principal,
                                &workspace,
                                &candidate_id,
                            )?
                        {
                            write_affinity_signal(
                                &tx,
                                &principal,
                                &workspace,
                                event_id.as_deref(),
                                Some(&contract),
                                &acted_vec,
                                now,
                            )
                            .context("recording durable affinity signal")?;
                        }
                    }
                },
            }

            // 3. P4c utility signal: attribute this action to the candidate's
            //    lane (`source_kind`) and bump the per-lane engagement counter in
            //    the SAME transaction, so the log/state/feedback-side-effects/
            //    counter all commit together. `Open` → positive ("more of this
            //    kind"), `Acknowledge` → neutral ("seen"), `Dismiss` → negative
            //    unless a "done" reason keeps it neutral. Reuse the
            //    `source_kind` read up front (a `None` — candidate absent from the
            //    scope — skips the counter gracefully, no lane to attribute to).
            if let Some(source_kind) = source_kind {
                let (positive, negative) = match action {
                    // "Mark useful" → positive ("more of this kind").
                    FeedbackAction::Open => (1_i64, 0_i64),
                    // Acknowledge → neutral: "seen", not a preference signal.
                    FeedbackAction::Acknowledge | FeedbackAction::OwnerWork => (0_i64, 0_i64),
                    // Dismiss → negative only when it penalizes similar (reasonless
                    // or not_relevant/spam); a "done" reason is neutral.
                    FeedbackAction::Dismiss => {
                        if reason.map(|r| r.penalizes_similar()).unwrap_or(true) {
                            (0_i64, 1_i64)
                        } else {
                            (0_i64, 0_i64)
                        }
                    },
                };
                // Skip a no-op (0,0) write for the neutral cases.
                if positive != 0 || negative != 0 {
                    bump_kind_engagement(
                        &tx,
                        &principal,
                        &workspace,
                        &source_kind,
                        positive,
                        negative,
                    )
                    .context("bumping resurfacing kind engagement")?;
                }
            }

            tx.commit()
                .context("committing resurfacing feedback transaction")?;
            Ok(())
        })
        .await
        .context("resurfacing record_action task panicked")?
    }

    /// Server-side page over candidates currently in the `surfaced` state for a
    /// scope, most recently surfaced first (ties broken by salience, then id).
    /// Backs the `GET /channel-assist/resurfacing/today` read: the curator has
    /// already moved the top eligible rows into `surfaced`, so this returns
    /// exactly the cards the owner should see now, capped at `limit`.
    ///
    /// Use `cursor` for interactive "load more" flows that may mutate rows
    /// between pages. `offset` remains available for simple first-page/back-compat
    /// callers and is ignored once a cursor is supplied.
    pub async fn list_surfaced_page(
        &self,
        principal: &str,
        workspace: &str,
        limit: usize,
        offset: usize,
        cursor: Option<SurfacedCursor>,
    ) -> Result<SurfacedPage> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let total = conn
                .query_row(
                    "SELECT COUNT(*) FROM resurfacing_candidates \
                     WHERE principal = ? AND workspace = ? AND state = ?",
                    params![principal, workspace, CandidateState::Surfaced.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .context("counting surfaced resurfacing candidates")?
                .max(0) as u64;

            let requested = limit.max(1);
            let minimum_fetch = requested.saturating_add(1).min(1_000);
            let fetch_limit = requested
                .saturating_mul(4)
                .saturating_add(1)
                .min(1_000)
                .max(minimum_fetch);
            let mut stmt;
            let mut rows = if let Some(cursor) = cursor.clone() {
                stmt = conn.prepare(&format!(
                    "SELECT {CANDIDATE_COLUMNS} FROM resurfacing_candidates \
                     WHERE principal = ? AND workspace = ? AND state = ? \
                     AND ( \
                         COALESCE(last_surfaced_at, 0) < ? \
                         OR (COALESCE(last_surfaced_at, 0) = ? AND salience_score < ?) \
                         OR (COALESCE(last_surfaced_at, 0) = ? AND salience_score = ? AND candidate_id > ?) \
                     ) \
                     ORDER BY COALESCE(last_surfaced_at, 0) DESC, salience_score DESC, candidate_id ASC \
                     LIMIT ?",
                ))?;
                stmt.query(params![
                    principal,
                    workspace,
                    CandidateState::Surfaced.as_str(),
                    cursor.surfaced_at,
                    cursor.surfaced_at,
                    cursor.salience_score,
                    cursor.surfaced_at,
                    cursor.salience_score,
                    cursor.candidate_id,
                    fetch_limit as i64,
                ])?
            } else {
                stmt = conn.prepare(&format!(
                    "SELECT {CANDIDATE_COLUMNS} FROM resurfacing_candidates \
                     WHERE principal = ? AND workspace = ? AND state = ? \
                     ORDER BY COALESCE(last_surfaced_at, 0) DESC, salience_score DESC, candidate_id ASC \
                     LIMIT ? OFFSET ?",
                ))?;
                stmt.query(params![
                    principal,
                    workspace,
                    CandidateState::Surfaced.as_str(),
                    fetch_limit as i64,
                    offset as i64,
                ])?
            };
            let mut out = Vec::new();
            let mut decoded_cursors = Vec::new();
            let mut raw_seen = 0usize;
            let mut last_raw_cursor: Option<SurfacedCursor> = None;
            while let Some(row) = rows.next()? {
                raw_seen += 1;
                let row_cursor = row_surfaced_cursor(row);
                last_raw_cursor = row_cursor.clone().or(last_raw_cursor);
                // Skip a single undecodable row rather than `?`-failing the whole
                // Today read (see list_top_candidates for the rationale).
                match map_candidate_row(row) {
                    Ok(candidate) => {
                        decoded_cursors.push(
                            row_cursor.unwrap_or_else(|| candidate_surfaced_cursor(&candidate)),
                        );
                        out.push(candidate);
                    },
                    Err(err) => {
                        let cid = row
                            .get::<_, String>(0)
                            .unwrap_or_else(|_| "<unknown>".to_string());
                        tracing::warn!(
                            candidate_id = %cid,
                            error = %err,
                            "skipping undecodable resurfacing candidate row in list_surfaced",
                        );
                    },
                }
            }
            let has_decodable_overflow = out.len() > requested;
            let has_more = has_decodable_overflow || raw_seen >= fetch_limit;
            if has_decodable_overflow {
                out.truncate(requested);
                decoded_cursors.truncate(requested);
            }
            let next_cursor = if has_more {
                if has_decodable_overflow {
                    decoded_cursors.last().cloned()
                } else {
                    last_raw_cursor
                }
            } else {
                None
            };
            Ok(SurfacedPage {
                candidates: out,
                total,
                limit: requested,
                offset,
                has_more,
                next_cursor,
            })
        })
        .await
        .context("resurfacing list_surfaced_page task panicked")?
    }

    /// Compact, authoritative identity for the bounded surfaced slice consumed
    /// by the canonical union. The query uses the page's exact source order and
    /// cap while the store mutex prevents a concurrent mutation, so callers
    /// retain only one row and one digest rather than materializing the slice.
    pub async fn surfaced_source_generation_token(
        &self,
        principal: &str,
        workspace: &str,
        as_of_ms: i64,
        limit: usize,
    ) -> Result<String> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let limit = limit.max(1).min(1_000);
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut statement = conn.prepare(&format!(
                "SELECT {CANDIDATE_COLUMNS} FROM resurfacing_candidates \
                 WHERE principal = ? AND workspace = ? AND state = ? \
                 ORDER BY COALESCE(last_surfaced_at, 0) DESC, salience_score DESC, candidate_id ASC \
                 LIMIT ?"
            ))?;
            let mut rows = statement.query(params![
                principal,
                workspace,
                CandidateState::Surfaced.as_str(),
                limit as i64,
            ])?;
            let mut digest = blake3::Hasher::new();
            digest.update(b"resurfacing-surfaced-source-generation-v1\0");
            let mut row_count = 0_u64;
            while let Some(row) = rows.next()? {
                row_count = row_count.saturating_add(1);
                for column in 0..19 {
                    update_generation_token_value(&mut digest, row.get_ref(column)?);
                }
                let last_surfaced_at: Option<i64> = row.get(12)?;
                match last_surfaced_at {
                    Some(source_at) => {
                        digest.update(&[1]);
                        let age_days = as_of_ms.saturating_sub(source_at).max(0) as f64
                            / 86_400_000.0;
                        digest.update(
                            &crate::magician_v2::attention::learning::actionability::bucket_age_days(
                                age_days,
                            )
                            .to_bits()
                            .to_le_bytes(),
                        );
                    },
                    None => {
                        digest.update(&[0]);
                    },
                }
                digest.update(&[0xff]);
            }
            digest.update(&row_count.to_le_bytes());
            Ok(format!(
                "resurfacing-surfaced-v1:{}",
                digest.finalize().to_hex()
            ))
        })
        .await
        .context("resurfacing surfaced source-generation task panicked")?
    }

    /// The surfaced lane's total, plus every surfaced `comm` source reference
    /// in it.
    ///
    /// Cross-lane de-duplication hides a Worth-a-look candidate only when an
    /// active Follow-up already owns its `(provider, account_alias, thread_id)`.
    /// Only `comm` rows carry that identity, and only their `source_ref`
    /// decides it — so the lane-wide count of hidden rows needs one narrow
    /// column from a subset of the lane, not the ~20-column candidate rows the
    /// page reader decodes and not a page-at-a-time walk of the whole lane.
    ///
    /// Returned rather than counted here on purpose: the caller compares with
    /// the same parser the page does, so a total and its page can never
    /// disagree about which rows are hidden.
    pub async fn list_surfaced_comm_source_refs(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<SurfacedCommSourceRefs> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let total = conn
                .query_row(
                    "SELECT COUNT(*) FROM resurfacing_candidates \
                     WHERE principal = ? AND workspace = ? AND state = ?",
                    params![principal, workspace, CandidateState::Surfaced.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .context("counting surfaced resurfacing candidates")?
                .max(0) as u64;
            let mut stmt = conn.prepare(
                "SELECT source_ref FROM resurfacing_candidates \
                 WHERE principal = ? AND workspace = ? AND state = ? AND source_kind = ?",
            )?;
            let mut rows = stmt.query(params![
                principal,
                workspace,
                CandidateState::Surfaced.as_str(),
                SourceKind::Comm.as_str(),
            ])?;
            let mut comm_source_refs = Vec::new();
            while let Some(row) = rows.next()? {
                comm_source_refs.push(row.get::<_, String>(0)?);
            }
            Ok(SurfacedCommSourceRefs {
                total,
                comm_source_refs,
            })
        })
        .await
        .context("resurfacing list_surfaced_comm_source_refs task panicked")?
    }

    /// Back-compat wrapper for callers that only need the first surfaced page.
    pub async fn list_surfaced(
        &self,
        principal: &str,
        workspace: &str,
        limit: usize,
    ) -> Result<Vec<Candidate>> {
        Ok(self
            .list_surfaced_page(principal, workspace, limit, 0, None)
            .await?
            .candidates)
    }

    /// Persist the LLM curator's phrasing for a surfaced candidate: the
    /// one-line surface `line` + a short `why` string. Kept in a SEPARATE table
    /// from `resurfacing_candidates` so the `Candidate` row (deterministic
    /// scoring state) never carries LLM-authored text; the `today` read joins
    /// it back in and falls back to the candidate title + a signal-derived
    /// "why now" when no phrasing row exists. Idempotent on `candidate_id`.
    pub async fn upsert_phrasing(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        line: &str,
        why: &str,
        content_revision: Option<&str>,
        now: i64,
    ) -> Result<()> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate_id = candidate_id.to_string();
        let line = line.to_string();
        let why = why.to_string();
        let content_revision = content_revision.map(str::to_string);
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let current_revision: Option<Option<String>> = {
                let mut stmt = conn.prepare(
                    "SELECT content_revision FROM resurfacing_candidates \
                     WHERE candidate_id = ? AND principal = ? AND workspace = ?",
                )?;
                let mut rows = stmt.query(params![candidate_id, principal, workspace])?;
                rows.next()?.map(|row| row.get(0)).transpose()?
            };
            if current_revision
                .as_ref()
                .is_some_and(|current| current != &content_revision)
            {
                // A curator can finish after the source has been re-distilled.
                // Never let its stale phrasing overwrite the current revision.
                return Ok(());
            }
            conn.execute(
                "INSERT INTO resurfacing_phrasing (
                    candidate_id, principal, workspace, line, why,
                    content_revision, at
                ) VALUES (?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(principal, workspace, candidate_id) DO UPDATE SET
                    line = excluded.line,
                    why = excluded.why,
                    content_revision = excluded.content_revision,
                    at = excluded.at",
                params![
                    candidate_id,
                    principal,
                    workspace,
                    line,
                    why,
                    content_revision,
                    now
                ],
            )
            .context("upserting resurfacing phrasing")?;
            Ok(())
        })
        .await
        .context("resurfacing upsert_phrasing task panicked")?
    }

    pub async fn put_memory_applications(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        content_revision: &str,
        memory_revision: &str,
        judgement: &crate::magician_v2::attention::resurfacing::memory_effects::MemoryJudgement,
        now: i64,
    ) -> Result<()> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate_id = candidate_id.to_string();
        let content_revision = content_revision.to_string();
        let memory_revision = memory_revision.to_string();
        let judgement_json =
            serde_json::to_string(judgement).context("serializing memory application judgement")?;
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            conn.execute(
                "INSERT INTO resurfacing_memory_applications (
                    principal, workspace, candidate_id, content_revision,
                    memory_revision, judgement_json, recorded_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(principal, workspace, candidate_id, content_revision, memory_revision)
                DO UPDATE SET judgement_json = excluded.judgement_json, recorded_at = excluded.recorded_at",
                params![
                    principal,
                    workspace,
                    candidate_id,
                    content_revision,
                    memory_revision,
                    judgement_json,
                    now
                ],
            )
            .context("upserting memory applications")?;
            Ok(())
        })
        .await
        .context("resurfacing put_memory_applications task panicked")?
    }

    pub async fn get_memory_applications(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        content_revision: &str,
        memory_revision: &str,
    ) -> Result<Option<crate::magician_v2::attention::resurfacing::memory_effects::MemoryJudgement>>
    {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate_id = candidate_id.to_string();
        let content_revision = content_revision.to_string();
        let memory_revision = memory_revision.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut stmt = conn.prepare(
                "SELECT judgement_json FROM resurfacing_memory_applications
                 WHERE principal = ? AND workspace = ? AND candidate_id = ?
                   AND content_revision = ? AND memory_revision = ?",
            )?;
            let mut rows = stmt.query(params![
                principal,
                workspace,
                candidate_id,
                content_revision,
                memory_revision
            ])?;
            let Some(row) = rows.next()? else {
                return Ok(None);
            };
            let json: String = row.get(0)?;
            let judgement =
                serde_json::from_str(&json).context("decoding persisted memory judgement")?;
            Ok(Some(judgement))
        })
        .await
        .context("resurfacing get_memory_applications task panicked")?
    }

    pub fn list_recent_memory_judgements_sync(
        &self,
        principal: &str,
        workspace: &str,
        limit: usize,
    ) -> Result<Vec<crate::magician_v2::attention::resurfacing::memory_effects::MemoryJudgement>>
    {
        let limit = limit.clamp(1, 500);
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = conn.prepare(
            "SELECT judgement_json FROM resurfacing_memory_applications
             WHERE principal = ? AND workspace = ?
             ORDER BY recorded_at DESC
             LIMIT ?",
        )?;
        let mut rows = stmt.query(params![principal, workspace, limit as i64])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let json: String = row.get(0)?;
            if let Ok(judgement) = serde_json::from_str(&json) {
                out.push(judgement);
            }
        }
        Ok(out)
    }

    pub async fn get_latest_memory_applications(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
    ) -> Result<Option<crate::magician_v2::attention::resurfacing::memory_effects::MemoryJudgement>>
    {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate_id = candidate_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut stmt = conn.prepare(
                "SELECT judgement_json FROM resurfacing_memory_applications
                 WHERE principal = ? AND workspace = ? AND candidate_id = ?
                 ORDER BY recorded_at DESC
                 LIMIT 1",
            )?;
            let mut rows = stmt.query(params![principal, workspace, candidate_id])?;
            let Some(row) = rows.next()? else {
                return Ok(None);
            };
            let json: String = row.get(0)?;
            let judgement = serde_json::from_str(&json)
                .context("decoding latest persisted memory judgement")?;
            Ok(Some(judgement))
        })
        .await
        .context("resurfacing get_latest_memory_applications task panicked")?
    }

    pub async fn list_latest_memory_judgements(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<
        std::collections::HashMap<
            String,
            crate::magician_v2::attention::resurfacing::memory_effects::MemoryJudgement,
        >,
    > {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut stmt = conn.prepare(
                "SELECT candidate_id, judgement_json FROM resurfacing_memory_applications
                 WHERE principal = ? AND workspace = ?
                 ORDER BY recorded_at DESC",
            )?;
            let mut rows = stmt.query(params![principal, workspace])?;
            let mut out = std::collections::HashMap::new();
            while let Some(row) = rows.next()? {
                let candidate_id: String = row.get(0)?;
                if out.contains_key(&candidate_id) {
                    continue;
                }
                let json: String = row.get(1)?;
                if let Ok(judgement) = serde_json::from_str(&json) {
                    out.insert(candidate_id, judgement);
                }
            }
            Ok(out)
        })
        .await
        .context("resurfacing list_latest_memory_judgements task panicked")?
    }

    /// Newest judgement for this candidate at an exact `content_revision`.
    pub async fn get_memory_applications_for_content_revision(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        content_revision: &str,
    ) -> Result<Option<crate::magician_v2::attention::resurfacing::memory_effects::MemoryJudgement>>
    {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate_id = candidate_id.to_string();
        let content_revision = content_revision.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut stmt = conn.prepare(
                "SELECT judgement_json FROM resurfacing_memory_applications
                 WHERE principal = ? AND workspace = ? AND candidate_id = ?
                   AND content_revision = ?
                 ORDER BY recorded_at DESC
                 LIMIT 1",
            )?;
            let mut rows = stmt.query(params![
                principal,
                workspace,
                candidate_id,
                content_revision,
            ])?;
            let Some(row) = rows.next()? else {
                return Ok(None);
            };
            let json: String = row.get(0)?;
            let judgement = serde_json::from_str(&json)
                .context("decoding content-revision persisted memory judgement")?;
            Ok(Some(judgement))
        })
        .await
        .context("resurfacing get_memory_applications_for_content_revision task panicked")?
    }

    pub async fn list_dismissed_candidate_ids(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<String>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut stmt = conn.prepare(
                "SELECT candidate_id FROM resurfacing_candidates
                 WHERE principal = ? AND workspace = ? AND state = ?
                 ORDER BY COALESCE(last_surfaced_at, last_scored_at) DESC
                 LIMIT 500",
            )?;
            let mut rows = stmt.query(params![
                principal,
                workspace,
                CandidateState::Dismissed.as_str()
            ])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push(row.get::<_, String>(0)?);
            }
            Ok(out)
        })
        .await
        .context("resurfacing list_dismissed_candidate_ids task panicked")?
    }

    pub async fn list_recent_memory_conflicts(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<MemoryConflictSurface>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut stmt = conn.prepare(
                "SELECT judgement_json FROM resurfacing_memory_applications
                 WHERE principal = ? AND workspace = ?
                 ORDER BY recorded_at DESC
                 LIMIT 200",
            )?;
            let mut rows = stmt.query(params![principal, workspace])?;
            let mut out = Vec::new();
            let mut seen = std::collections::HashSet::<String>::new();
            while let Some(row) = rows.next()? {
                let json: String = row.get(0)?;
                let Ok(judgement) = serde_json::from_str::<
                    crate::magician_v2::attention::resurfacing::memory_effects::MemoryJudgement,
                >(&json) else {
                    continue;
                };
                for conflict in judgement.conflicts {
                    if seen.insert(conflict.memory_key.clone()) {
                        out.push(MemoryConflictSurface {
                            memory_key: conflict.memory_key,
                            rationale: conflict.rationale,
                            agree_count: conflict.agree_count,
                            disagree_count: conflict.disagree_count,
                        });
                    }
                }
            }
            Ok(out)
        })
        .await
        .context("resurfacing list_recent_memory_conflicts task panicked")?
    }

    /// Read the LLM curator's stored phrasing for a candidate within a scope,
    /// returning `(line, why)` or `None` when the deterministic path surfaced
    /// it (no phrasing was written). The `today` read uses this to prefer the
    /// curator's phrasing over the generic signal-derived fallback.
    pub async fn get_phrasing(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
    ) -> Result<Option<(String, String)>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate_id = candidate_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut stmt = conn.prepare(
                "SELECT p.line, p.why FROM resurfacing_phrasing p \
                 WHERE p.candidate_id = ? AND p.principal = ? AND p.workspace = ? \
                   AND p.content_revision IS ( \
                       SELECT c.content_revision FROM resurfacing_candidates c \
                       WHERE c.candidate_id = p.candidate_id \
                         AND c.principal = p.principal AND c.workspace = p.workspace \
                   )",
            )?;
            let mut rows = stmt.query(params![candidate_id, principal, workspace])?;
            let mut out = std::collections::HashMap::new();
            if let Some(row) = rows.next()? {
                out.insert(
                    candidate_id.clone(),
                    (row.get::<_, String>(0)?, row.get::<_, String>(1)?),
                );
            }
            connections::overlay_connection_phrasing(
                &conn,
                &principal,
                &workspace,
                std::slice::from_ref(&candidate_id),
                &mut out,
            )?;
            Ok(out.remove(&candidate_id))
        })
        .await
        .context("resurfacing get_phrasing task panicked")?
    }

    /// Same revision-bound phrasing lookup as [`Self::get_phrasing`], for a
    /// whole page of candidates in one round trip.
    ///
    /// The list endpoints render up to a hundred cards per request and used to
    /// call the single-candidate form once per card — a hundred
    /// `spawn_blocking` hops each taking the store mutex, serialised inside one
    /// HTTP request. Candidates with no current-revision phrasing are simply
    /// absent from the map, which is the `Ok(None)` of the single form.
    pub async fn get_phrasing_batch(
        &self,
        principal: &str,
        workspace: &str,
        candidate_ids: &[String],
    ) -> Result<std::collections::HashMap<String, (String, String)>> {
        if candidate_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate_ids = candidate_ids.to_vec();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut out = std::collections::HashMap::with_capacity(candidate_ids.len());
            for ids in candidate_ids.chunks(EMBEDDING_SNAPSHOT_SQL_CHUNK_SIZE) {
                let placeholders = std::iter::repeat_n("?", ids.len())
                    .collect::<Vec<_>>()
                    .join(",");
                let sql = format!(
                    "SELECT p.candidate_id, p.line, p.why FROM resurfacing_phrasing p \
                     WHERE p.principal = ? AND p.workspace = ? \
                       AND p.candidate_id IN ({placeholders}) \
                       AND p.content_revision IS ( \
                           SELECT c.content_revision FROM resurfacing_candidates c \
                           WHERE c.candidate_id = p.candidate_id \
                             AND c.principal = p.principal AND c.workspace = p.workspace \
                       )"
                );
                let mut values = Vec::with_capacity(ids.len() + 2);
                values.push(principal.clone());
                values.push(workspace.clone());
                values.extend(ids.iter().cloned());
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt.query_map(params_from_iter(values), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?;
                for row in rows {
                    let (candidate_id, line, why) = row?;
                    out.insert(candidate_id, (line, why));
                }
            }
            connections::overlay_connection_phrasing(
                &conn,
                &principal,
                &workspace,
                &candidate_ids,
                &mut out,
            )?;
            Ok(out)
        })
        .await
        .context("resurfacing get_phrasing_batch task panicked")?
    }

    /// Same revision-bound recommendation lookup as
    /// [`Self::get_recommendation`], for a whole page of candidates in one
    /// round trip. See [`Self::get_phrasing_batch`] for why.
    pub async fn get_recommendation_batch(
        &self,
        principal: &str,
        workspace: &str,
        candidate_ids: &[String],
    ) -> Result<std::collections::HashMap<String, ResurfacingRecommendation>> {
        if candidate_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate_ids = candidate_ids.to_vec();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut out = std::collections::HashMap::with_capacity(candidate_ids.len());
            for ids in candidate_ids.chunks(EMBEDDING_SNAPSHOT_SQL_CHUNK_SIZE) {
                let placeholders = std::iter::repeat_n("?", ids.len())
                    .collect::<Vec<_>>()
                    .join(",");
                let sql = format!(
                    "SELECT r.candidate_id, r.action_kind, r.label, r.rationale, \
                            r.confidence, r.content_revision, r.source \
                     FROM resurfacing_recommendations r \
                     JOIN resurfacing_candidates c \
                       ON c.principal = r.principal AND c.workspace = r.workspace \
                      AND c.candidate_id = r.candidate_id \
                     WHERE r.principal = ? AND r.workspace = ? \
                       AND r.candidate_id IN ({placeholders}) \
                       AND r.content_revision IS c.content_revision"
                );
                let mut values = Vec::with_capacity(ids.len() + 2);
                values.push(principal.clone());
                values.push(workspace.clone());
                values.extend(ids.iter().cloned());
                let mut stmt = conn.prepare(&sql)?;
                let rows = stmt.query_map(params_from_iter(values), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, f32>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                })?;
                for row in rows {
                    let (candidate_id, action_kind, label, rationale, confidence, revision, source) =
                        row?;
                    out.insert(
                        candidate_id,
                        ResurfacingRecommendation {
                            kind: ResurfacingActionKind::from_str(&action_kind)?,
                            label,
                            rationale,
                            confidence,
                            content_revision: revision,
                            source: ResurfacingRecommendationSource::from_str(&source)?,
                        },
                    );
                }
            }
            Ok(out)
        })
        .await
        .context("resurfacing get_recommendation_batch task panicked")?
    }

    /// Persist one capability-only recommendation for the candidate's current
    /// content revision. A late curator response for an older revision is
    /// ignored, matching the phrasing guard above.
    pub async fn upsert_recommendation(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        recommendation: &ResurfacingRecommendation,
        now: i64,
    ) -> Result<()> {
        if !recommendation.confidence.is_finite()
            || !(0.0..=1.0).contains(&recommendation.confidence)
        {
            anyhow::bail!("resurfacing recommendation confidence must be between 0 and 1");
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate_id = candidate_id.to_string();
        let action_kind = recommendation.kind.as_str().to_string();
        let label = bounded_action_field(&recommendation.label, 80);
        let rationale = bounded_action_field(&recommendation.rationale, 240);
        let confidence = recommendation.confidence;
        let source = recommendation.source.as_str().to_string();
        let content_revision = recommendation.content_revision.clone();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let current = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM resurfacing_candidates \
                 WHERE principal = ? AND workspace = ? AND candidate_id = ? \
                   AND content_revision IS ?)",
                params![principal, workspace, candidate_id, content_revision],
                |row| row.get::<_, i64>(0),
            )? != 0;
            if !current {
                return Ok(());
            }
            conn.execute(
                "INSERT INTO resurfacing_recommendations ( \
                    candidate_id, principal, workspace, action_kind, label, rationale, \
                    confidence, source, content_revision, shown_at, at \
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?) \
                 ON CONFLICT(principal, workspace, candidate_id) DO UPDATE SET \
                    action_kind = excluded.action_kind, label = excluded.label, \
                    rationale = excluded.rationale, confidence = excluded.confidence, \
                    source = excluded.source, content_revision = excluded.content_revision, \
                    shown_at = CASE \
                        WHEN resurfacing_recommendations.action_kind = excluded.action_kind \
                         AND resurfacing_recommendations.content_revision IS excluded.content_revision \
                         AND resurfacing_recommendations.label = excluded.label \
                         AND resurfacing_recommendations.rationale = excluded.rationale \
                         AND resurfacing_recommendations.confidence = excluded.confidence \
                         AND resurfacing_recommendations.source = excluded.source \
                        THEN resurfacing_recommendations.shown_at ELSE NULL END, \
                    at = excluded.at",
                params![
                    candidate_id,
                    principal,
                    workspace,
                    action_kind,
                    label,
                    rationale,
                    confidence,
                    source,
                    content_revision,
                    now,
                ],
            )
            .context("upserting resurfacing recommendation")?;
            Ok(())
        })
        .await
        .context("resurfacing upsert_recommendation task panicked")?
    }

    /// Read only a recommendation that still matches the candidate's current
    /// content revision. Stale rows remain harmless until retention cleanup.
    pub async fn get_recommendation(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
    ) -> Result<Option<ResurfacingRecommendation>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate_id = candidate_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut stmt = conn.prepare(
                "SELECT r.action_kind, r.label, r.rationale, r.confidence, \
                        r.content_revision, r.source \
                 FROM resurfacing_recommendations r \
                 JOIN resurfacing_candidates c \
                   ON c.principal = r.principal AND c.workspace = r.workspace \
                  AND c.candidate_id = r.candidate_id \
                 WHERE r.principal = ? AND r.workspace = ? AND r.candidate_id = ? \
                   AND r.content_revision IS c.content_revision",
            )?;
            let mut rows = stmt.query(params![principal, workspace, candidate_id])?;
            let Some(row) = rows.next()? else {
                return Ok(None);
            };
            let action_kind = row.get::<_, String>(0)?;
            let source = row.get::<_, String>(5)?;
            Ok(Some(ResurfacingRecommendation {
                kind: ResurfacingActionKind::from_str(&action_kind)?,
                label: row.get(1)?,
                rationale: row.get(2)?,
                confidence: row.get(3)?,
                content_revision: row.get(4)?,
                source: ResurfacingRecommendationSource::from_str(&source)?,
            }))
        })
        .await
        .context("resurfacing get_recommendation task panicked")?
    }

    /// Record the first actual presentation of a revision-bound recommendation.
    /// Repeated list polling is deduplicated by the `shown_at IS NULL` update.
    pub async fn record_recommendation_shown(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        recommendation: &ResurfacingRecommendation,
        now: i64,
    ) -> Result<bool> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate_id = candidate_id.to_string();
        let action_kind = recommendation.kind.as_str().to_string();
        let content_revision = recommendation.content_revision.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tx = conn
                .transaction()
                .context("opening resurfacing recommendation shown transaction")?;
            let changed = tx.execute(
                "UPDATE resurfacing_recommendations SET shown_at = ? \
                 WHERE principal = ? AND workspace = ? AND candidate_id = ? \
                   AND action_kind = ? AND content_revision IS ? AND shown_at IS NULL \
                   AND EXISTS (SELECT 1 FROM resurfacing_candidates c \
                       WHERE c.principal = resurfacing_recommendations.principal \
                         AND c.workspace = resurfacing_recommendations.workspace \
                         AND c.candidate_id = resurfacing_recommendations.candidate_id \
                         AND c.content_revision IS resurfacing_recommendations.content_revision)",
                params![
                    now,
                    principal,
                    workspace,
                    candidate_id,
                    action_kind,
                    content_revision,
                ],
            )?;
            if changed > 0 {
                insert_contextual_action_event(
                    &tx,
                    &principal,
                    &workspace,
                    &candidate_id,
                    &action_kind,
                    content_revision.as_deref(),
                    "recommended",
                    None,
                    None,
                    now,
                )?;
            }
            tx.commit()
                .context("committing resurfacing recommendation shown transaction")?;
            Ok(changed > 0)
        })
        .await
        .context("resurfacing record_recommendation_shown task panicked")?
    }

    /// Record an explicit UI interaction for a read-style recommendation. The
    /// recommendation must already have been shown, still match the current
    /// revision, and completion must follow selection. The structured result
    /// distinguishes a new record, an idempotent duplicate, and invalid order.
    pub async fn record_recommendation_interaction(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        action_kind: ResurfacingActionKind,
        content_revision: Option<&str>,
        event_type: &str,
        now: i64,
    ) -> Result<ResurfacingRecommendationInteractionResult> {
        if !matches!(event_type, "selected" | "completed") {
            anyhow::bail!("unsupported resurfacing recommendation event: {event_type}");
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate_id = candidate_id.to_string();
        let action_kind = action_kind.as_str().to_string();
        let content_revision = content_revision.map(str::to_string);
        let event_type = event_type.to_string();
        tokio::task::spawn_blocking(move || {
            let mut conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tx = conn
                .transaction()
                .context("opening resurfacing recommendation interaction transaction")?;
            if matching_recommendation_kind(
                &tx,
                &principal,
                &workspace,
                &candidate_id,
                &action_kind,
                content_revision.as_deref(),
            )?
            .is_none()
            {
                return Ok(ResurfacingRecommendationInteractionResult::NotShown);
            }
            if event_type == "completed"
                && !recommendation_event_exists(
                    &tx,
                    &principal,
                    &workspace,
                    &candidate_id,
                    &action_kind,
                    content_revision.as_deref(),
                    "selected",
                )?
            {
                return Ok(ResurfacingRecommendationInteractionResult::OutOfOrder);
            }
            let existed = recommendation_event_exists(
                &tx,
                &principal,
                &workspace,
                &candidate_id,
                &action_kind,
                content_revision.as_deref(),
                &event_type,
            )?;
            if !existed {
                insert_contextual_action_event(
                    &tx,
                    &principal,
                    &workspace,
                    &candidate_id,
                    &action_kind,
                    content_revision.as_deref(),
                    &event_type,
                    None,
                    None,
                    now,
                )?;
            }
            tx.commit()
                .context("committing resurfacing recommendation interaction transaction")?;
            Ok(if existed {
                ResurfacingRecommendationInteractionResult::Duplicate
            } else {
                ResurfacingRecommendationInteractionResult::Recorded
            })
        })
        .await
        .context("resurfacing record_recommendation_interaction task panicked")?
    }

    /// Persist a candidate's embedding vector (JSON-serialized) for dismiss
    /// neighbor-suppression. Idempotent on `candidate_id`: a re-score overwrites
    /// the previous vector. Stored in a table SEPARATE from
    /// `resurfacing_candidates` so the scoring row never carries a large blob and
    /// scopes with no embedder simply have no rows here (suppression no-ops).
    pub async fn upsert_embedding(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        vec: &[f32],
    ) -> Result<()> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate_id = candidate_id.to_string();
        let vec = vec.to_vec();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut stmt = conn.prepare(
                "SELECT content_digest FROM resurfacing_candidates \
                 WHERE candidate_id = ? AND principal = ? AND workspace = ?",
            )?;
            let mut rows = stmt.query(params![candidate_id, principal, workspace])?;
            let content_digest = rows
                .next()?
                .map(|row| row.get::<_, String>(0))
                .transpose()?;
            drop(rows);
            drop(stmt);
            upsert_embedding_row(
                &conn,
                &principal,
                &workspace,
                &candidate_id,
                content_digest.as_deref(),
                None,
                &vec,
                unix_now_seconds(),
            )
        })
        .await
        .context("resurfacing upsert_embedding task panicked")?
    }

    /// Persist an embedding together with the exact content digest it was
    /// computed from. Background centrality can generate a vector before the
    /// scorer refreshes the candidate row, so it must supply this association
    /// explicitly rather than inheriting a potentially older candidate digest.
    pub async fn upsert_embedding_for_digest(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        content_digest: &str,
        embedding_contract: &str,
        vec: &[f32],
    ) -> Result<()> {
        self.upsert_embedding_for_digest_at(
            principal,
            workspace,
            candidate_id,
            content_digest,
            embedding_contract,
            vec,
            unix_now_seconds(),
        )
        .await
    }

    /// Promote a digest-bound embedding using the caller's logical timestamp.
    /// The scorer uses the same injected clock for the candidate and its vector,
    /// so retention cannot age out one while treating the other as newly cached.
    pub(super) async fn upsert_embedding_for_digest_at(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
        content_digest: &str,
        embedding_contract: &str,
        vec: &[f32],
        updated_at: i64,
    ) -> Result<()> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate_id = candidate_id.to_string();
        let content_digest = content_digest.to_string();
        let embedding_contract = embedding_contract.to_string();
        let vec = vec.to_vec();
        tokio::task::spawn_blocking(move || {
            let mut conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tx = conn
                .transaction()
                .context("opening embedding promotion transaction")?;
            upsert_embedding_row(
                &tx,
                &principal,
                &workspace,
                &candidate_id,
                Some(&content_digest),
                Some(&embedding_contract),
                &vec,
                updated_at,
            )?;
            tx.execute(
                "DELETE FROM resurfacing_embedding_cache
                 WHERE principal = ? AND workspace = ? AND candidate_id = ?
                   AND content_digest = ? AND embedding_contract = ?",
                params![
                    principal,
                    workspace,
                    candidate_id,
                    content_digest,
                    embedding_contract
                ],
            )?;
            prune_unattached_embedding_cache(
                &tx,
                &principal,
                &workspace,
                UNATTACHED_EMBEDDING_CACHE_CAP,
            )?;
            prune_versioned_reference_cache(
                &tx,
                &principal,
                &workspace,
                UNATTACHED_EMBEDDING_CACHE_CAP,
            )?;
            tx.commit()
                .context("committing embedding promotion transaction")
        })
        .await
        .context("resurfacing upsert_embedding_for_digest task panicked")?
    }

    /// Persist a bounded successful embedding prefix in one transaction and
    /// prune the pre-candidate cache once. This is the monotonic progress unit
    /// used by background centrality after a partial timeout/cancellation.
    pub async fn upsert_embeddings_for_digests(
        &self,
        principal: &str,
        workspace: &str,
        embeddings: &[CandidateEmbeddingSnapshot],
    ) -> Result<usize> {
        if embeddings.is_empty() {
            return Ok(0);
        }
        if embeddings.len() > 512 {
            anyhow::bail!("resurfacing embedding bulk upsert exceeds 512-row bound");
        }
        for embedding in embeddings {
            let norm_squared = embedding
                .embedding
                .iter()
                .map(|value| f64::from(*value) * f64::from(*value))
                .sum::<f64>();
            if embedding.candidate_id.is_empty()
                || embedding.content_digest.is_empty()
                || embedding.embedding_contract.is_empty()
                || embedding.embedding.is_empty()
                || embedding.embedding.iter().any(|value| !value.is_finite())
                || !norm_squared.is_finite()
                || norm_squared <= f64::EPSILON
            {
                anyhow::bail!("resurfacing embedding bulk upsert contains an invalid row");
            }
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let embeddings = embeddings.to_vec();
        tokio::task::spawn_blocking(move || {
            let mut conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tx = conn
                .transaction()
                .context("opening resurfacing embedding bulk transaction")?;
            let mut persisted = 0usize;
            for embedding in &embeddings {
                upsert_reference_cache_row(&tx, &principal, &workspace, embedding)?;
                persisted = persisted.saturating_add(1);
            }
            prune_versioned_reference_cache(
                &tx,
                &principal,
                &workspace,
                UNATTACHED_EMBEDDING_CACHE_CAP,
            )?;
            tx.commit()
                .context("committing resurfacing embedding bulk transaction")?;
            Ok::<_, anyhow::Error>(persisted)
        })
        .await
        .context("resurfacing upsert_embeddings_for_digests task panicked")?
    }

    /// Read a candidate's stored embedding vector within a scope, or `None` when
    /// none was ever persisted (e.g. the scope had no embedder when scored).
    pub async fn get_embedding(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
    ) -> Result<Option<Vec<f32>>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate_id = candidate_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            read_embedding(&conn, &principal, &workspace, &candidate_id)
        })
        .await
        .context("resurfacing get_embedding task panicked")?
    }

    /// List every `(candidate_id, vector)` stored in a scope. Bounded by the
    /// capped candidate set the scorer maintains, so it fits comfortably in
    /// memory; backs dismiss neighbor-suppression's scope-wide cosine sweep.
    pub async fn list_embeddings(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<(String, Vec<f32>)>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            read_all_embeddings(&conn, &principal, &workspace)
        })
        .await
        .context("resurfacing list_embeddings task panicked")?
    }

    /// Bulk-load only the requested digest/contract-qualified vectors for safe
    /// centrality reuse. Rows need not yet have a candidate: successful
    /// one-input embedding units are persisted before scoring so cancellation
    /// can resume rather than recompute a prefix. SQL reads are chunked and the
    /// connection mutex is released before JSON deserialization.
    pub async fn list_candidate_embedding_snapshots_for_ids(
        &self,
        principal: &str,
        workspace: &str,
        candidate_ids: &[String],
    ) -> Result<Vec<CandidateEmbeddingSnapshot>> {
        if candidate_ids.is_empty() {
            return Ok(Vec::new());
        }
        if candidate_ids.len() > 512 {
            anyhow::bail!("resurfacing embedding snapshot request exceeds 512 ids");
        }
        let store = self.clone();
        let cleanup_store = store.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let cleanup_principal = principal.clone();
        let cleanup_workspace = workspace.clone();
        let candidate_ids: Vec<String> = candidate_ids.to_vec();
        let raw_rows = tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut raw_rows = Vec::new();
            for ids in candidate_ids.chunks(EMBEDDING_SNAPSHOT_SQL_CHUNK_SIZE) {
                let placeholders = std::iter::repeat_n("?", ids.len())
                    .collect::<Vec<_>>()
                    .join(",");
                for (table, is_versioned_cache) in [
                    ("resurfacing_embeddings", false),
                    ("resurfacing_embedding_cache", true),
                ] {
                    let sql = format!(
                        "SELECT candidate_id, content_digest, embedding_contract, vec \
                         FROM {table} \
                         WHERE principal = ? AND workspace = ? \
                           AND content_digest IS NOT NULL \
                           AND embedding_contract IS NOT NULL \
                           AND candidate_id IN ({placeholders}) \
                         ORDER BY candidate_id ASC"
                    );
                    let mut values = Vec::with_capacity(ids.len() + 2);
                    values.push(principal.clone());
                    values.push(workspace.clone());
                    values.extend(ids.iter().cloned());
                    let mut stmt = conn.prepare(&sql)?;
                    let rows = stmt.query_map(params_from_iter(values), |row| {
                        Ok((
                            is_versioned_cache,
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    })?;
                    raw_rows.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
                }
            }
            Ok::<_, anyhow::Error>(raw_rows)
        })
        .await
        .context("resurfacing list_candidate_embedding_snapshots_for_ids task panicked")??;

        let mut snapshots = Vec::with_capacity(raw_rows.len());
        let mut skipped = 0usize;
        let mut malformed = Vec::new();
        for (is_versioned_cache, candidate_id, content_digest, embedding_contract, raw) in raw_rows
        {
            let embedding: Vec<f32> = match serde_json::from_str::<Vec<f32>>(&raw) {
                Ok(embedding) if valid_stored_vector_without_dims(&embedding) => embedding,
                _ => {
                    skipped = skipped.saturating_add(1);
                    if skipped <= 3 {
                        tracing::warn!(
                            candidate_id,
                            "skipping malformed reusable resurfacing embedding"
                        );
                    }
                    malformed.push((is_versioned_cache, candidate_id, raw));
                    continue;
                },
            };
            snapshots.push(CandidateEmbeddingSnapshot {
                candidate_id,
                content_digest,
                embedding_contract,
                embedding,
            });
        }
        if !malformed.is_empty() {
            let cleanup = tokio::task::spawn_blocking(move || {
                let conn = cleanup_store
                    .conn
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                for (is_versioned_cache, candidate_id, raw) in malformed {
                    let table = if is_versioned_cache {
                        "resurfacing_embedding_cache"
                    } else {
                        "resurfacing_embeddings"
                    };
                    conn.execute(
                        &format!("DELETE FROM {table} WHERE principal = ? AND workspace = ? AND candidate_id = ? AND vec = ?"),
                        params![cleanup_principal, cleanup_workspace, candidate_id, raw],
                    )?;
                }
                Ok::<_, anyhow::Error>(())
            })
            .await;
            match cleanup {
                Ok(Ok(())) => {},
                Ok(Err(error)) => tracing::warn!(
                    error = %error,
                    "could not clean malformed reusable resurfacing embeddings; returning healthy rows"
                ),
                Err(error) => tracing::warn!(
                    error = %error,
                    "malformed resurfacing embedding cleanup task panicked; returning healthy rows"
                ),
            }
        }
        Ok(snapshots)
    }

    /// Append one durable dismissed-signal vector for a scope (P3a): the
    /// embedding of a just-dismissed item, kept so future NEW candidates that
    /// resemble it can be penalized at score time (see the scorer's dismissal
    /// penalty). The table is append-only; after inserting we cap the scope to
    /// its newest [`DISMISSED_SIGNAL_CAP`] rows so a heavy dismisser can't grow
    /// it without bound. `record_action`'s `Dismiss` arm writes this inline on
    /// its own transaction; this async entry point is for direct callers/tests.
    pub async fn record_dismissed_signal(
        &self,
        principal: &str,
        workspace: &str,
        vec: &[f32],
        now: i64,
    ) -> Result<()> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let vec = vec.to_vec();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            write_dismissed_signal(&conn, &principal, &workspace, None, None, &vec, now)
        })
        .await
        .context("resurfacing record_dismissed_signal task panicked")?
    }

    /// List every durable dismissed-signal vector in a scope (P3a), newest
    /// first. Bounded by [`DISMISSED_SIGNAL_CAP`], so it fits comfortably in
    /// memory; the scorer loads this once per pass and penalizes new candidates
    /// resembling any of these past dismissals.
    pub async fn list_dismissed_signals(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<Vec<f32>>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            read_dismissed_signals(&conn, &principal, &workspace)
        })
        .await
        .context("resurfacing list_dismissed_signals task panicked")?
    }

    pub async fn list_dismissed_signals_for_contract(
        &self,
        principal: &str,
        workspace: &str,
        embedding_contract: &str,
    ) -> Result<Vec<Vec<f32>>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let embedding_contract = embedding_contract.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            read_dismissed_signals_for_contract(
                &conn,
                &principal,
                &workspace,
                Some(&embedding_contract),
            )
        })
        .await
        .context("resurfacing list_dismissed_signals_for_contract task panicked")?
    }

    pub async fn list_dismissed_attention_signals_after(
        &self,
        principal: &str,
        workspace: &str,
        after_id: i64,
        before_at: i64,
        limit: usize,
    ) -> Result<Vec<ResurfacingAttentionSignal>> {
        self.list_attention_signals_after(
            "resurfacing_dismissed_signals",
            principal,
            workspace,
            after_id,
            before_at,
            limit,
        )
        .await
    }

    /// Stable post-cutoff repair tail over the feedback rows atomically written
    /// with live Worth-a-look lifecycle transitions.
    pub async fn list_feedback_attention_repairs_after(
        &self,
        principal: &str,
        workspace: &str,
        after_at: i64,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<ResurfacingFeedbackRepairRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut statement = conn.prepare(
                "SELECT id, event_id, candidate_id, action, reason, semantic_text, \
                        embedding_contract, vec, at \
                 FROM resurfacing_feedback \
                 WHERE principal = ? AND workspace = ? AND event_id IS NOT NULL \
                   AND (at > ? OR (at = ? AND id > ?)) \
                 ORDER BY at, id LIMIT ?",
            )?;
            let raw = statement
                .query_map(
                    params![principal, workspace, after_at, after_at, after_id, limit],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, Option<String>>(4)?,
                            row.get::<_, Option<String>>(5)?,
                            row.get::<_, Option<String>>(6)?,
                            row.get::<_, Option<String>>(7)?,
                            row.get::<_, i64>(8)?,
                        ))
                    },
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let mut output = Vec::new();
            for (id, event_id, candidate_id, action, reason, semantic_text, contract, vec, at) in
                raw
            {
                let action = FeedbackAction::from_str(&action).ok();
                let reason = reason.as_deref().and_then(DismissReason::parse);
                let embedding = vec
                    .as_deref()
                    .and_then(|encoded| serde_json::from_str::<Vec<f32>>(encoded).ok())
                    .filter(|vector| valid_stored_vector_without_dims(vector));
                output.push(ResurfacingFeedbackRepairRow {
                    id,
                    event_id,
                    candidate_id,
                    action,
                    reason,
                    semantic_text: semantic_text.unwrap_or_default(),
                    embedding_contract: contract.filter(|value| !value.trim().is_empty()),
                    embedding,
                    occurred_at: at,
                });
            }
            Ok(output)
        })
        .await
        .context("resurfacing feedback attention repair task panicked")?
    }

    /// Append one durable affinity-signal vector for a scope (P4a): the embedding
    /// of a just-opened/acknowledged item, kept so future NEW candidates that
    /// resemble it can be BOOSTED at score time (see the scorer's affinity boost).
    /// The positive mirror of [`record_dismissed_signal`](Self::record_dismissed_signal):
    /// the table is append-only; after inserting we cap the scope to its newest
    /// [`AFFINITY_SIGNAL_CAP`] rows so a heavy engager can't grow it without
    /// bound. `record_action`'s `Open`/`Acknowledge` arm writes this inline on its
    /// own transaction; this async entry point is for direct callers/tests.
    pub async fn record_affinity_signal(
        &self,
        principal: &str,
        workspace: &str,
        vec: &[f32],
        now: i64,
    ) -> Result<()> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let vec = vec.to_vec();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            write_affinity_signal(&conn, &principal, &workspace, None, None, &vec, now)
        })
        .await
        .context("resurfacing record_affinity_signal task panicked")?
    }

    /// List every durable affinity-signal vector in a scope (P4a), newest first.
    /// Bounded by [`AFFINITY_SIGNAL_CAP`], so it fits comfortably in memory; the
    /// scorer loads this once per pass and boosts new candidates resembling any of
    /// these past positive actions. The positive mirror of
    /// [`list_dismissed_signals`](Self::list_dismissed_signals).
    pub async fn list_affinity_signals(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<Vec<f32>>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            read_affinity_signals(&conn, &principal, &workspace)
        })
        .await
        .context("resurfacing list_affinity_signals task panicked")?
    }

    pub async fn list_affinity_signals_for_contract(
        &self,
        principal: &str,
        workspace: &str,
        embedding_contract: &str,
    ) -> Result<Vec<Vec<f32>>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let embedding_contract = embedding_contract.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            read_affinity_signals_for_contract(
                &conn,
                &principal,
                &workspace,
                Some(&embedding_contract),
            )
        })
        .await
        .context("resurfacing list_affinity_signals_for_contract task panicked")?
    }

    pub async fn list_affinity_attention_signals_after(
        &self,
        principal: &str,
        workspace: &str,
        after_id: i64,
        before_at: i64,
        limit: usize,
    ) -> Result<Vec<ResurfacingAttentionSignal>> {
        self.list_attention_signals_after(
            "resurfacing_affinity_signals",
            principal,
            workspace,
            after_id,
            before_at,
            limit,
        )
        .await
    }

    async fn list_attention_signals_after(
        &self,
        table: &'static str,
        principal: &str,
        workspace: &str,
        after_id: i64,
        before_at: i64,
        limit: usize,
    ) -> Result<Vec<ResurfacingAttentionSignal>> {
        anyhow::ensure!(
            matches!(
                table,
                "resurfacing_dismissed_signals" | "resurfacing_affinity_signals"
            ),
            "unsupported resurfacing attention signal source"
        );
        if limit == 0 {
            return Ok(Vec::new());
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let sql = format!(
                "SELECT s.id, s.embedding_contract, s.vec, s.at, s.event_id \
                 FROM {table} s WHERE s.principal = ? AND s.workspace = ? \
                   AND s.id > ? AND s.at < ? AND s.embedding_contract IS NOT NULL \
                 ORDER BY s.id ASC LIMIT ?"
            );
            let mut statement = conn.prepare(&sql)?;
            let rows = statement.query_map(
                params![
                    principal,
                    workspace,
                    after_id,
                    before_at,
                    DISMISSED_SIGNAL_CAP.max(AFFINITY_SIGNAL_CAP) as i64,
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )?;
            let raw_rows = rows.collect::<rusqlite::Result<Vec<_>>>()?;
            drop(statement);
            let mut signals = Vec::new();
            for (id, embedding_contract, encoded, occurred_at, live_event_id) in raw_rows {
                let Ok(embedding) = serde_json::from_str::<Vec<f32>>(&encoded) else {
                    tracing::warn!(id, table, "reporting malformed historical attention signal");
                    signals.push(ResurfacingAttentionSignal {
                        id,
                        embedding_contract,
                        embedding: Vec::new(),
                        occurred_at,
                        live_event_id,
                    });
                    continue;
                };
                if embedding_contract.trim().is_empty()
                    || !valid_stored_vector_without_dims(&embedding)
                {
                    tracing::warn!(id, table, "reporting invalid historical attention signal");
                    signals.push(ResurfacingAttentionSignal {
                        id,
                        embedding_contract,
                        embedding: Vec::new(),
                        occurred_at,
                        live_event_id,
                    });
                    continue;
                }
                signals.push(ResurfacingAttentionSignal {
                    id,
                    embedding_contract,
                    embedding,
                    occurred_at,
                    live_event_id,
                });
            }
            signals.truncate(limit);
            Ok(signals)
        })
        .await
        .context("resurfacing historical attention signal read task panicked")?
    }

    /// Read the per-lane engagement tallies for a scope (P4c): one
    /// `(source_kind, positive, negative)` triple per lane that has ever been
    /// acted on. `positive` counts `Open` (mark-useful) actions; `Acknowledge` is
    /// neutral; `negative` counts a penalizing `Dismiss` (reasonless or
    /// not_relevant/spam). Backs the scorer's per-lane utility multiplier and the
    /// stats endpoint. Rows whose stored `source_kind` no longer parses are
    /// skipped (defensive). Lanes never acted on simply have no row (the scorer
    /// treats a missing lane as neutral `(0, 0)`).
    pub async fn kind_engagement(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<(SourceKind, u64, u64)>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            read_kind_engagement(&conn, &principal, &workspace)
        })
        .await
        .context("resurfacing kind_engagement task panicked")?
    }

    /// Persist one background-pass run record (O1 observability): what the engine
    /// did (`kind` ∈ `{"scorer","curator","retention"}`), when it started
    /// (`started_at`, unix ms), how long it took (`duration_ms`), how much it
    /// produced (`produced` — upserted / surfaced / pruned depending on the kind),
    /// and whether it succeeded (`success` + an optional `error` string on
    /// failure). Append-only and bounded to the newest [`RUN_HISTORY_CAP`] rows per
    /// scope. The worker calls this best-effort after every tick — a failed record
    /// must never break the pass it describes. (O2 adds the read/aggregate API.)
    #[allow(clippy::too_many_arguments)]
    pub async fn record_run(
        &self,
        principal: &str,
        workspace: &str,
        kind: &str,
        started_at: i64,
        duration_ms: i64,
        produced: i64,
        success: bool,
        error: Option<&str>,
        now: i64,
    ) -> Result<()> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let kind = kind.to_string();
        let error = error.map(|e| e.to_string());
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            write_run(
                &conn,
                &principal,
                &workspace,
                &kind,
                started_at,
                duration_ms,
                produced,
                success,
                error.as_deref(),
                now,
            )
        })
        .await
        .context("resurfacing record_run task panicked")?
    }

    /// Housekeeping prune (P3b) that keeps a scope's store bounded, in one
    /// atomic transaction. Returns the total number of rows deleted across every
    /// step. Four steps run in order:
    ///
    /// 1. **Age-prune terminal candidates** — delete `acted`/`dismissed` rows
    ///    whose most recent activity (`COALESCE(last_surfaced_at,
    ///    last_scored_at, first_seen_at)`) predates `now - retention_days`. Only
    ///    terminal states are aged out; live `candidate`/`surfaced`/`snoozed`
    ///    rows are never deleted by age.
    /// 1b. **Age-prune the feedback log** — delete `resurfacing_feedback` rows
    ///    older than the same (SECONDS) retention horizon. `record_action`
    ///    stamps feedback `at` in the same seconds domain as the candidate
    ///    lifecycle timestamps, so the seconds `cutoff` applies (NOT the runs
    ///    table's millisecond cutoff). Keeps the append-only log bounded.
    /// 2. **Cap-prune** — if the scope still holds more than `candidate_cap`
    ///    rows, keep the top `candidate_cap` by "liveness" (all non-terminal
    ///    `candidate`/`surfaced`/`snoozed` states first, then salience, then
    ///    recency) and delete the overflow. Bounds a scope even when nothing is
    ///    terminal; a scope at or under the cap is untouched, so no still-live
    ///    row is dropped merely to reach the cap. Protecting `surfaced`/`snoozed`
    ///    (not just `candidate`) keeps an actively-shown or user-deferred row
    ///    from being pruned ahead of a raw candidate.
    /// 3. **Age-prune dismissed AND affinity signals** — delete durable
    ///    dismissed-signal (P3a) and affinity-signal (P4a) rows older than the
    ///    same retention horizon (both already cap by count; this also ages them
    ///    out).
    /// 4. **Age-prune run history (O1)** — delete `resurfacing_runs` rows older
    ///    than the same retention horizon (the table already caps by count; this
    ///    also ages it out). NOTE: run timestamps are in MILLISECONDS while the
    ///    other tables' cutoff is in SECONDS, so this step uses a separate
    ///    millisecond cutoff (see the inline `cutoff_ms`).
    /// 5. **Orphan cleanup** — delete embedding/phrasing rows whose
    ///    `candidate_id` no longer exists. A recent digest+contract-qualified
    ///    embedding is retained as the bounded pre-candidate resume cache;
    ///    legacy/incomplete or aged rows are removed.
    pub async fn retention_sweep(
        &self,
        principal: &str,
        workspace: &str,
        now: i64,
        retention_days: i64,
        candidate_cap: usize,
    ) -> Result<usize> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            // Saturating math so an extreme `retention_days` can't overflow: a
            // huge horizon simply pushes the cutoff far into the past (age-prune
            // becomes a no-op) rather than wrapping.
            let cutoff = now.saturating_sub(retention_days.saturating_mul(86_400));
            let mut conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tx = conn
                .transaction()
                .context("opening resurfacing retention transaction")?;
            let mut pruned = 0usize;

            // 1. Age-prune terminal candidates only (never live rows).
            pruned += tx
                .execute(
                    "DELETE FROM resurfacing_candidates \
                     WHERE principal = ? AND workspace = ? \
                     AND state IN (?, ?) \
                     AND COALESCE(last_surfaced_at, last_scored_at, first_seen_at) < ?",
                    params![
                        principal,
                        workspace,
                        CandidateState::Acted.as_str(),
                        CandidateState::Dismissed.as_str(),
                        cutoff,
                    ],
                )
                .context("age-pruning terminal resurfacing candidates")?;

            // 1b. Age-prune the feedback log. `record_action` stamps feedback
            //     `at` in the same SECONDS domain as the candidate lifecycle
            //     timestamps (it derives cooldowns as `now + *_cooldown_secs`),
            //     so the seconds `cutoff` applies here — NOT the runs table's
            //     millisecond `cutoff_ms`. Bounds the append-only feedback log
            //     over time (an action on a foreign id is already refused at
            //     write time, but genuine old feedback still ages out here).
            pruned += tx
                .execute(
                    "DELETE FROM resurfacing_feedback \
                     WHERE principal = ? AND workspace = ? AND at < ?",
                    params![principal, workspace, cutoff],
                )
                .context("age-pruning resurfacing feedback rows")?;
            pruned += tx
                .execute(
                    "DELETE FROM resurfacing_action_events \
                     WHERE principal = ? AND workspace = ? AND at < ?",
                    params![principal, workspace, cutoff],
                )
                .context("age-pruning resurfacing contextual-action events")?;
            pruned += tx
                .execute(
                    "DELETE FROM resurfacing_action_claims \
                     WHERE principal = ? AND workspace = ? AND updated_at < ?",
                    params![principal, workspace, cutoff],
                )
                .context("age-pruning resurfacing contextual-action claims")?;
            pruned += tx
                .execute(
                    "DELETE FROM resurfacing_routing_repairs \
                     WHERE principal = ? AND workspace = ? AND repaired_at < ?",
                    params![principal, workspace, cutoff],
                )
                .context("age-pruning resurfacing routing repair receipts")?;

            // 2. Cap-prune: keep the top `candidate_cap` by liveness ordering and
            //    delete the overflow. When the scope holds <= cap rows the
            //    subquery returns all of them, so `NOT IN` matches nothing and no
            //    live row is dropped merely to reach the cap. The liveness
            //    predicate protects ALL non-terminal states (candidate, surfaced,
            //    snoozed) — not just raw `candidate` — so an actively-shown or
            //    user-deferred row never ranks below a raw candidate and gets
            //    pruned first.
            pruned += tx
                .execute(
                    "DELETE FROM resurfacing_candidates \
                     WHERE principal = ? AND workspace = ? \
                     AND candidate_id NOT IN ( \
                         SELECT candidate_id FROM resurfacing_candidates \
                         WHERE principal = ? AND workspace = ? \
                         ORDER BY (state IN (?, ?, ?)) DESC, salience_score DESC, last_scored_at DESC \
                         LIMIT ? \
                     )",
                    params![
                        principal,
                        workspace,
                        principal,
                        workspace,
                        CandidateState::Candidate.as_str(),
                        CandidateState::Surfaced.as_str(),
                        CandidateState::Snoozed.as_str(),
                        candidate_cap as i64,
                    ],
                )
                .context("cap-pruning resurfacing candidates")?;

            // 3. Age-prune durable dismissed AND affinity signals past the
            //    retention horizon (P3a dismissed + P4a affinity, symmetric).
            pruned += tx
                .execute(
                    "DELETE FROM resurfacing_dismissed_signals \
                     WHERE principal = ? AND workspace = ? AND at < ?",
                    params![principal, workspace, cutoff],
                )
                .context("age-pruning resurfacing dismissed signals")?;
            pruned += tx
                .execute(
                    "DELETE FROM resurfacing_affinity_signals \
                     WHERE principal = ? AND workspace = ? AND at < ?",
                    params![principal, workspace, cutoff],
                )
                .context("age-pruning resurfacing affinity signals")?;

            // 4. Age-prune the observability run-history rows (O1). NOTE: the runs
            //    table stamps `at`/`started_at` in UNIX MILLISECONDS, while
            //    `cutoff` above is in SECONDS (matching every other table here).
            //    Compare against a separate MILLISECOND cutoff so the age test is
            //    unit-correct — using the seconds `cutoff` would keep runs ~1000x
            //    too long. Derived from the same `cutoff` (saturating so an extreme
            //    horizon can't overflow the *1000).
            let cutoff_ms = cutoff.saturating_mul(1_000);
            pruned += tx
                .execute(
                    "DELETE FROM resurfacing_runs \
                     WHERE principal = ? AND workspace = ? AND at < ?",
                    params![principal, workspace, cutoff_ms],
                )
                .context("age-pruning resurfacing run records")?;

            // 5. Orphan cleanup: drop embedding/phrasing rows whose candidate no
            //    longer exists (pruned above or otherwise), leaving no dangling
            //    side-table rows.
            pruned += tx
                .execute(
                    "DELETE FROM resurfacing_embeddings \
                     WHERE principal = ? AND workspace = ? \
                     AND (content_digest IS NULL OR embedding_contract IS NULL OR updated_at < ?) \
                     AND candidate_id NOT IN ( \
                         SELECT candidate_id FROM resurfacing_candidates \
                         WHERE principal = ? AND workspace = ? \
                     )",
                    params![principal, workspace, cutoff, principal, workspace],
                )
                .context("cleaning orphaned resurfacing embeddings")?;
            pruned += tx
                .execute(
                    "DELETE FROM resurfacing_embedding_cache \
                     WHERE principal = ? AND workspace = ? AND updated_at < ?",
                    params![principal, workspace, cutoff],
                )
                .context("age-pruning resurfacing versioned embedding cache")?;
            pruned += tx
                .execute(
                    "DELETE FROM resurfacing_phrasing \
                     WHERE principal = ? AND workspace = ? \
                     AND candidate_id NOT IN ( \
                         SELECT candidate_id FROM resurfacing_candidates \
                         WHERE principal = ? AND workspace = ? \
                     )",
                    params![principal, workspace, principal, workspace],
                )
                .context("cleaning orphaned resurfacing phrasing")?;
            pruned += tx
                .execute(
                    "DELETE FROM resurfacing_recommendations \
                     WHERE principal = ? AND workspace = ? \
                     AND candidate_id NOT IN ( \
                         SELECT candidate_id FROM resurfacing_candidates \
                         WHERE principal = ? AND workspace = ? \
                     )",
                    params![principal, workspace, principal, workspace],
                )
                .context("cleaning orphaned resurfacing recommendations")?;
            pruned += tx
                .execute(
                    "DELETE FROM resurfacing_routing_repairs \
                     WHERE principal = ? AND workspace = ? \
                     AND candidate_id NOT IN ( \
                         SELECT candidate_id FROM resurfacing_candidates \
                         WHERE principal = ? AND workspace = ? \
                     )",
                    params![principal, workspace, principal, workspace],
                )
                .context("cleaning orphaned resurfacing routing repair receipts")?;

            tx.commit()
                .context("committing resurfacing retention transaction")?;
            Ok(pruned)
        })
        .await
        .context("resurfacing retention_sweep task panicked")?
    }

    /// Snapshot the per-`(state, source_kind)` candidate counts for a scope (O2
    /// observability): one row per distinct `(state, source_kind)` pair with its
    /// `COUNT(*)`. Rows whose stored `state` or `source_kind` no longer parses
    /// are skipped (defensive — a legacy/unknown enum string can't abort the
    /// read). The observability endpoint folds these into per-state and per-lane
    /// totals plus the pending/surfaced headline numbers.
    pub async fn candidate_funnel(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<(CandidateState, SourceKind, u64)>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut stmt = conn.prepare(
                "SELECT state, source_kind, COUNT(*) FROM resurfacing_candidates \
                 WHERE principal = ? AND workspace = ? \
                 GROUP BY state, source_kind",
            )?;
            let mapped = stmt.query_map(params![principal, workspace], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?;
            let mut out = Vec::new();
            for triple in mapped {
                let (state_raw, kind_raw, count) = triple?;
                if let (Ok(state), Ok(kind)) = (
                    CandidateState::from_str(&state_raw),
                    SourceKind::from_str(&kind_raw),
                ) {
                    out.push((state, kind, count.max(0) as u64));
                }
            }
            Ok(out)
        })
        .await
        .context("resurfacing candidate_funnel task panicked")?
    }

    /// Snapshot the active curation queue for a scope. `pending` is the real queue:
    /// candidate-state rows whose cooldown has elapsed and which the curator can
    /// review now. The raw candidate-state pool is reported separately as
    /// `candidate_pool`, so observability can tell "discoverable inventory" from
    /// "work waiting for curation".
    pub async fn queue_stats(
        &self,
        principal: &str,
        workspace: &str,
        now: i64,
    ) -> Result<ResurfacingQueueStats> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            conn.query_row(
                "SELECT \
                    COALESCE(SUM(CASE WHEN state = ? THEN 1 ELSE 0 END), 0), \
                    COALESCE(SUM(CASE WHEN state = ? AND cooldown_until <= ? THEN 1 ELSE 0 END), 0), \
                    COALESCE(SUM(CASE WHEN state = ? AND cooldown_until > ? THEN 1 ELSE 0 END), 0), \
                    COALESCE(SUM(CASE WHEN state = ? THEN 1 ELSE 0 END), 0), \
                    COALESCE(SUM(CASE WHEN state = ? THEN 1 ELSE 0 END), 0), \
                    COALESCE(SUM(CASE WHEN state = ? THEN 1 ELSE 0 END), 0), \
                    COALESCE(SUM(CASE WHEN state = ? THEN 1 ELSE 0 END), 0) \
                 FROM resurfacing_candidates \
                 WHERE principal = ? AND workspace = ?",
                params![
                    CandidateState::Candidate.as_str(),
                    CandidateState::Candidate.as_str(),
                    now,
                    CandidateState::Candidate.as_str(),
                    now,
                    CandidateState::Surfaced.as_str(),
                    CandidateState::Acted.as_str(),
                    CandidateState::Dismissed.as_str(),
                    CandidateState::Snoozed.as_str(),
                    principal,
                    workspace,
                ],
                |row| {
                    let candidate_pool = row.get::<_, i64>(0)?.max(0) as u64;
                    let eligible = row.get::<_, i64>(1)?.max(0) as u64;
                    Ok(ResurfacingQueueStats {
                        candidate_pool,
                        pending: eligible,
                        eligible,
                        cooling: row.get::<_, i64>(2)?.max(0) as u64,
                        surfaced: row.get::<_, i64>(3)?.max(0) as u64,
                        acted: row.get::<_, i64>(4)?.max(0) as u64,
                        dismissed: row.get::<_, i64>(5)?.max(0) as u64,
                        snoozed: row.get::<_, i64>(6)?.max(0) as u64,
                    })
                },
            )
            .context("reading resurfacing queue stats")
        })
        .await
        .context("resurfacing queue_stats task panicked")?
    }

    /// Snapshot the corpus scan cursors for a scope (O2 observability): one
    /// `(corpus_kind, cursor)` per `resurfacing_watermarks` row, so the endpoint
    /// can show how far each lane's scan has advanced. Ordered by `corpus_kind`
    /// for a stable read.
    pub async fn watermark_positions(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<(String, i64)>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut stmt = conn.prepare(
                "SELECT corpus_kind, cursor FROM resurfacing_watermarks \
                 WHERE principal = ? AND workspace = ? \
                 ORDER BY corpus_kind",
            )?;
            let mapped = stmt.query_map(params![principal, workspace], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?;
            let mut out = Vec::new();
            for pair in mapped {
                out.push(pair?);
            }
            Ok(out)
        })
        .await
        .context("resurfacing watermark_positions task panicked")?
    }

    /// Row-count every resurfacing table for a scope (O2 observability), so the
    /// endpoint can show the store's size per data plane. Every count is scoped
    /// by `principal`/`workspace`; a scope with no rows reports all zeros.
    pub async fn table_sizes(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<ResurfacingTableSizes> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // One scoped COUNT(*) per table. The table name is a fixed literal
            // (never user input), so interpolating it is safe.
            let count = |table: &str| -> Result<u64> {
                let sql =
                    format!("SELECT COUNT(*) FROM {table} WHERE principal = ? AND workspace = ?");
                let n: i64 = conn
                    .query_row(&sql, params![principal, workspace], |row| row.get(0))
                    .with_context(|| format!("counting rows in {table}"))?;
                Ok(n.max(0) as u64)
            };
            Ok(ResurfacingTableSizes {
                candidates: count("resurfacing_candidates")?,
                embeddings: count("resurfacing_embeddings")?,
                embedding_cache: count("resurfacing_embedding_cache")?,
                phrasing: count("resurfacing_phrasing")?,
                recommendations: count("resurfacing_recommendations")?,
                dismissed_signals: count("resurfacing_dismissed_signals")?,
                affinity_signals: count("resurfacing_affinity_signals")?,
                runs: count("resurfacing_runs")?,
                action_claims: count("resurfacing_action_claims")?,
                action_events: count("resurfacing_action_events")?,
                routing_repairs: count("resurfacing_routing_repairs")?,
            })
        })
        .await
        .context("resurfacing table_sizes task panicked")?
    }

    /// The newest `limit` run records for a scope (O2 observability), newest
    /// first (`at DESC, id DESC`, matching the O1 test reader), each shaped into
    /// a serializable [`ResurfacingRun`] the endpoint returns directly.
    pub async fn recent_runs(
        &self,
        principal: &str,
        workspace: &str,
        limit: usize,
    ) -> Result<Vec<ResurfacingRun>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut stmt = conn.prepare(
                "SELECT kind, started_at, duration_ms, produced, success, error \
                 FROM resurfacing_runs \
                 WHERE principal = ? AND workspace = ? \
                 ORDER BY at DESC, id DESC LIMIT ?",
            )?;
            let mapped = stmt.query_map(params![principal, workspace, limit as i64], |row| {
                Ok(ResurfacingRun {
                    kind: row.get::<_, String>(0)?,
                    started_at: row.get::<_, i64>(1)?,
                    duration_ms: row.get::<_, i64>(2)?,
                    produced: row.get::<_, i64>(3)?,
                    success: row.get::<_, i64>(4)? != 0,
                    error: row.get::<_, Option<String>>(5)?,
                })
            })?;
            let mut out = Vec::new();
            for run in mapped {
                out.push(run?);
            }
            Ok(out)
        })
        .await
        .context("resurfacing recent_runs task panicked")?
    }

    /// Per-`kind` run aggregates for a scope (O2 observability): one
    /// [`ResurfacingRunAggregate`] per distinct run kind. `successes` is
    /// `SUM(success)`, `failures = total - successes`, `total_produced` sums the
    /// per-run `produced`, `avg_duration_ms` averages `duration_ms`,
    /// `last_started_at` is the newest `started_at`, and `last_error` is the
    /// error of that kind's most recent FAILING run (a correlated subquery;
    /// `None` when the kind has never failed). Ordered by `kind` for a stable
    /// read.
    pub async fn run_aggregates(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<ResurfacingRunAggregate>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // The correlated subquery (most-recent failing run's error) appears
            // in the SELECT list, so its two scope params bind BEFORE the outer
            // WHERE's two — order: subquery principal, subquery workspace, outer
            // principal, outer workspace.
            let mut stmt = conn.prepare(
                "SELECT r1.kind, \
                        COUNT(*) AS total, \
                        SUM(r1.success) AS successes, \
                        SUM(r1.produced) AS total_produced, \
                        AVG(r1.duration_ms) AS avg_duration_ms, \
                        MAX(r1.started_at) AS last_started_at, \
                        ( SELECT r2.error FROM resurfacing_runs r2 \
                          WHERE r2.principal = ? AND r2.workspace = ? \
                            AND r2.kind = r1.kind AND r2.success = 0 \
                          ORDER BY r2.at DESC, r2.id DESC LIMIT 1 ) AS last_error \
                 FROM resurfacing_runs r1 \
                 WHERE r1.principal = ? AND r1.workspace = ? \
                 GROUP BY r1.kind \
                 ORDER BY r1.kind",
            )?;
            let mapped =
                stmt.query_map(params![principal, workspace, principal, workspace], |row| {
                    let total: i64 = row.get(1)?;
                    let successes: i64 = row.get(2)?;
                    Ok(ResurfacingRunAggregate {
                        kind: row.get::<_, String>(0)?,
                        total: total.max(0) as u64,
                        successes: successes.max(0) as u64,
                        failures: (total - successes).max(0) as u64,
                        total_produced: row.get::<_, i64>(3)?,
                        avg_duration_ms: row.get::<_, f64>(4)?,
                        last_started_at: row.get::<_, i64>(5)?,
                        last_error: row.get::<_, Option<String>>(6)?,
                    })
                })?;
            let mut out = Vec::new();
            for agg in mapped {
                out.push(agg?);
            }
            Ok(out)
        })
        .await
        .context("resurfacing run_aggregates task panicked")?
    }

    /// Structural recommendation telemetry grouped by action kind and event
    /// class. Free-form labels/rationales are intentionally excluded.
    pub async fn recommendation_event_aggregates(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<ResurfacingRecommendationEventAggregate>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut stmt = conn.prepare(
                "SELECT recommendation_kind, event_type, COUNT(*) \
                 FROM resurfacing_action_events \
                 WHERE principal = ? AND workspace = ? \
                   AND recommendation_kind IS NOT NULL \
                   AND event_type IN ('recommended', 'selected', 'completed') \
                 GROUP BY recommendation_kind, event_type \
                 ORDER BY recommendation_kind, event_type",
            )?;
            let mapped = stmt.query_map(params![principal, workspace], |row| {
                let count = row.get::<_, i64>(2)?;
                Ok(ResurfacingRecommendationEventAggregate {
                    recommendation_kind: row.get(0)?,
                    event_type: row.get(1)?,
                    count: count.max(0) as u64,
                })
            })?;
            let mut out = Vec::new();
            for row in mapped {
                out.push(row?);
            }
            Ok(out)
        })
        .await
        .context("resurfacing recommendation_event_aggregates task panicked")?
    }

    /// Structural action telemetry grouped without carrying user-entered
    /// payloads, source bodies, recipients, or generated text.
    pub async fn action_event_aggregates(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<ResurfacingActionEventAggregate>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut stmt = conn.prepare(
                "SELECT action_kind, event_type, error_class, COUNT(*) \
                 FROM resurfacing_action_events \
                 WHERE principal = ? AND workspace = ? \
                   AND (event_type IN ('started', 'failed', 'stale_revision_rejected') \
                        OR (event_type = 'completed' AND result_ref IS NOT NULL)) \
                 GROUP BY action_kind, event_type, error_class \
                 ORDER BY action_kind, event_type, error_class",
            )?;
            let mapped = stmt.query_map(params![principal, workspace], |row| {
                let count = row.get::<_, i64>(3)?;
                Ok(ResurfacingActionEventAggregate {
                    action_kind: row.get(0)?,
                    event_type: row.get(1)?,
                    error_class: row.get(2)?,
                    count: count.max(0) as u64,
                })
            })?;
            mapped
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("reading resurfacing action event aggregates")
        })
        .await
        .context("resurfacing action_event_aggregates task panicked")?
    }

    /// Current durable claim states. Unlike event totals, this counts one row
    /// per idempotent contextual action and therefore cannot double-count the
    /// recommendation interaction events that share the telemetry table.
    pub async fn action_claim_state_histogram(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<(String, u64)>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut stmt = conn.prepare(
                "SELECT state, COUNT(*) FROM resurfacing_action_claims \
                 WHERE principal = ? AND workspace = ? GROUP BY state ORDER BY state",
            )?;
            let mapped = stmt.query_map(params![principal, workspace], |row| {
                Ok((row.get(0)?, row.get::<_, i64>(1)?.max(0) as u64))
            })?;
            mapped
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("reading resurfacing action claim state histogram")
        })
        .await
        .context("resurfacing action_claim_state_histogram task panicked")?
    }

    pub async fn routing_repair_outcome_histogram(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<(String, u64)>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut stmt = conn.prepare(
                "SELECT outcome, COUNT(*) FROM resurfacing_routing_repairs \
                 WHERE principal = ? AND workspace = ? GROUP BY outcome ORDER BY outcome",
            )?;
            let mapped = stmt.query_map(params![principal, workspace], |row| {
                Ok((row.get(0)?, row.get::<_, i64>(1)?.max(0) as u64))
            })?;
            mapped
                .collect::<rusqlite::Result<Vec<_>>>()
                .context("reading resurfacing routing repair outcome histogram")
        })
        .await
        .context("resurfacing routing_repair_outcome_histogram task panicked")?
    }

    /// Coverage of the provider-neutral safe brief across communication
    /// candidates, including the active surfaced subset. JSON validity is
    /// checked before extraction so one legacy/corrupt row cannot darken the
    /// observability endpoint.
    pub async fn brief_coverage(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<ResurfacingBriefCoverage> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tuple: (i64, i64, i64, i64, i64, i64, i64) = conn.query_row(
                "SELECT COUNT(*), \
                    COALESCE(SUM(CASE WHEN state = 'surfaced' THEN 1 ELSE 0 END), 0), \
                    COALESCE(SUM(CASE WHEN content_details_json IS NOT NULL \
                        AND json_valid(content_details_json) THEN 1 ELSE 0 END), 0), \
                    COALESCE(SUM(CASE WHEN content_details_json IS NULL \
                        OR NOT json_valid(content_details_json) THEN 1 ELSE 0 END), 0), \
                    COALESCE(SUM(CASE WHEN json_valid(content_details_json) \
                        AND json_extract(content_details_json, '$.detail_status') = 'complete' THEN 1 ELSE 0 END), 0), \
                    COALESCE(SUM(CASE WHEN json_valid(content_details_json) \
                        AND json_extract(content_details_json, '$.detail_status') = 'partial' THEN 1 ELSE 0 END), 0), \
                    COALESCE(SUM(CASE WHEN json_valid(content_details_json) \
                        AND json_extract(content_details_json, '$.detail_status') = 'source_omits_details' THEN 1 ELSE 0 END), 0) \
                 FROM resurfacing_candidates \
                 WHERE principal = ? AND workspace = ? AND source_kind = 'comm'",
                params![principal, workspace],
                |row| {
                    Ok((
                        row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
                        row.get(4)?, row.get(5)?, row.get(6)?,
                    ))
                },
            )?;
            Ok(ResurfacingBriefCoverage {
                comm_total: tuple.0.max(0) as u64,
                comm_surfaced: tuple.1.max(0) as u64,
                with_brief: tuple.2.max(0) as u64,
                legacy: tuple.3.max(0) as u64,
                complete: tuple.4.max(0) as u64,
                partial: tuple.5.max(0) as u64,
                source_omits_details: tuple.6.max(0) as u64,
            })
        })
        .await
        .context("resurfacing brief_coverage task panicked")?
    }
}

impl ResurfacingStore {
    /// Open a throwaway store backed by a temp directory (tests only). The
    /// temp dir is intentionally persisted (`keep`) so the SQLite file and
    /// its WAL sidecars outlive this call for the rest of the test process,
    /// while the caller still gets back a bare `Self`.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn open_in_temp() -> Self {
        let dir = tempfile::TempDir::new().expect("creating resurfacing temp dir");
        let path = dir.keep();
        Self::open(&path).expect("opening resurfacing store in temp dir")
    }

    /// Test-only raw dump of a scope's run rows, newest first
    /// (`(kind, started_at, duration_ms, produced, success, error)`). O1 only
    /// persists runs; the real read/aggregate API lands in O2 — this minimal
    /// reader just backs the O1 persistence tests.
    pub async fn list_runs_for_test(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Vec<(String, i64, i64, i64, bool, Option<String>)> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut stmt = conn
                .prepare(
                    "SELECT kind, started_at, duration_ms, produced, success, error \
                     FROM resurfacing_runs \
                     WHERE principal = ? AND workspace = ? \
                     ORDER BY at DESC, id DESC",
                )
                .expect("preparing test run reader");
            let mapped = stmt
                .query_map(params![principal, workspace], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)? != 0,
                        row.get::<_, Option<String>>(5)?,
                    ))
                })
                .expect("querying test run reader");
            mapped.map(|r| r.expect("decoding test run row")).collect()
        })
        .await
        .expect("resurfacing list_runs_for_test task panicked")
    }

    /// Test-only count of feedback-log rows for a scope. `resurfacing_feedback`
    /// has no public reader; the feedback-bounds tests use this to assert the
    /// existence gate (no row for a foreign id) and the retention age-prune.
    pub async fn count_feedback_for_test(&self, principal: &str, workspace: &str) -> i64 {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            conn.query_row(
                "SELECT COUNT(*) FROM resurfacing_feedback \
                 WHERE principal = ? AND workspace = ?",
                params![principal, workspace],
                |row| row.get::<_, i64>(0),
            )
            .expect("counting test feedback rows")
        })
        .await
        .expect("resurfacing count_feedback_for_test task panicked")
    }
}

/// Add one nullable compatibility column to an existing SQLite table. Fresh
/// databases already get the columns from `BOOTSTRAP_DDL`; this path keeps
/// deployed databases additive without relying on SQLite versions that accept
/// `ADD COLUMN IF NOT EXISTS`.
/// Lane discriminators for `resurfacing_action_claims.target_kind`.
///
/// The claim row keys idempotency for every lane, so the id alone is ambiguous
/// once more than one lane can raise an action; this says which store the id
/// belongs to.
pub const TARGET_KIND_RESURFACING_CANDIDATE: &str = "resurfacing_candidate";
pub const TARGET_KIND_CHANNEL_FOLLOW_UP: &str = "channel_follow_up";

fn ensure_column(conn: &Connection, table: &str, column: &str, sql_type: &str) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(1)? == column {
            return Ok(());
        }
    }
    conn.execute_batch(&format!(
        "ALTER TABLE {table} ADD COLUMN {column} {sql_type}"
    ))
    .with_context(|| format!("adding {table}.{column}"))
}

fn query_contextual_action_claim(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    candidate_id: &str,
    idempotency_key: &str,
) -> Result<Option<PersistedResurfacingActionClaim>> {
    let mut stmt = conn.prepare(
        "SELECT action_kind, input_hash, content_revision, state, result_ref, result_json, updated_at \
         FROM resurfacing_action_claims \
         WHERE principal = ? AND workspace = ? AND candidate_id = ? \
           AND idempotency_key = ?",
    )?;
    let mut rows = stmt.query(params![principal, workspace, candidate_id, idempotency_key])?;
    match rows.next()? {
        Some(row) => Ok(Some(PersistedResurfacingActionClaim {
            action_kind: row.get(0)?,
            input_hash: row.get(1)?,
            content_revision: row.get(2)?,
            state: row.get(3)?,
            result_ref: row.get(4)?,
            result_json: row.get(5)?,
            updated_at: row.get(6)?,
        })),
        None => Ok(None),
    }
}

fn contextual_action_lookup(
    claim: &PersistedResurfacingActionClaim,
    action_kind: &str,
    input_hash: &str,
    now: i64,
    stale_after_secs: i64,
) -> Result<ResurfacingActionClaimLookup> {
    if claim.action_kind != action_kind || claim.input_hash != input_hash {
        return Ok(ResurfacingActionClaimLookup::Conflict);
    }
    match claim.state.as_str() {
        "completed" => Ok(ResurfacingActionClaimLookup::Completed(
            stored_contextual_action_result(claim)?,
        )),
        "started" if now.saturating_sub(claim.updated_at) >= stale_after_secs.max(1) => {
            Ok(ResurfacingActionClaimLookup::Retryable)
        },
        "started" => Ok(ResurfacingActionClaimLookup::InProgress),
        "failed" => Ok(ResurfacingActionClaimLookup::Retryable),
        other => anyhow::bail!("invalid resurfacing action claim state: {other}"),
    }
}

fn stored_contextual_action_result(
    claim: &PersistedResurfacingActionClaim,
) -> Result<ResurfacingStoredActionResult> {
    let result_json = claim
        .result_json
        .as_deref()
        .context("completed resurfacing action claim has no result")?;
    Ok(ResurfacingStoredActionResult {
        result_ref: claim.result_ref.clone(),
        result: serde_json::from_str(result_json)
            .context("decoding stored resurfacing contextual-action result")?,
    })
}

fn read_candidate_action_state(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    candidate_id: &str,
) -> Result<Option<(CandidateState, Option<String>)>> {
    let mut stmt = conn.prepare(
        "SELECT state, content_revision FROM resurfacing_candidates \
         WHERE principal = ? AND workspace = ? AND candidate_id = ?",
    )?;
    let mut rows = stmt.query(params![principal, workspace, candidate_id])?;
    match rows.next()? {
        Some(row) => {
            let state: String = row.get(0)?;
            let state = CandidateState::from_str(&state)
                .map_err(|error| anyhow::anyhow!("invalid resurfacing candidate state: {error}"))?;
            Ok(Some((state, row.get(1)?)))
        },
        None => Ok(None),
    }
}

#[allow(clippy::too_many_arguments)]
fn insert_contextual_action_event(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    candidate_id: &str,
    action_kind: &str,
    content_revision: Option<&str>,
    event_type: &str,
    result_ref: Option<&str>,
    error_class: Option<&str>,
    at: i64,
) -> Result<()> {
    let recommendation_kind = matching_recommendation_kind(
        conn,
        principal,
        workspace,
        candidate_id,
        action_kind,
        content_revision,
    )?;
    if recommendation_kind.is_some()
        && matches!(event_type, "recommended" | "selected" | "completed")
        && recommendation_event_exists(
            conn,
            principal,
            workspace,
            candidate_id,
            action_kind,
            content_revision,
            event_type,
        )?
    {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO resurfacing_action_events ( \
            principal, workspace, candidate_id, action_kind, recommendation_kind, \
            content_revision, event_type, result_ref, error_class, at \
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            principal,
            workspace,
            candidate_id,
            bounded_action_field(action_kind, 80),
            recommendation_kind,
            content_revision.map(|value| bounded_action_field(value, 160)),
            bounded_action_field(event_type, 80),
            result_ref.map(|value| bounded_action_field(value, 240)),
            error_class.map(|value| bounded_action_field(value, 80)),
            at,
        ],
    )
    .context("inserting resurfacing contextual-action event")?;
    Ok(())
}

fn matching_recommendation_kind(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    candidate_id: &str,
    action_kind: &str,
    content_revision: Option<&str>,
) -> Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT r.action_kind FROM resurfacing_recommendations r \
         JOIN resurfacing_candidates c \
           ON c.principal = r.principal AND c.workspace = r.workspace \
          AND c.candidate_id = r.candidate_id \
         WHERE r.principal = ? AND r.workspace = ? AND r.candidate_id = ? \
           AND r.action_kind = ? AND r.content_revision IS ? \
           AND r.shown_at IS NOT NULL \
           AND c.content_revision IS r.content_revision LIMIT 1",
    )?;
    let mut rows = stmt.query(params![
        principal,
        workspace,
        candidate_id,
        action_kind,
        content_revision,
    ])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

fn recommendation_event_exists(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    candidate_id: &str,
    action_kind: &str,
    content_revision: Option<&str>,
    event_type: &str,
) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM resurfacing_action_events \
         WHERE principal = ? AND workspace = ? AND candidate_id = ? \
           AND action_kind = ? AND recommendation_kind = ? \
           AND content_revision IS ? AND event_type = ?)",
        params![
            principal,
            workspace,
            candidate_id,
            action_kind,
            action_kind,
            content_revision,
            event_type,
        ],
        |row| row.get::<_, i64>(0),
    )? != 0)
}

fn record_contextual_action_positive(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    candidate_id: &str,
    content_revision: Option<&str>,
    now: i64,
    acted_cooldown_secs: i64,
) -> Result<()> {
    let source_kind = read_candidate_source_kind(conn, principal, workspace, candidate_id)?;
    let changed = conn
        .execute(
            "UPDATE resurfacing_candidates SET state = ?, cooldown_until = ? \
             WHERE principal = ? AND workspace = ? AND candidate_id = ? AND state = ? \
               AND content_revision IS ?",
            params![
                CandidateState::Acted.as_str(),
                now.saturating_add(acted_cooldown_secs.max(0)),
                principal,
                workspace,
                candidate_id,
                CandidateState::Surfaced.as_str(),
                content_revision,
            ],
        )
        .context("marking resurfacing contextual action acted")?;
    if changed == 0 {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO resurfacing_feedback \
         (principal, workspace, candidate_id, action, at) VALUES (?, ?, ?, ?, ?)",
        params![
            principal,
            workspace,
            candidate_id,
            FeedbackAction::Acknowledge.as_str(),
            now,
        ],
    )
    .context("recording positive resurfacing contextual-action feedback")?;
    if let Some((acted_vec, Some(contract))) =
        read_current_candidate_embedding_with_contract(conn, principal, workspace, candidate_id)?
    {
        write_affinity_signal(
            conn,
            principal,
            workspace,
            None,
            Some(&contract),
            &acted_vec,
            now,
        )
        .context("recording contextual-action affinity signal")?;
    }
    if let Some(source_kind) = source_kind {
        bump_kind_engagement(conn, principal, workspace, &source_kind, 1, 0)
            .context("bumping contextual-action kind engagement")?;
    }
    Ok(())
}

fn bounded_action_field(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars.max(1)).collect()
}

/// Decode one `resurfacing_candidates` row (in [`CANDIDATE_COLUMNS`] order)
/// into a [`Candidate`], parsing the enum strings and JSON signal bundle.
fn map_candidate_row(row: &rusqlite::Row<'_>) -> Result<Candidate> {
    let source_kind_raw: String = row.get(1)?;
    let signals_raw: String = row.get(6)?;
    let state_raw: String = row.get(9)?;
    Ok(Candidate {
        candidate_id: row.get(0)?,
        source_kind: SourceKind::from_str(&source_kind_raw)
            .map_err(|err| anyhow::anyhow!("invalid resurfacing source_kind: {err}"))?,
        source_ref: row.get(2)?,
        title: row.get(3)?,
        content_digest: row.get(4)?,
        content_details: row
            .get::<_, Option<String>>(16)?
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .context("deserializing resurfacing candidate content details")?,
        content_revision: row.get(17)?,
        semantic_features: row
            .get::<_, Option<String>>(18)?
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .context("deserializing resurfacing semantic features")?,
        salience_score: row.get(5)?,
        signals: serde_json::from_str(&signals_raw)
            .context("deserializing resurfacing candidate signals")?,
        temporal_anchor_at: row.get(7)?,
        embedding_id: row.get(8)?,
        state: CandidateState::from_str(&state_raw)
            .map_err(|err| anyhow::anyhow!("invalid resurfacing candidate state: {err}"))?,
        first_seen_at: row.get(10)?,
        last_scored_at: row.get(11)?,
        last_surfaced_at: row.get(12)?,
        cooldown_until: row.get(13)?,
        surface_count: row.get(14)?,
        dismiss_count: row.get(15)?,
    })
}

fn candidate_surfaced_cursor(candidate: &Candidate) -> SurfacedCursor {
    SurfacedCursor {
        surfaced_at: candidate.last_surfaced_at.unwrap_or(0),
        salience_score: f64::from(candidate.salience_score),
        candidate_id: candidate.candidate_id.clone(),
    }
}

fn row_surfaced_cursor(row: &rusqlite::Row<'_>) -> Option<SurfacedCursor> {
    Some(SurfacedCursor {
        candidate_id: row.get(0).ok()?,
        salience_score: row.get::<_, f64>(5).ok()?,
        surfaced_at: row.get::<_, Option<i64>>(12).ok().flatten().unwrap_or(0),
    })
}

/// Read one candidate's stored embedding vector on a held connection (shared by
/// the async [`ResurfacingStore::get_embedding`] and the synchronous dismiss
/// suppression). `None` when no vector was persisted for that id in the scope.
fn read_embedding(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    candidate_id: &str,
) -> Result<Option<Vec<f32>>> {
    Ok(
        read_embedding_with_contract(conn, principal, workspace, candidate_id)?
            .map(|(embedding, _)| embedding),
    )
}

fn valid_stored_vector_without_dims(vector: &[f32]) -> bool {
    if vector.is_empty() || vector.iter().any(|value| !value.is_finite()) {
        return false;
    }
    let norm_squared = vector
        .iter()
        .map(|value| f64::from(*value) * f64::from(*value))
        .sum::<f64>();
    norm_squared.is_finite() && norm_squared > f64::EPSILON
}

fn read_embedding_with_contract(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    candidate_id: &str,
) -> Result<Option<(Vec<f32>, Option<String>)>> {
    let mut stmt = conn.prepare(
        "SELECT vec, embedding_contract FROM resurfacing_embeddings \
         WHERE candidate_id = ? AND principal = ? AND workspace = ?",
    )?;
    let mut rows = stmt.query(params![candidate_id, principal, workspace])?;
    match rows.next()? {
        Some(row) => {
            let raw: String = row.get(0)?;
            let embedding_contract: Option<String> = row.get(1)?;
            let vec: Vec<f32> =
                serde_json::from_str(&raw).context("deserializing resurfacing embedding vector")?;
            Ok(Some((vec, embedding_contract)))
        },
        None => Ok(None),
    }
}

/// Action-path read that fails closed unless the vector was computed from the
/// candidate's currently committed content. This closes the small transaction
/// gap between scorer candidate upsert and embedding promotion.
fn read_current_candidate_embedding_with_contract(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    candidate_id: &str,
) -> Result<Option<(Vec<f32>, Option<String>)>> {
    let mut stmt = conn.prepare(
        "SELECT e.vec, e.embedding_contract FROM resurfacing_embeddings e \
         INNER JOIN resurfacing_candidates c \
           ON c.principal = e.principal AND c.workspace = e.workspace \
          AND c.candidate_id = e.candidate_id \
          AND c.content_digest = e.content_digest \
         WHERE e.candidate_id = ? AND e.principal = ? AND e.workspace = ?",
    )?;
    let mut rows = stmt.query(params![candidate_id, principal, workspace])?;
    match rows.next()? {
        Some(row) => {
            let raw: String = row.get(0)?;
            let embedding_contract: Option<String> = row.get(1)?;
            match serde_json::from_str::<Vec<f32>>(&raw) {
                Ok(embedding) if valid_stored_vector_without_dims(&embedding) => {
                    Ok(Some((embedding, embedding_contract)))
                },
                _ => {
                    tracing::warn!(
                        candidate_id,
                        "removing malformed current resurfacing embedding"
                    );
                    drop(rows);
                    drop(stmt);
                    conn.execute(
                        "DELETE FROM resurfacing_embeddings WHERE candidate_id = ? AND principal = ? AND workspace = ? AND vec = ?",
                        params![candidate_id, principal, workspace, raw],
                    )?;
                    Ok(None)
                },
            }
        },
        None => Ok(None),
    }
}

fn upsert_embedding_row(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    candidate_id: &str,
    content_digest: Option<&str>,
    embedding_contract: Option<&str>,
    vec: &[f32],
    updated_at: i64,
) -> Result<()> {
    let vec_json =
        serde_json::to_string(vec).context("serializing resurfacing embedding vector")?;
    conn.execute(
        "INSERT INTO resurfacing_embeddings ( \
            candidate_id, principal, workspace, content_digest, embedding_contract, vec, updated_at \
         ) VALUES (?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(principal, workspace, candidate_id) DO UPDATE SET \
            content_digest = excluded.content_digest, \
            embedding_contract = excluded.embedding_contract, \
            vec = excluded.vec, updated_at = excluded.updated_at",
        params![
            candidate_id,
            principal,
            workspace,
            content_digest,
            embedding_contract,
            vec_json,
            updated_at,
        ],
    )
    .context("upserting resurfacing embedding")?;
    Ok(())
}

fn unix_now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64
}

fn upsert_reference_cache_row(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    embedding: &CandidateEmbeddingSnapshot,
) -> Result<()> {
    let vec_json = serde_json::to_string(&embedding.embedding)
        .context("serializing resurfacing reference-cache vector")?;
    let updated_at = unix_now_seconds();
    conn.execute(
        "INSERT INTO resurfacing_embedding_cache (
            candidate_id, principal, workspace, content_digest, embedding_contract, vec, updated_at
         ) VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(principal, workspace, candidate_id, content_digest, embedding_contract)
         DO UPDATE SET vec = excluded.vec, updated_at = excluded.updated_at",
        params![
            embedding.candidate_id,
            principal,
            workspace,
            embedding.content_digest,
            embedding.embedding_contract,
            vec_json,
            updated_at,
        ],
    )
    .context("upserting resurfacing versioned reference cache")?;
    Ok(())
}

fn prune_versioned_reference_cache(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    cap: usize,
) -> Result<()> {
    conn.execute(
        "DELETE FROM resurfacing_embedding_cache
         WHERE principal = ? AND workspace = ? AND rowid NOT IN (
             SELECT rowid FROM resurfacing_embedding_cache
             WHERE principal = ? AND workspace = ?
             ORDER BY updated_at DESC, candidate_id DESC, content_digest DESC
             LIMIT ?
         )",
        params![principal, workspace, principal, workspace, cap as i64],
    )
    .context("pruning resurfacing versioned reference cache")?;
    Ok(())
}

/// Retain only the newest bounded set of successful pre-candidate vectors.
/// Candidate-attached embeddings are never pruned here; the normal resurfacing
/// retention sweep owns their lifecycle.
fn prune_unattached_embedding_cache(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    cap: usize,
) -> Result<()> {
    conn.execute(
        "DELETE FROM resurfacing_embeddings AS doomed \
         WHERE doomed.principal = ? AND doomed.workspace = ? \
           AND doomed.content_digest IS NOT NULL \
           AND doomed.embedding_contract IS NOT NULL \
           AND NOT EXISTS ( \
             SELECT 1 FROM resurfacing_candidates c \
             WHERE c.principal = doomed.principal \
               AND c.workspace = doomed.workspace \
               AND c.candidate_id = doomed.candidate_id \
           ) \
           AND doomed.candidate_id NOT IN ( \
             SELECT kept.candidate_id FROM resurfacing_embeddings kept \
             WHERE kept.principal = ? AND kept.workspace = ? \
               AND kept.content_digest IS NOT NULL \
               AND kept.embedding_contract IS NOT NULL \
               AND NOT EXISTS ( \
                 SELECT 1 FROM resurfacing_candidates c2 \
                 WHERE c2.principal = kept.principal \
                   AND c2.workspace = kept.workspace \
                   AND c2.candidate_id = kept.candidate_id \
               ) \
             ORDER BY kept.updated_at DESC, kept.candidate_id DESC \
             LIMIT ? \
           )",
        params![principal, workspace, principal, workspace, cap as i64],
    )
    .context("pruning unattached resurfacing embedding cache")?;
    Ok(())
}

/// Read the acted candidate's `source_kind` string on a held connection, or
/// `None` when the row is absent (so the P4c utility counter is skipped
/// gracefully). Used by [`ResurfacingStore::record_action`] to attribute an
/// action to its lane.
fn read_candidate_source_kind(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    candidate_id: &str,
) -> Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT source_kind FROM resurfacing_candidates \
         WHERE candidate_id = ? AND principal = ? AND workspace = ?",
    )?;
    let mut rows = stmt.query(params![candidate_id, principal, workspace])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get::<_, String>(0)?)),
        None => Ok(None),
    }
}

/// UPSERT-increment a scope's per-lane engagement counter (P4c) on a held
/// connection. `positive`/`negative` are the deltas to add for this action
/// (`Open`/`Acknowledge` → `(1, 0)`, `Dismiss` → `(0, 1)`). Runs on the caller's
/// transaction so the counter commits with the rest of `record_action`.
fn bump_kind_engagement(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    source_kind: &str,
    positive: i64,
    negative: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO resurfacing_kind_engagement \
         (principal, workspace, source_kind, positive, negative) \
         VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(principal, workspace, source_kind) DO UPDATE SET \
             positive = positive + excluded.positive, \
             negative = negative + excluded.negative",
        params![principal, workspace, source_kind, positive, negative],
    )
    .context("upserting resurfacing kind engagement")?;
    Ok(())
}

/// Read every per-lane engagement tally in a scope on a held connection (P4c),
/// shared by the async [`ResurfacingStore::kind_engagement`]. Rows whose stored
/// `source_kind` no longer parses are skipped so an unknown lane string can't
/// abort the read; negative-looking counts are floored at `0` before the `u64`
/// cast (defensive — the SQL only ever increments).
fn read_kind_engagement(
    conn: &Connection,
    principal: &str,
    workspace: &str,
) -> Result<Vec<(SourceKind, u64, u64)>> {
    let mut stmt = conn.prepare(
        "SELECT source_kind, positive, negative FROM resurfacing_kind_engagement \
         WHERE principal = ? AND workspace = ?",
    )?;
    let mapped = stmt.query_map(params![principal, workspace], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for triple in mapped {
        let (kind_raw, positive, negative) = triple?;
        if let Ok(kind) = SourceKind::from_str(&kind_raw) {
            out.push((kind, positive.max(0) as u64, negative.max(0) as u64));
        }
    }
    Ok(out)
}

/// Read every `(candidate_id, vector)` in a scope on a held connection (shared by
/// the async [`ResurfacingStore::list_embeddings`] and the synchronous dismiss
/// suppression's scope-wide sweep).
fn read_all_embeddings(
    conn: &Connection,
    principal: &str,
    workspace: &str,
) -> Result<Vec<(String, Vec<f32>)>> {
    let mut stmt = conn.prepare(
        "SELECT candidate_id, vec FROM resurfacing_embeddings \
         WHERE principal = ? AND workspace = ?",
    )?;
    let mapped = stmt.query_map(params![principal, workspace], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut out = Vec::new();
    for pair in mapped {
        let (candidate_id, raw) = pair?;
        let vec: Vec<f32> =
            serde_json::from_str(&raw).context("deserializing resurfacing embedding vector")?;
        out.push((candidate_id, vec));
    }
    Ok(out)
}

fn read_all_embeddings_for_contract(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    embedding_contract: &str,
) -> Result<Vec<(String, Vec<f32>)>> {
    let mut stmt = conn.prepare(
        "SELECT e.candidate_id, e.vec FROM resurfacing_embeddings e \
         INNER JOIN resurfacing_candidates c \
           ON c.principal = e.principal AND c.workspace = e.workspace \
          AND c.candidate_id = e.candidate_id \
          AND c.content_digest = e.content_digest \
         WHERE e.principal = ? AND e.workspace = ? AND e.embedding_contract = ?",
    )?;
    let mapped = stmt.query_map(params![principal, workspace, embedding_contract], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let raw_rows = mapped.collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    let mut out = Vec::new();
    for (candidate_id, raw) in raw_rows {
        match serde_json::from_str::<Vec<f32>>(&raw) {
            Ok(vector) if valid_stored_vector_without_dims(&vector) => {
                out.push((candidate_id, vector));
            },
            _ => {
                tracing::warn!(
                    candidate_id,
                    "removing malformed contract-scoped resurfacing embedding"
                );
                conn.execute(
                    "DELETE FROM resurfacing_embeddings WHERE candidate_id = ? AND principal = ? AND workspace = ? AND embedding_contract = ? AND vec = ?",
                    params![candidate_id, principal, workspace, embedding_contract, raw],
                )?;
            },
        }
    }
    Ok(out)
}

/// Persist one durable dismissed-signal row and enforce the per-scope cap, on a
/// held connection (shared by the async [`ResurfacingStore::record_dismissed_signal`]
/// and the synchronous dismiss path so both write + cap identically). The vector
/// is stored as a self-contained JSON snapshot, INDEPENDENT of the candidate/
/// embedding tables, so it survives the dismissed candidate being pruned or
/// re-embedded by later churn. After the insert we keep only the newest
/// [`DISMISSED_SIGNAL_CAP`] rows in the scope (ordered by `at`, ties broken by
/// the monotonic `id` so a burst of same-second dismissals still evicts oldest-
/// first deterministically).
fn write_dismissed_signal(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    event_id: Option<&str>,
    embedding_contract: Option<&str>,
    vec: &[f32],
    now: i64,
) -> Result<()> {
    let vec_json =
        serde_json::to_string(vec).context("serializing resurfacing dismissed signal vector")?;
    conn.execute(
        "INSERT INTO resurfacing_dismissed_signals (principal, workspace, event_id, embedding_contract, vec, at) \
         VALUES (?, ?, ?, ?, ?, ?)",
        params![principal, workspace, event_id, embedding_contract, vec_json, now],
    )
    .context("inserting resurfacing dismissed signal")?;
    // Bound the append-only history to the newest DISMISSED_SIGNAL_CAP rows in
    // this scope. The subquery picks the survivors; everything else in the scope
    // is deleted.
    conn.execute(
        "DELETE FROM resurfacing_dismissed_signals \
         WHERE principal = ? AND workspace = ? \
         AND id NOT IN ( \
             SELECT id FROM resurfacing_dismissed_signals \
             WHERE principal = ? AND workspace = ? \
             ORDER BY at DESC, id DESC LIMIT ? \
         )",
        params![
            principal,
            workspace,
            principal,
            workspace,
            DISMISSED_SIGNAL_CAP as i64,
        ],
    )
    .context("capping resurfacing dismissed signal history")?;
    Ok(())
}

/// Read every durable dismissed-signal vector in a scope on a held connection,
/// newest first (shared by the async [`ResurfacingStore::list_dismissed_signals`]
/// and any synchronous penalty path). Bounded by [`DISMISSED_SIGNAL_CAP`].
fn read_dismissed_signals(
    conn: &Connection,
    principal: &str,
    workspace: &str,
) -> Result<Vec<Vec<f32>>> {
    read_dismissed_signals_for_contract(conn, principal, workspace, None)
}

fn read_dismissed_signals_for_contract(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    embedding_contract: Option<&str>,
) -> Result<Vec<Vec<f32>>> {
    let contract_clause = if embedding_contract.is_some() {
        " AND embedding_contract = ?"
    } else {
        ""
    };
    let sql = format!(
        "SELECT id, vec FROM resurfacing_dismissed_signals \
         WHERE principal = ? AND workspace = ?{contract_clause} \
         ORDER BY at DESC, id DESC"
    );
    let mut values = vec![principal.to_string(), workspace.to_string()];
    if let Some(contract) = embedding_contract {
        values.push(contract.to_string());
    }
    let mut stmt = conn.prepare(&sql)?;
    let mapped = stmt.query_map(params_from_iter(values), |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let raw_rows = mapped.collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    let mut out = Vec::new();
    let mut skipped = 0usize;
    for (id, raw) in raw_rows {
        match serde_json::from_str::<Vec<f32>>(&raw) {
            Ok(vec) if embedding_contract.is_none() || valid_stored_vector_without_dims(&vec) => {
                out.push(vec)
            },
            _ => {
                skipped = skipped.saturating_add(1);
                if skipped <= 3 {
                    tracing::warn!(id, "removing malformed resurfacing dismissed signal vector");
                }
                conn.execute(
                    "DELETE FROM resurfacing_dismissed_signals WHERE id = ? AND vec = ?",
                    params![id, raw],
                )?;
            },
        }
    }
    Ok(out)
}

/// Persist one durable affinity-signal row and enforce the per-scope cap, on a
/// held connection (shared by the async [`ResurfacingStore::record_affinity_signal`]
/// and the synchronous open/ack path so both write + cap identically). The
/// positive mirror of [`write_dismissed_signal`]: the vector is stored as a
/// self-contained JSON snapshot, INDEPENDENT of the candidate/embedding tables,
/// so it survives the acted candidate being pruned or re-embedded by later
/// churn. After the insert we keep only the newest [`AFFINITY_SIGNAL_CAP`] rows
/// in the scope (ordered by `at`, ties broken by the monotonic `id` so a burst
/// of same-second actions still evicts oldest-first deterministically).
fn write_affinity_signal(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    event_id: Option<&str>,
    embedding_contract: Option<&str>,
    vec: &[f32],
    now: i64,
) -> Result<()> {
    let vec_json =
        serde_json::to_string(vec).context("serializing resurfacing affinity signal vector")?;
    conn.execute(
        "INSERT INTO resurfacing_affinity_signals (principal, workspace, event_id, embedding_contract, vec, at) \
         VALUES (?, ?, ?, ?, ?, ?)",
        params![principal, workspace, event_id, embedding_contract, vec_json, now],
    )
    .context("inserting resurfacing affinity signal")?;
    // Bound the append-only history to the newest AFFINITY_SIGNAL_CAP rows in
    // this scope. The subquery picks the survivors; everything else in the scope
    // is deleted.
    conn.execute(
        "DELETE FROM resurfacing_affinity_signals \
         WHERE principal = ? AND workspace = ? \
         AND id NOT IN ( \
             SELECT id FROM resurfacing_affinity_signals \
             WHERE principal = ? AND workspace = ? \
             ORDER BY at DESC, id DESC LIMIT ? \
         )",
        params![
            principal,
            workspace,
            principal,
            workspace,
            AFFINITY_SIGNAL_CAP as i64,
        ],
    )
    .context("capping resurfacing affinity signal history")?;
    Ok(())
}

/// Read every durable affinity-signal vector in a scope on a held connection,
/// newest first (shared by the async [`ResurfacingStore::list_affinity_signals`]
/// and any synchronous boost path). Bounded by [`AFFINITY_SIGNAL_CAP`]. The
/// positive mirror of [`read_dismissed_signals`].
fn read_affinity_signals(
    conn: &Connection,
    principal: &str,
    workspace: &str,
) -> Result<Vec<Vec<f32>>> {
    read_affinity_signals_for_contract(conn, principal, workspace, None)
}

fn read_affinity_signals_for_contract(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    embedding_contract: Option<&str>,
) -> Result<Vec<Vec<f32>>> {
    let contract_clause = if embedding_contract.is_some() {
        " AND embedding_contract = ?"
    } else {
        ""
    };
    let sql = format!(
        "SELECT id, vec FROM resurfacing_affinity_signals \
         WHERE principal = ? AND workspace = ?{contract_clause} \
         ORDER BY at DESC, id DESC"
    );
    let mut values = vec![principal.to_string(), workspace.to_string()];
    if let Some(contract) = embedding_contract {
        values.push(contract.to_string());
    }
    let mut stmt = conn.prepare(&sql)?;
    let mapped = stmt.query_map(params_from_iter(values), |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let raw_rows = mapped.collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    let mut out = Vec::new();
    let mut skipped = 0usize;
    for (id, raw) in raw_rows {
        match serde_json::from_str::<Vec<f32>>(&raw) {
            Ok(vec) if embedding_contract.is_none() || valid_stored_vector_without_dims(&vec) => {
                out.push(vec)
            },
            _ => {
                skipped = skipped.saturating_add(1);
                if skipped <= 3 {
                    tracing::warn!(id, "removing malformed resurfacing affinity signal vector");
                }
                conn.execute(
                    "DELETE FROM resurfacing_affinity_signals WHERE id = ? AND vec = ?",
                    params![id, raw],
                )?;
            },
        }
    }
    Ok(out)
}

/// Persist one background-pass run record and enforce the per-scope history cap,
/// on a held connection (shared by the async [`ResurfacingStore::record_run`]).
/// Mirrors [`write_dismissed_signal`]'s INSERT-then-cap shape: after inserting we
/// keep only the newest [`RUN_HISTORY_CAP`] rows in the scope (ordered by `at`,
/// ties broken by the monotonic `id` so a burst of same-millisecond runs still
/// evicts oldest-first deterministically). `success` is stored as `0`/`1`;
/// `error` is the failure string (NULL on success).
#[allow(clippy::too_many_arguments)]
fn write_run(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    kind: &str,
    started_at: i64,
    duration_ms: i64,
    produced: i64,
    success: bool,
    error: Option<&str>,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO resurfacing_runs \
         (principal, workspace, kind, started_at, duration_ms, produced, success, error, at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            principal,
            workspace,
            kind,
            started_at,
            duration_ms,
            produced,
            success as i64,
            error,
            now,
        ],
    )
    .context("inserting resurfacing run record")?;
    // Bound the append-only history to the newest RUN_HISTORY_CAP rows in this
    // scope. The subquery picks the survivors; everything else in the scope is
    // deleted.
    conn.execute(
        "DELETE FROM resurfacing_runs \
         WHERE principal = ? AND workspace = ? \
         AND id NOT IN ( \
             SELECT id FROM resurfacing_runs \
             WHERE principal = ? AND workspace = ? \
             ORDER BY at DESC, id DESC LIMIT ? \
         )",
        params![
            principal,
            workspace,
            principal,
            workspace,
            RUN_HISTORY_CAP as i64,
        ],
    )
    .context("capping resurfacing run history")?;
    Ok(())
}

/// Down-weight the semantic neighbors of a just-dismissed candidate — the
/// "teaches similar future ones" half of dismiss-teaches.
///
/// Cosine-compares the dismissed item's (already-fetched) embedding against every
/// OTHER stored embedding in the scope. Each still-`candidate` row scoring
/// `>= SUPPRESS_THRESHOLD` has its salience multiplied by `SUPPRESS_FACTOR` —
/// state and cooldown are left untouched, so a near-duplicate simply becomes less
/// likely to be surfaced without being terminally silenced. The caller passes the
/// dismissed vector (read once, reused for the durable dismissed-signal row too);
/// runs on the caller's held connection (a transaction) so it commits with the
/// dismiss state change in one round-trip.
fn suppress_neighbors(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    dismissed_id: &str,
    embedding_contract: &str,
    target_vec: &[f32],
) -> Result<()> {
    let target = normalize(target_vec.to_vec());

    for (candidate_id, vec) in
        read_all_embeddings_for_contract(conn, principal, workspace, embedding_contract)?
    {
        // Never down-weight the dismissed item itself.
        if candidate_id == dismissed_id {
            continue;
        }
        if vec.len() != target_vec.len() {
            continue;
        }
        if cosine(&target, &normalize(vec)) >= SUPPRESS_THRESHOLD {
            // Only living candidates are down-weighted; surfaced/acted/dismissed/
            // snoozed rows are already out of the eligibility filter.
            conn.execute(
                "UPDATE resurfacing_candidates \
                 SET salience_score = salience_score * ? \
                 WHERE candidate_id = ? AND principal = ? AND workspace = ? \
                 AND state = ?",
                params![
                    SUPPRESS_FACTOR,
                    candidate_id,
                    principal,
                    workspace,
                    CandidateState::Candidate.as_str(),
                ],
            )
            .context("down-weighting a dismissed candidate's neighbor")?;
        }
    }
    Ok(())
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::str::FromStr;

    use super::*;
    use crate::magician_v2::attention::resurfacing::types::{
        candidate_id, ResurfacingContentDetails, ResurfacingDetailStatus, SalienceSignals,
    };

    fn sample_candidate(kind: &str, source_ref: &str, score: f32) -> Candidate {
        let source_kind = SourceKind::from_str(kind).unwrap();
        Candidate {
            candidate_id: candidate_id(source_kind, source_ref),
            source_kind,
            source_ref: source_ref.to_string(),
            title: format!("title-{source_ref}"),
            content_digest: format!("digest-{source_ref}"),
            content_details: None,
            content_revision: None,
            semantic_features: None,
            salience_score: score,
            signals: SalienceSignals::default(),
            temporal_anchor_at: None,
            embedding_id: None,
            state: CandidateState::Candidate,
            first_seen_at: 1_000,
            last_scored_at: 1_000,
            last_surfaced_at: None,
            cooldown_until: 0,
            surface_count: 0,
            dismiss_count: 0,
        }
    }

    async fn upsert_test_contract_embedding(
        store: &ResurfacingStore,
        candidate: &Candidate,
        embedding: &[f32],
    ) {
        store
            .upsert_embedding_for_digest(
                "p",
                "w",
                &candidate.candidate_id,
                &candidate.content_digest,
                "test-contract",
                embedding,
            )
            .await
            .unwrap();
    }

    /// Workspace deletion retires this database's rows for the scope. Scope
    /// discovery reads these tables, so a row left behind re-proposes a
    /// workspace that no longer exists; a neighbouring scope must be
    /// untouched by that retirement.
    #[tokio::test]
    async fn retire_scope_removes_only_that_scopes_rows() {
        let store = ResurfacingStore::open_in_temp();
        for (principal, workspace) in [("owner", "doomed"), ("owner", "kept")] {
            store
                .upsert_candidate(
                    principal,
                    workspace,
                    &sample_candidate("note", &format!("{workspace}-ref"), 0.5),
                )
                .await
                .unwrap();
        }
        assert_eq!(
            store.list_scopes().await.unwrap().len(),
            2,
            "both scopes are present before retirement"
        );

        let removed = store.retire_scope("owner", "doomed").await.unwrap();
        assert!(removed > 0, "retirement removed rows");

        let scopes = store.list_scopes().await.unwrap();
        assert!(
            !scopes.contains(&("owner".to_string(), "doomed".to_string())),
            "the retired scope is gone from discovery: {scopes:?}"
        );
        assert!(
            scopes.contains(&("owner".to_string(), "kept".to_string())),
            "the neighbouring scope survives: {scopes:?}"
        );
        assert_eq!(
            store.retire_scope("owner", "doomed").await.unwrap(),
            0,
            "retiring an already-retired scope is a no-op"
        );
    }

    #[tokio::test]
    async fn memory_applications_round_trip_on_revision_key() {
        let store = ResurfacingStore::open_in_temp();
        let judgement =
            crate::magician_v2::attention::resurfacing::memory_effects::MemoryJudgement {
                would_suppress: true,
                suppress_reason: Some("stated preference preferences: vendor".to_string()),
                ..Default::default()
            };
        store
            .put_memory_applications("p", "w", "cand-1", "rev-a", "mem-1", &judgement, 10)
            .await
            .unwrap();
        let got = store
            .get_memory_applications("p", "w", "cand-1", "rev-a", "mem-1")
            .await
            .unwrap()
            .expect("row");
        assert!(got.would_suppress);
        assert!(store
            .get_memory_applications("p", "w", "cand-1", "rev-b", "mem-1")
            .await
            .unwrap()
            .is_none());
        let latest = store
            .get_latest_memory_applications("p", "w", "cand-1")
            .await
            .unwrap()
            .expect("latest");
        assert_eq!(latest.suppress_reason, judgement.suppress_reason);
    }

    #[tokio::test]
    async fn memory_applications_for_content_revision_picks_newest_row_for_that_content() {
        let store = ResurfacingStore::open_in_temp();
        let older = crate::magician_v2::attention::resurfacing::memory_effects::MemoryJudgement {
            suppress_reason: Some("rev-a-old-mem".to_string()),
            ..Default::default()
        };
        let newer_same =
            crate::magician_v2::attention::resurfacing::memory_effects::MemoryJudgement {
                suppress_reason: Some("rev-a-new-mem".to_string()),
                ..Default::default()
            };
        let other_content =
            crate::magician_v2::attention::resurfacing::memory_effects::MemoryJudgement {
                suppress_reason: Some("rev-b-latest".to_string()),
                ..Default::default()
            };
        store
            .put_memory_applications("p", "w", "cand-1", "rev-a", "mem-1", &older, 10)
            .await
            .unwrap();
        store
            .put_memory_applications("p", "w", "cand-1", "rev-a", "mem-2", &newer_same, 20)
            .await
            .unwrap();
        store
            .put_memory_applications("p", "w", "cand-1", "rev-b", "mem-3", &other_content, 30)
            .await
            .unwrap();

        let for_a = store
            .get_memory_applications_for_content_revision("p", "w", "cand-1", "rev-a")
            .await
            .unwrap()
            .expect("rev-a");
        assert_eq!(for_a.suppress_reason.as_deref(), Some("rev-a-new-mem"));
        let for_b = store
            .get_memory_applications_for_content_revision("p", "w", "cand-1", "rev-b")
            .await
            .unwrap()
            .expect("rev-b");
        assert_eq!(for_b.suppress_reason.as_deref(), Some("rev-b-latest"));
        let latest = store
            .get_latest_memory_applications("p", "w", "cand-1")
            .await
            .unwrap()
            .expect("latest-any");
        assert_eq!(latest.suppress_reason.as_deref(), Some("rev-b-latest"));
        assert!(store
            .get_memory_applications_for_content_revision("p", "w", "cand-1", "rev-missing")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn open_migrates_pre_detail_candidate_and_phrasing_tables() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("resurfacing.db");
        let legacy_id = candidate_id(SourceKind::Comm, "legacy");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE resurfacing_candidates (
                candidate_id TEXT NOT NULL, principal TEXT NOT NULL,
                workspace TEXT NOT NULL, source_kind TEXT NOT NULL,
                source_ref TEXT NOT NULL, title TEXT NOT NULL,
                content_digest TEXT NOT NULL, salience_score REAL NOT NULL,
                signals TEXT NOT NULL, temporal_anchor_at INTEGER,
                embedding_id TEXT, state TEXT NOT NULL,
                first_seen_at INTEGER NOT NULL, last_scored_at INTEGER NOT NULL,
                last_surfaced_at INTEGER, cooldown_until INTEGER NOT NULL,
                surface_count INTEGER NOT NULL, dismiss_count INTEGER NOT NULL,
                PRIMARY KEY (principal, workspace, candidate_id)
             );
             CREATE TABLE resurfacing_phrasing (
                candidate_id TEXT NOT NULL, principal TEXT NOT NULL,
                workspace TEXT NOT NULL, line TEXT NOT NULL, why TEXT NOT NULL,
                at INTEGER NOT NULL,
                PRIMARY KEY (principal, workspace, candidate_id)
             );
             CREATE TABLE resurfacing_embeddings (
                candidate_id TEXT NOT NULL, principal TEXT NOT NULL,
                workspace TEXT NOT NULL, vec TEXT NOT NULL,
                PRIMARY KEY (principal, workspace, candidate_id)
             );
             CREATE TABLE resurfacing_dismissed_signals (
                id INTEGER PRIMARY KEY AUTOINCREMENT, principal TEXT NOT NULL,
                workspace TEXT NOT NULL, vec TEXT NOT NULL, at INTEGER NOT NULL
             );
             CREATE TABLE resurfacing_affinity_signals (
                id INTEGER PRIMARY KEY AUTOINCREMENT, principal TEXT NOT NULL,
                workspace TEXT NOT NULL, vec TEXT NOT NULL, at INTEGER NOT NULL
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO resurfacing_embeddings (candidate_id, principal, workspace, vec) \
             VALUES (?, 'p', 'w', '[0.6,0.8]')",
            params![legacy_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO resurfacing_dismissed_signals (principal, workspace, vec, at) VALUES ('p', 'w', '[1.0,0.0]', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO resurfacing_affinity_signals (principal, workspace, vec, at) VALUES ('p', 'w', '[1.0,0.0]', 1)",
            [],
        )
        .unwrap();
        drop(conn);

        let store = ResurfacingStore::open(tmp.path()).unwrap();
        let mut candidate = sample_candidate("comm", "legacy", 0.5);
        candidate.content_revision = Some("9".to_string());
        candidate.content_details = Some(ResurfacingContentDetails {
            schema_version: 2,
            key_facts: vec!["Effective July 1".to_string()],
            changes: Vec::new(),
            temporal_facts: Vec::new(),
            detail_status: ResurfacingDetailStatus::Complete,
            missing_details: Vec::new(),
        });
        store.upsert_candidate("p", "w", &candidate).await.unwrap();
        store
            .upsert_phrasing(
                "p",
                "w",
                &candidate.candidate_id,
                "line",
                "why",
                candidate.content_revision.as_deref(),
                10,
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .get_candidate("p", "w", &candidate.candidate_id)
                .await
                .unwrap()
                .unwrap()
                .content_revision
                .as_deref(),
            Some("9")
        );
        assert_eq!(
            store.list_dismissed_signals("p", "w").await.unwrap().len(),
            1
        );
        assert!(store
            .list_dismissed_signals_for_contract("p", "w", "contract-v1")
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            store.list_affinity_signals("p", "w").await.unwrap().len(),
            1
        );
        assert!(store
            .list_affinity_signals_for_contract("p", "w", "contract-v1")
            .await
            .unwrap()
            .is_empty());
        assert!(store
            .get_phrasing("p", "w", &candidate.candidate_id)
            .await
            .unwrap()
            .is_some());

        let ids = vec![candidate.candidate_id.clone()];
        assert!(
            store
                .list_candidate_embedding_snapshots_for_ids("p", "w", &ids)
                .await
                .unwrap()
                .is_empty(),
            "a migrated row with no digest/contract is not reusable"
        );
        store
            .upsert_embedding("p", "w", &candidate.candidate_id, &[0.6, 0.8])
            .await
            .unwrap();
        assert!(
            store
                .list_candidate_embedding_snapshots_for_ids("p", "w", &ids)
                .await
                .unwrap()
                .is_empty(),
            "digest backfill alone cannot cross an unknown model contract"
        );
        store
            .upsert_embedding_for_digest(
                "p",
                "w",
                &candidate.candidate_id,
                &candidate.content_digest,
                "contract-v1",
                &[0.6, 0.8],
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .list_candidate_embedding_snapshots_for_ids("p", "w", &ids)
                .await
                .unwrap(),
            vec![CandidateEmbeddingSnapshot {
                candidate_id: candidate.candidate_id,
                content_digest: candidate.content_digest,
                embedding_contract: "contract-v1".to_string(),
                embedding: vec![0.6, 0.8],
            }]
        );
    }

    #[tokio::test]
    async fn upsert_then_get_candidate_roundtrips() {
        let store = ResurfacingStore::open_in_temp();
        let mut c = sample_candidate("memory", "k1", 0.5);
        c.content_revision = Some("7".to_string());
        c.content_details = Some(ResurfacingContentDetails {
            schema_version: 2,
            key_facts: vec!["Limit: 10".to_string()],
            changes: Vec::new(),
            temporal_facts: Vec::new(),
            detail_status: ResurfacingDetailStatus::Complete,
            missing_details: Vec::new(),
        });
        store
            .upsert_candidate("anonymous", "default", &c)
            .await
            .unwrap();
        let got = store
            .get_candidate("anonymous", "default", &c.candidate_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.salience_score, 0.5);
        assert_eq!(got.state, CandidateState::Candidate);
        assert_eq!(got.source_kind, SourceKind::Memory);
        assert_eq!(got.source_ref, "k1");
        assert_eq!(got.content_revision.as_deref(), Some("7"));
        assert_eq!(got.content_details, c.content_details);
    }

    #[tokio::test]
    async fn surfaced_source_generation_token_is_stable_and_tracks_authoritative_rows() {
        let store = ResurfacingStore::open_in_temp();
        let mut surfaced = sample_candidate("memory", "generation-source", 0.5);
        surfaced.state = CandidateState::Surfaced;
        surfaced.last_surfaced_at = Some(0);
        store
            .upsert_candidate("anonymous", "default", &surfaced)
            .await
            .unwrap();
        let first = store
            .surfaced_source_generation_token("anonymous", "default", 1_000, 1_000)
            .await
            .unwrap();
        assert_eq!(
            first,
            store
                .surfaced_source_generation_token("anonymous", "default", 1_000, 1_000)
                .await
                .unwrap()
        );
        assert_eq!(
            first,
            store
                .surfaced_source_generation_token("anonymous", "default", 10_000, 1_000)
                .await
                .unwrap(),
            "clock movement inside one temporal bucket must preserve source identity"
        );
        assert_ne!(
            first,
            store
                .surfaced_source_generation_token("anonymous", "default", 1_000_000, 1_000)
                .await
                .unwrap(),
            "crossing the model's temporal bucket must invalidate projection identity"
        );

        surfaced.title = "changed authoritative title".to_string();
        store
            .upsert_candidate("anonymous", "default", &surfaced)
            .await
            .unwrap();
        let changed = store
            .surfaced_source_generation_token("anonymous", "default", 1_000, 1_000)
            .await
            .unwrap();
        assert_ne!(first, changed);

        let hidden = sample_candidate("memory", "not-surfaced", 0.9);
        store
            .upsert_candidate("anonymous", "default", &hidden)
            .await
            .unwrap();
        assert_eq!(
            changed,
            store
                .surfaced_source_generation_token("anonymous", "default", 1_000, 1_000)
                .await
                .unwrap(),
            "non-surfaced rows must not invalidate a frozen surfaced cursor"
        );

        let mut outside_slice = sample_candidate("memory", "outside-slice", 0.1);
        outside_slice.state = CandidateState::Surfaced;
        outside_slice.last_surfaced_at = Some(0);
        store
            .upsert_candidate("anonymous", "default", &outside_slice)
            .await
            .unwrap();
        let capped = store
            .surfaced_source_generation_token("anonymous", "default", 1_000, 1)
            .await
            .unwrap();
        outside_slice.title = "changed outside bounded slice".to_string();
        store
            .upsert_candidate("anonymous", "default", &outside_slice)
            .await
            .unwrap();
        assert_eq!(
            capped,
            store
                .surfaced_source_generation_token("anonymous", "default", 1_000, 1)
                .await
                .unwrap()
        );
        outside_slice.salience_score = 0.9;
        store
            .upsert_candidate("anonymous", "default", &outside_slice)
            .await
            .unwrap();
        assert_ne!(
            capped,
            store
                .surfaced_source_generation_token("anonymous", "default", 1_000, 1)
                .await
                .unwrap(),
            "entering the bounded canonical slice must invalidate its source identity"
        );
    }

    #[tokio::test]
    async fn watermark_advances_and_is_read_back() {
        let tmp = tempfile::TempDir::new().unwrap();
        let store = ResurfacingStore::open(tmp.path()).unwrap();
        assert_eq!(
            store
                .get_watermark("anonymous", "default", "memory")
                .await
                .unwrap(),
            0
        );
        store
            .set_watermark("anonymous", "default", "memory", 42)
            .await
            .unwrap();
        drop(store);
        let store = ResurfacingStore::open(tmp.path()).unwrap();
        store
            .set_watermark("anonymous", "default", "memory", 41)
            .await
            .unwrap();
        assert_eq!(
            store
                .get_watermark("anonymous", "default", "memory")
                .await
                .unwrap(),
            42
        );
    }

    #[tokio::test]
    async fn list_top_candidates_orders_by_score_and_respects_cooldown() {
        let store = ResurfacingStore::open_in_temp();
        store
            .upsert_candidate("p", "w", &sample_candidate("memory", "hi", 0.9))
            .await
            .unwrap();
        let mut cold = sample_candidate("memory", "cold", 0.95);
        cold.cooldown_until = i64::MAX; // on cooldown
        store.upsert_candidate("p", "w", &cold).await.unwrap();
        let top = store.list_top_candidates("p", "w", 0, 10).await.unwrap();
        // cooled-down excluded despite higher score
        assert_eq!(top.first().unwrap().source_ref, "hi");
    }

    #[tokio::test]
    async fn list_top_candidates_scans_past_undecodable_rows() {
        let store = ResurfacingStore::open_in_temp();
        let bad = sample_candidate("memory", "bad", 1.0);
        let good = sample_candidate("memory", "good", 0.1);
        store.upsert_candidate("p", "w", &bad).await.unwrap();
        store.upsert_candidate("p", "w", &good).await.unwrap();
        {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            conn.execute(
                "UPDATE resurfacing_candidates SET signals = ? \
                 WHERE principal = ? AND workspace = ? AND candidate_id = ?",
                rusqlite::params!["not-json", "p", "w", bad.candidate_id.as_str()],
            )
            .unwrap();
        }

        let top = store.list_top_candidates("p", "w", 0, 1).await.unwrap();

        assert_eq!(top.len(), 1);
        assert_eq!(top[0].source_ref, "good");
    }

    #[tokio::test]
    async fn surfaced_page_cursor_advances_past_undecodable_rows() {
        let store = ResurfacingStore::open_in_temp();
        let mut candidates = Vec::new();
        for (source_ref, score) in [
            ("good-1", 1.0),
            ("good-2", 0.99),
            ("bad-0", 0.98),
            ("bad-1", 0.97),
            ("bad-2", 0.96),
            ("bad-3", 0.95),
            ("bad-4", 0.94),
            ("bad-5", 0.93),
            ("bad-6", 0.92),
            ("bad-7", 0.91),
            ("bad-8", 0.90),
            ("bad-9", 0.89),
            ("good-3", 0.1),
        ] {
            let candidate = sample_candidate("memory", source_ref, score);
            store.upsert_candidate("p", "w", &candidate).await.unwrap();
            candidates.push(candidate);
        }
        let ids = candidates
            .iter()
            .map(|candidate| candidate.candidate_id.clone())
            .collect::<Vec<_>>();
        store.mark_surfaced("p", "w", &ids, 10_000).await.unwrap();

        {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for candidate in candidates
                .iter()
                .filter(|candidate| candidate.source_ref.starts_with("bad-"))
            {
                conn.execute(
                    "UPDATE resurfacing_candidates SET signals = ? \
                     WHERE principal = ? AND workspace = ? AND candidate_id = ?",
                    rusqlite::params!["not-json", "p", "w", candidate.candidate_id.as_str()],
                )
                .unwrap();
            }
        }

        let first = store
            .list_surfaced_page("p", "w", 2, 0, None)
            .await
            .unwrap();
        assert_eq!(
            first
                .candidates
                .iter()
                .map(|candidate| candidate.source_ref.as_str())
                .collect::<Vec<_>>(),
            vec!["good-1", "good-2"]
        );
        let cursor = first
            .next_cursor
            .expect("cursor should advance past undecodable boundary rows");

        let second = store
            .list_surfaced_page("p", "w", 2, 0, Some(cursor))
            .await
            .unwrap();
        assert_eq!(
            second
                .candidates
                .iter()
                .map(|candidate| candidate.source_ref.as_str())
                .collect::<Vec<_>>(),
            vec!["good-3"]
        );
    }

    #[tokio::test]
    async fn surfaced_page_cursor_preserves_exact_database_score() {
        let store = ResurfacingStore::open_in_temp();
        let high = sample_candidate("memory", "high", 0.7);
        let boundary = sample_candidate("memory", "boundary", 0.6);
        let low = sample_candidate("memory", "low", 0.5);
        for candidate in [&high, &boundary, &low] {
            store.upsert_candidate("p", "w", candidate).await.unwrap();
        }
        store
            .mark_surfaced(
                "p",
                "w",
                &[
                    high.candidate_id.clone(),
                    boundary.candidate_id.clone(),
                    low.candidate_id.clone(),
                ],
                10_000,
            )
            .await
            .unwrap();

        // RL score recomputation is performed by SQLite and can persist a REAL
        // that is not exactly representable as f32. The old cursor rounded this
        // value up, causing the boundary row to qualify for the next page too.
        let exact_boundary_score = 0.619_111_161_635_338_7_f64;
        {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            conn.execute(
                "UPDATE resurfacing_candidates SET salience_score = ? \
                 WHERE principal = ? AND workspace = ? AND candidate_id = ?",
                rusqlite::params![
                    exact_boundary_score,
                    "p",
                    "w",
                    boundary.candidate_id.as_str()
                ],
            )
            .unwrap();
            conn.execute(
                "UPDATE resurfacing_candidates SET salience_score = ? \
                 WHERE principal = ? AND workspace = ? AND candidate_id = ?",
                rusqlite::params![0.6_f64, "p", "w", low.candidate_id.as_str()],
            )
            .unwrap();
        }

        let first = store
            .list_surfaced_page("p", "w", 2, 0, None)
            .await
            .unwrap();
        assert_eq!(
            first
                .candidates
                .iter()
                .map(|candidate| candidate.source_ref.as_str())
                .collect::<Vec<_>>(),
            vec!["high", "boundary"]
        );
        let cursor = first.next_cursor.expect("next cursor");
        assert_eq!(cursor.salience_score, exact_boundary_score);

        let second = store
            .list_surfaced_page("p", "w", 2, 0, Some(cursor))
            .await
            .unwrap();
        assert_eq!(
            second
                .candidates
                .iter()
                .map(|candidate| candidate.source_ref.as_str())
                .collect::<Vec<_>>(),
            vec!["low"]
        );
    }

    #[tokio::test]
    async fn surfaced_comm_source_refs_count_the_lane_without_paging_it() {
        let store = ResurfacingStore::open_in_temp();
        let comm_a = sample_candidate("comm", "gmail/work/thread-a/msg-1@1700000000000", 0.9);
        let comm_b = sample_candidate("comm", "gmail/work/thread-b/msg-2@1700000000000", 0.8);
        let memory = sample_candidate("memory", "note-1", 0.7);
        let unsurfaced = sample_candidate("comm", "gmail/work/thread-c/msg-3@1700000000000", 0.6);
        for candidate in [&comm_a, &comm_b, &memory, &unsurfaced] {
            store.upsert_candidate("p", "w", candidate).await.unwrap();
        }
        store
            .mark_surfaced(
                "p",
                "w",
                &[
                    comm_a.candidate_id.clone(),
                    comm_b.candidate_id.clone(),
                    memory.candidate_id.clone(),
                ],
                10_000,
            )
            .await
            .unwrap();

        let identities = store
            .list_surfaced_comm_source_refs("p", "w")
            .await
            .unwrap();

        // The total counts every surfaced row; only surfaced COMMUNICATION
        // rows can be hidden by a Follow-up, so only they come back.
        assert_eq!(identities.total, 3);
        let mut refs = identities.comm_source_refs;
        refs.sort();
        assert_eq!(
            refs,
            vec![comm_a.source_ref.clone(), comm_b.source_ref.clone()],
            "an unsurfaced comm row is not in the lane and a memory row can never be hidden"
        );

        // A different scope shares the file and must not leak into either.
        let other = store
            .list_surfaced_comm_source_refs("p", "other")
            .await
            .unwrap();
        assert_eq!(other.total, 0);
        assert!(other.comm_source_refs.is_empty());
    }

    #[tokio::test]
    async fn defer_candidates_removes_reviewed_rows_from_active_queue() {
        let store = ResurfacingStore::open_in_temp();
        let hi = sample_candidate("memory", "hi", 0.9);
        let lo = sample_candidate("memory", "lo", 0.5);
        store.upsert_candidate("p", "w", &hi).await.unwrap();
        store.upsert_candidate("p", "w", &lo).await.unwrap();

        let updated = store
            .defer_candidates("p", "w", &[hi.candidate_id.clone()], 5_000)
            .await
            .unwrap();
        assert_eq!(updated, vec![hi.candidate_id.clone()]);

        let stats = store.queue_stats("p", "w", 1_000).await.unwrap();
        assert_eq!(stats.candidate_pool, 2);
        assert_eq!(stats.pending, 1);
        assert_eq!(stats.eligible, 1);
        assert_eq!(stats.cooling, 1);

        let top = store
            .list_top_candidates("p", "w", 1_000, 10)
            .await
            .unwrap();
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].source_ref, "lo");

        let later = store
            .list_top_candidates("p", "w", 5_001, 10)
            .await
            .unwrap();
        assert_eq!(later.first().unwrap().source_ref, "hi");
    }

    #[tokio::test]
    async fn phrasing_upsert_and_get_roundtrips() {
        let store = ResurfacingStore::open_in_temp();
        let cid = candidate_id(SourceKind::Memory, "k1");

        // Absent until written.
        assert!(store.get_phrasing("p", "w", &cid).await.unwrap().is_none());

        store
            .upsert_phrasing(
                "p",
                "w",
                &cid,
                "Circle back on the Tokyo trip",
                "a date is near",
                None,
                10,
            )
            .await
            .unwrap();
        let got = store.get_phrasing("p", "w", &cid).await.unwrap().unwrap();
        assert_eq!(got.0, "Circle back on the Tokyo trip");
        assert_eq!(got.1, "a date is near");

        // Idempotent on candidate_id — the second write replaces the first.
        store
            .upsert_phrasing("p", "w", &cid, "New line", "new why", None, 20)
            .await
            .unwrap();
        let got = store.get_phrasing("p", "w", &cid).await.unwrap().unwrap();
        assert_eq!(got.0, "New line");
        assert_eq!(got.1, "new why");

        // Scoped by principal/workspace — a different scope sees nothing.
        assert!(store
            .get_phrasing("other", "w", &cid)
            .await
            .unwrap()
            .is_none());
    }

    /// The batched page lookup must answer exactly what a loop of
    /// single-candidate lookups answered — same rows, same revision guard,
    /// same scoping, absence for anything unwritten — because that loop is
    /// what the list endpoints replaced with it.
    #[tokio::test]
    async fn phrasing_batch_matches_a_loop_of_single_lookups() {
        let store = ResurfacingStore::open_in_temp();
        let written = candidate_id(SourceKind::Memory, "written");
        let unwritten = candidate_id(SourceKind::Memory, "unwritten");
        store
            .upsert_phrasing("p", "w", &written, "line one", "why one", None, 10)
            .await
            .unwrap();

        let ids = vec![written.clone(), unwritten.clone()];
        let batch = store.get_phrasing_batch("p", "w", &ids).await.unwrap();
        for id in &ids {
            assert_eq!(
                batch.get(id).cloned(),
                store.get_phrasing("p", "w", id).await.unwrap(),
                "batch disagreed with the single lookup for {id}"
            );
        }
        assert_eq!(batch.len(), 1);

        // Same scoping as the single form, and an empty request is free.
        assert!(store
            .get_phrasing_batch("other", "w", &ids)
            .await
            .unwrap()
            .is_empty());
        assert!(store
            .get_phrasing_batch("p", "w", &[])
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn decay_reduces_score_over_elapsed_time() {
        let store = ResurfacingStore::open_in_temp();
        let mut c = sample_candidate("memory", "d", 1.0);
        c.last_scored_at = 0;
        store.upsert_candidate("p", "w", &c).await.unwrap();
        // one half-life elapsed -> ~0.5
        store.decay_all("p", "w", 1.0, 86_400).await.unwrap();
        let got = store
            .get_candidate("p", "w", &c.candidate_id)
            .await
            .unwrap()
            .unwrap();
        assert!((got.salience_score - 0.5).abs() < 0.01);
    }

    #[tokio::test]
    async fn embedding_upsert_get_list_roundtrips() {
        let store = ResurfacingStore::open_in_temp();
        let a = candidate_id(SourceKind::Memory, "a");
        let b = candidate_id(SourceKind::Memory, "b");

        // Absent until written.
        assert!(store.get_embedding("p", "w", &a).await.unwrap().is_none());

        store
            .upsert_embedding("p", "w", &a, &[1.0, 2.0, 3.0])
            .await
            .unwrap();
        store
            .upsert_embedding("p", "w", &b, &[4.0, 5.0])
            .await
            .unwrap();
        assert_eq!(
            store.get_embedding("p", "w", &a).await.unwrap().unwrap(),
            vec![1.0, 2.0, 3.0]
        );

        // Idempotent on candidate_id — the second write replaces the first.
        store.upsert_embedding("p", "w", &a, &[9.0]).await.unwrap();
        assert_eq!(
            store.get_embedding("p", "w", &a).await.unwrap().unwrap(),
            vec![9.0]
        );

        let all = store.list_embeddings("p", "w").await.unwrap();
        assert_eq!(all.len(), 2);

        // Scoped by principal/workspace — a different scope sees nothing.
        assert!(store
            .get_embedding("other", "w", &a)
            .await
            .unwrap()
            .is_none());
        assert!(store
            .list_embeddings("other", "w")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn candidate_embedding_snapshots_are_targeted_and_include_pre_candidate_cache() {
        let store = ResurfacingStore::open_in_temp();
        let mut candidate = sample_candidate("memory", "durable", 1.0);
        candidate.content_digest = "digest-v1".to_string();
        store.upsert_candidate("p", "w", &candidate).await.unwrap();
        store
            .upsert_embedding_for_digest(
                "p",
                "w",
                &candidate.candidate_id,
                "digest-v1",
                "contract-a",
                &[1.0, 2.0],
            )
            .await
            .unwrap();
        // A later content refresh without a replacement vector must not relabel
        // the old vector as current; its association stays digest-v1 so the
        // centrality builder can reject it against digest-v2.
        candidate.content_digest = "digest-v2".to_string();
        store.upsert_candidate("p", "w", &candidate).await.unwrap();
        store
            .upsert_embedding_for_digest(
                "p",
                "w",
                "pre-candidate",
                "digest-pre",
                "contract-a",
                &[0.0, 1.0],
            )
            .await
            .unwrap();
        store
            .upsert_embedding_for_digest(
                "p",
                "w",
                "not-requested",
                "digest-other",
                "contract-a",
                &[9.0, 9.0],
            )
            .await
            .unwrap();

        let requested = vec![candidate.candidate_id.clone(), "pre-candidate".to_string()];

        assert_eq!(
            store
                .list_candidate_embedding_snapshots_for_ids("p", "w", &requested)
                .await
                .unwrap(),
            vec![
                CandidateEmbeddingSnapshot {
                    candidate_id: candidate.candidate_id,
                    content_digest: "digest-v1".to_string(),
                    embedding_contract: "contract-a".to_string(),
                    embedding: vec![1.0, 2.0],
                },
                CandidateEmbeddingSnapshot {
                    candidate_id: "pre-candidate".to_string(),
                    content_digest: "digest-pre".to_string(),
                    embedding_contract: "contract-a".to_string(),
                    embedding: vec![0.0, 1.0],
                },
            ]
        );
    }

    #[test]
    fn unattached_embedding_cache_is_bounded_without_pruning_candidates() {
        let store = ResurfacingStore::open_in_temp();
        let conn = store
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.execute(
            "INSERT INTO resurfacing_candidates (candidate_id, principal, workspace, source_kind, source_ref, title, content_digest, salience_score, signals, state, first_seen_at, last_scored_at, cooldown_until, surface_count, dismiss_count) \
             VALUES ('attached', 'p', 'w', 'memory', 'attached', 'attached', 'd', 1.0, '{}', 'candidate', 1, 1, 0, 0, 0)",
            [],
        )
        .unwrap();
        for (id, updated_at) in [("old", 1), ("middle", 2), ("new", 3), ("attached", 0)] {
            conn.execute(
                "INSERT INTO resurfacing_embeddings (candidate_id, principal, workspace, content_digest, embedding_contract, vec, updated_at) \
                 VALUES (?, 'p', 'w', 'digest', 'contract', '[1.0,0.0]', ?)",
                params![id, updated_at],
            )
            .unwrap();
        }
        prune_unattached_embedding_cache(&conn, "p", "w", 2).unwrap();
        let remaining: Vec<String> = conn
            .prepare("SELECT candidate_id FROM resurfacing_embeddings WHERE principal = 'p' AND workspace = 'w' ORDER BY candidate_id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(remaining, vec!["attached", "middle", "new"]);
    }

    #[tokio::test]
    async fn feedback_vectors_are_isolated_by_exact_embedding_contract() {
        let store = ResurfacingStore::open_in_temp();
        {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            write_dismissed_signal(&conn, "p", "w", None, Some("model-a"), &[1.0, 0.0], 1).unwrap();
            write_dismissed_signal(&conn, "p", "w", None, Some("model-b"), &[0.0, 1.0], 2).unwrap();
            write_affinity_signal(&conn, "p", "w", None, Some("model-a"), &[0.8, 0.2], 1).unwrap();
            write_affinity_signal(&conn, "p", "w", None, Some("model-b"), &[0.2, 0.8], 2).unwrap();
            conn.execute(
                "INSERT INTO resurfacing_dismissed_signals (principal, workspace, embedding_contract, vec, at) VALUES ('p', 'w', 'model-a', 'not-json', 3)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO resurfacing_affinity_signals (principal, workspace, embedding_contract, vec, at) VALUES ('p', 'w', 'model-b', '[0.0,0.0]', 3)",
                [],
            )
            .unwrap();
        }
        assert_eq!(
            store
                .list_dismissed_signals_for_contract("p", "w", "model-a")
                .await
                .unwrap(),
            vec![vec![1.0, 0.0]]
        );
        assert_eq!(
            store
                .list_affinity_signals_for_contract("p", "w", "model-b")
                .await
                .unwrap(),
            vec![vec![0.2, 0.8]]
        );
    }

    #[tokio::test]
    async fn malformed_snapshot_row_does_not_discard_healthy_reuse() {
        let store = ResurfacingStore::open_in_temp();
        let mut ids = Vec::with_capacity(512);
        {
            let mut conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            let tx = conn.transaction().unwrap();
            for index in 0..511 {
                let id = format!("healthy-{index:03}");
                ids.push(id.clone());
                tx.execute(
                    "INSERT INTO resurfacing_embeddings (candidate_id, principal, workspace, content_digest, embedding_contract, vec, updated_at) VALUES (?, 'p', 'w', 'digest', 'contract', '[0.6,0.8]', 1)",
                    params![id],
                )
                .unwrap();
            }
            ids.push("zz-corrupt".to_string());
            tx.execute(
                "INSERT INTO resurfacing_embeddings (candidate_id, principal, workspace, content_digest, embedding_contract, vec, updated_at) VALUES ('zz-corrupt', 'p', 'w', 'digest', 'contract', 'not-json', 1)",
                [],
            )
            .unwrap();
            tx.commit().unwrap();
        }
        let rows = store
            .list_candidate_embedding_snapshots_for_ids("p", "w", &ids)
            .await
            .unwrap();
        assert_eq!(rows.len(), 511);
        assert!(rows.iter().all(|row| row.candidate_id != "zz-corrupt"));
        assert_eq!(
            store
                .list_candidate_embedding_snapshots_for_ids("p", "w", &["zz-corrupt".to_string()],)
                .await
                .unwrap(),
            Vec::new(),
            "successful best-effort cleanup should self-heal the corrupt row"
        );
    }

    #[tokio::test]
    async fn snapshot_cleanup_failure_still_returns_healthy_rows() {
        let store = ResurfacingStore::open_in_temp();
        {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.execute_batch(
                "INSERT INTO resurfacing_embeddings (candidate_id, principal, workspace, content_digest, embedding_contract, vec, updated_at) VALUES
                    ('healthy', 'p', 'w', 'digest', 'contract', '[0.6,0.8]', 1),
                    ('corrupt', 'p', 'w', 'digest', 'contract', 'not-json', 1);
                 CREATE TRIGGER block_embedding_cleanup BEFORE DELETE ON resurfacing_embeddings
                 BEGIN SELECT RAISE(ABORT, 'cleanup blocked'); END;",
            )
            .unwrap();
        }
        let rows = store
            .list_candidate_embedding_snapshots_for_ids(
                "p",
                "w",
                &["healthy".to_string(), "corrupt".to_string()],
            )
            .await
            .expect("cleanup is best-effort and cannot discard a healthy snapshot");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].candidate_id, "healthy");
    }

    #[tokio::test]
    async fn reference_precache_never_overwrites_an_attached_old_card_before_promotion() {
        let store = ResurfacingStore::open_in_temp();
        let mut candidate = sample_candidate("memory", "race", 1.0);
        candidate.content_digest = "old-digest".to_string();
        store.upsert_candidate("p", "w", &candidate).await.unwrap();
        store
            .upsert_embedding_for_digest(
                "p",
                "w",
                &candidate.candidate_id,
                "old-digest",
                "model-a",
                &[1.0, 0.0],
            )
            .await
            .unwrap();

        let future = CandidateEmbeddingSnapshot {
            candidate_id: candidate.candidate_id.clone(),
            content_digest: "new-digest".to_string(),
            embedding_contract: "model-a".to_string(),
            embedding: vec![0.0, 1.0],
        };
        assert_eq!(
            store
                .upsert_embeddings_for_digests("p", "w", std::slice::from_ref(&future))
                .await
                .unwrap(),
            1,
            "a versioned pre-cache row must advance even while the old card remains attached"
        );
        let ids = vec![candidate.candidate_id.clone()];
        let before = store
            .list_candidate_embedding_snapshots_for_ids("p", "w", &ids)
            .await
            .unwrap();
        let old = before
            .iter()
            .find(|row| row.content_digest == "old-digest")
            .expect("old action vector remains attached");
        assert_eq!(old.embedding, vec![1.0, 0.0]);
        assert!(before.iter().any(|row| row == &future));
        store
            .record_action_with_reason(
                "p",
                "w",
                &candidate.candidate_id,
                FeedbackAction::Dismiss,
                Some(DismissReason::NotRelevant),
                10,
                10,
                10,
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .list_dismissed_signals_for_contract("p", "w", "model-a")
                .await
                .unwrap(),
            vec![vec![1.0, 0.0]],
            "an old-card action during pre-cache must teach the old card vector"
        );

        candidate.content_digest = "new-digest".to_string();
        store.upsert_candidate("p", "w", &candidate).await.unwrap();
        store
            .record_action_with_reason(
                "p",
                "w",
                &candidate.candidate_id,
                FeedbackAction::Dismiss,
                Some(DismissReason::NotRelevant),
                20,
                10,
                10,
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .list_dismissed_signals_for_contract("p", "w", "model-a")
                .await
                .unwrap()
                .len(),
            1,
            "candidate/embedding digest mismatch must not teach the stale vector"
        );
        store
            .upsert_embedding_for_digest(
                "p",
                "w",
                &future.candidate_id,
                &future.content_digest,
                &future.embedding_contract,
                &future.embedding,
            )
            .await
            .unwrap();
        let promoted = store
            .list_candidate_embedding_snapshots_for_ids("p", "w", &ids)
            .await
            .unwrap();
        assert_eq!(promoted[0], future);
    }

    #[tokio::test]
    async fn live_feedback_outbox_is_atomic_and_event_idempotent() {
        let store = ResurfacingStore::open_in_temp();
        let mut candidate = sample_candidate("memory", "repair", 1.0);
        candidate.title = "x".repeat(9_000);
        store.upsert_candidate("p", "w", &candidate).await.unwrap();
        store
            .upsert_embedding_for_digest(
                "p",
                "w",
                &candidate.candidate_id,
                &candidate.content_digest,
                "contract-v1",
                &[1.0, 0.0],
            )
            .await
            .unwrap();

        for invalid_event_id in [String::new(), "   ".to_string(), "x".repeat(201)] {
            assert!(store
                .record_action_with_reason_event(
                    "p",
                    "w",
                    &candidate.candidate_id,
                    FeedbackAction::Dismiss,
                    Some(DismissReason::Spam),
                    Some(&invalid_event_id),
                    9,
                    10,
                    10,
                )
                .await
                .is_err());
        }

        for _ in 0..2 {
            store
                .record_action_with_reason_event(
                    "p",
                    "w",
                    &candidate.candidate_id,
                    FeedbackAction::Dismiss,
                    Some(DismissReason::Spam),
                    Some("event-1"),
                    10,
                    10,
                    10,
                )
                .await
                .unwrap();
        }
        let rows = store
            .list_feedback_attention_repairs_after("p", "w", 0, 0, 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].event_id, "event-1");
        assert_eq!(rows[0].reason, Some(DismissReason::Spam));
        assert_eq!(rows[0].semantic_text.chars().count(), 8_192);
        assert_eq!(rows[0].embedding_contract.as_deref(), Some("contract-v1"));
        assert_eq!(rows[0].embedding.as_deref(), Some([1.0, 0.0].as_slice()));
        let signals = store
            .list_dismissed_attention_signals_after("p", "w", 0, 11, 10)
            .await
            .unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].live_event_id.as_deref(), Some("event-1"));
    }

    #[tokio::test]
    async fn malformed_feedback_repair_rows_remain_cursor_visible() {
        let store = ResurfacingStore::open_in_temp();
        {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.execute(
                "INSERT INTO resurfacing_feedback ( \
                    principal, workspace, candidate_id, action, event_id, semantic_text, at \
                 ) VALUES (?, ?, ?, ?, ?, ?, ?)",
                params![
                    "p",
                    "w",
                    "candidate-1",
                    "future_action",
                    "event-invalid",
                    "bounded private brief",
                    10,
                ],
            )
            .unwrap();
        }

        let rows = store
            .list_feedback_attention_repairs_after("p", "w", 0, 0, 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].event_id, "event-invalid");
        assert_eq!(rows[0].action, None);
    }

    #[tokio::test]
    async fn changed_attached_cards_make_monotonic_versioned_cache_progress_across_passes() {
        let store = ResurfacingStore::open_in_temp();
        let future: Vec<CandidateEmbeddingSnapshot> = (0..130)
            .map(|index| CandidateEmbeddingSnapshot {
                candidate_id: format!("attached-{index:03}"),
                content_digest: format!("new-{index:03}"),
                embedding_contract: "model-a".to_string(),
                embedding: vec![0.0, 1.0],
            })
            .collect();
        {
            let mut conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            let tx = conn.transaction().unwrap();
            for row in &future {
                tx.execute(
                    "INSERT INTO resurfacing_candidates (candidate_id, principal, workspace, source_kind, source_ref, title, content_digest, salience_score, signals, state, first_seen_at, last_scored_at, cooldown_until, surface_count, dismiss_count)
                     VALUES (?, 'p', 'w', 'memory', ?, 'old card', ?, 1.0, '{}', 'candidate', 1, 1, 0, 0, 0)",
                    params![row.candidate_id, row.candidate_id, format!("old-{}", row.candidate_id)],
                )
                .unwrap();
                tx.execute(
                    "INSERT INTO resurfacing_embeddings (candidate_id, principal, workspace, content_digest, embedding_contract, vec, updated_at)
                     VALUES (?, 'p', 'w', ?, 'model-a', '[1.0,0.0]', 1)",
                    params![row.candidate_id, format!("old-{}", row.candidate_id)],
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }
        let ids: Vec<String> = future.iter().map(|row| row.candidate_id.clone()).collect();
        assert_eq!(
            store
                .upsert_embeddings_for_digests("p", "w", &future[..65])
                .await
                .unwrap(),
            65
        );
        let first = store
            .list_candidate_embedding_snapshots_for_ids("p", "w", &ids)
            .await
            .unwrap();
        assert_eq!(
            first
                .iter()
                .filter(|row| row.content_digest.starts_with("new-"))
                .count(),
            65,
            "a deadline-limited first prefix remains reusable without scoring promotion"
        );
        assert_eq!(
            store
                .upsert_embeddings_for_digests("p", "w", &future[65..])
                .await
                .unwrap(),
            65
        );
        let second = store
            .list_candidate_embedding_snapshots_for_ids("p", "w", &ids)
            .await
            .unwrap();
        assert_eq!(
            second
                .iter()
                .filter(|row| row.content_digest.starts_with("new-"))
                .count(),
            130,
            "the second pass advances to the suffix instead of re-embedding the prefix"
        );
        assert_eq!(
            second
                .iter()
                .filter(|row| row.content_digest.starts_with("old-"))
                .count(),
            130,
            "old action vectors remain intact until scorer promotion"
        );
    }

    #[tokio::test]
    async fn malformed_current_embedding_does_not_rollback_the_user_action() {
        let store = ResurfacingStore::open_in_temp();
        let candidate = sample_candidate("memory", "malformed-current", 1.0);
        store.upsert_candidate("p", "w", &candidate).await.unwrap();
        {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.execute(
                "INSERT INTO resurfacing_embeddings (candidate_id, principal, workspace, content_digest, embedding_contract, vec, updated_at) VALUES (?, 'p', 'w', ?, 'test-contract', 'not-json', 1)",
                params![candidate.candidate_id, candidate.content_digest],
            )
            .unwrap();
        }
        store
            .record_action_with_reason(
                "p",
                "w",
                &candidate.candidate_id,
                FeedbackAction::Dismiss,
                Some(DismissReason::NotRelevant),
                100,
                10,
                10,
            )
            .await
            .expect("malformed optional learning data cannot reject a user action");
        assert_eq!(
            store
                .get_candidate("p", "w", &candidate.candidate_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            CandidateState::Dismissed
        );
        assert!(store
            .list_dismissed_signals_for_contract("p", "w", "test-contract")
            .await
            .unwrap()
            .is_empty());
        assert!(store
            .get_embedding("p", "w", &candidate.candidate_id)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn malformed_neighbor_isolated_from_valid_dismiss_learning() {
        let store = ResurfacingStore::open_in_temp();
        let target = sample_candidate("memory", "valid-target", 1.0);
        let neighbor = sample_candidate("memory", "corrupt-neighbor", 1.0);
        store.upsert_candidate("p", "w", &target).await.unwrap();
        store.upsert_candidate("p", "w", &neighbor).await.unwrap();
        upsert_test_contract_embedding(&store, &target, &[1.0, 0.0]).await;
        {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.execute(
                "INSERT INTO resurfacing_embeddings (candidate_id, principal, workspace, content_digest, embedding_contract, vec, updated_at) VALUES (?, 'p', 'w', ?, 'test-contract', '[0.0,0.0]', 1)",
                params![neighbor.candidate_id, neighbor.content_digest],
            )
            .unwrap();
        }
        store
            .record_action_with_reason(
                "p",
                "w",
                &target.candidate_id,
                FeedbackAction::Dismiss,
                Some(DismissReason::NotRelevant),
                100,
                10,
                10,
            )
            .await
            .expect("one corrupt neighbor must not reject valid dismiss learning");
        assert_eq!(
            store
                .list_dismissed_signals_for_contract("p", "w", "test-contract")
                .await
                .unwrap(),
            vec![vec![1.0, 0.0]]
        );
        assert!(store
            .get_embedding("p", "w", &neighbor.candidate_id)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn changed_neighbor_is_not_suppressed_by_its_stale_pre_promotion_vector() {
        let store = ResurfacingStore::open_in_temp();
        let target = sample_candidate("memory", "dismiss-target", 1.0);
        let mut changed = sample_candidate("memory", "changed-neighbor", 1.0);
        store.upsert_candidate("p", "w", &target).await.unwrap();
        store.upsert_candidate("p", "w", &changed).await.unwrap();
        upsert_test_contract_embedding(&store, &target, &[1.0, 0.0]).await;
        upsert_test_contract_embedding(&store, &changed, &[0.99, 0.01]).await;

        // Model scorer's candidate-upsert -> embedding-promotion gap. The old
        // vector remains stored but no longer describes the current card.
        changed.content_digest = "changed-content-digest".to_string();
        store.upsert_candidate("p", "w", &changed).await.unwrap();
        store
            .record_action_with_reason(
                "p",
                "w",
                &target.candidate_id,
                FeedbackAction::Dismiss,
                Some(DismissReason::NotRelevant),
                100,
                10,
                10,
            )
            .await
            .unwrap();

        assert_eq!(
            store
                .get_candidate("p", "w", &changed.candidate_id)
                .await
                .unwrap()
                .unwrap()
                .salience_score,
            1.0,
            "neighbor suppression must only consider current-digest vectors"
        );
        assert_eq!(
            store
                .list_dismissed_signals_for_contract("p", "w", "test-contract")
                .await
                .unwrap(),
            vec![vec![1.0, 0.0]],
            "the valid acted card still teaches its durable signal"
        );
    }

    #[tokio::test]
    async fn dismiss_suppresses_near_neighbors_not_far() {
        let store = ResurfacingStore::open_in_temp();

        // Three live candidates at equal salience.
        let a = sample_candidate("memory", "a", 1.0);
        let b = sample_candidate("memory", "b", 1.0);
        let c = sample_candidate("memory", "c", 1.0);
        store.upsert_candidate("p", "w", &a).await.unwrap();
        store.upsert_candidate("p", "w", &b).await.unwrap();
        store.upsert_candidate("p", "w", &c).await.unwrap();

        // a & b are near-parallel (cosine ~0.99 >= threshold); c is orthogonal
        // to a (cosine 0 < threshold).
        upsert_test_contract_embedding(&store, &a, &[1.0, 0.0]).await;
        upsert_test_contract_embedding(&store, &b, &[0.9, 0.1]).await;
        upsert_test_contract_embedding(&store, &c, &[0.0, 1.0]).await;

        // Dismiss a -> its near neighbor b is down-weighted; the far c is not.
        store
            .record_action(
                "p",
                "w",
                &a.candidate_id,
                FeedbackAction::Dismiss,
                100,
                3_600,
                86_400,
            )
            .await
            .unwrap();

        let b_after = store
            .get_candidate("p", "w", &b.candidate_id)
            .await
            .unwrap()
            .unwrap();
        let c_after = store
            .get_candidate("p", "w", &c.candidate_id)
            .await
            .unwrap()
            .unwrap();

        assert!(
            (b_after.salience_score - 0.5).abs() < 1e-5,
            "near neighbor should drop by SUPPRESS_FACTOR (1.0 -> 0.5), got {}",
            b_after.salience_score
        );
        assert_eq!(
            c_after.salience_score, 1.0,
            "orthogonal candidate must be untouched"
        );
    }

    #[tokio::test]
    async fn dismissed_signal_record_list_and_cap() {
        let store = ResurfacingStore::open_in_temp();

        // Record MORE than the cap, each at a strictly-increasing `at` so the
        // newest are unambiguous. The first tag (`v[0]`) doubles as a per-row id.
        let total = DISMISSED_SIGNAL_CAP + 25;
        for i in 0..total {
            store
                .record_dismissed_signal("p", "w", &[i as f32, 0.0], i as i64)
                .await
                .unwrap();
        }

        // Exactly the cap is retained.
        let signals = store.list_dismissed_signals("p", "w").await.unwrap();
        assert_eq!(signals.len(), DISMISSED_SIGNAL_CAP);

        // Newest kept: the most recent record (highest `at`) survives; the oldest
        // (i == 0) was evicted.
        assert!(
            signals.iter().any(|v| v[0] == (total - 1) as f32),
            "the newest dismissed signal must be retained"
        );
        assert!(
            !signals.iter().any(|v| v[0] == 0.0),
            "the oldest dismissed signal must be evicted past the cap"
        );

        // Scoped by principal/workspace — a different scope sees nothing.
        assert!(store
            .list_dismissed_signals("other", "w")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn dismiss_persists_signal_that_survives_candidate_removal() {
        let store = ResurfacingStore::open_in_temp();

        // A live candidate with a stored embedding.
        let c = sample_candidate("memory", "x", 1.0);
        store.upsert_candidate("p", "w", &c).await.unwrap();
        upsert_test_contract_embedding(&store, &c, &[0.6, 0.8]).await;

        // Dismissing it captures a DURABLE snapshot of its embedding.
        store
            .record_action(
                "p",
                "w",
                &c.candidate_id,
                FeedbackAction::Dismiss,
                100,
                3_600,
                86_400,
            )
            .await
            .unwrap();

        // Simulate later candidate churn: the item is re-embedded with a totally
        // different vector (standing in for the candidate/embedding row being
        // pruned or replaced). The dismissed-signal row is an INDEPENDENT
        // snapshot in its own table, so it still returns the ORIGINAL vector.
        store
            .upsert_embedding("p", "w", &c.candidate_id, &[0.0, 0.0, 1.0])
            .await
            .unwrap();

        let signals = store.list_dismissed_signals("p", "w").await.unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(
            signals[0],
            vec![0.6, 0.8],
            "the durable dismissed signal survives candidate churn unchanged"
        );
    }

    #[tokio::test]
    async fn dismiss_without_embedding_records_no_signal() {
        let store = ResurfacingStore::open_in_temp();

        // A candidate with NO stored embedding.
        let c = sample_candidate("memory", "y", 1.0);
        store.upsert_candidate("p", "w", &c).await.unwrap();

        // Dismiss is a graceful no-op for the durable signal table.
        store
            .record_action(
                "p",
                "w",
                &c.candidate_id,
                FeedbackAction::Dismiss,
                100,
                3_600,
                86_400,
            )
            .await
            .unwrap();

        assert!(store
            .list_dismissed_signals("p", "w")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn dismiss_with_done_reason_skips_similar_penalty() {
        let store = ResurfacingStore::open_in_temp();
        let c = sample_candidate("memory", "y", 1.0);
        store.upsert_candidate("p", "w", &c).await.unwrap();
        upsert_test_contract_embedding(&store, &c, &[1.0, 0.0]).await;

        // "already_handled": this instance is done; the category is fine, so the
        // card is dismissed but similar items are NOT penalized.
        store
            .record_action_with_reason(
                "p",
                "w",
                &c.candidate_id,
                FeedbackAction::Dismiss,
                Some(DismissReason::AlreadyHandled),
                100,
                3_600,
                86_400,
            )
            .await
            .unwrap();

        assert!(
            store
                .list_dismissed_signals("p", "w")
                .await
                .unwrap()
                .is_empty(),
            "a done-reason dismiss must not write a durable penalty signal"
        );
        assert!(
            store
                .kind_engagement("p", "w")
                .await
                .unwrap()
                .iter()
                .all(|(_, pos, neg)| *pos == 0 && *neg == 0),
            "a done-reason dismiss is neutral on the lane counter"
        );
    }

    #[tokio::test]
    async fn dismiss_with_penalizing_reason_records_signal_and_negative() {
        let store = ResurfacingStore::open_in_temp();
        let c = sample_candidate("memory", "y", 1.0);
        store.upsert_candidate("p", "w", &c).await.unwrap();
        upsert_test_contract_embedding(&store, &c, &[1.0, 0.0]).await;

        store
            .record_action_with_reason(
                "p",
                "w",
                &c.candidate_id,
                FeedbackAction::Dismiss,
                Some(DismissReason::NotRelevant),
                100,
                3_600,
                86_400,
            )
            .await
            .unwrap();

        assert_eq!(
            store.list_dismissed_signals("p", "w").await.unwrap().len(),
            1,
            "not_relevant penalizes similar items (durable signal)"
        );
        let rows = store.kind_engagement("p", "w").await.unwrap();
        let mem = rows
            .iter()
            .find(|(k, _, _)| *k == SourceKind::Memory)
            .expect("memory lane present");
        assert_eq!(
            (mem.1, mem.2),
            (0, 1),
            "not_relevant is negative on the lane"
        );
    }

    #[tokio::test]
    async fn acknowledge_is_neutral_no_affinity_no_engagement() {
        let store = ResurfacingStore::open_in_temp();
        let c = sample_candidate("memory", "y", 1.0);
        store.upsert_candidate("p", "w", &c).await.unwrap();
        upsert_test_contract_embedding(&store, &c, &[1.0, 0.0]).await;

        // Acknowledge = neutral "seen": no affinity boost even though an embedding
        // exists, and no lane engagement (contrast Open, which boosts).
        store
            .record_action(
                "p",
                "w",
                &c.candidate_id,
                FeedbackAction::Acknowledge,
                100,
                3_600,
                86_400,
            )
            .await
            .unwrap();

        assert!(
            store
                .list_affinity_signals("p", "w")
                .await
                .unwrap()
                .is_empty(),
            "Acknowledge must not write an affinity (boost) signal"
        );
        assert!(
            store
                .kind_engagement("p", "w")
                .await
                .unwrap()
                .iter()
                .all(|(_, pos, neg)| *pos == 0 && *neg == 0),
            "Acknowledge is neutral on the lane counter"
        );
    }

    #[tokio::test]
    async fn affinity_signal_record_list_and_cap() {
        let store = ResurfacingStore::open_in_temp();

        // Record MORE than the cap, each at a strictly-increasing `at` so the
        // newest are unambiguous. The first tag (`v[0]`) doubles as a per-row id.
        let total = AFFINITY_SIGNAL_CAP + 25;
        for i in 0..total {
            store
                .record_affinity_signal("p", "w", &[i as f32, 0.0], i as i64)
                .await
                .unwrap();
        }

        // Exactly the cap is retained.
        let signals = store.list_affinity_signals("p", "w").await.unwrap();
        assert_eq!(signals.len(), AFFINITY_SIGNAL_CAP);

        // Newest kept: the most recent record (highest `at`) survives; the oldest
        // (i == 0) was evicted.
        assert!(
            signals.iter().any(|v| v[0] == (total - 1) as f32),
            "the newest affinity signal must be retained"
        );
        assert!(
            !signals.iter().any(|v| v[0] == 0.0),
            "the oldest affinity signal must be evicted past the cap"
        );

        // Scoped by principal/workspace — a different scope sees nothing.
        assert!(store
            .list_affinity_signals("other", "w")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn open_affinity_signal_survives_candidate_churn() {
        let store = ResurfacingStore::open_in_temp();

        // A live candidate with a stored embedding.
        let c = sample_candidate("memory", "x", 1.0);
        store.upsert_candidate("p", "w", &c).await.unwrap();
        upsert_test_contract_embedding(&store, &c, &[0.6, 0.8]).await;

        // Opening (mark-useful) it captures a DURABLE snapshot of its embedding.
        // (Acknowledge is neutral and records no affinity signal — see
        // `acknowledge_is_neutral_no_affinity_no_engagement`.)
        store
            .record_action(
                "p",
                "w",
                &c.candidate_id,
                FeedbackAction::Open,
                100,
                3_600,
                86_400,
            )
            .await
            .unwrap();

        // Simulate later candidate churn: the item is re-embedded with a totally
        // different vector. The affinity-signal row is an INDEPENDENT snapshot in
        // its own table, so it still returns the ORIGINAL vector.
        store
            .upsert_embedding("p", "w", &c.candidate_id, &[0.0, 0.0, 1.0])
            .await
            .unwrap();

        let signals = store.list_affinity_signals("p", "w").await.unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(
            signals[0],
            vec![0.6, 0.8],
            "the durable affinity signal survives candidate churn unchanged"
        );
    }

    #[tokio::test]
    async fn open_persists_affinity_signal() {
        let store = ResurfacingStore::open_in_temp();

        // A live candidate with a stored embedding.
        let c = sample_candidate("memory", "o", 1.0);
        store.upsert_candidate("p", "w", &c).await.unwrap();
        upsert_test_contract_embedding(&store, &c, &[0.8, 0.6]).await;

        // Open ALSO counts as a positive action -> records an affinity signal.
        store
            .record_action(
                "p",
                "w",
                &c.candidate_id,
                FeedbackAction::Open,
                100,
                3_600,
                86_400,
            )
            .await
            .unwrap();

        let signals = store.list_affinity_signals("p", "w").await.unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0], vec![0.8, 0.6]);
    }

    #[tokio::test]
    async fn ack_without_embedding_records_no_affinity() {
        let store = ResurfacingStore::open_in_temp();

        // A candidate with NO stored embedding.
        let c = sample_candidate("memory", "y", 1.0);
        store.upsert_candidate("p", "w", &c).await.unwrap();

        // Acknowledge is a graceful no-op for the durable affinity table.
        store
            .record_action(
                "p",
                "w",
                &c.candidate_id,
                FeedbackAction::Acknowledge,
                100,
                3_600,
                86_400,
            )
            .await
            .unwrap();

        assert!(store
            .list_affinity_signals("p", "w")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn retention_sweep_ages_out_old_affinity_signals() {
        let store = ResurfacingStore::open_in_temp();

        let now = 10_000_000_i64;
        // Older than the 30-day (2_592_000s) retention horizon used below.
        let old = now - 3_000_000;

        // One aged affinity-signal (at = old) and one fresh (at = now). Values are
        // exactly binary-representable so the round-trip equality is exact.
        store
            .record_affinity_signal("p", "w", &[0.75, 0.5], old)
            .await
            .unwrap();
        store
            .record_affinity_signal("p", "w", &[0.25, 0.5], now)
            .await
            .unwrap();

        let pruned = store
            .retention_sweep("p", "w", now, 30, 1000)
            .await
            .unwrap();
        assert!(
            pruned > 0,
            "sweep should have deleted the aged affinity signal"
        );

        // The old affinity-signal aged out; only the fresh one remains.
        let signals = store.list_affinity_signals("p", "w").await.unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0], vec![0.25, 0.5]);
    }

    #[tokio::test]
    async fn retention_sweep_prunes_old_terminal_candidates_and_orphans() {
        let store = ResurfacingStore::open_in_temp();

        let now = 10_000_000_i64;
        // Older than the 30-day (2_592_000s) retention horizon used below.
        let old = now - 3_000_000;

        // A fresh live candidate — recent, must survive (with its embedding).
        let mut fresh = sample_candidate("memory", "fresh", 0.5);
        fresh.first_seen_at = now;
        fresh.last_scored_at = now;

        // An old dismissed candidate — terminal + aged out, with an embedding,
        // phrasing, and a matching durable dismissed-signal.
        let mut old_dismissed = sample_candidate("memory", "dismissed", 0.4);
        old_dismissed.state = CandidateState::Dismissed;
        old_dismissed.first_seen_at = old;
        old_dismissed.last_scored_at = old;
        old_dismissed.last_surfaced_at = Some(old);

        // An old acted candidate — terminal + aged out.
        let mut old_acted = sample_candidate("memory", "acted", 0.3);
        old_acted.state = CandidateState::Acted;
        old_acted.first_seen_at = old;
        old_acted.last_scored_at = old;

        store.upsert_candidate("p", "w", &fresh).await.unwrap();
        store
            .upsert_candidate("p", "w", &old_dismissed)
            .await
            .unwrap();
        store.upsert_candidate("p", "w", &old_acted).await.unwrap();

        store
            .upsert_embedding("p", "w", &fresh.candidate_id, &[1.0, 0.0])
            .await
            .unwrap();
        store
            .upsert_embedding("p", "w", &old_dismissed.candidate_id, &[0.0, 1.0])
            .await
            .unwrap();
        store
            .upsert_phrasing(
                "p",
                "w",
                &old_dismissed.candidate_id,
                "line",
                "why",
                None,
                old,
            )
            .await
            .unwrap();

        // One aged dismissed-signal (at = old) and one fresh (at = now). Values
        // are exactly binary-representable so the round-trip equality is exact.
        store
            .record_dismissed_signal("p", "w", &[0.75, 0.5], old)
            .await
            .unwrap();
        store
            .record_dismissed_signal("p", "w", &[0.25, 0.5], now)
            .await
            .unwrap();

        let pruned = store
            .retention_sweep("p", "w", now, 30, 1000)
            .await
            .unwrap();
        assert!(pruned > 0, "sweep should have deleted some rows");

        // Old terminal candidates are gone.
        assert!(store
            .get_candidate("p", "w", &old_dismissed.candidate_id)
            .await
            .unwrap()
            .is_none());
        assert!(store
            .get_candidate("p", "w", &old_acted.candidate_id)
            .await
            .unwrap()
            .is_none());

        // Their side-table rows are orphan-cleaned.
        assert!(store
            .get_embedding("p", "w", &old_dismissed.candidate_id)
            .await
            .unwrap()
            .is_none());
        assert!(store
            .get_phrasing("p", "w", &old_dismissed.candidate_id)
            .await
            .unwrap()
            .is_none());

        // The fresh candidate + its embedding survive.
        assert!(store
            .get_candidate("p", "w", &fresh.candidate_id)
            .await
            .unwrap()
            .is_some());
        assert!(store
            .get_embedding("p", "w", &fresh.candidate_id)
            .await
            .unwrap()
            .is_some());

        // The old dismissed-signal aged out; only the fresh one remains.
        let signals = store.list_dismissed_signals("p", "w").await.unwrap();
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0], vec![0.25, 0.5]);
    }

    #[tokio::test]
    async fn retention_sweep_caps_scope_size() {
        let store = ResurfacingStore::open_in_temp();

        let cap = 5usize;
        let k = 3usize;
        let now = 5_000_000_i64;

        // Seed `cap + k` live candidates with strictly-ascending salience so the
        // liveness ordering (score DESC) is unambiguous. Index i (0-based) gets
        // score (i+1)/100; the k lowest-scored are the cap-prune overflow.
        let mut ids = Vec::new();
        for i in 0..(cap + k) {
            let score = (i as f32 + 1.0) / 100.0;
            let mut c = sample_candidate("memory", &format!("c{i}"), score);
            c.first_seen_at = now;
            c.last_scored_at = now;
            store.upsert_candidate("p", "w", &c).await.unwrap();
            ids.push(c.candidate_id.clone());
        }

        // A huge retention window makes age-prune a no-op; only cap-prune fires.
        let pruned = store
            .retention_sweep("p", "w", now, 100_000, cap)
            .await
            .unwrap();
        assert_eq!(pruned, k, "only the {k} overflow rows should be pruned");

        // Exactly `cap` candidates remain (all still live, cooldown elapsed).
        let remaining = store
            .list_top_candidates("p", "w", now, 1000)
            .await
            .unwrap();
        assert_eq!(remaining.len(), cap);

        // The k lowest-scored (indices 0..k) are gone; the higher-scored kept.
        for (idx, id) in ids.iter().enumerate() {
            let present = store.get_candidate("p", "w", id).await.unwrap().is_some();
            if idx < k {
                assert!(
                    !present,
                    "low-priority candidate {idx} should be capped out"
                );
            } else {
                assert!(present, "high-priority candidate {idx} should be kept");
            }
        }
    }

    #[tokio::test]
    async fn retention_sweep_keeps_live_candidates() {
        let store = ResurfacingStore::open_in_temp();

        let now = 8_000_000_i64;
        let ancient = 1_000_i64;

        // A live `candidate` and a `surfaced` row, both with ancient timestamps.
        let mut live = sample_candidate("memory", "live", 0.7);
        live.first_seen_at = ancient;
        live.last_scored_at = ancient;

        let mut surfaced = sample_candidate("memory", "surfaced", 0.6);
        surfaced.state = CandidateState::Surfaced;
        surfaced.first_seen_at = ancient;
        surfaced.last_scored_at = ancient;
        surfaced.last_surfaced_at = Some(ancient);

        store.upsert_candidate("p", "w", &live).await.unwrap();
        store.upsert_candidate("p", "w", &surfaced).await.unwrap();

        // Even with a 1-day retention window (everything is "old"), non-terminal
        // rows are never age-pruned, and a generous cap makes cap-prune a no-op.
        let pruned = store.retention_sweep("p", "w", now, 1, 1000).await.unwrap();
        assert_eq!(pruned, 0);

        assert!(store
            .get_candidate("p", "w", &live.candidate_id)
            .await
            .unwrap()
            .is_some());
        assert!(store
            .get_candidate("p", "w", &surfaced.candidate_id)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn kind_engagement_counts_positive_and_negative_by_lane() {
        let store = ResurfacingStore::open_in_temp();

        // One candidate per lane so each action has a `source_kind` to attribute.
        let mem = sample_candidate("memory", "m1", 0.5);
        let comm = sample_candidate("comm", "c1", 0.5);
        store.upsert_candidate("p", "w", &mem).await.unwrap();
        store.upsert_candidate("p", "w", &comm).await.unwrap();

        // One positive action on the memory lane (Open) plus an Acknowledge, which
        // is NEUTRAL ("seen", not a preference) and must NOT bump either counter;
        // one negative on the comm lane (Dismiss).
        store
            .record_action(
                "p",
                "w",
                &mem.candidate_id,
                FeedbackAction::Open,
                100,
                3_600,
                86_400,
            )
            .await
            .unwrap();
        store
            .record_action(
                "p",
                "w",
                &mem.candidate_id,
                FeedbackAction::Acknowledge,
                200,
                3_600,
                86_400,
            )
            .await
            .unwrap();
        store
            .record_action(
                "p",
                "w",
                &comm.candidate_id,
                FeedbackAction::Dismiss,
                300,
                3_600,
                86_400,
            )
            .await
            .unwrap();

        let rows = store.kind_engagement("p", "w").await.unwrap();
        let mem_row = rows
            .iter()
            .find(|(k, _, _)| *k == SourceKind::Memory)
            .expect("memory lane present");
        let comm_row = rows
            .iter()
            .find(|(k, _, _)| *k == SourceKind::Comm)
            .expect("comm lane present");
        assert_eq!(
            (mem_row.1, mem_row.2),
            (1, 0),
            "memory lane: 1 positive (Open); Acknowledge is neutral"
        );
        assert_eq!(
            (comm_row.1, comm_row.2),
            (0, 1),
            "comm lane: 0 positive, 1 negative"
        );

        // Scoped by principal/workspace — a different scope sees nothing.
        assert!(store
            .kind_engagement("other", "w")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn engagement_survives_candidate_pruning() {
        let store = ResurfacingStore::open_in_temp();

        let now = 10_000_000_i64;
        // Older than the 30-day (2_592_000s) retention horizon used below, so the
        // terminal candidate ages out of the sweep.
        let old = now - 3_000_000;

        // A terminal (soon-to-be dismissed), aged candidate.
        let mut c = sample_candidate("memory", "gone", 0.5);
        c.first_seen_at = old;
        c.last_scored_at = old;
        store.upsert_candidate("p", "w", &c).await.unwrap();

        // Dismiss it -> memory lane negative = 1 (the candidate row still exists
        // at action time, so the lane is attributed).
        store
            .record_action(
                "p",
                "w",
                &c.candidate_id,
                FeedbackAction::Dismiss,
                old,
                3_600,
                86_400,
            )
            .await
            .unwrap();

        // Prune the terminal aged candidate row entirely.
        let pruned = store
            .retention_sweep("p", "w", now, 30, 1000)
            .await
            .unwrap();
        assert!(
            pruned > 0,
            "sweep should have deleted the aged terminal candidate"
        );
        assert!(store
            .get_candidate("p", "w", &c.candidate_id)
            .await
            .unwrap()
            .is_none());

        // The engagement counter lives in its own table -> survives the prune.
        let rows = store.kind_engagement("p", "w").await.unwrap();
        let mem_row = rows
            .iter()
            .find(|(k, _, _)| *k == SourceKind::Memory)
            .expect("memory lane persists after candidate pruning");
        assert_eq!((mem_row.1, mem_row.2), (0, 1));
    }

    #[tokio::test]
    async fn record_run_and_cap() {
        let store = ResurfacingStore::open_in_temp();

        // Record MORE than the cap, each at a strictly-increasing `at`/`started_at`
        // so the newest are unambiguous. `started_at` doubles as a per-row id.
        let total = RUN_HISTORY_CAP + 25;
        for i in 0..total {
            store
                .record_run(
                    "p", "w", "scorer", i as i64, 1, i as i64, true, None, i as i64,
                )
                .await
                .unwrap();
        }

        let runs = store.list_runs_for_test("p", "w").await;
        // Exactly the cap is retained.
        assert_eq!(runs.len(), RUN_HISTORY_CAP);

        // Newest kept, oldest evicted (started_at doubles as the id).
        let started_ats: Vec<i64> = runs.iter().map(|r| r.1).collect();
        assert!(
            started_ats.contains(&((total - 1) as i64)),
            "the newest run must be retained"
        );
        assert!(
            !started_ats.contains(&0),
            "the oldest run must be evicted past the cap"
        );

        // Scoped by principal/workspace — a different scope sees nothing.
        assert!(store.list_runs_for_test("other", "w").await.is_empty());
    }

    #[tokio::test]
    async fn record_run_roundtrips_success_and_failure() {
        let store = ResurfacingStore::open_in_temp();

        // A successful scorer run: produced 3, no error.
        store
            .record_run("p", "w", "scorer", 1_000, 42, 3, true, None, 1_050)
            .await
            .unwrap();
        // A failed curator run: produced 0, carries an error string.
        store
            .record_run(
                "p",
                "w",
                "curator",
                2_000,
                7,
                0,
                false,
                Some("curation blew up"),
                2_010,
            )
            .await
            .unwrap();

        let runs = store.list_runs_for_test("p", "w").await;
        assert_eq!(runs.len(), 2);

        // Newest first (curator at=2_010 before scorer at=1_050).
        let curator = &runs[0];
        assert_eq!(curator.0, "curator");
        assert_eq!(curator.2, 7, "duration_ms");
        assert_eq!(curator.3, 0, "produced");
        assert!(!curator.4, "failure run has success = false");
        assert_eq!(curator.5.as_deref(), Some("curation blew up"));

        let scorer = &runs[1];
        assert_eq!(scorer.0, "scorer");
        assert_eq!(scorer.1, 1_000, "started_at");
        assert_eq!(scorer.3, 3, "produced");
        assert!(scorer.4, "success run has success = true");
        assert!(scorer.5.is_none(), "success run carries no error");
    }

    #[tokio::test]
    async fn retention_sweep_ages_out_old_runs() {
        let store = ResurfacingStore::open_in_temp();

        // `retention_sweep` receives `now` in SECONDS; the runs table stamps `at`
        // in MILLISECONDS. Use a seconds `now` and ms stamps so the unit
        // conversion (cutoff_ms) is exercised.
        let now_secs = 10_000_000_i64;
        let now_ms = now_secs * 1_000;
        // Older than the 30-day horizon (in ms): 40 days back.
        let old_ms = now_ms - 40 * 86_400_000;
        // Fresh: 1 day back — well inside the 30-day horizon.
        let fresh_ms = now_ms - 86_400_000;

        store
            .record_run("p", "w", "scorer", old_ms, 1, 1, true, None, old_ms)
            .await
            .unwrap();
        store
            .record_run("p", "w", "curator", fresh_ms, 1, 2, true, None, fresh_ms)
            .await
            .unwrap();

        let pruned = store
            .retention_sweep("p", "w", now_secs, 30, 1000)
            .await
            .unwrap();
        assert!(pruned > 0, "sweep should have deleted the aged run");

        // The old run aged out; only the fresh one remains.
        let runs = store.list_runs_for_test("p", "w").await;
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].0, "curator");
        assert_eq!(runs[0].1, fresh_ms, "the fresh run (started_at) survives");
    }

    #[tokio::test]
    async fn candidate_funnel_groups_by_state_and_lane() {
        let store = ResurfacingStore::open_in_temp();

        // Two memory `candidate`s, one memory `surfaced`, one comm `dismissed`.
        let m1 = sample_candidate("memory", "m1", 0.5);
        let m2 = sample_candidate("memory", "m2", 0.5);
        let mut m3 = sample_candidate("memory", "m3", 0.5);
        m3.state = CandidateState::Surfaced;
        let mut c1 = sample_candidate("comm", "c1", 0.5);
        c1.state = CandidateState::Dismissed;
        for c in [&m1, &m2, &m3, &c1] {
            store.upsert_candidate("p", "w", c).await.unwrap();
        }

        let funnel = store.candidate_funnel("p", "w").await.unwrap();
        let count_of = |state: CandidateState, kind: SourceKind| -> u64 {
            funnel
                .iter()
                .find(|(s, k, _)| *s == state && *k == kind)
                .map(|(_, _, n)| *n)
                .unwrap_or(0)
        };
        assert_eq!(count_of(CandidateState::Candidate, SourceKind::Memory), 2);
        assert_eq!(count_of(CandidateState::Surfaced, SourceKind::Memory), 1);
        assert_eq!(count_of(CandidateState::Dismissed, SourceKind::Comm), 1);

        // Scoped by principal/workspace — a different scope sees nothing.
        assert!(store
            .candidate_funnel("other", "w")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn table_sizes_counts_all_tables() {
        let store = ResurfacingStore::open_in_temp();

        // One row in each table for the scope.
        let c = sample_candidate("memory", "s", 0.5);
        store.upsert_candidate("p", "w", &c).await.unwrap();
        store
            .upsert_embedding("p", "w", &c.candidate_id, &[1.0, 0.0])
            .await
            .unwrap();
        store
            .upsert_embeddings_for_digests(
                "p",
                "w",
                &[CandidateEmbeddingSnapshot {
                    candidate_id: "cached".to_string(),
                    content_digest: "digest".to_string(),
                    embedding_contract: "contract".to_string(),
                    embedding: vec![0.0, 1.0],
                }],
            )
            .await
            .unwrap();
        store
            .upsert_phrasing("p", "w", &c.candidate_id, "line", "why", None, 10)
            .await
            .unwrap();
        store
            .record_dismissed_signal("p", "w", &[0.1, 0.2], 1)
            .await
            .unwrap();
        store
            .record_affinity_signal("p", "w", &[0.3, 0.4], 2)
            .await
            .unwrap();
        store
            .record_run("p", "w", "scorer", 1, 1, 1, true, None, 1)
            .await
            .unwrap();

        let sizes = store.table_sizes("p", "w").await.unwrap();
        assert_eq!(sizes.candidates, 1);
        assert_eq!(sizes.embeddings, 1);
        assert_eq!(sizes.embedding_cache, 1);
        assert_eq!(sizes.phrasing, 1);
        assert_eq!(sizes.dismissed_signals, 1);
        assert_eq!(sizes.affinity_signals, 1);
        assert_eq!(sizes.runs, 1);

        // Scoped by principal/workspace — an empty scope reports all zeros.
        let empty = store.table_sizes("other", "w").await.unwrap();
        assert_eq!(empty.candidates, 0);
        assert_eq!(empty.runs, 0);
    }

    #[tokio::test]
    async fn recent_runs_orders_newest_first_and_limits() {
        let store = ResurfacingStore::open_in_temp();

        // Three runs at strictly-increasing `at` so newest is unambiguous.
        store
            .record_run("p", "w", "scorer", 10, 5, 1, true, None, 10)
            .await
            .unwrap();
        store
            .record_run("p", "w", "curator", 20, 6, 2, true, None, 20)
            .await
            .unwrap();
        store
            .record_run("p", "w", "retention", 30, 7, 0, false, Some("boom"), 30)
            .await
            .unwrap();

        // Limit caps the result; newest comes first.
        let runs = store.recent_runs("p", "w", 2).await.unwrap();
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].kind, "retention");
        assert!(!runs[0].success);
        assert_eq!(runs[0].error.as_deref(), Some("boom"));
        assert_eq!(runs[1].kind, "curator");

        // A larger limit returns everything, still newest-first.
        let all = store.recent_runs("p", "w", 50).await.unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].kind, "retention");
        assert_eq!(all[2].kind, "scorer");
    }

    #[tokio::test]
    async fn run_aggregates_summarize_success_failure_and_last_error() {
        let store = ResurfacingStore::open_in_temp();

        // scorer: two successes + one failure (the failure is the most recent).
        store
            .record_run("p", "w", "scorer", 10, 4, 1, true, None, 10)
            .await
            .unwrap();
        store
            .record_run("p", "w", "scorer", 20, 6, 3, true, None, 20)
            .await
            .unwrap();
        store
            .record_run(
                "p",
                "w",
                "scorer",
                30,
                8,
                0,
                false,
                Some("scorer failed"),
                30,
            )
            .await
            .unwrap();
        // curator: a single success, no failures -> last_error stays None.
        store
            .record_run("p", "w", "curator", 40, 2, 5, true, None, 40)
            .await
            .unwrap();

        let aggs = store.run_aggregates("p", "w").await.unwrap();

        let scorer = aggs
            .iter()
            .find(|a| a.kind == "scorer")
            .expect("scorer agg");
        assert_eq!(scorer.total, 3);
        assert_eq!(scorer.successes, 2);
        assert_eq!(scorer.failures, 1);
        assert_eq!(scorer.total_produced, 4, "1 + 3 + 0");
        assert_eq!(scorer.last_started_at, 30);
        assert_eq!(scorer.last_error.as_deref(), Some("scorer failed"));
        assert!(
            (scorer.avg_duration_ms - 6.0).abs() < 1e-9,
            "(4 + 6 + 8) / 3 == 6.0, got {}",
            scorer.avg_duration_ms
        );

        let curator = aggs
            .iter()
            .find(|a| a.kind == "curator")
            .expect("curator agg");
        assert_eq!(curator.total, 1);
        assert_eq!(curator.successes, 1);
        assert_eq!(curator.failures, 0);
        assert!(curator.last_error.is_none());

        // Scoped by principal/workspace — a different scope sees nothing.
        assert!(store.run_aggregates("other", "w").await.unwrap().is_empty());
    }

    // ---- Store-correctness review fixes -----------------------------------

    /// Fix 2: a conflicting `upsert_candidate` over an EXISTING dismissed row
    /// updates only the scoring/content columns and PRESERVES the lifecycle
    /// (state/cooldown/dismiss_count/first_seen_at), so a stale concurrent
    /// scorer write can't resurrect a just-dismissed candidate.
    #[tokio::test]
    async fn upsert_on_conflict_preserves_lifecycle_columns() {
        let store = ResurfacingStore::open_in_temp();

        // Seed, then dismiss so the row carries terminal lifecycle state.
        let mut c = sample_candidate("memory", "k", 0.4);
        c.first_seen_at = 111;
        c.content_revision = Some("1".to_string());
        store.upsert_candidate("p", "w", &c).await.unwrap();
        store
            .upsert_phrasing(
                "p",
                "w",
                &c.candidate_id,
                "old line",
                "old why",
                c.content_revision.as_deref(),
                100,
            )
            .await
            .unwrap();
        store
            .record_action(
                "p",
                "w",
                &c.candidate_id,
                FeedbackAction::Dismiss,
                500,
                3_600,
                86_400,
            )
            .await
            .unwrap();

        let dismissed = store
            .get_candidate("p", "w", &c.candidate_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(dismissed.state, CandidateState::Dismissed);
        assert_eq!(dismissed.dismiss_count, 1);
        let cooldown = dismissed.cooldown_until;
        assert_eq!(cooldown, 500 + 3_600);

        // A concurrent scorer pass re-upserts the SAME id with a STALE pre-dismiss
        // snapshot (state=candidate, cooldown 0, dismiss_count 0, a different
        // first_seen_at) but a fresh higher score.
        let mut restale = sample_candidate("memory", "k", 0.9);
        restale.title = "revision two".to_string();
        restale.content_revision = Some("2".to_string());
        restale.first_seen_at = 999;
        restale.state = CandidateState::Candidate;
        restale.cooldown_until = 0;
        restale.dismiss_count = 0;
        store.upsert_candidate("p", "w", &restale).await.unwrap();

        let after = store
            .get_candidate("p", "w", &c.candidate_id)
            .await
            .unwrap()
            .unwrap();
        // Scoring/content updated...
        assert!((after.salience_score - 0.9).abs() < 1e-6, "score updates");
        // ...but the lifecycle is preserved (NOT resurrected).
        assert_eq!(
            after.state,
            CandidateState::Dismissed,
            "concurrent upsert must not resurrect a dismissed row"
        );
        assert_eq!(after.cooldown_until, cooldown, "cooldown preserved");
        assert_eq!(after.dismiss_count, 1, "dismiss_count preserved");
        assert_eq!(after.first_seen_at, 111, "first_seen_at preserved");
        assert_eq!(after.content_revision.as_deref(), Some("2"));
        assert_eq!(after.title, "revision two");
        assert!(
            store
                .get_phrasing("p", "w", &c.candidate_id)
                .await
                .unwrap()
                .is_none(),
            "phrasing from revision one must be ignored after a content refresh"
        );
        store
            .upsert_phrasing(
                "p",
                "w",
                &c.candidate_id,
                "revision two line",
                "current why",
                Some("2"),
                600,
            )
            .await
            .unwrap();
        store
            .upsert_phrasing(
                "p",
                "w",
                &c.candidate_id,
                "late revision one line",
                "stale why",
                Some("1"),
                700,
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .get_phrasing("p", "w", &c.candidate_id)
                .await
                .unwrap()
                .unwrap()
                .0,
            "revision two line",
            "late stale curator output must not overwrite current phrasing"
        );

        let mut out_of_order = sample_candidate("memory", "k", 1.0);
        out_of_order.title = "stale revision one".to_string();
        out_of_order.content_revision = Some("1".to_string());
        let stale_applied = store
            .upsert_candidate("p", "w", &out_of_order)
            .await
            .unwrap();
        assert!(!stale_applied);
        let after_stale = store
            .get_candidate("p", "w", &c.candidate_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after_stale.content_revision.as_deref(), Some("2"));
        assert_eq!(after_stale.title, "revision two");
    }

    /// Fix 1: the same `candidate_id` (same kind+source_ref) can be held by two
    /// DIFFERENT scopes without a PRIMARY KEY collision clobbering either — the
    /// PK is now `(principal, workspace, candidate_id)`.
    #[tokio::test]
    async fn same_candidate_id_isolated_across_scopes() {
        let store = ResurfacingStore::open_in_temp();

        // Identical source_ref -> identical candidate_id, two distinct scopes.
        let a = sample_candidate("memory", "user.knowledge#name", 0.2);
        let mut b = sample_candidate("memory", "user.knowledge#name", 0.8);
        b.title = "scope-b-title".to_string();
        assert_eq!(a.candidate_id, b.candidate_id, "same id by construction");

        store
            .upsert_candidate("alice", "default", &a)
            .await
            .unwrap();
        store.upsert_candidate("bob", "default", &b).await.unwrap();

        // Both coexist with their own values (no cross-scope clobber).
        let got_a = store
            .get_candidate("alice", "default", &a.candidate_id)
            .await
            .unwrap()
            .unwrap();
        let got_b = store
            .get_candidate("bob", "default", &b.candidate_id)
            .await
            .unwrap()
            .unwrap();
        assert!((got_a.salience_score - 0.2).abs() < 1e-6);
        assert!((got_b.salience_score - 0.8).abs() < 1e-6);
        assert_eq!(got_a.title, "title-user.knowledge#name");
        assert_eq!(got_b.title, "scope-b-title");
    }

    /// Fix 3: when over cap, cap-prune keeps a live `surfaced` row over a
    /// terminal `acted` row of equal salience (the liveness predicate now
    /// protects all non-terminal states, not just raw `candidate`).
    #[tokio::test]
    async fn cap_prune_keeps_surfaced_over_acted() {
        let store = ResurfacingStore::open_in_temp();
        let now = 5_000_000_i64;

        // Equal salience so ONLY the liveness predicate decides the survivor.
        let mut surfaced = sample_candidate("memory", "surfaced", 0.5);
        surfaced.state = CandidateState::Surfaced;
        surfaced.first_seen_at = now;
        surfaced.last_scored_at = now;
        surfaced.last_surfaced_at = Some(now);

        let mut acted = sample_candidate("memory", "acted", 0.5);
        acted.state = CandidateState::Acted;
        acted.first_seen_at = now;
        acted.last_scored_at = now;

        store.upsert_candidate("p", "w", &surfaced).await.unwrap();
        store.upsert_candidate("p", "w", &acted).await.unwrap();

        // Huge retention window -> age-prune is a no-op; only cap-prune fires.
        let pruned = store
            .retention_sweep("p", "w", now, 100_000, 1)
            .await
            .unwrap();
        assert_eq!(pruned, 1, "exactly the overflow row is pruned");

        assert!(
            store
                .get_candidate("p", "w", &surfaced.candidate_id)
                .await
                .unwrap()
                .is_some(),
            "the live surfaced row must survive cap-prune"
        );
        assert!(
            store
                .get_candidate("p", "w", &acted.candidate_id)
                .await
                .unwrap()
                .is_none(),
            "the terminal acted row is the one pruned"
        );
    }

    /// Fix 4: `mark_surfaced` is guarded by `state='candidate'`, so a row the
    /// owner dismissed between the curator's pick and this call is NOT flipped
    /// back to `surfaced`.
    #[tokio::test]
    async fn mark_surfaced_skips_dismissed_row() {
        let store = ResurfacingStore::open_in_temp();

        let live = sample_candidate("memory", "live", 0.7);
        let dismissed = sample_candidate("memory", "dismissed", 0.6);
        store.upsert_candidate("p", "w", &live).await.unwrap();
        store.upsert_candidate("p", "w", &dismissed).await.unwrap();
        store
            .record_action(
                "p",
                "w",
                &dismissed.candidate_id,
                FeedbackAction::Dismiss,
                50,
                3_600,
                86_400,
            )
            .await
            .unwrap();

        // The curator marks BOTH surfaced. The guard must skip the dismissed one.
        let surfaced = store
            .mark_surfaced(
                "p",
                "w",
                &[live.candidate_id.clone(), dismissed.candidate_id.clone()],
                100,
            )
            .await
            .unwrap();
        assert_eq!(surfaced, vec![live.candidate_id.clone()]);

        let live_after = store
            .get_candidate("p", "w", &live.candidate_id)
            .await
            .unwrap()
            .unwrap();
        let dismissed_after = store
            .get_candidate("p", "w", &dismissed.candidate_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            live_after.state,
            CandidateState::Surfaced,
            "a live candidate is surfaced normally"
        );
        assert_eq!(live_after.surface_count, 1);
        assert_eq!(
            dismissed_after.state,
            CandidateState::Dismissed,
            "a concurrently-dismissed row is NOT flipped back to surfaced"
        );
        assert_eq!(
            dismissed_after.surface_count, 0,
            "the guarded UPDATE was a no-op for the dismissed row"
        );
    }

    /// Fix 8: `record_action` on an id absent from the scope appends NO feedback
    /// row (a foreign/guessed id can't grow the log unbounded), while a real
    /// in-scope candidate does log one; and `retention_sweep` age-prunes old
    /// feedback rows (seconds domain).
    #[tokio::test]
    async fn record_action_gates_feedback_and_retention_ages_it_out() {
        let store = ResurfacingStore::open_in_temp();

        // (a) An action on a nonexistent id writes NO feedback row.
        store
            .record_action(
                "p",
                "w",
                "totally-made-up-id",
                FeedbackAction::Dismiss,
                100,
                3_600,
                86_400,
            )
            .await
            .unwrap();
        assert_eq!(
            store.count_feedback_for_test("p", "w").await,
            0,
            "no feedback row for a nonexistent candidate"
        );

        // A real in-scope candidate DOES log feedback.
        let c = sample_candidate("memory", "real", 0.5);
        store.upsert_candidate("p", "w", &c).await.unwrap();
        store
            .record_action(
                "p",
                "w",
                &c.candidate_id,
                FeedbackAction::Dismiss,
                100,
                3_600,
                86_400,
            )
            .await
            .unwrap();
        assert_eq!(
            store.count_feedback_for_test("p", "w").await,
            1,
            "a real in-scope candidate logs one feedback row"
        );

        // (b) Retention age-prunes old feedback (at=100 << the seconds cutoff).
        let now = 10_000_000_i64;
        store
            .retention_sweep("p", "w", now, 30, 1000)
            .await
            .unwrap();
        assert_eq!(
            store.count_feedback_for_test("p", "w").await,
            0,
            "old feedback rows age out of the log"
        );
    }

    #[tokio::test]
    async fn contextual_action_claim_replays_and_conflicting_payload_is_rejected() {
        let store = ResurfacingStore::open_in_temp();
        let mut candidate = sample_candidate("comm", "message-1", 0.8);
        candidate.state = CandidateState::Surfaced;
        candidate.content_revision = Some("7".to_string());
        store.upsert_candidate("p", "w", &candidate).await.unwrap();

        let stale = store
            .begin_contextual_action_claim(
                "p",
                "w",
                &candidate.candidate_id,
                "stale-key",
                "create_task",
                "stale-hash",
                Some("6"),
                "task_resurfacing_stale",
                99,
                300,
                TARGET_KIND_RESURFACING_CANDIDATE,
            )
            .await
            .unwrap();
        assert!(matches!(
            stale,
            ResurfacingActionClaimBegin::StaleRevision {
                current_revision: Some(revision)
            } if revision == "7"
        ));

        let first = store
            .begin_contextual_action_claim(
                "p",
                "w",
                &candidate.candidate_id,
                "key-1",
                "create_task",
                "hash-a",
                Some("7"),
                "task_resurfacing_1",
                100,
                300,
                TARGET_KIND_RESURFACING_CANDIDATE,
            )
            .await
            .unwrap();
        assert!(matches!(first, ResurfacingActionClaimBegin::Claimed { .. }));
        let result = serde_json::json!({
            "kind": "task",
            "task_id": "task_resurfacing_1",
            "route": "/tasks?selected=task_resurfacing_1"
        });
        store
            .complete_contextual_action_claim(
                "p",
                "w",
                &candidate.candidate_id,
                "key-1",
                "create_task",
                "hash-a",
                Some("7"),
                100,
                "task_resurfacing_1",
                &result,
                true,
                101,
                86_400,
            )
            .await
            .unwrap();

        let replay = store
            .lookup_contextual_action_claim(
                "p",
                "w",
                &candidate.candidate_id,
                "key-1",
                "create_task",
                "hash-a",
                101,
                300,
            )
            .await
            .unwrap();
        assert!(matches!(
            replay,
            ResurfacingActionClaimLookup::Completed(ResurfacingStoredActionResult {
                result_ref,
                ..
            }) if result_ref == "task_resurfacing_1"
        ));
        assert_eq!(
            store
                .get_candidate("p", "w", &candidate.candidate_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            CandidateState::Acted
        );
        assert_eq!(store.count_feedback_for_test("p", "w").await, 1);

        let conflict = store
            .lookup_contextual_action_claim(
                "p",
                "w",
                &candidate.candidate_id,
                "key-1",
                "create_task",
                "different-hash",
                101,
                300,
            )
            .await
            .unwrap();
        assert_eq!(conflict, ResurfacingActionClaimLookup::Conflict);
    }

    #[tokio::test]
    async fn failed_contextual_action_is_retryable_without_positive_feedback() {
        let store = ResurfacingStore::open_in_temp();
        let mut candidate = sample_candidate("comm", "message-2", 0.8);
        candidate.state = CandidateState::Surfaced;
        store.upsert_candidate("p", "w", &candidate).await.unwrap();
        store
            .begin_contextual_action_claim(
                "p",
                "w",
                &candidate.candidate_id,
                "key-2",
                "save_to_memory",
                "hash-b",
                None,
                "lc_resurfacing_2",
                100,
                300,
                TARGET_KIND_RESURFACING_CANDIDATE,
            )
            .await
            .unwrap();
        store
            .fail_contextual_action_claim(
                "p",
                "w",
                &candidate.candidate_id,
                "key-2",
                "save_to_memory",
                "hash-b",
                None,
                100,
                "downstream_write",
                101,
            )
            .await
            .unwrap();
        assert_eq!(store.count_feedback_for_test("p", "w").await, 0);
        assert_eq!(
            store
                .get_candidate("p", "w", &candidate.candidate_id)
                .await
                .unwrap()
                .unwrap()
                .state,
            CandidateState::Surfaced
        );
        assert_eq!(
            store
                .lookup_contextual_action_claim(
                    "p",
                    "w",
                    &candidate.candidate_id,
                    "key-2",
                    "save_to_memory",
                    "hash-b",
                    102,
                    300,
                )
                .await
                .unwrap(),
            ResurfacingActionClaimLookup::Retryable
        );
        let retry = store
            .begin_contextual_action_claim(
                "p",
                "w",
                &candidate.candidate_id,
                "key-2",
                "save_to_memory",
                "hash-b",
                None,
                "lc_resurfacing_2",
                102,
                300,
                TARGET_KIND_RESURFACING_CANDIDATE,
            )
            .await
            .unwrap();
        assert!(matches!(retry, ResurfacingActionClaimBegin::Claimed { .. }));
    }

    #[tokio::test]
    async fn stale_started_claim_is_retryable_at_boundary_and_counts_retry_attempts() {
        let store = ResurfacingStore::open_in_temp();
        let mut candidate = sample_candidate("comm", "message-crash", 0.8);
        candidate.state = CandidateState::Surfaced;
        candidate.content_revision = Some("4".to_string());
        store.upsert_candidate("p", "w", &candidate).await.unwrap();
        store
            .begin_contextual_action_claim(
                "p",
                "w",
                &candidate.candidate_id,
                "crashed-key",
                "create_task",
                "crashed-hash",
                Some("4"),
                "task_resurfacing_crash",
                100,
                300,
                TARGET_KIND_RESURFACING_CANDIDATE,
            )
            .await
            .unwrap();

        assert_eq!(
            store
                .lookup_contextual_action_claim(
                    "p",
                    "w",
                    &candidate.candidate_id,
                    "crashed-key",
                    "create_task",
                    "crashed-hash",
                    399,
                    300,
                )
                .await
                .unwrap(),
            ResurfacingActionClaimLookup::InProgress
        );
        assert_eq!(
            store
                .lookup_contextual_action_claim(
                    "p",
                    "w",
                    &candidate.candidate_id,
                    "crashed-key",
                    "create_task",
                    "crashed-hash",
                    400,
                    300,
                )
                .await
                .unwrap(),
            ResurfacingActionClaimLookup::Retryable
        );
        assert!(matches!(
            store
                .begin_contextual_action_claim(
                    "p",
                    "w",
                    &candidate.candidate_id,
                    "crashed-key",
                    "create_task",
                    "crashed-hash",
                    Some("4"),
                    "task_resurfacing_crash",
                    400,
                    300,
                    TARGET_KIND_RESURFACING_CANDIDATE,
                )
                .await
                .unwrap(),
            ResurfacingActionClaimBegin::Claimed { .. }
        ));

        store
            .fail_contextual_action_claim(
                "p",
                "w",
                &candidate.candidate_id,
                "crashed-key",
                "create_task",
                "crashed-hash",
                Some("4"),
                100,
                "expired_worker",
                401,
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .lookup_contextual_action_claim(
                    "p",
                    "w",
                    &candidate.candidate_id,
                    "crashed-key",
                    "create_task",
                    "crashed-hash",
                    401,
                    300,
                )
                .await
                .unwrap(),
            ResurfacingActionClaimLookup::InProgress,
            "an expired worker must not fail the reclaimed attempt"
        );
        let superseded = store
            .complete_contextual_action_claim(
                "p",
                "w",
                &candidate.candidate_id,
                "crashed-key",
                "create_task",
                "crashed-hash",
                Some("4"),
                100,
                "task_resurfacing_crash",
                &serde_json::json!({"kind": "task", "task_id": "task_resurfacing_crash"}),
                true,
                401,
                86_400,
            )
            .await
            .unwrap_err();
        assert!(superseded.to_string().contains("superseded"));

        store
            .fail_contextual_action_claim(
                "p",
                "w",
                &candidate.candidate_id,
                "crashed-key",
                "create_task",
                "crashed-hash",
                Some("4"),
                400,
                "current_worker",
                402,
            )
            .await
            .unwrap();

        let events = store.action_event_aggregates("p", "w").await.unwrap();
        assert!(events.iter().any(|event| {
            event.action_kind == "create_task" && event.event_type == "started" && event.count == 2
        }));
        assert!(events.iter().any(|event| {
            event.action_kind == "create_task"
                && event.event_type == "failed"
                && event.error_class.as_deref() == Some("current_worker")
                && event.count == 1
        }));
        assert!(!events.iter().any(|event| {
            event.event_type == "failed" && event.error_class.as_deref() == Some("expired_worker")
        }));
    }

    #[tokio::test]
    async fn completion_only_marks_acted_for_the_claimed_revision() {
        let store = ResurfacingStore::open_in_temp();
        let mut candidate = sample_candidate("comm", "message-refreshed", 0.8);
        candidate.state = CandidateState::Surfaced;
        candidate.content_revision = Some("7".to_string());
        store.upsert_candidate("p", "w", &candidate).await.unwrap();
        store
            .begin_contextual_action_claim(
                "p",
                "w",
                &candidate.candidate_id,
                "refresh-key",
                "create_task",
                "refresh-hash",
                Some("7"),
                "task_resurfacing_refresh",
                100,
                300,
                TARGET_KIND_RESURFACING_CANDIDATE,
            )
            .await
            .unwrap();

        candidate.content_revision = Some("8".to_string());
        candidate.content_digest = "refreshed evidence".to_string();
        store.upsert_candidate("p", "w", &candidate).await.unwrap();
        store
            .complete_contextual_action_claim(
                "p",
                "w",
                &candidate.candidate_id,
                "refresh-key",
                "create_task",
                "refresh-hash",
                Some("7"),
                100,
                "task_resurfacing_refresh",
                &serde_json::json!({"kind": "task", "task_id": "task_resurfacing_refresh"}),
                true,
                101,
                86_400,
            )
            .await
            .unwrap();

        let refreshed = store
            .get_candidate("p", "w", &candidate.candidate_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(refreshed.content_revision.as_deref(), Some("8"));
        assert_eq!(refreshed.state, CandidateState::Surfaced);
        assert_eq!(store.count_feedback_for_test("p", "w").await, 0);
    }

    /// Narrowing `memory_tiers` governs ingestion, so without this the lane
    /// keeps whatever the previous policy surfaced until it ages out. The
    /// retraction is the repair half, and it must move only the cards whose
    /// tier is no longer allowed.
    #[tokio::test]
    async fn retraction_removes_only_memory_cards_whose_tier_is_no_longer_allowed() {
        let store = ResurfacingStore::open_in_temp();
        let allowed: Vec<String> = ["knowledge", "preferences"]
            .into_iter()
            .map(str::to_string)
            .collect();

        let surfaced = |source_ref: &str| {
            let mut candidate = sample_candidate("memory", source_ref, 0.9);
            candidate.state = CandidateState::Surfaced;
            candidate
        };
        for source_ref in [
            "workflows#gtm_gate",
            "organization#roster",
            "knowledge#a_fact",
            "preferences#tone",
        ] {
            store
                .upsert_candidate("p", "w", &surfaced(source_ref))
                .await
                .unwrap();
        }
        // A comm card sharing a denied prefix must be untouched: this repairs
        // the memory source's allowlist, not every corpus.
        let mut comm = sample_candidate("comm", "workflows#not-a-memory-card", 0.9);
        comm.state = CandidateState::Surfaced;
        store.upsert_candidate("p", "w", &comm).await.unwrap();

        // apply=false reports the same cohort without moving anything.
        let preview = store
            .retract_ineligible_memory_candidates("p", "w", &allowed, 50, false)
            .await
            .unwrap();
        assert_eq!(preview, 2);
        assert_eq!(surfaced_refs(&store).await.len(), 5);

        let retracted = store
            .retract_ineligible_memory_candidates("p", "w", &allowed, 50, true)
            .await
            .unwrap();
        assert_eq!(retracted, 2);
        assert_eq!(
            surfaced_refs(&store).await,
            vec![
                "knowledge#a_fact".to_string(),
                "preferences#tone".to_string(),
                "workflows#not-a-memory-card".to_string(),
            ]
        );

        // Idempotent: a second pass finds nothing left to retract.
        assert_eq!(
            store
                .retract_ineligible_memory_candidates("p", "w", &allowed, 50, true)
                .await
                .unwrap(),
            0
        );

        // Policy retracted these, not the owner, so no dismissal was recorded.
        let dismissals: i64 = {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.query_row(
                "SELECT COALESCE(SUM(dismiss_count), 0) FROM resurfacing_candidates",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(dismissals, 0);

        // An empty allowlist is fail-closed, matching the source's own reading.
        assert_eq!(
            store
                .retract_ineligible_memory_candidates("p", "w", &[], 50, true)
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            surfaced_refs(&store).await,
            vec!["workflows#not-a-memory-card".to_string()]
        );
    }

    async fn surfaced_refs(store: &ResurfacingStore) -> Vec<String> {
        let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
        let mut statement = conn
            .prepare(
                "SELECT source_ref FROM resurfacing_candidates \
                 WHERE state = 'surfaced' ORDER BY source_ref",
            )
            .unwrap();
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        rows
    }

    /// Batched recommendation reads answer exactly what the per-card loop
    /// answered, revision guard included: a recommendation written against an
    /// older `content_revision` is invisible to both forms.
    #[tokio::test]
    async fn recommendation_batch_matches_a_loop_of_single_lookups() {
        let store = ResurfacingStore::open_in_temp();
        let mut current = sample_candidate("comm", "message-current", 0.8);
        current.state = CandidateState::Surfaced;
        current.content_revision = Some("9".to_string());
        store.upsert_candidate("p", "w", &current).await.unwrap();

        let mut moved_on = sample_candidate("comm", "message-moved-on", 0.7);
        moved_on.state = CandidateState::Surfaced;
        moved_on.content_revision = Some("9".to_string());
        store.upsert_candidate("p", "w", &moved_on).await.unwrap();

        let mut bare = sample_candidate("comm", "message-bare", 0.6);
        bare.state = CandidateState::Surfaced;
        bare.content_revision = Some("9".to_string());
        store.upsert_candidate("p", "w", &bare).await.unwrap();

        let recommendation = ResurfacingRecommendation {
            kind: ResurfacingActionKind::CreateTask,
            label: "Create a task".to_string(),
            rationale: "This may need follow-up work.".to_string(),
            confidence: 0.9,
            content_revision: Some("9".to_string()),
            source: ResurfacingRecommendationSource::Curator,
        };
        store
            .upsert_recommendation("p", "w", &current.candidate_id, &recommendation, 100)
            .await
            .unwrap();
        // Same row, then the candidate's content moves on underneath it.
        store
            .upsert_recommendation("p", "w", &moved_on.candidate_id, &recommendation, 100)
            .await
            .unwrap();
        moved_on.content_revision = Some("10".to_string());
        store.upsert_candidate("p", "w", &moved_on).await.unwrap();

        let ids = vec![
            current.candidate_id.clone(),
            moved_on.candidate_id.clone(),
            bare.candidate_id.clone(),
        ];
        let batch = store
            .get_recommendation_batch("p", "w", &ids)
            .await
            .unwrap();
        for id in &ids {
            assert_eq!(
                batch.get(id).cloned(),
                store.get_recommendation("p", "w", id).await.unwrap(),
                "batch disagreed with the single lookup for {id}"
            );
        }
        assert_eq!(batch.len(), 1);
        assert!(batch.contains_key(&current.candidate_id));
        assert!(store
            .get_recommendation_batch("other", "w", &ids)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn recommendation_events_dedupe_shown_and_attribute_action_completion() {
        let store = ResurfacingStore::open_in_temp();
        let mut candidate = sample_candidate("comm", "message-recommended", 0.8);
        candidate.state = CandidateState::Surfaced;
        candidate.content_revision = Some("9".to_string());
        store.upsert_candidate("p", "w", &candidate).await.unwrap();
        let recommendation = ResurfacingRecommendation {
            kind: ResurfacingActionKind::CreateTask,
            label: "Create a task".to_string(),
            rationale: "This may need follow-up work.".to_string(),
            confidence: 0.9,
            content_revision: Some("9".to_string()),
            source: ResurfacingRecommendationSource::Curator,
        };
        store
            .upsert_recommendation("p", "w", &candidate.candidate_id, &recommendation, 100)
            .await
            .unwrap();
        assert_eq!(
            store
                .record_recommendation_interaction(
                    "p",
                    "w",
                    &candidate.candidate_id,
                    ResurfacingActionKind::CreateTask,
                    Some("9"),
                    "selected",
                    100,
                )
                .await
                .unwrap(),
            ResurfacingRecommendationInteractionResult::NotShown
        );
        assert!(store
            .record_recommendation_shown("p", "w", &candidate.candidate_id, &recommendation, 101,)
            .await
            .unwrap());
        assert_eq!(
            store
                .record_recommendation_interaction(
                    "p",
                    "w",
                    &candidate.candidate_id,
                    ResurfacingActionKind::CreateTask,
                    Some("9"),
                    "completed",
                    102,
                )
                .await
                .unwrap(),
            ResurfacingRecommendationInteractionResult::OutOfOrder
        );
        assert_eq!(
            store
                .record_recommendation_interaction(
                    "p",
                    "w",
                    &candidate.candidate_id,
                    ResurfacingActionKind::CreateTask,
                    Some("9"),
                    "selected",
                    102,
                )
                .await
                .unwrap(),
            ResurfacingRecommendationInteractionResult::Recorded
        );
        assert_eq!(
            store
                .record_recommendation_interaction(
                    "p",
                    "w",
                    &candidate.candidate_id,
                    ResurfacingActionKind::CreateTask,
                    Some("9"),
                    "selected",
                    102,
                )
                .await
                .unwrap(),
            ResurfacingRecommendationInteractionResult::Duplicate
        );
        assert!(!store
            .record_recommendation_shown("p", "w", &candidate.candidate_id, &recommendation, 102,)
            .await
            .unwrap());
        assert!(matches!(
            store
                .begin_contextual_action_claim(
                    "p",
                    "w",
                    &candidate.candidate_id,
                    "recommended-key",
                    "create_task",
                    "recommended-hash",
                    Some("9"),
                    "task_resurfacing_recommended",
                    103,
                    300,
                    TARGET_KIND_RESURFACING_CANDIDATE,
                )
                .await
                .unwrap(),
            ResurfacingActionClaimBegin::Claimed { .. }
        ));
        store
            .complete_contextual_action_claim(
                "p",
                "w",
                &candidate.candidate_id,
                "recommended-key",
                "create_task",
                "recommended-hash",
                Some("9"),
                103,
                "task_resurfacing_recommended",
                &serde_json::json!({"kind": "task", "task_id": "task-1"}),
                false,
                104,
                86_400,
            )
            .await
            .unwrap();

        let events = store
            .recommendation_event_aggregates("p", "w")
            .await
            .unwrap();
        assert!(events.iter().any(|event| {
            event.recommendation_kind == "create_task"
                && event.event_type == "recommended"
                && event.count == 1
        }));
        assert!(events.iter().any(|event| {
            event.recommendation_kind == "create_task"
                && event.event_type == "selected"
                && event.count == 1
        }));
        assert!(events.iter().any(|event| {
            event.recommendation_kind == "create_task"
                && event.event_type == "completed"
                && event.count == 1
        }));
    }

    #[tokio::test]
    async fn active_routing_repair_is_idempotent_per_content_revision() {
        let store = ResurfacingStore::open_in_temp();
        let mut candidate = sample_candidate("comm", "gmail/acct/thread/message@1000", 0.8);
        candidate.state = CandidateState::Surfaced;
        candidate.last_surfaced_at = Some(2_000);
        candidate.content_revision = Some("1".to_string());
        store.upsert_candidate("p", "w", &candidate).await.unwrap();

        assert_eq!(
            store.brief_coverage("p", "w").await.unwrap(),
            ResurfacingBriefCoverage {
                comm_total: 1,
                comm_surfaced: 1,
                with_brief: 0,
                legacy: 1,
                complete: 0,
                partial: 0,
                source_omits_details: 0,
            }
        );

        assert_eq!(
            store
                .list_active_repair_candidates("p", "w", 10)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(store
            .complete_active_repair(
                "p",
                "w",
                &candidate.candidate_id,
                Some("1"),
                "Routed(WorthALook)",
                false,
                0,
                2_001,
            )
            .await
            .unwrap());
        assert!(store
            .list_active_repair_candidates("p", "w", 10)
            .await
            .unwrap()
            .is_empty());

        candidate.content_revision = Some("2".to_string());
        candidate.last_scored_at = 3_000;
        store.upsert_candidate("p", "w", &candidate).await.unwrap();
        assert_eq!(
            store
                .list_active_repair_candidates("p", "w", 10)
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(store
            .complete_active_repair(
                "p",
                "w",
                &candidate.candidate_id,
                Some("2"),
                "Routed(FollowUp)",
                true,
                86_400,
                3_001,
            )
            .await
            .unwrap());
        let stored = store
            .get_candidate("p", "w", &candidate.candidate_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.state, CandidateState::Candidate);
        assert_eq!(stored.cooldown_until, 86_400);
    }
}

/// The lib-side seam: content-source observation upserts through this trait
/// so the magician lib never names the concrete store.
#[async_trait::async_trait]
impl crate::magician_v2::resurfacing_seam::ResurfacingSink for ResurfacingStore {
    async fn upsert_candidate(
        &self,
        principal: &str,
        workspace: &str,
        candidate: &crate::magician_v2::resurfacing_seam::Candidate,
    ) -> anyhow::Result<()> {
        ResurfacingStore::upsert_candidate(self, principal, workspace, candidate)
            .await
            .map(|_| ())
    }

    async fn get_candidate(
        &self,
        principal: &str,
        workspace: &str,
        candidate_id: &str,
    ) -> anyhow::Result<Option<crate::magician_v2::resurfacing_seam::Candidate>> {
        ResurfacingStore::get_candidate(self, principal, workspace, candidate_id)
            .await
            .map_err(anyhow::Error::from)
    }
}
