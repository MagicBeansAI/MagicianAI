//! Durable scoped outcomes, embeddings, and Slice-1 rank scores.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{File, OpenOptions},
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Condvar, Mutex, OnceLock,
    },
    time::Instant,
};

use anyhow::{Context, Result};
use fs2::FileExt;
use rusqlite::{
    params, params_from_iter, Connection, OpenFlags, OptionalExtension, TransactionBehavior,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::magician_v2::json_traversal::{
    json_bytes_nesting_is_bounded, json_bytes_nodes_are_bounded, MAX_RETAINED_JSON_DEPTH,
};

use super::{
    actionability::{
        ActionabilityExplanation, ActionabilityInference, ActionabilityModelSnapshot,
        ATTENTION_TEMPORAL_FEATURE_CONTRACT,
    },
    bandit::{
        extract_bandit_features, AttentionBanditAttributionQuality, AttentionBanditPolicySnapshot,
        AttentionBanditPosteriorState, AttentionPosteriorUpdateReceipt,
        AttentionPosteriorUpdateStatus,
    },
    delivery::{
        AttentionDeliveryLedgerHealth, AttentionDeliveryPage, AttentionDeliveryReadError,
        AttentionDeliveryRefreshReason, AttentionDeliveryStatus, CreateAttentionDelivery,
        FrozenAttentionDelivery, FrozenAttentionDeliveryItem,
    },
    grouping::{
        canonical_pair, AttentionPairCandidateRef, AttentionPairLabelKind,
        AttentionPairLabelSource, AttentionPairModelSnapshot, PersistedPairEvidence,
        RecordAttentionPairLabel,
    },
    rank_recompute::{
        AttentionRankRecomputeJob, AttentionRankRecomputeQueueCounts, AttentionRankRecomputeResult,
        AttentionRankRecomputeStatus, ScheduleAttentionRankRecompute,
        ATTENTION_RANK_RECOMPUTE_REASON_MAX_CHARS, ATTENTION_RANK_RECOMPUTE_SCHEMA_VERSION,
        ATTENTION_RANK_RECOMPUTE_WRONGLY_STALED_REASON,
    },
    routing::{
        AttentionDecision, AttentionDecisionContext, AttentionDecisionDetail,
        AttentionDecisionItem, AttentionImpressionError, AttentionImpressionReceipt,
        AttentionRoute, AttentionRoutingEvaluation, AttentionRoutingHealth,
        AttentionRoutingPolicySnapshot, RecordAttentionImpression,
        ATTENTION_CANDIDATE_ID_MAX_CHARS, ATTENTION_CLIENT_TYPE_MAX_CHARS,
        ATTENTION_CLIENT_VERSION_MAX_CHARS, ATTENTION_DECISION_ID_MAX_CHARS,
        ATTENTION_EVENT_ID_MAX_CHARS, ATTENTION_IMPRESSION_SCHEMA_VERSION,
        ATTENTION_MIN_VISIBLE_MS_MAX, ATTENTION_ROUTING_DECISION_SCHEMA_VERSION,
        ATTENTION_SOURCE_REVISION_MAX_CHARS, ATTENTION_VIEWPORT_CLASS_MAX_CHARS,
        ATTENTION_VISIBILITY_RULE_MAX_CHARS, ATTENTION_VISIBLE_MS_MAX,
    },
    AttentionOutcomeKind, AttentionSurface, BayesianKnnEstimate, BayesianKnnLabel,
    RecordAttentionOutcome, SemanticAttentionCandidate, SemanticEmbedding,
    ATTENTION_OUTCOME_SCHEMA_VERSION,
};

mod history_prune;
mod queue_recovery;

type CandidateEmbeddingSnapshot = Arc<HashMap<String, PersistedCandidateEmbedding>>;

/// Parsed candidate embeddings per `(principal, workspace, surface)`, held with
/// the `(row count, max updated_at)` they were parsed from. Any insert, update,
/// or delete moves one of those two, so a stale snapshot can never be served.
#[allow(clippy::type_complexity)]
static CANDIDATE_EMBEDDING_CACHE: OnceLock<
    Mutex<HashMap<(String, String, &'static str), (i64, i64, CandidateEmbeddingSnapshot)>>,
> = OnceLock::new();

/// Schema bootstrap is single-owner inside one process. SQLite still provides
/// the cross-process writer lock; this guard prevents two service constructors
/// in the same boot from interleaving the small versioned migration steps.
static ATTENTION_SCHEMA_BOOTSTRAP_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

const ATTENTION_READ_POOL_SIZE: usize = 4;
const ATTENTION_SQLITE_BUSY_TIMEOUT_MS: u64 = 5_000;
const ATTENTION_READ_POOL_WAIT_TIMEOUT_MS: u64 = 8_000;
const ATTENTION_RECONCILIATION_SQL_TIMEOUT_MS: u64 = 5_000;
const ATTENTION_RECONCILIATION_PROGRESS_INTERVAL_OPS: i32 = 10_000;
const ATTENTION_MAINTENANCE_DRAIN_TIMEOUT_MS: u64 = 120_000;
const ATTENTION_PROCESS_DRAIN_TIMEOUT_MS: u64 = 120_000;
const NORMALIZED_CANONICAL_PROJECTION_SCHEMA_VERSION: i64 = 2;
const CURRENT_CANONICAL_PROJECTION_SCHEMA_VERSION: i64 = 3;
const MAX_NORMALIZED_PROJECTION_ITEMS: usize = 10_000;
const MAX_ATTENTION_STORED_JSON_BYTES: usize = 64 * 1024 * 1024;
const MAX_ATTENTION_STORED_JSON_NODES: usize = 1_000_000;
const MAX_ATTENTION_FEATURE_VECTOR_BYTES: usize = 64 * 1024;
const LEGACY_CONTINUOUS_TEMPORAL_FEATURE_CONTRACT: &str = "attention_temporal_continuous_v0";
const LEGACY_FEATURE_MIGRATION_KIND: &str = "feature_snapshot_v1";
const LEGACY_CANONICAL_MIGRATION_KIND: &str = "canonical_projection_v1";

const RANK_RECOMPUTE_RECONCILIATION_SELECT_SQL: &str =
    "SELECT o.outcome_id, o.surface, o.candidate_id, o.source_revision, o.outcome, \
            CASE WHEN di.decision_id IS NOT NULL OR ci.decision_id IS NOT NULL \
                      OR i.impression_id IS NOT NULL \
                THEN u.decision_id ELSE NULL END, \
            COALESCE(dp.delivery_id, i.delivery_id), \
            i.impression_id, \
            o.projection_id, \
            u.affected_rank_before, u.snapshot_id, u.posterior_version_after \
     FROM attention_outcomes o \
     LEFT JOIN attention_bandit_updates u ON u.outcome_id = o.outcome_id \
     LEFT JOIN attention_delivery_decisions dd ON dd.decision_id = u.decision_id \
        AND dd.principal = o.principal AND dd.workspace = o.workspace \
        AND dd.lane = o.surface \
     LEFT JOIN attention_delivery_decision_items di ON di.decision_id = dd.decision_id \
        AND (di.candidate_id = o.candidate_id \
            OR di.candidate_id = o.surface || ':' || o.candidate_id) \
        AND di.source_revision IS o.source_revision \
     LEFT JOIN attention_delivery_page_items dpi ON dpi.decision_id = di.decision_id \
        AND dpi.position = di.position \
     LEFT JOIN attention_delivery_pages dp ON dp.delivery_id = dpi.delivery_id \
     LEFT JOIN attention_decision_items ci ON ci.principal = o.principal \
        AND ci.workspace = o.workspace AND ci.decision_id = u.decision_id \
        AND (ci.candidate_id = o.candidate_id \
            OR ci.candidate_id = o.surface || ':' || o.candidate_id) \
        AND ci.source_revision IS o.source_revision AND ci.served_route = o.surface \
        AND ci.served_rank > 0 \
     LEFT JOIN attention_impressions i ON i.impression_id = u.impression_id \
        AND i.principal = o.principal AND i.workspace = o.workspace \
        AND i.decision_id = u.decision_id \
        AND (i.candidate_id = o.candidate_id \
            OR i.candidate_id = o.surface || ':' || o.candidate_id) \
        AND i.source_revision IS o.source_revision AND i.surface = o.surface \
        AND i.verified = 1 \
        AND (dp.delivery_id IS NULL OR i.delivery_id = dp.delivery_id) \
     LEFT JOIN attention_rank_recompute_jobs j ON j.principal = o.principal \
        AND j.workspace = o.workspace AND j.outcome_id = o.outcome_id \
     WHERE o.principal = ? AND o.workspace = ? AND j.outcome_id IS NULL \
     ORDER BY o.created_at, o.outcome_id LIMIT ?";

/// Row counts removed by one `prune_canonical_history` pass. Zeroed when the
/// scope sits under the keep bound and nothing was eligible.
#[derive(Debug, Default, Clone, Copy)]
pub struct AttentionHistoryPruneReport {
    pub projections_pruned: u64,
    pub decisions_pruned: u64,
    pub decision_items_pruned: u64,
    pub item_revisions_pruned: u64,
    pub diagnostics_pruned: u64,
}

impl AttentionHistoryPruneReport {
    pub fn pruned_anything(&self) -> bool {
        self.projections_pruned > 0
            || self.decisions_pruned > 0
            || self.decision_items_pruned > 0
            || self.item_revisions_pruned > 0
            || self.diagnostics_pruned > 0
    }
}

/// Last wall-clock time the automatic scoped retention pass ran, per scope.
/// Retention and delivery compaction are operator-invocable by design; this
/// gate adds a best-effort automatic floor (once per scope per
/// [`AUTO_SCOPED_RETENTION_INTERVAL`]) driven by the same persist path that
/// grows the store, so a scope that is actively writing cannot accumulate
/// indefinitely even if no operator ever runs the maintenance command.
static AUTO_SCOPED_RETENTION_LAST_RUN: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<(String, String), AutoRetentionRun>>,
> = std::sync::OnceLock::new();

/// What the gate remembers about a scope's latest automatic pass. A pass is
/// in flight until `finished_at_ms` is set; `empty_passes` counts consecutive
/// passes that retired nothing, which is what stretches the oversized cadence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AutoRetentionRun {
    started_at_ms: i64,
    finished_at_ms: Option<i64>,
    empty_passes: u32,
}

/// The writer share an oversized store's maintenance may take: a productive
/// pass that held the writer for `t` is followed by at least `t` times this
/// factor of quiet, so the page reads that share the writer keep at least
/// four fifths of it. Measured before this bound: passes of 575 s re-arming
/// after 60 s left every 20 s poll serving degraded.
const AUTO_SCOPED_RETENTION_WRITER_SHARE_FACTOR: i64 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoRetentionGate {
    Run,
    Skip,
}

/// Decide whether an automatic pass may start now. Never while the previous
/// one is still running: the gate used to stamp the START of a pass, so a
/// pass that held the writer for ten minutes was followed by another the
/// moment it finished, back to back, for as long as the file stayed over the
/// guard. The cadence is measured from the previous FINISH. Over the guard it
/// doubles for every consecutive pass that retired nothing, capped at the
/// ordinary daily cadence — a pass that cannot shrink the file must not hold
/// the writer once a minute for no gain — and never sooner than
/// [`AUTO_SCOPED_RETENTION_WRITER_SHARE_FACTOR`] times the previous pass's
/// own writer hold. A healthy file keeps the daily cadence regardless of
/// history.
fn auto_retention_gate(
    previous: Option<&AutoRetentionRun>,
    now_ms: i64,
    oversized: bool,
) -> AutoRetentionGate {
    let Some(previous) = previous else {
        return AutoRetentionGate::Run;
    };
    let Some(finished_at_ms) = previous.finished_at_ms else {
        return AutoRetentionGate::Skip;
    };
    let interval_ms = if oversized {
        let backoff = 1_i64 << previous.empty_passes.min(20);
        let held_ms = finished_at_ms.saturating_sub(previous.started_at_ms).max(0);
        AUTO_SCOPED_RETENTION_OVERSIZED_INTERVAL_MS
            .saturating_mul(backoff)
            .max(held_ms.saturating_mul(AUTO_SCOPED_RETENTION_WRITER_SHARE_FACTOR))
            .min(AUTO_SCOPED_RETENTION_INTERVAL_MS)
    } else {
        AUTO_SCOPED_RETENTION_INTERVAL_MS
    };
    if now_ms.saturating_sub(finished_at_ms) < interval_ms {
        AutoRetentionGate::Skip
    } else {
        AutoRetentionGate::Run
    }
}

const AUTO_SCOPED_RETENTION_INTERVAL_MS: i64 = 24 * 60 * 60 * 1000;

/// Auto-retention only runs on a healthy, reclaimed database. The retention
/// pass holds the store's single writer for one transaction; on an oversized
/// unreclaimed file that transaction takes minutes, stalling every persisting
/// request behind it — and with the in-memory gate re-arming on each restart
/// a restart loop would re-trigger the stall endlessly. Above this size the
/// pass narrows to one slice and runs on the faster
/// [`AUTO_SCOPED_RETENTION_OVERSIZED_INTERVAL_MS`] cadence so the file drains
/// back under the guard; the operator reclaim command returns the freed pages.
const AUTO_SCOPED_RETENTION_MAX_DB_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// How much history one automatic pass may retire while the database is over
/// [`AUTO_SCOPED_RETENTION_MAX_DB_BYTES`]. Every retention delete is bounded
/// by `<= cutoff` with no lower bound, so moving the cutoff forward one slice
/// at a time from the oldest surviving row bounds the work — and therefore the
/// writer hold — without changing a single statement.
const AUTO_SCOPED_RETENTION_OVERSIZED_SLICE_MS: i64 = 24 * 60 * 60 * 1000;
/// How often an automatic pass may run while the database is over
/// [`AUTO_SCOPED_RETENTION_MAX_DB_BYTES`]. One slice per ordinary interval
/// retires a day of history a day — exactly the rate history accrues — so the
/// backlog freezes instead of draining and the file never returns under the
/// guard. Slices are bounded work, so the way out is to run them more often,
/// not to make one bigger: an oversized store drains at a slice a minute and
/// falls back to the ordinary cadence the moment it is back under the guard.
const AUTO_SCOPED_RETENTION_OVERSIZED_INTERVAL_MS: i64 = 60 * 1000;

/// Key under which an applied retention report carries the number of rows
/// the pass actually deleted, as opposed to the per-table eligibility counts.
pub const RETENTION_RETIRED_ROWS_KEY: &str = "retired_rows";

/// A rank-recompute job is history the moment its outcome is: a terminal job
/// retires when it completed inside the retention window OR when the outcome
/// it recomputed for did. Keying the delete on `completed_at` alone let a job
/// that finished a week later pin an outcome older than the cutoff, and that
/// outcome pinned its decision items, and they pinned the decision — the
/// oldest surviving row never moved, so a narrowed pass over the guard
/// retired nothing, ten minutes of writer hold at a time. Binds `?1` = cutoff.
const RETIRABLE_RECOMPUTE_JOB_SQL: &str = "j.status IN ('succeeded', 'stale', 'dead') \
     AND (j.completed_at <= ?1 OR EXISTS (SELECT 1 FROM attention_outcomes o \
         WHERE o.outcome_id = j.outcome_id AND o.occurred_at <= ?1))";

/// An outcome retires with the window it occurred in unless a live bandit
/// update, embedding bind work, or a job that is not yet terminal still
/// needs it. Terminal jobs are retired first by [`RETIRABLE_RECOMPUTE_JOB_SQL`].
/// Binds `?1` = cutoff.
const RETIRABLE_OUTCOME_SQL: &str = "o.occurred_at <= ?1 \
     AND NOT EXISTS (SELECT 1 FROM attention_bandit_updates u WHERE u.outcome_id = o.outcome_id) \
     AND NOT EXISTS (SELECT 1 FROM attention_embedding_bind_work w WHERE w.outcome_id = o.outcome_id) \
     AND NOT EXISTS (SELECT 1 FROM attention_rank_recompute_jobs j \
         WHERE j.outcome_id = o.outcome_id AND j.status NOT IN ('succeeded', 'stale', 'dead'))";

/// A decision retires with its items when nothing newer than the cutoff
/// refers to it: no impression, no outcome that survives this pass, no job
/// that survives this pass. Binds `?1` = cutoff.
const RETIRABLE_DECISION_SQL: &str = "d.decided_at <= ?1 \
     AND NOT EXISTS (SELECT 1 FROM attention_impressions i \
         WHERE i.decision_id = d.decision_id AND i.last_recorded_at > ?1) \
     AND NOT EXISTS (SELECT 1 FROM attention_outcomes o \
         WHERE o.decision_id = d.decision_id AND NOT (o.occurred_at <= ?1 \
             AND NOT EXISTS (SELECT 1 FROM attention_bandit_updates u WHERE u.outcome_id = o.outcome_id) \
             AND NOT EXISTS (SELECT 1 FROM attention_embedding_bind_work w WHERE w.outcome_id = o.outcome_id) \
             AND NOT EXISTS (SELECT 1 FROM attention_rank_recompute_jobs j \
                 WHERE j.outcome_id = o.outcome_id AND j.status NOT IN ('succeeded', 'stale', 'dead')))) \
     AND NOT EXISTS (SELECT 1 FROM attention_rank_recompute_jobs j \
         WHERE j.decision_id = d.decision_id AND NOT (j.status IN ('succeeded', 'stale', 'dead') \
             AND (j.completed_at <= ?1 OR EXISTS (SELECT 1 FROM attention_outcomes o \
                 WHERE o.outcome_id = j.outcome_id AND o.occurred_at <= ?1))))";

impl From<rusqlite::Error> for AttentionDeliveryReadError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error.into())
    }
}

impl From<serde_json::Error> for AttentionDeliveryReadError {
    fn from(error: serde_json::Error) -> Self {
        Self::Storage(error.into())
    }
}

const BOOTSTRAP_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS attention_outcomes (
    outcome_id TEXT NOT NULL PRIMARY KEY,
    schema_version INTEGER NOT NULL,
    event_id TEXT NOT NULL,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    surface TEXT NOT NULL,
    candidate_id TEXT NOT NULL,
    source_revision TEXT,
    outcome TEXT NOT NULL,
    reason TEXT,
    label_quality TEXT NOT NULL,
    embedding_contract TEXT,
    occurred_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    decision_id TEXT,
    impression_id TEXT,
    delivery_id TEXT,
    projection_id TEXT,
    UNIQUE(principal, workspace, event_id)
);
CREATE INDEX IF NOT EXISTS attention_outcomes_scope_time_idx
    ON attention_outcomes(principal, workspace, occurred_at DESC, outcome_id DESC);
CREATE INDEX IF NOT EXISTS attention_outcomes_scope_candidate_idx
    ON attention_outcomes(principal, workspace, surface, candidate_id);

-- Durable, bounded private semantic snapshots for outcomes whose embedding could
-- not be resolved during the request. The outcome and repair row are committed
-- in one transaction; a background pass removes the row only after the vector
-- and outcome binding are both durable.
CREATE TABLE IF NOT EXISTS attention_embedding_bind_work (
    outcome_id TEXT NOT NULL PRIMARY KEY,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    surface TEXT NOT NULL CHECK(surface IN ('follow_up', 'worth_a_look')),
    candidate_id TEXT NOT NULL,
    source_revision TEXT,
    semantic_text TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    attempts INTEGER NOT NULL DEFAULT 0,
    next_retry_at INTEGER,
    lease_owner TEXT,
    lease_expires_at INTEGER,
    last_error_code TEXT,
    last_attempt_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY(outcome_id) REFERENCES attention_outcomes(outcome_id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS attention_embedding_bind_work_order_idx
    ON attention_embedding_bind_work(created_at, outcome_id);

-- Content-free, exactly-once post-feedback rank diagnostics. The canonical
-- outcome is committed first; UNIQUE(principal, workspace, outcome_id) makes
-- scoped reconciliation after a fail-soft enqueue safe and idempotent.
CREATE TABLE IF NOT EXISTS attention_rank_recompute_jobs (
    job_id TEXT NOT NULL PRIMARY KEY,
    schema_version INTEGER NOT NULL,
    outcome_id TEXT NOT NULL,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    origin_surface TEXT NOT NULL,
    canonical_candidate_id TEXT NOT NULL,
    raw_candidate_id TEXT NOT NULL,
    source_revision TEXT,
    outcome TEXT NOT NULL,
    decision_id TEXT,
    delivery_id TEXT,
    impression_id TEXT,
    projection_id TEXT,
    affected_rank_before INTEGER,
    enqueue_policy_snapshot_id TEXT,
    enqueue_posterior_version INTEGER,
    status TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_retry_at INTEGER,
    lease_owner TEXT,
    lease_expires_at INTEGER,
    reason TEXT,
    result_json TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    completed_at INTEGER,
    UNIQUE(principal, workspace, outcome_id),
    FOREIGN KEY(outcome_id) REFERENCES attention_outcomes(outcome_id)
);
CREATE INDEX IF NOT EXISTS attention_rank_recompute_scope_queue_idx
    ON attention_rank_recompute_jobs(principal, workspace, status, next_retry_at, created_at);

CREATE TABLE IF NOT EXISTS attention_candidate_embeddings (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    surface TEXT NOT NULL,
    candidate_id TEXT NOT NULL,
    source_revision TEXT,
    content_digest TEXT NOT NULL,
    embedding_contract TEXT NOT NULL,
    vec_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY(principal, workspace, surface, candidate_id)
);
CREATE INDEX IF NOT EXISTS attention_candidate_embeddings_contract_idx
    ON attention_candidate_embeddings(principal, workspace, embedding_contract);

-- The feature vector a candidate had when it was served, keyed by the exact
-- revision the owner saw. Without this there is no training set at all: acting
-- on an item is what produces a label, and acting on it is also what retires
-- the source row its semantics live in, so features cannot be recovered after
-- the fact. Revision is part of the key because a re-distilled thread is a
-- different observation, not an update to the old one.
CREATE TABLE IF NOT EXISTS attention_candidate_feature_snapshots (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    surface TEXT NOT NULL,
    candidate_id TEXT NOT NULL,
    source_revision TEXT NOT NULL,
    feature_contract TEXT NOT NULL,
    semantic_extractor_contract TEXT NOT NULL,
    semantic_prompt_version TEXT,
    features_json TEXT NOT NULL,
    content_digest TEXT NOT NULL DEFAULT '',
    first_served_at INTEGER NOT NULL,
    -- Keyed by content, not by revision. `source_revision` binds the
    -- classifier's INPUT (a message revision), never its output: re-running the
    -- classifier over an unchanged message can yield a different label, reason
    -- and summary, and the thread-state join moves subject and received_at
    -- whenever a newer message lands. Measured live, 10 of 464 candidate/
    -- revision pairs served more than one payload within a single hour, one of
    -- them twelve. Keying on the revision alone therefore pins the FIRST vector
    -- seen and silently attributes every later label to it.
    PRIMARY KEY(principal, workspace, surface, candidate_id, source_revision, content_digest)
);
CREATE INDEX IF NOT EXISTS attention_candidate_feature_snapshots_scope_idx
    ON attention_candidate_feature_snapshots(principal, workspace, surface, first_served_at);
-- A label is matched to the variant that was live when the owner acted, so the
-- lookup is by candidate and time rather than by exact content.
CREATE INDEX IF NOT EXISTS attention_candidate_feature_snapshots_lookup_idx
    ON attention_candidate_feature_snapshots(
        principal, workspace, surface, candidate_id, first_served_at);

-- Deterministically unreadable legacy rows must not monopolize the head of a
-- bounded migration forever. Quarantine stores only identity, a payload hash,
-- and a bounded reason code; it never copies the private legacy body. The
-- source row remains intact for explicit repair or forensic recovery.
CREATE TABLE IF NOT EXISTS attention_legacy_migration_quarantine (
    migration_kind TEXT NOT NULL,
    source_key TEXT NOT NULL,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    source_fingerprint TEXT NOT NULL,
    error_code TEXT NOT NULL,
    payload_bytes INTEGER NOT NULL,
    quarantined_at INTEGER NOT NULL,
    PRIMARY KEY(migration_kind, source_key)
);
CREATE INDEX IF NOT EXISTS attention_legacy_migration_quarantine_scope_idx
    ON attention_legacy_migration_quarantine(principal, workspace, migration_kind);

CREATE TABLE IF NOT EXISTS attention_candidate_scores (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    surface TEXT NOT NULL,
    candidate_id TEXT NOT NULL,
    source_revision TEXT,
    embedding_contract TEXT NOT NULL,
    usefulness_probability REAL NOT NULL,
    usefulness_weight REAL NOT NULL,
    actionability_probability REAL NOT NULL,
    actionability_weight REAL NOT NULL,
    surface_score REAL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY(principal, workspace, surface, candidate_id)
);
CREATE INDEX IF NOT EXISTS attention_candidate_scores_scope_score_idx
    ON attention_candidate_scores(principal, workspace, surface, surface_score DESC);

CREATE TABLE IF NOT EXISTS attention_rank_generations (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    surface TEXT NOT NULL,
    generation INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY(principal, workspace, surface)
);

-- One immutable startup cutoff and monotonic cursor per legacy evidence
-- source. Import event ids are deterministic as a second idempotency layer;
-- the cutoff prevents the migration from tailing events already dual-written
-- by the live handlers.
CREATE TABLE IF NOT EXISTS attention_historical_bootstrap_checkpoints (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    source TEXT NOT NULL,
    cutoff_at INTEGER NOT NULL,
    cursor_at INTEGER NOT NULL DEFAULT 0,
    cursor_id TEXT NOT NULL DEFAULT '',
    imported_count INTEGER NOT NULL DEFAULT 0,
    skipped_count INTEGER NOT NULL DEFAULT 0,
    completed INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY(principal, workspace, source)
);

-- Content-free durable semantic extraction control plane. Safe briefs stay in
-- their source stores and are fetched only after a bounded lease is acquired.
CREATE TABLE IF NOT EXISTS attention_semantic_extraction_work (
    work_id TEXT NOT NULL PRIMARY KEY,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    surface TEXT NOT NULL,
    candidate_id TEXT NOT NULL,
    source_revision TEXT NOT NULL,
    source_revision_number INTEGER NOT NULL,
    semantic_schema_version INTEGER NOT NULL,
    extractor_contract TEXT NOT NULL,
    prompt_version TEXT NOT NULL,
    model TEXT,
    profile TEXT,
    status TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_retry_at INTEGER,
    lease_owner TEXT,
    lease_expires_at INTEGER,
    last_error_code TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE(principal, workspace, surface, candidate_id)
);
CREATE INDEX IF NOT EXISTS attention_semantic_work_due_idx
    ON attention_semantic_extraction_work(status, next_retry_at, lease_expires_at, updated_at);
CREATE INDEX IF NOT EXISTS attention_semantic_work_scope_idx
    ON attention_semantic_extraction_work(principal, workspace, surface, status);

CREATE TABLE IF NOT EXISTS attention_semantic_extraction_checkpoints (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    surface TEXT NOT NULL,
    cursor TEXT,
    lease_owner TEXT,
    lease_expires_at INTEGER,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY(principal, workspace, surface)
);

CREATE TABLE IF NOT EXISTS attention_actionability_model_snapshots (
    snapshot_id TEXT NOT NULL PRIMARY KEY,
    snapshot_json TEXT NOT NULL,
    content_digest TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS attention_actionability_scores (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    surface TEXT NOT NULL,
    candidate_id TEXT NOT NULL,
    source_revision TEXT,
    snapshot_id TEXT NOT NULL,
    model_version TEXT NOT NULL,
    probability REAL NOT NULL,
    explanation_code TEXT NOT NULL,
    explanation_label TEXT NOT NULL,
    input_digest TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY(principal, workspace, surface, candidate_id, snapshot_id)
);
CREATE INDEX IF NOT EXISTS attention_actionability_scores_scope_idx
    ON attention_actionability_scores(principal, workspace, surface, snapshot_id, probability DESC);

CREATE TABLE IF NOT EXISTS attention_pair_labels (
    pair_label_id TEXT NOT NULL PRIMARY KEY,
    schema_version INTEGER NOT NULL,
    event_id TEXT NOT NULL,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    surface TEXT NOT NULL,
    left_candidate_id TEXT NOT NULL,
    left_source_revision TEXT,
    right_candidate_id TEXT NOT NULL,
    right_source_revision TEXT,
    label TEXT NOT NULL,
    source TEXT NOT NULL,
    label_quality TEXT NOT NULL,
    confidence REAL NOT NULL,
    occurred_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE(principal, workspace, event_id)
);
CREATE INDEX IF NOT EXISTS attention_pair_labels_scope_pair_idx
    ON attention_pair_labels(principal, workspace, surface, left_candidate_id, right_candidate_id);

CREATE TABLE IF NOT EXISTS attention_pair_model_snapshots (
    snapshot_id TEXT NOT NULL PRIMARY KEY,
    snapshot_json TEXT NOT NULL,
    content_digest TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS attention_group_generations (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    surface TEXT NOT NULL,
    generation INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY(principal, workspace, surface)
);

CREATE TABLE IF NOT EXISTS attention_routing_policy_snapshots (
    snapshot_id TEXT NOT NULL PRIMARY KEY,
    snapshot_json TEXT NOT NULL,
    content_digest TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS attention_bandit_policy_snapshots (
    snapshot_id TEXT NOT NULL PRIMARY KEY,
    snapshot_json TEXT NOT NULL,
    content_digest TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS attention_bandit_posteriors (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    surface TEXT NOT NULL,
    snapshot_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    update_count INTEGER NOT NULL,
    state_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY(principal, workspace, surface, snapshot_id),
    FOREIGN KEY(snapshot_id) REFERENCES attention_bandit_policy_snapshots(snapshot_id)
);

CREATE TABLE IF NOT EXISTS attention_decisions (
    decision_id TEXT NOT NULL PRIMARY KEY,
    schema_version INTEGER NOT NULL,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    surface TEXT NOT NULL,
    decided_at INTEGER NOT NULL,
    policy_mode TEXT NOT NULL,
    policy_snapshot_id TEXT,
    policy_model_version TEXT,
    candidate_set_digest TEXT NOT NULL,
    eligible_item_count INTEGER NOT NULL,
    selected_item_count INTEGER NOT NULL,
    returned_item_count INTEGER NOT NULL,
    complete_universe_recorded INTEGER NOT NULL,
    complete_cross_lane_universe INTEGER NOT NULL,
    context_json TEXT NOT NULL,
    policy_seed_identity TEXT NOT NULL,
    canary_assigned INTEGER NOT NULL,
    baseline_route_summary_json TEXT NOT NULL,
    learned_route_summary_json TEXT NOT NULL,
    latency_ms INTEGER NOT NULL,
    degradation_reason TEXT,
    bandit_health_json TEXT,
    decision_json TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS attention_decisions_scope_time_idx
    ON attention_decisions(principal, workspace, decided_at DESC, decision_id DESC);

-- One immutable, origin-qualified materialization for a complete Follow-up +
-- Worth-a-look universe, policy identity, and owner scope. Projection JSON is
-- bounded by scoped retention and prevents each legacy lane read from
-- independently re-evaluating the same cross-lane universe.
CREATE TABLE IF NOT EXISTS attention_canonical_projections (
    projection_id TEXT NOT NULL PRIMARY KEY,
    schema_version INTEGER NOT NULL,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    universe_digest TEXT NOT NULL,
    policy_identity TEXT NOT NULL,
    projection_json TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE(principal, workspace, universe_digest, policy_identity)
);
CREATE INDEX IF NOT EXISTS attention_canonical_projections_scope_time_idx
    ON attention_canonical_projections(principal, workspace, created_at DESC, projection_id DESC);

-- V2+ canonical projections keep immutable item bodies once and store only
-- ordered membership plus compact per-projection rank/routing bindings. The
-- V1 table above remains the identity/compatibility anchor: V1 rows retain
-- `projection_json`; V2 rows use an empty payload and materialize through the
-- normalized tables at the API edge.
CREATE TABLE IF NOT EXISTS attention_canonical_item_revisions (
    item_digest TEXT NOT NULL PRIMARY KEY,
    canonical_id TEXT NOT NULL,
    source_revision TEXT,
    item_json TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS attention_canonical_item_revisions_identity_idx
    ON attention_canonical_item_revisions(canonical_id, source_revision, item_digest);

CREATE TABLE IF NOT EXISTS attention_canonical_diagnostic_revisions (
    diagnostic_digest TEXT NOT NULL PRIMARY KEY,
    diagnostic_kind TEXT NOT NULL CHECK(diagnostic_kind IN ('rank', 'decision')),
    diagnostic_json TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS attention_canonical_projection_manifests (
    projection_id TEXT NOT NULL PRIMARY KEY,
    schema_version INTEGER NOT NULL,
    metadata_json TEXT NOT NULL,
    diagnostics_json TEXT,
    item_count INTEGER NOT NULL,
    metadata_bytes INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    last_materialized_at INTEGER NOT NULL,
    FOREIGN KEY(projection_id) REFERENCES attention_canonical_projections(projection_id)
        ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS attention_canonical_projection_members (
    projection_id TEXT NOT NULL,
    lane TEXT NOT NULL CHECK(lane IN ('follow_up', 'worth_a_look', 'non_surfaced')),
    position INTEGER NOT NULL,
    canonical_id TEXT NOT NULL,
    item_digest TEXT NOT NULL,
    item_binding_json TEXT,
    rank_position INTEGER,
    rank_digest TEXT,
    rank_json TEXT,
    decision_position INTEGER,
    decision_digest TEXT,
    decision_item_json TEXT,
    PRIMARY KEY(projection_id, lane, position),
    UNIQUE(projection_id, canonical_id),
    FOREIGN KEY(projection_id) REFERENCES attention_canonical_projection_manifests(projection_id)
        ON DELETE CASCADE,
    FOREIGN KEY(item_digest) REFERENCES attention_canonical_item_revisions(item_digest)
);
CREATE INDEX IF NOT EXISTS attention_canonical_projection_members_page_idx
    ON attention_canonical_projection_members(projection_id, lane, position);

-- Feature payloads are content-addressed globally; candidate/revision rows are
-- small attribution bindings. The legacy snapshot table remains readable and
-- is migrated in bounded batches rather than rebuilt during startup.
CREATE TABLE IF NOT EXISTS attention_feature_vectors (
    content_digest TEXT NOT NULL PRIMARY KEY,
    feature_contract TEXT NOT NULL,
    temporal_contract TEXT NOT NULL,
    semantic_schema_version INTEGER NOT NULL,
    semantic_extractor_contract TEXT NOT NULL,
    semantic_prompt_version TEXT,
    semantic_model TEXT,
    semantic_profile TEXT,
    features_json TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS attention_candidate_feature_bindings (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    surface TEXT NOT NULL,
    candidate_id TEXT NOT NULL,
    source_revision TEXT NOT NULL,
    content_digest TEXT NOT NULL,
    first_served_at INTEGER NOT NULL,
    last_served_at INTEGER NOT NULL,
    PRIMARY KEY(principal, workspace, surface, candidate_id, source_revision, content_digest),
    FOREIGN KEY(content_digest) REFERENCES attention_feature_vectors(content_digest)
);
CREATE INDEX IF NOT EXISTS attention_candidate_feature_bindings_scope_idx
    ON attention_candidate_feature_bindings(
        principal, workspace, surface, candidate_id, first_served_at);

CREATE TABLE IF NOT EXISTS attention_decision_items (
    decision_id TEXT NOT NULL,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    candidate_id TEXT NOT NULL,
    source_revision TEXT,
    source_family TEXT NOT NULL,
    hard_eligible INTEGER NOT NULL,
    ineligibility_reason TEXT,
    baseline_route TEXT NOT NULL,
    learned_route TEXT NOT NULL,
    served_route TEXT NOT NULL,
    routing_mode TEXT NOT NULL,
    routing_snapshot_id TEXT,
    routing_model_version TEXT,
    learned_route_confidence REAL,
    utility_margin REAL,
    route_reason TEXT NOT NULL,
    route_applied INTEGER NOT NULL,
    canary_assigned INTEGER NOT NULL,
    owner_action_required_probability REAL,
    information_value_probability REAL,
    follow_up_utility REAL,
    worth_a_look_utility REAL,
    uncertainty REAL,
    cluster_id TEXT NOT NULL,
    cluster_size INTEGER NOT NULL,
    representative INTEGER NOT NULL,
    baseline_rank INTEGER NOT NULL,
    learned_rank INTEGER NOT NULL,
    served_rank INTEGER NOT NULL DEFAULT 0,
    selected INTEGER NOT NULL,
    selection_probability REAL NOT NULL,
    exploration INTEGER NOT NULL,
    feature_snapshot_digest TEXT,
    extraction_status TEXT NOT NULL,
    feature_contracts_json TEXT NOT NULL,
    bandit_decision_json TEXT,
    item_json TEXT NOT NULL,
    PRIMARY KEY(decision_id, candidate_id),
    FOREIGN KEY(decision_id) REFERENCES attention_decisions(decision_id)
);
CREATE INDEX IF NOT EXISTS attention_decision_items_scope_candidate_idx
    ON attention_decision_items(principal, workspace, candidate_id, decision_id);
CREATE INDEX IF NOT EXISTS attention_decision_items_scope_served_idx
    ON attention_decision_items(principal, workspace, served_route, selected);
-- Decision reads always resolve and authorize the parent first, then hydrate
-- its immutable item set. Keeping decision_id first prevents SQLite from
-- choosing a scope-wide historical index for that local hydration.
CREATE INDEX IF NOT EXISTS attention_decision_items_decision_rank_idx
    ON attention_decision_items(decision_id, baseline_rank, candidate_id);

-- One immutable lane-wide root order. The exact policy artifact and posterior
-- version are frozen beside the complete origin-qualified candidate sequence.
CREATE TABLE IF NOT EXISTS attention_delivery_decisions (
    decision_id TEXT NOT NULL PRIMARY KEY,
    schema_version INTEGER NOT NULL,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    lane TEXT NOT NULL,
    projection_id TEXT NOT NULL,
    universe_digest TEXT NOT NULL,
    source_generation_token TEXT,
    policy_snapshot_id TEXT,
    policy_model_version TEXT,
    policy_snapshot_json TEXT,
    posterior_version INTEGER NOT NULL,
    seed_identity TEXT NOT NULL,
    universe_size INTEGER NOT NULL,
    page_size INTEGER NOT NULL,
    status TEXT NOT NULL,
    fallback_reason TEXT,
    health_json TEXT NOT NULL,
    min_visible_ms INTEGER NOT NULL,
    visibility_rule_version TEXT NOT NULL,
    context_json TEXT NOT NULL,
    decision_json TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS attention_delivery_decisions_scope_expiry_idx
    ON attention_delivery_decisions(principal, workspace, expires_at, decision_id);

CREATE TABLE IF NOT EXISTS attention_delivery_decision_items (
    decision_id TEXT NOT NULL,
    position INTEGER NOT NULL,
    candidate_id TEXT NOT NULL,
    source_revision TEXT,
    root_policy_propensity REAL NOT NULL,
    exposure_token TEXT NOT NULL UNIQUE,
    item_json TEXT NOT NULL,
    attribution_item_json TEXT,
    PRIMARY KEY(decision_id, position),
    UNIQUE(decision_id, candidate_id),
    FOREIGN KEY(decision_id) REFERENCES attention_delivery_decisions(decision_id)
);

CREATE TABLE IF NOT EXISTS attention_delivery_pages (
    delivery_id TEXT NOT NULL PRIMARY KEY,
    decision_id TEXT NOT NULL,
    page_index INTEGER NOT NULL,
    page_start INTEGER NOT NULL,
    page_size INTEGER NOT NULL,
    cursor TEXT,
    next_cursor TEXT,
    has_more INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    page_json TEXT NOT NULL,
    UNIQUE(decision_id, page_index),
    FOREIGN KEY(decision_id) REFERENCES attention_delivery_decisions(decision_id)
);

CREATE TABLE IF NOT EXISTS attention_delivery_page_items (
    delivery_id TEXT NOT NULL,
    decision_id TEXT NOT NULL,
    position INTEGER NOT NULL,
    PRIMARY KEY(delivery_id, position),
    FOREIGN KEY(delivery_id) REFERENCES attention_delivery_pages(delivery_id),
    FOREIGN KEY(decision_id, position)
        REFERENCES attention_delivery_decision_items(decision_id, position)
);
CREATE INDEX IF NOT EXISTS attention_delivery_page_items_decision_position_idx
    ON attention_delivery_page_items(decision_id, position);

CREATE TABLE IF NOT EXISTS attention_delivery_cursors (
    cursor TEXT NOT NULL PRIMARY KEY,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    lane TEXT NOT NULL,
    decision_id TEXT NOT NULL,
    delivery_id TEXT NOT NULL,
    projection_id TEXT NOT NULL,
    universe_digest TEXT NOT NULL,
    policy_snapshot_id TEXT,
    policy_model_version TEXT,
    posterior_version INTEGER NOT NULL,
    page_start INTEGER NOT NULL,
    page_size INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    FOREIGN KEY(decision_id) REFERENCES attention_delivery_decisions(decision_id),
    FOREIGN KEY(delivery_id) REFERENCES attention_delivery_pages(delivery_id)
);
CREATE INDEX IF NOT EXISTS attention_delivery_cursors_scope_idx
    ON attention_delivery_cursors(principal, workspace, lane, expires_at);

CREATE TABLE IF NOT EXISTS attention_impressions (
    impression_id TEXT NOT NULL PRIMARY KEY,
    schema_version INTEGER NOT NULL,
    event_id TEXT NOT NULL,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    decision_id TEXT NOT NULL,
    projection_id TEXT,
    delivery_id TEXT,
    page_index INTEGER,
    position INTEGER,
    exposure_token TEXT,
    candidate_id TEXT NOT NULL,
    source_revision TEXT,
    cluster_id TEXT NOT NULL,
    surface TEXT NOT NULL,
    rank INTEGER NOT NULL,
    first_visible_at INTEGER NOT NULL,
    accumulated_visible_ms INTEGER NOT NULL,
    visibility_rule_version TEXT NOT NULL,
    client_type TEXT NOT NULL,
    client_version TEXT NOT NULL,
    viewport_class TEXT NOT NULL,
    root_policy_propensity REAL,
    conditional_delivery_propensity REAL,
    verified INTEGER NOT NULL,
    dedupe_count INTEGER NOT NULL,
    last_recorded_at INTEGER NOT NULL,
    UNIQUE(principal, workspace, event_id)
);
CREATE INDEX IF NOT EXISTS attention_impressions_scope_decision_idx
    ON attention_impressions(principal, workspace, decision_id, candidate_id);

-- Serving health must not count the entire retained impression history on
-- every projection. Triggers keep the exact scoped total transactionally with
-- the authoritative rows; a one-time migration below backfills older stores.
CREATE TABLE IF NOT EXISTS attention_scope_health_counters (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    verified_impression_total INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(principal, workspace)
);
CREATE TRIGGER IF NOT EXISTS attention_scope_health_impression_insert
AFTER INSERT ON attention_impressions
WHEN NEW.verified = 1
BEGIN
    INSERT INTO attention_scope_health_counters(
        principal, workspace, verified_impression_total
    ) VALUES (NEW.principal, NEW.workspace, 1)
    ON CONFLICT(principal, workspace) DO UPDATE SET
        verified_impression_total = verified_impression_total + 1;
END;
CREATE TRIGGER IF NOT EXISTS attention_scope_health_impression_verify
AFTER UPDATE OF verified ON attention_impressions
WHEN OLD.verified <> NEW.verified
BEGIN
    INSERT INTO attention_scope_health_counters(
        principal, workspace, verified_impression_total
    ) VALUES (
        NEW.principal,
        NEW.workspace,
        CASE WHEN NEW.verified = 1 THEN 1 ELSE 0 END
    )
    ON CONFLICT(principal, workspace) DO UPDATE SET
        verified_impression_total = MAX(
            0,
            verified_impression_total
                + CASE WHEN NEW.verified = 1 THEN 1 ELSE -1 END
        );
END;
CREATE TRIGGER IF NOT EXISTS attention_scope_health_impression_delete
AFTER DELETE ON attention_impressions
WHEN OLD.verified = 1
BEGIN
    UPDATE attention_scope_health_counters
       SET verified_impression_total = MAX(0, verified_impression_total - 1)
     WHERE principal = OLD.principal AND workspace = OLD.workspace;
END;

CREATE TABLE IF NOT EXISTS attention_schema_migrations (
    migration_id TEXT NOT NULL PRIMARY KEY,
    applied_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS attention_bandit_updates (
    outcome_id TEXT NOT NULL PRIMARY KEY,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    surface TEXT NOT NULL,
    snapshot_id TEXT NOT NULL,
    decision_id TEXT,
    candidate_id TEXT NOT NULL,
    source_revision TEXT,
    impression_id TEXT,
    attribution_quality TEXT NOT NULL,
    degradation_reason TEXT,
    reward REAL,
    reward_strength REAL NOT NULL,
    feature_digest TEXT,
    posterior_version_before INTEGER,
    posterior_version_after INTEGER,
    uncertainty_before REAL,
    uncertainty_after REAL,
    affected_rank_before INTEGER,
    update_applied INTEGER NOT NULL,
    occurred_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    FOREIGN KEY(outcome_id) REFERENCES attention_outcomes(outcome_id),
    FOREIGN KEY(snapshot_id) REFERENCES attention_bandit_policy_snapshots(snapshot_id)
);
CREATE INDEX IF NOT EXISTS attention_bandit_updates_scope_time_idx
    ON attention_bandit_updates(principal, workspace, surface, snapshot_id, occurred_at);

CREATE TABLE IF NOT EXISTS attention_bandit_posterior_compactions (
    compaction_id TEXT NOT NULL PRIMARY KEY,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    surface TEXT NOT NULL,
    snapshot_id TEXT NOT NULL,
    posterior_version INTEGER NOT NULL,
    compacted_through_at INTEGER NOT NULL,
    update_count INTEGER NOT NULL,
    update_digest TEXT NOT NULL,
    posterior_json TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS attention_routing_scope_installs (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    snapshot_id TEXT NOT NULL,
    requested_mode TEXT NOT NULL,
    effective_mode TEXT NOT NULL,
    installed_at INTEGER NOT NULL,
    PRIMARY KEY (principal, workspace)
);

CREATE TABLE IF NOT EXISTS attention_actionability_scope_installs (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    snapshot_id TEXT NOT NULL,
    requested_mode TEXT NOT NULL,
    effective_mode TEXT NOT NULL,
    installed_at INTEGER NOT NULL,
    PRIMARY KEY (principal, workspace)
);

CREATE TABLE IF NOT EXISTS attention_bandit_scope_installs (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    snapshot_id TEXT NOT NULL,
    requested_mode TEXT NOT NULL,
    effective_mode TEXT NOT NULL,
    installed_at INTEGER NOT NULL,
    PRIMARY KEY (principal, workspace)
);

CREATE TABLE IF NOT EXISTS attention_training_runs (
    run_id TEXT NOT NULL PRIMARY KEY,
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    slice TEXT NOT NULL,
    status TEXT NOT NULL,
    reason TEXT,
    metrics_json TEXT,
    snapshot_id TEXT,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS attention_training_runs_scope_idx
    ON attention_training_runs(principal, workspace, created_at DESC, run_id DESC);
CREATE INDEX IF NOT EXISTS attention_outcomes_scope_decision_idx
    ON attention_outcomes(principal, workspace, decision_id, candidate_id);
"#;

const ATTENTION_EMBEDDING_BIND_MAX_ATTEMPTS: u32 = 8;
const ATTENTION_EMBEDDING_BIND_RETRY_BASE_MS: i64 = 60_000;
const ATTENTION_EMBEDDING_BIND_RETRY_MAX_MS: i64 = 3_600_000;
const ATTENTION_REPAIR_SEMANTIC_TEXT_MAX_CHARS: usize = 8_192;

fn ensure_actionability_score_input_digest(conn: &Connection) -> Result<()> {
    // Serialize schema inspection and alteration across concurrent process
    // openers. The second opener re-reads the committed schema and becomes a
    // no-op instead of racing into a duplicate-column ALTER.
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let migration = (|| -> Result<()> {
        let mut statement = conn.prepare("PRAGMA table_info(attention_actionability_scores)")?;
        let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
        let has_input_digest = columns
            .collect::<std::result::Result<Vec<_>, _>>()?
            .iter()
            .any(|column| column == "input_digest");
        if !has_input_digest {
            // Existing rows predate content-bound caching. The empty sentinel
            // can never match a real BLAKE3 digest, so they are recomputed.
            conn.execute(
                "ALTER TABLE attention_actionability_scores ADD COLUMN input_digest TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }
        Ok(())
    })();
    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        },
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        },
    }
}

fn ensure_embedding_bind_work_queue_columns(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let migration = (|| -> Result<()> {
        let mut statement = conn.prepare("PRAGMA table_info(attention_embedding_bind_work)")?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<HashSet<_>, _>>()?;
        drop(statement);
        for (column, definition) in [
            ("status", "TEXT NOT NULL DEFAULT 'pending'"),
            ("attempts", "INTEGER NOT NULL DEFAULT 0"),
            ("next_retry_at", "INTEGER"),
            ("lease_owner", "TEXT"),
            ("lease_expires_at", "INTEGER"),
            ("last_error_code", "TEXT"),
            ("last_attempt_at", "INTEGER"),
        ] {
            if !columns.contains(column) {
                conn.execute(
                    &format!(
                        "ALTER TABLE attention_embedding_bind_work ADD COLUMN {column} {definition}"
                    ),
                    [],
                )?;
            }
        }
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS attention_embedding_bind_work_due_idx
                 ON attention_embedding_bind_work(status, next_retry_at, created_at, outcome_id);
             CREATE INDEX IF NOT EXISTS attention_embedding_bind_work_scope_due_idx
                 ON attention_embedding_bind_work(principal, workspace, status, next_retry_at, created_at);
             CREATE INDEX IF NOT EXISTS attention_embedding_bind_work_reclaim_idx
                 ON attention_embedding_bind_work(status, lease_expires_at, created_at);",
        )?;
        Ok(())
    })();
    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        },
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        },
    }
}

fn embedding_bind_failure_is_retryable(error_code: &str) -> bool {
    matches!(
        error_code,
        "embedding_unavailable"
            | "embedding_resolution_failed"
            | "embedding_missing_from_batch"
            | "embedding_bind_failed"
    )
}

/// Add the V1 compatibility digest without rebuilding the legacy table.
///
/// Earlier code rebuilt the complete table during every schema transition.
/// That made startup materialize every feature payload and made a crash halfway
/// through a deployment unnecessarily risky. New writes use the normalized V2+
/// vector/binding tables. Legacy rows are copied by the bounded, resumable
/// repair method and remain readable until that finishes.
fn ensure_feature_snapshot_content_digest(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let migration = (|| -> Result<()> {
        let mut statement =
            conn.prepare("PRAGMA table_info(attention_candidate_feature_snapshots)")?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);
        if !columns.is_empty() && !columns.iter().any(|column| column == "content_digest") {
            conn.execute(
                "ALTER TABLE attention_candidate_feature_snapshots \
                 ADD COLUMN content_digest TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }
        Ok(())
    })();
    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        },
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        },
    }
}

fn ensure_decision_item_served_rank(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let migration = (|| -> Result<()> {
        let mut statement = conn.prepare("PRAGMA table_info(attention_decision_items)")?;
        let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
        let has_served_rank = columns
            .collect::<std::result::Result<Vec<_>, _>>()?
            .iter()
            .any(|column| column == "served_rank");
        if !has_served_rank {
            conn.execute(
                "ALTER TABLE attention_decision_items ADD COLUMN served_rank INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        Ok(())
    })();
    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        },
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        },
    }
}

fn ensure_bandit_decision_columns(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let migration = (|| -> Result<()> {
        let table_columns = |table: &str| -> Result<HashSet<String>> {
            let pragma = format!("PRAGMA table_info({table})");
            let mut statement = conn.prepare(&pragma)?;
            let columns = statement
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<std::result::Result<HashSet<_>, _>>()?;
            Ok(columns)
        };
        if !table_columns("attention_decisions")?.contains("bandit_health_json") {
            conn.execute(
                "ALTER TABLE attention_decisions ADD COLUMN bandit_health_json TEXT",
                [],
            )?;
        }
        if !table_columns("attention_decision_items")?.contains("bandit_decision_json") {
            conn.execute(
                "ALTER TABLE attention_decision_items ADD COLUMN bandit_decision_json TEXT",
                [],
            )?;
        }
        // The client already sends which decision surfaced the card it is acting
        // on (and its verified impression, when there is one), but the outcome
        // had nowhere to keep them — so the link was dropped on write and later
        // reconstructed by joining through `attention_bandit_updates`, which is
        // empty until the bandit runs. That is why 91% of rank-recompute jobs
        // carry no decision and every one of them goes stale.
        //
        // Storing the attribution the client already supplies preserves the
        // exact claim for later validation. Learning/recovery still requires a
        // matching durable decision/item/delivery/impression binding; the raw
        // claim is never authoritative by itself. Reconstructing even the claim
        // after the fact is not viable: the same lookup over ~1M decision items
        // costs 47ms or over two minutes depending purely on which rows it
        // touches, and the enqueue holds an Immediate transaction.
        let outcome_columns = table_columns("attention_outcomes")?;
        if !outcome_columns.contains("decision_id") {
            conn.execute(
                "ALTER TABLE attention_outcomes ADD COLUMN decision_id TEXT",
                [],
            )?;
        }
        if !outcome_columns.contains("impression_id") {
            conn.execute(
                "ALTER TABLE attention_outcomes ADD COLUMN impression_id TEXT",
                [],
            )?;
        }
        if !outcome_columns.contains("delivery_id") {
            conn.execute(
                "ALTER TABLE attention_outcomes ADD COLUMN delivery_id TEXT",
                [],
            )?;
        }
        Ok(())
    })();
    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        },
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        },
    }
}

fn ensure_delivery_impression_columns(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let migration = (|| -> Result<()> {
        let mut statement = conn.prepare("PRAGMA table_info(attention_impressions)")?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<HashSet<_>, _>>()?;
        if !columns.contains("delivery_id") {
            // Rebuild rather than ALTER so the obsolete FK to routing
            // decision membership is removed. Delivery verification is the
            // stricter transactional join performed by record_impression.
            conn.execute_batch(
                "CREATE TABLE attention_impressions_delivery_v1 ( \
                    impression_id TEXT NOT NULL PRIMARY KEY, schema_version INTEGER NOT NULL, \
                    event_id TEXT NOT NULL, principal TEXT NOT NULL, workspace TEXT NOT NULL, \
                    decision_id TEXT NOT NULL, projection_id TEXT, delivery_id TEXT, page_index INTEGER, position INTEGER, \
                    exposure_token TEXT, candidate_id TEXT NOT NULL, source_revision TEXT, \
                    cluster_id TEXT NOT NULL, surface TEXT NOT NULL, rank INTEGER NOT NULL, \
                    first_visible_at INTEGER NOT NULL, accumulated_visible_ms INTEGER NOT NULL, \
                    visibility_rule_version TEXT NOT NULL, client_type TEXT NOT NULL, \
                    client_version TEXT NOT NULL, viewport_class TEXT NOT NULL, \
                    root_policy_propensity REAL, conditional_delivery_propensity REAL, \
                    verified INTEGER NOT NULL, dedupe_count INTEGER NOT NULL, \
                    last_recorded_at INTEGER NOT NULL, UNIQUE(principal, workspace, event_id) \
                 ); \
                 INSERT INTO attention_impressions_delivery_v1 ( \
                    impression_id, schema_version, event_id, principal, workspace, decision_id, \
                    candidate_id, source_revision, cluster_id, surface, rank, first_visible_at, \
                    accumulated_visible_ms, visibility_rule_version, client_type, client_version, \
                    viewport_class, verified, dedupe_count, last_recorded_at \
                 ) SELECT impression_id, schema_version, event_id, principal, workspace, decision_id, \
                    candidate_id, source_revision, cluster_id, surface, rank, first_visible_at, \
                    accumulated_visible_ms, visibility_rule_version, client_type, client_version, \
                    viewport_class, verified, dedupe_count, last_recorded_at \
                   FROM attention_impressions; \
                 DROP TABLE attention_impressions; \
                 ALTER TABLE attention_impressions_delivery_v1 RENAME TO attention_impressions; \
                 CREATE INDEX attention_impressions_scope_decision_idx \
                   ON attention_impressions(principal, workspace, decision_id, candidate_id);",
            )?;
        } else if !columns.contains("projection_id") {
            conn.execute(
                "ALTER TABLE attention_impressions ADD COLUMN projection_id TEXT",
                [],
            )?;
        }
        Ok(())
    })();
    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        },
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        },
    }
}

/// Add the compact source-generation binding used by new delivery cursors.
/// Existing rows deliberately remain `NULL`: they retain the legacy exact
/// projection/digest validation path until their normal TTL expires.
fn ensure_delivery_source_generation_token(conn: &Connection) -> Result<()> {
    // The process bootstrap mutex cannot serialize two Magician processes
    // opening the same store. Acquire SQLite's cross-process writer lock
    // before inspecting the schema, then re-read under that lock so only one
    // opener can decide to ALTER the table.
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let migration = (|| -> Result<()> {
        let mut statement = conn.prepare("PRAGMA table_info(attention_delivery_decisions)")?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<HashSet<_>, _>>()?;
        drop(statement);
        if !columns.contains("source_generation_token") {
            conn.execute(
                "ALTER TABLE attention_delivery_decisions ADD COLUMN source_generation_token TEXT",
                [],
            )?;
        }
        Ok(())
    })();
    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        },
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        },
    }
}

/// Add durable projection references without performing an unbounded startup
/// backfill. Existing attribution rows are repaired by the explicit bounded
/// migration path after all tables are available; new writes populate the
/// columns transactionally.
fn ensure_projection_reference_columns(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let migration = (|| -> Result<()> {
        for (table, index) in [
            ("attention_outcomes", "attention_outcomes_projection_idx"),
            (
                "attention_impressions",
                "attention_impressions_projection_idx",
            ),
            (
                "attention_rank_recompute_jobs",
                "attention_rank_recompute_projection_idx",
            ),
        ] {
            let pragma = format!("PRAGMA table_info({table})");
            let mut statement = conn.prepare(&pragma)?;
            let columns = statement
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<std::result::Result<HashSet<_>, _>>()?;
            drop(statement);
            if !columns.contains("projection_id") {
                conn.execute(
                    &format!("ALTER TABLE {table} ADD COLUMN projection_id TEXT"),
                    [],
                )?;
            }
            conn.execute(
                &format!(
                    "CREATE INDEX IF NOT EXISTS {index} \
                     ON {table}(principal, workspace, projection_id)"
                ),
                [],
            )?;
        }
        Ok(())
    })();
    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        },
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        },
    }
}

fn ensure_normalized_projection_columns(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let migration = (|| -> Result<()> {
        let mut statement =
            conn.prepare("PRAGMA table_info(attention_canonical_projection_members)")?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<HashSet<_>, _>>()?;
        drop(statement);
        for column in ["item_binding_json", "rank_digest", "decision_digest"] {
            if !columns.contains(column) {
                conn.execute(
                    &format!(
                        "ALTER TABLE attention_canonical_projection_members ADD COLUMN {column} TEXT"
                    ),
                    [],
                )?;
            }
        }
        Ok(())
    })();
    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        },
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        },
    }
}

fn ensure_normalized_feature_vector_columns(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let migration = (|| -> Result<()> {
        let mut statement = conn.prepare("PRAGMA table_info(attention_feature_vectors)")?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<std::result::Result<HashSet<_>, _>>()?;
        drop(statement);
        for (column, definition) in [
            ("semantic_schema_version", "INTEGER NOT NULL DEFAULT 1"),
            ("semantic_model", "TEXT"),
            ("semantic_profile", "TEXT"),
        ] {
            if !columns.contains(column) {
                conn.execute(
                    &format!(
                        "ALTER TABLE attention_feature_vectors ADD COLUMN {column} {definition}"
                    ),
                    [],
                )?;
            }
        }
        Ok(())
    })();
    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        },
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        },
    }
}

const SCOPE_HEALTH_COUNTER_BACKFILL_MIGRATION: &str = "attention_scope_health_counters_v1";

/// Reassert the counter triggers after schema migrations which may rebuild the
/// impressions table. SQLite drops a table's triggers with the table, so
/// defining them only in the initial bootstrap would silently stop counters on
/// older stores during the delivery-column migration.
fn ensure_scope_health_counter_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS attention_scope_health_counters ( \
            principal TEXT NOT NULL, \
            workspace TEXT NOT NULL, \
            verified_impression_total INTEGER NOT NULL DEFAULT 0, \
            PRIMARY KEY(principal, workspace) \
         ); \
         CREATE TABLE IF NOT EXISTS attention_schema_migrations ( \
            migration_id TEXT NOT NULL PRIMARY KEY, \
            applied_at INTEGER NOT NULL \
         ); \
         CREATE TRIGGER IF NOT EXISTS attention_scope_health_impression_insert \
         AFTER INSERT ON attention_impressions WHEN NEW.verified = 1 BEGIN \
            INSERT INTO attention_scope_health_counters( \
                principal, workspace, verified_impression_total \
            ) VALUES (NEW.principal, NEW.workspace, 1) \
            ON CONFLICT(principal, workspace) DO UPDATE SET \
                verified_impression_total = verified_impression_total + 1; \
         END; \
         CREATE TRIGGER IF NOT EXISTS attention_scope_health_impression_verify \
         AFTER UPDATE OF verified ON attention_impressions \
         WHEN OLD.verified <> NEW.verified BEGIN \
            INSERT INTO attention_scope_health_counters( \
                principal, workspace, verified_impression_total \
            ) VALUES ( \
                NEW.principal, NEW.workspace, \
                CASE WHEN NEW.verified = 1 THEN 1 ELSE 0 END \
            ) ON CONFLICT(principal, workspace) DO UPDATE SET \
                verified_impression_total = MAX( \
                    0, verified_impression_total \
                       + CASE WHEN NEW.verified = 1 THEN 1 ELSE -1 END \
                ); \
         END; \
         CREATE TRIGGER IF NOT EXISTS attention_scope_health_impression_delete \
         AFTER DELETE ON attention_impressions WHEN OLD.verified = 1 BEGIN \
            UPDATE attention_scope_health_counters \
               SET verified_impression_total = MAX(0, verified_impression_total - 1) \
             WHERE principal = OLD.principal AND workspace = OLD.workspace; \
         END;",
    )
    .context("ensuring attention scope health counters and triggers")
}

/// Backfill the transactional scope counter exactly once for stores created
/// before the counter triggers existed. This may scan retained impressions,
/// but it runs under schema bootstrap rather than on an owner-facing serving
/// request. The durable marker prevents that historical scan on later boots.
fn ensure_scope_health_counter_backfill(conn: &Connection) -> Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let migration = (|| -> Result<()> {
        let applied = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM attention_schema_migrations WHERE migration_id = ?)",
            params![SCOPE_HEALTH_COUNTER_BACKFILL_MIGRATION],
            |row| row.get::<_, i64>(0),
        )? == 1;
        if applied {
            return Ok(());
        }
        conn.execute("DELETE FROM attention_scope_health_counters", [])?;
        conn.execute(
            "INSERT INTO attention_scope_health_counters( \
                 principal, workspace, verified_impression_total \
             ) \
             SELECT principal, workspace, COUNT(*) \
             FROM attention_impressions WHERE verified = 1 \
             GROUP BY principal, workspace",
            [],
        )?;
        conn.execute(
            "INSERT INTO attention_schema_migrations(migration_id, applied_at) VALUES (?, ?)",
            params![
                SCOPE_HEALTH_COUNTER_BACKFILL_MIGRATION,
                chrono::Utc::now().timestamp_millis(),
            ],
        )?;
        Ok(())
    })();
    match migration {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            Ok(())
        },
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        },
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AttentionConnectionTelemetry {
    pub writer_wait_count: u64,
    pub writer_wait_micros: u64,
    pub read_wait_count: u64,
    pub read_wait_micros: u64,
    pub read_hold_count: u64,
    pub read_hold_micros: u64,
    pub read_max_hold_micros: u64,
    pub read_in_use: usize,
    pub read_peak_in_use: usize,
    pub read_capacity: usize,
}

struct AttentionWriterConnection {
    conn: Mutex<Connection>,
    queue: Mutex<AttentionWriterQueueState>,
    available: Condvar,
    wait_count: AtomicU64,
    wait_micros: AtomicU64,
}

#[derive(Default)]
struct AttentionWriterQueueState {
    next_ticket: u64,
    serving_ticket: u64,
    /// Tickets whose waiter gave up ([`AttentionWriterConnection::lock_within`]).
    /// The release path skips them; otherwise every later caller would wait
    /// on a ticket nobody holds.
    abandoned: std::collections::BTreeSet<u64>,
}

/// A bounded writer acquisition gave up: the writer stayed held for longer
/// than the caller was prepared to wait. Request-path writes map this to a
/// degraded response instead of queueing behind maintenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttentionWriterBusy {
    pub waited: std::time::Duration,
}

impl std::fmt::Display for AttentionWriterBusy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "attention writer busy: another writer held it for the whole {} ms wait",
            self.waited.as_millis()
        )
    }
}

impl std::error::Error for AttentionWriterBusy {}

/// How long a serving-path write (a decision, a projection, a delivery) waits
/// for the writer before the page is served degraded. Maintenance passes are
/// bounded elsewhere; this is the guard for the next long holder.
pub const REQUEST_PATH_WRITER_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

struct AttentionWriterGuard<'a> {
    owner: &'a AttentionWriterConnection,
    conn: Option<std::sync::MutexGuard<'a, Connection>>,
    ticket: u64,
}

impl Deref for AttentionWriterGuard<'_> {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        self.conn
            .as_ref()
            .expect("attention writer connection already released")
    }
}

impl DerefMut for AttentionWriterGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.conn
            .as_mut()
            .expect("attention writer connection already released")
    }
}

impl Drop for AttentionWriterGuard<'_> {
    fn drop(&mut self) {
        // Release SQLite before advancing the ticket. A waiter that observes
        // its ticket as serving can therefore acquire the connection without
        // racing the previous owner or being reordered by std::Mutex.
        drop(self.conn.take());
        let mut queue = self
            .owner
            .queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if queue.serving_ticket != self.ticket {
            tracing::error!(
                serving_ticket = queue.serving_ticket,
                owner_ticket = self.ticket,
                "attention writer ticket state drifted while releasing owner"
            );
        }
        // Drop must remain panic-free during an owner unwind. Recover liveness
        // from impossible/corrupt queue drift by advancing from the actual
        // owner ticket rather than aborting on a second panic.
        queue.serving_ticket = self.ticket.wrapping_add(1);
        loop {
            let next = queue.serving_ticket;
            if !queue.abandoned.remove(&next) {
                break;
            }
            queue.serving_ticket = next.wrapping_add(1);
        }
        drop(queue);
        // A ticket condition cannot safely use notify_one: it may wake a later
        // ticket and leave the serving waiter asleep. Every waiter rechecks the
        // bounded scalar predicate before proceeding.
        self.owner.available.notify_all();
    }
}

impl AttentionWriterConnection {
    fn new(conn: Connection) -> Self {
        Self {
            conn: Mutex::new(conn),
            queue: Mutex::new(AttentionWriterQueueState::default()),
            available: Condvar::new(),
            wait_count: AtomicU64::new(0),
            wait_micros: AtomicU64::new(0),
        }
    }

    #[track_caller]
    fn lock(&self) -> std::sync::LockResult<AttentionWriterGuard<'_>> {
        let started = Instant::now();
        let mut queue = self
            .queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let ticket = queue.next_ticket;
        queue.next_ticket = queue.next_ticket.wrapping_add(1);
        let waited = queue.serving_ticket != ticket;
        while queue.serving_ticket != ticket {
            queue = self
                .available
                .wait(queue)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        drop(queue);

        let (conn, poisoned) = match self.conn.lock() {
            Ok(conn) => (conn, false),
            Err(error) => (error.into_inner(), true),
        };
        let wait_micros = started.elapsed().as_micros().try_into().unwrap_or(u64::MAX);
        if waited {
            // A real queued acquisition can complete inside the clock's
            // reporting granularity. Preserve a non-zero telemetry sample so
            // wait_count and aggregate wait time cannot contradict each other.
            let wait_micros = wait_micros.max(1);
            self.wait_count.fetch_add(1, Ordering::Relaxed);
            self.wait_micros.fetch_add(wait_micros, Ordering::Relaxed);
            if wait_micros >= 10_000 {
                let caller = std::panic::Location::caller();
                tracing::warn!(
                    wait_micros,
                    file = caller.file(),
                    line = caller.line(),
                    "attention writer acquisition waited"
                );
            }
        }
        let guard = AttentionWriterGuard {
            owner: self,
            conn: Some(conn),
            ticket,
        };
        if poisoned {
            Err(std::sync::PoisonError::new(guard))
        } else {
            Ok(guard)
        }
    }

    /// Acquire the writer in FIFO order, giving up after `wait`. A waiter
    /// that gives up abandons its ticket, and the release path skips
    /// abandoned tickets, so a timed-out caller leaves the queue exactly as
    /// serviceable as before it queued.
    #[track_caller]
    fn lock_within(
        &self,
        wait: std::time::Duration,
    ) -> std::result::Result<AttentionWriterGuard<'_>, AttentionWriterBusy> {
        let started = Instant::now();
        let deadline = started + wait;
        let mut queue = self
            .queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let ticket = queue.next_ticket;
        queue.next_ticket = queue.next_ticket.wrapping_add(1);
        let waited = queue.serving_ticket != ticket;
        while queue.serving_ticket != ticket {
            let now = Instant::now();
            if now >= deadline {
                queue.abandoned.insert(ticket);
                let waited = started.elapsed();
                drop(queue);
                self.wait_count.fetch_add(1, Ordering::Relaxed);
                self.wait_micros.fetch_add(
                    waited.as_micros().try_into().unwrap_or(u64::MAX),
                    Ordering::Relaxed,
                );
                let caller = std::panic::Location::caller();
                tracing::warn!(
                    wait_micros = waited.as_micros() as u64,
                    file = caller.file(),
                    line = caller.line(),
                    "attention writer acquisition gave up; serving degraded"
                );
                return Err(AttentionWriterBusy { waited });
            }
            let (next, _) = self
                .available
                .wait_timeout(queue, deadline - now)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            queue = next;
        }
        drop(queue);
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if waited {
            let wait_micros: u64 = started.elapsed().as_micros().try_into().unwrap_or(u64::MAX);
            self.wait_count.fetch_add(1, Ordering::Relaxed);
            self.wait_micros
                .fetch_add(wait_micros.max(1), Ordering::Relaxed);
        }
        Ok(AttentionWriterGuard {
            owner: self,
            conn: Some(conn),
            ticket,
        })
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    fn issued_ticket_count(&self) -> u64 {
        self.queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .next_ticket
    }
}

struct AttentionReadPoolState {
    idle: Vec<Connection>,
    maintenance: bool,
}

/// Every supported process that opens the attention ledger retains a shared
/// lifecycle lease. Physical replacement first wins the separate election
/// lock, then upgrades its own lifecycle lease to exclusive. The election
/// prevents two reclaimers from dropping their shared leases simultaneously;
/// the exclusive lifecycle lease proves no other supported process still has
/// live SQLite handles that could write the inode after it is replaced.
struct AttentionProcessLease {
    lifecycle: Mutex<File>,
    lifecycle_path: PathBuf,
    election_path: PathBuf,
}

impl AttentionProcessLease {
    fn open(database_path: &Path) -> Result<Self> {
        let lifecycle_path = sqlite_companion_path(database_path, ".lifecycle.lock");
        let election_path = sqlite_companion_path(database_path, ".reclaim.lock");
        let lifecycle = open_attention_lock_file(&lifecycle_path)?;
        FileExt::lock_shared(&lifecycle).with_context(|| {
            format!(
                "acquiring shared attention lifecycle lease {}",
                lifecycle_path.display()
            )
        })?;
        Ok(Self {
            lifecycle: Mutex::new(lifecycle),
            lifecycle_path,
            election_path,
        })
    }

    fn acquire_exclusive(&self) -> Result<AttentionExclusiveProcessLease<'_>> {
        self.acquire_exclusive_with_timeout(std::time::Duration::from_millis(
            ATTENTION_PROCESS_DRAIN_TIMEOUT_MS,
        ))
    }

    fn acquire_exclusive_with_timeout(
        &self,
        timeout: std::time::Duration,
    ) -> Result<AttentionExclusiveProcessLease<'_>> {
        let election = open_attention_lock_file(&self.election_path)?;
        match FileExt::try_lock_exclusive(&election) {
            Ok(()) => {},
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                anyhow::bail!("attention learning physical reclaim is already running")
            },
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "acquiring attention reclaim election {}",
                        self.election_path.display()
                    )
                })
            },
        }

        let lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        FileExt::unlock(&*lifecycle).with_context(|| {
            format!(
                "releasing shared attention lifecycle lease {}",
                self.lifecycle_path.display()
            )
        })?;
        let started = Instant::now();
        loop {
            match FileExt::try_lock_exclusive(&*lifecycle) {
                Ok(()) => {
                    return Ok(AttentionExclusiveProcessLease {
                        lifecycle,
                        lifecycle_path: &self.lifecycle_path,
                        _election: election,
                        exclusive: true,
                    })
                },
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && started.elapsed() < timeout =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                },
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    FileExt::lock_shared(&*lifecycle).with_context(|| {
                        format!(
                            "restoring shared attention lifecycle lease {}",
                            self.lifecycle_path.display()
                        )
                    })?;
                    anyhow::bail!(
                        "timed out draining other attention-learning processes after {} ms",
                        timeout.as_millis()
                    );
                },
                Err(error) => {
                    let restore = FileExt::lock_shared(&*lifecycle).with_context(|| {
                        format!(
                            "restoring shared attention lifecycle lease {}",
                            self.lifecycle_path.display()
                        )
                    });
                    return match restore {
                        Ok(()) => Err(error).with_context(|| {
                            format!(
                                "acquiring exclusive attention lifecycle lease {}",
                                self.lifecycle_path.display()
                            )
                        }),
                        Err(restore_error) => Err(anyhow::anyhow!(
                            "acquiring exclusive attention lifecycle lease failed: {error}; restoring the shared lease also failed: {restore_error:#}"
                        )),
                    };
                },
            }
        }
    }
}

struct AttentionExclusiveProcessLease<'a> {
    lifecycle: std::sync::MutexGuard<'a, File>,
    lifecycle_path: &'a Path,
    _election: File,
    exclusive: bool,
}

impl AttentionExclusiveProcessLease<'_> {
    fn downgrade(&mut self) -> Result<()> {
        if !self.exclusive {
            return Ok(());
        }
        FileExt::unlock(&*self.lifecycle).with_context(|| {
            format!(
                "releasing exclusive attention lifecycle lease {}",
                self.lifecycle_path.display()
            )
        })?;
        if let Err(error) = FileExt::lock_shared(&*self.lifecycle) {
            // Conservatively retain exclusive ownership if the shared lease
            // cannot be restored. That blocks another process from opening a
            // stale handle while this process reports the degraded state.
            let _ = FileExt::lock_exclusive(&*self.lifecycle);
            return Err(error).with_context(|| {
                format!(
                    "restoring shared attention lifecycle lease {}",
                    self.lifecycle_path.display()
                )
            });
        }
        self.exclusive = false;
        Ok(())
    }
}

impl Drop for AttentionExclusiveProcessLease<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.downgrade() {
            tracing::error!(
                error = %error,
                path = %self.lifecycle_path.display(),
                "attention reclaim could not restore its shared process lease"
            );
        }
    }
}

struct AttentionReadPool {
    state: Mutex<AttentionReadPoolState>,
    available: Condvar,
    capacity: usize,
    in_use: AtomicUsize,
    peak_in_use: AtomicUsize,
    wait_count: AtomicU64,
    wait_micros: AtomicU64,
    hold_count: AtomicU64,
    hold_micros: AtomicU64,
    max_hold_micros: AtomicU64,
}

impl AttentionReadPool {
    fn open(path: &Path, capacity: usize) -> Result<Self> {
        anyhow::ensure!(capacity > 0, "attention read pool capacity is zero");
        let mut idle = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .with_context(|| {
                    format!(
                        "opening attention learning read connection at {}",
                        path.display()
                    )
                })?;
            configure_attention_connection(&conn, true)?;
            idle.push(conn);
        }
        Ok(Self {
            state: Mutex::new(AttentionReadPoolState {
                idle,
                maintenance: false,
            }),
            available: Condvar::new(),
            capacity,
            in_use: AtomicUsize::new(0),
            peak_in_use: AtomicUsize::new(0),
            wait_count: AtomicU64::new(0),
            wait_micros: AtomicU64::new(0),
            hold_count: AtomicU64::new(0),
            hold_micros: AtomicU64::new(0),
            max_hold_micros: AtomicU64::new(0),
        })
    }

    fn acquire(&self, operation: &'static str) -> Result<AttentionReadConnection<'_>> {
        let started = Instant::now();
        let max_wait = std::time::Duration::from_millis(ATTENTION_READ_POOL_WAIT_TIMEOUT_MS);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.maintenance {
            anyhow::bail!("attention learning maintenance is in progress");
        }
        let mut waited = false;
        while state.idle.is_empty() {
            waited = true;
            let elapsed = started.elapsed();
            if elapsed >= max_wait {
                let wait_micros = elapsed.as_micros().try_into().unwrap_or(u64::MAX);
                self.wait_count.fetch_add(1, Ordering::Relaxed);
                self.wait_micros.fetch_add(wait_micros, Ordering::Relaxed);
                tracing::warn!(
                    operation,
                    wait_micros,
                    capacity = self.capacity,
                    "attention read pool acquisition timed out"
                );
                anyhow::bail!(
                    "attention read pool exhausted during {operation} after {} ms",
                    ATTENTION_READ_POOL_WAIT_TIMEOUT_MS
                );
            }
            let (next_state, timeout) = self
                .available
                .wait_timeout(state, max_wait.saturating_sub(elapsed))
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next_state;
            if state.maintenance {
                anyhow::bail!("attention learning maintenance is in progress");
            }
            if timeout.timed_out() && state.idle.is_empty() {
                let wait_micros = started.elapsed().as_micros().try_into().unwrap_or(u64::MAX);
                self.wait_count.fetch_add(1, Ordering::Relaxed);
                self.wait_micros.fetch_add(wait_micros, Ordering::Relaxed);
                tracing::warn!(
                    operation,
                    wait_micros,
                    capacity = self.capacity,
                    "attention read pool acquisition timed out"
                );
                anyhow::bail!(
                    "attention read pool exhausted during {operation} after {} ms",
                    ATTENTION_READ_POOL_WAIT_TIMEOUT_MS
                );
            }
        }
        let conn = state
            .idle
            .pop()
            .context("attention read pool woke without an available connection")?;
        drop(state);
        let in_use = self.in_use.fetch_add(1, Ordering::AcqRel) + 1;
        self.peak_in_use.fetch_max(in_use, Ordering::AcqRel);
        let wait_micros = started.elapsed().as_micros().try_into().unwrap_or(u64::MAX);
        if waited {
            self.wait_count.fetch_add(1, Ordering::Relaxed);
            self.wait_micros.fetch_add(wait_micros, Ordering::Relaxed);
            tracing::debug!(
                operation,
                wait_micros,
                capacity = self.capacity,
                "attention read pool acquisition waited"
            );
        }
        Ok(AttentionReadConnection {
            pool: self,
            conn: Some(conn),
            operation,
            acquired_at: Instant::now(),
        })
    }

    fn begin_maintenance(&self, recovery_path: &Path) -> Result<AttentionReadMaintenanceGuard<'_>> {
        let started = Instant::now();
        let timeout = std::time::Duration::from_millis(ATTENTION_MAINTENANCE_DRAIN_TIMEOUT_MS);
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        anyhow::ensure!(
            !state.maintenance,
            "attention learning maintenance is already running"
        );
        state.maintenance = true;
        while state.idle.len() != self.capacity {
            let elapsed = started.elapsed();
            if elapsed >= timeout {
                state.maintenance = false;
                self.available.notify_all();
                anyhow::bail!(
                    "timed out draining attention readers after {} ms",
                    ATTENTION_MAINTENANCE_DRAIN_TIMEOUT_MS
                );
            }
            let (next, wait) = self
                .available
                .wait_timeout(state, timeout.saturating_sub(elapsed))
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
            if wait.timed_out() && state.idle.len() != self.capacity {
                state.maintenance = false;
                self.available.notify_all();
                anyhow::bail!(
                    "timed out draining attention readers after {} ms",
                    ATTENTION_MAINTENANCE_DRAIN_TIMEOUT_MS
                );
            }
        }
        let connections = std::mem::take(&mut state.idle);
        drop(state);
        Ok(AttentionReadMaintenanceGuard {
            pool: self,
            connections,
            recovery_path: recovery_path.to_path_buf(),
        })
    }

    fn telemetry(&self) -> AttentionConnectionTelemetry {
        AttentionConnectionTelemetry {
            writer_wait_count: 0,
            writer_wait_micros: 0,
            read_wait_count: self.wait_count.load(Ordering::Relaxed),
            read_wait_micros: self.wait_micros.load(Ordering::Relaxed),
            read_hold_count: self.hold_count.load(Ordering::Relaxed),
            read_hold_micros: self.hold_micros.load(Ordering::Relaxed),
            read_max_hold_micros: self.max_hold_micros.load(Ordering::Relaxed),
            read_in_use: self.in_use.load(Ordering::Relaxed),
            read_peak_in_use: self.peak_in_use.load(Ordering::Relaxed),
            read_capacity: self.capacity,
        }
    }
}

struct AttentionReadMaintenanceGuard<'a> {
    pool: &'a AttentionReadPool,
    connections: Vec<Connection>,
    recovery_path: PathBuf,
}

impl AttentionReadMaintenanceGuard<'_> {
    fn close_connections(&mut self) {
        self.connections.clear();
    }

    fn reopen(&mut self, path: &Path) -> Result<()> {
        let mut replacements = Vec::with_capacity(self.pool.capacity);
        for _ in 0..self.pool.capacity {
            let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .with_context(|| {
                    format!(
                        "reopening attention learning read connection at {}",
                        path.display()
                    )
                })?;
            configure_attention_connection(&conn, true)?;
            replacements.push(conn);
        }
        self.connections = replacements;
        Ok(())
    }
}

impl Drop for AttentionReadMaintenanceGuard<'_> {
    fn drop(&mut self) {
        if self.connections.len() != self.pool.capacity {
            if let Err(error) = self.reopen(&self.recovery_path.clone()) {
                tracing::error!(
                    error = %error,
                    path = %self.recovery_path.display(),
                    "attention maintenance could not restore the read pool"
                );
            }
        }
        let mut state = self
            .pool
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.idle.append(&mut self.connections);
        state.maintenance = false;
        drop(state);
        self.pool.available.notify_all();
    }
}

struct AttentionReadConnection<'a> {
    pool: &'a AttentionReadPool,
    conn: Option<Connection>,
    operation: &'static str,
    acquired_at: Instant,
}

impl Deref for AttentionReadConnection<'_> {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        self.conn
            .as_ref()
            .expect("attention read connection already returned")
    }
}

impl DerefMut for AttentionReadConnection<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.conn
            .as_mut()
            .expect("attention read connection already returned")
    }
}

impl Drop for AttentionReadConnection<'_> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            let hold_micros = self
                .acquired_at
                .elapsed()
                .as_micros()
                .try_into()
                .unwrap_or(u64::MAX);
            self.pool.hold_count.fetch_add(1, Ordering::Relaxed);
            self.pool
                .hold_micros
                .fetch_add(hold_micros, Ordering::Relaxed);
            self.pool
                .max_hold_micros
                .fetch_max(hold_micros, Ordering::Relaxed);
            if hold_micros >= 1_000_000 {
                tracing::warn!(
                    operation = self.operation,
                    hold_micros,
                    capacity = self.pool.capacity,
                    "attention read connection was held for too long"
                );
            }
            let mut state = self
                .pool
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.idle.push(conn);
            self.pool.in_use.fetch_sub(1, Ordering::AcqRel);
            drop(state);
            self.pool.available.notify_one();
        }
    }
}

/// Selects the read pool for preview/query-only operations while retaining the
/// same narrow code path for operations that optionally mutate. This prevents
/// `apply=false` maintenance probes from parking behind an unrelated writer.
enum AttentionConnectionGuard<'a> {
    Writer(AttentionWriterGuard<'a>),
    Reader(AttentionReadConnection<'a>),
}

impl Deref for AttentionConnectionGuard<'_> {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Writer(conn) => conn,
            Self::Reader(conn) => conn,
        }
    }
}

impl DerefMut for AttentionConnectionGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            Self::Writer(conn) => conn,
            Self::Reader(conn) => conn,
        }
    }
}

/// Clears the SQLite progress callback even when reconciliation returns early.
/// A connection is reused after this operation, so leaving the deadline-backed
/// callback installed would make an unrelated later query fail spuriously.
struct AttentionReconciliationProgressGuard<'a>(&'a Connection);

impl Drop for AttentionReconciliationProgressGuard<'_> {
    fn drop(&mut self) {
        self.0.progress_handler(0, None::<fn() -> bool>);
    }
}

fn install_attention_reconciliation_progress_guard(
    connection: &Connection,
) -> AttentionReconciliationProgressGuard<'_> {
    install_attention_reconciliation_progress_guard_with_timeout(
        connection,
        std::time::Duration::from_millis(ATTENTION_RECONCILIATION_SQL_TIMEOUT_MS),
    )
}

fn install_attention_reconciliation_progress_guard_with_timeout(
    connection: &Connection,
    timeout: std::time::Duration,
) -> AttentionReconciliationProgressGuard<'_> {
    let deadline = Instant::now() + timeout;
    connection.progress_handler(
        ATTENTION_RECONCILIATION_PROGRESS_INTERVAL_OPS,
        Some(move || Instant::now() >= deadline),
    );
    AttentionReconciliationProgressGuard(connection)
}

fn configure_attention_connection(conn: &Connection, read_only: bool) -> Result<()> {
    conn.busy_timeout(std::time::Duration::from_millis(
        ATTENTION_SQLITE_BUSY_TIMEOUT_MS,
    ))
    .context("configuring attention SQLite busy timeout")?;
    conn.execute_batch("PRAGMA foreign_keys = ON")
        .context("enabling attention SQLite foreign keys")?;
    if read_only {
        conn.execute_batch("PRAGMA query_only = ON")
            .context("marking attention SQLite read connection query-only")?;
    }
    Ok(())
}

fn attention_file_size(path: &Path) -> Result<u64> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.file_type().is_file(),
                "attention database is not a regular file"
            );
            anyhow::ensure!(
                !metadata.file_type().is_symlink(),
                "attention database cannot be a symlink"
            );
            Ok(metadata.len())
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error)
            .with_context(|| format!("reading attention database metadata at {}", path.display())),
    }
}

fn quote_sqlite_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn sqlite_schema_rows(conn: &Connection) -> Result<Vec<(String, String, Option<String>)>> {
    let mut statement = conn.prepare(
        "SELECT type, name, sql FROM sqlite_master \
         WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
    )?;
    let rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn sqlite_table_counts(conn: &Connection) -> Result<Vec<(String, u64)>> {
    let mut statement = conn.prepare(
        "SELECT name FROM sqlite_master \
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(statement);
    let mut counts = Vec::with_capacity(names.len());
    for name in names {
        let count: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM {}", quote_sqlite_identifier(&name)),
            [],
            |row| row.get(0),
        )?;
        counts.push((name, count.max(0) as u64));
    }
    Ok(counts)
}

fn sqlite_sequence_rows(conn: &Connection) -> Result<Vec<(String, i64)>> {
    let exists = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'sqlite_sequence')",
        [],
        |row| row.get::<_, i64>(0),
    )? == 1;
    if !exists {
        return Ok(Vec::new());
    }
    let mut statement = conn.prepare("SELECT name, seq FROM sqlite_sequence ORDER BY name")?;
    let rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn verify_attention_rebuild(source: &Connection, candidate_path: &Path) -> Result<(usize, u64)> {
    let candidate = Connection::open_with_flags(candidate_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| {
        format!(
            "opening rebuilt attention database at {}",
            candidate_path.display()
        )
    })?;
    configure_attention_connection(&candidate, true)?;
    let integrity: String = candidate.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    anyhow::ensure!(
        integrity == "ok",
        "rebuilt attention database failed integrity_check: {integrity}"
    );
    let mut foreign_keys = candidate.prepare("PRAGMA foreign_key_check")?;
    anyhow::ensure!(
        !foreign_keys.exists([])?,
        "rebuilt attention database failed foreign_key_check"
    );
    drop(foreign_keys);
    anyhow::ensure!(
        sqlite_schema_rows(source)? == sqlite_schema_rows(&candidate)?,
        "rebuilt attention database schema does not match the source"
    );
    let source_counts = sqlite_table_counts(source)?;
    let candidate_counts = sqlite_table_counts(&candidate)?;
    anyhow::ensure!(
        source_counts == candidate_counts,
        "rebuilt attention database row counts do not match the source"
    );
    anyhow::ensure!(
        sqlite_sequence_rows(source)? == sqlite_sequence_rows(&candidate)?,
        "rebuilt attention database AUTOINCREMENT state does not match the source"
    );
    for pragma in ["user_version", "application_id"] {
        let source_value: i64 =
            source.query_row(&format!("PRAGMA {pragma}"), [], |row| row.get(0))?;
        let candidate_value: i64 =
            candidate.query_row(&format!("PRAGMA {pragma}"), [], |row| row.get(0))?;
        anyhow::ensure!(
            source_value == candidate_value,
            "rebuilt attention database {pragma} does not match the source"
        );
    }
    let row_count = source_counts.iter().map(|(_, count)| *count).sum();
    Ok((source_counts.len(), row_count))
}

fn sync_parent_directory(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .context("attention database has no parent directory")?;
    std::fs::File::open(parent)
        .with_context(|| format!("opening attention database parent {}", parent.display()))?
        .sync_all()
        .with_context(|| format!("syncing attention database parent {}", parent.display()))
}

fn sync_regular_file(path: &Path) -> Result<()> {
    std::fs::File::open(path)
        .with_context(|| format!("opening rebuilt attention database {}", path.display()))?
        .sync_all()
        .with_context(|| format!("syncing rebuilt attention database {}", path.display()))
}

fn sqlite_companion_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn open_attention_lock_file(path: &Path) -> Result<File> {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        anyhow::ensure!(
            metadata.file_type().is_file() && !metadata.file_type().is_symlink(),
            "attention lifecycle lock is not a regular file: {}",
            path.display()
        );
    }
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .with_context(|| format!("opening attention lifecycle lock {}", path.display()))?;
    anyhow::ensure!(
        file.metadata()?.file_type().is_file(),
        "attention lifecycle lock is not a regular file: {}",
        path.display()
    );
    Ok(file)
}

fn remove_sqlite_companions(path: &Path) -> Result<()> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let companion = sqlite_companion_path(path, suffix);
        match std::fs::remove_file(&companion) {
            Ok(()) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("removing SQLite companion {}", companion.display()))
            },
        }
    }
    Ok(())
}

fn open_attention_writer(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)
        .with_context(|| format!("reopening attention writer at {}", path.display()))?;
    configure_attention_connection(&conn, false)?;
    conn.query_row("PRAGMA journal_mode=WAL", [], |_row| Ok(()))
        .context("restoring attention WAL journal mode")?;
    Ok(conn)
}

#[cfg(unix)]
fn atomically_replace_file(source: &Path, target: &Path) -> Result<()> {
    std::fs::rename(source, target).with_context(|| {
        format!(
            "atomically replacing {} with {}",
            target.display(),
            source.display()
        )
    })
}

#[cfg(not(unix))]
fn atomically_replace_file(_source: &Path, _target: &Path) -> Result<()> {
    anyhow::bail!("atomic attention database replacement is not supported on this platform")
}

#[derive(Debug, Clone, Copy)]
pub struct AttentionScorePosterior {
    pub surface_score: Option<f64>,
    pub actionability_probability: Option<f64>,
    pub actionability_weight: f64,
}

#[derive(Clone)]
pub struct AttentionLearningStore {
    /// Single ordered writer boundary. It is never held across `.await`; every
    /// caller enters it inside `spawn_blocking`.
    conn: Arc<AttentionWriterConnection>,
    reads: Arc<AttentionReadPool>,
    path: Arc<PathBuf>,
    process_lease: Arc<AttentionProcessLease>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PersistedAttentionOutcome {
    pub outcome_id: String,
    pub outcome: AttentionOutcomeKind,
    /// False means the same scoped client event was already committed and the
    /// original row was replayed idempotently.
    pub inserted: bool,
}

/// One outcome joined to the feature vector captured when it was served.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionabilityTrainingRow {
    pub outcome_id: String,
    pub outcome: AttentionOutcomeKind,
    pub reason: Option<String>,
    pub decision_id: Option<String>,
    pub impression_id: Option<String>,
    pub occurred_at: i64,
    pub candidate_id: String,
    pub surface: String,
    pub source_revision: Option<String>,
    pub feature_contract: Option<String>,
    pub semantic_extractor_contract: Option<String>,
    pub semantic_prompt_version: Option<String>,
    pub semantic_schema_version: Option<u32>,
    pub semantic_model: Option<String>,
    pub semantic_profile: Option<String>,
    pub features: Option<BTreeMap<String, f64>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoutingTrainingRow {
    pub outcome: AttentionOutcomeKind,
    pub reason: Option<String>,
    pub decision_id: Option<String>,
    pub occurred_at: i64,
    pub candidate_id: String,
    pub surface: String,
    pub served_route: Option<String>,
    pub owner_action_required_probability: Option<f64>,
    pub information_value_probability: Option<f64>,
    pub cluster_size: usize,
    pub features: Option<BTreeMap<String, f64>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionabilityScopeInstall {
    pub snapshot_id: String,
    pub requested_mode: crate::config::AttentionActionabilityMode,
    pub effective_mode: crate::config::AttentionActionabilityMode,
    pub installed_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BanditScopeInstall {
    pub snapshot_id: String,
    pub requested_mode: crate::config::AttentionBanditMode,
    pub effective_mode: crate::config::AttentionBanditMode,
    pub installed_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttentionTrainingRunRecord {
    pub run_id: String,
    pub principal: String,
    pub workspace: String,
    pub slice: String,
    pub status: String,
    pub reason: Option<String>,
    pub metrics_json: Option<String>,
    pub snapshot_id: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAttentionEmbeddingBind {
    pub outcome_id: String,
    pub principal: String,
    pub workspace: String,
    pub surface: AttentionSurface,
    pub candidate_id: String,
    pub source_revision: Option<String>,
    pub semantic_text: String,
    pub attempts: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttentionEmbeddingBindQueueCounts {
    pub pending: u64,
    pub in_flight: u64,
    pub retry: u64,
    pub dead: u64,
    pub next_retry_at: Option<i64>,
    pub error_counts: BTreeMap<String, u64>,
}

#[derive(Debug, Clone)]
pub(super) struct PersistedCandidateEmbedding {
    pub source_revision: Option<String>,
    pub content_digest: String,
    pub embedding: SemanticEmbedding,
}

/// One candidate's features exactly as serving used them.
///
/// The contract fields travel with the vector so a trainer can refuse rows
/// produced by a different extractor or prompt instead of silently mixing two
/// populations into one model.
#[derive(Debug, Clone)]
pub struct CapturedCandidateFeatures {
    pub surface: String,
    pub candidate_id: String,
    pub source_revision: String,
    /// Distinguishes variants a shared revision cannot. Two rows with the same
    /// candidate and revision are two genuinely different things the owner may
    /// have been shown, and `first_served_at` orders them.
    pub content_digest: String,
    pub feature_contract: String,
    pub temporal_contract: String,
    pub semantic_extractor_contract: String,
    pub semantic_prompt_version: Option<String>,
    pub semantic_schema_version: u32,
    pub semantic_model: Option<String>,
    pub semantic_profile: Option<String>,
    pub features: BTreeMap<String, f64>,
    pub first_served_at: i64,
}

#[derive(Debug, Clone)]
pub struct PersistedActionabilityScore {
    pub candidate_id: String,
    pub source_revision: Option<String>,
    pub inference: ActionabilityInference,
}

#[derive(Debug, Clone)]
pub struct PersistedAttentionPairLabel {
    pub pair_label_id: String,
    pub inserted: bool,
    pub left: AttentionPairCandidateRef,
    pub right: AttentionPairCandidateRef,
    pub label: AttentionPairLabelKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttentionRetentionReport {
    pub principal: String,
    pub workspace: String,
    pub apply: bool,
    pub cutoff_at: Option<i64>,
    pub affected_rows: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttentionOptimizeReport {
    pub database: String,
    pub started_at: i64,
    pub completed_at: i64,
    pub analyzed_tables: Vec<String>,
    pub bytes_before: u64,
    pub bytes_after: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttentionReclaimReport {
    pub database: String,
    pub started_at: i64,
    pub completed_at: i64,
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub bytes_reclaimed: u64,
    pub table_count: usize,
    pub row_count: u64,
    pub integrity_check: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalProjectionLanePage {
    pub projection_id: String,
    pub lane: String,
    pub offset: usize,
    pub total: usize,
    pub items_json: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticExtractionWorkStatus {
    Pending,
    InFlight,
    Retry,
    Succeeded,
    Missing,
    Invalid,
    Dead,
}

impl SemanticExtractionWorkStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InFlight => "in_flight",
            Self::Retry => "retry",
            Self::Succeeded => "succeeded",
            Self::Missing => "missing",
            Self::Invalid => "invalid",
            Self::Dead => "dead",
        }
    }
}

impl FromStr for SemanticExtractionWorkStatus {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "in_flight" => Ok(Self::InFlight),
            "retry" => Ok(Self::Retry),
            "succeeded" => Ok(Self::Succeeded),
            "missing" => Ok(Self::Missing),
            "invalid" => Ok(Self::Invalid),
            "dead" => Ok(Self::Dead),
            other => anyhow::bail!("unknown semantic extraction work status: {other}"),
        }
    }
}

/// Producer contract copied onto every scheduled revision. A prompt, model,
/// profile, schema, or extractor change makes an old result ineligible.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticExtractionContract {
    pub semantic_schema_version: u32,
    pub extractor_contract: String,
    pub prompt_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ScheduleSemanticExtraction {
    pub surface: AttentionSurface,
    pub candidate_id: String,
    pub source_revision: String,
    pub source_revision_number: i64,
    pub contract: SemanticExtractionContract,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticExtractionWorkItem {
    pub work_id: String,
    pub principal: String,
    pub workspace: String,
    pub surface: AttentionSurface,
    pub candidate_id: String,
    pub source_revision: String,
    pub source_revision_number: i64,
    pub contract: SemanticExtractionContract,
    pub status: SemanticExtractionWorkStatus,
    pub attempts: u32,
    pub next_retry_at: Option<i64>,
    pub lease_owner: Option<String>,
    pub lease_expires_at: Option<i64>,
    pub last_error_code: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticExtractionQueueCounts {
    pub pending: u64,
    pub in_flight: u64,
    pub active_in_flight: u64,
    pub expired_in_flight: u64,
    pub retry: u64,
    pub succeeded: u64,
    pub missing: u64,
    pub invalid: u64,
    pub dead: u64,
    pub next_retry_at: Option<i64>,
    pub oldest_ready_at: Option<i64>,
    pub last_succeeded_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticExtractionCheckpoint {
    pub surface: AttentionSurface,
    pub cursor: Option<String>,
    pub lease_owner: Option<String>,
    pub lease_expires_at: Option<i64>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HistoricalBootstrapCheckpoint {
    pub source: String,
    pub cutoff_at: i64,
    pub cursor_at: i64,
    pub cursor_id: String,
    pub imported_count: u64,
    pub skipped_count: u64,
    pub completed: bool,
    pub updated_at: i64,
}

#[derive(Debug)]
struct PreparedCanonicalProjectionMember {
    lane: &'static str,
    position: i64,
    canonical_id: String,
    source_revision: Option<String>,
    item_digest: String,
    item_json: String,
    item_binding_json: String,
    rank_position: Option<i64>,
    rank_digest: Option<String>,
    rank_json: Option<String>,
    decision_position: Option<i64>,
    decision_digest: Option<String>,
    decision_item_json: Option<String>,
}

#[derive(Debug)]
struct PreparedCanonicalProjection {
    metadata_json: String,
    diagnostics_json: Option<String>,
    metadata_bytes: i64,
    members: Vec<PreparedCanonicalProjectionMember>,
}

#[derive(Default)]
struct CanonicalProjectionInsertStats {
    item_bodies_inserted: u64,
    item_bodies_reused: u64,
    diagnostic_bodies_inserted: u64,
    diagnostic_bodies_reused: u64,
}

struct PreparedFeatureVector {
    content_digest: String,
    features_json: String,
}

struct PreparedAttentionDecisionItem {
    item: AttentionDecisionItem,
    contracts_json: String,
    bandit_decision_json: Option<String>,
    item_json: String,
    feature: Option<PreparedFeatureVector>,
}

fn update_feature_digest_component(digest: &mut blake3::Hasher, value: &[u8]) {
    digest.update(&(value.len() as u64).to_le_bytes());
    digest.update(value);
}

#[allow(clippy::too_many_arguments)]
fn feature_vector_content_digest(
    feature_contract: &str,
    temporal_contract: &str,
    semantic_schema_version: u32,
    semantic_extractor_contract: &str,
    semantic_prompt_version: Option<&str>,
    semantic_model: Option<&str>,
    semantic_profile: Option<&str>,
    features_json: &str,
) -> String {
    let mut digest = blake3::Hasher::new();
    let semantic_schema_version = semantic_schema_version.to_string();
    for component in [
        feature_contract.as_bytes(),
        temporal_contract.as_bytes(),
        semantic_schema_version.as_bytes(),
        semantic_extractor_contract.as_bytes(),
    ] {
        update_feature_digest_component(&mut digest, component);
    }
    for component in [semantic_prompt_version, semantic_model, semantic_profile] {
        digest.update(&[u8::from(component.is_some())]);
        update_feature_digest_component(&mut digest, component.unwrap_or_default().as_bytes());
    }
    update_feature_digest_component(&mut digest, features_json.as_bytes());
    digest.finalize().to_hex().to_string()
}

fn projection_binding_map(
    values: Vec<serde_json::Value>,
    field: &str,
) -> Result<HashMap<String, (i64, String)>> {
    let mut bindings = HashMap::with_capacity(values.len());
    for (position, value) in values.into_iter().enumerate() {
        let identity = value
            .get(field)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .with_context(|| format!("canonical projection binding is missing {field}"))?
            .to_string();
        let encoded = serde_json::to_string(&value)?;
        anyhow::ensure!(
            bindings
                .insert(identity, (usize_to_i64(position), encoded))
                .is_none(),
            "canonical projection contains duplicate {field} binding"
        );
    }
    Ok(bindings)
}

fn diagnostic_digest(kind: &str, json: &str) -> String {
    blake3::hash(format!("{kind}\0{json}").as_bytes())
        .to_hex()
        .to_string()
}

/// Reject deeply nested JSON before Serde allocates or recursively drops a
/// value tree. This is intentionally a structural preflight, not a second JSON
/// parser: Serde remains the syntax authority after the bounded scan succeeds.
fn ensure_bounded_json_nesting(input: &str, label: &str) -> Result<()> {
    anyhow::ensure!(
        json_bytes_nesting_is_bounded(input.as_bytes(), MAX_RETAINED_JSON_DEPTH),
        "{label} exceeds the maximum JSON nesting depth or has malformed container structure"
    );
    Ok(())
}

fn parse_bounded_stored_json<T: DeserializeOwned>(
    input: &str,
    label: &str,
    max_bytes: usize,
) -> Result<T> {
    parse_bounded_stored_json_with_limits(input, label, max_bytes, MAX_ATTENTION_STORED_JSON_NODES)
}

fn parse_bounded_stored_json_with_limits<T: DeserializeOwned>(
    input: &str,
    label: &str,
    max_bytes: usize,
    max_nodes: usize,
) -> Result<T> {
    anyhow::ensure!(
        input.len() <= max_bytes,
        "{label} exceeds its stored JSON size limit"
    );
    ensure_bounded_json_nesting(input, label)?;
    anyhow::ensure!(
        json_bytes_nodes_are_bounded(input.as_bytes(), max_nodes),
        "{label} exceeds the stored JSON node admission limit"
    );
    serde_json::from_str(input).with_context(|| format!("parsing {label}"))
}

fn parse_bounded_json_value(input: &str, label: &str) -> Result<serde_json::Value> {
    parse_bounded_stored_json(input, label, MAX_ATTENTION_STORED_JSON_BYTES)
}

fn legacy_migration_error_code(error: &anyhow::Error) -> &'static str {
    let message = error.to_string();
    if message.contains("size limit") || message.contains("admission limit") {
        "payload_too_large"
    } else if message.contains("maximum JSON nesting depth") {
        "json_nesting_exceeded"
    } else if message.contains("parsing") {
        "invalid_json"
    } else {
        "invalid_contract"
    }
}

fn legacy_migration_payload_fingerprint(input: &str) -> String {
    // Hash once before discarding the in-memory body. The quarantine record can
    // then identify the exact rejected revision without copying private text.
    blake3::hash(input.as_bytes()).to_hex().to_string()
}

fn quarantine_legacy_migration_payload(
    tx: &rusqlite::Transaction<'_>,
    migration_kind: &str,
    source_key: &str,
    principal: &str,
    workspace: &str,
    source_fingerprint: &str,
    error_code: &str,
    payload_bytes: usize,
) -> Result<()> {
    tx.execute(
        "INSERT INTO attention_legacy_migration_quarantine ( \
            migration_kind, source_key, principal, workspace, source_fingerprint, \
            error_code, payload_bytes, quarantined_at \
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(migration_kind, source_key) DO UPDATE SET \
            principal = excluded.principal, workspace = excluded.workspace, \
            source_fingerprint = excluded.source_fingerprint, \
            error_code = excluded.error_code, payload_bytes = excluded.payload_bytes, \
            quarantined_at = excluded.quarantined_at",
        params![
            migration_kind,
            source_key,
            principal,
            workspace,
            source_fingerprint,
            error_code,
            usize_to_i64(payload_bytes),
            chrono::Utc::now().timestamp_millis(),
        ],
    )?;
    Ok(())
}

fn prepare_normalized_canonical_projection(
    projection_json: &str,
) -> Result<PreparedCanonicalProjection> {
    // Fail closed before parsing an unexpectedly large authoritative object.
    anyhow::ensure!(
        projection_json.len() <= 64 * 1024 * 1024,
        "canonical projection exceeds the 64 MiB admission limit"
    );
    let mut root = parse_bounded_json_value(
        projection_json,
        "canonical projection for normalized persistence",
    )?;
    let root_object = root
        .as_object_mut()
        .context("canonical projection root is not an object")?;
    let lanes = root_object
        .remove("lanes")
        .context("canonical projection has no lanes")?;
    let diagnostics = root_object.remove("diagnostics");

    let (diagnostics_json, mut rank_bindings, mut decision_bindings) =
        if let Some(mut diagnostics) = diagnostics {
            let diagnostics_object = diagnostics
                .as_object_mut()
                .context("canonical projection diagnostics is not an object")?;
            let ranks = match diagnostics_object.remove("ranks") {
                Some(serde_json::Value::Array(values)) => {
                    diagnostics_object
                        .insert("ranks".to_string(), serde_json::Value::Array(Vec::new()));
                    values
                },
                Some(value) => {
                    diagnostics_object.insert("ranks".to_string(), value);
                    Vec::new()
                },
                None => Vec::new(),
            };
            let decision_items = match diagnostics_object.remove("decision_items") {
                Some(serde_json::Value::Array(values)) => {
                    diagnostics_object.insert(
                        "decision_items".to_string(),
                        serde_json::Value::Array(Vec::new()),
                    );
                    values
                },
                Some(value) => {
                    diagnostics_object.insert("decision_items".to_string(), value);
                    Vec::new()
                },
                None => Vec::new(),
            };
            (
                Some(serde_json::to_string(&diagnostics)?),
                projection_binding_map(ranks, "candidate_id")?,
                projection_binding_map(decision_items, "candidate_id")?,
            )
        } else {
            (None, HashMap::new(), HashMap::new())
        };

    // Move the object out of `lanes`; retaining it while cloning every item
    // doubled the full projection tree at peak.
    let mut lane_object = match lanes {
        serde_json::Value::Object(object) => object,
        _ => anyhow::bail!("canonical projection lanes is not an object"),
    };
    let mut members = Vec::new();
    for (lane_key, lane) in [
        ("follow_up", "follow_up"),
        ("worth_a_look", "worth_a_look"),
        ("non_surfaced", "non_surfaced"),
    ] {
        let items = match lane_object.remove(lane_key) {
            Some(serde_json::Value::Array(values)) => values,
            _ => anyhow::bail!("canonical projection lane {lane_key} is missing"),
        };
        for (position, item) in items.into_iter().enumerate() {
            anyhow::ensure!(
                members.len() < MAX_NORMALIZED_PROJECTION_ITEMS,
                "canonical projection exceeds normalized item limit"
            );
            let canonical_id = item
                .get("canonical_id")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .context("canonical projection item has no canonical_id")?
                .to_string();
            let source_revision = item
                .get("source_revision")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let mut item_body = item;
            let body_object = item_body
                .as_object_mut()
                .context("canonical projection item is not an object")?;
            let mut binding = serde_json::Map::new();
            for field in [
                "served_lane",
                "learned_lane",
                "route_reason",
                "route_applied",
                "group",
            ] {
                if let Some(value) = body_object.remove(field) {
                    binding.insert(field.to_string(), value);
                }
            }
            anyhow::ensure!(
                binding.len() == 5,
                "canonical projection item routing/group binding is incomplete"
            );
            let item_json = serde_json::to_string(&item_body)?;
            let item_binding_json = serde_json::to_string(&serde_json::Value::Object(binding))?;
            let item_digest = blake3::hash(item_json.as_bytes()).to_hex().to_string();
            let (rank_position, rank_json) = rank_bindings
                .remove(&canonical_id)
                .map_or((None, None), |(position, json)| {
                    (Some(position), Some(json))
                });
            let (decision_position, decision_item_json) = decision_bindings
                .remove(&canonical_id)
                .map_or((None, None), |(position, json)| {
                    (Some(position), Some(json))
                });
            let rank_digest = rank_json
                .as_deref()
                .map(|json| diagnostic_digest("rank", json));
            let decision_digest = decision_item_json
                .as_deref()
                .map(|json| diagnostic_digest("decision", json));
            members.push(PreparedCanonicalProjectionMember {
                lane,
                position: usize_to_i64(position),
                canonical_id,
                source_revision,
                item_digest,
                item_json,
                item_binding_json,
                rank_position,
                rank_digest,
                rank_json,
                decision_position,
                decision_digest,
                decision_item_json,
            });
        }
    }
    anyhow::ensure!(
        lane_object.is_empty(),
        "canonical projection contains unsupported lanes"
    );
    anyhow::ensure!(
        rank_bindings.is_empty() && decision_bindings.is_empty(),
        "canonical projection diagnostics reference non-materialized items"
    );
    let metadata_json = serde_json::to_string(&root)?;
    let metadata_bytes =
        usize_to_i64(metadata_json.len() + diagnostics_json.as_ref().map_or(0, String::len));
    Ok(PreparedCanonicalProjection {
        metadata_json,
        diagnostics_json,
        metadata_bytes,
        members,
    })
}

fn insert_normalized_canonical_projection(
    tx: &rusqlite::Transaction<'_>,
    projection_id: &str,
    created_at: i64,
    prepared: &PreparedCanonicalProjection,
) -> Result<CanonicalProjectionInsertStats> {
    let mut stats = CanonicalProjectionInsertStats::default();
    for member in &prepared.members {
        let inserted = tx.execute(
            "INSERT INTO attention_canonical_item_revisions ( \
                item_digest, canonical_id, source_revision, item_json, size_bytes, created_at \
             ) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT(item_digest) DO NOTHING",
            params![
                member.item_digest,
                member.canonical_id,
                member.source_revision,
                member.item_json,
                usize_to_i64(member.item_json.len()),
                created_at,
            ],
        )?;
        if inserted == 1 {
            stats.item_bodies_inserted = stats.item_bodies_inserted.saturating_add(1);
        } else {
            stats.item_bodies_reused = stats.item_bodies_reused.saturating_add(1);
            let stored_matches: i64 = tx.query_row(
                "SELECT COUNT(*) FROM attention_canonical_item_revisions \
                 WHERE item_digest = ? AND canonical_id = ? AND source_revision IS ? \
                   AND item_json = ? AND size_bytes = ?",
                params![
                    member.item_digest,
                    member.canonical_id,
                    member.source_revision,
                    member.item_json,
                    usize_to_i64(member.item_json.len()),
                ],
                |row| row.get(0),
            )?;
            anyhow::ensure!(
                stored_matches == 1,
                "canonical projection item digest collision"
            );
        }
        for (kind, digest, json) in [
            (
                "rank",
                member.rank_digest.as_deref(),
                member.rank_json.as_deref(),
            ),
            (
                "decision",
                member.decision_digest.as_deref(),
                member.decision_item_json.as_deref(),
            ),
        ] {
            if let (Some(digest), Some(json)) = (digest, json) {
                let inserted = tx.execute(
                    "INSERT INTO attention_canonical_diagnostic_revisions ( \
                        diagnostic_digest, diagnostic_kind, diagnostic_json, size_bytes, created_at \
                     ) VALUES (?, ?, ?, ?, ?) ON CONFLICT(diagnostic_digest) DO NOTHING",
                    params![digest, kind, json, usize_to_i64(json.len()), created_at],
                )?;
                if inserted == 1 {
                    stats.diagnostic_bodies_inserted =
                        stats.diagnostic_bodies_inserted.saturating_add(1);
                } else {
                    stats.diagnostic_bodies_reused =
                        stats.diagnostic_bodies_reused.saturating_add(1);
                    let stored_matches: i64 = tx.query_row(
                        "SELECT COUNT(*) FROM attention_canonical_diagnostic_revisions \
                         WHERE diagnostic_digest = ? AND diagnostic_kind = ? \
                           AND diagnostic_json = ? AND size_bytes = ?",
                        params![digest, kind, json, usize_to_i64(json.len())],
                        |row| row.get(0),
                    )?;
                    anyhow::ensure!(
                        stored_matches == 1,
                        "canonical projection diagnostic digest collision"
                    );
                }
            }
        }
    }
    tx.execute(
        "INSERT INTO attention_canonical_projection_manifests ( \
            projection_id, schema_version, metadata_json, diagnostics_json, item_count, \
            metadata_bytes, created_at, last_materialized_at \
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            projection_id,
            CURRENT_CANONICAL_PROJECTION_SCHEMA_VERSION,
            prepared.metadata_json,
            prepared.diagnostics_json,
            usize_to_i64(prepared.members.len()),
            prepared.metadata_bytes,
            created_at,
            created_at,
        ],
    )?;
    for member in &prepared.members {
        tx.execute(
            "INSERT INTO attention_canonical_projection_members ( \
                projection_id, lane, position, canonical_id, item_digest, item_binding_json, rank_position, \
                rank_digest, rank_json, decision_position, decision_digest, decision_item_json \
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, NULL)",
            params![
                projection_id,
                member.lane,
                member.position,
                member.canonical_id,
                member.item_digest,
                member.item_binding_json,
                member.rank_position,
                member.rank_digest,
                member.decision_position,
                member.decision_digest,
            ],
        )?;
    }
    Ok(stats)
}

fn materialize_canonical_item_value(
    body_json: &str,
    binding_json: Option<&str>,
) -> Result<serde_json::Value> {
    let mut body = parse_bounded_json_value(body_json, "normalized canonical item body")?;
    if let Some(binding_json) = binding_json.filter(|value| !value.is_empty()) {
        let binding = parse_bounded_json_value(binding_json, "normalized canonical item binding")?;
        let body_object = body
            .as_object_mut()
            .context("normalized canonical item body is not an object")?;
        let binding_object = match binding {
            serde_json::Value::Object(object) => object,
            _ => anyhow::bail!("normalized canonical item binding is not an object"),
        };
        for (field, value) in binding_object {
            anyhow::ensure!(
                !body_object.contains_key(&field),
                "normalized canonical item binding overlaps immutable body"
            );
            body_object.insert(field, value);
        }
    }
    Ok(body)
}

type CanonicalProjectionMemberRow = (
    String,
    i64,
    String,
    Option<String>,
    Option<i64>,
    Option<String>,
    Option<i64>,
    Option<String>,
);

enum CanonicalProjectionMaterialization {
    Legacy(String),
    Normalized {
        metadata_json: String,
        diagnostics_json: Option<String>,
        expected_items: i64,
        members: Vec<CanonicalProjectionMemberRow>,
    },
}

/// Copy one projection revision from SQLite under a consistent WAL snapshot.
/// JSON parsing and compatibility serialization deliberately happen after the
/// caller releases its scarce read-pool permit.
fn load_canonical_projection_materialization(
    conn: &Connection,
    projection_id: &str,
    schema_version: i64,
    legacy_json: String,
) -> Result<CanonicalProjectionMaterialization> {
    if schema_version < NORMALIZED_CANONICAL_PROJECTION_SCHEMA_VERSION {
        anyhow::ensure!(
            !legacy_json.is_empty(),
            "legacy canonical projection body is empty"
        );
        anyhow::ensure!(
            legacy_json.len() <= MAX_ATTENTION_STORED_JSON_BYTES,
            "legacy canonical projection body exceeds the 64 MiB stored JSON limit"
        );
        return Ok(CanonicalProjectionMaterialization::Legacy(legacy_json));
    }
    anyhow::ensure!(
        schema_version <= CURRENT_CANONICAL_PROJECTION_SCHEMA_VERSION,
        "canonical projection schema is newer than this runtime"
    );
    // Retention can remove an unreferenced projection concurrently. Keep the
    // manifest, members, and content-addressed bodies on one WAL snapshot so a
    // compatibility response can never mix two database revisions.
    let snapshot = conn
        .unchecked_transaction()
        .context("opening canonical projection read snapshot")?;
    let (manifest_schema, metadata_json, diagnostics_json, expected_items): (
        i64,
        String,
        Option<String>,
        i64,
    ) = snapshot
        .query_row(
            "SELECT schema_version, metadata_json, diagnostics_json, item_count \
             FROM attention_canonical_projection_manifests WHERE projection_id = ?",
            params![projection_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .context("normalized canonical projection manifest is missing")?;
    anyhow::ensure!(
        manifest_schema == schema_version,
        "canonical projection anchor/manifest schema mismatch"
    );
    anyhow::ensure!(
        (0..=usize_to_i64(MAX_NORMALIZED_PROJECTION_ITEMS)).contains(&expected_items),
        "normalized canonical projection item count is invalid"
    );
    let mut statement = snapshot.prepare(
        "SELECT m.lane, m.position, i.item_json, m.item_binding_json, m.rank_position, \
                COALESCE(m.rank_json, rank_body.diagnostic_json), \
                m.decision_position, \
                COALESCE(m.decision_item_json, decision_body.diagnostic_json) \
         FROM attention_canonical_projection_members m \
         JOIN attention_canonical_item_revisions i ON i.item_digest = m.item_digest \
         LEFT JOIN attention_canonical_diagnostic_revisions rank_body \
           ON rank_body.diagnostic_digest = m.rank_digest \
          AND rank_body.diagnostic_kind = 'rank' \
         LEFT JOIN attention_canonical_diagnostic_revisions decision_body \
           ON decision_body.diagnostic_digest = m.decision_digest \
          AND decision_body.diagnostic_kind = 'decision' \
         WHERE m.projection_id = ? \
         ORDER BY CASE m.lane WHEN 'follow_up' THEN 0 WHEN 'worth_a_look' THEN 1 ELSE 2 END, \
                  m.position",
    )?;
    let members = statement
        .query_map(params![projection_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(statement);
    anyhow::ensure!(
        usize_to_i64(members.len()) == expected_items,
        "normalized canonical projection membership is incomplete"
    );
    snapshot
        .commit()
        .context("closing canonical projection read snapshot")?;
    Ok(CanonicalProjectionMaterialization::Normalized {
        metadata_json,
        diagnostics_json,
        expected_items,
        members,
    })
}

fn materialize_canonical_projection_json(
    materialization: CanonicalProjectionMaterialization,
) -> Result<String> {
    let (metadata_json, diagnostics_json, expected_items, members) = match materialization {
        CanonicalProjectionMaterialization::Legacy(legacy_json) => {
            ensure_bounded_json_nesting(&legacy_json, "legacy canonical projection body")?;
            return Ok(legacy_json);
        },
        CanonicalProjectionMaterialization::Normalized {
            metadata_json,
            diagnostics_json,
            expected_items,
            members,
        } => (metadata_json, diagnostics_json, expected_items, members),
    };
    let mut root =
        parse_bounded_json_value(&metadata_json, "normalized canonical projection metadata")?;
    let root_object = root
        .as_object_mut()
        .context("normalized canonical projection metadata is not an object")?;
    let mut lane_values: HashMap<&'static str, Vec<serde_json::Value>> = HashMap::from([
        ("follow_up", Vec::new()),
        ("worth_a_look", Vec::new()),
        ("non_surfaced", Vec::new()),
    ]);
    let mut ranks = Vec::<(i64, serde_json::Value)>::new();
    let mut decisions = Vec::<(i64, serde_json::Value)>::new();
    for row in members {
        let (
            lane,
            _position,
            item_json,
            item_binding_json,
            rank_position,
            rank_json,
            decision_position,
            decision_json,
        ) = row;
        let lane = match lane.as_str() {
            "follow_up" => "follow_up",
            "worth_a_look" => "worth_a_look",
            "non_surfaced" => "non_surfaced",
            _ => anyhow::bail!("normalized canonical projection lane is invalid"),
        };
        anyhow::ensure!(
            rank_position.is_some() == rank_json.is_some()
                && decision_position.is_some() == decision_json.is_some(),
            "normalized canonical projection diagnostic body is missing"
        );
        lane_values
            .get_mut(lane)
            .expect("all normalized lanes are initialized")
            .push(materialize_canonical_item_value(
                &item_json,
                item_binding_json.as_deref(),
            )?);
        if let (Some(position), Some(json)) = (rank_position, rank_json) {
            ranks.push((
                position,
                parse_bounded_json_value(&json, "normalized canonical rank diagnostic")?,
            ));
        }
        if let (Some(position), Some(json)) = (decision_position, decision_json) {
            decisions.push((
                position,
                parse_bounded_json_value(&json, "normalized canonical decision diagnostic")?,
            ));
        }
    }
    anyhow::ensure!(
        usize_to_i64(lane_values.values().map(Vec::len).sum::<usize>()) == expected_items,
        "normalized canonical projection membership is incomplete"
    );
    anyhow::ensure!(
        diagnostics_json.is_some() || (ranks.is_empty() && decisions.is_empty()),
        "normalized canonical projection diagnostics metadata is missing"
    );
    anyhow::ensure!(
        ranks
            .iter()
            .map(|(position, _)| position)
            .collect::<HashSet<_>>()
            .len()
            == ranks.len()
            && decisions
                .iter()
                .map(|(position, _)| position)
                .collect::<HashSet<_>>()
                .len()
                == decisions.len(),
        "normalized canonical projection diagnostic positions are not unique"
    );
    let lanes = serde_json::json!({
        "follow_up": lane_values.remove("follow_up").unwrap_or_default(),
        "worth_a_look": lane_values.remove("worth_a_look").unwrap_or_default(),
        "non_surfaced": lane_values.remove("non_surfaced").unwrap_or_default(),
    });
    root_object.insert("lanes".to_string(), lanes);
    if let Some(diagnostics_json) = diagnostics_json {
        let mut diagnostics = parse_bounded_json_value(
            &diagnostics_json,
            "normalized canonical projection diagnostics",
        )?;
        let object = diagnostics
            .as_object_mut()
            .context("normalized canonical diagnostics is not an object")?;
        ranks.sort_by_key(|(position, _)| *position);
        decisions.sort_by_key(|(position, _)| *position);
        if !ranks.is_empty() {
            object.insert(
                "ranks".to_string(),
                serde_json::Value::Array(ranks.into_iter().map(|(_, value)| value).collect()),
            );
        }
        if !decisions.is_empty() {
            object.insert(
                "decision_items".to_string(),
                serde_json::Value::Array(decisions.into_iter().map(|(_, value)| value).collect()),
            );
        }
        root_object.insert("diagnostics".to_string(), diagnostics);
    }
    serde_json::to_string(&root).context("serializing normalized canonical projection")
}

fn read_frozen_delivery_page_items(
    conn: &Connection,
    delivery_id: &str,
    projection_id: &str,
) -> Result<Vec<FrozenAttentionDeliveryItem>> {
    let mut statement = conn.prepare(
        "SELECT i.position, i.candidate_id, i.source_revision, i.root_policy_propensity, \
                i.exposure_token, i.item_json, body.item_json, member.item_binding_json \
         FROM attention_delivery_page_items p \
         JOIN attention_delivery_decision_items i \
           ON i.decision_id = p.decision_id AND i.position = p.position \
         LEFT JOIN attention_canonical_projection_members member \
           ON member.projection_id = ? AND member.canonical_id = i.candidate_id \
         LEFT JOIN attention_canonical_item_revisions body \
           ON body.item_digest = member.item_digest \
         WHERE p.delivery_id = ? ORDER BY i.position",
    )?;
    let rows = statement
        .query_map(params![projection_id, delivery_id], |row| {
            Ok((
                row.get::<_, i64>(0)?.max(0) as usize,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, f64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let items = rows
        .into_iter()
        .map(
            |(
                position,
                candidate_id,
                source_revision,
                root_policy_propensity,
                exposure_token,
                frozen_json,
                body_json,
                binding_json,
            )|
             -> Result<_> {
                let item_json = if frozen_json.is_empty() {
                    serde_json::to_string(&materialize_canonical_item_value(
                        body_json
                            .as_deref()
                            .context("normalized frozen delivery item body is missing")?,
                        binding_json.as_deref(),
                    )?)?
                } else {
                    ensure_bounded_json_nesting(
                        &frozen_json,
                        "legacy frozen attention delivery item",
                    )?;
                    frozen_json
                };
                Ok(FrozenAttentionDeliveryItem {
                    position,
                    candidate_id,
                    source_revision,
                    root_policy_propensity,
                    conditional_delivery_propensity: 1.0,
                    exposure_token,
                    item_json,
                })
            },
        )
        .collect::<Result<Vec<_>>>()?;
    anyhow::ensure!(
        items.iter().all(|item| !item.item_json.is_empty()),
        "frozen attention delivery page has no canonical item body"
    );
    Ok(items)
}

fn canonical_projection_lane_size_on(
    conn: &Connection,
    projection_id: &str,
    lane: &str,
) -> Result<Option<usize>> {
    let schema_version: i64 = conn.query_row(
        "SELECT schema_version FROM attention_canonical_projections WHERE projection_id = ?",
        params![projection_id],
        |row| row.get(0),
    )?;
    if schema_version < NORMALIZED_CANONICAL_PROJECTION_SCHEMA_VERSION {
        return Ok(None);
    }
    anyhow::ensure!(
        schema_version <= CURRENT_CANONICAL_PROJECTION_SCHEMA_VERSION,
        "canonical projection schema is newer than this runtime"
    );
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM attention_canonical_projection_members \
         WHERE projection_id = ? AND lane = ?",
        params![projection_id, lane],
        |row| row.get(0),
    )?;
    Ok(Some(usize::try_from(total.max(0)).unwrap_or(usize::MAX)))
}

impl AttentionLearningStore {
    pub fn open(base_root: &Path) -> Result<Self> {
        std::fs::create_dir_all(base_root).with_context(|| {
            format!(
                "creating attention learning store directory: {}",
                base_root.display()
            )
        })?;
        let path = crate::magician_v2::database_owners::host_database_path(
            base_root,
            crate::magician_v2::database_owners::DatabaseOwner::AttentionLearning,
        );
        let process_lease = Arc::new(AttentionProcessLease::open(&path)?);
        let bootstrap_guard = ATTENTION_SCHEMA_BOOTSTRAP_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let conn = Connection::open(&path)
            .with_context(|| format!("opening attention learning store at {}", path.display()))?;
        configure_attention_connection(&conn, false)?;
        conn.query_row("PRAGMA journal_mode=WAL", [], |_row| Ok(()))
            .context("enabling WAL journal mode on attention learning store")?;
        // Before the bootstrap DDL, because rebuilding a table drops its
        // indexes and the bootstrap's `CREATE INDEX IF NOT EXISTS` is what
        // puts them back. On a fresh store the table does not exist yet and
        // this is a no-op.
        ensure_feature_snapshot_content_digest(&conn)
            .context("re-keying attention candidate feature snapshots by content")?;
        conn.execute_batch(BOOTSTRAP_DDL)
            .context("bootstrapping attention learning store")?;
        ensure_actionability_score_input_digest(&conn)
            .context("migrating actionability score input digest")?;
        ensure_embedding_bind_work_queue_columns(&conn)
            .context("migrating attention embedding bind repair queue")?;
        ensure_decision_item_served_rank(&conn)
            .context("migrating attention decision served rank")?;
        ensure_bandit_decision_columns(&conn)
            .context("migrating attention decision bandit metadata")?;
        ensure_delivery_impression_columns(&conn)
            .context("migrating delivery-bound impression metadata")?;
        ensure_scope_health_counter_schema(&conn)
            .context("restoring attention scope health counter triggers")?;
        ensure_scope_health_counter_backfill(&conn)
            .context("backfilling attention scope health counters")?;
        ensure_delivery_source_generation_token(&conn)
            .context("migrating delivery source-generation bindings")?;
        ensure_projection_reference_columns(&conn)
            .context("migrating durable attention projection references")?;
        ensure_normalized_projection_columns(&conn)
            .context("migrating normalized attention diagnostic bindings")?;
        ensure_normalized_feature_vector_columns(&conn)
            .context("migrating normalized attention feature producer bindings")?;
        queue_recovery::ensure_schema(&conn)?;
        history_prune::ensure_indexes(&conn)
            .context("indexing canonical history reference checks")?;
        drop(bootstrap_guard);
        let reads = AttentionReadPool::open(&path, ATTENTION_READ_POOL_SIZE)?;
        Ok(Self {
            conn: Arc::new(AttentionWriterConnection::new(conn)),
            reads: Arc::new(reads),
            path: Arc::new(path),
            process_lease,
        })
    }

    pub fn connection_telemetry(&self) -> AttentionConnectionTelemetry {
        let mut telemetry = self.reads.telemetry();
        telemetry.writer_wait_count = self.conn.wait_count.load(Ordering::Relaxed);
        telemetry.writer_wait_micros = self.conn.wait_micros.load(Ordering::Relaxed);
        telemetry
    }

    pub fn database_path(&self) -> &Path {
        self.path.as_path()
    }

    /// Refresh SQLite planner statistics without removing logical history or
    /// requiring a reader drain. This is the safe online maintenance action
    /// exposed by Storage Governance.
    pub async fn optimize_database(&self) -> Result<AttentionOptimizeReport> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let started_at = chrono::Utc::now().timestamp_millis();
            let bytes_before = attention_file_size(store.path.as_path())?;
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            conn.execute_batch(
                "ANALYZE attention_decision_items; \
                 ANALYZE attention_impressions; \
                 ANALYZE attention_canonical_projection_members; \
                 PRAGMA optimize;",
            )
            .context("optimizing attention learning SQLite planner statistics")?;
            drop(conn);
            Ok(AttentionOptimizeReport {
                database: store.path.display().to_string(),
                started_at,
                completed_at: chrono::Utc::now().timestamp_millis(),
                analyzed_tables: vec![
                    "attention_decision_items".to_string(),
                    "attention_impressions".to_string(),
                    "attention_canonical_projection_members".to_string(),
                ],
                bytes_before,
                bytes_after: attention_file_size(store.path.as_path())?,
            })
        })
        .await
        .context("attention database optimize task panicked")?
    }

    /// Rebuild the physical SQLite file while preserving every logical row.
    /// The writer is held throughout the rebuild, verification, and swap.
    /// Reads remain online until the final replacement window; then new reads
    /// fail fast and in-flight readers are drained. On Unix the verified
    /// candidate atomically replaces the original while a
    /// hard-link rollback copy keeps the previous inode recoverable until all
    /// new connections are healthy.
    pub async fn reclaim_database_space(&self) -> Result<AttentionReclaimReport> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let started_at = chrono::Utc::now().timestamp_millis();
            let path = store.path.as_path();
            let mut writer = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // This must be acquired before VACUUM starts. The candidate and
            // its verification then observe a database on which no supported
            // external process can commit, and no process can retain an old
            // SQLite handle across the eventual inode replacement.
            let mut process_lease = store
                .process_lease
                .acquire_exclusive()
                .context("draining other attention-learning processes")?;
            let bytes_before = attention_file_size(path)?;
            anyhow::ensure!(
                bytes_before > 0,
                "attention learning database does not exist"
            );
            let suffix = uuid::Uuid::new_v4().simple().to_string();
            let candidate = path.with_file_name(format!("attention_learning.rebuild-{suffix}.db"));
            let backup = path.with_file_name(format!("attention_learning.rollback-{suffix}.db"));
            anyhow::ensure!(!candidate.exists() && !backup.exists(), "attention rebuild paths already exist");

            let candidate_sql = candidate
                .to_str()
                .context("attention rebuild path is not valid UTF-8")?;
            if let Err(error) = writer.execute("VACUUM INTO ?", params![candidate_sql]) {
                let _ = std::fs::remove_file(&candidate);
                return Err(error).context("rebuilding attention database with VACUUM INTO");
            }
            let (table_count, row_count) = match verify_attention_rebuild(&writer, &candidate) {
                Ok(value) => value,
                Err(error) => {
                    let _ = std::fs::remove_file(&candidate);
                    return Err(error).context("verifying rebuilt attention database");
                },
            };
            if let Err(error) = sync_regular_file(&candidate) {
                let _ = std::fs::remove_file(&candidate);
                return Err(error);
            }

            // VACUUM and verification hold the only in-process writer but do
            // not block ordinary read-pool traffic. Enter maintenance only for
            // the final checkpoint and atomic replacement, keeping the visible
            // drain window brief even for a multi-gigabyte database.
            let mut readers = match store.reads.begin_maintenance(path) {
                Ok(readers) => readers,
                Err(error) => {
                    let _ = std::fs::remove_file(&candidate);
                    return Err(error).context("draining attention readers for replacement");
                },
            };
            // No connection may retain the old WAL identity across the file
            // replacement. The source is fully checkpointed before the idle
            // read handles are closed and journal mode is temporarily changed.
            if let Err(error) = writer.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
                let _ = std::fs::remove_file(&candidate);
                return Err(error).context("checkpointing attention WAL before replacement");
            }
            readers.close_connections();
            let mut swapped = false;
            let swap_result = (|| -> Result<()> {
                let journal_mode: String = writer
                    .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
                    .context("quiescing attention WAL before replacement")?;
                anyhow::ensure!(
                    journal_mode.eq_ignore_ascii_case("delete"),
                    "attention database did not leave WAL mode"
                );
                remove_sqlite_companions(path)?;
                std::fs::hard_link(path, &backup).with_context(|| {
                    format!("creating attention rollback link at {}", backup.display())
                })?;
                sync_parent_directory(path)?;
                atomically_replace_file(&candidate, path)?;
                swapped = true;
                sync_parent_directory(path)?;
                Ok(())
            })();
            if let Err(error) = swap_result {
                let rollback = if swapped {
                    remove_sqlite_companions(path)
                        .and_then(|()| atomically_replace_file(&backup, path))
                        .and_then(|()| sync_parent_directory(path))
                        .and_then(|()| {
                            writer
                                .query_row("PRAGMA journal_mode=WAL", [], |_row| Ok(()))
                                .context("restoring attention WAL mode after swap rollback")
                        })
                        .and_then(|()| readers.reopen(path))
                } else {
                    let _ = std::fs::remove_file(&backup);
                    writer
                        .query_row("PRAGMA journal_mode=WAL", [], |_row| Ok(()))
                        .context("restoring attention WAL mode after aborted swap")
                        .and_then(|()| readers.reopen(path))
                };
                let _ = std::fs::remove_file(&candidate);
                return match rollback {
                    Ok(()) => Err(error),
                    Err(rollback_error) => Err(anyhow::anyhow!(
                        "attention database replacement failed: {error:#}; rollback also failed: {rollback_error:#}"
                    )),
                };
            }

            let replacement = (|| -> Result<Connection> {
                let replacement = open_attention_writer(path)?;
                let integrity: String =
                    replacement.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
                anyhow::ensure!(integrity == "ok", "installed attention database failed integrity_check: {integrity}");
                readers.reopen(path)?;
                Ok(replacement)
            })();

            let replacement = match replacement {
                Ok(replacement) => replacement,
                Err(error) => {
                    readers.close_connections();
                    let rollback = remove_sqlite_companions(path)
                        .and_then(|()| atomically_replace_file(&backup, path))
                        .and_then(|()| sync_parent_directory(path))
                        .and_then(|()| {
                            writer
                                .query_row("PRAGMA journal_mode=WAL", [], |_row| Ok(()))
                                .context("restoring attention WAL mode after rollback")
                        })
                        .and_then(|()| readers.reopen(path));
                    let _ = std::fs::remove_file(&candidate);
                    return match rollback {
                        Ok(()) => Err(error).context("installing rebuilt attention database; original restored"),
                        Err(rollback_error) => Err(anyhow::anyhow!(
                            "installing rebuilt attention database failed: {error:#}; rollback also failed: {rollback_error:#}"
                        )),
                    };
                },
            };

            let old_writer = std::mem::replace(&mut *writer, replacement);
            drop(old_writer);
            if let Err(error) = std::fs::remove_file(&backup) {
                tracing::warn!(
                    error = %error,
                    path = %backup.display(),
                    "verified attention rebuild left its rollback link for later cleanup"
                );
            } else if let Err(error) = sync_parent_directory(path) {
                tracing::warn!(
                    error = %error,
                    path = %path.display(),
                    "attention rebuild succeeded but rollback-link cleanup was not directory-synced"
                );
            }
            let bytes_after = attention_file_size(path)?;
            let report = AttentionReclaimReport {
                database: path.display().to_string(),
                started_at,
                completed_at: chrono::Utc::now().timestamp_millis(),
                bytes_before,
                bytes_after,
                bytes_reclaimed: bytes_before.saturating_sub(bytes_after),
                table_count,
                row_count,
                integrity_check: "ok".to_string(),
            };
            process_lease
                .downgrade()
                .context("restoring shared attention process ownership")?;
            Ok(report)
        })
        .await
        .context("attention database reclaim task panicked")?
    }

    pub async fn list_scopes(&self) -> Result<Vec<(String, String)>> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("list_scopes")?;
            let mut statement = conn.prepare(
                "SELECT principal, workspace FROM attention_outcomes \
                 UNION SELECT principal, workspace FROM attention_historical_bootstrap_checkpoints \
                 UNION SELECT principal, workspace FROM attention_candidate_scores \
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
        .context("attention learning scope list task panicked")?
    }

    /// Persist the first-run migration boundary before live producers start.
    /// Existing cutoffs are immutable across rebuilds and restarts.
    pub async fn initialize_historical_bootstrap(
        &self,
        principal: &str,
        workspace: &str,
        sources: &[&str],
        cutoff_at: i64,
    ) -> Result<()> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let sources = sources
            .iter()
            .map(|source| (*source).to_string())
            .collect::<Vec<_>>();
        tokio::task::spawn_blocking(move || {
            let mut conn = store.conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            for source in sources {
                tx.execute(
                    "INSERT INTO attention_historical_bootstrap_checkpoints (principal, workspace, source, cutoff_at, updated_at) VALUES (?, ?, ?, ?, ?) ON CONFLICT(principal, workspace, source) DO NOTHING",
                    params![principal, workspace, source, cutoff_at, cutoff_at],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
        .context("attention historical bootstrap initialization task panicked")?
    }

    pub async fn historical_bootstrap_checkpoint(
        &self,
        principal: &str,
        workspace: &str,
        source: &str,
    ) -> Result<Option<HistoricalBootstrapCheckpoint>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let source = source.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("historical_bootstrap_checkpoint")?;
            conn.query_row(
                "SELECT source, cutoff_at, cursor_at, cursor_id, imported_count, skipped_count, completed, updated_at FROM attention_historical_bootstrap_checkpoints WHERE principal = ? AND workspace = ? AND source = ?",
                params![principal, workspace, source],
                |row| {
                    Ok(HistoricalBootstrapCheckpoint {
                        source: row.get(0)?,
                        cutoff_at: row.get(1)?,
                        cursor_at: row.get(2)?,
                        cursor_id: row.get(3)?,
                        imported_count: row.get::<_, i64>(4)?.max(0) as u64,
                        skipped_count: row.get::<_, i64>(5)?.max(0) as u64,
                        completed: row.get::<_, i64>(6)? != 0,
                        updated_at: row.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
        })
        .await
        .context("attention historical bootstrap checkpoint read task panicked")?
    }

    /// Advance only monotonically. Concurrent process startup may replay a
    /// batch, but cannot move a durable cursor backwards.
    #[allow(clippy::too_many_arguments)]
    pub async fn advance_historical_bootstrap_checkpoint(
        &self,
        principal: &str,
        workspace: &str,
        source: &str,
        cursor_at: i64,
        cursor_id: &str,
        imported_delta: u64,
        skipped_delta: u64,
        completed: bool,
        updated_at: i64,
    ) -> Result<()> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let source = source.to_string();
        let cursor_id = cursor_id.to_string();
        tokio::task::spawn_blocking(move || {
            let mut conn = store.conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let current = tx
                .query_row(
                    "SELECT cursor_at, cursor_id, completed FROM attention_historical_bootstrap_checkpoints WHERE principal = ? AND workspace = ? AND source = ?",
                    params![principal, workspace, source],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)? != 0)),
                )
                .optional()?;
            let Some((current_at, current_id, already_completed)) = current else {
                anyhow::bail!("historical bootstrap checkpoint is not initialized")
            };
            let advances = cursor_at > current_at || (cursor_at == current_at && cursor_id > current_id);
            if advances {
                tx.execute(
                    "UPDATE attention_historical_bootstrap_checkpoints SET cursor_at = ?, cursor_id = ?, imported_count = imported_count + ?, skipped_count = skipped_count + ?, completed = CASE WHEN ? THEN 1 ELSE completed END, updated_at = ? WHERE principal = ? AND workspace = ? AND source = ?",
                    params![
                        cursor_at,
                        cursor_id,
                        i64::try_from(imported_delta).unwrap_or(i64::MAX),
                        i64::try_from(skipped_delta).unwrap_or(i64::MAX),
                        completed,
                        updated_at,
                        principal,
                        workspace,
                        source,
                    ],
                )?;
            } else if completed && !already_completed {
                tx.execute(
                    "UPDATE attention_historical_bootstrap_checkpoints SET completed = 1, updated_at = ? WHERE principal = ? AND workspace = ? AND source = ?",
                    params![updated_at, principal, workspace, source],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
        .context("attention historical bootstrap checkpoint update task panicked")?
    }

    pub async fn schedule_rank_recompute(
        &self,
        principal: &str,
        workspace: &str,
        request: &ScheduleAttentionRankRecompute,
        now: i64,
    ) -> Result<AttentionRankRecomputeJob> {
        anyhow::ensure!(
            !principal.trim().is_empty(),
            "rank recompute principal is empty"
        );
        anyhow::ensure!(
            !workspace.trim().is_empty(),
            "rank recompute workspace is empty"
        );
        anyhow::ensure!(
            !request.outcome_id.trim().is_empty(),
            "rank recompute outcome_id is empty"
        );
        anyhow::ensure!(
            !request.canonical_candidate_id.trim().is_empty(),
            "rank recompute canonical candidate is empty"
        );
        anyhow::ensure!(
            !request.raw_candidate_id.trim().is_empty(),
            "rank recompute raw candidate is empty"
        );
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let request = request.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let outcome_projection_id = tx.query_row(
                "SELECT projection_id FROM attention_outcomes WHERE outcome_id = ? AND principal = ? AND workspace = ? AND surface = ? AND candidate_id = ? AND source_revision IS ? AND outcome = ?",
                params![request.outcome_id, principal, workspace, request.origin_surface.as_str(), request.raw_candidate_id, request.source_revision, request.outcome.as_str()],
                |row| row.get::<_, Option<String>>(0),
            ).optional()?;
            let Some(outcome_projection_id) = outcome_projection_id else {
                anyhow::bail!("rank recompute outcome binding mismatch")
            };
            // Normalize attribution through the immutable decision/delivery
            // ledgers before anything is exposed on the durable job. Supplied
            // identifiers are claims, not validated bindings.
            let delivered_binding = if let Some(decision_id) = request.decision_id.as_deref() {
                tx.query_row(
                    "SELECT p.delivery_id FROM attention_delivery_decisions d \
                     JOIN attention_delivery_decision_items i ON i.decision_id = d.decision_id \
                     JOIN attention_delivery_page_items pi ON pi.decision_id = i.decision_id \
                        AND pi.position = i.position \
                     JOIN attention_delivery_pages p ON p.delivery_id = pi.delivery_id \
                     WHERE d.principal = ? AND d.workspace = ? AND d.decision_id = ? \
                       AND d.lane = ? AND (i.candidate_id = ? OR i.candidate_id = ?) \
                       AND i.source_revision IS ? \
                       AND (? IS NULL OR p.delivery_id = ?)",
                    params![
                        principal,
                        workspace,
                        decision_id,
                        request.origin_surface.as_str(),
                        request.canonical_candidate_id,
                        request.raw_candidate_id,
                        request.source_revision,
                        request.delivery_id,
                        request.delivery_id,
                    ],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
            } else {
                None
            };
            let canonical_decision_valid = if delivered_binding.is_some()
                || request.delivery_id.is_some()
            {
                false
            } else if let Some(decision_id) = request.decision_id.as_deref() {
                tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM attention_decision_items \
                     WHERE principal = ? AND workspace = ? AND decision_id = ? \
                       AND (candidate_id = ? OR candidate_id = ?) \
                       AND source_revision IS ? AND served_route = ? AND served_rank > 0)",
                    params![
                        principal,
                        workspace,
                        decision_id,
                        request.canonical_candidate_id,
                        request.raw_candidate_id,
                        request.source_revision,
                        request.origin_surface.as_str(),
                    ],
                    |row| row.get::<_, i64>(0),
                )? == 1
            } else {
                false
            };
            let mut decision_id = (delivered_binding.is_some() || canonical_decision_valid)
                .then(|| request.decision_id.clone())
                .flatten();
            if decision_id.is_none() {
                decision_id = reconstruct_served_decision_id(
                    &tx,
                    &principal,
                    &workspace,
                    &request.canonical_candidate_id,
                    &request.raw_candidate_id,
                    request.source_revision.as_deref(),
                    now,
                )?;
                if let Some(reconstructed) = decision_id.as_deref() {
                    tx.execute(
                        "UPDATE attention_outcomes SET decision_id = ? \
                         WHERE outcome_id = ? AND principal = ? AND workspace = ? \
                           AND decision_id IS NULL",
                        params![reconstructed, request.outcome_id, principal, workspace],
                    )?;
                }
            }
            let validated_impression = if let (Some(decision_id), Some(impression_id)) =
                (decision_id.as_deref(), request.impression_id.as_deref())
            {
                tx.query_row(
                    "SELECT delivery_id FROM attention_impressions \
                     WHERE principal = ? AND workspace = ? AND impression_id = ? \
                       AND decision_id = ? AND (candidate_id = ? OR candidate_id = ?) \
                       AND source_revision IS ? AND surface = ? AND verified = 1",
                    params![
                        principal,
                        workspace,
                        impression_id,
                        decision_id,
                        request.canonical_candidate_id,
                        request.raw_candidate_id,
                        request.source_revision,
                        request.origin_surface.as_str(),
                    ],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .filter(|impression_delivery| {
                    let impression_delivery = impression_delivery.as_deref();
                    delivered_binding
                        .as_deref()
                        .or(request.delivery_id.as_deref())
                        .is_none_or(|expected| impression_delivery == Some(expected))
                })
            } else {
                None
            };
            let impression_id = validated_impression
                .as_ref()
                .map(|_| request.impression_id.clone())
                .flatten();
            let delivery_id = delivered_binding.or_else(|| {
                request.delivery_id.is_none()
                    .then(|| validated_impression.flatten())
                    .flatten()
            });
            let job_id = uuid::Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO attention_rank_recompute_jobs (
                    job_id, schema_version, outcome_id, principal, workspace, origin_surface,
                    canonical_candidate_id, raw_candidate_id, source_revision, outcome,
                    decision_id, delivery_id, impression_id, projection_id, affected_rank_before,
                    enqueue_policy_snapshot_id, enqueue_posterior_version, status, attempts,
                    next_retry_at, lease_owner, lease_expires_at, reason, result_json,
                    created_at, updated_at, completed_at
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', 0, NULL, NULL, NULL, NULL, NULL, ?, ?, NULL)
                 ON CONFLICT(principal, workspace, outcome_id) DO NOTHING",
                params![
                    job_id, i64::from(ATTENTION_RANK_RECOMPUTE_SCHEMA_VERSION), request.outcome_id,
                    principal, workspace, request.origin_surface.as_str(), request.canonical_candidate_id,
                    request.raw_candidate_id, request.source_revision, request.outcome.as_str(),
                    decision_id, delivery_id, impression_id, outcome_projection_id,
                    request.affected_rank_before.map(usize_to_i64), request.enqueue_policy_snapshot_id,
                    request.enqueue_posterior_version.map(u64_to_i64), now, now,
                ],
            )?;
            let job = read_rank_recompute_job_tx(&tx, &request.outcome_id, true)?
                .context("rank recompute job missing after idempotent enqueue")?;
            anyhow::ensure!(
                job.principal == principal && job.workspace == workspace
                    && job.origin_surface == request.origin_surface
                    && job.canonical_candidate_id == request.canonical_candidate_id
                    && job.raw_candidate_id == request.raw_candidate_id
                    && job.source_revision == request.source_revision
                    && job.outcome == request.outcome,
                "rank recompute outcome_id collision with different payload"
            );
            tx.commit()?;
            Ok(job)
        }).await.context("attention rank recompute enqueue task panicked")?
    }

    /// Recover the served decision for a rank-recompute job that was enqueued
    /// without a validated binding, persist it, and return the decision id.
    pub async fn reconstruct_and_bind_rank_recompute_decision(
        &self,
        job: &AttentionRankRecomputeJob,
    ) -> Result<Option<String>> {
        if job
            .decision_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            return Ok(job.decision_id.clone());
        }
        let store = self.clone();
        let job_id = job.job_id.clone();
        let outcome_id = job.outcome_id.clone();
        let principal = job.principal.clone();
        let workspace = job.workspace.clone();
        let canonical_candidate_id = job.canonical_candidate_id.clone();
        let raw_candidate_id = job.raw_candidate_id.clone();
        let source_revision = job.source_revision.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let decided_at_latest: i64 = tx.query_row(
                "SELECT occurred_at FROM attention_outcomes WHERE outcome_id = ? AND principal = ? AND workspace = ?",
                params![outcome_id, principal, workspace], |r| r.get(0),
            )?;
            let reconstructed = reconstruct_served_decision_id(
                &tx,
                &principal,
                &workspace,
                &canonical_candidate_id,
                &raw_candidate_id,
                source_revision.as_deref(),
                decided_at_latest,
            )?;
            if let Some(decision_id) = reconstructed.as_deref() {
                tx.execute(
                    "UPDATE attention_rank_recompute_jobs SET decision_id = ? \
                     WHERE job_id = ? AND decision_id IS NULL",
                    params![decision_id, job_id],
                )?;
                tx.execute(
                    "UPDATE attention_outcomes SET decision_id = ? \
                     WHERE outcome_id = ? AND principal = ? AND workspace = ? \
                       AND decision_id IS NULL",
                    params![decision_id, outcome_id, principal, workspace],
                )?;
            }
            tx.commit()?;
            Ok(reconstructed)
        })
        .await
        .context("attention rank recompute decision reconstruct task panicked")?
    }

    pub async fn get_rank_recompute_job(
        &self,
        principal: &str,
        workspace: &str,
        job_id: &str,
    ) -> Result<Option<AttentionRankRecomputeJob>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let job_id = job_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("get_rank_recompute_job")?;
            let job = read_rank_recompute_job_conn(&conn, &job_id, false)?;
            Ok(job.filter(|job| job.principal == principal && job.workspace == workspace))
        })
        .await
        .context("attention rank recompute read task panicked")?
    }

    pub async fn lease_rank_recompute_jobs(
        &self,
        principal: &str,
        workspace: &str,
        lease_owner: &str,
        now: i64,
        lease_expires_at: i64,
        limit: usize,
    ) -> Result<Vec<AttentionRankRecomputeJob>> {
        anyhow::ensure!(
            !lease_owner.trim().is_empty(),
            "rank recompute lease owner is empty"
        );
        anyhow::ensure!(
            lease_expires_at > now,
            "rank recompute lease expiry is invalid"
        );
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let lease_owner = lease_owner.to_string();
        tokio::task::spawn_blocking(move || {
            let mut conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute(
                "UPDATE attention_rank_recompute_jobs SET status = 'retry', next_retry_at = ?, lease_owner = NULL, lease_expires_at = NULL, reason = 'lease_expired', updated_at = ? WHERE principal = ? AND workspace = ? AND status = 'in_flight' AND lease_expires_at <= ?",
                params![now, now, principal, workspace, now],
            )?;
            let mut statement = tx.prepare(
                "SELECT job_id FROM attention_rank_recompute_jobs WHERE principal = ? AND workspace = ? AND (status = 'pending' OR (status = 'retry' AND COALESCE(next_retry_at, 0) <= ?)) ORDER BY created_at, job_id LIMIT ?",
            )?;
            let ids = statement.query_map(params![principal, workspace, now, limit.max(1)], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            drop(statement);
            let mut jobs = Vec::new();
            for job_id in ids {
                let changed = tx.execute(
                    "UPDATE attention_rank_recompute_jobs SET status = 'in_flight', attempts = attempts + 1, next_retry_at = NULL, lease_owner = ?, lease_expires_at = ?, reason = NULL, updated_at = ? WHERE job_id = ? AND (status = 'pending' OR status = 'retry')",
                    params![lease_owner, lease_expires_at, now, job_id],
                )?;
                if changed == 1 {
                    if let Some(job) = read_rank_recompute_job_tx(&tx, &job_id, false)? { jobs.push(job); }
                }
            }
            tx.commit()?;
            Ok(jobs)
        }).await.context("attention rank recompute lease task panicked")?
    }

    /// Lease due jobs across all scopes. Each returned job retains its
    /// principal/workspace, which the worker must use for every source read and
    /// terminal update.
    pub async fn lease_rank_recompute_jobs_globally(
        &self,
        lease_owner: &str,
        now: i64,
        lease_expires_at: i64,
        limit: usize,
    ) -> Result<Vec<AttentionRankRecomputeJob>> {
        anyhow::ensure!(
            !lease_owner.trim().is_empty(),
            "rank recompute lease owner is empty"
        );
        anyhow::ensure!(
            lease_expires_at > now,
            "rank recompute lease expiry is invalid"
        );
        let store = self.clone();
        let lease_owner = lease_owner.to_string();
        tokio::task::spawn_blocking(move || {
            let mut conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute(
                "UPDATE attention_rank_recompute_jobs SET status = 'retry', next_retry_at = ?, lease_owner = NULL, lease_expires_at = NULL, reason = 'lease_expired', updated_at = ? WHERE status = 'in_flight' AND lease_expires_at <= ?",
                params![now, now, now],
            )?;
            let mut statement = tx.prepare(
                "SELECT job_id FROM attention_rank_recompute_jobs WHERE status = 'pending' OR (status = 'retry' AND COALESCE(next_retry_at, 0) <= ?) ORDER BY created_at, principal, workspace, job_id LIMIT ?",
            )?;
            let ids = statement
                .query_map(params![now, limit.max(1)], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            drop(statement);
            let mut jobs = Vec::new();
            for job_id in ids {
                let changed = tx.execute(
                    "UPDATE attention_rank_recompute_jobs SET status = 'in_flight', attempts = attempts + 1, next_retry_at = NULL, lease_owner = ?, lease_expires_at = ?, reason = NULL, updated_at = ? WHERE job_id = ? AND (status = 'pending' OR status = 'retry')",
                    params![lease_owner, lease_expires_at, now, job_id],
                )?;
                if changed == 1 {
                    if let Some(job) = read_rank_recompute_job_tx(&tx, &job_id, false)? {
                        jobs.push(job);
                    }
                }
            }
            tx.commit()?;
            Ok(jobs)
        }).await.context("attention global rank recompute lease task panicked")?
    }

    pub async fn rank_recompute_reconciliation_scopes(
        &self,
        limit: usize,
    ) -> Result<Vec<(String, String)>> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .reads
                .acquire("rank_recompute_reconciliation_scopes")?;
            let mut statement = conn.prepare(
                "SELECT DISTINCT o.principal, o.workspace \
                 FROM attention_outcomes o \
                 LEFT JOIN attention_rank_recompute_jobs j \
                   ON j.principal = o.principal AND j.workspace = o.workspace \
                  AND j.outcome_id = o.outcome_id \
                 WHERE j.job_id IS NULL \
                 ORDER BY o.principal, o.workspace LIMIT ?",
            )?;
            let scopes = statement
                .query_map(params![limit.max(1)], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(scopes)
        })
        .await
        .context("attention rank recompute reconciliation scope task panicked")?
    }

    /// Extend an active rank-recompute lease while the owning worker is still
    /// evaluating the job. The compare-and-set deliberately refuses expired
    /// or transferred leases: reviving either would let a stale worker race a
    /// newer owner at terminal commit.
    pub async fn renew_rank_recompute_job_lease(
        &self,
        job_id: &str,
        lease_owner: &str,
        now: i64,
        lease_expires_at: i64,
    ) -> Result<bool> {
        anyhow::ensure!(
            !lease_owner.trim().is_empty(),
            "rank recompute lease owner is empty"
        );
        anyhow::ensure!(
            lease_expires_at > now,
            "rank recompute lease expiry is invalid"
        );
        let store = self.clone();
        let job_id = job_id.to_string();
        let lease_owner = lease_owner.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            let changed = conn.execute(
                "UPDATE attention_rank_recompute_jobs \
                 SET lease_expires_at = MAX(lease_expires_at, ?), updated_at = ? \
                 WHERE job_id = ? AND status = 'in_flight' AND lease_owner = ? \
                   AND lease_expires_at > ?",
                params![lease_expires_at, now, job_id, lease_owner, now],
            )?;
            Ok(changed == 1)
        })
        .await
        .context("attention rank recompute lease renewal task panicked")?
    }

    pub async fn finish_rank_recompute_job(
        &self,
        job_id: &str,
        lease_owner: &str,
        result: &AttentionRankRecomputeResult,
        now: i64,
    ) -> Result<()> {
        let encoded = serde_json::to_string(result)?;
        self.update_rank_recompute_terminal(
            job_id,
            lease_owner,
            "succeeded",
            None,
            Some(encoded),
            now,
        )
        .await
    }

    pub async fn stale_rank_recompute_job(
        &self,
        job_id: &str,
        lease_owner: &str,
        reason: &str,
        now: i64,
    ) -> Result<()> {
        validate_rank_recompute_reason(reason)?;
        self.update_rank_recompute_terminal(
            job_id,
            lease_owner,
            "stale",
            Some(reason.to_string()),
            None,
            now,
        )
        .await
    }

    async fn update_rank_recompute_terminal(
        &self,
        job_id: &str,
        lease_owner: &str,
        status: &str,
        reason: Option<String>,
        result_json: Option<String>,
        now: i64,
    ) -> Result<()> {
        let store = self.clone();
        let job_id = job_id.to_string();
        let lease_owner = lease_owner.to_string();
        let status = status.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            let changed = conn.execute(
                "UPDATE attention_rank_recompute_jobs SET status = ?, next_retry_at = NULL, lease_owner = NULL, lease_expires_at = NULL, reason = ?, result_json = ?, updated_at = ?, completed_at = ? WHERE job_id = ? AND status = 'in_flight' AND lease_owner = ? AND lease_expires_at > ?",
                params![status, reason, result_json, now, now, job_id, lease_owner, now],
            )?;
            anyhow::ensure!(changed == 1, "rank recompute terminal commit lost lease");
            Ok(())
        }).await.context("attention rank recompute terminal update task panicked")?
    }

    pub async fn retry_rank_recompute_job(
        &self,
        job_id: &str,
        lease_owner: &str,
        next_retry_at: Option<i64>,
        reason: &str,
        now: i64,
    ) -> Result<()> {
        validate_rank_recompute_reason(reason)?;
        let store = self.clone();
        let job_id = job_id.to_string();
        let lease_owner = lease_owner.to_string();
        let reason = reason.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            let status = if next_retry_at.is_some() { "retry" } else { "dead" };
            let changed = conn.execute(
                "UPDATE attention_rank_recompute_jobs SET status = ?, next_retry_at = ?, lease_owner = NULL, lease_expires_at = NULL, reason = ?, result_json = NULL, updated_at = ?, completed_at = CASE WHEN ? IS NULL THEN ? ELSE NULL END WHERE job_id = ? AND status = 'in_flight' AND lease_owner = ? AND lease_expires_at > ?",
                params![status, next_retry_at, reason, now, next_retry_at, now, job_id, lease_owner, now],
            )?;
            anyhow::ensure!(changed == 1, "rank recompute retry commit lost lease");
            Ok(())
        }).await.context("attention rank recompute retry task panicked")?
    }

    pub async fn rank_recompute_queue_counts(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<AttentionRankRecomputeQueueCounts> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("rank_recompute_queue_counts")?;
            let mut counts = AttentionRankRecomputeQueueCounts::default();
            let mut statement = conn.prepare("SELECT status, COUNT(*) FROM attention_rank_recompute_jobs WHERE principal = ? AND workspace = ? GROUP BY status")?;
            let rows = statement.query_map(params![principal, workspace], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?;
            for row in rows {
                let (status, count) = row?;
                let count = count.max(0) as u64;
                match status.as_str() {
                    "pending" => counts.pending = count,
                    "in_flight" => counts.in_flight = count,
                    "retry" => counts.retry = count,
                    "succeeded" => counts.succeeded = count,
                    "stale" => counts.stale = count,
                    "dead" => counts.dead = count,
                    _ => {},
                }
            }
            counts.next_retry_at = conn.query_row("SELECT MIN(next_retry_at) FROM attention_rank_recompute_jobs WHERE principal = ? AND workspace = ? AND status = 'retry'", params![principal, workspace], |row| row.get(0))?;
            counts.oldest_pending_at = conn.query_row("SELECT MIN(created_at) FROM attention_rank_recompute_jobs WHERE principal = ? AND workspace = ? AND status = 'pending'", params![principal, workspace], |row| row.get(0))?;
            Ok(counts)
        }).await.context("attention rank recompute queue count task panicked")?
    }

    pub async fn schedule_missing_rank_recompute_jobs(
        &self,
        principal: &str,
        workspace: &str,
        limit: usize,
        now: i64,
        apply: bool,
    ) -> Result<u64> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let mut conn = if apply {
                AttentionConnectionGuard::Writer(
                    store.conn.lock().unwrap_or_else(|p| p.into_inner()),
                )
            } else {
                AttentionConnectionGuard::Reader(
                    store.reads.acquire("schedule_missing_rank_recompute_jobs")?,
                )
            };
            let tx = conn.transaction_with_behavior(if apply {
                TransactionBehavior::Immediate
            } else {
                TransactionBehavior::Deferred
            })?;
            // This scans owner-retained attribution state while the writer
            // snapshot is held. The join index keeps the ordinary path short;
            // the progress deadline is the second line of defence against a
            // future planner/schema regression monopolising every writer.
            let progress_guard = install_attention_reconciliation_progress_guard(&tx);
            let mut statement = tx.prepare(RANK_RECOMPUTE_RECONCILIATION_SELECT_SQL)?;
            let rows = statement.query_map(params![principal, workspace, limit.max(1)], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, Option<String>>(3)?, row.get::<_, String>(4)?, row.get::<_, Option<String>>(5)?, row.get::<_, Option<String>>(6)?, row.get::<_, Option<String>>(7)?, row.get::<_, Option<String>>(8)?, row.get::<_, Option<i64>>(9)?, row.get::<_, Option<String>>(10)?, row.get::<_, Option<i64>>(11)?))
            })?.collect::<std::result::Result<Vec<_>, _>>()
                .context("scanning missing attention rank recompute jobs")?;
            drop(statement);
            drop(progress_guard);
            if apply {
                for (outcome_id, surface, raw_id, revision, outcome, decision_id, delivery_id, impression_id, projection_id, rank_before, snapshot_id, posterior_version) in &rows {
                    tx.execute(
                        "INSERT INTO attention_rank_recompute_jobs (job_id, schema_version, outcome_id, principal, workspace, origin_surface, canonical_candidate_id, raw_candidate_id, source_revision, outcome, decision_id, delivery_id, impression_id, projection_id, affected_rank_before, enqueue_policy_snapshot_id, enqueue_posterior_version, status, attempts, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', 0, ?, ?) ON CONFLICT(principal, workspace, outcome_id) DO NOTHING",
                        params![uuid::Uuid::new_v4().to_string(), i64::from(ATTENTION_RANK_RECOMPUTE_SCHEMA_VERSION), outcome_id, principal, workspace, surface, format!("{surface}:{raw_id}"), raw_id, revision, outcome, decision_id, delivery_id, impression_id, projection_id, rank_before, snapshot_id, posterior_version, now, now],
                    )?;
                }
            }
            let count = rows.len() as u64;
            tx.commit()?;
            Ok(count)
        }).await.context("attention rank recompute reconciliation task panicked")?
    }

    /// Return jobs whose stale verdict is now known to have been wrong to the
    /// queue, bounded and idempotent, so a fixed guard can re-decide them.
    ///
    /// Scoped deliberately to `universe_changed_during_commit`. That reason was
    /// produced by comparing a served projection's historical universe against
    /// the live one, which differ by construction, so it never carried
    /// information about the job. Every other stale reason is a correct verdict
    /// about the job's own inputs and must stay terminal — a blanket reset would
    /// resurrect genuinely inactive candidates and revive superseded revisions.
    ///
    /// `affected_rank_before` is backfilled from the rank the candidate was
    /// actually served at while requeuing. It is NULL on this whole cohort
    /// because the enqueue reconciler sourced it from `attention_bandit_updates`,
    /// which has no rows until the bandit runs, while the served rank was
    /// recorded all along. Without it a recovered job can only report an after
    /// rank and never a delta, which is the part training needs.
    ///
    /// Which served rank depends on how the job was served, and the two are not
    /// interchangeable: a delivery-native job takes its own delivery decision's
    /// position, and only a job served from the canonical decision takes that
    /// decision's `served_rank`. They order different candidate sets.
    pub async fn requeue_wrongly_staled_rank_recompute_jobs(
        &self,
        principal: &str,
        workspace: &str,
        limit: usize,
        now: i64,
        apply: bool,
    ) -> Result<u64> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let mut conn = if apply {
                AttentionConnectionGuard::Writer(
                    store.conn.lock().unwrap_or_else(|p| p.into_inner()),
                )
            } else {
                AttentionConnectionGuard::Reader(
                    store
                        .reads
                        .acquire("requeue_wrongly_staled_rank_recompute_jobs")?,
                )
            };
            let tx = conn.transaction_with_behavior(if apply {
                TransactionBehavior::Immediate
            } else {
                TransactionBehavior::Deferred
            })?;
            let mut statement = tx.prepare(
                // A delivery-native job was served out of its own delivery
                // decision, so the rank its owner saw is that decision's served
                // position — not the canonical projection's rank for the same
                // candidate, which orders a different set entirely. Measured on
                // the recovered cohort, the two disagree for 57 of 80 delivery
                // jobs, by as much as position 1 against rank 101. Taking the
                // wrong one produces a plausible number and a meaningless delta.
                "SELECT j.job_id, \
                        COALESCE(j.affected_rank_before, \
                                 NULLIF(ddi.position, 0), \
                                 NULLIF(i.served_rank, 0)) \
                 FROM attention_rank_recompute_jobs j \
                 LEFT JOIN attention_delivery_decisions dd \
                        ON dd.decision_id = j.decision_id \
                       AND dd.principal = j.principal AND dd.workspace = j.workspace \
                 LEFT JOIN attention_delivery_decision_items ddi \
                        ON ddi.decision_id = j.decision_id \
                       AND ddi.candidate_id = j.canonical_candidate_id \
                 LEFT JOIN attention_decision_items i \
                        ON i.decision_id = COALESCE(dd.projection_id, j.decision_id) \
                       AND i.principal = j.principal AND i.workspace = j.workspace \
                       AND i.candidate_id = j.canonical_candidate_id \
                 WHERE j.principal = ? AND j.workspace = ? \
                   AND j.status = 'stale' AND j.reason = ? \
                 ORDER BY j.created_at, j.job_id LIMIT ?",
            )?;
            let rows = statement
                .query_map(
                    params![
                        principal.as_str(),
                        workspace.as_str(),
                        ATTENTION_RANK_RECOMPUTE_WRONGLY_STALED_REASON,
                        limit.max(1)
                    ],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?)),
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            drop(statement);
            if apply {
                for (job_id, rank_before) in &rows {
                    tx.execute(
                        "UPDATE attention_rank_recompute_jobs \
                            SET status = 'pending', attempts = 0, next_retry_at = NULL, \
                                lease_owner = NULL, lease_expires_at = NULL, reason = NULL, \
                                result_json = NULL, completed_at = NULL, \
                                affected_rank_before = ?, updated_at = ? \
                          WHERE job_id = ? AND status = 'stale' AND reason = ?",
                        params![
                            rank_before,
                            now,
                            job_id,
                            ATTENTION_RANK_RECOMPUTE_WRONGLY_STALED_REASON
                        ],
                    )?;
                }
            }
            let count = rows.len() as u64;
            tx.commit()?;
            Ok(count)
        })
        .await
        .context("attention rank recompute requeue task panicked")?
    }

    pub async fn compact_rank_recompute_jobs(
        &self,
        principal: &str,
        workspace: &str,
        cutoff_at: i64,
        apply: bool,
    ) -> Result<u64> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = if apply {
                AttentionConnectionGuard::Writer(
                    store.conn.lock().unwrap_or_else(|p| p.into_inner()),
                )
            } else {
                AttentionConnectionGuard::Reader(
                    store.reads.acquire("compact_rank_recompute_jobs_preview")?,
                )
            };
            let count: i64 = conn.query_row("SELECT COUNT(*) FROM attention_rank_recompute_jobs WHERE principal = ? AND workspace = ? AND completed_at <= ? AND status IN ('succeeded','stale','dead')", params![principal, workspace, cutoff_at], |row| row.get(0))?;
            if apply { conn.execute("DELETE FROM attention_rank_recompute_jobs WHERE principal = ? AND workspace = ? AND completed_at <= ? AND status IN ('succeeded','stale','dead')", params![principal, workspace, cutoff_at])?; }
            Ok(count.max(0) as u64)
        }).await.context("attention rank recompute retention task panicked")?
    }

    /// Idempotently schedule one exact source revision. Re-enqueuing the same
    /// producer contract is a no-op; a revision or producer migration resets
    /// the row to pending without persisting any candidate text.
    pub async fn schedule_semantic_extraction(
        &self,
        principal: &str,
        workspace: &str,
        request: &ScheduleSemanticExtraction,
        now: i64,
    ) -> Result<bool> {
        self.schedule_semantic_extraction_inner(principal, workspace, request, now, false)
            .await
    }

    /// Discovery has read an active source revision without compatible features.
    /// A prior successful queue receipt alone cannot establish source coverage.
    pub async fn schedule_semantic_extraction_from_source(
        &self,
        principal: &str,
        workspace: &str,
        request: &ScheduleSemanticExtraction,
        now: i64,
    ) -> Result<bool> {
        self.schedule_semantic_extraction_inner(principal, workspace, request, now, true)
            .await
    }

    async fn schedule_semantic_extraction_inner(
        &self,
        principal: &str,
        workspace: &str,
        request: &ScheduleSemanticExtraction,
        now: i64,
        incompatible_source_observed: bool,
    ) -> Result<bool> {
        anyhow::ensure!(
            !principal.trim().is_empty(),
            "semantic extraction principal is empty"
        );
        anyhow::ensure!(
            !workspace.trim().is_empty(),
            "semantic extraction workspace is empty"
        );
        anyhow::ensure!(
            !request.candidate_id.trim().is_empty(),
            "semantic extraction candidate_id is empty"
        );
        anyhow::ensure!(
            !request.source_revision.trim().is_empty(),
            "semantic extraction source revision is empty"
        );
        anyhow::ensure!(
            !request.contract.extractor_contract.trim().is_empty(),
            "semantic extraction contract is empty"
        );
        anyhow::ensure!(
            !request.contract.prompt_version.trim().is_empty(),
            "semantic extraction prompt version is empty"
        );
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let request = request.clone();
        tokio::task::spawn_blocking(move || {
            let work_id = blake3::hash(
                format!(
                    "{}\x1f{}\x1f{}\x1f{}",
                    principal,
                    workspace,
                    request.surface.as_str(),
                    request.candidate_id
                )
                .as_bytes(),
            )
            .to_hex()
            .to_string();
            let mut conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let changed = tx.execute(
                "INSERT INTO attention_semantic_extraction_work (
                    work_id, principal, workspace, surface, candidate_id,
                    source_revision, source_revision_number,
                    semantic_schema_version, extractor_contract, prompt_version,
                    model, profile, status, attempts, next_retry_at,
                    lease_owner, lease_expires_at, last_error_code, created_at, updated_at
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', 0, NULL, NULL, NULL, NULL, ?, ?)
                 ON CONFLICT(principal, workspace, surface, candidate_id) DO UPDATE SET
                    source_revision = excluded.source_revision,
                    source_revision_number = excluded.source_revision_number,
                    semantic_schema_version = excluded.semantic_schema_version,
                    extractor_contract = excluded.extractor_contract,
                    prompt_version = excluded.prompt_version,
                    model = excluded.model,
                    profile = excluded.profile,
                    status = 'pending', attempts = 0, next_retry_at = NULL,
                    lease_owner = NULL, lease_expires_at = NULL,
                    last_error_code = NULL, updated_at = excluded.updated_at
                 WHERE attention_semantic_extraction_work.source_revision <> excluded.source_revision
                    OR attention_semantic_extraction_work.source_revision_number <> excluded.source_revision_number
                    OR attention_semantic_extraction_work.semantic_schema_version <> excluded.semantic_schema_version
                    OR attention_semantic_extraction_work.extractor_contract <> excluded.extractor_contract
                    OR attention_semantic_extraction_work.prompt_version <> excluded.prompt_version
                    OR NOT (attention_semantic_extraction_work.model IS excluded.model)
                    OR NOT (attention_semantic_extraction_work.profile IS excluded.profile)",
                params![
                    work_id,
                    principal,
                    workspace,
                    request.surface.as_str(),
                    request.candidate_id,
                    request.source_revision,
                    request.source_revision_number,
                    request.contract.semantic_schema_version,
                    request.contract.extractor_contract,
                    request.contract.prompt_version,
                    request.contract.model,
                    request.contract.profile,
                    now,
                    now,
                ],
            )?;
            let repaired = changed == 0 && queue_recovery::recover_invalid_semantics(&tx, &work_id, &request, now, incompatible_source_observed)?;
            tx.commit()?;
            Ok(changed > 0 || repaired)
        })
        .await
        .context("semantic extraction schedule task panicked")?
    }

    /// Acquire a bounded batch using an immediate transaction. Expired leases
    /// are recoverable after restart; exact-revision commits still protect the
    /// source stores from stale completion.
    pub async fn lease_semantic_extraction_work(
        &self,
        principal: &str,
        workspace: &str,
        lease_owner: &str,
        now: i64,
        lease_expires_at: i64,
        limit: usize,
    ) -> Result<Vec<SemanticExtractionWorkItem>> {
        anyhow::ensure!(
            !principal.trim().is_empty(),
            "semantic extraction principal is empty"
        );
        anyhow::ensure!(
            !workspace.trim().is_empty(),
            "semantic extraction workspace is empty"
        );
        anyhow::ensure!(
            !lease_owner.trim().is_empty(),
            "semantic extraction lease owner is empty"
        );
        anyhow::ensure!(
            (1..=100).contains(&limit),
            "semantic extraction lease limit must be within 1..=100"
        );
        anyhow::ensure!(
            lease_expires_at > now,
            "semantic extraction lease must expire after now"
        );
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let lease_owner = lease_owner.to_string();
        tokio::task::spawn_blocking(move || {
            let mut conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let work_ids = {
                let mut stmt = tx.prepare(
                    "SELECT work_id FROM attention_semantic_extraction_work
                     WHERE principal = ? AND workspace = ?
                       AND ((status IN ('pending', 'retry') AND COALESCE(next_retry_at, 0) <= ?)
                            OR (status = 'in_flight' AND COALESCE(lease_expires_at, 0) <= ?))
                     ORDER BY
                       CASE WHEN status = 'in_flight' AND COALESCE(lease_expires_at, 0) <= ?
                            THEN 0 ELSE 1 END,
                       updated_at ASC, work_id ASC LIMIT ?",
                )?;
                let work_ids = stmt
                    .query_map(
                        params![principal, workspace, now, now, now, limit as i64],
                        |row| row.get::<_, String>(0),
                    )?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                work_ids
            };
            for work_id in &work_ids {
                tx.execute(
                    "UPDATE attention_semantic_extraction_work
                     SET status = 'in_flight', attempts = attempts + 1,
                         lease_owner = ?, lease_expires_at = ?, updated_at = ?
                     WHERE work_id = ?",
                    params![lease_owner, lease_expires_at, now, work_id],
                )?;
            }
            let mut leased = Vec::with_capacity(work_ids.len());
            for work_id in &work_ids {
                leased.push(read_semantic_work_item(&tx, work_id)?);
            }
            tx.commit()?;
            Ok(leased)
        })
        .await
        .context("semantic extraction lease task panicked")?
    }

    /// Extend a lease that is still owned by the same worker and exact source
    /// revision. This lets slow local inference remain safe without leasing
    /// queued work long before an execution slot is available.
    pub async fn renew_semantic_extraction_work_lease(
        &self,
        work_id: &str,
        source_revision: &str,
        lease_owner: &str,
        lease_expires_at: i64,
        now: i64,
    ) -> Result<bool> {
        anyhow::ensure!(
            lease_expires_at > now,
            "semantic extraction renewed lease must expire after now"
        );
        let store = self.clone();
        let work_id = work_id.to_string();
        let source_revision = source_revision.to_string();
        let lease_owner = lease_owner.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            Ok(conn.execute(
                "UPDATE attention_semantic_extraction_work
                 SET lease_expires_at = ?, updated_at = ?
                 WHERE work_id = ? AND source_revision = ?
                   AND status = 'in_flight' AND lease_owner = ?",
                params![lease_expires_at, now, work_id, source_revision, lease_owner],
            )? > 0)
        })
        .await
        .context("semantic extraction lease renewal task panicked")?
    }

    /// Finish a leased item only when the worker still owns the exact revision.
    /// Returns false for a stale result or an expired/reassigned lease.
    pub async fn finish_semantic_extraction_work(
        &self,
        work_id: &str,
        source_revision: &str,
        lease_owner: &str,
        status: SemanticExtractionWorkStatus,
        error_code: Option<&str>,
        now: i64,
    ) -> Result<bool> {
        anyhow::ensure!(
            matches!(
                status,
                SemanticExtractionWorkStatus::Succeeded
                    | SemanticExtractionWorkStatus::Missing
                    | SemanticExtractionWorkStatus::Invalid
            ),
            "semantic extraction finish status must be terminal"
        );
        let store = self.clone();
        let work_id = work_id.to_string();
        let source_revision = source_revision.to_string();
        let lease_owner = lease_owner.to_string();
        let error_code = error_code.map(str::to_string);
        tokio::task::spawn_blocking(move || {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            Ok(conn.execute(
                "UPDATE attention_semantic_extraction_work
                 SET status = ?, next_retry_at = NULL, lease_owner = NULL,
                     lease_expires_at = NULL, last_error_code = ?, updated_at = ?
                 WHERE work_id = ? AND source_revision = ?
                   AND status = 'in_flight' AND lease_owner = ?
                   AND lease_expires_at > ?",
                params![
                    status.as_str(),
                    error_code,
                    now,
                    work_id,
                    source_revision,
                    lease_owner,
                    now
                ],
            )? > 0)
        })
        .await
        .context("semantic extraction finish task panicked")?
    }

    pub async fn semantic_extraction_lease_is_current(
        &self,
        work_id: &str,
        source_revision: &str,
        lease_owner: &str,
        now: i64,
    ) -> Result<bool> {
        let store = self.clone();
        let work_id = work_id.to_string();
        let source_revision = source_revision.to_string();
        let lease_owner = lease_owner.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .reads
                .acquire("semantic_extraction_lease_is_current")?;
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM attention_semantic_extraction_work
                 WHERE work_id = ? AND source_revision = ? AND status = 'in_flight'
                   AND lease_owner = ? AND lease_expires_at > ?",
                params![work_id, source_revision, lease_owner, now],
                |row| row.get(0),
            )?;
            Ok(count == 1)
        })
        .await
        .context("semantic extraction lease validation task panicked")?
    }

    pub async fn retry_semantic_extraction_work(
        &self,
        work_id: &str,
        source_revision: &str,
        lease_owner: &str,
        next_retry_at: Option<i64>,
        error_code: &str,
        now: i64,
    ) -> Result<bool> {
        let store = self.clone();
        let work_id = work_id.to_string();
        let source_revision = source_revision.to_string();
        let lease_owner = lease_owner.to_string();
        let error_code = error_code.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            let status = if next_retry_at.is_some() {
                "retry"
            } else {
                "dead"
            };
            Ok(conn.execute(
                "UPDATE attention_semantic_extraction_work
                 SET status = ?, next_retry_at = ?, lease_owner = NULL,
                     lease_expires_at = NULL, last_error_code = ?, updated_at = ?
                 WHERE work_id = ? AND source_revision = ?
                   AND status = 'in_flight' AND lease_owner = ?
                   AND lease_expires_at > ?",
                params![
                    status,
                    next_retry_at,
                    error_code,
                    now,
                    work_id,
                    source_revision,
                    lease_owner,
                    now
                ],
            )? > 0)
        })
        .await
        .context("semantic extraction retry task panicked")?
    }

    pub async fn semantic_extraction_queue_counts(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<SemanticExtractionQueueCounts> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let now = chrono::Utc::now().timestamp_millis();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("semantic_extraction_queue_counts")?;
            let mut counts = SemanticExtractionQueueCounts::default();
            let mut stmt = conn.prepare(
                "SELECT status, COUNT(*), MIN(next_retry_at)
                 FROM attention_semantic_extraction_work
                 WHERE principal = ? AND workspace = ? GROUP BY status",
            )?;
            let mut rows = stmt.query(params![principal, workspace])?;
            while let Some(row) = rows.next()? {
                let status: String = row.get(0)?;
                let count: i64 = row.get(1)?;
                match SemanticExtractionWorkStatus::from_str(&status)? {
                    SemanticExtractionWorkStatus::Pending => counts.pending = count as u64,
                    SemanticExtractionWorkStatus::InFlight => counts.in_flight = count as u64,
                    SemanticExtractionWorkStatus::Retry => {
                        counts.retry = count as u64;
                        counts.next_retry_at = row.get(2)?;
                    },
                    SemanticExtractionWorkStatus::Succeeded => counts.succeeded = count as u64,
                    SemanticExtractionWorkStatus::Missing => counts.missing = count as u64,
                    SemanticExtractionWorkStatus::Invalid => counts.invalid = count as u64,
                    SemanticExtractionWorkStatus::Dead => counts.dead = count as u64,
                }
            }
            let (expired_in_flight, oldest_ready_at, last_succeeded_at): (
                i64,
                Option<i64>,
                Option<i64>,
            ) = conn.query_row(
                "SELECT
                   COALESCE(SUM(CASE WHEN status = 'in_flight'
                                      AND COALESCE(lease_expires_at, 0) <= ?
                                     THEN 1 ELSE 0 END), 0),
                   MIN(CASE WHEN status = 'pending'
                              OR (status = 'retry' AND COALESCE(next_retry_at, 0) <= ?)
                              OR (status = 'in_flight' AND COALESCE(lease_expires_at, 0) <= ?)
                            THEN updated_at END),
                   MAX(CASE WHEN status = 'succeeded' THEN updated_at END)
                 FROM attention_semantic_extraction_work
                 WHERE principal = ? AND workspace = ?",
                params![now, now, now, principal, workspace],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            counts.expired_in_flight = expired_in_flight.max(0) as u64;
            counts.active_in_flight = counts.in_flight.saturating_sub(counts.expired_in_flight);
            counts.oldest_ready_at = oldest_ready_at;
            counts.last_succeeded_at = last_succeeded_at;
            Ok(counts)
        })
        .await
        .context("semantic extraction queue counts task panicked")?
    }

    pub async fn semantic_extraction_work_items(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<SemanticExtractionWorkItem>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("semantic_extraction_work_items")?;
            let mut stmt = conn.prepare(
                "SELECT work_id FROM attention_semantic_extraction_work
                 WHERE principal = ? AND workspace = ? ORDER BY surface, candidate_id",
            )?;
            let work_ids = stmt
                .query_map(params![principal, workspace], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            work_ids
                .iter()
                .map(|work_id| read_semantic_work_item(&conn, work_id))
                .collect()
        })
        .await
        .context("semantic extraction scoped work read task panicked")?
    }

    pub async fn semantic_extraction_checkpoints(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<SemanticExtractionCheckpoint>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("semantic_extraction_checkpoints")?;
            let mut stmt = conn.prepare(
                "SELECT surface, cursor, lease_owner, lease_expires_at, updated_at
                 FROM attention_semantic_extraction_checkpoints
                 WHERE principal = ? AND workspace = ? ORDER BY surface",
            )?;
            let rows = stmt.query_map(params![principal, workspace], |row| {
                let surface: String = row.get(0)?;
                Ok((surface, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
            })?;
            let mut checkpoints = Vec::new();
            for row in rows {
                let (surface, cursor, lease_owner, lease_expires_at, updated_at) = row?;
                checkpoints.push(SemanticExtractionCheckpoint {
                    surface: AttentionSurface::from_str(&surface)?,
                    cursor,
                    lease_owner,
                    lease_expires_at,
                    updated_at,
                });
            }
            Ok(checkpoints)
        })
        .await
        .context("semantic extraction checkpoint read task panicked")?
    }

    pub async fn update_semantic_extraction_checkpoint(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
        cursor: Option<&str>,
        now: i64,
    ) -> Result<()> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let cursor = cursor.map(str::to_string);
        tokio::task::spawn_blocking(move || {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.execute(
                "INSERT INTO attention_semantic_extraction_checkpoints
                    (principal, workspace, surface, cursor, updated_at)
                 VALUES (?, ?, ?, ?, ?)
                 ON CONFLICT(principal, workspace, surface) DO UPDATE SET
                    cursor = excluded.cursor, updated_at = excluded.updated_at",
                params![principal, workspace, surface.as_str(), cursor, now],
            )?;
            Ok(())
        })
        .await
        .context("semantic extraction checkpoint update task panicked")?
    }

    pub async fn record_outcome(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
        request: &RecordAttentionOutcome,
        embedding: Option<&SemanticEmbedding>,
    ) -> Result<PersistedAttentionOutcome> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        anyhow::ensure!(
            !request.event_id.trim().is_empty() && request.event_id.chars().count() <= 200,
            "attention outcome event_id must contain 1..=200 characters"
        );
        anyhow::ensure!(
            !request.candidate.candidate_id.trim().is_empty()
                && request.candidate.candidate_id.chars().count() <= 500,
            "attention outcome candidate_id must contain 1..=500 characters"
        );
        let mut request = request.clone();
        request.reason = canonical_reason(request.outcome, request.reason.as_deref());
        if let Some(embedding) = embedding {
            anyhow::ensure!(
                valid_embedding(&embedding.vector),
                "attention outcome embedding must contain finite non-zero values"
            );
        }
        let embedding = embedding.cloned();
        let embedding_contract = embedding
            .as_ref()
            .map(|embedding| embedding.contract.clone());
        tokio::task::spawn_blocking(move || {
            let mut conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tx = conn
                .transaction()
                .context("opening attention outcome transaction")?;
            let requested_decision_id = request
                .attribution
                .as_ref()
                .map(|attribution| attribution.decision_id.as_str());
            let requested_impression_id = request
                .attribution
                .as_ref()
                .and_then(|attribution| attribution.impression_id.as_deref());
            let requested_delivery_id = request
                .attribution
                .as_ref()
                .and_then(|attribution| attribution.delivery_id.as_deref());
            let requested_projection_id = resolve_projection_reference(
                &tx,
                &principal,
                &workspace,
                requested_decision_id,
                requested_impression_id,
                requested_delivery_id,
            )?;
            if let Some(existing) =
                read_outcome_by_event(&tx, &principal, &workspace, request.event_id.as_str())?
            {
                anyhow::ensure!(
                    existing.surface == surface.as_str()
                        && existing.candidate_id == request.candidate.candidate_id
                        && existing.source_revision == request.candidate.source_revision
                        && existing.outcome == request.outcome
                        && existing.reason == request.reason
                        && existing.label_quality == request.label_quality.as_str()
                        && attribution_replay_compatible(
                            existing.decision_id.as_deref(),
                            existing.impression_id.as_deref(),
                            existing.delivery_id.as_deref(),
                            requested_decision_id,
                            requested_impression_id,
                            requested_delivery_id,
                        )
                        && requested_projection_id.as_deref().is_none_or(|requested| {
                            existing.projection_id.as_deref().is_none_or(|stored| stored == requested)
                        }),
                    "attention outcome event_id collision with a different payload"
                );
                // A first delivery can reach capture before its attribution
                // envelope is attached. Permit exactly one monotonic
                // enrichment from no attribution to the supplied decision;
                // every other attribution change remains an idempotency
                // collision. The guarded UPDATE keeps concurrent replays
                // deterministic.
                if existing.decision_id.is_none() && requested_decision_id.is_some() {
                    let changed = tx.execute(
                        "UPDATE attention_outcomes SET decision_id = ?, impression_id = ?, delivery_id = ?, \
                            projection_id = COALESCE(projection_id, ?) \
                         WHERE outcome_id = ? AND decision_id IS NULL AND impression_id IS NULL AND delivery_id IS NULL",
                        params![
                            requested_decision_id,
                            requested_impression_id,
                            requested_delivery_id,
                            requested_projection_id,
                            existing.persisted.outcome_id.as_str(),
                        ],
                    )?;
                    anyhow::ensure!(
                        changed == 1,
                        "attention outcome attribution enrichment raced with a different payload"
                    );
                } else if existing.decision_id.as_deref() == requested_decision_id
                    && ((existing.impression_id.is_none() && requested_impression_id.is_some())
                        || (existing.delivery_id.is_none() && requested_delivery_id.is_some())
                        || (existing.projection_id.is_none()
                            && requested_projection_id.is_some()))
                {
                    let changed = tx.execute(
                        "UPDATE attention_outcomes SET \
                            impression_id = COALESCE(impression_id, ?), \
                            delivery_id = COALESCE(delivery_id, ?), \
                            projection_id = COALESCE(projection_id, ?) \
                         WHERE outcome_id = ? AND decision_id = ? \
                           AND (impression_id IS NULL OR impression_id IS ?) \
                           AND (delivery_id IS NULL OR delivery_id IS ?)",
                        params![
                            requested_impression_id,
                            requested_delivery_id,
                            requested_projection_id,
                            existing.persisted.outcome_id.as_str(),
                            requested_decision_id,
                            requested_impression_id,
                            requested_delivery_id,
                        ],
                    )?;
                    anyhow::ensure!(
                        changed == 1,
                        "attention outcome impression enrichment raced with a different payload"
                    );
                }
                tx.commit()
                    .context("committing replayed attention outcome transaction")?;
                return Ok(existing.persisted);
            }
            let outcome_id = uuid::Uuid::new_v4().to_string();
            let created_at = chrono::Utc::now().timestamp_millis();
            tx.execute(
                "INSERT INTO attention_outcomes ( \
                    outcome_id, schema_version, event_id, principal, workspace, surface, \
                    candidate_id, source_revision, outcome, reason, label_quality, \
                    embedding_contract, occurred_at, created_at, decision_id, impression_id, delivery_id, \
                    projection_id \
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    outcome_id,
                    i64::from(ATTENTION_OUTCOME_SCHEMA_VERSION),
                    request.event_id,
                    principal,
                    workspace,
                    surface.as_str(),
                    request.candidate.candidate_id,
                    request.candidate.source_revision,
                    request.outcome.as_str(),
                    request.reason,
                    request.label_quality.as_str(),
                    embedding_contract,
                    request.occurred_at,
                    created_at,
                    // Persisting what the caller already supplied. Capture stays
                    // independent of it: a missing or malformed attribution
                    // still records the outcome, exactly as before — it just
                    // cannot be traced back to what was shown.
                    request.attribution.as_ref().map(|a| a.decision_id.clone()),
                    request
                        .attribution
                        .as_ref()
                        .and_then(|a| a.impression_id.clone()),
                    request
                        .attribution
                        .as_ref()
                        .and_then(|a| a.delivery_id.clone()),
                    requested_projection_id,
                ],
            )
            .context("inserting canonical attention outcome")?;
            if let Some(embedding) = embedding {
                let vec_json = serde_json::to_string(&embedding.vector)
                    .context("serializing canonical outcome embedding")?;
                let content_digest =
                    blake3::hash(request.candidate.semantic_text.trim().as_bytes())
                        .to_hex()
                        .to_string();
                tx.execute(
                    "INSERT INTO attention_candidate_embeddings ( \
                        principal, workspace, surface, candidate_id, source_revision, \
                        content_digest, embedding_contract, vec_json, updated_at \
                     ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
                     ON CONFLICT(principal, workspace, surface, candidate_id) DO UPDATE SET \
                        source_revision = excluded.source_revision, \
                        content_digest = excluded.content_digest, \
                        embedding_contract = excluded.embedding_contract, \
                        vec_json = excluded.vec_json, updated_at = excluded.updated_at",
                    params![
                        principal,
                        workspace,
                        surface.as_str(),
                        request.candidate.candidate_id,
                        request.candidate.source_revision,
                        content_digest,
                        embedding.contract,
                        vec_json,
                        created_at,
                    ],
                )
                .context("persisting canonical outcome embedding atomically")?;
            } else {
                let semantic_text = request
                    .candidate
                    .semantic_text
                    .trim()
                    .chars()
                    .take(ATTENTION_REPAIR_SEMANTIC_TEXT_MAX_CHARS)
                    .collect::<String>();
                if !semantic_text.is_empty() {
                    tx.execute(
                        "INSERT OR IGNORE INTO attention_embedding_bind_work ( \
                            outcome_id, principal, workspace, surface, candidate_id, \
                            source_revision, semantic_text, created_at, updated_at \
                         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                        params![
                            outcome_id,
                            principal,
                            workspace,
                            surface.as_str(),
                            request.candidate.candidate_id,
                            request.candidate.source_revision,
                            semantic_text,
                            created_at,
                            created_at,
                        ],
                    )
                    .context("scheduling durable attention embedding bind")?;
                }
            }
            tx.commit()
                .context("committing canonical attention outcome")?;
            Ok(PersistedAttentionOutcome {
                outcome_id,
                outcome: request.outcome,
                inserted: true,
            })
        })
        .await
        .context("attention learning record_outcome task panicked")?
    }

    pub async fn bind_outcome_embedding(
        &self,
        principal: &str,
        workspace: &str,
        outcome_id: &str,
        candidate_id: &str,
        source_revision: Option<&str>,
        embedding_contract: &str,
    ) -> Result<()> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let outcome_id = outcome_id.to_string();
        let candidate_id = candidate_id.to_string();
        let source_revision = source_revision.map(str::to_string);
        let embedding_contract = embedding_contract.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let changed = conn.execute(
                "UPDATE attention_outcomes SET embedding_contract = ? \
                 WHERE principal = ? AND workspace = ? AND outcome_id = ? \
                   AND candidate_id = ? AND source_revision IS ?",
                params![
                    embedding_contract,
                    principal,
                    workspace,
                    outcome_id,
                    candidate_id,
                    source_revision,
                ],
            )?;
            anyhow::ensure!(
                changed == 1,
                "attention outcome revision changed before embedding bind"
            );
            Ok(())
        })
        .await
        .context("attention learning bind_outcome_embedding task panicked")?
    }

    pub async fn outcome_has_embedding(
        &self,
        principal: &str,
        workspace: &str,
        outcome_id: &str,
    ) -> Result<bool> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let outcome_id = outcome_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("outcome_has_embedding")?;
            conn.query_row(
                "SELECT embedding_contract IS NOT NULL FROM attention_outcomes WHERE principal = ? AND workspace = ? AND outcome_id = ?",
                params![principal, workspace, outcome_id],
                |row| row.get::<_, i64>(0),
            )
            .map(|value| value != 0)
            .map_err(Into::into)
        })
        .await
        .context("attention outcome embedding read task panicked")?
    }

    pub async fn outcome_exists_for_event(
        &self,
        principal: &str,
        workspace: &str,
        event_id: &str,
    ) -> Result<bool> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let event_id = event_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("outcome_exists_for_event")?;
            Ok(conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM attention_outcomes WHERE principal = ? AND workspace = ? AND event_id = ?)",
                params![principal, workspace, event_id],
                |row| row.get::<_, i64>(0),
            )? != 0)
        })
        .await
        .context("attention outcome event existence task panicked")?
    }

    pub async fn list_pending_embedding_binds(
        &self,
        limit: usize,
    ) -> Result<Vec<PendingAttentionEmbeddingBind>> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("list_pending_embedding_binds")?;
            let now = chrono::Utc::now().timestamp_millis();
            let mut statement = conn.prepare(
                "WITH fair AS ( \
                    SELECT outcome_id, principal, workspace, surface, candidate_id, \
                           source_revision, semantic_text, attempts, created_at, \
                           ROW_NUMBER() OVER ( \
                               PARTITION BY principal, workspace \
                               ORDER BY created_at, outcome_id \
                           ) AS scope_rank \
                    FROM attention_embedding_bind_work \
                    WHERE surface IN ('follow_up', 'worth_a_look') \
                      AND (status = 'pending' \
                       OR (status = 'retry' AND COALESCE(next_retry_at, 0) <= ?)) \
                 ) \
                 SELECT outcome_id, principal, workspace, surface, candidate_id, \
                        source_revision, semantic_text, attempts \
                 FROM fair \
                 ORDER BY scope_rank, created_at, principal, workspace, outcome_id LIMIT ?",
            )?;
            let rows = statement.query_map(params![now, limit.max(1)], |row| {
                let surface = AttentionSurface::from_str(&row.get::<_, String>(3)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?;
                Ok(PendingAttentionEmbeddingBind {
                    outcome_id: row.get(0)?,
                    principal: row.get(1)?,
                    workspace: row.get(2)?,
                    surface,
                    candidate_id: row.get(4)?,
                    source_revision: row.get(5)?,
                    semantic_text: row.get(6)?,
                    attempts: row.get::<_, i64>(7)?.max(0) as u32,
                })
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(Into::into)
        })
        .await
        .context("attention pending embedding bind list task panicked")?
    }

    pub async fn claim_pending_embedding_binds(
        &self,
        limit: usize,
        lease_owner: &str,
        lease_secs: u64,
    ) -> Result<Vec<PendingAttentionEmbeddingBind>> {
        anyhow::ensure!(
            !lease_owner.trim().is_empty()
                && lease_owner.chars().count() <= 200
                && !lease_owner.chars().any(char::is_control),
            "attention embedding bind lease owner must contain 1..=200 characters without controls"
        );
        anyhow::ensure!(
            (30..=3_600).contains(&lease_secs),
            "attention embedding bind lease must be within 30..=3600 seconds"
        );
        if limit == 0 {
            return Ok(Vec::new());
        }
        let store = self.clone();
        let lease_owner = lease_owner.to_string();
        tokio::task::spawn_blocking(move || {
            let mut conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = chrono::Utc::now().timestamp_millis();
            let lease_millis = i64::try_from(lease_secs)
                .unwrap_or(i64::MAX)
                .saturating_mul(1_000);
            let lease_expires_at = now.saturating_add(lease_millis);
            tx.execute(
                "UPDATE attention_embedding_bind_work \
                 SET status = 'dead', attempts = ?, next_retry_at = NULL, \
                     lease_owner = NULL, lease_expires_at = NULL, \
                     last_error_code = 'invalid_surface', last_attempt_at = ?, updated_at = ? \
                 WHERE surface NOT IN ('follow_up', 'worth_a_look') AND status != 'dead'",
                params![i64::from(ATTENTION_EMBEDDING_BIND_MAX_ATTEMPTS), now, now],
            )?;
            let mut statement = tx.prepare(
                "WITH fair AS ( \
                    SELECT outcome_id, principal, workspace, surface, candidate_id, \
                           source_revision, semantic_text, attempts, created_at, \
                           ROW_NUMBER() OVER ( \
                               PARTITION BY principal, workspace \
                               ORDER BY created_at, outcome_id \
                           ) AS scope_rank \
                    FROM attention_embedding_bind_work \
                    WHERE surface IN ('follow_up', 'worth_a_look') \
                      AND (status = 'pending' \
                       OR (status = 'retry' AND COALESCE(next_retry_at, 0) <= ?) \
                       OR (status = 'in_flight' AND COALESCE(lease_expires_at, 0) <= ?)) \
                 ) \
                 SELECT outcome_id, principal, workspace, surface, candidate_id, \
                        source_revision, semantic_text, attempts \
                 FROM fair \
                 ORDER BY scope_rank, created_at, principal, workspace, outcome_id LIMIT ?",
            )?;
            let rows = statement.query_map(params![now, now, limit], |row| {
                let surface = AttentionSurface::from_str(&row.get::<_, String>(3)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?;
                Ok(PendingAttentionEmbeddingBind {
                    outcome_id: row.get(0)?,
                    principal: row.get(1)?,
                    workspace: row.get(2)?,
                    surface,
                    candidate_id: row.get(4)?,
                    source_revision: row.get(5)?,
                    semantic_text: row.get(6)?,
                    attempts: row.get::<_, i64>(7)?.max(0) as u32,
                })
            })?;
            let work = rows.collect::<std::result::Result<Vec<_>, _>>()?;
            drop(statement);
            for item in &work {
                tx.execute(
                    "UPDATE attention_embedding_bind_work \
                     SET status = 'in_flight', lease_owner = ?, lease_expires_at = ?, \
                         last_attempt_at = ?, updated_at = ? \
                     WHERE outcome_id = ?",
                    params![
                        lease_owner.as_str(),
                        lease_expires_at,
                        now,
                        now,
                        item.outcome_id.as_str(),
                    ],
                )?;
            }
            tx.commit()?;
            Ok(work)
        })
        .await
        .context("attention embedding bind claim task panicked")?
    }

    pub async fn fail_claimed_embedding_bind(
        &self,
        outcome_id: &str,
        lease_owner: &str,
        error_code: &str,
    ) -> Result<()> {
        anyhow::ensure!(
            !error_code.is_empty()
                && error_code.len() <= 64
                && error_code
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_'),
            "attention embedding bind error code must be lowercase snake_case"
        );
        let store = self.clone();
        let outcome_id = outcome_id.to_string();
        let lease_owner = lease_owner.to_string();
        let error_code = error_code.to_string();
        tokio::task::spawn_blocking(move || {
            let mut conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let attempts = tx
                .query_row(
                    "SELECT attempts FROM attention_embedding_bind_work \
                     WHERE outcome_id = ? AND status = 'in_flight' AND lease_owner = ?",
                    params![outcome_id, lease_owner],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            let Some(attempts) = attempts else {
                tx.commit()?;
                return Ok(());
            };
            let attempts = attempts.max(0).saturating_add(1).min(i64::from(u32::MAX));
            let now = chrono::Utc::now().timestamp_millis();
            // Provider availability, timeouts, incomplete batches, and
            // transient binding failures stay retryable indefinitely at the
            // capped cadence. Only deterministic poison rows exhaust into the
            // terminal diagnostic state.
            let is_dead = !embedding_bind_failure_is_retryable(&error_code)
                && attempts >= i64::from(ATTENTION_EMBEDDING_BIND_MAX_ATTEMPTS);
            let next_retry_at = if is_dead {
                None
            } else {
                let exponent = u32::try_from(attempts.saturating_sub(1)).unwrap_or(0).min(6);
                let delay = ATTENTION_EMBEDDING_BIND_RETRY_BASE_MS
                    .saturating_mul(1_i64 << exponent)
                    .min(ATTENTION_EMBEDDING_BIND_RETRY_MAX_MS);
                Some(now.saturating_add(delay))
            };
            tx.execute(
                "UPDATE attention_embedding_bind_work \
                 SET status = ?, attempts = ?, next_retry_at = ?, last_error_code = ?, \
                     last_attempt_at = ?, lease_owner = NULL, lease_expires_at = NULL, updated_at = ? \
                 WHERE outcome_id = ? AND status = 'in_flight' AND lease_owner = ?",
                params![
                    if is_dead { "dead" } else { "retry" },
                    attempts,
                    next_retry_at,
                    error_code,
                    now,
                    now,
                    outcome_id,
                    lease_owner,
                ],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
        .context("attention embedding bind failure task panicked")?
    }

    pub async fn complete_claimed_embedding_bind(
        &self,
        outcome_id: &str,
        lease_owner: &str,
    ) -> Result<bool> {
        let store = self.clone();
        let outcome_id = outcome_id.to_string();
        let lease_owner = lease_owner.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let deleted = conn.execute(
                "DELETE FROM attention_embedding_bind_work \
                 WHERE outcome_id = ? AND status = 'in_flight' AND lease_owner = ?",
                params![outcome_id, lease_owner],
            )?;
            Ok(deleted == 1)
        })
        .await
        .context("attention claimed embedding bind completion task panicked")?
    }

    pub async fn complete_embedding_bind(&self, outcome_id: &str) -> Result<()> {
        let store = self.clone();
        let outcome_id = outcome_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            conn.execute(
                "DELETE FROM attention_embedding_bind_work WHERE outcome_id = ?",
                params![outcome_id],
            )?;
            Ok(())
        })
        .await
        .context("attention embedding bind completion task panicked")?
    }

    pub async fn pending_embedding_bind_count(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<u64> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("pending_embedding_bind_count")?;
            let count = conn.query_row(
                "SELECT COUNT(*) FROM attention_embedding_bind_work WHERE principal = ? AND workspace = ?",
                params![principal, workspace],
                |row| row.get::<_, i64>(0),
            )?;
            Ok(count.max(0) as u64)
        })
        .await
        .context("attention pending embedding bind count task panicked")?
    }

    pub async fn embedding_bind_queue_counts(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<AttentionEmbeddingBindQueueCounts> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("embedding_bind_queue_counts")?;
            let mut counts = AttentionEmbeddingBindQueueCounts::default();
            let mut statement = conn.prepare(
                "SELECT status, COUNT(*), MIN(next_retry_at) \
                 FROM attention_embedding_bind_work \
                 WHERE principal = ? AND workspace = ? GROUP BY status",
            )?;
            let mut rows = statement.query(params![principal, workspace])?;
            while let Some(row) = rows.next()? {
                let status: String = row.get(0)?;
                let count = row.get::<_, i64>(1)?.max(0) as u64;
                match status.as_str() {
                    "pending" => counts.pending = count,
                    "in_flight" => counts.in_flight = count,
                    "retry" => {
                        counts.retry = count;
                        counts.next_retry_at = row.get(2)?;
                    },
                    "dead" => counts.dead = count,
                    _ => {},
                }
            }
            drop(rows);
            drop(statement);
            let mut error_statement = conn.prepare(
                "SELECT last_error_code, COUNT(*) FROM attention_embedding_bind_work \
                 WHERE principal = ? AND workspace = ? AND last_error_code IS NOT NULL \
                 GROUP BY last_error_code ORDER BY last_error_code",
            )?;
            let error_rows = error_statement.query_map(params![principal, workspace], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?;
            for row in error_rows {
                let (error_code, count) = row?;
                counts.error_counts.insert(error_code, count.max(0) as u64);
            }
            Ok(counts)
        })
        .await
        .context("attention embedding bind queue count task panicked")?
    }

    pub async fn embedding_bind_status_for_outcome(
        &self,
        outcome_id: &str,
    ) -> Result<Option<String>> {
        let store = self.clone();
        let outcome_id = outcome_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("embedding_bind_status_for_outcome")?;
            conn.query_row(
                "SELECT status FROM attention_embedding_bind_work WHERE outcome_id = ?",
                params![outcome_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(Into::into)
        })
        .await
        .context("attention embedding bind status task panicked")?
    }

    pub async fn upsert_embedding(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
        candidate: &SemanticAttentionCandidate,
        embedding: &SemanticEmbedding,
        updated_at: i64,
    ) -> Result<()> {
        anyhow::ensure!(
            valid_embedding(&embedding.vector),
            "attention embedding must contain finite non-zero values"
        );
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidate = candidate.clone();
        let embedding = embedding.clone();
        tokio::task::spawn_blocking(move || {
            let vec_json = serde_json::to_string(&embedding.vector)
                .context("serializing attention candidate embedding")?;
            let digest = blake3::hash(candidate.semantic_text.trim().as_bytes())
                .to_hex()
                .to_string();
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            conn.execute(
                "INSERT INTO attention_candidate_embeddings ( \
                    principal, workspace, surface, candidate_id, source_revision, \
                    content_digest, embedding_contract, vec_json, updated_at \
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(principal, workspace, surface, candidate_id) DO UPDATE SET \
                    source_revision = excluded.source_revision, \
                    content_digest = excluded.content_digest, \
                    embedding_contract = excluded.embedding_contract, \
                    vec_json = excluded.vec_json, updated_at = excluded.updated_at",
                params![
                    principal,
                    workspace,
                    surface.as_str(),
                    candidate.candidate_id,
                    candidate.source_revision,
                    digest,
                    embedding.contract,
                    vec_json,
                    updated_at,
                ],
            )
            .context("upserting attention candidate embedding")?;
            Ok(())
        })
        .await
        .context("attention learning upsert_embedding task panicked")?
    }

    /// Parsed embeddings for one scope+surface, memoised across calls.
    ///
    /// The stored vectors are JSON, and a scope can hold tens of megabytes of
    /// them. Every projection resolves embeddings at least once per surface and
    /// the coverage count reads them again, so re-parsing per call dominated
    /// request latency whenever the rows had fallen out of the page cache. The
    /// snapshot is keyed by a cheap `(row count, max updated_at)` probe: any
    /// insert, update, or delete moves one of the two, so a stale snapshot
    /// cannot be served.
    /// Every feature vector captured at serve time for a scope.
    ///
    /// This is the join partner for `attention_outcomes`: an outcome carries
    /// `candidate_id` and `source_revision`, which is exactly this table's key,
    /// so a label and the features it was given are matched by construction
    /// rather than by hoping the source row still exists.
    pub async fn list_candidate_feature_snapshots(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<CapturedCandidateFeatures>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("list_candidate_feature_snapshots")?;
            let mut statement = conn.prepare(
                "SELECT b.surface, b.candidate_id, b.source_revision, v.feature_contract, \
                        v.temporal_contract, v.semantic_schema_version, \
                        v.semantic_extractor_contract, v.semantic_prompt_version, \
                        v.semantic_model, v.semantic_profile, v.features_json, \
                        b.first_served_at, b.content_digest \
                 FROM attention_candidate_feature_bindings b \
                 JOIN attention_feature_vectors v ON v.content_digest = b.content_digest \
                 WHERE b.principal = ? AND b.workspace = ? \
                 UNION ALL \
                 SELECT l.surface, l.candidate_id, l.source_revision, l.feature_contract, \
                        'attention_temporal_continuous_v0', 1, l.semantic_extractor_contract, \
                        l.semantic_prompt_version, NULL, NULL, l.features_json, \
                        l.first_served_at, l.content_digest \
                 FROM attention_candidate_feature_snapshots l \
                 WHERE l.principal = ? AND l.workspace = ? \
                 ORDER BY candidate_id, first_served_at",
            )?;
            let rows = statement.query_map(
                params![principal, workspace, principal, workspace],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, Option<String>>(9)?,
                        row.get::<_, String>(10)?,
                        row.get::<_, i64>(11)?,
                        row.get::<_, String>(12)?,
                    ))
                },
            )?;
            let mut out = Vec::new();
            for row in rows {
                let (
                    surface,
                    candidate_id,
                    source_revision,
                    feature_contract,
                    temporal_contract,
                    semantic_schema_version,
                    semantic_extractor_contract,
                    semantic_prompt_version,
                    semantic_model,
                    semantic_profile,
                    features_json,
                    first_served_at,
                    content_digest,
                ) = row?;
                // A row whose vector will not parse is skipped rather than
                // failing the read: one bad row must not deny a trainer every
                // other usable observation in the scope.
                let Ok(features) = parse_bounded_stored_json::<BTreeMap<String, f64>>(
                    &features_json,
                    "captured attention feature vector",
                    MAX_ATTENTION_FEATURE_VECTOR_BYTES,
                ) else {
                    tracing::warn!(
                        candidate_id,
                        source_revision,
                        "skipping unreadable captured feature vector"
                    );
                    continue;
                };
                out.push(CapturedCandidateFeatures {
                    surface,
                    candidate_id,
                    source_revision,
                    content_digest: if content_digest.is_empty() {
                        blake3::hash(features_json.as_bytes()).to_hex().to_string()
                    } else {
                        content_digest
                    },
                    feature_contract,
                    temporal_contract,
                    semantic_schema_version: u32::try_from(semantic_schema_version.max(0))
                        .unwrap_or(u32::MAX),
                    semantic_extractor_contract,
                    semantic_prompt_version,
                    semantic_model,
                    semantic_profile,
                    features,
                    first_served_at,
                });
            }
            Ok(out)
        })
        .await?
    }

    /// Process at most `limit` legacy feature rows. The returned count includes
    /// successful migrations and deterministic quarantines so a batch drainer
    /// does not mistake quarantine progress for completion. Each processed row
    /// commits independently; a restart resumes from the remaining eligible
    /// legacy rowids.
    pub async fn migrate_legacy_feature_snapshots(&self, limit: usize) -> Result<u64> {
        let limit = limit.clamp(1, 512);
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut processed = 0_u64;
            let mut scan_after_rowid = i64::MIN;
            for _ in 0..limit {
                // Load one legacy body at a time. A row bound alone does not
                // bound retained bytes, and old databases predate the 64 KiB
                // feature-vector admission contract.
                let row = loop {
                    let candidate = {
                        let conn = store
                            .reads
                            .acquire("migrate_legacy_feature_snapshots_scan")?;
                        conn.query_row(
                        "SELECT l.rowid, l.principal, l.workspace, l.surface, l.candidate_id, \
                                l.source_revision, l.feature_contract, \
                                l.semantic_extractor_contract, l.semantic_prompt_version, \
                                l.features_json, l.content_digest, l.first_served_at, \
                                hex(l.principal) || ':' || hex(l.workspace) || ':' || \
                                hex(l.surface) || ':' || hex(l.candidate_id) || ':' || \
                                hex(l.source_revision) || ':' || hex(l.content_digest), \
                                q.source_fingerprint \
                         FROM attention_candidate_feature_snapshots l \
                         LEFT JOIN attention_legacy_migration_quarantine q \
                           ON q.migration_kind = ? AND q.source_key = \
                              hex(l.principal) || ':' || hex(l.workspace) || ':' || \
                              hex(l.surface) || ':' || hex(l.candidate_id) || ':' || \
                              hex(l.source_revision) || ':' || hex(l.content_digest) \
                         WHERE l.rowid > ? ORDER BY l.rowid LIMIT 1",
                        params![LEGACY_FEATURE_MIGRATION_KIND, scan_after_rowid],
                        |row| Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, String>(5)?,
                            row.get::<_, String>(6)?,
                            row.get::<_, String>(7)?,
                            row.get::<_, Option<String>>(8)?,
                            row.get::<_, String>(9)?,
                            row.get::<_, String>(10)?,
                            row.get::<_, i64>(11)?,
                            row.get::<_, String>(12)?,
                            row.get::<_, Option<String>>(13)?,
                        )),
                    ).optional()?
                };
                    let Some(candidate) = candidate else { break None };
                    scan_after_rowid = candidate.0;
                    if candidate
                        .13
                        .as_deref()
                        .is_some_and(|fingerprint| {
                            legacy_migration_payload_fingerprint(&candidate.9) == fingerprint
                        })
                    {
                        continue;
                    }
                    break Some((
                        candidate.0, candidate.1, candidate.2, candidate.3,
                        candidate.4, candidate.5, candidate.6, candidate.7,
                        candidate.8, candidate.9, candidate.10, candidate.11,
                        candidate.12,
                    ));
                };
                let Some(row) = row else { break };
                let parse_error = match parse_bounded_stored_json::<BTreeMap<String, f64>>(
                    &row.9,
                    "legacy attention feature vector",
                    MAX_ATTENTION_FEATURE_VECTOR_BYTES,
                ) {
                    Ok(parsed) => {
                        // Validation is the only use of the decoded map. Drop
                        // it before opening the migration transaction so the
                        // legacy JSON and its full tree are never retained
                        // together through binding updates.
                        drop(parsed);
                        None
                    },
                    Err(error) => Some(error),
                };
                if let Some(error) = parse_error {
                    let source_key = row.12.clone();
                    let error_code = legacy_migration_error_code(&error);
                    let source_fingerprint = legacy_migration_payload_fingerprint(&row.9);
                    let mut conn = store
                        .conn
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
                    let unchanged: i64 = tx.query_row(
                        "SELECT COUNT(*) FROM attention_candidate_feature_snapshots \
                         WHERE rowid = ? AND principal = ? AND workspace = ? AND surface = ? \
                           AND candidate_id = ? AND source_revision = ? AND feature_contract = ? \
                           AND semantic_extractor_contract = ? AND semantic_prompt_version IS ? \
                           AND features_json = ? AND content_digest = ? AND first_served_at = ?",
                        params![
                            row.0, row.1, row.2, row.3, row.4, row.5, row.6,
                            row.7, row.8, row.9, row.10, row.11,
                        ],
                        |record| record.get(0),
                    )?;
                    let quarantined = unchanged == 1;
                    if quarantined {
                        quarantine_legacy_migration_payload(
                            &tx,
                            LEGACY_FEATURE_MIGRATION_KIND,
                            &source_key,
                            &row.1,
                            &row.2,
                            &source_fingerprint,
                            error_code,
                            row.9.len(),
                        )?;
                    }
                    tx.commit()?;
                    processed = processed.saturating_add(1);
                    if quarantined {
                        tracing::warn!(
                            migration_kind = LEGACY_FEATURE_MIGRATION_KIND,
                            source_fingerprint,
                            error_code,
                            "quarantined unreadable legacy attention row"
                        );
                    }
                    continue;
                }
                let content_digest = feature_vector_content_digest(
                    row.6.as_str(),
                    LEGACY_CONTINUOUS_TEMPORAL_FEATURE_CONTRACT,
                    1,
                    row.7.as_str(),
                    row.8.as_deref(),
                    None,
                    None,
                    row.9.as_str(),
                );
                let mut conn = store
                    .conn
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let still_exists: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM attention_candidate_feature_snapshots \
                     WHERE rowid = ? AND principal = ? AND workspace = ? AND surface = ? \
                       AND candidate_id = ? AND source_revision = ? AND feature_contract = ? \
                       AND semantic_extractor_contract = ? AND semantic_prompt_version IS ? \
                       AND features_json = ? AND content_digest = ? AND first_served_at = ?",
                    params![
                        row.0, row.1, row.2, row.3, row.4, row.5, row.6,
                        row.7, row.8, row.9, row.10, row.11,
                    ],
                    |record| record.get(0),
                )?;
                if still_exists == 1 {
                    tx.execute(
                        "INSERT INTO attention_feature_vectors ( \
                            content_digest, feature_contract, temporal_contract, \
                            semantic_schema_version, semantic_extractor_contract, \
                            semantic_prompt_version, semantic_model, semantic_profile, features_json, \
                            size_bytes, created_at \
                         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
                         ON CONFLICT(content_digest) DO NOTHING",
                        params![
                            content_digest,
                            row.6,
                            LEGACY_CONTINUOUS_TEMPORAL_FEATURE_CONTRACT,
                            1_i64,
                            row.7,
                            row.8,
                            Option::<String>::None,
                            Option::<String>::None,
                            row.9,
                            usize_to_i64(row.9.len()),
                            row.11,
                        ],
                    )?;
                    let stored_matches: i64 = tx.query_row(
                        "SELECT COUNT(*) FROM attention_feature_vectors \
                         WHERE content_digest = ? AND feature_contract = ? \
                           AND temporal_contract = ? AND semantic_schema_version = ? \
                           AND semantic_extractor_contract = ? \
                           AND semantic_prompt_version IS ? AND semantic_model IS NULL \
                           AND semantic_profile IS NULL AND features_json = ? \
                           AND size_bytes = ?",
                        params![
                            content_digest,
                            row.6,
                            LEGACY_CONTINUOUS_TEMPORAL_FEATURE_CONTRACT,
                            1_i64,
                            row.7,
                            row.8,
                            row.9,
                            usize_to_i64(row.9.len()),
                        ],
                        |record| record.get(0),
                    )?;
                    anyhow::ensure!(
                        stored_matches == 1,
                        "legacy attention feature digest collision"
                    );
                    let served_surfaces = {
                        let mut statement = tx.prepare(
                            "SELECT DISTINCT i.served_route FROM attention_decision_items i \
                             JOIN attention_decisions d ON d.decision_id = i.decision_id \
                             WHERE i.principal = ? AND i.workspace = ? AND i.candidate_id = ? \
                               AND COALESCE(i.source_revision, '') = ? AND d.decided_at = ?",
                        )?;
                        let values = statement
                            .query_map(params![row.1, row.2, row.4, row.5, row.11], |record| {
                                record.get::<_, String>(0)
                            })?
                            .collect::<std::result::Result<Vec<_>, _>>()?;
                        values
                    };
                    anyhow::ensure!(
                        served_surfaces.len() <= 1,
                        "legacy attention feature row maps to conflicting served lanes"
                    );
                    let binding_surface = served_surfaces
                        .first()
                        .map(String::as_str)
                        .unwrap_or(row.3.as_str());
                    tx.execute(
                        "INSERT INTO attention_candidate_feature_bindings ( \
                            principal, workspace, surface, candidate_id, source_revision, \
                            content_digest, first_served_at, last_served_at \
                         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
                         ON CONFLICT(principal, workspace, surface, candidate_id, source_revision, content_digest) \
                         DO UPDATE SET first_served_at = MIN(first_served_at, excluded.first_served_at), \
                                       last_served_at = MAX(last_served_at, excluded.last_served_at)",
                        params![row.1, row.2, binding_surface, row.4, row.5, content_digest, row.11, row.11],
                    )?;
                    // Legacy `first_served_at` was the exact decision time.
                    // Rebind only that immutable decision/candidate/revision;
                    // choosing a nearest vector would reintroduce the learning
                    // drift this normalization removes.
                    tx.execute(
                        "UPDATE attention_decision_items SET feature_snapshot_digest = ? \
                         WHERE principal = ? AND workspace = ? AND served_route = ? \
                           AND candidate_id = ? AND COALESCE(source_revision, '') = ? \
                           AND decision_id IN (SELECT decision_id FROM attention_decisions \
                               WHERE principal = ? AND workspace = ? AND decided_at = ?)",
                        params![
                            content_digest,
                            row.1,
                            row.2,
                            binding_surface,
                            row.4,
                            row.5,
                            row.1,
                            row.2,
                            row.11,
                        ],
                    )?;
                    let deleted = tx.execute(
                        "DELETE FROM attention_candidate_feature_snapshots \
                         WHERE rowid = ? AND principal = ? AND workspace = ? AND surface = ? \
                           AND candidate_id = ? AND source_revision = ? AND feature_contract = ? \
                           AND semantic_extractor_contract = ? AND semantic_prompt_version IS ? \
                           AND features_json = ? AND content_digest = ? AND first_served_at = ?",
                        params![
                            row.0, row.1, row.2, row.3, row.4, row.5, row.6,
                            row.7, row.8, row.9, row.10, row.11,
                        ],
                    )?;
                    anyhow::ensure!(deleted == 1, "legacy attention feature source changed during migration");
                }
                tx.commit()?;
                processed = processed.saturating_add(1);
            }
            Ok(processed)
        })
        .await
        .context("attention legacy feature snapshot migration task panicked")?
    }

    /// Outcome → served features for training.
    ///
    /// Decision items are keyed by the surface-qualified id (`follow_up:…`)
    /// while Follow-up outcomes often store the raw annotation id. Match both
    /// forms. When the decision has no digest (unselected, or a reconcile
    /// completion with no attribution), fall back to the last-served binding
    /// for that candidate — features only, never an invented propensity id.
    const TRAINING_OUTCOME_FEATURE_FROM: &str = "
                 FROM attention_outcomes o \
                 LEFT JOIN attention_decision_items i \
                   ON i.decision_id = o.decision_id \
                  AND i.principal = o.principal \
                  AND i.workspace = o.workspace \
                  AND (i.candidate_id = o.candidate_id \
                       OR i.candidate_id = o.surface || ':' || o.candidate_id \
                       OR o.candidate_id = i.served_route || ':' || i.candidate_id) \
                 LEFT JOIN attention_feature_vectors v \
                   ON v.content_digest = COALESCE( \
                        i.feature_snapshot_digest, \
                        (SELECT b.content_digest \
                         FROM attention_candidate_feature_bindings b \
                         WHERE b.principal = o.principal \
                           AND b.workspace = o.workspace \
                           AND b.surface = o.surface \
                           AND b.candidate_id IN ( \
                                o.candidate_id, \
                                o.surface || ':' || o.candidate_id, \
                                CASE WHEN substr(o.candidate_id, 1, length(o.surface) + 1) \
                                          = o.surface || ':' \
                                     THEN substr(o.candidate_id, length(o.surface) + 2) END) \
                           AND (o.source_revision IS NULL OR b.source_revision = o.source_revision) \
                         ORDER BY b.last_served_at DESC, b.content_digest DESC \
                         LIMIT 1)) ";

    /// Exact vector used by a persisted decision item. This is the training
    /// join: it follows the immutable digest captured in the decision instead
    /// of selecting whichever candidate variant happens to be newest.
    pub async fn candidate_features_for_decision(
        &self,
        principal: &str,
        workspace: &str,
        decision_id: &str,
        candidate_id: &str,
    ) -> Result<Option<CapturedCandidateFeatures>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let decision_id = decision_id.to_string();
        let candidate_id = candidate_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("candidate_features_for_decision")?;
            let row = conn
                .query_row(
                    "SELECT i.served_route, i.candidate_id, COALESCE(i.source_revision, ''), \
                            v.content_digest, v.feature_contract, v.temporal_contract, \
                            v.semantic_schema_version, v.semantic_extractor_contract, \
                            v.semantic_prompt_version, v.semantic_model, v.semantic_profile, \
                            v.features_json, b.first_served_at \
                     FROM attention_decision_items i \
                     JOIN attention_feature_vectors v \
                       ON v.content_digest = i.feature_snapshot_digest \
                     JOIN attention_candidate_feature_bindings b \
                       ON b.principal = i.principal AND b.workspace = i.workspace \
                      AND b.surface = i.served_route AND b.candidate_id = i.candidate_id \
                      AND b.source_revision = COALESCE(i.source_revision, '') \
                      AND b.content_digest = v.content_digest \
                     WHERE i.principal = ? AND i.workspace = ? AND i.decision_id = ? \
                       AND i.candidate_id = ?",
                    params![principal, workspace, decision_id, candidate_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, String>(5)?,
                            row.get::<_, i64>(6)?,
                            row.get::<_, String>(7)?,
                            row.get::<_, Option<String>>(8)?,
                            row.get::<_, Option<String>>(9)?,
                            row.get::<_, Option<String>>(10)?,
                            row.get::<_, String>(11)?,
                            row.get::<_, i64>(12)?,
                        ))
                    },
                )
                .optional()?;
            let Some(row) = row else { return Ok(None) };
            let features = parse_bounded_stored_json(
                &row.11,
                "decision-bound attention feature vector",
                MAX_ATTENTION_FEATURE_VECTOR_BYTES,
            )?;
            Ok(Some(CapturedCandidateFeatures {
                surface: row.0,
                candidate_id: row.1,
                source_revision: row.2,
                content_digest: row.3,
                feature_contract: row.4,
                temporal_contract: row.5,
                semantic_schema_version: u32::try_from(row.6.max(0)).unwrap_or(u32::MAX),
                semantic_extractor_contract: row.7,
                semantic_prompt_version: row.8,
                semantic_model: row.9,
                semantic_profile: row.10,
                features,
                first_served_at: row.12,
            }))
        })
        .await
        .context("attention decision feature lookup task panicked")?
    }

    pub(super) async fn list_candidate_embeddings(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
    ) -> Result<Arc<HashMap<String, PersistedCandidateEmbedding>>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("list_candidate_embeddings")?;
            let (row_count, latest_updated_at): (i64, i64) = conn.query_row(
                "SELECT COUNT(*), COALESCE(MAX(updated_at), 0) \
                 FROM attention_candidate_embeddings \
                 WHERE principal = ? AND workspace = ? AND surface = ?",
                params![principal, workspace, surface.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let cache_key = (principal.clone(), workspace.clone(), surface.as_str());
            let cache = CANDIDATE_EMBEDDING_CACHE.get_or_init(Default::default);
            let cached = cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&cache_key)
                .map(|(count, updated_at, snapshot)| (*count, *updated_at, Arc::clone(snapshot)));
            if let Some((cached_count, cached_updated_at, snapshot)) = cached.as_ref() {
                if *cached_count == row_count && *cached_updated_at == latest_updated_at {
                    return Ok(Arc::clone(snapshot));
                }
            }
            // Backfill writes embeddings continuously, so an all-or-nothing
            // refresh re-parses the whole corpus for a single new row. Reparse
            // only what changed and carry the rest forward; if the merged count
            // disagrees with the table, rows were deleted and only a full
            // rebuild is safe.
            if let Some((_, cached_updated_at, snapshot)) = cached {
                let mut merged = (*snapshot).clone();
                let mut delta = conn.prepare(
                    "SELECT candidate_id, source_revision, content_digest, embedding_contract, vec_json \
                     FROM attention_candidate_embeddings \
                     WHERE principal = ? AND workspace = ? AND surface = ? AND updated_at > ?",
                )?;
                let rows = delta.query_map(
                    params![principal, workspace, surface.as_str(), cached_updated_at],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    },
                )?;
                for row in rows {
                    let (candidate_id, source_revision, content_digest, contract, vec_json) = row?;
                    let Ok(vector) = parse_bounded_stored_json::<Vec<f32>>(
                        &vec_json,
                        "cached attention candidate embedding",
                        MAX_ATTENTION_STORED_JSON_BYTES,
                    ) else {
                        continue;
                    };
                    if valid_embedding(&vector) {
                        merged.insert(
                            candidate_id,
                            PersistedCandidateEmbedding {
                                source_revision,
                                content_digest,
                                embedding: SemanticEmbedding { contract, vector },
                            },
                        );
                    }
                }
                drop(delta);
                if merged.len() as i64 == row_count {
                    let refreshed = Arc::new(merged);
                    cache
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .insert(
                            cache_key,
                            (row_count, latest_updated_at, Arc::clone(&refreshed)),
                        );
                    return Ok(refreshed);
                }
            }
            let mut stmt = conn.prepare(
                "SELECT candidate_id, source_revision, content_digest, embedding_contract, vec_json \
                 FROM attention_candidate_embeddings \
                 WHERE principal = ? AND workspace = ? AND surface = ?",
            )?;
            let rows = stmt.query_map(params![principal, workspace, surface.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?;
            let mut output = HashMap::new();
            for row in rows {
                let (candidate_id, source_revision, content_digest, contract, vec_json) = row?;
                let Ok(vector) = parse_bounded_stored_json::<Vec<f32>>(
                    &vec_json,
                    "attention candidate embedding",
                    MAX_ATTENTION_STORED_JSON_BYTES,
                ) else {
                    continue;
                };
                if valid_embedding(&vector) {
                    output.insert(
                        candidate_id,
                        PersistedCandidateEmbedding {
                            source_revision,
                            content_digest,
                            embedding: SemanticEmbedding {
                                contract,
                                vector,
                            },
                        },
                    );
                }
            }
            let snapshot = Arc::new(output);
            cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(
                    cache_key,
                    (row_count, latest_updated_at, Arc::clone(&snapshot)),
                );
            Ok(snapshot)
        })
        .await
        .context("attention learning list_candidate_embeddings task panicked")?
    }

    pub async fn list_strong_labels(
        &self,
        principal: &str,
        workspace: &str,
        embedding_contract: &str,
    ) -> Result<Vec<BayesianKnnLabel>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let embedding_contract = embedding_contract.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("list_strong_labels")?;
            let mut stmt = conn.prepare(
                "SELECT o.outcome, e.vec_json \
                 FROM attention_outcomes o \
                 JOIN attention_candidate_embeddings e \
                   ON e.principal = o.principal AND e.workspace = o.workspace \
                  AND e.surface = o.surface AND e.candidate_id = o.candidate_id \
                 WHERE o.principal = ? AND o.workspace = ? \
                   AND o.label_quality = 'strong' \
                   AND e.source_revision IS o.source_revision \
                   AND o.embedding_contract = ? AND e.embedding_contract = ? \
                 ORDER BY o.occurred_at DESC, o.outcome_id DESC",
            )?;
            let rows = stmt.query_map(
                params![principal, workspace, embedding_contract, embedding_contract],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )?;
            let mut labels = Vec::new();
            for row in rows {
                let (outcome, vec_json) = row?;
                let Ok(outcome) = AttentionOutcomeKind::from_str(&outcome) else {
                    continue;
                };
                let Ok(embedding) = parse_bounded_stored_json::<Vec<f32>>(
                    &vec_json,
                    "attention strong-label embedding",
                    MAX_ATTENTION_STORED_JSON_BYTES,
                ) else {
                    continue;
                };
                if valid_embedding(&embedding) {
                    labels.push(BayesianKnnLabel { outcome, embedding });
                }
            }
            Ok(labels)
        })
        .await
        .context("attention learning list_strong_labels task panicked")?
    }

    pub async fn upsert_score(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
        candidate: &SemanticAttentionCandidate,
        embedding_contract: &str,
        estimate: &BayesianKnnEstimate,
        updated_at: i64,
    ) -> Result<bool> {
        self.upsert_scores(
            principal,
            workspace,
            surface,
            vec![(candidate.clone(), *estimate)],
            embedding_contract,
            updated_at,
        )
        .await
    }

    /// Commit one propagation wave under one writer ticket and one SQLite
    /// transaction. Feedback can affect hundreds of active candidates; taking
    /// the global writer once per candidate caused avoidable head-of-line
    /// blocking for every other attention operation.
    pub async fn upsert_scores(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
        scores: Vec<(SemanticAttentionCandidate, BayesianKnnEstimate)>,
        embedding_contract: &str,
        updated_at: i64,
    ) -> Result<bool> {
        if scores.is_empty() {
            return Ok(false);
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let embedding_contract = embedding_contract.to_string();
        tokio::task::spawn_blocking(move || {
            let mut conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .context("opening attention candidate score batch")?;
            let mut changed = false;
            {
                let mut statement = tx.prepare_cached(
                    "INSERT INTO attention_candidate_scores ( \
                    principal, workspace, surface, candidate_id, source_revision, \
                    embedding_contract, usefulness_probability, usefulness_weight, \
                    actionability_probability, actionability_weight, surface_score, updated_at \
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(principal, workspace, surface, candidate_id) DO UPDATE SET \
                    source_revision = excluded.source_revision, \
                    embedding_contract = excluded.embedding_contract, \
                    usefulness_probability = excluded.usefulness_probability, \
                    usefulness_weight = excluded.usefulness_weight, \
                    actionability_probability = excluded.actionability_probability, \
                    actionability_weight = excluded.actionability_weight, \
                    surface_score = excluded.surface_score, updated_at = excluded.updated_at \
                 WHERE attention_candidate_scores.source_revision IS NOT excluded.source_revision \
                    OR attention_candidate_scores.embedding_contract != excluded.embedding_contract \
                    OR attention_candidate_scores.usefulness_probability != excluded.usefulness_probability \
                    OR attention_candidate_scores.usefulness_weight != excluded.usefulness_weight \
                    OR attention_candidate_scores.actionability_probability != excluded.actionability_probability \
                    OR attention_candidate_scores.actionability_weight != excluded.actionability_weight \
                    OR attention_candidate_scores.surface_score IS NOT excluded.surface_score",
                )?;
                for (candidate, estimate) in scores {
                    changed |= statement.execute(params![
                        principal,
                        workspace,
                        surface.as_str(),
                        candidate.candidate_id,
                        candidate.source_revision,
                        embedding_contract,
                        estimate.usefulness.probability,
                        estimate.usefulness.evidence_weight,
                        estimate.actionability.probability,
                        estimate.actionability.evidence_weight,
                        estimate.surface_score(surface),
                        updated_at,
                    ])? > 0;
                }
            }
            tx.commit()
                .context("committing attention candidate score batch")?;
            Ok(changed)
        })
        .await
        .context("attention learning upsert_scores task panicked")?
    }

    pub async fn list_scores(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
        candidates: &[(String, Option<String>)],
    ) -> Result<HashMap<String, Option<f64>>> {
        if candidates.is_empty() {
            return Ok(HashMap::new());
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidates = candidates.to_vec();
        tokio::task::spawn_blocking(move || {
            let expected_revisions: HashMap<String, Option<String>> =
                candidates.iter().cloned().collect();
            let placeholders = std::iter::repeat_n("?", candidates.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT candidate_id, source_revision, surface_score FROM attention_candidate_scores \
                 WHERE principal = ? AND workspace = ? AND surface = ? \
                   AND candidate_id IN ({placeholders})"
            );
            let mut values = vec![
                rusqlite::types::Value::Text(principal),
                rusqlite::types::Value::Text(workspace),
                rusqlite::types::Value::Text(surface.as_str().to_string()),
            ];
            values.extend(
                candidates
                    .into_iter()
                    .map(|(candidate_id, _)| rusqlite::types::Value::Text(candidate_id)),
            );
            let conn = store.reads.acquire("list_scores")?;
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(values), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<f64>>(2)?,
                ))
            })?;
            let mut scores = HashMap::new();
            for row in rows {
                let (candidate_id, source_revision, score) = row?;
                if expected_revisions
                    .get(&candidate_id)
                    .is_some_and(|expected| *expected == source_revision)
                {
                    scores.insert(candidate_id, score.filter(|score| score.is_finite()));
                }
            }
            Ok(scores)
        })
        .await
        .context("attention learning list_scores task panicked")?
    }

    pub async fn list_score_posteriors(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
        candidates: &[(String, Option<String>)],
    ) -> Result<HashMap<String, AttentionScorePosterior>> {
        if candidates.is_empty() {
            return Ok(HashMap::new());
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let candidates = candidates.to_vec();
        tokio::task::spawn_blocking(move || {
            let expected_revisions: HashMap<String, Option<String>> =
                candidates.iter().cloned().collect();
            let placeholders = std::iter::repeat_n("?", candidates.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT candidate_id, source_revision, surface_score, \
                        actionability_probability, actionability_weight \
                 FROM attention_candidate_scores \
                 WHERE principal = ? AND workspace = ? AND surface = ? \
                   AND candidate_id IN ({placeholders})"
            );
            let mut values = vec![
                rusqlite::types::Value::Text(principal),
                rusqlite::types::Value::Text(workspace),
                rusqlite::types::Value::Text(surface.as_str().to_string()),
            ];
            values.extend(
                candidates
                    .into_iter()
                    .map(|(candidate_id, _)| rusqlite::types::Value::Text(candidate_id)),
            );
            let conn = store.reads.acquire("list_score_posteriors")?;
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(values), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<f64>>(2)?,
                    row.get::<_, Option<f64>>(3)?,
                    row.get::<_, Option<f64>>(4)?,
                ))
            })?;
            let mut scores = HashMap::new();
            for row in rows {
                let (candidate_id, source_revision, surface_score, actionability, weight) = row?;
                if expected_revisions
                    .get(&candidate_id)
                    .is_some_and(|expected| *expected == source_revision)
                {
                    scores.insert(
                        candidate_id,
                        AttentionScorePosterior {
                            surface_score: surface_score.filter(|score| score.is_finite()),
                            actionability_probability: actionability
                                .filter(|score| score.is_finite()),
                            actionability_weight: weight
                                .filter(|value| value.is_finite())
                                .unwrap_or(0.0),
                        },
                    );
                }
            }
            Ok(scores)
        })
        .await
        .context("attention learning list_score_posteriors task panicked")?
    }

    pub async fn count_embedded_candidates(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
        candidate_ids: &[String],
    ) -> Result<usize> {
        let available = self
            .list_candidate_embeddings(principal, workspace, surface)
            .await?;
        // Cohorts are canonical surface-qualified ids, but embeddings are
        // persisted under the raw surface-local id. Matching on one form alone
        // intersects to nothing and reports zero coverage for a fully embedded
        // cohort, so accept either spelling of the same candidate.
        let prefix = format!("{}:", surface.as_str());
        let mut unique: HashSet<&str> = HashSet::with_capacity(candidate_ids.len() * 2);
        for candidate_id in candidate_ids {
            unique.insert(candidate_id.as_str());
            if let Some(raw) = candidate_id.strip_prefix(prefix.as_str()) {
                unique.insert(raw);
            }
        }
        Ok(available
            .keys()
            .filter(|candidate_id| unique.contains(candidate_id.as_str()))
            .count())
    }

    pub async fn advance_rank_generation(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
        updated_at: i64,
    ) -> Result<u64> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            conn.execute(
                "INSERT INTO attention_rank_generations ( \
                    principal, workspace, surface, generation, updated_at \
                 ) VALUES (?, ?, ?, 1, ?) \
                 ON CONFLICT(principal, workspace, surface) DO UPDATE SET \
                    generation = generation + 1, updated_at = excluded.updated_at",
                params![principal, workspace, surface.as_str(), updated_at],
            )?;
            let generation: i64 = conn.query_row(
                "SELECT generation FROM attention_rank_generations \
                 WHERE principal = ? AND workspace = ? AND surface = ?",
                params![principal, workspace, surface.as_str()],
                |row| row.get(0),
            )?;
            Ok(generation.max(0) as u64)
        })
        .await
        .context("attention learning advance_rank_generation task panicked")?
    }

    pub async fn rank_generation(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
    ) -> Result<u64> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("rank_generation")?;
            let generation = conn
                .query_row(
                    "SELECT generation FROM attention_rank_generations \
                     WHERE principal = ? AND workspace = ? AND surface = ?",
                    params![principal, workspace, surface.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap_or(0);
            Ok(generation.max(0) as u64)
        })
        .await
        .context("attention learning rank_generation task panicked")?
    }

    /// Read every mutable generation consumed by the canonical projector from
    /// one SQLite statement snapshot. Four independent reads could otherwise
    /// manufacture an identity that never existed while a label writer was
    /// advancing another surface between calls.
    pub async fn canonical_projection_generations(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<[u64; 4]> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("canonical_projection_generations")?;
            let generations = conn.query_row(
                "WITH scoped_generations(kind, surface, generation) AS ( \
                    SELECT 'rank', surface, generation FROM attention_rank_generations \
                     WHERE principal = ? AND workspace = ? \
                    UNION ALL \
                    SELECT 'grouping', surface, generation FROM attention_group_generations \
                     WHERE principal = ? AND workspace = ? \
                 ) \
                 SELECT \
                    COALESCE(MAX(CASE WHEN kind = 'rank' AND surface = 'follow_up' THEN generation END), 0), \
                    COALESCE(MAX(CASE WHEN kind = 'rank' AND surface = 'worth_a_look' THEN generation END), 0), \
                    COALESCE(MAX(CASE WHEN kind = 'grouping' AND surface = 'follow_up' THEN generation END), 0), \
                    COALESCE(MAX(CASE WHEN kind = 'grouping' AND surface = 'worth_a_look' THEN generation END), 0) \
                 FROM scoped_generations",
                params![
                    &principal,
                    &workspace,
                    &principal,
                    &workspace,
                ],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )?;
            Ok([
                generations.0.max(0) as u64,
                generations.1.max(0) as u64,
                generations.2.max(0) as u64,
                generations.3.max(0) as u64,
            ])
        })
        .await
        .context("attention canonical generation snapshot task panicked")?
    }

    /// Install an immutable model snapshot. Replaying the exact same snapshot
    /// is idempotent; reusing an id for different bytes is rejected.
    pub async fn install_actionability_snapshot(
        &self,
        snapshot: &ActionabilityModelSnapshot,
    ) -> Result<()> {
        snapshot.validate()?;
        let encoded =
            serde_json::to_string(snapshot).context("serializing actionability model snapshot")?;
        let digest = blake3::hash(encoded.as_bytes()).to_hex().to_string();
        let store = self.clone();
        let snapshot_id = snapshot.snapshot_id.clone();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let existing = conn.query_row(
                "SELECT content_digest FROM attention_actionability_model_snapshots WHERE snapshot_id = ?",
                params![snapshot_id],
                |row| row.get::<_, String>(0),
            );
            match existing {
                Ok(existing) => {
                    anyhow::ensure!(existing == digest, "actionability snapshot id already exists with different content");
                    Ok(())
                },
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    conn.execute(
                        "INSERT INTO attention_actionability_model_snapshots (snapshot_id, snapshot_json, content_digest, created_at) VALUES (?, ?, ?, ?)",
                        params![snapshot_id, encoded, digest, chrono::Utc::now().timestamp_millis()],
                    )?;
                    Ok(())
                },
                Err(error) => Err(error.into()),
            }
        })
        .await
        .context("attention learning install_actionability_snapshot task panicked")?
    }

    /// Install the immutable artifact and record the serving mode for one
    /// scope. A scope that has never had a snapshot is forced to shadow even
    /// if the caller asked for enforcement: training metrics measure fit, and
    /// only shadow measures behaviour.
    pub async fn install_actionability_snapshot_for_scope(
        &self,
        principal: &str,
        workspace: &str,
        snapshot: &ActionabilityModelSnapshot,
        requested: crate::config::AttentionActionabilityMode,
    ) -> Result<crate::config::AttentionActionabilityMode> {
        self.install_actionability_snapshot(snapshot).await?;
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let snapshot_id = snapshot.snapshot_id.clone();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let first_install: i64 = conn.query_row(
                "SELECT COUNT(*) FROM attention_actionability_scope_installs \
                 WHERE principal = ? AND workspace = ?",
                params![principal, workspace],
                |row| row.get(0),
            )?;
            let effective = if first_install == 0 {
                crate::config::AttentionActionabilityMode::Shadow
            } else {
                requested
            };
            let installed_at = chrono::Utc::now().timestamp_millis();
            conn.execute(
                "INSERT INTO attention_actionability_scope_installs ( \
                    principal, workspace, snapshot_id, requested_mode, effective_mode, installed_at \
                 ) VALUES (?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(principal, workspace) DO UPDATE SET \
                    snapshot_id = excluded.snapshot_id, \
                    requested_mode = excluded.requested_mode, \
                    effective_mode = excluded.effective_mode, \
                    installed_at = excluded.installed_at",
                params![
                    principal,
                    workspace,
                    snapshot_id,
                    requested.as_str(),
                    effective.as_str(),
                    installed_at,
                ],
            )?;
            Ok(effective)
        })
        .await
        .context("attention learning scoped actionability install task panicked")?
    }

    pub async fn actionability_scope_install(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Option<ActionabilityScopeInstall>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("actionability_scope_install")?;
            let row = conn
                .query_row(
                    "SELECT snapshot_id, requested_mode, effective_mode, installed_at \
                     FROM attention_actionability_scope_installs \
                     WHERE principal = ? AND workspace = ?",
                    params![principal, workspace],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)?,
                        ))
                    },
                )
                .optional()?;
            let Some((snapshot_id, requested, effective, installed_at)) = row else {
                return Ok(None);
            };
            Ok(Some(ActionabilityScopeInstall {
                snapshot_id,
                requested_mode: requested.parse()?,
                effective_mode: effective.parse()?,
                installed_at,
            }))
        })
        .await
        .context("attention learning scope install read task panicked")?
    }

    pub async fn list_actionability_training_rows(
        &self,
        principal: &str,
        workspace: &str,
        cutoff_at: i64,
    ) -> Result<Vec<ActionabilityTrainingRow>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("list_actionability_training_rows")?;
            let mut statement = conn.prepare(&format!(
                "SELECT o.outcome_id, o.outcome, o.reason, o.decision_id, o.impression_id, \
                        o.occurred_at, o.candidate_id, o.surface, o.source_revision, \
                        v.feature_contract, v.semantic_extractor_contract, \
                        v.semantic_prompt_version, v.semantic_schema_version, \
                        v.semantic_model, v.semantic_profile, v.features_json \
                 {} \
                 WHERE o.principal = ? AND o.workspace = ? AND o.occurred_at <= ? \
                 ORDER BY o.occurred_at ASC, o.outcome_id ASC",
                Self::TRAINING_OUTCOME_FEATURE_FROM
            ))?;
            let rows = statement
                .query_map(params![principal, workspace, cutoff_at], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, Option<String>>(9)?,
                        row.get::<_, Option<String>>(10)?,
                        row.get::<_, Option<String>>(11)?,
                        row.get::<_, Option<i64>>(12)?,
                        row.get::<_, Option<String>>(13)?,
                        row.get::<_, Option<String>>(14)?,
                        row.get::<_, Option<String>>(15)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows.into_iter()
                .map(|row| {
                    let features = match row.15.as_deref() {
                        Some(encoded) => Some(parse_bounded_stored_json(
                            encoded,
                            "training feature vector",
                            MAX_ATTENTION_FEATURE_VECTOR_BYTES,
                        )?),
                        None => None,
                    };
                    Ok(ActionabilityTrainingRow {
                        outcome_id: row.0,
                        outcome: AttentionOutcomeKind::from_str(&row.1)?,
                        reason: row.2,
                        decision_id: row.3,
                        impression_id: row.4,
                        occurred_at: row.5,
                        candidate_id: row.6,
                        surface: row.7,
                        source_revision: row.8,
                        feature_contract: row.9,
                        semantic_extractor_contract: row.10,
                        semantic_prompt_version: row.11,
                        semantic_schema_version: row
                            .12
                            .map(|value| u32::try_from(value.max(0)).unwrap_or(u32::MAX)),
                        semantic_model: row.13,
                        semantic_profile: row.14,
                        features,
                    })
                })
                .collect()
        })
        .await
        .context("attention actionability training row list task panicked")?
    }

    pub async fn list_routing_training_rows(
        &self,
        principal: &str,
        workspace: &str,
        cutoff_at: i64,
    ) -> Result<Vec<RoutingTrainingRow>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("list_routing_training_rows")?;
            let mut statement = conn.prepare(&format!(
                "SELECT o.outcome, o.reason, o.decision_id, o.occurred_at, o.candidate_id, \
                        o.surface, i.served_route, i.owner_action_required_probability, \
                        i.information_value_probability, i.cluster_size, v.features_json \
                 {} \
                 WHERE o.principal = ? AND o.workspace = ? AND o.occurred_at <= ? \
                 ORDER BY o.occurred_at ASC, o.outcome_id ASC",
                Self::TRAINING_OUTCOME_FEATURE_FROM
            ))?;
            let rows = statement
                .query_map(params![principal, workspace, cutoff_at], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<f64>>(7)?,
                        row.get::<_, Option<f64>>(8)?,
                        row.get::<_, Option<i64>>(9)?,
                        row.get::<_, Option<String>>(10)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows.into_iter()
                .map(|row| {
                    let features = match row.10.as_deref() {
                        Some(encoded) => Some(parse_bounded_stored_json(
                            encoded,
                            "routing training feature vector",
                            MAX_ATTENTION_FEATURE_VECTOR_BYTES,
                        )?),
                        None => None,
                    };
                    Ok(RoutingTrainingRow {
                        outcome: AttentionOutcomeKind::from_str(&row.0)?,
                        reason: row.1,
                        decision_id: row.2,
                        occurred_at: row.3,
                        candidate_id: row.4,
                        surface: row.5,
                        served_route: row.6,
                        owner_action_required_probability: row.7,
                        information_value_probability: row.8,
                        cluster_size: row.9.unwrap_or(1).max(0) as usize,
                        features,
                    })
                })
                .collect()
        })
        .await
        .context("attention routing training row list task panicked")?
    }

    pub async fn install_routing_policy_snapshot_for_scope(
        &self,
        principal: &str,
        workspace: &str,
        snapshot: &crate::magician_v2::attention::learning::AttentionRoutingPolicySnapshot,
        requested: crate::config::AttentionRoutingMode,
    ) -> Result<crate::config::AttentionRoutingMode> {
        self.install_routing_policy_snapshot(snapshot).await?;
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let snapshot_id = snapshot.snapshot_id.clone();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let first_install: i64 = conn.query_row(
                "SELECT COUNT(*) FROM attention_routing_scope_installs \
                 WHERE principal = ? AND workspace = ?",
                params![principal, workspace],
                |row| row.get(0),
            )?;
            let effective = if first_install == 0 {
                crate::config::AttentionRoutingMode::Shadow
            } else {
                requested
            };
            conn.execute(
                "INSERT INTO attention_routing_scope_installs ( \
                    principal, workspace, snapshot_id, requested_mode, effective_mode, installed_at \
                 ) VALUES (?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(principal, workspace) DO UPDATE SET \
                    snapshot_id = excluded.snapshot_id, \
                    requested_mode = excluded.requested_mode, \
                    effective_mode = excluded.effective_mode, \
                    installed_at = excluded.installed_at",
                params![
                    principal,
                    workspace,
                    snapshot_id,
                    requested.as_str(),
                    effective.as_str(),
                    chrono::Utc::now().timestamp_millis(),
                ],
            )?;
            Ok(effective)
        })
        .await
        .context("attention routing scoped install task panicked")?
    }

    pub async fn routing_scope_install(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Option<(String, crate::config::AttentionRoutingMode, i64)>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("routing_scope_install")?;
            let row = conn
                .query_row(
                    "SELECT snapshot_id, effective_mode, installed_at \
                     FROM attention_routing_scope_installs \
                     WHERE principal = ? AND workspace = ?",
                    params![principal, workspace],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    },
                )
                .optional()?;
            let Some((snapshot_id, effective, installed_at)) = row else {
                return Ok(None);
            };
            let mode = effective
                .parse()
                .unwrap_or(crate::config::AttentionRoutingMode::Baseline);
            Ok(Some((snapshot_id, mode, installed_at)))
        })
        .await
        .context("attention routing scope install read task panicked")?
    }

    /// Install the immutable artifact and record the serving mode for one
    /// scope. A new snapshot id is always forced to shadow so a prior-only
    /// policy cannot shuffle lists. Reinstalling the same id may promote.
    pub async fn install_bandit_policy_snapshot_for_scope(
        &self,
        principal: &str,
        workspace: &str,
        snapshot: &AttentionBanditPolicySnapshot,
        requested: crate::config::AttentionBanditMode,
    ) -> Result<crate::config::AttentionBanditMode> {
        self.install_bandit_policy_snapshot(snapshot).await?;
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let snapshot_id = snapshot.snapshot_id.clone();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let existing: Option<String> = conn
                .query_row(
                    "SELECT snapshot_id FROM attention_bandit_scope_installs \
                     WHERE principal = ? AND workspace = ?",
                    params![principal, workspace],
                    |row| row.get(0),
                )
                .optional()?;
            let effective = if existing.as_deref() != Some(snapshot_id.as_str()) {
                crate::config::AttentionBanditMode::Shadow
            } else {
                requested
            };
            conn.execute(
                "INSERT INTO attention_bandit_scope_installs ( \
                    principal, workspace, snapshot_id, requested_mode, effective_mode, installed_at \
                 ) VALUES (?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(principal, workspace) DO UPDATE SET \
                    snapshot_id = excluded.snapshot_id, \
                    requested_mode = excluded.requested_mode, \
                    effective_mode = excluded.effective_mode, \
                    installed_at = excluded.installed_at",
                params![
                    principal,
                    workspace,
                    snapshot_id,
                    requested.as_str(),
                    effective.as_str(),
                    chrono::Utc::now().timestamp_millis(),
                ],
            )?;
            Ok(effective)
        })
        .await
        .context("attention bandit scoped install task panicked")?
    }

    pub async fn bandit_scope_install(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Option<BanditScopeInstall>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("bandit_scope_install")?;
            let row = conn
                .query_row(
                    "SELECT snapshot_id, requested_mode, effective_mode, installed_at \
                     FROM attention_bandit_scope_installs \
                     WHERE principal = ? AND workspace = ?",
                    params![principal, workspace],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)?,
                        ))
                    },
                )
                .optional()?;
            let Some((snapshot_id, requested, effective, installed_at)) = row else {
                return Ok(None);
            };
            Ok(Some(BanditScopeInstall {
                snapshot_id,
                requested_mode: requested.parse()?,
                effective_mode: effective.parse()?,
                installed_at,
            }))
        })
        .await
        .context("attention bandit scope install read task panicked")?
    }

    pub async fn record_training_run(&self, record: &AttentionTrainingRunRecord) -> Result<()> {
        let store = self.clone();
        let record = record.clone();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            conn.execute(
                "INSERT INTO attention_training_runs ( \
                    run_id, principal, workspace, slice, status, reason, \
                    metrics_json, snapshot_id, created_at \
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    record.run_id,
                    record.principal,
                    record.workspace,
                    record.slice,
                    record.status,
                    record.reason,
                    record.metrics_json,
                    record.snapshot_id,
                    record.created_at,
                ],
            )?;
            Ok(())
        })
        .await
        .context("attention training run persist task panicked")?
    }

    pub async fn latest_training_run(
        &self,
        principal: &str,
        workspace: &str,
        slice: &str,
    ) -> Result<Option<AttentionTrainingRunRecord>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let slice = slice.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("latest_training_run")?;
            let row = conn
                .query_row(
                    "SELECT run_id, principal, workspace, slice, status, reason, \
                            metrics_json, snapshot_id, created_at \
                     FROM attention_training_runs \
                     WHERE principal = ? AND workspace = ? AND slice = ? \
                     ORDER BY created_at DESC, run_id DESC LIMIT 1",
                    params![principal, workspace, slice],
                    |row| {
                        Ok(AttentionTrainingRunRecord {
                            run_id: row.get(0)?,
                            principal: row.get(1)?,
                            workspace: row.get(2)?,
                            slice: row.get(3)?,
                            status: row.get(4)?,
                            reason: row.get(5)?,
                            metrics_json: row.get(6)?,
                            snapshot_id: row.get(7)?,
                            created_at: row.get(8)?,
                        })
                    },
                )
                .optional()?;
            Ok(row)
        })
        .await
        .context("attention training run read task panicked")?
    }

    pub async fn get_actionability_snapshot(
        &self,
        snapshot_id: &str,
    ) -> Result<Option<ActionabilityModelSnapshot>> {
        let store = self.clone();
        let snapshot_id = snapshot_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("get_actionability_snapshot")?;
            let encoded = conn.query_row(
                "SELECT snapshot_json FROM attention_actionability_model_snapshots WHERE snapshot_id = ?",
                params![snapshot_id],
                |row| row.get::<_, String>(0),
            );
            match encoded {
                Ok(encoded) => {
                    let snapshot: ActionabilityModelSnapshot = parse_bounded_stored_json(
                        &encoded,
                        "stored actionability snapshot",
                        MAX_ATTENTION_STORED_JSON_BYTES,
                    )?;
                    snapshot.validate()?;
                    Ok(Some(snapshot))
                },
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(error) => Err(error.into()),
            }
        })
        .await
        .context("attention learning get_actionability_snapshot task panicked")?
    }

    pub async fn upsert_actionability_score(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
        candidate_id: &str,
        source_revision: Option<&str>,
        inference: &ActionabilityInference,
        updated_at: i64,
    ) -> Result<()> {
        self.upsert_actionability_scores(
            principal,
            workspace,
            surface,
            vec![(
                candidate_id.to_string(),
                source_revision.map(str::to_string),
                inference.clone(),
            )],
            updated_at,
        )
        .await
    }

    pub async fn upsert_actionability_scores(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
        scores: Vec<(String, Option<String>, ActionabilityInference)>,
        updated_at: i64,
    ) -> Result<()> {
        if scores.is_empty() {
            return Ok(());
        }
        for (_, _, inference) in &scores {
            anyhow::ensure!(
                inference.probability.is_finite() && (0.0..=1.0).contains(&inference.probability),
                "invalid calibrated actionability probability"
            );
            anyhow::ensure!(
                !inference.input_digest.is_empty(),
                "actionability inference input digest is empty"
            );
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let mut conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .context("opening attention actionability score batch")?;
            {
                let mut statement = tx.prepare_cached(
                    "INSERT INTO attention_actionability_scores (principal, workspace, surface, candidate_id, source_revision, snapshot_id, model_version, probability, explanation_code, explanation_label, input_digest, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(principal, workspace, surface, candidate_id, snapshot_id) DO UPDATE SET \
                   source_revision = excluded.source_revision, model_version = excluded.model_version, \
                   probability = excluded.probability, explanation_code = excluded.explanation_code, \
                   explanation_label = excluded.explanation_label, input_digest = excluded.input_digest, \
                   updated_at = excluded.updated_at",
                )?;
                for (candidate_id, source_revision, inference) in scores {
                    statement.execute(params![
                        principal,
                        workspace,
                        surface.as_str(),
                        candidate_id,
                        source_revision,
                        inference.snapshot_id,
                        inference.model_version,
                        inference.probability,
                        inference.explanation.code,
                        inference.explanation.label,
                        inference.input_digest,
                        updated_at,
                    ])?;
                }
            }
            tx.commit()
                .context("committing attention actionability score batch")?;
            Ok(())
        })
        .await
        .context("attention learning upsert_actionability_scores task panicked")?
    }

    pub async fn list_actionability_scores(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
        snapshot_id: &str,
        candidates: &[(String, Option<String>, String)],
    ) -> Result<HashMap<String, PersistedActionabilityScore>> {
        if candidates.is_empty() {
            return Ok(HashMap::new());
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let snapshot_id = snapshot_id.to_string();
        let candidates = candidates.to_vec();
        tokio::task::spawn_blocking(move || {
            let expected_inputs: HashMap<String, (Option<String>, String)> = candidates
                .iter()
                .map(|(candidate_id, revision, digest)| {
                    (candidate_id.clone(), (revision.clone(), digest.clone()))
                })
                .collect();
            let placeholders = std::iter::repeat_n("?", candidates.len()).collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT candidate_id, source_revision, model_version, probability, explanation_code, explanation_label, input_digest \
                 FROM attention_actionability_scores WHERE principal = ? AND workspace = ? AND surface = ? \
                   AND snapshot_id = ? AND candidate_id IN ({placeholders})"
            );
            let mut values = vec![
                rusqlite::types::Value::Text(principal),
                rusqlite::types::Value::Text(workspace),
                rusqlite::types::Value::Text(surface.as_str().to_string()),
                rusqlite::types::Value::Text(snapshot_id.clone()),
            ];
            values.extend(candidates.into_iter().map(|(candidate_id, _, _)| rusqlite::types::Value::Text(candidate_id)));
            let conn = store.reads.acquire("list_actionability_scores")?;
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(values), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, f64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })?;
            let mut output = HashMap::new();
            for row in rows {
                let (candidate_id, source_revision, model_version, probability, code, label, input_digest) = row?;
                if !probability.is_finite() || !(0.0..=1.0).contains(&probability) {
                    continue;
                }
                if !expected_inputs.get(&candidate_id).is_some_and(|(expected_revision, expected_digest)| {
                    expected_revision == &source_revision && expected_digest == &input_digest
                }) {
                    continue;
                }
                output.insert(candidate_id.clone(), PersistedActionabilityScore {
                    candidate_id,
                    source_revision,
                    inference: ActionabilityInference {
                        probability,
                        explanation: ActionabilityExplanation { code, label },
                        model_version,
                        snapshot_id: snapshot_id.clone(),
                        input_digest,
                    },
                });
            }
            Ok(output)
        })
        .await
        .context("attention learning list_actionability_scores task panicked")?
    }

    pub async fn record_pair_label(
        &self,
        principal: &str,
        workspace: &str,
        request: &RecordAttentionPairLabel,
    ) -> Result<PersistedAttentionPairLabel> {
        anyhow::ensure!(
            !request.event_id.trim().is_empty() && request.event_id.chars().count() <= 200,
            "attention pair event_id must contain 1..=200 characters"
        );
        anyhow::ensure!(
            request.confidence.is_finite() && (0.0..=1.0).contains(&request.confidence),
            "attention pair confidence must be within 0..=1"
        );
        let (left, right) = canonical_pair(request.left.clone(), request.right.clone())?;
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let request = request.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tx = conn.transaction()?;
            let existing = tx.query_row(
                "SELECT pair_label_id, surface, left_candidate_id, left_source_revision, \
                        right_candidate_id, right_source_revision, label, source, label_quality, \
                        confidence \
                 FROM attention_pair_labels WHERE principal = ? AND workspace = ? AND event_id = ?",
                params![principal, workspace, request.event_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, f64>(9)?,
                    ))
                },
            );
            match existing {
                Ok((pair_label_id, surface, left_id, left_revision, right_id, right_revision, label, source, quality, confidence)) => {
                    anyhow::ensure!(
                        surface == request.surface.as_str()
                            && left_id == left.candidate_id
                            && left_revision == left.source_revision
                            && right_id == right.candidate_id
                            && right_revision == right.source_revision
                            && label == request.label.as_str()
                            && source == request.source.as_str()
                            && quality == request.label_quality.as_str()
                            && confidence == request.confidence,
                        "attention pair event_id collision with a different payload"
                    );
                    tx.commit()?;
                    return Ok(PersistedAttentionPairLabel {
                        pair_label_id,
                        inserted: false,
                        left,
                        right,
                        label: request.label,
                    });
                },
                Err(rusqlite::Error::QueryReturnedNoRows) => {},
                Err(error) => return Err(error.into()),
            }
            let pair_label_id = uuid::Uuid::new_v4().to_string();
            tx.execute(
                "INSERT INTO attention_pair_labels (pair_label_id, schema_version, event_id, principal, workspace, surface, left_candidate_id, left_source_revision, right_candidate_id, right_source_revision, label, source, label_quality, confidence, occurred_at, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    pair_label_id,
                    i64::from(super::grouping::ATTENTION_PAIR_LABEL_SCHEMA_VERSION),
                    request.event_id,
                    principal,
                    workspace,
                    request.surface.as_str(),
                    left.candidate_id,
                    left.source_revision,
                    right.candidate_id,
                    right.source_revision,
                    request.label.as_str(),
                    request.source.as_str(),
                    request.label_quality.as_str(),
                    request.confidence,
                    request.occurred_at,
                    chrono::Utc::now().timestamp_millis(),
                ],
            )?;
            tx.commit()?;
            Ok(PersistedAttentionPairLabel {
                pair_label_id,
                inserted: true,
                left,
                right,
                label: request.label,
            })
        })
        .await
        .context("attention learning record_pair_label task panicked")?
    }

    pub async fn list_pair_evidence(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
    ) -> Result<Vec<PersistedPairEvidence>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("list_pair_evidence")?;
            let mut statement = conn.prepare(
                "SELECT left_candidate_id, left_source_revision, right_candidate_id, \
                        right_source_revision, label, source, confidence \
                 FROM attention_pair_labels \
                 WHERE principal = ? AND workspace = ? AND surface = ? \
                 ORDER BY occurred_at ASC, pair_label_id ASC",
            )?;
            let rows =
                statement.query_map(params![principal, workspace, surface.as_str()], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, f64>(6)?,
                    ))
                })?;
            rows.map(|row| {
                let (left_id, left_revision, right_id, right_revision, label, source, confidence) =
                    row?;
                Ok(PersistedPairEvidence {
                    left: AttentionPairCandidateRef {
                        candidate_id: left_id,
                        source_revision: left_revision,
                    },
                    right: AttentionPairCandidateRef {
                        candidate_id: right_id,
                        source_revision: right_revision,
                    },
                    label: AttentionPairLabelKind::from_str(&label)?,
                    source: AttentionPairLabelSource::from_str(&source)?,
                    confidence,
                })
            })
            .collect()
        })
        .await
        .context("attention learning list_pair_evidence task panicked")?
    }

    pub async fn install_pair_model_snapshot(
        &self,
        snapshot: &AttentionPairModelSnapshot,
    ) -> Result<()> {
        snapshot.validate()?;
        let encoded = serde_json::to_string(snapshot)?;
        let digest = blake3::hash(encoded.as_bytes()).to_hex().to_string();
        let snapshot_id = snapshot.snapshot_id.clone();
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match conn.query_row(
                "SELECT content_digest FROM attention_pair_model_snapshots WHERE snapshot_id = ?",
                params![snapshot_id],
                |row| row.get::<_, String>(0),
            ) {
                Ok(existing) => {
                    anyhow::ensure!(existing == digest, "pair snapshot id already exists with different content");
                    Ok(())
                },
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    conn.execute(
                        "INSERT INTO attention_pair_model_snapshots (snapshot_id, snapshot_json, content_digest, created_at) VALUES (?, ?, ?, ?)",
                        params![snapshot_id, encoded, digest, chrono::Utc::now().timestamp_millis()],
                    )?;
                    Ok(())
                },
                Err(error) => Err(error.into()),
            }
        })
        .await
        .context("attention learning install_pair_model_snapshot task panicked")?
    }

    pub async fn get_pair_model_snapshot(
        &self,
        snapshot_id: &str,
    ) -> Result<Option<AttentionPairModelSnapshot>> {
        let store = self.clone();
        let snapshot_id = snapshot_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("get_pair_model_snapshot")?;
            match conn.query_row(
                "SELECT snapshot_json FROM attention_pair_model_snapshots WHERE snapshot_id = ?",
                params![snapshot_id],
                |row| row.get::<_, String>(0),
            ) {
                Ok(encoded) => {
                    let snapshot: AttentionPairModelSnapshot = parse_bounded_stored_json(
                        &encoded,
                        "stored attention pair model snapshot",
                        MAX_ATTENTION_STORED_JSON_BYTES,
                    )?;
                    snapshot.validate()?;
                    Ok(Some(snapshot))
                },
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(error) => Err(error.into()),
            }
        })
        .await
        .context("attention learning get_pair_model_snapshot task panicked")?
    }

    pub async fn grouping_generation(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
    ) -> Result<u64> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("grouping_generation")?;
            let generation = conn
                .query_row(
                    "SELECT generation FROM attention_group_generations WHERE principal = ? AND workspace = ? AND surface = ?",
                    params![principal, workspace, surface.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap_or(0);
            Ok(generation.max(0) as u64)
        })
        .await
        .context("attention learning grouping_generation task panicked")?
    }

    pub async fn advance_grouping_generation(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
        updated_at: i64,
    ) -> Result<u64> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            conn.execute(
                "INSERT INTO attention_group_generations (principal, workspace, surface, generation, updated_at) VALUES (?, ?, ?, 1, ?) \
                 ON CONFLICT(principal, workspace, surface) DO UPDATE SET generation = generation + 1, updated_at = excluded.updated_at",
                params![principal, workspace, surface.as_str(), updated_at],
            )?;
            let generation = conn.query_row(
                "SELECT generation FROM attention_group_generations WHERE principal = ? AND workspace = ? AND surface = ?",
                params![principal, workspace, surface.as_str()],
                |row| row.get::<_, i64>(0),
            )?;
            Ok(generation.max(0) as u64)
        })
        .await
        .context("attention learning advance_grouping_generation task panicked")?
    }

    /// Install an immutable routing-policy snapshot. Installation never
    /// changes config or activates serving; the same id may be replayed only
    /// with byte-identical canonical JSON.
    pub async fn install_routing_policy_snapshot(
        &self,
        snapshot: &AttentionRoutingPolicySnapshot,
    ) -> Result<()> {
        snapshot.validate()?;
        let encoded =
            serde_json::to_string(snapshot).context("serializing routing policy snapshot")?;
        let digest = blake3::hash(encoded.as_bytes()).to_hex().to_string();
        let snapshot_id = snapshot.snapshot_id.clone();
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match conn.query_row(
                "SELECT content_digest FROM attention_routing_policy_snapshots WHERE snapshot_id = ?",
                params![snapshot_id],
                |row| row.get::<_, String>(0),
            ) {
                Ok(existing) => {
                    anyhow::ensure!(
                        existing == digest,
                        "routing snapshot id already exists with different content"
                    );
                    Ok(())
                },
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    conn.execute(
                        "INSERT INTO attention_routing_policy_snapshots \
                         (snapshot_id, snapshot_json, content_digest, created_at) \
                         VALUES (?, ?, ?, ?)",
                        params![
                            snapshot_id,
                            encoded,
                            digest,
                            chrono::Utc::now().timestamp_millis()
                        ],
                    )?;
                    Ok(())
                },
                Err(error) => Err(error.into()),
            }
        })
        .await
        .context("attention learning install_routing_policy_snapshot task panicked")?
    }

    pub async fn get_routing_policy_snapshot(
        &self,
        snapshot_id: &str,
    ) -> Result<Option<AttentionRoutingPolicySnapshot>> {
        let store = self.clone();
        let snapshot_id = snapshot_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("get_routing_policy_snapshot")?;
            match conn.query_row(
                "SELECT snapshot_json FROM attention_routing_policy_snapshots WHERE snapshot_id = ?",
                params![snapshot_id],
                |row| row.get::<_, String>(0),
            ) {
                Ok(encoded) => {
                    let snapshot: AttentionRoutingPolicySnapshot = parse_bounded_stored_json(
                        &encoded,
                        "stored routing policy snapshot",
                        MAX_ATTENTION_STORED_JSON_BYTES,
                    )?;
                    snapshot.validate()?;
                    Ok(Some(snapshot))
                },
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(error) => Err(error.into()),
            }
        })
        .await
        .context("attention learning get_routing_policy_snapshot task panicked")?
    }

    /// Install-only, immutable Slice-5 policy artifact. Installation cannot
    /// select the snapshot or mutate runtime configuration.
    pub async fn install_bandit_policy_snapshot(
        &self,
        snapshot: &AttentionBanditPolicySnapshot,
    ) -> Result<()> {
        snapshot.validate()?;
        let encoded =
            serde_json::to_string(snapshot).context("serializing bandit policy snapshot")?;
        let digest = blake3::hash(encoded.as_bytes()).to_hex().to_string();
        let snapshot_id = snapshot.snapshot_id.clone();
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match conn.query_row(
                "SELECT content_digest FROM attention_bandit_policy_snapshots WHERE snapshot_id = ?",
                params![snapshot_id],
                |row| row.get::<_, String>(0),
            ) {
                Ok(existing) => {
                    anyhow::ensure!(
                        existing == digest,
                        "bandit snapshot id already exists with different content"
                    );
                    Ok(())
                },
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    conn.execute(
                        "INSERT INTO attention_bandit_policy_snapshots \
                         (snapshot_id, snapshot_json, content_digest, created_at) \
                         VALUES (?, ?, ?, ?)",
                        params![
                            snapshot_id,
                            encoded,
                            digest,
                            chrono::Utc::now().timestamp_millis()
                        ],
                    )?;
                    Ok(())
                },
                Err(error) => Err(error.into()),
            }
        })
        .await
        .context("attention learning install_bandit_policy_snapshot task panicked")?
    }

    pub async fn get_bandit_policy_snapshot(
        &self,
        snapshot_id: &str,
    ) -> Result<Option<AttentionBanditPolicySnapshot>> {
        let store = self.clone();
        let snapshot_id = snapshot_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("get_bandit_policy_snapshot")?;
            match conn.query_row(
                "SELECT snapshot_json FROM attention_bandit_policy_snapshots WHERE snapshot_id = ?",
                params![snapshot_id],
                |row| row.get::<_, String>(0),
            ) {
                Ok(encoded) => {
                    let snapshot: AttentionBanditPolicySnapshot = parse_bounded_stored_json(
                        &encoded,
                        "stored bandit policy snapshot",
                        MAX_ATTENTION_STORED_JSON_BYTES,
                    )?;
                    snapshot.validate()?;
                    Ok(Some(snapshot))
                },
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(error) => Err(error.into()),
            }
        })
        .await
        .context("attention learning get_bandit_policy_snapshot task panicked")?
    }

    pub async fn get_bandit_posterior(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
        snapshot: &AttentionBanditPolicySnapshot,
    ) -> Result<AttentionBanditPosteriorState> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let snapshot = snapshot.clone();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("get_bandit_posterior")?;
            let encoded = conn.query_row(
                "SELECT state_json FROM attention_bandit_posteriors \
                 WHERE principal = ? AND workspace = ? AND surface = ? AND snapshot_id = ?",
                params![principal, workspace, surface.as_str(), snapshot.snapshot_id],
                |row| row.get::<_, String>(0),
            );
            match encoded {
                Ok(encoded) => {
                    let posterior: AttentionBanditPosteriorState = parse_bounded_stored_json(
                        &encoded,
                        "scoped bandit posterior",
                        MAX_ATTENTION_STORED_JSON_BYTES,
                    )?;
                    posterior.validate(&snapshot)?;
                    Ok(posterior)
                },
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    AttentionBanditPosteriorState::from_prior(&snapshot)
                },
                Err(error) => Err(error.into()),
            }
        })
        .await
        .context("attention learning get_bandit_posterior task panicked")?
    }

    /// Apply one causally attributed outcome at most once. The canonical
    /// outcome must already exist; any mismatch is written as a degraded
    /// immutable update-ledger row and leaves posterior state unchanged.
    pub async fn apply_bandit_outcome_update(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
        persisted: &PersistedAttentionOutcome,
        request: &RecordAttentionOutcome,
        snapshot: &AttentionBanditPolicySnapshot,
    ) -> Result<AttentionPosteriorUpdateReceipt> {
        snapshot.validate()?;
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let persisted = persisted.clone();
        let request = request.clone();
        let snapshot = snapshot.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .context("opening bandit outcome-update transaction")?;
            if let Some(mut receipt) = read_bandit_update_receipt(&tx, &persisted.outcome_id)? {
                receipt.status = AttentionPosteriorUpdateStatus::Duplicate;
                tx.commit().context("committing replayed bandit update")?;
                return Ok(receipt);
            }
            let posterior = read_bandit_posterior_tx(
                &tx,
                &principal,
                &workspace,
                surface,
                &snapshot,
            )?;
            let uncertainty_before = posterior.uncertainty();
            let mapping = snapshot
                .reward(request.outcome)
                .ok_or_else(|| anyhow::anyhow!("canonical outcome has no registered reward mapping"))?;
            let reward = mapping.reward;
            let reward_strength = mapping.strength(request.label_quality);
            let mut quality = AttentionBanditAttributionQuality::Missing;
            let mut degradation_reason = None;
            let mut affected_rank_before = None;
            let mut feature_digest = None;
            let mut updated_posterior = None;

            if reward.is_none() || reward_strength == 0.0 {
                degradation_reason = Some(if reward.is_none() {
                    "outcome_censored_by_task_mapping".to_string()
                } else {
                    "label_quality_has_zero_registered_strength".to_string()
                });
            } else if let Some(attribution) = request.attribution.as_ref() {
                let canonical_candidate_id = format!(
                    "{}:{}",
                    surface.as_str(),
                    request.candidate.candidate_id
                );
                if attribution.candidate_id != request.candidate.candidate_id
                    && attribution.candidate_id != canonical_candidate_id
                {
                    quality = AttentionBanditAttributionQuality::Mismatch;
                    degradation_reason = Some("attribution_candidate_mismatch".to_string());
                } else {
                    // Delivery roots are lane-isolated policy decisions. They
                    // bridge to the canonical routing decision only for the
                    // frozen feature snapshot; attribution identity,
                    // position, propensity, policy and impression remain
                    // delivery-owned.
                    let delivery_binding = tx.query_row(
                        "SELECT d.projection_id, d.lane, d.policy_snapshot_id, \
                                d.policy_model_version, d.posterior_version, d.created_at, \
                                d.expires_at, d.universe_size, d.context_json, i.source_revision, \
                                i.position, i.root_policy_propensity, i.attribution_item_json, \
                                p.delivery_id, p.page_index \
                         FROM attention_delivery_decisions d \
                         JOIN attention_delivery_decision_items i ON i.decision_id = d.decision_id \
                         JOIN attention_delivery_page_items pi ON pi.decision_id = i.decision_id \
                            AND pi.position = i.position \
                         JOIN attention_delivery_pages p ON p.delivery_id = pi.delivery_id \
                         WHERE d.principal = ? AND d.workspace = ? AND d.decision_id = ? \
                           AND i.candidate_id = ? AND (? IS NULL OR p.delivery_id = ?)",
                        params![
                            principal,
                            workspace,
                            attribution.decision_id,
                            attribution.candidate_id,
                            attribution.delivery_id,
                            attribution.delivery_id,
                        ],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, Option<String>>(2)?,
                                row.get::<_, Option<String>>(3)?,
                                row.get::<_, i64>(4)?,
                                row.get::<_, i64>(5)?,
                                row.get::<_, i64>(6)?,
                                row.get::<_, i64>(7)?,
                                row.get::<_, String>(8)?,
                                row.get::<_, Option<String>>(9)?,
                                row.get::<_, i64>(10)?,
                                row.get::<_, f64>(11)?,
                                row.get::<_, Option<String>>(12)?,
                                row.get::<_, String>(13)?,
                                row.get::<_, i64>(14)?,
                            ))
                        },
                    );
                    let delivery_binding = match delivery_binding {
                        Ok(binding) => Some(binding),
                        Err(rusqlite::Error::QueryReturnedNoRows) => None,
                        Err(error) => return Err(error.into()),
                    };
                    let (decision_json, item_json): (
                        rusqlite::Result<String>,
                        rusqlite::Result<String>,
                    ) = if let Some(binding) = delivery_binding.as_ref() {
                        let context: AttentionDecisionContext = parse_bounded_stored_json(
                            &binding.8,
                            "frozen delivery decision context",
                            MAX_ATTENTION_STORED_JSON_BYTES,
                        )?;
                        let lane = AttentionSurface::from_str(&binding.1)?;
                        let synthetic = AttentionDecision {
                            decision_id: attribution.decision_id.clone(),
                            decided_at: binding.5,
                            surface: lane,
                            routing_mode: crate::config::AttentionRoutingMode::Baseline,
                            routing_snapshot_id: None,
                            routing_model_version: None,
                            candidate_set_digest: String::new(),
                            eligible_item_count: binding.7.max(0) as usize,
                            selected_item_count: binding.7.max(0) as usize,
                            returned_item_count: binding.7.max(0) as usize,
                            complete_universe_recorded: true,
                            complete_cross_lane_universe: true,
                            policy_seed_identity: String::new(),
                            canary_assigned: false,
                            context,
                            latency_ms: 0,
                            degradation_reason: None,
                            bandit_health: None,
                        };
                        (
                            Ok(serde_json::to_string(&synthetic)?),
                            binding.12.clone().ok_or(rusqlite::Error::QueryReturnedNoRows),
                        )
                    } else if attribution.delivery_id.is_some() {
                        (
                            Err(rusqlite::Error::QueryReturnedNoRows),
                            Err(rusqlite::Error::QueryReturnedNoRows),
                        )
                    } else {
                        (
                            tx.query_row(
                                "SELECT decision_json FROM attention_decisions \
                                 WHERE principal = ? AND workspace = ? AND decision_id = ?",
                                params![principal, workspace, attribution.decision_id],
                                |row| row.get::<_, String>(0),
                            ),
                            tx.query_row(
                                "SELECT item_json FROM attention_decision_items \
                                 WHERE principal = ? AND workspace = ? AND decision_id = ? AND candidate_id = ?",
                                params![principal, workspace, attribution.decision_id, attribution.candidate_id],
                                |row| row.get::<_, String>(0),
                            ),
                        )
                    };
                    match (decision_json, item_json) {
                        (Ok(decision_json), Ok(item_json)) => {
                            let decision: AttentionDecision = parse_bounded_stored_json(
                                &decision_json,
                                "attributed attention decision",
                                MAX_ATTENTION_STORED_JSON_BYTES,
                            )?;
                            let item: AttentionDecisionItem = parse_bounded_stored_json(
                                &item_json,
                                "attributed attention decision item",
                                MAX_ATTENTION_STORED_JSON_BYTES,
                            )?;
                            affected_rank_before = Some(
                                delivery_binding
                                    .as_ref()
                                    .map(|binding| binding.10.max(0) as usize)
                                    .unwrap_or(item.served_rank),
                            );
                            let elapsed = request.occurred_at.saturating_sub(decision.decided_at);
                            if item.candidate_id != attribution.candidate_id
                                || item.source_revision != request.candidate.source_revision
                                || attribution.source_revision != request.candidate.source_revision
                                || item.served_route.as_str() != surface.as_str()
                                || delivery_binding.as_ref().is_some_and(|binding| {
                                    binding.1 != surface.as_str()
                                        || binding.9 != request.candidate.source_revision
                                })
                            {
                                quality = AttentionBanditAttributionQuality::Mismatch;
                                degradation_reason = Some("decision_item_revision_or_surface_mismatch".to_string());
                            } else if elapsed < 0 || elapsed > snapshot.attribution_window_ms {
                                quality = AttentionBanditAttributionQuality::OutsideWindow;
                                degradation_reason = Some("decision_outside_attribution_window".to_string());
                            } else if delivery_binding.as_ref().is_some_and(|binding| {
                                binding.2.as_deref() != Some(snapshot.snapshot_id.as_str())
                                    || binding.3.as_deref() != Some(snapshot.model_version.as_str())
                                    || binding.4 < 0
                                    || binding.4 as u64 > posterior.version
                                    || binding.5 <= 0
                                    || binding.5 >= binding.6
                                    || binding.7 <= 0
                                    || binding.10 <= 0
                                    || !binding.11.is_finite()
                                    || binding.11 <= 0.0
                                    || binding.11 > 1.0
                            }) || delivery_binding.is_none() && (!item.selected
                                || item.served_rank == 0
                                || item.bandit_decision.as_ref().is_none_or(|bandit| {
                                    bandit.policy_snapshot_id.as_deref()
                                        != Some(snapshot.snapshot_id.as_str())
                                        || bandit.policy_model_version.as_deref()
                                            != Some(snapshot.model_version.as_str())
                                        || bandit.posterior_version > posterior.version
                                        || bandit.served_position != item.served_rank
                                        || !bandit.served_propensity.is_finite()
                                        || !(0.0..=1.0).contains(&bandit.served_propensity)
                                        || bandit.served_propensity == 0.0
                                        || !bandit.support
                                        || decision.bandit_health.as_ref().is_none_or(|health| {
                                            health.policy_snapshot_id.as_deref()
                                                != Some(snapshot.snapshot_id.as_str())
                                                || health.posterior_version
                                                    != bandit.posterior_version
                                        })
                                })) {
                                quality = AttentionBanditAttributionQuality::Mismatch;
                                degradation_reason = Some("decision_bandit_contract_mismatch".to_string());
                            } else {
                                quality = AttentionBanditAttributionQuality::DecisionOnly;
                                if let Some(impression_id) = attribution.impression_id.as_deref() {
                                    let impression = tx.query_row(
                                        "SELECT decision_id, candidate_id, source_revision, surface, verified, first_visible_at, \
                                                delivery_id, page_index, position, root_policy_propensity, \
                                                conditional_delivery_propensity \
                                         FROM attention_impressions \
                                         WHERE principal = ? AND workspace = ? AND impression_id = ?",
                                        params![principal, workspace, impression_id],
                                        |row| {
                                            Ok((
                                                row.get::<_, String>(0)?,
                                                row.get::<_, String>(1)?,
                                                row.get::<_, Option<String>>(2)?,
                                                row.get::<_, String>(3)?,
                                                row.get::<_, i64>(4)?,
                                                row.get::<_, i64>(5)?,
                                                row.get::<_, Option<String>>(6)?,
                                                row.get::<_, Option<i64>>(7)?,
                                                row.get::<_, Option<i64>>(8)?,
                                                row.get::<_, Option<f64>>(9)?,
                                                row.get::<_, Option<f64>>(10)?,
                                            ))
                                        },
                                    );
                                    match impression {
                                        Ok((decision_id, candidate_id, revision, impression_surface, verified, visible_at,
                                            delivery_id, page_index, position, impression_root_propensity,
                                            conditional_delivery_propensity))
                                            if decision_id == attribution.decision_id
                                                && candidate_id == attribution.candidate_id
                                                && revision == attribution.source_revision
                                                && impression_surface == surface.as_str()
                                                && verified == 1
                                                && request.occurred_at.saturating_sub(visible_at) >= 0
                                                && request.occurred_at.saturating_sub(visible_at)
                                                    <= snapshot.attribution_window_ms
                                                && delivery_binding.as_ref().is_none_or(|binding| {
                                                    delivery_id.as_deref() == Some(binding.13.as_str())
                                                        && page_index == Some(binding.14)
                                                        && position == Some(binding.10)
                                                        && impression_root_propensity == Some(binding.11)
                                                        && conditional_delivery_propensity == Some(1.0)
                                                }) =>
                                        {
                                            quality = AttentionBanditAttributionQuality::VerifiedImpression;
                                        },
                                        Ok(_) => {
                                            quality = AttentionBanditAttributionQuality::Mismatch;
                                            degradation_reason = Some("impression_identity_or_verification_mismatch".to_string());
                                        },
                                        Err(rusqlite::Error::QueryReturnedNoRows) => {
                                            quality = AttentionBanditAttributionQuality::Mismatch;
                                            degradation_reason = Some("impression_not_found".to_string());
                                        },
                                        Err(error) => return Err(error.into()),
                                    }
                                }
                                if degradation_reason.is_none()
                                    && snapshot.require_verified_impression
                                    && quality
                                        != AttentionBanditAttributionQuality::VerifiedImpression
                                {
                                    degradation_reason = Some("verified_impression_required".to_string());
                                }
                                if degradation_reason.is_none() {
                                    let features = extract_bandit_features(
                                        &snapshot,
                                        &item,
                                        &decision.context,
                                    )?;
                                    let encoded_features = serde_json::to_vec(&features)?;
                                    feature_digest = Some(
                                        blake3::hash(&encoded_features).to_hex().to_string(),
                                    );
                                    updated_posterior = Some(posterior.update(
                                        &snapshot,
                                        &features,
                                        reward.unwrap_or_default(),
                                        reward_strength,
                                        request.occurred_at,
                                    )?);
                                }
                            }
                        },
                        (Err(rusqlite::Error::QueryReturnedNoRows), _)
                        | (_, Err(rusqlite::Error::QueryReturnedNoRows)) => {
                            quality = AttentionBanditAttributionQuality::Mismatch;
                            degradation_reason = Some("decision_item_not_found".to_string());
                        },
                        (Err(error), _) | (_, Err(error)) => return Err(error.into()),
                    }
                }
            } else {
                degradation_reason = Some("missing_decision_attribution".to_string());
            }

            let update_applied = updated_posterior.is_some();
            let posterior_after = updated_posterior.as_ref().unwrap_or(&posterior);
            if let Some(updated) = updated_posterior.as_ref() {
                let state_json = serde_json::to_string(updated)?;
                let changed = tx.execute(
                    "INSERT INTO attention_bandit_posteriors \
                     (principal, workspace, surface, snapshot_id, version, update_count, state_json, updated_at) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
                     ON CONFLICT(principal, workspace, surface, snapshot_id) DO UPDATE SET \
                       version = excluded.version, update_count = excluded.update_count, \
                       state_json = excluded.state_json, updated_at = excluded.updated_at \
                     WHERE attention_bandit_posteriors.version = ?",
                    params![
                        principal,
                        workspace,
                        surface.as_str(),
                        snapshot.snapshot_id,
                        u64_to_i64(updated.version),
                        u64_to_i64(updated.update_count),
                        state_json,
                        updated.updated_at,
                        u64_to_i64(posterior.version),
                    ],
                )?;
                anyhow::ensure!(
                    changed == 1,
                    "bandit posterior version changed during atomic update"
                );
            }
            tx.execute(
                "INSERT INTO attention_bandit_updates ( \
                    outcome_id, principal, workspace, surface, snapshot_id, decision_id, \
                    candidate_id, source_revision, impression_id, attribution_quality, \
                    degradation_reason, reward, reward_strength, feature_digest, \
                    posterior_version_before, posterior_version_after, uncertainty_before, \
                    uncertainty_after, affected_rank_before, update_applied, occurred_at, created_at \
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    persisted.outcome_id,
                    principal,
                    workspace,
                    surface.as_str(),
                    snapshot.snapshot_id,
                    request.attribution.as_ref().map(|value| &value.decision_id),
                    request.candidate.candidate_id,
                    request.candidate.source_revision,
                    request
                        .attribution
                        .as_ref()
                        .and_then(|value| value.impression_id.as_ref()),
                    quality.as_str(),
                    degradation_reason,
                    reward,
                    reward_strength,
                    feature_digest,
                    u64_to_i64(posterior.version),
                    u64_to_i64(posterior_after.version),
                    uncertainty_before,
                    posterior_after.uncertainty(),
                    affected_rank_before.map(usize_to_i64),
                    bool_to_i64(update_applied),
                    request.occurred_at,
                    chrono::Utc::now().timestamp_millis(),
                ],
            )?;
            let receipt = AttentionPosteriorUpdateReceipt {
                status: if update_applied {
                    AttentionPosteriorUpdateStatus::Updated
                } else if reward.is_none() || reward_strength == 0.0 {
                    AttentionPosteriorUpdateStatus::Neutral
                } else {
                    AttentionPosteriorUpdateStatus::Degraded
                },
                policy_snapshot_id: Some(snapshot.snapshot_id.clone()),
                posterior_version_before: Some(posterior.version),
                posterior_version_after: Some(posterior_after.version),
                attribution_quality: quality,
                degradation_reason,
                uncertainty_before: Some(uncertainty_before),
                uncertainty_after: Some(posterior_after.uncertainty()),
                affected_rank_before,
                affected_rank_after: None,
                affected_rank_delta: None,
                rescore_scheduled: update_applied,
            };
            tx.commit().context("committing bandit outcome update")?;
            Ok(receipt)
        })
        .await
        .context("attention learning apply_bandit_outcome_update task panicked")?
    }

    /// Atomically commit one decision and every evaluated item. Callers must
    /// await this method before returning a list response. A partial decision
    /// is never visible because both tables are written in one transaction and
    /// the inserted item count is reconciled before commit.
    pub async fn record_decision(
        &self,
        principal: &str,
        workspace: &str,
        candidate_set_digest: &str,
        context: AttentionDecisionContext,
        latency_ms: u64,
        returned_item_count: usize,
        evaluation: &AttentionRoutingEvaluation,
    ) -> Result<AttentionDecision> {
        anyhow::ensure!(
            !evaluation.decision_id.trim().is_empty(),
            "attention decision id is empty"
        );
        anyhow::ensure!(
            !candidate_set_digest.trim().is_empty(),
            "attention decision candidate-set digest is empty"
        );
        let unique: HashSet<&str> = evaluation
            .items
            .iter()
            .map(|item| item.candidate_id.as_str())
            .collect();
        anyhow::ensure!(
            unique.len() == evaluation.items.len(),
            "attention decision contains duplicate candidate ids"
        );
        anyhow::ensure!(
            evaluation
                .items
                .iter()
                .all(|item| item.decision_id == evaluation.decision_id),
            "attention decision item belongs to a different decision"
        );
        let selected_item_count = evaluation.items.iter().filter(|item| item.selected).count();
        anyhow::ensure!(
            returned_item_count == selected_item_count,
            "returned attention item count must equal selected item count"
        );
        anyhow::ensure!(
            evaluation.items.iter().all(|item| {
                let probability_valid = item.selection_probability.is_finite()
                    && (0.0..=1.0).contains(&item.selection_probability)
                    && if item.selected {
                        item.selection_probability > 0.0 && item.served_rank > 0
                    } else {
                        item.selection_probability == 0.0
                    };
                let bandit_consistent = item.bandit_decision.as_ref().map_or_else(
                    || {
                        item.selection_probability == if item.selected { 1.0 } else { 0.0 }
                            && !item.exploration
                    },
                    |bandit| {
                        bandit.served_position == item.served_rank
                            && bandit.served_propensity == item.selection_probability
                            && bandit.exploration == item.exploration
                    },
                );
                probability_valid && bandit_consistent
            }),
            "attention decision selection/propensity metadata is inconsistent"
        );
        let decision = AttentionDecision {
            decision_id: evaluation.decision_id.clone(),
            decided_at: evaluation.decided_at,
            surface: evaluation.surface,
            routing_mode: evaluation.mode,
            routing_snapshot_id: evaluation.snapshot_id.clone(),
            routing_model_version: evaluation.model_version.clone(),
            candidate_set_digest: candidate_set_digest.to_string(),
            eligible_item_count: evaluation.items.len(),
            selected_item_count,
            returned_item_count,
            complete_universe_recorded: true,
            complete_cross_lane_universe: evaluation.complete_cross_lane_universe,
            policy_seed_identity: evaluation.policy_seed_identity.clone(),
            canary_assigned: evaluation.canary_assigned,
            context,
            latency_ms,
            degradation_reason: evaluation.degradation_reason.clone(),
            bandit_health: evaluation.bandit_health.clone(),
        };
        let items = evaluation.items.clone();
        let decision_json = serde_json::to_string(&decision)?;
        let context_json = serde_json::to_string(&decision.context)?;
        let baseline_route_summary_json =
            route_summary_json(items.iter().map(|item| item.baseline_route))?;
        let learned_route_summary_json =
            route_summary_json(items.iter().map(|item| item.learned_route))?;
        let bandit_health_json = decision
            .bandit_health
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let mut prepared_items = Vec::with_capacity(items.len());
        for item in items {
            let contracts_json = serde_json::to_string(&item.feature_contracts)?;
            let bandit_decision_json = item
                .bandit_decision
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?;
            let item_json = serde_json::to_string(&item)?;
            let feature = if let (true, Some(values), Some(_source_revision)) = (
                item.selected,
                item.feature_values.as_ref(),
                item.source_revision.as_deref(),
            ) {
                let features_json = serde_json::to_string(values)?;
                anyhow::ensure!(
                    features_json.len() <= MAX_ATTENTION_FEATURE_VECTOR_BYTES,
                    "attention feature vector exceeds the 64 KiB admission limit"
                );
                let content_digest = feature_vector_content_digest(
                    super::actionability::ACTIONABILITY_FEATURE_CONTRACT,
                    ATTENTION_TEMPORAL_FEATURE_CONTRACT,
                    item.feature_contracts.semantic_schema_version,
                    item.feature_contracts.semantic_extractor_contract.as_str(),
                    item.feature_contracts.semantic_prompt_version.as_deref(),
                    item.feature_contracts.semantic_model.as_deref(),
                    item.feature_contracts.semantic_profile.as_deref(),
                    features_json.as_str(),
                );
                Some(PreparedFeatureVector {
                    content_digest,
                    features_json,
                })
            } else {
                None
            };
            prepared_items.push(PreparedAttentionDecisionItem {
                item,
                contracts_json,
                bandit_decision_json,
                item_json,
                feature,
            });
        }
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let transaction_started = Instant::now();
            // A decision is recorded on the page read that made it. It waits
            // a bounded time for the writer and otherwise fails closed to the
            // caller's degraded path; it must never sit behind maintenance.
            let mut conn = store.conn.lock_within(REQUEST_PATH_WRITER_WAIT)?;
            let tx = conn
                .transaction()
                .context("opening attention decision transaction")?;
            let mut feature_vectors_inserted = 0_u64;
            let mut feature_vectors_reused = 0_u64;
            tx.execute(
                "INSERT INTO attention_decisions ( \
                    decision_id, schema_version, principal, workspace, surface, decided_at, \
                    policy_mode, policy_snapshot_id, policy_model_version, candidate_set_digest, \
                    eligible_item_count, selected_item_count, returned_item_count, \
                    complete_universe_recorded, complete_cross_lane_universe, context_json, policy_seed_identity, \
                    canary_assigned, baseline_route_summary_json, learned_route_summary_json, \
                    latency_ms, degradation_reason, bandit_health_json, decision_json, created_at \
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    decision.decision_id,
                    i64::from(ATTENTION_ROUTING_DECISION_SCHEMA_VERSION),
                    principal,
                    workspace,
                    decision.surface.as_str(),
                    decision.decided_at,
                    decision.routing_mode.as_str(),
                    decision.routing_snapshot_id,
                    decision.routing_model_version,
                    decision.candidate_set_digest,
                    usize_to_i64(decision.eligible_item_count),
                    usize_to_i64(decision.selected_item_count),
                    usize_to_i64(decision.returned_item_count),
                    bool_to_i64(decision.complete_universe_recorded),
                    bool_to_i64(decision.complete_cross_lane_universe),
                    context_json,
                    decision.policy_seed_identity,
                    bool_to_i64(decision.canary_assigned),
                    baseline_route_summary_json,
                    learned_route_summary_json,
                    u64_to_i64(decision.latency_ms),
                    decision.degradation_reason,
                    bandit_health_json,
                    decision_json,
                    chrono::Utc::now().timestamp_millis(),
                ],
            )
            .context("inserting attention decision")?;
            for prepared in &prepared_items {
                let item = &prepared.item;
                let mut persisted_feature_digest = None;
                // Capture the features this candidate was served with, once per
                // distinct VECTOR rather than once per revision. The same
                // universe is re-decided constantly, so `OR IGNORE` still costs
                // one index probe on a repeat -- but a candidate whose features
                // actually changed now records the new variant instead of being
                // swallowed by the first one, which is what the revision-only
                // key did. A candidate with no revision is skipped: there is
                // nothing to anchor the vector to.
                if let (Some(feature), Some(source_revision)) =
                    (prepared.feature.as_ref(), item.source_revision.as_deref())
                {
                    let content_digest = feature.content_digest.as_str();
                    let features_json = feature.features_json.as_str();
                    let inserted_vector = tx.execute(
                        "INSERT INTO attention_feature_vectors ( \
                            content_digest, feature_contract, temporal_contract, \
                            semantic_schema_version, semantic_extractor_contract, \
                            semantic_prompt_version, semantic_model, semantic_profile, features_json, \
                            size_bytes, created_at \
                         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
                         ON CONFLICT(content_digest) DO NOTHING",
                        params![
                            content_digest,
                            super::actionability::ACTIONABILITY_FEATURE_CONTRACT,
                            ATTENTION_TEMPORAL_FEATURE_CONTRACT,
                            i64::from(item.feature_contracts.semantic_schema_version),
                            item.feature_contracts.semantic_extractor_contract,
                            item.feature_contracts.semantic_prompt_version,
                            item.feature_contracts.semantic_model,
                            item.feature_contracts.semantic_profile,
                            features_json,
                            usize_to_i64(features_json.len()),
                            decision.decided_at,
                        ],
                    )
                    .context("recording content-addressed attention feature vector")?;
                    if inserted_vector == 1 {
                        feature_vectors_inserted = feature_vectors_inserted.saturating_add(1);
                    } else {
                        feature_vectors_reused = feature_vectors_reused.saturating_add(1);
                        let stored_matches: i64 = tx.query_row(
                            "SELECT COUNT(*) FROM attention_feature_vectors \
                             WHERE content_digest = ? AND feature_contract = ? \
                               AND temporal_contract = ? AND semantic_schema_version = ? \
                               AND semantic_extractor_contract = ? \
                               AND semantic_prompt_version IS ? AND semantic_model IS ? \
                               AND semantic_profile IS ? AND features_json = ? AND size_bytes = ?",
                            params![
                                content_digest,
                                super::actionability::ACTIONABILITY_FEATURE_CONTRACT,
                                ATTENTION_TEMPORAL_FEATURE_CONTRACT,
                                i64::from(item.feature_contracts.semantic_schema_version),
                                item.feature_contracts.semantic_extractor_contract,
                                item.feature_contracts.semantic_prompt_version,
                                item.feature_contracts.semantic_model,
                                item.feature_contracts.semantic_profile,
                                features_json,
                                usize_to_i64(features_json.len()),
                            ],
                            |row| row.get(0),
                        )?;
                        anyhow::ensure!(
                            stored_matches == 1,
                            "attention feature digest collision"
                        );
                    }
                    tx.execute(
                        "INSERT INTO attention_candidate_feature_bindings ( \
                            principal, workspace, surface, candidate_id, source_revision, \
                            content_digest, first_served_at, last_served_at \
                         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
                         ON CONFLICT(principal, workspace, surface, candidate_id, source_revision, content_digest) \
                         DO UPDATE SET last_served_at = MAX(last_served_at, excluded.last_served_at)",
                        params![
                            principal,
                            workspace,
                            item.served_route.as_str(),
                            item.candidate_id,
                            source_revision,
                            content_digest,
                            decision.decided_at,
                            decision.decided_at,
                        ],
                    )
                    .context("binding attention candidate to content-addressed feature vector")?;
                    persisted_feature_digest = Some(content_digest);
                }
                tx.execute(
                    "INSERT INTO attention_decision_items ( \
                        decision_id, principal, workspace, candidate_id, source_revision, \
                        source_family, hard_eligible, ineligibility_reason, baseline_route, \
                        learned_route, served_route, routing_mode, routing_snapshot_id, \
                        routing_model_version, learned_route_confidence, utility_margin, \
                        route_reason, route_applied, canary_assigned, \
                        owner_action_required_probability, information_value_probability, \
                        follow_up_utility, worth_a_look_utility, uncertainty, cluster_id, \
                        cluster_size, representative, baseline_rank, learned_rank, served_rank, selected, \
                        selection_probability, exploration, feature_snapshot_digest, \
                        extraction_status, feature_contracts_json, bandit_decision_json, item_json \
                     ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, \
                               ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    params![
                        item.decision_id,
                        principal,
                        workspace,
                        item.candidate_id,
                        item.source_revision,
                        item.source_family,
                        bool_to_i64(item.hard_eligible),
                        item.ineligibility_reason,
                        item.baseline_route.as_str(),
                        item.learned_route.as_str(),
                        item.served_route.as_str(),
                        item.routing_mode.as_str(),
                        item.routing_snapshot_id,
                        item.routing_model_version,
                        item.learned_route_confidence,
                        item.utility_margin,
                        item.route_reason,
                        bool_to_i64(item.route_applied),
                        bool_to_i64(item.canary_assigned),
                        item.owner_action_required_probability,
                        item.information_value_probability,
                        item.follow_up_utility,
                        item.worth_a_look_utility,
                        item.uncertainty,
                        item.cluster_id,
                        usize_to_i64(item.cluster_size),
                        bool_to_i64(item.representative),
                        usize_to_i64(item.baseline_rank),
                        usize_to_i64(item.learned_rank),
                        usize_to_i64(item.served_rank),
                        bool_to_i64(item.selected),
                        item.selection_probability,
                        bool_to_i64(item.exploration),
                        persisted_feature_digest,
                        item.extraction_status.as_str(),
                        prepared.contracts_json,
                        prepared.bandit_decision_json,
                        prepared.item_json,
                    ],
                )
                .context("inserting attention decision item")?;
            }
            let persisted_count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM attention_decision_items WHERE decision_id = ?",
                params![decision.decision_id],
                |row| row.get(0),
            )?;
            anyhow::ensure!(
                persisted_count == usize_to_i64(prepared_items.len()),
                "attention decision item count did not reconcile"
            );
            tx.commit().context("committing attention decision")?;
            tracing::info!(
                decision_id = %decision.decision_id,
                feature_vectors_inserted,
                feature_vectors_reused,
                temporal_contract = ATTENTION_TEMPORAL_FEATURE_CONTRACT,
                transaction_ms = transaction_started.elapsed().as_millis(),
                "committed attention decision feature bindings"
            );
            Ok(decision)
        })
        .await
        .context("attention learning record_decision task panicked")?
    }

    pub async fn get_canonical_projection_json(
        &self,
        principal: &str,
        workspace: &str,
        universe_digest: &str,
        policy_identity: &str,
    ) -> Result<Option<String>> {
        anyhow::ensure!(
            !principal.trim().is_empty(),
            "canonical projection principal is empty"
        );
        anyhow::ensure!(
            !workspace.trim().is_empty(),
            "canonical projection workspace is empty"
        );
        anyhow::ensure!(
            !universe_digest.trim().is_empty(),
            "canonical projection universe digest is empty"
        );
        anyhow::ensure!(
            !policy_identity.trim().is_empty(),
            "canonical projection policy identity is empty"
        );
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let universe_digest = universe_digest.to_string();
        let policy_identity = policy_identity.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("get_canonical_projection_json")?;
            let materialization: Result<Option<CanonicalProjectionMaterialization>> =
                match conn.query_row(
                "SELECT projection_id, schema_version, projection_json \
                 FROM attention_canonical_projections \
                 WHERE principal = ? AND workspace = ? AND universe_digest = ? AND policy_identity = ?",
                params![principal, workspace, universe_digest, policy_identity],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            ) {
                Ok((projection_id, schema_version, projection_json)) => Ok(Some(
                    load_canonical_projection_materialization(
                        &conn,
                        &projection_id,
                        schema_version,
                        projection_json,
                    )?,
                )),
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(error) => Err(error.into()),
                };
            let materialization = materialization?;
            drop(conn);
            materialization
                .map(materialize_canonical_projection_json)
                .transpose()
        })
        .await
        .context("attention learning canonical projection read task panicked")?
    }

    /// The projection a decision was served from.
    ///
    /// A rank recompute is enqueued *because* the owner acted, and acting
    /// removes the candidate from the live projection — so recomputing the
    /// projection and looking for the candidate can never find it. The decision
    /// records the universe it served (`candidate_set_digest`), and projections
    /// are retained under that digest, so the state the owner actually saw is
    /// still recoverable.
    ///
    /// A digest can match more than one stored projection (same universe,
    /// different evaluation). The one at or before the decision, most recent
    /// first, is the one that decision was served from.
    pub async fn get_decision_projection_json(
        &self,
        principal: &str,
        workspace: &str,
        decision_id: &str,
    ) -> Result<Option<String>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let decision_id = decision_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("get_decision_projection_json")?;
            let exact = conn.query_row(
                "SELECT projection_id, schema_version, projection_json FROM ( \
                    SELECT 0 AS source_order, p.projection_id, p.schema_version, \
                           p.projection_json AS projection_json \
                    FROM attention_delivery_decisions d \
                    JOIN attention_canonical_projections p \
                      ON p.projection_id = d.projection_id \
                     AND p.principal = d.principal AND p.workspace = d.workspace \
                    WHERE d.decision_id = ? AND d.principal = ? AND d.workspace = ? \
                    UNION ALL \
                    SELECT 1 AS source_order, p.projection_id, p.schema_version, \
                           p.projection_json AS projection_json \
                    FROM attention_decisions d \
                    JOIN attention_canonical_projections p \
                      ON p.projection_id = d.decision_id \
                     AND p.principal = d.principal AND p.workspace = d.workspace \
                    WHERE d.decision_id = ? AND d.principal = ? AND d.workspace = ? \
                 ) ORDER BY source_order LIMIT 1",
                params![
                    decision_id,
                    principal,
                    workspace,
                    decision_id,
                    principal,
                    workspace,
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            );
            match exact {
                Ok((projection_id, schema_version, projection_json)) => {
                    let materialization = load_canonical_projection_materialization(
                        &conn,
                        &projection_id,
                        schema_version,
                        projection_json,
                    )?;
                    drop(conn);
                    return Ok(Some(materialize_canonical_projection_json(
                        materialization,
                    )?));
                },
                Err(rusqlite::Error::QueryReturnedNoRows) => {},
                Err(error) => return Err(error.into()),
            }

            // Compatibility fallback for decisions written before exact
            // projection identity was retained. New delivery and canonical
            // decisions always resolve through the exact joins above.
            match conn.query_row(
                "SELECT p.projection_id, p.schema_version, p.projection_json \
                 FROM attention_decisions d \
                 JOIN attention_canonical_projections p \
                   ON p.principal = d.principal AND p.workspace = d.workspace \
                  AND p.universe_digest = d.candidate_set_digest \
                 WHERE d.decision_id = ? AND d.principal = ? AND d.workspace = ? \
                   AND p.created_at <= d.decided_at \
                 ORDER BY p.created_at DESC LIMIT 1",
                params![decision_id, principal, workspace],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            ) {
                Ok((projection_id, schema_version, projection_json)) => {
                    let materialization = load_canonical_projection_materialization(
                        &conn,
                        &projection_id,
                        schema_version,
                        projection_json,
                    )?;
                    drop(conn);
                    Ok(Some(materialize_canonical_projection_json(
                        materialization,
                    )?))
                },
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                Err(error) => Err(error.into()),
            }
        })
        .await
        .context("attention learning decision projection read task panicked")?
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn persist_canonical_projection_json(
        &self,
        principal: &str,
        workspace: &str,
        universe_digest: &str,
        policy_identity: &str,
        projection_id: &str,
        created_at: i64,
        projection_json: &str,
    ) -> Result<String> {
        self.persist_canonical_projection_json_owned(
            principal,
            workspace,
            universe_digest,
            policy_identity,
            projection_id,
            created_at,
            projection_json.to_string(),
        )
        .await
    }

    /// Ownership-taking production path. Callers that just encoded a complete
    /// projection can move that allocation across the blocking boundary rather
    /// than retaining one string and cloning a second multi-megabyte copy.
    #[allow(clippy::too_many_arguments)]
    pub async fn persist_canonical_projection_json_owned(
        &self,
        principal: &str,
        workspace: &str,
        universe_digest: &str,
        policy_identity: &str,
        projection_id: &str,
        created_at: i64,
        projection_json: String,
    ) -> Result<String> {
        anyhow::ensure!(
            !principal.trim().is_empty(),
            "canonical projection principal is empty"
        );
        anyhow::ensure!(
            !workspace.trim().is_empty(),
            "canonical projection workspace is empty"
        );
        anyhow::ensure!(
            !universe_digest.trim().is_empty(),
            "canonical projection digest is empty"
        );
        anyhow::ensure!(
            !policy_identity.trim().is_empty(),
            "canonical projection policy is empty"
        );
        anyhow::ensure!(
            !projection_id.trim().is_empty(),
            "canonical projection id is empty"
        );
        anyhow::ensure!(created_at > 0, "canonical projection timestamp is invalid");
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let universe_digest = universe_digest.to_string();
        let policy_identity = policy_identity.to_string();
        let projection_id = projection_id.to_string();
        tokio::task::spawn_blocking(move || {
            {
                let conn = store.reads.acquire("persist_canonical_projection_reuse")?;
                let existing = conn
                    .query_row(
                        "SELECT projection_id, schema_version, projection_json \
                         FROM attention_canonical_projections \
                         WHERE principal = ? AND workspace = ? AND universe_digest = ? \
                           AND policy_identity = ?",
                        params![principal, workspace, universe_digest, policy_identity],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, i64>(1)?,
                                row.get::<_, String>(2)?,
                            ))
                        },
                    )
                    .optional()?;
                if let Some((persisted_id, schema_version, persisted_json)) = existing {
                    anyhow::ensure!(
                        persisted_id == projection_id,
                        "canonical projection deterministic identity mismatch"
                    );
                    tracing::info!(
                        projection_id = %persisted_id,
                        "reused normalized canonical attention projection"
                    );
                    drop(projection_json);
                    let materialization = load_canonical_projection_materialization(
                        &conn,
                        &persisted_id,
                        schema_version,
                        persisted_json,
                    )?;
                    drop(conn);
                    return materialize_canonical_projection_json(materialization);
                }
            }
            let prepare_started = Instant::now();
            let prepared = prepare_normalized_canonical_projection(&projection_json)?;
            let prepare_ms = prepare_started.elapsed().as_millis();
            let source_bytes = projection_json.len();
            // `prepared` owns every normalized component; keeping the encoded
            // monolith through the writer transaction only amplifies heap use.
            drop(projection_json);
            let transaction_started = Instant::now();
            // Serving-path write: bounded wait, see `record_decision`.
            let mut conn = store.conn.lock_within(REQUEST_PATH_WRITER_WAIT)?;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .context("opening canonical attention projection transaction")?;
            let inserted = tx.execute(
                "INSERT INTO attention_canonical_projections ( \
                    projection_id, schema_version, principal, workspace, universe_digest, \
                    policy_identity, projection_json, created_at, updated_at \
                 ) VALUES (?, ?, ?, ?, ?, ?, '', ?, ?) \
                 ON CONFLICT(principal, workspace, universe_digest, policy_identity) DO NOTHING",
                params![
                    projection_id,
                    CURRENT_CANONICAL_PROJECTION_SCHEMA_VERSION,
                    principal,
                    workspace,
                    universe_digest,
                    policy_identity,
                    created_at,
                    chrono::Utc::now().timestamp_millis(),
                ],
            )?;
            let insert_stats = if inserted == 1 {
                insert_normalized_canonical_projection(
                    &tx,
                    &projection_id,
                    created_at,
                    &prepared,
                )?
            } else {
                CanonicalProjectionInsertStats::default()
            };
            let (persisted_id, schema_version, persisted_json): (String, i64, String) = tx.query_row(
                "SELECT projection_id, schema_version, projection_json \
                 FROM attention_canonical_projections \
                 WHERE principal = ? AND workspace = ? AND universe_digest = ? AND policy_identity = ?",
                params![principal, workspace, universe_digest, policy_identity],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            anyhow::ensure!(
                persisted_id == projection_id,
                "canonical projection deterministic identity mismatch"
            );
            tx.commit().context("committing canonical attention projection")?;
            let transaction_ms = transaction_started.elapsed().as_millis();
            let item_count = prepared.members.len();
            let metadata_bytes = prepared.metadata_bytes;
            let item_body_bytes = prepared
                .members
                .iter()
                .map(|member| member.item_json.len())
                .sum::<usize>();
            let diagnostic_body_bytes = prepared
                .members
                .iter()
                .map(|member| {
                    member.rank_json.as_ref().map_or(0, String::len)
                        + member.decision_item_json.as_ref().map_or(0, String::len)
                })
                .sum::<usize>();
            let binding_bytes = prepared
                .members
                .iter()
                .map(|member| member.item_binding_json.len())
                .sum::<usize>();
            // Do not retain either the encoded source or all prepared item
            // strings while reconstructing the compatibility response.
            drop(prepared);
            drop(conn);
            let materialization_started = Instant::now();
            let read = store
                .reads
                .acquire("persist_canonical_projection_materialize")?;
            let materialization = load_canonical_projection_materialization(
                &read,
                &persisted_id,
                schema_version,
                persisted_json,
            )?;
            drop(read);
            let materialized = materialize_canonical_projection_json(materialization)?;
            tracing::info!(
                projection_id = %persisted_id,
                inserted = inserted == 1,
                item_count,
                source_bytes,
                metadata_bytes,
                item_body_bytes,
                diagnostic_body_bytes,
                item_bodies_inserted = insert_stats.item_bodies_inserted,
                item_bodies_reused = insert_stats.item_bodies_reused,
                diagnostic_bodies_inserted = insert_stats.diagnostic_bodies_inserted,
                diagnostic_bodies_reused = insert_stats.diagnostic_bodies_reused,
                binding_bytes,
                prepare_ms,
                transaction_ms,
                materialization_ms = materialization_started.elapsed().as_millis(),
                "persisted normalized canonical attention projection"
            );
            Ok(materialized)
        })
        .await
        .context("attention learning canonical projection persist task panicked")?
    }

    /// Bounded repair for projection references written before outcomes,
    /// impressions, and recompute jobs carried `projection_id` directly. It
    /// intentionally does no unbounded work during service startup.
    pub async fn migrate_legacy_projection_references(&self, limit: usize) -> Result<u64> {
        let limit = limit.clamp(1, 512);
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let impressions = {
                let mut statement = tx.prepare(
                    "SELECT impression_id, principal, workspace, decision_id, delivery_id \
                     FROM attention_impressions WHERE projection_id IS NULL \
                     ORDER BY last_recorded_at, impression_id LIMIT ?",
                )?;
                let rows = statement
                    .query_map(params![limit], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, Option<String>>(4)?,
                        ))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                rows
            };
            let mut migrated = 0_u64;
            for (impression_id, principal, workspace, decision_id, delivery_id) in impressions {
                if let Some(projection_id) = resolve_projection_reference(
                    &tx,
                    &principal,
                    &workspace,
                    Some(&decision_id),
                    None,
                    delivery_id.as_deref(),
                )? {
                    migrated = migrated.saturating_add(tx.execute(
                        "UPDATE attention_impressions SET projection_id = ? \
                         WHERE impression_id = ? AND projection_id IS NULL",
                        params![projection_id, impression_id],
                    )? as u64);
                }
            }
            let outcomes = {
                let mut statement = tx.prepare(
                    "SELECT outcome_id, principal, workspace, decision_id, impression_id, delivery_id \
                     FROM attention_outcomes WHERE projection_id IS NULL \
                       AND (decision_id IS NOT NULL OR impression_id IS NOT NULL OR delivery_id IS NOT NULL) \
                     ORDER BY created_at, outcome_id LIMIT ?",
                )?;
                let rows = statement
                    .query_map(params![limit], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, Option<String>>(4)?,
                            row.get::<_, Option<String>>(5)?,
                        ))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                rows
            };
            for (outcome_id, principal, workspace, decision_id, impression_id, delivery_id) in
                outcomes
            {
                if let Some(projection_id) = resolve_projection_reference(
                    &tx,
                    &principal,
                    &workspace,
                    decision_id.as_deref(),
                    impression_id.as_deref(),
                    delivery_id.as_deref(),
                )? {
                    migrated = migrated.saturating_add(tx.execute(
                        "UPDATE attention_outcomes SET projection_id = ? \
                         WHERE outcome_id = ? AND projection_id IS NULL",
                        params![projection_id, outcome_id],
                    )? as u64);
                }
            }
            let jobs = {
                let mut statement = tx.prepare(
                    "SELECT j.job_id, j.principal, j.workspace, j.decision_id, j.impression_id, \
                            j.delivery_id, o.projection_id \
                     FROM attention_rank_recompute_jobs j \
                     JOIN attention_outcomes o ON o.outcome_id = j.outcome_id \
                     WHERE j.projection_id IS NULL \
                     ORDER BY j.created_at, j.job_id LIMIT ?",
                )?;
                let rows = statement
                    .query_map(params![limit], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, Option<String>>(4)?,
                            row.get::<_, Option<String>>(5)?,
                            row.get::<_, Option<String>>(6)?,
                        ))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                rows
            };
            for (job_id, principal, workspace, decision_id, impression_id, delivery_id, inherited) in
                jobs
            {
                let projection_id = if inherited.is_some() {
                    inherited
                } else {
                    resolve_projection_reference(
                        &tx,
                        &principal,
                        &workspace,
                        decision_id.as_deref(),
                        impression_id.as_deref(),
                        delivery_id.as_deref(),
                    )?
                };
                if let Some(projection_id) = projection_id {
                    migrated = migrated.saturating_add(tx.execute(
                        "UPDATE attention_rank_recompute_jobs SET projection_id = ? \
                         WHERE job_id = ? AND projection_id IS NULL",
                        params![projection_id, job_id],
                    )? as u64);
                }
            }
            tx.commit()?;
            Ok(migrated)
        })
        .await
        .context("attention legacy projection reference migration task panicked")?
    }

    /// Bounded repair path for V1 projection rows. The returned count includes
    /// normalized and deterministically quarantined rows so a batch drainer
    /// cannot stop on quarantine-only progress. Each row commits in its own
    /// transaction, so interruption resumes at the first remaining eligible V1
    /// row and replay is idempotent.
    pub async fn migrate_legacy_canonical_projections(&self, limit: usize) -> Result<u64> {
        let limit = limit.clamp(1, 64);
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut processed = 0_u64;
            let mut scan_after_created_at = i64::MIN;
            let mut scan_after_projection_id = String::new();
            for _ in 0..limit {
                // Historical bodies are multi-megabyte documents. Keep only
                // one in memory while the per-row transaction owns the atomic
                // anchor switch.
                let legacy = loop {
                    let candidate = {
                        let conn = store
                            .reads
                            .acquire("migrate_legacy_canonical_projections_scan")?;
                        conn.query_row(
                            "SELECT p.projection_id, p.principal, p.workspace, p.universe_digest, \
                                p.policy_identity, p.created_at, p.projection_json, \
                                q.source_fingerprint \
                         FROM attention_canonical_projections p \
                         LEFT JOIN attention_legacy_migration_quarantine q \
                           ON q.migration_kind = ? AND q.source_key = p.projection_id \
                         WHERE p.schema_version < ? AND p.projection_json != '' \
                           AND (p.created_at > ? OR (p.created_at = ? AND p.projection_id > ?)) \
                         ORDER BY p.created_at, p.projection_id LIMIT 1",
                            params![
                                LEGACY_CANONICAL_MIGRATION_KIND,
                                NORMALIZED_CANONICAL_PROJECTION_SCHEMA_VERSION,
                                scan_after_created_at,
                                scan_after_created_at,
                                scan_after_projection_id.as_str(),
                            ],
                            |row| {
                                Ok((
                                    row.get::<_, String>(0)?,
                                    row.get::<_, String>(1)?,
                                    row.get::<_, String>(2)?,
                                    row.get::<_, String>(3)?,
                                    row.get::<_, String>(4)?,
                                    row.get::<_, i64>(5)?,
                                    row.get::<_, String>(6)?,
                                    row.get::<_, Option<String>>(7)?,
                                ))
                            },
                        )
                        .optional()?
                    };
                    let Some(candidate) = candidate else {
                        break None;
                    };
                    scan_after_created_at = candidate.5;
                    scan_after_projection_id.clone_from(&candidate.0);
                    if candidate.7.as_deref().is_some_and(|fingerprint| {
                        legacy_migration_payload_fingerprint(&candidate.6) == fingerprint
                    }) {
                        continue;
                    }
                    break Some((
                        candidate.0,
                        candidate.1,
                        candidate.2,
                        candidate.3,
                        candidate.4,
                        candidate.5,
                        candidate.6,
                    ));
                };
                let Some((
                    projection_id,
                    principal,
                    workspace,
                    universe_digest,
                    policy_identity,
                    created_at,
                    projection_json,
                )) = legacy
                else {
                    break;
                };
                let prepared = match prepare_normalized_canonical_projection(&projection_json) {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        let error_code = legacy_migration_error_code(&error);
                        let source_fingerprint =
                            legacy_migration_payload_fingerprint(&projection_json);
                        let mut conn = store
                            .conn
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
                        let unchanged: i64 = tx.query_row(
                            "SELECT COUNT(*) FROM attention_canonical_projections \
                             WHERE projection_id = ? AND principal = ? AND workspace = ? \
                               AND universe_digest = ? AND policy_identity = ? AND created_at = ? \
                               AND schema_version < ? AND projection_json = ?",
                            params![
                                projection_id,
                                principal,
                                workspace,
                                universe_digest,
                                policy_identity,
                                created_at,
                                NORMALIZED_CANONICAL_PROJECTION_SCHEMA_VERSION,
                                projection_json,
                            ],
                            |row| row.get(0),
                        )?;
                        let quarantined = unchanged == 1;
                        if quarantined {
                            quarantine_legacy_migration_payload(
                                &tx,
                                LEGACY_CANONICAL_MIGRATION_KIND,
                                &projection_id,
                                &principal,
                                &workspace,
                                &source_fingerprint,
                                error_code,
                                projection_json.len(),
                            )?;
                        }
                        tx.commit()?;
                        processed = processed.saturating_add(1);
                        if quarantined {
                            tracing::warn!(
                                migration_kind = LEGACY_CANONICAL_MIGRATION_KIND,
                                source_fingerprint,
                                error_code,
                                "quarantined unreadable legacy attention row"
                            );
                        }
                        continue;
                    },
                };
                let mut conn = store
                    .conn
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let still_legacy: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM attention_canonical_projections \
                     WHERE projection_id = ? AND principal = ? AND workspace = ? \
                       AND universe_digest = ? AND policy_identity = ? AND created_at = ? \
                       AND schema_version < ? AND projection_json = ?",
                    params![
                        projection_id,
                        principal,
                        workspace,
                        universe_digest,
                        policy_identity,
                        created_at,
                        NORMALIZED_CANONICAL_PROJECTION_SCHEMA_VERSION,
                        projection_json,
                    ],
                    |row| row.get(0),
                )?;
                if still_legacy == 1 {
                    let _insert_stats = insert_normalized_canonical_projection(
                        &tx,
                        &projection_id,
                        created_at,
                        &prepared,
                    )?;
                    let updated = tx.execute(
                        "UPDATE attention_canonical_projections \
                         SET schema_version = ?, projection_json = '', updated_at = ? \
                         WHERE projection_id = ? AND principal = ? AND workspace = ? \
                           AND universe_digest = ? AND policy_identity = ? AND created_at = ? \
                           AND schema_version < ? AND projection_json = ?",
                        params![
                            CURRENT_CANONICAL_PROJECTION_SCHEMA_VERSION,
                            chrono::Utc::now().timestamp_millis(),
                            projection_id,
                            principal,
                            workspace,
                            universe_digest,
                            policy_identity,
                            created_at,
                            NORMALIZED_CANONICAL_PROJECTION_SCHEMA_VERSION,
                            projection_json,
                        ],
                    )?;
                    anyhow::ensure!(
                        updated == 1,
                        "legacy canonical projection source changed during migration"
                    );
                }
                tx.commit()?;
                processed = processed.saturating_add(1);
            }
            Ok(processed)
        })
        .await
        .context("attention legacy canonical projection migration task panicked")?
    }

    /// Read one normalized lane page without materializing the rest of the
    /// projection. V1 projections deliberately return a typed compatibility
    /// error until the bounded repair path has migrated that row.
    pub async fn canonical_projection_lane_page(
        &self,
        projection_id: &str,
        lane: &str,
        offset: usize,
        limit: usize,
    ) -> Result<CanonicalProjectionLanePage> {
        anyhow::ensure!(
            matches!(lane, "follow_up" | "worth_a_look" | "non_surfaced"),
            "canonical projection lane is invalid"
        );
        anyhow::ensure!(
            (1..=200).contains(&limit),
            "canonical projection page limit is invalid"
        );
        let store = self.clone();
        let projection_id = projection_id.to_string();
        let lane = lane.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("canonical_projection_lane_page")?;
            let snapshot = conn
                .unchecked_transaction()
                .context("opening canonical projection page snapshot")?;
            let total = canonical_projection_lane_size_on(&snapshot, &projection_id, &lane)?;
            anyhow::ensure!(
                total.is_some(),
                "canonical projection requires bounded legacy migration before paging"
            );
            let mut statement = snapshot.prepare(
                "SELECT i.item_json, m.item_binding_json \
                 FROM attention_canonical_projection_members m \
                 JOIN attention_canonical_item_revisions i ON i.item_digest = m.item_digest \
                 WHERE m.projection_id = ? AND m.lane = ? \
                 ORDER BY m.position LIMIT ? OFFSET ?",
            )?;
            let items_json = statement
                .query_map(params![projection_id, lane, limit, offset], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
                .into_iter()
                .map(|(body, binding)| -> Result<String> {
                    Ok(serde_json::to_string(&materialize_canonical_item_value(
                        &body,
                        binding.as_deref(),
                    )?)?)
                })
                .collect::<Result<Vec<_>>>()?;
            drop(statement);
            snapshot
                .commit()
                .context("closing canonical projection page snapshot")?;
            Ok(CanonicalProjectionLanePage {
                projection_id,
                lane,
                offset,
                total: total.unwrap_or_default(),
                items_json,
            })
        })
        .await
        .context("attention canonical projection page read task panicked")?
    }

    /// Returns `None` for a still-legacy projection and `Some(total)` for a
    /// normalized lane. Unlike reading a one-item page, this compatibility
    /// probe never parses or materializes a canonical item body.
    pub async fn canonical_projection_lane_size(
        &self,
        projection_id: &str,
        lane: &str,
    ) -> Result<Option<usize>> {
        anyhow::ensure!(
            matches!(lane, "follow_up" | "worth_a_look" | "non_surfaced"),
            "canonical projection lane is invalid"
        );
        let store = self.clone();
        let projection_id = projection_id.to_string();
        let lane = lane.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("canonical_projection_lane_size")?;
            canonical_projection_lane_size_on(&conn, &projection_id, &lane)
        })
        .await
        .context("attention canonical projection lane-size task panicked")?
    }

    /// Atomically persist one complete root order and every deterministic page
    /// boundary before returning page zero. No later read invokes the policy.
    pub async fn create_attention_delivery(
        &self,
        principal: &str,
        workspace: &str,
        request: &CreateAttentionDelivery,
    ) -> Result<FrozenAttentionDelivery> {
        anyhow::ensure!(
            !principal.trim().is_empty(),
            "attention delivery principal is empty"
        );
        anyhow::ensure!(
            !workspace.trim().is_empty(),
            "attention delivery workspace is empty"
        );
        anyhow::ensure!(
            (1..=200).contains(&request.page_size),
            "attention delivery page size must be within 1..=200"
        );
        anyhow::ensure!(
            request.ordered_items.len() <= MAX_NORMALIZED_PROJECTION_ITEMS,
            "attention delivery exceeds the canonical item limit"
        );
        anyhow::ensure!(
            request.root_decision.universe_size == request.ordered_items.len(),
            "attention delivery root universe did not reconcile"
        );
        anyhow::ensure!(
            request
                .root_decision
                .source_generation_token
                .as_deref()
                .is_none_or(|token| !token.trim().is_empty()),
            "attention delivery source-generation token is empty"
        );
        let unique_ids: HashSet<&str> = request
            .ordered_items
            .iter()
            .map(|item| item.candidate_id.as_str())
            .collect();
        anyhow::ensure!(
            unique_ids.len() == request.ordered_items.len()
                && request.ordered_items.iter().all(|item| {
                    !item.candidate_id.trim().is_empty()
                        && item.root_policy_propensity.is_finite()
                        && item.root_policy_propensity > 0.0
                        && item.root_policy_propensity <= 1.0
                }),
            "attention delivery root items are invalid"
        );
        let root_json = serde_json::to_string(&request.root_decision)?;
        let health_json = serde_json::to_string(&request.health)?;
        let context_json = serde_json::to_string(&request.context)?;
        let exposure_tokens = request
            .ordered_items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let position = index + 1;
                blake3::hash(
                    format!(
                        "attention-exposure-v1\0{}\0{}\0{position}",
                        request.root_decision.decision_id, item.candidate_id
                    )
                    .as_bytes(),
                )
                .to_hex()
                .to_string()
            })
            .collect::<Vec<_>>();
        let page_count = request
            .ordered_items
            .len()
            .max(1)
            .div_ceil(request.page_size);
        let cursors = (0..page_count)
            .map(|page_index| {
                (page_index > 0).then(|| {
                    blake3::hash(
                        format!(
                            "attention-cursor-v1\0{}\0{page_index}",
                            request.root_decision.decision_id
                        )
                        .as_bytes(),
                    )
                    .to_hex()
                    .to_string()
                })
            })
            .collect::<Vec<_>>();
        let prepared_pages = (0..page_count)
            .map(|page_index| -> Result<_> {
                let page_start = page_index.saturating_mul(request.page_size);
                let page_end = page_start
                    .saturating_add(request.page_size)
                    .min(request.ordered_items.len());
                let page = AttentionDeliveryPage {
                    delivery_id: blake3::hash(
                        format!(
                            "attention-delivery-page-v1\0{}\0{page_index}",
                            request.root_decision.decision_id
                        )
                        .as_bytes(),
                    )
                    .to_hex()
                    .to_string(),
                    page_index,
                    page_start,
                    page_size: request.page_size,
                    cursor: cursors[page_index].clone(),
                    next_cursor: cursors.get(page_index + 1).cloned().flatten(),
                    has_more: page_index + 1 < page_count,
                    expires_at: request.root_decision.expires_at,
                };
                let page_json = serde_json::to_string(&page)?;
                Ok((page, page_json, page_end))
            })
            .collect::<Result<Vec<_>>>()?;
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let request = request.clone();
        tokio::task::spawn_blocking(move || {
            // Serving-path write: bounded wait, see `record_decision`.
            let mut conn = store.conn.lock_within(REQUEST_PATH_WRITER_WAIT)?;
            let transaction_started = Instant::now();
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .context("opening attention delivery transaction")?;
            let normalized_projection: bool = tx
                .query_row(
                    "SELECT schema_version BETWEEN ? AND ? FROM attention_canonical_projections \
                     WHERE principal = ? AND workspace = ? AND projection_id = ?",
                    params![
                        NORMALIZED_CANONICAL_PROJECTION_SCHEMA_VERSION,
                        CURRENT_CANONICAL_PROJECTION_SCHEMA_VERSION,
                        principal,
                        workspace,
                        request.root_decision.projection_id,
                    ],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or(false);
            tx.execute(
                "INSERT INTO attention_delivery_decisions ( \
                    decision_id, schema_version, principal, workspace, lane, projection_id, \
                    universe_digest, source_generation_token, policy_snapshot_id, policy_model_version, policy_snapshot_json, \
                    posterior_version, seed_identity, universe_size, page_size, status, fallback_reason, \
                    health_json, min_visible_ms, visibility_rule_version, context_json, \
                    decision_json, created_at, expires_at \
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    request.root_decision.decision_id,
                    i64::from(super::delivery::ATTENTION_DELIVERY_SCHEMA_VERSION),
                    principal,
                    workspace,
                    request.root_decision.lane.as_str(),
                    request.root_decision.projection_id,
                    request.root_decision.universe_digest,
                    request.root_decision.source_generation_token,
                    request.root_decision.policy_snapshot_id,
                    request.root_decision.policy_model_version,
                    request.policy_snapshot_json,
                    u64_to_i64(request.root_decision.posterior_version),
                    request.root_decision.seed_identity,
                    usize_to_i64(request.root_decision.universe_size),
                    usize_to_i64(request.page_size),
                    match request.status {
                        AttentionDeliveryStatus::Succeeded => "succeeded",
                        AttentionDeliveryStatus::BaselineFallback => "baseline_fallback",
                    },
                    request.fallback_reason,
                    health_json,
                    u64_to_i64(request.min_visible_ms),
                    request.visibility_rule_version,
                    context_json,
                    root_json,
                    request.root_decision.created_at,
                    request.root_decision.expires_at,
                ],
            )
            .context("inserting attention delivery root")?;
            for (index, item) in request.ordered_items.iter().enumerate() {
                let position = index + 1;
                tx.execute(
                    "INSERT INTO attention_delivery_decision_items ( \
                        decision_id, position, candidate_id, source_revision, \
                        root_policy_propensity, exposure_token, item_json, attribution_item_json \
                     ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                    params![
                        request.root_decision.decision_id,
                        usize_to_i64(position),
                        item.candidate_id,
                        item.source_revision,
                        item.root_policy_propensity,
                        exposure_tokens[index],
                        if normalized_projection {
                            ""
                        } else {
                            item.item_json.as_str()
                        },
                        item.attribution_item_json,
                    ],
                )?;
            }
            if normalized_projection {
                let (member_count, delivery_count, mismatches): (i64, i64, i64) = tx.query_row(
                    "SELECT \
                       (SELECT COUNT(*) FROM attention_canonical_projection_members \
                          WHERE projection_id = ?1 AND lane = ?2), \
                       (SELECT COUNT(*) FROM attention_delivery_decision_items \
                          WHERE decision_id = ?3), \
                       (SELECT COUNT(*) FROM attention_delivery_decision_items delivery \
                          LEFT JOIN attention_canonical_projection_members member \
                            ON member.projection_id = ?1 AND member.lane = ?2 \
                           AND member.canonical_id = delivery.candidate_id \
                          LEFT JOIN attention_canonical_item_revisions body \
                            ON body.item_digest = member.item_digest \
                          WHERE delivery.decision_id = ?3 \
                            AND (member.canonical_id IS NULL \
                                 OR NOT (body.source_revision IS delivery.source_revision)))",
                    params![
                        request.root_decision.projection_id,
                        request.root_decision.lane.as_str(),
                        request.root_decision.decision_id,
                    ],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?;
                anyhow::ensure!(
                    member_count == delivery_count
                        && delivery_count == usize_to_i64(request.ordered_items.len())
                        && mismatches == 0,
                    "attention delivery items do not match normalized projection revisions"
                );
            }
            let mut first_page = None;
            for (page, page_json, page_end) in prepared_pages {
                let page_index = page.page_index;
                let page_start = page.page_start;
                let has_more = page.has_more;
                let delivery_id = page.delivery_id.clone();
                tx.execute(
                    "INSERT INTO attention_delivery_pages ( \
                        delivery_id, decision_id, page_index, page_start, page_size, cursor, \
                        next_cursor, has_more, expires_at, page_json \
                     ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    params![
                        delivery_id,
                        request.root_decision.decision_id,
                        usize_to_i64(page_index),
                        usize_to_i64(page_start),
                        usize_to_i64(request.page_size),
                        page.cursor,
                        page.next_cursor,
                        bool_to_i64(has_more),
                        request.root_decision.expires_at,
                        page_json,
                    ],
                )?;
                for position in (page_start + 1)..=page_end {
                    tx.execute(
                        "INSERT INTO attention_delivery_page_items (delivery_id, decision_id, position) \
                         VALUES (?, ?, ?)",
                        params![delivery_id, request.root_decision.decision_id, usize_to_i64(position)],
                    )?;
                }
                if let Some(cursor) = &page.cursor {
                    tx.execute(
                        "INSERT INTO attention_delivery_cursors ( \
                            cursor, principal, workspace, lane, decision_id, delivery_id, projection_id, \
                            universe_digest, policy_snapshot_id, policy_model_version, posterior_version, \
                            page_start, page_size, expires_at \
                         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                        params![
                            cursor,
                            principal,
                            workspace,
                            request.root_decision.lane.as_str(),
                            request.root_decision.decision_id,
                            delivery_id,
                            request.root_decision.projection_id,
                            request.root_decision.universe_digest,
                            request.root_decision.policy_snapshot_id,
                            request.root_decision.policy_model_version,
                            u64_to_i64(request.root_decision.posterior_version),
                            usize_to_i64(page_start),
                            usize_to_i64(request.page_size),
                            request.root_decision.expires_at,
                        ],
                    )?;
                }
                if page_index == 0 {
                    first_page = Some((page, page_start, page_end));
                }
            }
            tx.commit().context("committing attention delivery root/pages")?;
            tracing::info!(
                decision_id = %request.root_decision.decision_id,
                normalized_projection,
                item_count = request.ordered_items.len(),
                page_count,
                transaction_ms = transaction_started.elapsed().as_millis(),
                "committed attention delivery pages"
            );
            let (page, _page_start, page_end) =
                first_page.context("attention delivery page zero missing")?;
            drop(conn);
            let read = store.reads.acquire("create_attention_delivery_page_zero")?;
            let page_items = read_frozen_delivery_page_items(
                &read,
                &page.delivery_id,
                &request.root_decision.projection_id,
            )?;
            let mut health = request.health.clone();
            health.delivered_count = page_end;
            health.remaining_count = request.ordered_items.len().saturating_sub(page_end);
            health.exact_revision_match = true;
            health.replay = false;
            Ok(FrozenAttentionDelivery {
                status: request.status,
                fallback_reason: request.fallback_reason,
                root_decision: request.root_decision,
                page,
                items: page_items,
                health,
                min_visible_ms: request.min_visible_ms,
                visibility_rule_version: request.visibility_rule_version,
            })
        })
        .await
        .context("attention delivery create task panicked")?
    }

    /// Read the compact source-generation binding for an opaque cursor after
    /// validating scope, lane, and expiry. `None` identifies a legacy cursor
    /// whose caller must reconstruct the canonical projection for the old
    /// projection/digest drift check.
    pub async fn attention_delivery_cursor_source_generation_token(
        &self,
        principal: &str,
        workspace: &str,
        lane: AttentionSurface,
        cursor: &str,
        now: i64,
    ) -> std::result::Result<Option<String>, AttentionDeliveryReadError> {
        if cursor.trim().is_empty() {
            return Err(AttentionDeliveryReadError::RefreshRequired(
                AttentionDeliveryRefreshReason::CursorNotFound,
            ));
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let cursor = cursor.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store
                .reads
                .acquire("attention_delivery_cursor_source_generation_token")
                .map_err(AttentionDeliveryReadError::Storage)?;
            let binding = conn.query_row(
                "SELECT c.principal, c.workspace, c.lane, c.expires_at, \
                        d.source_generation_token \
                 FROM attention_delivery_cursors AS c \
                 JOIN attention_delivery_decisions AS d \
                   ON d.decision_id = c.decision_id \
                  AND d.principal = c.principal AND d.workspace = c.workspace \
                  AND d.lane = c.lane AND d.projection_id = c.projection_id \
                  AND d.universe_digest = c.universe_digest \
                  AND d.expires_at = c.expires_at \
                 WHERE c.cursor = ?",
                params![cursor],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            );
            let (bound_principal, bound_workspace, bound_lane, expires_at, token) = match binding {
                Ok(value) => value,
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    return Err(AttentionDeliveryReadError::RefreshRequired(
                        AttentionDeliveryRefreshReason::CursorNotFound,
                    ));
                },
                Err(error) => return Err(AttentionDeliveryReadError::Storage(error.into())),
            };
            if bound_principal != principal || bound_workspace != workspace {
                return Err(AttentionDeliveryReadError::RefreshRequired(
                    AttentionDeliveryRefreshReason::ScopeMismatch,
                ));
            }
            if bound_lane != lane.as_str() {
                return Err(AttentionDeliveryReadError::RefreshRequired(
                    AttentionDeliveryRefreshReason::BindingMismatch,
                ));
            }
            if expires_at <= now {
                return Err(AttentionDeliveryReadError::RefreshRequired(
                    AttentionDeliveryRefreshReason::Expired,
                ));
            }
            if token
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
            {
                return Err(AttentionDeliveryReadError::RefreshRequired(
                    AttentionDeliveryRefreshReason::BindingMismatch,
                ));
            }
            Ok(token)
        })
        .await
        .map_err(|error| {
            AttentionDeliveryReadError::Storage(anyhow::anyhow!(
                "attention delivery cursor source-generation read task panicked: {error}"
            ))
        })?
    }

    /// Read one opaque cursor after validating its complete frozen binding and
    /// the current canonical source generation. Legacy rows without a compact
    /// generation token retain the exact projection/digest comparison.
    #[allow(clippy::too_many_arguments)]
    pub async fn read_attention_delivery_page(
        &self,
        principal: &str,
        workspace: &str,
        lane: AttentionSurface,
        cursor: &str,
        current_source_generation_token: Option<&str>,
        legacy_current_projection_id: Option<&str>,
        legacy_current_universe_digest: Option<&str>,
        now: i64,
    ) -> std::result::Result<FrozenAttentionDelivery, AttentionDeliveryReadError> {
        if cursor.trim().is_empty() {
            return Err(AttentionDeliveryReadError::RefreshRequired(
                AttentionDeliveryRefreshReason::CursorNotFound,
            ));
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let cursor = cursor.to_string();
        let current_source_generation_token = current_source_generation_token.map(str::to_string);
        let legacy_current_projection_id = legacy_current_projection_id.map(str::to_string);
        let legacy_current_universe_digest = legacy_current_universe_digest.map(str::to_string);
        tokio::task::spawn_blocking(move || {
            let read = store
                .reads
                .acquire("read_attention_delivery_page")
                .map_err(AttentionDeliveryReadError::Storage)?;
            let conn = read
                .unchecked_transaction()
                .map_err(|error| AttentionDeliveryReadError::Storage(error.into()))?;
            let binding = conn.query_row(
                "SELECT principal, workspace, lane, decision_id, delivery_id, projection_id, \
                        universe_digest, policy_snapshot_id, policy_model_version, posterior_version, \
                        page_start, page_size, expires_at \
                 FROM attention_delivery_cursors WHERE cursor = ?",
                params![cursor],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?, row.get::<_, Option<String>>(7)?,
                        row.get::<_, Option<String>>(8)?, row.get::<_, i64>(9)?,
                        row.get::<_, i64>(10)?, row.get::<_, i64>(11)?, row.get::<_, i64>(12)?,
                    ))
                },
            );
            let (bound_principal, bound_workspace, bound_lane, decision_id, delivery_id,
                projection_id, universe_digest, policy_snapshot_id, policy_model_version,
                posterior_version, page_start, page_size, expires_at) = match binding {
                Ok(value) => value,
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    return Err(AttentionDeliveryReadError::RefreshRequired(
                        AttentionDeliveryRefreshReason::CursorNotFound,
                    ))
                },
                Err(error) => return Err(AttentionDeliveryReadError::Storage(error.into())),
            };
            if bound_principal != principal || bound_workspace != workspace {
                return Err(AttentionDeliveryReadError::RefreshRequired(
                    AttentionDeliveryRefreshReason::ScopeMismatch,
                ));
            }
            if bound_lane != lane.as_str() {
                return Err(AttentionDeliveryReadError::RefreshRequired(
                    AttentionDeliveryRefreshReason::BindingMismatch,
                ));
            }
            if expires_at <= now {
                return Err(AttentionDeliveryReadError::RefreshRequired(
                    AttentionDeliveryRefreshReason::Expired,
                ));
            }
            let stored_root = conn.query_row(
                "SELECT principal, workspace, lane, projection_id, universe_digest, expires_at, \
                        policy_snapshot_id, policy_model_version, posterior_version, status, \
                        fallback_reason, health_json, min_visible_ms, visibility_rule_version, \
                        decision_json, source_generation_token \
                 FROM attention_delivery_decisions WHERE decision_id = ?",
                params![decision_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?, row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?, row.get::<_, i64>(5)?,
                        row.get::<_, Option<String>>(6)?, row.get::<_, Option<String>>(7)?,
                        row.get::<_, i64>(8)?, row.get::<_, String>(9)?,
                        row.get::<_, Option<String>>(10)?, row.get::<_, String>(11)?,
                        row.get::<_, i64>(12)?, row.get::<_, String>(13)?,
                        row.get::<_, String>(14)?, row.get::<_, Option<String>>(15)?,
                    ))
                },
            );
            let (root_principal, root_workspace, root_lane, root_projection_id,
                root_universe_digest, root_expires_at, root_policy, root_model,
                root_posterior, status, fallback_reason, health_json, min_visible_ms,
                visibility_rule_version, root_json, stored_source_generation_token) =
                match stored_root {
                    Ok(value) => value,
                    Err(rusqlite::Error::QueryReturnedNoRows) => {
                        return Err(AttentionDeliveryReadError::RefreshRequired(
                            AttentionDeliveryRefreshReason::CursorNotFound,
                        ))
                    },
                    Err(error) => return Err(AttentionDeliveryReadError::Storage(error.into())),
                };
            if root_principal != bound_principal
                || root_workspace != bound_workspace
                || root_lane != bound_lane
                || root_projection_id != projection_id
                || root_universe_digest != universe_digest
                || root_expires_at != expires_at
                || root_policy != policy_snapshot_id
                || root_model != policy_model_version
                || root_posterior != posterior_version
                || !matches!(status.as_str(), "succeeded" | "baseline_fallback")
            {
                return Err(AttentionDeliveryReadError::RefreshRequired(
                    AttentionDeliveryRefreshReason::BindingMismatch,
                ));
            }
            if let Some(stored_token) = stored_source_generation_token.as_deref() {
                if stored_token.trim().is_empty()
                    || current_source_generation_token.as_deref() != Some(stored_token)
                {
                    return Err(AttentionDeliveryReadError::RefreshRequired(
                        AttentionDeliveryRefreshReason::ProjectionDrift,
                    ));
                }
            } else if legacy_current_projection_id.as_deref() != Some(projection_id.as_str())
                || legacy_current_universe_digest.as_deref() != Some(universe_digest.as_str())
            {
                return Err(AttentionDeliveryReadError::RefreshRequired(
                    AttentionDeliveryRefreshReason::ProjectionDrift,
                ));
            }
            let (projection_schema, legacy_projection_json): (i64, String) = conn
                .query_row(
                    "SELECT schema_version, projection_json \
                     FROM attention_canonical_projections WHERE projection_id = ?",
                    params![projection_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|error| match error {
                    rusqlite::Error::QueryReturnedNoRows => {
                        AttentionDeliveryReadError::RefreshRequired(
                            AttentionDeliveryRefreshReason::ProjectionDrift,
                        )
                    },
                    other => AttentionDeliveryReadError::Storage(other.into()),
                })?;
            let stored_revision_count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM attention_delivery_decision_items WHERE decision_id = ?",
                params![decision_id],
                |row| row.get(0),
            )?;
            if projection_schema > CURRENT_CANONICAL_PROJECTION_SCHEMA_VERSION {
                return Err(AttentionDeliveryReadError::Storage(anyhow::anyhow!(
                    "canonical projection schema is newer than this runtime"
                )));
            }
            if projection_schema >= NORMALIZED_CANONICAL_PROJECTION_SCHEMA_VERSION {
                let current_revision_count: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM attention_canonical_projection_members \
                     WHERE projection_id = ? AND lane = ?",
                    params![projection_id, lane.as_str()],
                    |row| row.get(0),
                )?;
                let revision_mismatches: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM attention_delivery_decision_items delivery \
                     LEFT JOIN attention_canonical_projection_members member \
                       ON member.projection_id = ? AND member.lane = ? \
                      AND member.canonical_id = delivery.candidate_id \
                     LEFT JOIN attention_canonical_item_revisions body \
                       ON body.item_digest = member.item_digest \
                     WHERE delivery.decision_id = ? \
                       AND (member.canonical_id IS NULL \
                            OR NOT (body.source_revision IS delivery.source_revision))",
                    params![projection_id, lane.as_str(), decision_id],
                    |row| row.get(0),
                )?;
                if current_revision_count != stored_revision_count || revision_mismatches != 0 {
                    return Err(AttentionDeliveryReadError::RefreshRequired(
                        if current_revision_count == stored_revision_count {
                            AttentionDeliveryRefreshReason::RevisionDrift
                        } else {
                            AttentionDeliveryRefreshReason::ProjectionDrift
                        },
                    ));
                }
            } else {
                // Compatibility only: old frozen deliveries still work, but
                // only legacy rows pay the full-body materialization cost.
                let projection = parse_bounded_json_value(
                    &legacy_projection_json,
                    "legacy canonical delivery projection",
                )
                .map_err(AttentionDeliveryReadError::Storage)?;
                let lane_items = projection
                    .get("lanes")
                    .and_then(|lanes| lanes.get(lane.as_str()))
                    .and_then(serde_json::Value::as_array)
                    .ok_or_else(|| {
                        AttentionDeliveryReadError::RefreshRequired(
                            AttentionDeliveryRefreshReason::BindingMismatch,
                        )
                    })?;
                let current_revisions = lane_items
                    .iter()
                    .filter_map(|item| {
                        Some((
                            item.get("canonical_id")?.as_str()?.to_string(),
                            item.get("source_revision")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_string),
                        ))
                    })
                    .collect::<HashMap<_, _>>();
                let mut statement = conn.prepare(
                    "SELECT candidate_id, source_revision \
                     FROM attention_delivery_decision_items WHERE decision_id = ?",
                )?;
                let stored_revisions = statement
                    .query_map(params![decision_id], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
                    })?
                    .collect::<std::result::Result<HashMap<_, _>, _>>()?;
                if stored_revisions != current_revisions {
                    return Err(AttentionDeliveryReadError::RefreshRequired(
                        if stored_revisions.len() == current_revisions.len() {
                            AttentionDeliveryRefreshReason::RevisionDrift
                        } else {
                            AttentionDeliveryRefreshReason::ProjectionDrift
                        },
                    ));
                }
            }
            let root_decision: super::delivery::AttentionDeliveryRootDecision =
                parse_bounded_stored_json(
                    &root_json,
                    "frozen attention delivery root",
                    MAX_ATTENTION_STORED_JSON_BYTES,
                )
                .map_err(|_| {
                    AttentionDeliveryReadError::RefreshRequired(
                        AttentionDeliveryRefreshReason::BindingMismatch,
                    )
                })?;
            if root_decision.decision_id != decision_id
                || root_decision.lane != lane
                || root_decision.projection_id != projection_id
                || root_decision.universe_digest != universe_digest
                || root_decision.source_generation_token != stored_source_generation_token
                || root_decision.policy_snapshot_id != policy_snapshot_id
                || root_decision.policy_model_version != policy_model_version
                || root_decision.posterior_version != posterior_version.max(0) as u64
                || root_decision.universe_size != stored_revision_count.max(0) as usize
                || root_decision.created_at <= 0
                || root_decision.created_at > root_decision.expires_at
                || root_decision.expires_at != expires_at
                || root_decision.seed_identity.trim().is_empty()
            {
                return Err(AttentionDeliveryReadError::RefreshRequired(
                    AttentionDeliveryRefreshReason::BindingMismatch,
                ));
            }
            let page_json = conn.query_row(
                "SELECT page_json FROM attention_delivery_pages WHERE delivery_id = ? AND decision_id = ?",
                params![delivery_id, decision_id],
                |row| row.get(0),
            );
            let page_json: String = match page_json {
                Ok(value) => value,
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    return Err(AttentionDeliveryReadError::RefreshRequired(
                        AttentionDeliveryRefreshReason::CursorNotFound,
                    ))
                },
                Err(error) => return Err(AttentionDeliveryReadError::Storage(error.into())),
            };
            let page: AttentionDeliveryPage = parse_bounded_stored_json(
                &page_json,
                "frozen attention delivery page",
                MAX_ATTENTION_STORED_JSON_BYTES,
            )
            .map_err(|_| {
                    AttentionDeliveryReadError::RefreshRequired(
                        AttentionDeliveryRefreshReason::BindingMismatch,
                    )
                })?;
            if page.page_start != page_start.max(0) as usize
                || page.page_size != page_size.max(0) as usize
                || page.cursor.as_deref() != Some(cursor.as_str())
                || page.delivery_id != delivery_id
                || page.page_start != page.page_index.saturating_mul(page.page_size)
                || page.expires_at != root_decision.expires_at
            {
                return Err(AttentionDeliveryReadError::RefreshRequired(
                    AttentionDeliveryRefreshReason::BindingMismatch,
                ));
            }
            let items = read_frozen_delivery_page_items(&conn, &delivery_id, &projection_id)
                .map_err(AttentionDeliveryReadError::Storage)?;
            let expected_end = page
                .page_start
                .saturating_add(page.page_size)
                .min(root_decision.universe_size);
            let expected_count = expected_end.saturating_sub(page.page_start);
            let unique_candidates = items
                .iter()
                .map(|item| item.candidate_id.as_str())
                .collect::<HashSet<_>>();
            let contiguous = items.iter().enumerate().all(|(index, item)| {
                item.position == page.page_start + index + 1
                    && item.root_policy_propensity.is_finite()
                    && item.root_policy_propensity > 0.0
                    && item.root_policy_propensity <= 1.0
                    && item.conditional_delivery_propensity == 1.0
            });
            let expected_has_more = expected_end < root_decision.universe_size;
            if items.len() != expected_count
                || unique_candidates.len() != items.len()
                || !contiguous
                || page.has_more != expected_has_more
                || page.next_cursor.is_some() != expected_has_more
            {
                return Err(AttentionDeliveryReadError::RefreshRequired(
                    AttentionDeliveryRefreshReason::BindingMismatch,
                ));
            }
            let mut health: super::delivery::AttentionDeliveryHealth =
                parse_bounded_stored_json(
                    &health_json,
                    "frozen attention delivery health",
                    MAX_ATTENTION_STORED_JSON_BYTES,
                )
                .map_err(|_| {
                    AttentionDeliveryReadError::RefreshRequired(
                        AttentionDeliveryRefreshReason::BindingMismatch,
                    )
                })?;
            health.delivered_count = page.page_start.saturating_add(items.len());
            health.remaining_count = root_decision
                .universe_size
                .saturating_sub(health.delivered_count);
            health.exact_revision_match = true;
            health.replay = true;
            let delivery = FrozenAttentionDelivery {
                status: if status == "succeeded" {
                    AttentionDeliveryStatus::Succeeded
                } else {
                    AttentionDeliveryStatus::BaselineFallback
                },
                fallback_reason,
                root_decision,
                page,
                items,
                health,
                min_visible_ms: min_visible_ms.max(0) as u64,
                visibility_rule_version,
            };
            conn.commit()
                .map_err(|error| AttentionDeliveryReadError::Storage(error.into()))?;
            Ok(delivery)
        })
        .await
        .map_err(|error| AttentionDeliveryReadError::Storage(anyhow::anyhow!(
            "attention delivery read task panicked: {error}"
        )))?
    }

    pub async fn get_decision(
        &self,
        principal: &str,
        workspace: &str,
        decision_id: &str,
    ) -> Result<Option<AttentionDecisionDetail>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let decision_id = decision_id.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("get_decision")?;
            let decision_json = conn.query_row(
                "SELECT decision_json FROM attention_decisions \
                 WHERE principal = ? AND workspace = ? AND decision_id = ?",
                params![principal, workspace, decision_id],
                |row| row.get::<_, String>(0),
            );
            let decision_json = match decision_json {
                Ok(value) => value,
                Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            let decision: AttentionDecision = parse_bounded_stored_json(
                &decision_json,
                "stored attention decision",
                MAX_ATTENTION_STORED_JSON_BYTES,
            )?;
            let mut statement = conn.prepare(
                "SELECT item_json FROM attention_decision_items \
                 WHERE decision_id = ? \
                 ORDER BY baseline_rank ASC, candidate_id ASC",
            )?;
            // The parent decision was scope-authorized immediately above and
            // decision_id is globally unique. Repeating scope predicates here
            // made SQLite prefer a scope-history index and scan millions of
            // unrelated items before hydrating this one immutable decision.
            let rows = statement.query_map(params![decision_id], |row| row.get::<_, String>(0))?;
            let mut items = Vec::new();
            for row in rows {
                let item_json = row?;
                items.push(parse_bounded_stored_json::<AttentionDecisionItem>(
                    &item_json,
                    "stored attention decision item",
                    MAX_ATTENTION_STORED_JSON_BYTES,
                )?);
            }
            anyhow::ensure!(
                items.len() == decision.eligible_item_count,
                "stored attention decision item count does not reconcile"
            );
            Ok(Some(AttentionDecisionDetail { decision, items }))
        })
        .await
        .context("attention learning get_decision task panicked")?
    }

    /// Record one visibility event after verifying its exact selected decision
    /// item. Replays use a monotonic cumulative dwell value, so retrying a
    /// request cannot double-count visibility.
    pub async fn record_impression(
        &self,
        principal: &str,
        workspace: &str,
        request: &RecordAttentionImpression,
        min_visible_ms: u64,
        configured_visibility_rule_version: &str,
        recorded_at: i64,
    ) -> std::result::Result<AttentionImpressionReceipt, AttentionImpressionError> {
        if !(1..=ATTENTION_MIN_VISIBLE_MS_MAX).contains(&min_visible_ms) {
            return Err(AttentionImpressionError::InvalidRequest(format!(
                "min_visible_ms must be within 1..={ATTENTION_MIN_VISIBLE_MS_MAX}"
            )));
        }
        validate_impression_request(request, configured_visibility_rule_version)?;
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let request = request.clone();
        let configured_visibility_rule_version = configured_visibility_rule_version.to_string();
        tokio::task::spawn_blocking(move || {
            let mut conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(anyhow::Error::from)?;
            let existing = tx.query_row(
                "SELECT impression_id, decision_id, delivery_id, page_index, position, exposure_token, \
                        candidate_id, source_revision, surface, visibility_rule_version, client_type, \
                        client_version, viewport_class, root_policy_propensity, \
                        conditional_delivery_propensity, accumulated_visible_ms, verified, dedupe_count \
                 FROM attention_impressions \
                 WHERE principal = ? AND workspace = ? AND event_id = ?",
                params![principal, workspace, request.event_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, String>(10)?,
                        row.get::<_, String>(11)?,
                        row.get::<_, String>(12)?,
                        row.get::<_, Option<f64>>(13)?,
                        row.get::<_, Option<f64>>(14)?,
                        row.get::<_, i64>(15)?,
                        row.get::<_, i64>(16)?,
                        row.get::<_, i64>(17)?,
                    ))
                },
            );
            if let Ok((
                _,
                decision_id,
                delivery_id,
                page_index,
                position,
                exposure_token,
                candidate_id,
                existing_revision,
                surface,
                rule,
                client_type,
                client_version,
                viewport_class,
                _,
                _,
                _,
                _,
                _,
            )) = &existing
            {
                if decision_id != &request.decision_id
                    || delivery_id.as_deref() != Some(request.delivery_id.as_str())
                    || *page_index != Some(usize_to_i64(request.page_index))
                    || *position != Some(usize_to_i64(request.position))
                    || exposure_token.as_deref() != Some(request.exposure_token.as_str())
                    || candidate_id != &request.candidate_id
                    || existing_revision != &request.source_revision
                    || surface.as_str() != request.surface.as_str()
                    || rule != &request.visibility_rule_version
                    || client_type != &request.client_type
                    || client_version != &request.client_version
                    || viewport_class != &request.viewport_class
                {
                    return Err(AttentionImpressionError::EventIdentityConflict);
                }
            }
            let delivered_item = tx.query_row(
                "SELECT d.projection_id, i.source_revision, d.lane, r.cluster_id, i.position, \
                        i.root_policy_propensity, p.page_index, p.expires_at \
                 FROM attention_delivery_decisions d \
                 JOIN attention_delivery_decision_items i ON i.decision_id = d.decision_id \
                 JOIN attention_delivery_pages p ON p.decision_id = d.decision_id \
                    AND p.delivery_id = ? \
                 JOIN attention_delivery_page_items pi ON pi.delivery_id = p.delivery_id \
                    AND pi.decision_id = i.decision_id AND pi.position = i.position \
                 JOIN attention_decision_items r ON r.decision_id = d.projection_id \
                    AND r.candidate_id = i.candidate_id \
                 WHERE d.principal = ? AND d.workspace = ? AND d.decision_id = ? \
                   AND i.candidate_id = ? AND i.position = ? AND i.exposure_token = ?",
                params![
                    request.delivery_id,
                    principal,
                    workspace,
                    request.decision_id,
                    request.candidate_id,
                    usize_to_i64(request.position),
                    request.exposure_token,
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, f64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, i64>(7)?,
                    ))
                },
            );
            let (projection_id, source_revision, served_route, cluster_id, rank, root_policy_propensity,
                delivered_page_index, delivery_expires_at) = match delivered_item {
                Ok(value) => value,
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    return Err(AttentionImpressionError::DeliveryItemNotFound)
                },
                Err(error) => {
                    return Err(AttentionImpressionError::Storage(error.into()))
                },
            };
            if source_revision != request.source_revision
                || served_route != request.surface.as_str()
                || rank != usize_to_i64(request.position)
                || delivered_page_index != usize_to_i64(request.page_index)
                || delivery_expires_at <= recorded_at
                || !root_policy_propensity.is_finite()
                || root_policy_propensity <= 0.0
                || root_policy_propensity > 1.0
            {
                return Err(AttentionImpressionError::DeliveryBindingMismatch);
            }
            let visible_ms = u64_to_i64(request.visible_ms);
            let min_visible_i64 = u64_to_i64(min_visible_ms);
            let (impression_id, accumulated_visible_ms, verified, deduplicated) = match existing {
                Ok((
                    impression_id,
                    decision_id,
                    delivery_id,
                    page_index,
                    position,
                    exposure_token,
                    candidate_id,
                    existing_revision,
                    surface,
                    rule,
                    client_type,
                    client_version,
                    viewport_class,
                    existing_root_propensity,
                    existing_delivery_propensity,
                    existing_visible,
                    existing_verified,
                    dedupe_count,
                )) => {
                    if decision_id != request.decision_id
                        || delivery_id.as_deref() != Some(request.delivery_id.as_str())
                        || page_index != Some(usize_to_i64(request.page_index))
                        || position != Some(usize_to_i64(request.position))
                        || exposure_token.as_deref() != Some(request.exposure_token.as_str())
                        || candidate_id != request.candidate_id
                        || existing_revision != request.source_revision
                        || surface != request.surface.as_str()
                        || rule != request.visibility_rule_version
                        || client_type != request.client_type
                        || client_version != request.client_version
                        || viewport_class != request.viewport_class
                        || existing_root_propensity != Some(root_policy_propensity)
                        || existing_delivery_propensity != Some(1.0)
                    {
                        return Err(AttentionImpressionError::EventIdentityConflict);
                    }
                    let accumulated = existing_visible.max(visible_ms);
                    let verified = existing_verified == 1 || accumulated >= min_visible_i64;
                    tx.execute(
                        "UPDATE attention_impressions SET accumulated_visible_ms = ?, \
                            verified = ?, dedupe_count = ?, last_recorded_at = ? \
                         WHERE principal = ? AND workspace = ? AND event_id = ?",
                        params![
                            accumulated,
                            bool_to_i64(verified),
                            dedupe_count.saturating_add(1),
                            recorded_at,
                            principal,
                            workspace,
                            request.event_id,
                        ],
                    )
                    .map_err(anyhow::Error::from)?;
                    (impression_id, accumulated, verified, true)
                },
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    let impression_id = uuid::Uuid::new_v4().to_string();
                    let verified = visible_ms >= min_visible_i64;
                    tx.execute(
                        "INSERT INTO attention_impressions ( \
                            impression_id, schema_version, event_id, principal, workspace, \
                            decision_id, projection_id, delivery_id, page_index, position, exposure_token, \
                            candidate_id, source_revision, cluster_id, surface, rank, \
                            first_visible_at, accumulated_visible_ms, visibility_rule_version, \
                            client_type, client_version, viewport_class, root_policy_propensity, \
                            conditional_delivery_propensity, verified, dedupe_count, last_recorded_at \
                         ) VALUES ( \
                            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, \
                            ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, 0, ?26 \
                         )",
                        params![
                            impression_id,
                            i64::from(ATTENTION_IMPRESSION_SCHEMA_VERSION),
                            request.event_id,
                            principal,
                            workspace,
                            request.decision_id,
                            projection_id,
                            request.delivery_id,
                            usize_to_i64(request.page_index),
                            usize_to_i64(request.position),
                            request.exposure_token,
                            request.candidate_id,
                            request.source_revision,
                            cluster_id,
                            request.surface.as_str(),
                            rank,
                            recorded_at,
                            visible_ms,
                            configured_visibility_rule_version,
                            request.client_type,
                            request.client_version,
                            request.viewport_class,
                            root_policy_propensity,
                            1.0_f64,
                            bool_to_i64(verified),
                            recorded_at,
                        ],
                    )
                    .map_err(anyhow::Error::from)?;
                    (impression_id, visible_ms, verified, false)
                },
                Err(error) => return Err(AttentionImpressionError::Storage(error.into())),
            };
            tx.commit().map_err(anyhow::Error::from)?;
            Ok(AttentionImpressionReceipt {
                impression_id,
                event_id: request.event_id,
                decision_id: request.decision_id,
                delivery_id: request.delivery_id,
                page_index: request.page_index,
                position: request.position,
                exposure_token: request.exposure_token,
                candidate_id: request.candidate_id,
                source_revision: request.source_revision,
                surface: request.surface,
                accumulated_visible_ms: accumulated_visible_ms.max(0) as u64,
                min_visible_ms,
                visibility_rule_version: configured_visibility_rule_version,
                root_policy_propensity,
                conditional_delivery_propensity: 1.0,
                verified,
                deduplicated,
            })
        })
        .await
        .map_err(|error| AttentionImpressionError::Storage(anyhow::anyhow!(
            "attention learning record_impression task panicked: {error}"
        )))?
    }

    pub async fn routing_health(
        &self,
        principal: &str,
        workspace: &str,
        evaluation: &AttentionRoutingEvaluation,
    ) -> Result<AttentionRoutingHealth> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let decision_id = evaluation.decision_id.clone();
        let evaluated_count = evaluation.items.len();
        let learned_route_count = evaluation
            .items
            .iter()
            .filter(|item| item.learned_route != item.baseline_route)
            .count();
        let applied_route_count = evaluation
            .items
            .iter()
            .filter(|item| item.route_applied)
            .count();
        let baseline_retained_count = evaluation
            .items
            .iter()
            .filter(|item| item.served_route == item.baseline_route)
            .count();
        let routing_snapshot_valid = evaluation.degradation_reason.is_none()
            && (evaluation.snapshot_id.is_some()
                || evaluation.mode == crate::config::AttentionRoutingMode::Baseline);
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("routing_health")?;
            let (persisted_items, selected_items): (i64, i64) = conn.query_row(
                "SELECT COUNT(*), COALESCE(SUM(CASE WHEN selected = 1 THEN 1 ELSE 0 END), 0) \
                 FROM attention_decision_items WHERE decision_id = ?",
                params![decision_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let (verified_items, dedupe_count): (i64, i64) = conn.query_row(
                "SELECT COUNT(DISTINCT CASE WHEN verified = 1 THEN candidate_id END), \
                        COALESCE(SUM(dedupe_count), 0) \
                 FROM attention_impressions \
                 WHERE principal = ? AND workspace = ? AND decision_id = ?",
                params![principal, workspace, decision_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let verified_impression_total: i64 = conn
                .query_row(
                    "SELECT COALESCE(verified_impression_total, 0) \
                 FROM attention_scope_health_counters \
                 WHERE principal = ? AND workspace = ?",
                    params![principal, workspace],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or(0);
            Ok(AttentionRoutingHealth {
                routing_snapshot_valid,
                evaluated_count,
                learned_route_count,
                applied_route_count,
                baseline_retained_count,
                impression_eligible_count: selected_items.max(0) as usize,
                verified_impression_total: verified_impression_total.max(0) as u64,
                decision_item_coverage: if evaluated_count == 0 {
                    1.0
                } else {
                    (persisted_items.max(0) as f64 / evaluated_count as f64).clamp(0.0, 1.0)
                },
                verified_impression_coverage: if selected_items <= 0 {
                    0.0
                } else {
                    (verified_items.max(0) as f64 / selected_items as f64).clamp(0.0, 1.0)
                },
                impression_dedupe_count: dedupe_count.max(0) as u64,
                all_candidates_path: format!(
                    "/api/magician/v2/channel-assist/attention-learning/decisions/{decision_id}"
                ),
            })
        })
        .await
        .context("attention learning routing_health task panicked")?
    }

    pub async fn attention_delivery_health(
        &self,
        principal: &str,
        workspace: &str,
        now: i64,
    ) -> Result<AttentionDeliveryLedgerHealth> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("attention_delivery_health")?;
            let active_root_decisions: i64 = conn.query_row(
                "SELECT COUNT(*) FROM attention_delivery_decisions \
                 WHERE principal = ? AND workspace = ? AND expires_at > ?",
                params![principal, workspace, now],
                |row| row.get(0),
            )?;
            let expired_root_decisions: i64 = conn.query_row(
                "SELECT COUNT(*) FROM attention_delivery_decisions \
                 WHERE principal = ? AND workspace = ? AND expires_at <= ?",
                params![principal, workspace, now],
                |row| row.get(0),
            )?;
            let active_pages: i64 = conn.query_row(
                "SELECT COUNT(*) FROM attention_delivery_pages p \
                 JOIN attention_delivery_decisions d ON d.decision_id = p.decision_id \
                 WHERE d.principal = ? AND d.workspace = ? AND d.expires_at > ?",
                params![principal, workspace, now],
                |row| row.get(0),
            )?;
            let active_cursors: i64 = conn.query_row(
                "SELECT COUNT(*) FROM attention_delivery_cursors \
                 WHERE principal = ? AND workspace = ? AND expires_at > ?",
                params![principal, workspace, now],
                |row| row.get(0),
            )?;
            let delivery_bound_impressions: i64 = conn.query_row(
                "SELECT COUNT(*) FROM attention_impressions \
                 WHERE principal = ? AND workspace = ? AND delivery_id IS NOT NULL",
                params![principal, workspace],
                |row| row.get(0),
            )?;
            let (root_items, supported_root_items): (i64, i64) = conn.query_row(
                "SELECT COUNT(*), COALESCE(SUM(CASE WHEN i.root_policy_propensity > 0.0 \
                    AND i.root_policy_propensity <= 1.0 THEN 1 ELSE 0 END), 0) \
                 FROM attention_delivery_decision_items i \
                 JOIN attention_delivery_decisions d ON d.decision_id = i.decision_id \
                 WHERE d.principal = ? AND d.workspace = ?",
                params![principal, workspace],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let next_expiry_at: Option<i64> = conn.query_row(
                "SELECT MIN(expires_at) FROM attention_delivery_decisions \
                 WHERE principal = ? AND workspace = ? AND expires_at > ?",
                params![principal, workspace, now],
                |row| row.get(0),
            )?;
            Ok(AttentionDeliveryLedgerHealth {
                schema_version: super::delivery::ATTENTION_DELIVERY_SCHEMA_VERSION,
                active_root_decisions: active_root_decisions.max(0) as u64,
                expired_root_decisions: expired_root_decisions.max(0) as u64,
                active_pages: active_pages.max(0) as u64,
                active_cursors: active_cursors.max(0) as u64,
                delivery_bound_impressions: delivery_bound_impressions.max(0) as u64,
                root_propensity_coverage: if root_items <= 0 {
                    1.0
                } else {
                    (supported_root_items.max(0) as f64 / root_items as f64).clamp(0.0, 1.0)
                },
                next_expiry_at,
            })
        })
        .await
        .context("attention delivery health task panicked")?
    }

    /// Compact only expired delivery materializations. Verified impressions
    /// are intentionally outside this retention owner and keep their copied
    /// OPE bindings until the independent impression policy removes them.
    pub async fn compact_attention_deliveries(
        &self,
        principal: &str,
        workspace: &str,
        cutoff_at: i64,
        apply: bool,
    ) -> Result<AttentionRetentionReport> {
        anyhow::ensure!(cutoff_at > 0, "attention delivery cutoff must be positive");
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let mut conn = if apply {
                AttentionConnectionGuard::Writer(
                    store
                        .conn
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()),
                )
            } else {
                AttentionConnectionGuard::Reader(
                    store.reads.acquire("compact_attention_deliveries_preview")?,
                )
            };
            let queries = [
                ("delivery_decisions", "SELECT COUNT(*) FROM attention_delivery_decisions WHERE principal = ? AND workspace = ? AND expires_at <= ?"),
                ("delivery_decision_items", "SELECT COUNT(*) FROM attention_delivery_decision_items i JOIN attention_delivery_decisions d ON d.decision_id = i.decision_id WHERE d.principal = ? AND d.workspace = ? AND d.expires_at <= ?"),
                ("delivery_pages", "SELECT COUNT(*) FROM attention_delivery_pages p JOIN attention_delivery_decisions d ON d.decision_id = p.decision_id WHERE d.principal = ? AND d.workspace = ? AND d.expires_at <= ?"),
                ("delivery_page_items", "SELECT COUNT(*) FROM attention_delivery_page_items i JOIN attention_delivery_decisions d ON d.decision_id = i.decision_id WHERE d.principal = ? AND d.workspace = ? AND d.expires_at <= ?"),
                ("delivery_cursors", "SELECT COUNT(*) FROM attention_delivery_cursors WHERE principal = ? AND workspace = ? AND expires_at <= ?"),
            ];
            let mut affected_rows = BTreeMap::new();
            for (name, query) in queries {
                let count: i64 = conn.query_row(query, params![principal, workspace, cutoff_at], |row| row.get(0))?;
                affected_rows.insert(name.to_string(), count.max(0) as u64);
            }
            if apply {
                let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
                tx.execute("DELETE FROM attention_delivery_cursors WHERE principal = ? AND workspace = ? AND expires_at <= ?", params![principal, workspace, cutoff_at])?;
                for table in ["attention_delivery_page_items", "attention_delivery_pages", "attention_delivery_decision_items"] {
                    tx.execute(
                        &format!("DELETE FROM {table} WHERE decision_id IN (SELECT decision_id FROM attention_delivery_decisions WHERE principal = ? AND workspace = ? AND expires_at <= ?)"),
                        params![principal, workspace, cutoff_at],
                    )?;
                }
                tx.execute("DELETE FROM attention_delivery_decisions WHERE principal = ? AND workspace = ? AND expires_at <= ?", params![principal, workspace, cutoff_at])?;
                tx.commit()?;
            }
            Ok(AttentionRetentionReport {
                principal,
                workspace,
                apply,
                cutoff_at: Some(cutoff_at),
                affected_rows,
            })
        })
        .await
        .context("attention delivery compaction task panicked")?
    }

    /// Retention owner for analytical rows. Posterior updates are checkpointed
    /// into an immutable content digest while their compact update/outcome
    /// idempotency anchors remain. Current posterior state and global immutable
    /// snapshots are retained.
    /// Oldest decision this scope still holds, the anchor a narrowed
    /// automatic pass slices forward from. Cheap: one indexed MIN.
    pub async fn oldest_retention_anchor(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Option<i64>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = store.reads.acquire("oldest_retention_anchor")?;
            let oldest: Option<i64> = conn.query_row(
                "SELECT MIN(decided_at) FROM attention_decisions \
                 WHERE principal = ? AND workspace = ?",
                params![principal, workspace],
                |row| row.get(0),
            )?;
            Ok(oldest)
        })
        .await
        .context("attention retention anchor task panicked")?
    }

    pub async fn apply_scoped_retention(
        &self,
        principal: &str,
        workspace: &str,
        cutoff_at: i64,
        apply: bool,
    ) -> Result<AttentionRetentionReport> {
        anyhow::ensure!(cutoff_at > 0, "attention retention cutoff must be positive");
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            // The eligibility counts are a report, not a lock: they run on a
            // read connection even for an applied pass, so the store's single
            // writer is held only for the transaction below. Counting under
            // the writer had every persisting request queue behind a survey.
            let conn = store.reads.acquire(if apply {
                "apply_scoped_retention_survey"
            } else {
                "apply_scoped_retention_preview"
            })?;
            let tables = [
                ("bandit_updates_checkpointed", "attention_bandit_updates", "occurred_at"),
                ("impressions", "attention_impressions", "last_recorded_at"),
                ("decisions", "attention_decisions", "decided_at"),
                ("canonical_projections", "attention_canonical_projections", "created_at"),
                ("outcomes", "attention_outcomes", "occurred_at"),
                ("pair_labels", "attention_pair_labels", "occurred_at"),
                ("embeddings", "attention_candidate_embeddings", "updated_at"),
                ("scores", "attention_candidate_scores", "updated_at"),
                ("actionability_scores", "attention_actionability_scores", "updated_at"),
                ("feature_bindings", "attention_candidate_feature_bindings", "last_served_at"),
            ];
            let mut affected_rows = BTreeMap::new();
            for (name, table, time_column) in tables {
                let count: i64 = conn.query_row(
                    &format!(
                        "SELECT COUNT(*) FROM {table} WHERE principal = ? AND workspace = ? AND {time_column} <= ?"
                    ),
                    params![principal, workspace, cutoff_at],
                    |row| row.get(0),
                )?;
                affected_rows.insert(name.to_string(), count.max(0) as u64);
            }
            let retirable_jobs: i64 = conn.query_row(
                &format!(
                    "SELECT COUNT(*) FROM attention_rank_recompute_jobs j \
                     WHERE j.principal = ?2 AND j.workspace = ?3 AND {RETIRABLE_RECOMPUTE_JOB_SQL}"
                ),
                params![cutoff_at, principal, workspace],
                |row| row.get(0),
            )?;
            affected_rows.insert(
                "rank_recompute_jobs".to_string(),
                retirable_jobs.max(0) as u64,
            );
            let deletable_decisions: i64 = conn.query_row(
                &format!(
                    "SELECT COUNT(*) FROM attention_decisions d \
                     WHERE d.principal = ?2 AND d.workspace = ?3 AND {RETIRABLE_DECISION_SQL}"
                ),
                params![cutoff_at, principal, workspace],
                |row| row.get(0),
            )?;
            affected_rows.insert(
                "decisions".to_string(),
                deletable_decisions.max(0) as u64,
            );
            let deletable_outcomes: i64 = conn.query_row(
                &format!(
                    "SELECT COUNT(*) FROM attention_outcomes o \
                     WHERE o.principal = ?2 AND o.workspace = ?3 AND {RETIRABLE_OUTCOME_SQL}"
                ),
                params![cutoff_at, principal, workspace],
                |row| row.get(0),
            )?;
            affected_rows.insert(
                "outcomes".to_string(),
                deletable_outcomes.max(0) as u64,
            );
            let deletable_projections: i64 = conn.query_row(
                "SELECT COUNT(*) FROM attention_canonical_projections p \
                 WHERE p.principal = ? AND p.workspace = ? AND p.created_at <= ? \
                   AND NOT EXISTS (SELECT 1 FROM attention_delivery_decisions d \
                       WHERE d.projection_id = p.projection_id) \
                   AND NOT EXISTS (SELECT 1 FROM attention_decisions d \
                       WHERE d.decision_id = p.projection_id \
                          OR (p.schema_version < ? \
                              AND d.candidate_set_digest = p.universe_digest)) \
                   AND NOT EXISTS (SELECT 1 FROM attention_outcomes o \
                       WHERE o.principal = p.principal AND o.workspace = p.workspace \
                         AND (o.projection_id = p.projection_id OR o.decision_id = p.projection_id)) \
                   AND NOT EXISTS (SELECT 1 FROM attention_impressions i \
                       WHERE i.principal = p.principal AND i.workspace = p.workspace \
                         AND i.projection_id = p.projection_id) \
                   AND NOT EXISTS (SELECT 1 FROM attention_rank_recompute_jobs j \
                       WHERE j.principal = p.principal AND j.workspace = p.workspace \
                         AND (j.projection_id = p.projection_id OR j.decision_id = p.projection_id))",
                params![
                    principal,
                    workspace,
                    cutoff_at,
                    NORMALIZED_CANONICAL_PROJECTION_SCHEMA_VERSION,
                ],
                |row| row.get(0),
            )?;
            affected_rows.insert(
                "canonical_projections".to_string(),
                deletable_projections.max(0) as u64,
            );
            let terminal_semantic_work: i64 = conn.query_row(
                "SELECT COUNT(*) FROM attention_semantic_extraction_work
                 WHERE principal = ? AND workspace = ? AND updated_at <= ?
                   AND status IN ('succeeded', 'missing', 'invalid', 'dead')",
                params![principal, workspace, cutoff_at],
                |row| row.get(0),
            )?;
            affected_rows.insert(
                "semantic_extraction_work".to_string(),
                terminal_semantic_work.max(0) as u64,
            );
            let terminal_embedding_bind_work: i64 = conn.query_row(
                "SELECT COUNT(*) FROM attention_embedding_bind_work
                 WHERE principal = ? AND workspace = ? AND updated_at <= ? AND status = 'dead'",
                params![principal, workspace, cutoff_at],
                |row| row.get(0),
            )?;
            affected_rows.insert(
                "embedding_bind_work_dead".to_string(),
                terminal_embedding_bind_work.max(0) as u64,
            );
            if !apply {
                return Ok(AttentionRetentionReport {
                    principal,
                    workspace,
                    apply,
                    cutoff_at: Some(cutoff_at),
                    affected_rows,
                });
            }
            drop(conn);
            let mut conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .context("opening scoped attention retention transaction")?;
            let mut group_statement = tx.prepare(
                "SELECT surface, snapshot_id FROM attention_bandit_updates \
                 WHERE principal = ? AND workspace = ? AND occurred_at <= ? \
                 GROUP BY surface, snapshot_id",
            )?;
            let group_rows = group_statement.query_map(params![principal, workspace, cutoff_at], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let groups = group_rows.collect::<std::result::Result<Vec<_>, _>>()?;
            drop(group_statement);
            for (surface, snapshot_id) in groups {
                let mut statement = tx.prepare(
                    "SELECT outcome_id FROM attention_bandit_updates \
                     WHERE principal = ? AND workspace = ? AND surface = ? AND snapshot_id = ? \
                       AND occurred_at <= ? ORDER BY outcome_id",
                )?;
                let ids = statement
                    .query_map(
                        params![principal, workspace, surface, snapshot_id, cutoff_at],
                        |row| row.get::<_, String>(0),
                    )?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                let mut digest = blake3::Hasher::new();
                for id in &ids {
                    digest.update(id.as_bytes());
                    digest.update(b"\0");
                }
                let posterior = tx.query_row(
                    "SELECT version, state_json FROM attention_bandit_posteriors \
                     WHERE principal = ? AND workspace = ? AND surface = ? AND snapshot_id = ?",
                    params![principal, workspace, surface, snapshot_id],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                );
                let (version, posterior_json) = match posterior {
                    Ok(value) => value,
                    Err(rusqlite::Error::QueryReturnedNoRows) => (0, "null".to_string()),
                    Err(error) => return Err(error.into()),
                };
                tx.execute(
                    "INSERT INTO attention_bandit_posterior_compactions ( \
                        compaction_id, principal, workspace, surface, snapshot_id, posterior_version, \
                        compacted_through_at, update_count, update_digest, posterior_json, created_at \
                     ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    params![
                        uuid::Uuid::new_v4().to_string(),
                        principal,
                        workspace,
                        surface,
                        snapshot_id,
                        version,
                        cutoff_at,
                        usize_to_i64(ids.len()),
                        digest.finalize().to_hex().to_string(),
                        posterior_json,
                        chrono::Utc::now().timestamp_millis(),
                    ],
                )?;
            }
            // Keep the compact immutable update row as the outcome-id
            // idempotency anchor. Deleting both it and its canonical outcome
            // would allow a delayed replay of the same client event to update
            // the already-compacted posterior a second time. The checkpoint
            // above owns replay/audit state; a future schema may replace these
            // rows with explicit tombstones before physically removing them.
            let deletes_started_at = tx.total_changes();
            tx.execute(
                "DELETE FROM attention_impressions WHERE principal = ? AND workspace = ? AND last_recorded_at <= ?",
                params![principal, workspace, cutoff_at],
            )?;
            tx.execute(
                &format!(
                    "DELETE FROM attention_rank_recompute_jobs WHERE job_id IN ( \
                       SELECT j.job_id FROM attention_rank_recompute_jobs j \
                       WHERE j.principal = ?2 AND j.workspace = ?3 AND {RETIRABLE_RECOMPUTE_JOB_SQL})"
                ),
                params![cutoff_at, principal, workspace],
            )?;
            // Outcomes go before decision items: an outcome older than the
            // cutoff whose terminal job just retired must not pin its
            // decision's items for one more pass.
            tx.execute(
                &format!(
                    "DELETE FROM attention_outcomes WHERE outcome_id IN ( \
                       SELECT o.outcome_id FROM attention_outcomes o \
                       WHERE o.principal = ?2 AND o.workspace = ?3 AND {RETIRABLE_OUTCOME_SQL})"
                ),
                params![cutoff_at, principal, workspace],
            )?;
            tx.execute(
                "DELETE FROM attention_decision_items WHERE principal = ? AND workspace = ? AND decision_id IN ( \
                   SELECT decision_id FROM attention_decisions d WHERE d.principal = ? AND d.workspace = ? \
                     AND d.decided_at <= ? \
                     AND NOT EXISTS (SELECT 1 FROM attention_impressions i WHERE i.decision_id = d.decision_id) \
                     AND NOT EXISTS (SELECT 1 FROM attention_outcomes o WHERE o.decision_id = d.decision_id) \
                     AND NOT EXISTS (SELECT 1 FROM attention_rank_recompute_jobs j WHERE j.decision_id = d.decision_id) \
                 )",
                params![principal, workspace, principal, workspace, cutoff_at],
            )?;
            tx.execute(
                "DELETE FROM attention_decisions WHERE principal = ? AND workspace = ? AND decided_at <= ? \
                   AND NOT EXISTS (SELECT 1 FROM attention_decision_items i WHERE i.decision_id = attention_decisions.decision_id)",
                params![principal, workspace, cutoff_at],
            )?;
            tx.execute(
                "DELETE FROM attention_canonical_projections \
                 WHERE principal = ? AND workspace = ? AND created_at <= ? \
                   AND NOT EXISTS (SELECT 1 FROM attention_delivery_decisions d \
                       WHERE d.projection_id = attention_canonical_projections.projection_id) \
                   AND NOT EXISTS (SELECT 1 FROM attention_decisions d \
                       WHERE d.decision_id = attention_canonical_projections.projection_id \
                          OR (attention_canonical_projections.schema_version < ? \
                              AND d.candidate_set_digest = attention_canonical_projections.universe_digest)) \
                   AND NOT EXISTS (SELECT 1 FROM attention_outcomes o \
                       WHERE o.principal = attention_canonical_projections.principal \
                         AND o.workspace = attention_canonical_projections.workspace \
                         AND (o.projection_id = attention_canonical_projections.projection_id \
                              OR o.decision_id = attention_canonical_projections.projection_id)) \
                   AND NOT EXISTS (SELECT 1 FROM attention_impressions i \
                       WHERE i.principal = attention_canonical_projections.principal \
                         AND i.workspace = attention_canonical_projections.workspace \
                         AND i.projection_id = attention_canonical_projections.projection_id) \
                   AND NOT EXISTS (SELECT 1 FROM attention_rank_recompute_jobs j \
                       WHERE j.principal = attention_canonical_projections.principal \
                         AND j.workspace = attention_canonical_projections.workspace \
                         AND (j.projection_id = attention_canonical_projections.projection_id \
                              OR j.decision_id = attention_canonical_projections.projection_id))",
                params![
                    principal,
                    workspace,
                    cutoff_at,
                    NORMALIZED_CANONICAL_PROJECTION_SCHEMA_VERSION,
                ],
            )?;
            tx.execute(
                "DELETE FROM attention_legacy_migration_quarantine \
                 WHERE principal = ? AND workspace = ? AND ( \
                     (migration_kind = ? AND NOT EXISTS ( \
                         SELECT 1 FROM attention_canonical_projections p \
                         WHERE p.projection_id = attention_legacy_migration_quarantine.source_key \
                     )) OR \
                     (migration_kind = ? AND NOT EXISTS ( \
                         SELECT 1 FROM attention_candidate_feature_snapshots f \
                         WHERE hex(f.principal) || ':' || hex(f.workspace) || ':' || \
                               hex(f.surface) || ':' || hex(f.candidate_id) || ':' || \
                               hex(f.source_revision) || ':' || hex(f.content_digest) = \
                               attention_legacy_migration_quarantine.source_key \
                     )) \
                 )",
                params![
                    principal,
                    workspace,
                    LEGACY_CANONICAL_MIGRATION_KIND,
                    LEGACY_FEATURE_MIGRATION_KIND,
                ],
            )?;
            tx.execute(
                "DELETE FROM attention_semantic_extraction_work
                 WHERE principal = ? AND workspace = ? AND updated_at <= ?
                   AND status IN ('succeeded', 'missing', 'invalid', 'dead')",
                params![principal, workspace, cutoff_at],
            )?;
            tx.execute(
                "DELETE FROM attention_embedding_bind_work
                 WHERE principal = ? AND workspace = ? AND updated_at <= ? AND status = 'dead'",
                params![principal, workspace, cutoff_at],
            )?;
            for (table, time_column) in [
                ("attention_pair_labels", "occurred_at"),
                ("attention_candidate_embeddings", "updated_at"),
                ("attention_candidate_scores", "updated_at"),
                ("attention_actionability_scores", "updated_at"),
            ] {
                tx.execute(
                    &format!(
                        "DELETE FROM {table} WHERE principal = ? AND workspace = ? AND {time_column} <= ?"
                    ),
                    params![principal, workspace, cutoff_at],
                )?;
            }
            tx.execute(
                "DELETE FROM attention_candidate_feature_bindings \
                 WHERE principal = ? AND workspace = ? AND last_served_at <= ? \
                   AND NOT EXISTS (SELECT 1 FROM attention_decision_items i \
                       WHERE i.principal = attention_candidate_feature_bindings.principal \
                         AND i.workspace = attention_candidate_feature_bindings.workspace \
                         AND i.served_route = attention_candidate_feature_bindings.surface \
                         AND i.candidate_id = attention_candidate_feature_bindings.candidate_id \
                         AND COALESCE(i.source_revision, '') = attention_candidate_feature_bindings.source_revision \
                         AND i.feature_snapshot_digest = attention_candidate_feature_bindings.content_digest) \
                   AND NOT EXISTS (SELECT 1 FROM attention_outcomes o \
                       WHERE o.principal = attention_candidate_feature_bindings.principal \
                         AND o.workspace = attention_candidate_feature_bindings.workspace \
                         AND o.surface = attention_candidate_feature_bindings.surface \
                         AND (o.candidate_id = attention_candidate_feature_bindings.candidate_id \
                              OR o.surface || ':' || o.candidate_id = attention_candidate_feature_bindings.candidate_id) \
                         AND COALESCE(o.source_revision, '') = attention_candidate_feature_bindings.source_revision \
                         AND (o.decision_id IS NULL OR EXISTS ( \
                           SELECT 1 FROM attention_decision_items i \
                            WHERE i.principal = o.principal AND i.workspace = o.workspace \
                              AND i.decision_id = o.decision_id \
                              AND i.served_route = o.surface \
                              AND (i.candidate_id = o.candidate_id \
                                   OR i.candidate_id = o.surface || ':' || o.candidate_id) \
                              AND COALESCE(i.source_revision, '') = COALESCE(o.source_revision, '') \
                              AND i.feature_snapshot_digest = attention_candidate_feature_bindings.content_digest)))",
                params![principal, workspace, cutoff_at],
            )?;
            let mut retired_rows = tx.total_changes().saturating_sub(deletes_started_at);
            // The body sweeps reclaim what the deletes above orphaned. They
            // are whole-table anti-joins over the largest tables in the
            // store, so a pass that retired nothing has nothing for them to
            // find and must not pay for them under the writer.
            if retired_rows > 0 {
                tx.execute(
                    "DELETE FROM attention_feature_vectors \
                     WHERE NOT EXISTS (SELECT 1 FROM attention_candidate_feature_bindings b \
                         WHERE b.content_digest = attention_feature_vectors.content_digest) \
                       AND NOT EXISTS (SELECT 1 FROM attention_decision_items i \
                         WHERE i.feature_snapshot_digest = attention_feature_vectors.content_digest)",
                    [],
                )?;
                tx.execute(
                    "DELETE FROM attention_canonical_item_revisions \
                     WHERE NOT EXISTS (SELECT 1 FROM attention_canonical_projection_members m \
                         WHERE m.item_digest = attention_canonical_item_revisions.item_digest)",
                    [],
                )?;
                tx.execute(
                    "DELETE FROM attention_canonical_diagnostic_revisions \
                     WHERE NOT EXISTS (SELECT 1 FROM attention_canonical_projection_members m \
                         WHERE m.rank_digest = attention_canonical_diagnostic_revisions.diagnostic_digest \
                            OR m.decision_digest = attention_canonical_diagnostic_revisions.diagnostic_digest)",
                    [],
                )?;
                retired_rows = tx.total_changes().saturating_sub(deletes_started_at);
            }
            tx.commit().context("committing scoped attention retention")?;
            affected_rows.insert(RETENTION_RETIRED_ROWS_KEY.to_string(), retired_rows);
            Ok(AttentionRetentionReport {
                principal,
                workspace,
                apply,
                cutoff_at: Some(cutoff_at),
                affected_rows,
            })
        })
        .await
        .context("attention learning retention task panicked")?
    }

    /// Best-effort automatic retention floor: the ordinary scoped retention
    /// pass plus delivery compaction in a background task, on the cadence
    /// [`auto_retention_gate`] decides. Guards and tier semantics are exactly
    /// the operator path ([`Self::apply_scoped_retention`] +
    /// [`Self::compact_attention_deliveries`]); only the trigger is new. The
    /// gate records the pass as in flight before spawning, and its finish —
    /// with whether it retired anything — when it completes; a failed pass
    /// clears the gate so the next persist retries.
    pub fn schedule_scoped_retention_if_due(
        &self,
        principal: &str,
        workspace: &str,
        retention_days: u64,
    ) {
        let registry = AUTO_SCOPED_RETENTION_LAST_RUN
            .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
        let key = (principal.to_string(), workspace.to_string());
        let now_ms = chrono::Utc::now().timestamp_millis();
        // The cheap gate is consulted first, so a hot loop of persists cannot
        // reach the file stat below: an in-flight pass or one that finished
        // less than the oversized interval ago skips before the syscall.
        let previous = {
            let registry = registry
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            registry.get(&key).copied()
        };
        if auto_retention_gate(previous.as_ref(), now_ms, true) == AutoRetentionGate::Skip {
            return;
        }
        let db_bytes = std::fs::metadata(self.path.as_path())
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        // Over the guard the pass is NARROWED, never skipped. Skipping was a
        // trap: the only thing that shrinks the file is the pass being
        // refused, so a database that crossed the guard stayed over it for
        // ever and grew. Measured 2026-09-15: 17.25 GB against an 8 GB guard,
        // with ~6M rows eligible for an ordinary 30-day pass that had not run
        // in months. The guard's real intent is to bound the writer hold, and
        // a one-slice cutoff does that while still making progress on every
        // interval until the file is back under the limit.
        let oversized = db_bytes > AUTO_SCOPED_RETENTION_MAX_DB_BYTES;
        {
            let mut registry = registry
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // Re-read under the lock: two persists racing past the cheap
            // gate must not both spawn a pass.
            let current = registry.get(&key).copied();
            if current != previous
                || auto_retention_gate(current.as_ref(), now_ms, oversized)
                    == AutoRetentionGate::Skip
            {
                return;
            }
            registry.insert(
                key.clone(),
                AutoRetentionRun {
                    started_at_ms: now_ms,
                    finished_at_ms: None,
                    empty_passes: previous.map_or(0, |run| run.empty_passes),
                },
            );
        }
        let store = self.clone();
        let retention_days = retention_days.clamp(1, 3650);
        tokio::spawn(async move {
            let now = chrono::Utc::now().timestamp_millis();
            let cutoff_at = now.saturating_sub(
                i64::try_from(retention_days.saturating_mul(86_400_000)).unwrap_or(i64::MAX),
            );
            let principal = key.0.clone();
            let workspace = key.1.clone();
            // One slice past the oldest surviving row: everything older than
            // that is exactly one slice of history, because no delete here
            // carries a lower bound. Capped at the policy cutoff so a narrowed
            // pass can never retire anything the operator's retention window
            // still covers. With no anchor there is nothing old to retire, so
            // the ordinary cutoff stands.
            let cutoff_at = if oversized {
                match store.oldest_retention_anchor(&principal, &workspace).await {
                    Ok(Some(oldest)) => {
                        let slice = oldest
                            .saturating_add(AUTO_SCOPED_RETENTION_OVERSIZED_SLICE_MS)
                            .min(cutoff_at);
                        tracing::info!(
                            principal = %principal,
                            workspace = %workspace,
                            db_bytes,
                            slice_cutoff_at = slice,
                            policy_cutoff_at = cutoff_at,
                            "attention database is over the auto size guard; retiring one slice this pass"
                        );
                        slice
                    },
                    Ok(None) => cutoff_at,
                    Err(error) => {
                        tracing::warn!(
                            principal = %principal,
                            workspace = %workspace,
                            error = %error,
                            "could not read the retention anchor; running the ordinary cutoff"
                        );
                        cutoff_at
                    },
                }
            } else {
                cutoff_at
            };
            let started = Instant::now();
            let result = store
                .apply_scoped_retention(&principal, &workspace, cutoff_at.max(1), true)
                .await;
            let delivery = store
                .compact_attention_deliveries(&principal, &workspace, cutoff_at.max(1), true)
                .await;
            let registry = AUTO_SCOPED_RETENTION_LAST_RUN
                .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
            match (result, delivery) {
                (Ok(report), Ok(_)) => {
                    let retired = report
                        .affected_rows
                        .get(RETENTION_RETIRED_ROWS_KEY)
                        .copied()
                        .unwrap_or(0);
                    let finished_at_ms = chrono::Utc::now().timestamp_millis();
                    let empty_passes = {
                        let mut registry = registry
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        let entry = registry.entry(key.clone()).or_insert(AutoRetentionRun {
                            started_at_ms: finished_at_ms,
                            finished_at_ms: None,
                            empty_passes: 0,
                        });
                        entry.finished_at_ms = Some(finished_at_ms);
                        entry.empty_passes = if retired == 0 {
                            entry.empty_passes.saturating_add(1)
                        } else {
                            0
                        };
                        entry.empty_passes
                    };
                    tracing::info!(
                        principal = %principal,
                        workspace = %workspace,
                        oversized,
                        cutoff_at,
                        retired_rows = retired,
                        empty_passes,
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "automatic scoped attention retention pass completed"
                    );
                },
                (result, delivery) => {
                    tracing::warn!(
                        principal = %key.0,
                        workspace = %key.1,
                        retention_error = ?result.err(),
                        delivery_error = ?delivery.err(),
                        "automatic scoped attention retention failed; rolling back the schedule gate so the next persist retries"
                    );
                    if let Ok(mut registry) = registry.lock() {
                        registry.remove(&key);
                    }
                },
            }
        });
    }

    /// Exact scoped deletion boundary. Global immutable model snapshots remain
    /// installed; all principal/workspace personal evidence and posterior
    /// state are deleted in foreign-key-safe order only with `apply=true`.
    pub async fn delete_scope(
        &self,
        principal: &str,
        workspace: &str,
        apply: bool,
    ) -> Result<AttentionRetentionReport> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let mut conn = if apply {
                AttentionConnectionGuard::Writer(
                    store
                        .conn
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()),
                )
            } else {
                AttentionConnectionGuard::Reader(
                    store.reads.acquire("delete_attention_scope_preview")?,
                )
            };
            let tables = [
                "attention_rank_recompute_jobs",
                "attention_bandit_updates",
                "attention_impressions",
                "attention_decision_items",
                "attention_decisions",
                "attention_canonical_projections",
                "attention_embedding_bind_work",
                "attention_outcomes",
                "attention_pair_labels",
                "attention_candidate_embeddings",
                "attention_candidate_scores",
                "attention_actionability_scores",
                "attention_candidate_feature_bindings",
                "attention_candidate_feature_snapshots",
                "attention_legacy_migration_quarantine",
                "attention_bandit_posteriors",
                "attention_bandit_posterior_compactions",
                "attention_rank_generations",
                "attention_group_generations",
                "attention_semantic_extraction_work",
                "attention_semantic_extraction_checkpoints",
                "attention_scope_health_counters",
                "attention_actionability_scope_installs",
                "attention_routing_scope_installs",
                "attention_bandit_scope_installs",
                "attention_training_runs",
                "attention_historical_bootstrap_checkpoints",
            ];
            let mut affected_rows = BTreeMap::new();
            for table in tables {
                let count: i64 = conn.query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE principal = ? AND workspace = ?"),
                    params![principal, workspace],
                    |row| row.get(0),
                )?;
                affected_rows.insert(table.to_string(), count.max(0) as u64);
            }
            for (name, query) in [
                ("attention_delivery_decisions", "SELECT COUNT(*) FROM attention_delivery_decisions WHERE principal = ? AND workspace = ?"),
                ("attention_delivery_decision_items", "SELECT COUNT(*) FROM attention_delivery_decision_items i JOIN attention_delivery_decisions d ON d.decision_id = i.decision_id WHERE d.principal = ? AND d.workspace = ?"),
                ("attention_delivery_pages", "SELECT COUNT(*) FROM attention_delivery_pages p JOIN attention_delivery_decisions d ON d.decision_id = p.decision_id WHERE d.principal = ? AND d.workspace = ?"),
                ("attention_delivery_page_items", "SELECT COUNT(*) FROM attention_delivery_page_items i JOIN attention_delivery_decisions d ON d.decision_id = i.decision_id WHERE d.principal = ? AND d.workspace = ?"),
                ("attention_delivery_cursors", "SELECT COUNT(*) FROM attention_delivery_cursors WHERE principal = ? AND workspace = ?"),
            ] {
                let count: i64 = conn.query_row(query, params![principal, workspace], |row| row.get(0))?;
                affected_rows.insert(name.to_string(), count.max(0) as u64);
            }
            if apply {
                let tx = conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .context("opening attention scope deletion transaction")?;
                tx.execute(
                    "DELETE FROM attention_delivery_cursors WHERE principal = ? AND workspace = ?",
                    params![principal, workspace],
                )?;
                for table in [
                    "attention_delivery_page_items",
                    "attention_delivery_pages",
                    "attention_delivery_decision_items",
                ] {
                    tx.execute(
                        &format!("DELETE FROM {table} WHERE decision_id IN (SELECT decision_id FROM attention_delivery_decisions WHERE principal = ? AND workspace = ?)"),
                        params![principal, workspace],
                    )?;
                }
                tx.execute(
                    "DELETE FROM attention_delivery_decisions WHERE principal = ? AND workspace = ?",
                    params![principal, workspace],
                )?;
                for table in tables {
                    tx.execute(
                        &format!("DELETE FROM {table} WHERE principal = ? AND workspace = ?"),
                        params![principal, workspace],
                    )?;
                }
                tx.execute(
                    "DELETE FROM attention_feature_vectors \
                     WHERE NOT EXISTS (SELECT 1 FROM attention_candidate_feature_bindings b \
                         WHERE b.content_digest = attention_feature_vectors.content_digest) \
                       AND NOT EXISTS (SELECT 1 FROM attention_decision_items i \
                         WHERE i.feature_snapshot_digest = attention_feature_vectors.content_digest)",
                    [],
                )?;
                tx.execute(
                    "DELETE FROM attention_canonical_item_revisions \
                     WHERE NOT EXISTS (SELECT 1 FROM attention_canonical_projection_members m \
                         WHERE m.item_digest = attention_canonical_item_revisions.item_digest)",
                    [],
                )?;
                tx.execute(
                    "DELETE FROM attention_canonical_diagnostic_revisions \
                     WHERE NOT EXISTS (SELECT 1 FROM attention_canonical_projection_members m \
                         WHERE m.rank_digest = attention_canonical_diagnostic_revisions.diagnostic_digest \
                            OR m.decision_digest = attention_canonical_diagnostic_revisions.diagnostic_digest)",
                    [],
                )?;
                tx.commit().context("committing attention scope deletion")?;
            }
            Ok(AttentionRetentionReport {
                principal,
                workspace,
                apply,
                cutoff_at: None,
                affected_rows,
            })
        })
        .await
        .context("attention learning scope deletion task panicked")?
    }
}

fn validate_rank_recompute_reason(reason: &str) -> Result<()> {
    anyhow::ensure!(
        !reason.trim().is_empty()
            && reason.chars().count() <= ATTENTION_RANK_RECOMPUTE_REASON_MAX_CHARS
            && !reason.chars().any(char::is_control),
        "rank recompute reason must contain 1..={} characters without controls",
        ATTENTION_RANK_RECOMPUTE_REASON_MAX_CHARS
    );
    Ok(())
}

fn read_rank_recompute_job_tx(
    tx: &rusqlite::Transaction<'_>,
    key: &str,
    by_outcome: bool,
) -> Result<Option<AttentionRankRecomputeJob>> {
    read_rank_recompute_job_conn(tx, key, by_outcome)
}

fn read_rank_recompute_job_conn(
    conn: &Connection,
    key: &str,
    by_outcome: bool,
) -> Result<Option<AttentionRankRecomputeJob>> {
    let column = if by_outcome { "outcome_id" } else { "job_id" };
    let sql = format!(
        "SELECT job_id, outcome_id, principal, workspace, origin_surface,
                canonical_candidate_id, raw_candidate_id, source_revision, outcome,
                decision_id, delivery_id, impression_id, affected_rank_before,
                enqueue_policy_snapshot_id, enqueue_posterior_version, status, attempts,
                next_retry_at, lease_owner, lease_expires_at, reason, result_json,
                created_at, updated_at, completed_at
         FROM attention_rank_recompute_jobs WHERE {column} = ?"
    );
    let encoded = conn.query_row(&sql, params![key], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, Option<String>>(9)?,
            row.get::<_, Option<String>>(10)?,
            row.get::<_, Option<String>>(11)?,
            row.get::<_, Option<i64>>(12)?,
            row.get::<_, Option<String>>(13)?,
            row.get::<_, Option<i64>>(14)?,
            row.get::<_, String>(15)?,
            row.get::<_, i64>(16)?,
            row.get::<_, Option<i64>>(17)?,
            row.get::<_, Option<String>>(18)?,
            row.get::<_, Option<i64>>(19)?,
            row.get::<_, Option<String>>(20)?,
            row.get::<_, Option<String>>(21)?,
            row.get::<_, i64>(22)?,
            row.get::<_, i64>(23)?,
            row.get::<_, Option<i64>>(24)?,
        ))
    });
    let (
        job_id,
        outcome_id,
        principal,
        workspace,
        surface,
        canonical_candidate_id,
        raw_candidate_id,
        source_revision,
        outcome,
        decision_id,
        delivery_id,
        impression_id,
        affected_rank_before,
        snapshot_id,
        posterior_version,
        status,
        attempts,
        next_retry_at,
        lease_owner,
        lease_expires_at,
        reason,
        result_json,
        created_at,
        updated_at,
        completed_at,
    ) = match encoded {
        Ok(row) => row,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let result = result_json
        .as_deref()
        .map(|encoded| {
            parse_bounded_stored_json::<AttentionRankRecomputeResult>(
                encoded,
                "attention rank recompute result",
                MAX_ATTENTION_STORED_JSON_BYTES,
            )
        })
        .transpose()?;
    Ok(Some(AttentionRankRecomputeJob {
        job_id,
        outcome_id,
        status: AttentionRankRecomputeStatus::from_str(&status)?,
        origin_surface: AttentionSurface::from_str(&surface)?,
        canonical_candidate_id,
        raw_candidate_id,
        source_revision,
        outcome: AttentionOutcomeKind::from_str(&outcome)?,
        decision_id,
        delivery_id,
        impression_id,
        affected_rank_before: affected_rank_before.map(|value| value.max(0) as usize),
        enqueue_policy_snapshot_id: snapshot_id,
        enqueue_posterior_version: posterior_version.map(|value| value.max(0) as u64),
        attempts: attempts.max(0) as u32,
        next_retry_at,
        lease_expires_at,
        created_at,
        updated_at,
        completed_at,
        reason,
        result,
        principal,
        workspace,
        lease_owner,
    }))
}

fn route_summary_json(routes: impl Iterator<Item = AttentionRoute>) -> Result<String> {
    let mut summary = std::collections::BTreeMap::<&'static str, usize>::new();
    for route in routes {
        *summary.entry(route.as_str()).or_default() += 1;
    }
    serde_json::to_string(&summary).context("serializing attention route summary")
}

fn read_bandit_posterior_tx(
    tx: &rusqlite::Transaction<'_>,
    principal: &str,
    workspace: &str,
    surface: AttentionSurface,
    snapshot: &AttentionBanditPolicySnapshot,
) -> Result<AttentionBanditPosteriorState> {
    let encoded = tx.query_row(
        "SELECT state_json FROM attention_bandit_posteriors \
         WHERE principal = ? AND workspace = ? AND surface = ? AND snapshot_id = ?",
        params![principal, workspace, surface.as_str(), snapshot.snapshot_id],
        |row| row.get::<_, String>(0),
    );
    match encoded {
        Ok(encoded) => {
            let posterior: AttentionBanditPosteriorState = parse_bounded_stored_json(
                &encoded,
                "scoped bandit posterior",
                MAX_ATTENTION_STORED_JSON_BYTES,
            )?;
            posterior.validate(snapshot)?;
            Ok(posterior)
        },
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            AttentionBanditPosteriorState::from_prior(snapshot)
        },
        Err(error) => Err(error.into()),
    }
}

fn read_bandit_update_receipt(
    tx: &rusqlite::Transaction<'_>,
    outcome_id: &str,
) -> Result<Option<AttentionPosteriorUpdateReceipt>> {
    let row = tx.query_row(
        "SELECT snapshot_id, attribution_quality, degradation_reason, \
                posterior_version_before, posterior_version_after, uncertainty_before, \
                uncertainty_after, affected_rank_before, update_applied \
         FROM attention_bandit_updates WHERE outcome_id = ?",
        params![outcome_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<f64>>(5)?,
                row.get::<_, Option<f64>>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, i64>(8)?,
            ))
        },
    );
    let (
        snapshot_id,
        quality,
        degradation_reason,
        version_before,
        version_after,
        uncertainty_before,
        uncertainty_after,
        affected_rank_before,
        update_applied,
    ) = match row {
        Ok(row) => row,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let attribution_quality = match quality.as_str() {
        "verified_impression" => AttentionBanditAttributionQuality::VerifiedImpression,
        "decision_only" => AttentionBanditAttributionQuality::DecisionOnly,
        "mismatch" => AttentionBanditAttributionQuality::Mismatch,
        "outside_window" => AttentionBanditAttributionQuality::OutsideWindow,
        _ => AttentionBanditAttributionQuality::Missing,
    };
    Ok(Some(AttentionPosteriorUpdateReceipt {
        status: AttentionPosteriorUpdateStatus::Duplicate,
        policy_snapshot_id: Some(snapshot_id),
        posterior_version_before: version_before.map(|value| value.max(0) as u64),
        posterior_version_after: version_after.map(|value| value.max(0) as u64),
        attribution_quality,
        degradation_reason,
        uncertainty_before,
        uncertainty_after,
        affected_rank_before: affected_rank_before.map(|value| value.max(0) as usize),
        affected_rank_after: None,
        affected_rank_delta: None,
        rescore_scheduled: update_applied == 1,
    }))
}

fn candidate_id_aliases(canonical_candidate_id: &str, raw_candidate_id: &str) -> Vec<String> {
    let mut ids = Vec::with_capacity(6);
    let push = |ids: &mut Vec<String>, value: &str| {
        if !value.is_empty() && !ids.iter().any(|existing| existing == value) {
            ids.push(value.to_string());
        }
    };
    push(&mut ids, canonical_candidate_id);
    push(&mut ids, raw_candidate_id);
    for surface in ["follow_up", "worth_a_look"] {
        push(&mut ids, &format!("{surface}:{raw_candidate_id}"));
        if let Some(stripped) = canonical_candidate_id.strip_prefix(&format!("{surface}:")) {
            push(&mut ids, stripped);
        }
        if let Some(stripped) = raw_candidate_id.strip_prefix(&format!("{surface}:")) {
            push(&mut ids, stripped);
            push(&mut ids, &format!("{surface}:{stripped}"));
        }
    }
    ids
}

/// Most recent selected, ranked item matching the exact source revision before
/// the owner outcome. Candidate aliases support legacy raw IDs, but a later or
/// wrong-revision decision cannot stand in for missing historical evidence.
fn reconstruct_served_decision_id(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    canonical_candidate_id: &str,
    raw_candidate_id: &str,
    source_revision: Option<&str>,
    decided_at_latest: i64,
) -> Result<Option<String>> {
    let aliases = candidate_id_aliases(canonical_candidate_id, raw_candidate_id);
    if aliases.is_empty() {
        return Ok(None);
    }
    let placeholders = std::iter::repeat_n("?", aliases.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT i.decision_id \
         FROM attention_decision_items i \
         JOIN attention_decisions d ON d.decision_id = i.decision_id \
         WHERE i.principal = ? AND i.workspace = ? \
           AND i.candidate_id IN ({placeholders}) \
           AND i.selected = 1 AND i.served_rank > 0 \
           AND i.source_revision IS ? AND d.decided_at <= ? \
           AND d.principal = i.principal AND d.workspace = i.workspace \
         ORDER BY d.decided_at DESC, d.decision_id DESC \
         LIMIT 1"
    );
    let mut values: Vec<rusqlite::types::Value> = vec![
        rusqlite::types::Value::Text(principal.to_string()),
        rusqlite::types::Value::Text(workspace.to_string()),
    ];
    values.extend(aliases.into_iter().map(rusqlite::types::Value::Text));
    values.push(match source_revision {
        Some(revision) => rusqlite::types::Value::Text(revision.to_string()),
        None => rusqlite::types::Value::Null,
    });
    values.push(rusqlite::types::Value::Integer(decided_at_latest));
    conn.query_row(&sql, params_from_iter(values), |row| {
        row.get::<_, String>(0)
    })
    .optional()
    .map_err(Into::into)
}

fn usize_to_i64(value: usize) -> i64 {
    value.min(i64::MAX as usize) as i64
}

fn u64_to_i64(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

const fn bool_to_i64(value: bool) -> i64 {
    if value {
        1
    } else {
        0
    }
}

fn validate_impression_request(
    request: &RecordAttentionImpression,
    configured_visibility_rule_version: &str,
) -> std::result::Result<(), AttentionImpressionError> {
    let bounded = |value: &str, max: usize| {
        !value.trim().is_empty()
            && value.chars().count() <= max
            && !value.chars().any(char::is_control)
    };
    if request.visible_ms == 0
        || request.visible_ms > ATTENTION_VISIBLE_MS_MAX
        || !bounded(&request.event_id, ATTENTION_EVENT_ID_MAX_CHARS)
        || !bounded(&request.decision_id, ATTENTION_DECISION_ID_MAX_CHARS)
        || !bounded(&request.delivery_id, ATTENTION_DECISION_ID_MAX_CHARS)
        || request.position == 0
        || !bounded(&request.exposure_token, 128)
        || !bounded(&request.candidate_id, ATTENTION_CANDIDATE_ID_MAX_CHARS)
        || !bounded(
            &request.visibility_rule_version,
            ATTENTION_VISIBILITY_RULE_MAX_CHARS,
        )
        || !bounded(&request.client_type, ATTENTION_CLIENT_TYPE_MAX_CHARS)
        || !bounded(&request.client_version, ATTENTION_CLIENT_VERSION_MAX_CHARS)
        || !bounded(&request.viewport_class, ATTENTION_VIEWPORT_CLASS_MAX_CHARS)
        || request
            .source_revision
            .as_deref()
            .is_some_and(|value| !bounded(value, ATTENTION_SOURCE_REVISION_MAX_CHARS))
    {
        return Err(AttentionImpressionError::InvalidRequest(
            format!(
                "visible_ms must be within 1..={ATTENTION_VISIBLE_MS_MAX}; identity fields must be bounded, non-empty, and free of control characters"
            ),
        ));
    }
    if request.visibility_rule_version != configured_visibility_rule_version {
        return Err(AttentionImpressionError::VisibilityRuleMismatch);
    }
    Ok(())
}

struct ExistingAttentionOutcome {
    persisted: PersistedAttentionOutcome,
    surface: String,
    candidate_id: String,
    source_revision: Option<String>,
    outcome: AttentionOutcomeKind,
    reason: Option<String>,
    label_quality: String,
    decision_id: Option<String>,
    impression_id: Option<String>,
    delivery_id: Option<String>,
    projection_id: Option<String>,
}

/// Resolve an attribution claim to the immutable canonical projection it came
/// from. Claims remain non-authoritative: zero matches means no durable
/// projection binding, while conflicting ledger matches fail closed.
fn resolve_projection_reference(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    decision_id: Option<&str>,
    impression_id: Option<&str>,
    delivery_id: Option<&str>,
) -> Result<Option<String>> {
    let mut candidates = HashSet::new();
    if let Some(impression_id) = impression_id {
        if let Some(projection_id) = conn
            .query_row(
                "SELECT projection_id FROM attention_impressions \
                 WHERE principal = ? AND workspace = ? AND impression_id = ?",
                params![principal, workspace, impression_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
        {
            candidates.insert(projection_id);
        }
    }
    if let Some(decision_id) = decision_id {
        if let Some(projection_id) = conn
            .query_row(
                "SELECT projection_id FROM attention_delivery_decisions \
                 WHERE principal = ? AND workspace = ? AND decision_id = ?",
                params![principal, workspace, decision_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            candidates.insert(projection_id);
        }
        if let Some(projection_id) = conn
            .query_row(
                "SELECT projection_id FROM attention_canonical_projections \
                 WHERE principal = ? AND workspace = ? AND projection_id = ?",
                params![principal, workspace, decision_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            candidates.insert(projection_id);
        }
    }
    if let Some(delivery_id) = delivery_id {
        if let Some(projection_id) = conn
            .query_row(
                "SELECT d.projection_id FROM attention_delivery_pages p \
                 JOIN attention_delivery_decisions d ON d.decision_id = p.decision_id \
                 WHERE d.principal = ? AND d.workspace = ? AND p.delivery_id = ?",
                params![principal, workspace, delivery_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
        {
            candidates.insert(projection_id);
        }
    }
    anyhow::ensure!(
        candidates.len() <= 1,
        "attention attribution resolves to conflicting canonical projections"
    );
    Ok(candidates.into_iter().next())
}

fn attribution_replay_compatible(
    existing_decision_id: Option<&str>,
    existing_impression_id: Option<&str>,
    existing_delivery_id: Option<&str>,
    requested_decision_id: Option<&str>,
    requested_impression_id: Option<&str>,
    requested_delivery_id: Option<&str>,
) -> bool {
    let optional_component_compatible = |existing: Option<&str>, requested: Option<&str>| {
        matches!((existing, requested), (None, None) | (None, Some(_)))
            || matches!((existing, requested), (Some(left), Some(right)) if left == right)
    };
    match (existing_decision_id, requested_decision_id) {
        (None, None) => {
            existing_impression_id == requested_impression_id
                && existing_delivery_id == requested_delivery_id
        },
        (None, Some(_)) => existing_impression_id.is_none() && existing_delivery_id.is_none(),
        (Some(existing), Some(requested)) if existing == requested => {
            optional_component_compatible(existing_impression_id, requested_impression_id)
                && optional_component_compatible(existing_delivery_id, requested_delivery_id)
        },
        _ => false,
    }
}

fn read_outcome_by_event(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    event_id: &str,
) -> Result<Option<ExistingAttentionOutcome>> {
    let row = conn.query_row(
        "SELECT outcome_id, surface, candidate_id, source_revision, outcome, reason, label_quality, \
                decision_id, impression_id, delivery_id, projection_id \
         FROM attention_outcomes \
         WHERE principal = ? AND workspace = ? AND event_id = ?",
        params![principal, workspace, event_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<String>>(10)?,
            ))
        },
    );
    match row {
        Ok((
            outcome_id,
            surface,
            candidate_id,
            source_revision,
            outcome,
            reason,
            label_quality,
            decision_id,
            impression_id,
            delivery_id,
            projection_id,
        )) => {
            let outcome = AttentionOutcomeKind::from_str(&outcome)?;
            Ok(Some(ExistingAttentionOutcome {
                persisted: PersistedAttentionOutcome {
                    outcome_id,
                    outcome,
                    inserted: false,
                },
                surface,
                candidate_id,
                source_revision,
                outcome,
                reason,
                label_quality,
                decision_id,
                impression_id,
                delivery_id,
                projection_id,
            }))
        },
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn read_semantic_work_item(conn: &Connection, work_id: &str) -> Result<SemanticExtractionWorkItem> {
    let row = conn.query_row(
        "SELECT work_id, principal, workspace, surface, candidate_id,
                source_revision, source_revision_number, semantic_schema_version,
                extractor_contract, prompt_version, model, profile, status,
                attempts, next_retry_at, lease_owner, lease_expires_at,
                last_error_code, created_at, updated_at
         FROM attention_semantic_extraction_work WHERE work_id = ?",
        params![work_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, u32>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, Option<String>>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, String>(12)?,
                row.get::<_, u32>(13)?,
                row.get::<_, Option<i64>>(14)?,
                row.get::<_, Option<String>>(15)?,
                row.get::<_, Option<i64>>(16)?,
                row.get::<_, Option<String>>(17)?,
                row.get::<_, i64>(18)?,
                row.get::<_, i64>(19)?,
            ))
        },
    )?;
    Ok(SemanticExtractionWorkItem {
        work_id: row.0,
        principal: row.1,
        workspace: row.2,
        surface: AttentionSurface::from_str(&row.3)?,
        candidate_id: row.4,
        source_revision: row.5,
        source_revision_number: row.6,
        contract: SemanticExtractionContract {
            semantic_schema_version: row.7,
            extractor_contract: row.8,
            prompt_version: row.9,
            model: row.10,
            profile: row.11,
        },
        status: SemanticExtractionWorkStatus::from_str(&row.12)?,
        attempts: row.13,
        next_retry_at: row.14,
        lease_owner: row.15,
        lease_expires_at: row.16,
        last_error_code: row.17,
        created_at: row.18,
        updated_at: row.19,
    })
}

/// Persist only the bounded canonical reason vocabulary. The canonical outcome
/// retains the learning signal when an older or malformed client sends an
/// unknown free-form value; private comments never leak into this analytical
/// store.
fn canonical_reason(outcome: AttentionOutcomeKind, reason: Option<&str>) -> Option<String> {
    let reason = reason?.trim();
    let accepted = match (outcome, reason) {
        (AttentionOutcomeKind::Irrelevant, "spam" | "not_relevant")
        | (AttentionOutcomeKind::NotActionable, "wrong_classification" | "not_actionable")
        | (AttentionOutcomeKind::Obsolete, "already_handled")
        | (AttentionOutcomeKind::NotOwner, "delegated" | "wrong_owner")
        | (AttentionOutcomeKind::DuplicateOf, "duplicate")
        | (AttentionOutcomeKind::TimingNegative, "not_now" | "timing_negative") => reason,
        _ => return None,
    };
    Some(accepted.to_string())
}

fn valid_embedding(vector: &[f32]) -> bool {
    !vector.is_empty()
        && vector.iter().all(|value| value.is_finite())
        && vector.iter().any(|value| value.abs() > f32::EPSILON)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::config::AttentionRoutingMode;
    use crate::magician_v2::attention::learning::{
        ActionabilityTrainingManifest, AttentionDecisionFeatureContracts, AttentionLabelQuality,
        AttentionOutcomeAttribution, ACTIONABILITY_FEATURE_CONTRACT,
        ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT, ATTENTION_SEMANTIC_SCHEMA_VERSION,
    };

    fn store() -> AttentionLearningStore {
        let directory = tempfile::TempDir::new().expect("creating attention learning temp dir");
        let path = directory.keep();
        AttentionLearningStore::open(&path).expect("opening attention learning store")
    }

    /// Seed one decision with one item, an outcome attributed to it, and a
    /// terminal rank-recompute job for that outcome. `completed_at` is the
    /// only timestamp AFTER the cutoff: it is what pinned every old decision
    /// in the measured 17.7 GB store.
    fn seed_pinned_decision_chain(store: &AttentionLearningStore, cutoff_at: i64) {
        let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
        let old = cutoff_at - 1_000;
        let recent = cutoff_at + 1_000;
        conn.execute(
            "INSERT INTO attention_decisions (decision_id, schema_version, principal, workspace, surface, \
                decided_at, policy_mode, candidate_set_digest, eligible_item_count, selected_item_count, \
                returned_item_count, complete_universe_recorded, complete_cross_lane_universe, context_json, \
                policy_seed_identity, canary_assigned, baseline_route_summary_json, learned_route_summary_json, \
                latency_ms, decision_json, created_at) \
             VALUES ('decision-old', 1, 'p', 'w', 'follow_up', ?1, 'observe', 'digest', 1, 1, 1, 1, 1, '{}', \
                'seed', 0, '{}', '{}', 1, '{}', ?1)",
            params![old],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO attention_decision_items (decision_id, principal, workspace, candidate_id, source_family, \
                hard_eligible, baseline_route, learned_route, served_route, routing_mode, route_reason, route_applied, \
                canary_assigned, cluster_id, cluster_size, representative, baseline_rank, learned_rank, selected, \
                selection_probability, exploration, extraction_status, feature_contracts_json, item_json) \
             VALUES ('decision-old', 'p', 'w', 'candidate', 'mail', 1, 'follow_up', 'follow_up', 'follow_up', \
                'observe', 'baseline', 0, 0, 'cluster', 1, 1, 0, 0, 1, 1.0, 0, 'missing', '{}', '{}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO attention_outcomes (outcome_id, schema_version, event_id, principal, workspace, surface, \
                candidate_id, outcome, label_quality, occurred_at, created_at, decision_id) \
             VALUES ('outcome-old', 1, 'event', 'p', 'w', 'follow_up', 'candidate', 'acted', 'explicit', ?1, ?1, \
                'decision-old')",
            params![old],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO attention_rank_recompute_jobs (job_id, schema_version, outcome_id, principal, workspace, \
                origin_surface, canonical_candidate_id, raw_candidate_id, outcome, decision_id, status, attempts, \
                created_at, updated_at, completed_at) \
             VALUES ('job-old', 1, 'outcome-old', 'p', 'w', 'follow_up', 'candidate', 'candidate', 'acted', \
                'decision-old', 'stale', 1, ?1, ?2, ?2)",
            params![old, recent],
        )
        .unwrap();
    }

    #[test]
    fn retention_body_sweep_looks_up_decision_items_by_feature_digest_through_an_index() {
        // The feature-vector orphan sweep asks, per vector, whether any
        // decision item still binds its digest. Measured on the 17.7 GB
        // store: without an index that subquery was a full scan of 7.7 M
        // rows per vector, 492 s inside the writer transaction, to retire
        // nothing.
        let store = store();
        let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
        // Both correlated lookups: the bindings one scanned 96,625 rows per
        // vector on the same store (412 s) once the items one was fixed, and
        // the bindings table's foreign key on that column makes every vector
        // delete pay the same scan again.
        let mut statement = conn
            .prepare(
                "EXPLAIN QUERY PLAN SELECT COUNT(*) FROM attention_feature_vectors \
                 WHERE NOT EXISTS (SELECT 1 FROM attention_candidate_feature_bindings b \
                     WHERE b.content_digest = attention_feature_vectors.content_digest) \
                   AND NOT EXISTS (SELECT 1 FROM attention_decision_items i \
                     WHERE i.feature_snapshot_digest = attention_feature_vectors.content_digest)",
            )
            .expect("prepare the sweep predicate plan");
        let details = statement
            .query_map([], |row| row.get::<_, String>(3))
            .expect("inspect the sweep predicate plan")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("read the sweep predicate plan");
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("SEARCH i USING")
                    && detail.contains("feature_snapshot_digest")),
            "decision items must be looked up by feature digest, plan was {details:?}"
        );
        assert!(
            details.iter().any(
                |detail| detail.contains("SEARCH b USING") && detail.contains("content_digest")
            ),
            "feature bindings must be looked up by content digest, plan was {details:?}"
        );
        assert!(
            !details.iter().any(|detail| {
                let detail = detail.trim();
                detail.starts_with("SCAN i") || detail.starts_with("SCAN b")
            }),
            "the sweep must not scan a whole table per vector, plan was {details:?}"
        );
    }

    #[tokio::test]
    async fn stale_recompute_jobs_retire_with_their_outcome_so_the_slice_advances() {
        // The oldest surviving decision anchors the narrowed pass. Its
        // outcome is older than the cutoff, but the job that recomputed for
        // that outcome COMPLETED after it, and the job delete was keyed on
        // completion time — so the job pinned the outcome, the outcome pinned
        // the decision items, the items pinned the decision, and the anchor
        // never moved: 2,805 stale jobs held 289 decisions on the measured
        // store and every ten-minute pass retired nothing.
        let store = store();
        let cutoff_at = 100_000;
        seed_pinned_decision_chain(&store, cutoff_at);
        assert_eq!(
            store.oldest_retention_anchor("p", "w").await.unwrap(),
            Some(cutoff_at - 1_000)
        );

        let report = store
            .apply_scoped_retention("p", "w", cutoff_at, true)
            .await
            .unwrap();

        assert_eq!(report.affected_rows.get("rank_recompute_jobs"), Some(&1));
        assert_eq!(report.affected_rows.get("outcomes"), Some(&1));
        assert_eq!(report.affected_rows.get("decisions"), Some(&1));
        let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
        let remaining: (i64, i64, i64, i64) = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM attention_rank_recompute_jobs), \
                        (SELECT COUNT(*) FROM attention_outcomes), \
                        (SELECT COUNT(*) FROM attention_decision_items), \
                        (SELECT COUNT(*) FROM attention_decisions)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(remaining, (0, 0, 0, 0), "one pass retires the whole chain");
        drop(conn);
        assert_eq!(store.oldest_retention_anchor("p", "w").await.unwrap(), None);
    }

    #[test]
    fn auto_retention_gate_never_overlaps_and_backs_off_after_empty_passes() {
        let oversized = true;
        let healthy = false;
        // First ever pass: runs.
        assert_eq!(
            auto_retention_gate(None, 1_000, oversized),
            AutoRetentionGate::Run
        );
        // A pass still in flight blocks the next, however long ago it started.
        let running = AutoRetentionRun {
            started_at_ms: 0,
            finished_at_ms: None,
            empty_passes: 0,
        };
        assert_eq!(
            auto_retention_gate(
                Some(&running),
                0 + AUTO_SCOPED_RETENTION_INTERVAL_MS * 3,
                oversized
            ),
            AutoRetentionGate::Skip
        );
        // Oversized and productive: the next pass may start one oversized
        // interval after the previous one FINISHED, not after it started.
        let productive = AutoRetentionRun {
            started_at_ms: 595_000,
            finished_at_ms: Some(600_000),
            empty_passes: 0,
        };
        assert_eq!(
            auto_retention_gate(
                Some(&productive),
                600_000 + AUTO_SCOPED_RETENTION_OVERSIZED_INTERVAL_MS - 1,
                oversized
            ),
            AutoRetentionGate::Skip
        );
        assert_eq!(
            auto_retention_gate(
                Some(&productive),
                600_000 + AUTO_SCOPED_RETENTION_OVERSIZED_INTERVAL_MS,
                oversized
            ),
            AutoRetentionGate::Run
        );
        // A pass that held the writer for ten minutes is followed by at least
        // forty minutes of quiet, however productive it was.
        let long_hold = AutoRetentionRun {
            started_at_ms: 0,
            finished_at_ms: Some(600_000),
            empty_passes: 0,
        };
        assert_eq!(
            auto_retention_gate(
                Some(&long_hold),
                600_000 + 600_000 * AUTO_SCOPED_RETENTION_WRITER_SHARE_FACTOR - 1,
                oversized
            ),
            AutoRetentionGate::Skip
        );
        assert_eq!(
            auto_retention_gate(
                Some(&long_hold),
                600_000 + 600_000 * AUTO_SCOPED_RETENTION_WRITER_SHARE_FACTOR,
                oversized
            ),
            AutoRetentionGate::Run
        );
        // Oversized but the last three passes retired nothing: the cadence
        // doubles per empty pass, so nothing can hold the writer every
        // minute for no gain.
        let empty = AutoRetentionRun {
            started_at_ms: 599_000,
            finished_at_ms: Some(600_000),
            empty_passes: 3,
        };
        assert_eq!(
            auto_retention_gate(
                Some(&empty),
                600_000 + AUTO_SCOPED_RETENTION_OVERSIZED_INTERVAL_MS * 7,
                oversized
            ),
            AutoRetentionGate::Skip
        );
        assert_eq!(
            auto_retention_gate(
                Some(&empty),
                600_000 + AUTO_SCOPED_RETENTION_OVERSIZED_INTERVAL_MS * 8,
                oversized
            ),
            AutoRetentionGate::Run
        );
        // The backoff is capped at the ordinary daily cadence.
        let hopeless = AutoRetentionRun {
            started_at_ms: 599_000,
            finished_at_ms: Some(600_000),
            empty_passes: 40,
        };
        assert_eq!(
            auto_retention_gate(
                Some(&hopeless),
                600_000 + AUTO_SCOPED_RETENTION_INTERVAL_MS,
                oversized
            ),
            AutoRetentionGate::Run
        );
        // A healthy file keeps the ordinary cadence regardless of history.
        assert_eq!(
            auto_retention_gate(
                Some(&productive),
                600_000 + AUTO_SCOPED_RETENTION_INTERVAL_MS - 1,
                healthy
            ),
            AutoRetentionGate::Skip
        );
        assert_eq!(
            auto_retention_gate(
                Some(&productive),
                600_000 + AUTO_SCOPED_RETENTION_INTERVAL_MS,
                healthy
            ),
            AutoRetentionGate::Run
        );
    }

    #[test]
    fn a_bounded_writer_wait_gives_up_and_leaves_the_queue_serviceable() {
        // A page read must not sit behind maintenance for minutes. The
        // bounded acquisition abandons its FIFO ticket on timeout, and the
        // holder's release must skip that ticket or every later caller
        // waits on a ticket nobody holds.
        let store = store();
        let holder = store.conn.lock().unwrap_or_else(|p| p.into_inner());
        let started = Instant::now();
        let bounded = store.conn.lock_within(std::time::Duration::from_millis(50));
        assert!(
            bounded.is_err(),
            "the writer is held, so the bounded wait must give up"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "giving up must not take much longer than the bound"
        );
        drop(holder);
        let after = store
            .conn
            .lock_within(std::time::Duration::from_secs(5))
            .expect("the abandoned ticket must not wedge the queue");
        drop(after);
        let plain = store.conn.lock().unwrap_or_else(|p| p.into_inner());
        drop(plain);
    }

    #[test]
    fn rank_recompute_reconciliation_uses_decision_position_index() {
        let store = store();
        let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
        let index_exists = conn
            .query_row(
                "SELECT EXISTS( \
                     SELECT 1 FROM sqlite_master \
                     WHERE type = 'index' \
                       AND name = 'attention_delivery_page_items_decision_position_idx' \
                 )",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("query reconciliation index");
        assert_eq!(index_exists, 1, "bootstrap must migrate existing stores");

        let explain_sql = format!("EXPLAIN QUERY PLAN {RANK_RECOMPUTE_RECONCILIATION_SELECT_SQL}");
        let mut statement = conn
            .prepare(&explain_sql)
            .expect("prepare exact reconciliation query plan");
        let details = statement
            .query_map(params!["owner", "default", 10], |row| {
                row.get::<_, String>(3)
            })
            .expect("inspect exact reconciliation query plan")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("collect exact reconciliation query plan");
        assert!(
            details.iter().any(|detail| {
                detail.contains("SEARCH dpi")
                    && detail.contains("attention_delivery_page_items_decision_position_idx")
            }),
            "reconciliation must seek page items by decision and position: {details:#?}"
        );
    }

    #[test]
    fn rank_recompute_reconciliation_deadline_interrupts_and_clears() {
        let store = store();
        let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
        let progress_guard = install_attention_reconciliation_progress_guard_with_timeout(
            &conn,
            std::time::Duration::ZERO,
        );
        let error = conn
            .query_row(
                "WITH RECURSIVE counter(value) AS ( \
                     VALUES(0) \
                     UNION ALL \
                     SELECT value + 1 FROM counter WHERE value < 1000000 \
                 ) SELECT SUM(value) FROM counter",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect_err("expired reconciliation query must be interrupted");
        assert!(
            matches!(
                error,
                rusqlite::Error::SqliteFailure(ref code, _)
                    if code.code == rusqlite::ErrorCode::OperationInterrupted
            ),
            "unexpected expired reconciliation error: {error}"
        );
        drop(progress_guard);

        assert_eq!(
            conn.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
                .expect("progress callback cleared after reconciliation"),
            1
        );
    }

    #[test]
    fn read_pool_maintenance_fails_fast_and_restores_capacity() {
        let store = store();
        let guard = store
            .reads
            .begin_maintenance(store.database_path())
            .expect("enter maintenance");
        let error = match store.reads.acquire("maintenance-regression") {
            Ok(_) => panic!("new reads must not queue behind physical replacement"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("maintenance is in progress"));
        drop(guard);
        let restored = store
            .reads
            .acquire("maintenance-restored")
            .expect("read pool restored after maintenance");
        drop(restored);
        assert_eq!(
            store.connection_telemetry().read_capacity,
            ATTENTION_READ_POOL_SIZE
        );
    }

    #[cfg(unix)]
    #[test]
    fn physical_reclaim_requires_exclusive_cross_process_ownership() {
        let store = store();
        let peer = AttentionProcessLease::open(store.database_path()).expect("peer shared lease");
        let error = match store
            .process_lease
            .acquire_exclusive_with_timeout(std::time::Duration::from_millis(25))
        {
            Ok(_) => panic!("a live peer must prevent physical replacement"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("timed out draining other attention-learning processes"));
        drop(peer);
        let mut exclusive = store
            .process_lease
            .acquire_exclusive_with_timeout(std::time::Duration::from_secs(1))
            .expect("exclusive lease after peer exits");
        exclusive.downgrade().expect("restore shared lease");
    }

    #[tokio::test]
    async fn verified_scope_counter_tracks_mutation_and_scope_deletion_exactly() {
        let store = store();
        let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute(
            "INSERT INTO attention_impressions ( \
                impression_id, schema_version, event_id, principal, workspace, decision_id, \
                candidate_id, cluster_id, surface, rank, first_visible_at, \
                accumulated_visible_ms, visibility_rule_version, client_type, client_version, \
                viewport_class, verified, dedupe_count, last_recorded_at \
             ) VALUES ('i-1', 1, 'e-1', 'owner', 'default', 'd-1', 'c-1', 'g-1', \
                       'follow_up', 1, 1, 1, 'visible-v1', 'web', '1', 'desktop', 0, 0, 1)",
            [],
        )
        .unwrap();
        let count = || {
            conn.query_row(
                "SELECT COALESCE(verified_impression_total, 0) \
                 FROM attention_scope_health_counters \
                 WHERE principal = 'owner' AND workspace = 'default'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .unwrap()
            .unwrap_or(0)
        };
        assert_eq!(count(), 0);
        conn.execute(
            "UPDATE attention_impressions SET verified = 1 WHERE impression_id = 'i-1'",
            [],
        )
        .unwrap();
        assert_eq!(count(), 1);
        conn.execute(
            "UPDATE attention_impressions SET dedupe_count = dedupe_count + 1 \
             WHERE impression_id = 'i-1'",
            [],
        )
        .unwrap();
        assert_eq!(count(), 1, "non-verification updates must not double-count");
        conn.execute(
            "DELETE FROM attention_impressions WHERE impression_id = 'i-1'",
            [],
        )
        .unwrap();
        assert_eq!(count(), 0);
        conn.execute(
            "INSERT INTO attention_impressions ( \
                impression_id, schema_version, event_id, principal, workspace, decision_id, \
                candidate_id, cluster_id, surface, rank, first_visible_at, \
                accumulated_visible_ms, visibility_rule_version, client_type, client_version, \
                viewport_class, verified, dedupe_count, last_recorded_at \
             ) VALUES ('i-2', 1, 'e-2', 'owner', 'default', 'd-2', 'c-2', 'g-2', \
                       'follow_up', 1, 1, 1, 'visible-v1', 'web', '1', 'desktop', 1, 0, 1)",
            [],
        )
        .unwrap();
        assert_eq!(count(), 1);
        drop(conn);

        let deletion = store.delete_scope("owner", "default", true).await.unwrap();
        assert_eq!(
            deletion
                .affected_rows
                .get("attention_scope_health_counters"),
            Some(&1)
        );
        let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM attention_scope_health_counters \
                 WHERE principal = 'owner' AND workspace = 'default'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0, "scope deletion must remove counter identity");
    }

    #[tokio::test]
    async fn optimize_and_reclaim_preserve_rows_and_restore_live_connections() {
        let directory = tempfile::TempDir::new().unwrap();
        let store = AttentionLearningStore::open(directory.path()).unwrap();
        {
            let mut conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            let tx = conn.transaction().unwrap();
            for index in 0..256 {
                let migration_id = format!("reclaim-fixture-{index:04}-{}", "x".repeat(4 * 1024));
                tx.execute(
                    "INSERT INTO attention_schema_migrations(migration_id, applied_at) VALUES (?, 1)",
                    params![migration_id],
                )
                .unwrap();
            }
            tx.commit().unwrap();
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                .unwrap();
            conn.execute(
                "DELETE FROM attention_schema_migrations WHERE migration_id LIKE 'reclaim-fixture-%'",
                [],
            )
            .unwrap();
            conn.execute_batch(
                "CREATE TABLE attention_reclaim_autoincrement_fixture ( \
                     id INTEGER PRIMARY KEY AUTOINCREMENT, value TEXT NOT NULL \
                 ); \
                 INSERT INTO attention_reclaim_autoincrement_fixture(value) VALUES ('one'); \
                 DELETE FROM attention_reclaim_autoincrement_fixture;",
            )
            .unwrap();
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                .unwrap();
        }
        let optimized = store.optimize_database().await.unwrap();
        assert!(optimized.bytes_before > 0);
        assert!(optimized.bytes_after > 0);
        assert_eq!(optimized.analyzed_tables.len(), 3);

        let reclaimed = store.reclaim_database_space().await.unwrap();
        assert_eq!(reclaimed.integrity_check, "ok");
        assert!(reclaimed.bytes_after < reclaimed.bytes_before);
        assert_eq!(
            reclaimed.bytes_reclaimed,
            reclaimed.bytes_before - reclaimed.bytes_after
        );
        let scopes = store.list_scopes().await.expect("read pool remains usable");
        assert!(scopes.is_empty());
        let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
        let migration_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM attention_schema_migrations \
                 WHERE migration_id = ?",
                params![SCOPE_HEALTH_COUNTER_BACKFILL_MIGRATION],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(migration_count, 1);
        conn.execute(
            "INSERT INTO attention_reclaim_autoincrement_fixture(value) VALUES ('two')",
            [],
        )
        .unwrap();
        let next_id: i64 = conn
            .query_row(
                "SELECT id FROM attention_reclaim_autoincrement_fixture WHERE value = 'two'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(next_id, 2, "reclaim must preserve SQLite sequence state");
    }

    #[test]
    fn decision_item_hydration_plan_is_decision_local() {
        let store = store();
        let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
        let detail: String = conn
            .query_row(
                "EXPLAIN QUERY PLAN SELECT item_json FROM attention_decision_items \
                 WHERE decision_id = 'decision-1' \
                 ORDER BY baseline_rank ASC, candidate_id ASC",
                [],
                |row| row.get(3),
            )
            .unwrap();
        assert!(
            detail.contains("attention_decision_items_decision_rank_idx")
                || detail.contains("sqlite_autoindex_attention_decision_items_1"),
            "decision hydration must not scan retained scope history: {detail}"
        );
    }

    #[test]
    fn stored_json_node_preflight_is_exact_and_ignores_wide_string_contents() {
        let exact = r#"{"rows":[null,false]}"#;
        let parsed: serde_json::Value =
            parse_bounded_stored_json_with_limits(exact, "exact attention fixture", 1_024, 4)
                .expect("root, array, and two scalar nodes fit exactly");
        assert_eq!(parsed["rows"][1], false);
        let over = parse_bounded_stored_json_with_limits::<serde_json::Value>(
            exact,
            "over-wide attention fixture",
            1_024,
            3,
        )
        .expect_err("one node over the limit fails before Serde allocation");
        assert!(over.to_string().contains("node admission limit"));

        let wide_string = format!(r#"{{"value":"{}"}}"#, "[null]".repeat(10_000));
        let parsed: serde_json::Value = parse_bounded_stored_json_with_limits(
            &wide_string,
            "wide attention string fixture",
            wide_string.len(),
            2,
        )
        .expect("string contents are one value node");
        assert_eq!(parsed["value"].as_str().map(str::len), Some(60_000));
    }

    #[test]
    fn delivery_generation_column_migration_serializes_cross_process_openers() {
        let directory = tempfile::TempDir::new().unwrap();
        let database = directory.path().join("delivery-migration.db");
        {
            let conn = Connection::open(&database).unwrap();
            configure_attention_connection(&conn, false).unwrap();
            conn.execute_batch(
                "CREATE TABLE attention_delivery_decisions ( \
                    decision_id TEXT NOT NULL PRIMARY KEY \
                 )",
            )
            .unwrap();
        }

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut openers = Vec::new();
        for _ in 0..2 {
            let database = database.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            openers.push(std::thread::spawn(move || -> Result<()> {
                let conn = Connection::open(database)?;
                configure_attention_connection(&conn, false)?;
                barrier.wait();
                ensure_delivery_source_generation_token(&conn)
            }));
        }
        barrier.wait();
        for opener in openers {
            opener.join().expect("migration opener panicked").unwrap();
        }

        let conn = Connection::open(database).unwrap();
        let mut statement = conn
            .prepare("PRAGMA table_info(attention_delivery_decisions)")
            .unwrap();
        let column_count = statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap()
            .into_iter()
            .filter(|column| column == "source_generation_token")
            .count();
        assert_eq!(column_count, 1);
    }

    fn canonical_projection_fixture(projection_id: &str, item_count: usize) -> String {
        let items = (0..item_count)
            .map(|index| {
                serde_json::json!({
                    "canonical_id": format!("follow_up:item-{index}"),
                    "source_revision": format!("revision-{index}"),
                    "origin_lane": "follow_up",
                    "served_lane": "follow_up",
                    "learned_lane": "follow_up",
                    "route_reason": "baseline_route_retained",
                    "route_applied": false,
                    "origin": {"kind": "follow_up", "annotation_id": format!("item-{index}"), "provider": "fixture", "account_alias": "default", "thread_id": format!("thread-{index}")},
                    "group": {"cluster_id": format!("cluster-{index}"), "representative_id": format!("follow_up:item-{index}"), "member_ids": [format!("follow_up:item-{index}")], "member_count": 1},
                    "actions": [],
                    "payload": {"kind": "follow_up", "annotation_id": format!("item-{index}"), "subject": "subject", "sender": null, "summary": "summary", "label": null, "reason": null, "received_at": 1, "open_url": null}
                })
            })
            .collect::<Vec<_>>();
        let ranks = (0..item_count)
            .map(|index| {
                serde_json::json!({
                    "candidate_id": format!("follow_up:item-{index}"),
                    "baseline_rank": index + 1,
                    "learned_rank": index + 1
                })
            })
            .collect::<Vec<_>>();
        let decision_items = (0..item_count)
            .map(|index| {
                serde_json::json!({
                    "candidate_id": format!("follow_up:item-{index}"),
                    "served_rank": index + 1
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "schema_version": 1,
            "status": "succeeded",
            "projection_id": projection_id,
            "universe_digest": format!("universe-{projection_id}"),
            "created_at": 10,
            "policy": {"mode": "baseline"},
            "integrity": {"materialized_total": item_count},
            "cross_lane_reconciliation": {},
            "duplicate_aliases": [],
            "diagnostics": {"rank_generation": 1, "ranks": ranks, "decision_items": decision_items},
            "lanes": {"follow_up": items, "worth_a_look": [], "non_surfaced": []}
        })
        .to_string()
    }

    fn request(event_id: &str, outcome: AttentionOutcomeKind) -> RecordAttentionOutcome {
        RecordAttentionOutcome {
            event_id: event_id.to_string(),
            candidate: SemanticAttentionCandidate {
                candidate_id: "candidate-1".to_string(),
                source_revision: Some("revision-1".to_string()),
                semantic_text: "safe summary".to_string(),
                existing_embedding: None,
                actionability_features: None,
                grouping_features: None,
            },
            outcome,
            reason: None,
            label_quality: AttentionLabelQuality::Strong,
            occurred_at: 1_000,
            attribution: None,
        }
    }

    /// The client already supplies which decision surfaced the card being acted
    /// on. Before this was stored, the link was dropped on write and later
    /// reconstructed by joining through `attention_bandit_updates` — which is
    /// empty until the bandit runs, so 91% of rank-recompute jobs carried no
    /// decision and every one of them went stale.
    #[tokio::test]
    async fn recording_an_outcome_persists_the_supplied_attribution() {
        let store = store();
        let mut with_attribution = request("event-attributed", AttentionOutcomeKind::Useful);
        with_attribution.attribution = Some(AttentionOutcomeAttribution {
            decision_id: "decision-7".to_string(),
            candidate_id: "candidate-1".to_string(),
            source_revision: Some("revision-1".to_string()),
            impression_id: Some("impression-9".to_string()),
            delivery_id: None,
        });
        store
            .record_outcome(
                "p",
                "w",
                AttentionSurface::FollowUp,
                &with_attribution,
                None,
            )
            .await
            .expect("recording an attributed outcome");

        let (decision_id, impression_id): (Option<String>, Option<String>) = {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.query_row(
                "SELECT decision_id, impression_id FROM attention_outcomes WHERE event_id = ?",
                params!["event-attributed"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("reading the attributed outcome")
        };
        assert_eq!(decision_id.as_deref(), Some("decision-7"));
        assert_eq!(impression_id.as_deref(), Some("impression-9"));
    }

    /// Capture must not depend on attribution: an outcome with none is still
    /// recorded in full, it simply cannot be traced back to what was shown.
    #[tokio::test]
    async fn an_outcome_without_attribution_is_still_recorded() {
        let store = store();
        store
            .record_outcome(
                "p",
                "w",
                AttentionSurface::FollowUp,
                &request("event-bare", AttentionOutcomeKind::Useful),
                None,
            )
            .await
            .expect("recording an unattributed outcome");

        let decision_id: Option<String> = {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.query_row(
                "SELECT decision_id FROM attention_outcomes WHERE event_id = ?",
                params!["event-bare"],
                |row| row.get(0),
            )
            .expect("reading the unattributed outcome")
        };
        assert!(decision_id.is_none());
    }

    #[tokio::test]
    async fn replay_can_monotonically_enrich_missing_outcome_attribution() {
        let store = store();
        let bare = request("event-enriched", AttentionOutcomeKind::Useful);
        store
            .record_outcome("p", "w", AttentionSurface::FollowUp, &bare, None)
            .await
            .expect("record bare outcome");
        let mut enriched = bare.clone();
        enriched.attribution = Some(AttentionOutcomeAttribution {
            decision_id: "delivery-decision-1".to_string(),
            candidate_id: "candidate-1".to_string(),
            source_revision: Some("revision-1".to_string()),
            impression_id: Some("impression-1".to_string()),
            delivery_id: Some("delivery-1".to_string()),
        });
        let replay = store
            .record_outcome("p", "w", AttentionSurface::FollowUp, &enriched, None)
            .await
            .expect("enrich replay attribution");
        assert!(!replay.inserted);

        let stored: (Option<String>, Option<String>, Option<String>) = {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.query_row(
                "SELECT decision_id, impression_id, delivery_id \
                 FROM attention_outcomes WHERE event_id = ?",
                params!["event-enriched"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("read enriched attribution")
        };
        assert_eq!(stored.0.as_deref(), Some("delivery-decision-1"));
        assert_eq!(stored.1.as_deref(), Some("impression-1"));
        assert_eq!(stored.2.as_deref(), Some("delivery-1"));
    }

    #[tokio::test]
    async fn replay_can_enrich_delivery_then_impression_without_rebinding_either() {
        let store = store();
        let mut original = request("event-staged-attribution", AttentionOutcomeKind::Useful);
        original.attribution = Some(AttentionOutcomeAttribution {
            decision_id: "decision-a".to_string(),
            candidate_id: "candidate-1".to_string(),
            source_revision: Some("revision-1".to_string()),
            impression_id: None,
            delivery_id: None,
        });
        store
            .record_outcome("p", "w", AttentionSurface::FollowUp, &original, None)
            .await
            .expect("record decision-only attribution");

        let mut delivered = original.clone();
        delivered.attribution.as_mut().unwrap().delivery_id = Some("delivery-a".to_string());
        store
            .record_outcome("p", "w", AttentionSurface::FollowUp, &delivered, None)
            .await
            .expect("enrich delivery attribution");

        let mut impressed = delivered.clone();
        impressed.attribution.as_mut().unwrap().impression_id = Some("impression-a".to_string());
        store
            .record_outcome("p", "w", AttentionSurface::FollowUp, &impressed, None)
            .await
            .expect("enrich impression attribution");

        let mut rebound = impressed.clone();
        rebound.attribution.as_mut().unwrap().impression_id = Some("impression-b".to_string());
        let error = store
            .record_outcome("p", "w", AttentionSurface::FollowUp, &rebound, None)
            .await
            .expect_err("a durable impression must not be rebound on replay");
        assert!(error.to_string().contains("different payload"));

        let stored: (Option<String>, Option<String>, Option<String>) = {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.query_row(
                "SELECT decision_id, impression_id, delivery_id \
                 FROM attention_outcomes WHERE event_id = ?",
                params!["event-staged-attribution"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("read staged attribution")
        };
        assert_eq!(stored.0.as_deref(), Some("decision-a"));
        assert_eq!(stored.1.as_deref(), Some("impression-a"));
        assert_eq!(stored.2.as_deref(), Some("delivery-a"));
    }

    #[tokio::test]
    async fn replay_rejects_a_different_outcome_attribution() {
        let store = store();
        let mut original = request("event-attribution-collision", AttentionOutcomeKind::Useful);
        original.attribution = Some(AttentionOutcomeAttribution {
            decision_id: "decision-a".to_string(),
            candidate_id: "candidate-1".to_string(),
            source_revision: Some("revision-1".to_string()),
            impression_id: Some("impression-a".to_string()),
            delivery_id: None,
        });
        store
            .record_outcome("p", "w", AttentionSurface::FollowUp, &original, None)
            .await
            .expect("record original attribution");
        let mut collision = original.clone();
        collision.attribution.as_mut().unwrap().decision_id = "decision-b".to_string();
        let error = store
            .record_outcome("p", "w", AttentionSurface::FollowUp, &collision, None)
            .await
            .expect_err("different decision must be an idempotency collision");
        assert!(error.to_string().contains("different payload"));
    }

    #[tokio::test]
    async fn replay_rejects_a_different_delivery_for_the_same_decision() {
        let store = store();
        let mut original = request("event-delivery-collision", AttentionOutcomeKind::Useful);
        original.attribution = Some(AttentionOutcomeAttribution {
            decision_id: "decision-a".to_string(),
            candidate_id: "candidate-1".to_string(),
            source_revision: Some("revision-1".to_string()),
            impression_id: None,
            delivery_id: Some("delivery-a".to_string()),
        });
        store
            .record_outcome("p", "w", AttentionSurface::FollowUp, &original, None)
            .await
            .expect("record original delivery attribution");

        let mut collision = original.clone();
        collision.attribution.as_mut().unwrap().delivery_id = Some("delivery-b".to_string());
        let error = store
            .record_outcome("p", "w", AttentionSurface::FollowUp, &collision, None)
            .await
            .expect_err("different delivery must be an idempotency collision");
        assert!(error.to_string().contains("different payload"));
    }

    #[tokio::test]
    async fn delivery_decision_resolves_its_exact_canonical_projection() {
        let store = store();
        {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.execute(
                "INSERT INTO attention_canonical_projections \
                    (projection_id, schema_version, principal, workspace, universe_digest, \
                     policy_identity, projection_json, created_at, updated_at) \
                 VALUES (?, 1, ?, ?, ?, ?, ?, 10, 10)",
                params![
                    "projection-exact",
                    "p",
                    "w",
                    "universe-exact",
                    "policy-exact",
                    "{\"projection_id\":\"projection-exact\"}",
                ],
            )
            .expect("insert projection");
            conn.execute(
                "INSERT INTO attention_delivery_decisions \
                    (decision_id, schema_version, principal, workspace, lane, projection_id, \
                     universe_digest, policy_snapshot_id, policy_model_version, \
                     policy_snapshot_json, posterior_version, seed_identity, universe_size, \
                     page_size, status, fallback_reason, health_json, min_visible_ms, \
                     visibility_rule_version, context_json, decision_json, created_at, expires_at) \
                 VALUES (?, 1, ?, ?, 'follow_up', ?, ?, NULL, NULL, '{}', 0, 'seed', 1, 1, \
                         'active', NULL, '{}', 1, 'v1', '{}', '{}', 11, 1000)",
                params![
                    "delivery-decision-exact",
                    "p",
                    "w",
                    "projection-exact",
                    "universe-exact"
                ],
            )
            .expect("insert delivery decision");
        }

        let projection = store
            .get_decision_projection_json("p", "w", "delivery-decision-exact")
            .await
            .expect("resolve projection")
            .expect("projection exists");
        assert_eq!(projection, "{\"projection_id\":\"projection-exact\"}");
    }

    #[tokio::test]
    async fn canonical_decision_prefers_its_exact_projection_over_newer_digest_match() {
        let store = store();
        let exact_id = "canonical-decision-exact";
        let exact_projection = canonical_projection_fixture(exact_id, 1);
        store
            .persist_canonical_projection_json(
                "p",
                "w",
                "shared-universe",
                "policy-a",
                exact_id,
                8,
                &exact_projection,
            )
            .await
            .expect("persist exact projection");
        let newer_projection = canonical_projection_fixture("newer-digest-match", 1);
        store
            .persist_canonical_projection_json(
                "p",
                "w",
                "shared-universe",
                "policy-b",
                "newer-digest-match",
                9,
                &newer_projection,
            )
            .await
            .expect("persist ambiguous digest match");
        let mut evaluation = routing_evaluation();
        evaluation.decision_id = exact_id.to_string();
        for item in &mut evaluation.items {
            item.decision_id = exact_id.to_string();
        }
        store
            .record_decision(
                "p",
                "w",
                "shared-universe",
                AttentionDecisionContext {
                    queue_size: 1,
                    ..Default::default()
                },
                4,
                1,
                &evaluation,
            )
            .await
            .expect("record canonical decision");

        let projection = store
            .get_decision_projection_json("p", "w", exact_id)
            .await
            .expect("resolve canonical projection")
            .expect("projection exists");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&projection).unwrap(),
            serde_json::from_str::<serde_json::Value>(&exact_projection).unwrap()
        );
    }

    #[tokio::test]
    async fn normalized_projection_stores_one_body_per_item_not_three_copies() {
        let store = store();
        let projection = canonical_projection_fixture("projection-500", 500);
        store
            .persist_canonical_projection_json(
                "owner",
                "default",
                "universe-500",
                "policy-1",
                "projection-500",
                10,
                &projection,
            )
            .await
            .expect("persist normalized projection");
        let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
        let bodies: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM attention_canonical_item_revisions",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let members: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM attention_canonical_projection_members",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let legacy_bytes: i64 = conn
            .query_row("SELECT length(projection_json) FROM attention_canonical_projections WHERE projection_id = 'projection-500'", [], |row| row.get(0))
            .unwrap();
        let embedded_diagnostic_bytes: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(length(rank_json) + length(decision_item_json)), 0) \
                 FROM attention_canonical_projection_members",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let diagnostic_bodies: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM attention_canonical_diagnostic_revisions",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let schema_version: i64 = conn
            .query_row(
                "SELECT schema_version FROM attention_canonical_projections \
                 WHERE projection_id = 'projection-500'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!((bodies, members, legacy_bytes), (500, 500, 0));
        assert_eq!(schema_version, CURRENT_CANONICAL_PROJECTION_SCHEMA_VERSION);
        assert_eq!(embedded_diagnostic_bytes, 0);
        assert_eq!(diagnostic_bodies, 1_000);
    }

    #[tokio::test]
    async fn normalized_projection_releases_read_permit_before_json_materialization() {
        let store = store();
        let projection = canonical_projection_fixture("projection-read-split", 20);
        store
            .persist_canonical_projection_json(
                "owner",
                "default",
                "universe-read-split",
                "policy-1",
                "projection-read-split",
                10,
                &projection,
            )
            .await
            .unwrap();

        let read = store
            .reads
            .acquire("projection_read_split_fixture")
            .unwrap();
        let (schema_version, legacy_json) = read
            .query_row(
                "SELECT schema_version, projection_json FROM attention_canonical_projections \
                 WHERE projection_id = 'projection-read-split'",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .unwrap();
        let staged = load_canonical_projection_materialization(
            &read,
            "projection-read-split",
            schema_version,
            legacy_json,
        )
        .unwrap();
        drop(read);
        assert_eq!(store.connection_telemetry().read_in_use, 0);

        let materialized = materialize_canonical_projection_json(staged).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&materialized).unwrap(),
            serde_json::from_str::<serde_json::Value>(&projection).unwrap()
        );
    }

    #[tokio::test]
    async fn normalized_projection_preserves_empty_diagnostic_arrays_exactly() {
        let store = store();
        let projection = canonical_projection_fixture("projection-empty-diagnostics", 0);
        let persisted = store
            .persist_canonical_projection_json(
                "p",
                "w",
                "empty-diagnostics-u",
                "policy",
                "projection-empty-diagnostics",
                10,
                &projection,
            )
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&persisted).unwrap(),
            serde_json::from_str::<serde_json::Value>(&projection).unwrap(),
            "normalization must not erase present-but-empty compatibility fields"
        );
    }

    #[test]
    fn projection_json_depth_is_rejected_before_serde_materialization() {
        let deep = format!(
            "{}0{}",
            "[".repeat(MAX_RETAINED_JSON_DEPTH + 1),
            "]".repeat(MAX_RETAINED_JSON_DEPTH + 1)
        );
        let error = prepare_normalized_canonical_projection(&deep).unwrap_err();
        assert!(error.to_string().contains("maximum JSON nesting depth"));

        let brackets_inside_a_string = serde_json::to_string(&format!(
            "{}{}",
            "[".repeat(MAX_RETAINED_JSON_DEPTH + 10),
            "]".repeat(MAX_RETAINED_JSON_DEPTH + 10)
        ))
        .unwrap();
        ensure_bounded_json_nesting(&brackets_inside_a_string, "string fixture").unwrap();
    }

    #[test]
    fn normalized_projection_preparation_consumes_lane_and_diagnostic_values() {
        let source = include_str!("store.rs");
        let start = source
            .find("fn prepare_normalized_canonical_projection(")
            .expect("preparation start marker");
        let end = source[start..]
            .find("fn insert_normalized_canonical_projection(")
            .map(|offset| start + offset)
            .expect("preparation end marker");
        let implementation = &source[start..end];
        assert!(!implementation.contains("as_array().cloned()"));
        assert!(!implementation.contains("item.clone()"));
        assert!(implementation.contains("items.into_iter()"));
    }

    #[tokio::test]
    async fn content_address_reuse_fails_closed_on_a_corrupt_item_body() {
        let store = store();
        let first = canonical_projection_fixture("projection-corrupt-a", 1);
        store
            .persist_canonical_projection_json(
                "p",
                "w",
                "corrupt-u-a",
                "policy",
                "projection-corrupt-a",
                10,
                &first,
            )
            .await
            .unwrap();
        {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.execute(
                "UPDATE attention_canonical_item_revisions SET item_json = '{}'",
                [],
            )
            .unwrap();
        }
        let mut second: serde_json::Value = serde_json::from_str(&first).unwrap();
        second["projection_id"] = serde_json::json!("projection-corrupt-b");
        second["universe_digest"] = serde_json::json!("corrupt-u-b");
        let error = store
            .persist_canonical_projection_json(
                "p",
                "w",
                "corrupt-u-b",
                "policy",
                "projection-corrupt-b",
                11,
                &second.to_string(),
            )
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("canonical projection item digest collision"));
    }

    #[tokio::test]
    async fn unchanged_projection_reuses_bodies_and_one_changed_item_adds_one_body() {
        let store = store();
        let first = canonical_projection_fixture("projection-reuse-a", 20);
        store
            .persist_canonical_projection_json(
                "p",
                "w",
                "u-a",
                "policy",
                "projection-reuse-a",
                10,
                &first,
            )
            .await
            .unwrap();
        store
            .persist_canonical_projection_json(
                "p",
                "w",
                "u-a",
                "policy",
                "projection-reuse-a",
                10,
                &first,
            )
            .await
            .unwrap();
        let mut changed: serde_json::Value = serde_json::from_str(&first).unwrap();
        changed["projection_id"] = serde_json::json!("projection-reuse-b");
        changed["universe_digest"] = serde_json::json!("u-b");
        changed["lanes"]["follow_up"][7]["payload"]["summary"] =
            serde_json::json!("changed summary");
        store
            .persist_canonical_projection_json(
                "p",
                "w",
                "u-b",
                "policy",
                "projection-reuse-b",
                11,
                &changed.to_string(),
            )
            .await
            .unwrap();
        let mut binding_only: serde_json::Value = serde_json::from_str(&first).unwrap();
        binding_only["projection_id"] = serde_json::json!("projection-reuse-c");
        binding_only["universe_digest"] = serde_json::json!("u-c");
        binding_only["lanes"]["follow_up"][0]["route_reason"] =
            serde_json::json!("changed_projection_binding_only");
        store
            .persist_canonical_projection_json(
                "p",
                "w",
                "u-c",
                "policy",
                "projection-reuse-c",
                12,
                &binding_only.to_string(),
            )
            .await
            .unwrap();
        let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
        let bodies: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM attention_canonical_item_revisions",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(bodies, 21);
        let diagnostic_bodies: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM attention_canonical_diagnostic_revisions",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(diagnostic_bodies, 40);
    }

    #[tokio::test]
    async fn legacy_projection_migration_is_resumable_idempotent_and_compatible() {
        let store = store();
        let legacy = canonical_projection_fixture("legacy-projection", 3);
        {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.execute(
                "INSERT INTO attention_canonical_projections (projection_id, schema_version, principal, workspace, universe_digest, policy_identity, projection_json, created_at, updated_at) VALUES (?, 1, 'p', 'w', 'legacy-u', 'legacy-policy', ?, 10, 10)",
                params!["legacy-projection", legacy],
            )
            .unwrap();
        }
        assert_eq!(
            store.migrate_legacy_canonical_projections(1).await.unwrap(),
            1
        );
        assert_eq!(
            store.migrate_legacy_canonical_projections(1).await.unwrap(),
            0
        );
        let restored = store
            .get_canonical_projection_json("p", "w", "legacy-u", "legacy-policy")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&restored).unwrap(),
            serde_json::from_str::<serde_json::Value>(&legacy).unwrap()
        );
    }

    #[tokio::test]
    async fn corrupt_legacy_projection_is_quarantined_without_starving_later_rows() {
        let directory = tempfile::TempDir::new().unwrap();
        let valid = canonical_projection_fixture("legacy-valid-after-corrupt", 2);
        {
            let store = AttentionLearningStore::open(directory.path()).unwrap();
            {
                let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
                conn.execute(
                    "INSERT INTO attention_canonical_projections ( \
                        projection_id, schema_version, principal, workspace, universe_digest, \
                        policy_identity, projection_json, created_at, updated_at \
                     ) VALUES ('legacy-corrupt-first', 1, 'p', 'w', 'corrupt-u', \
                               'corrupt-policy', '{}', 10, 10)",
                    [],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO attention_canonical_projections ( \
                        projection_id, schema_version, principal, workspace, universe_digest, \
                        policy_identity, projection_json, created_at, updated_at \
                     ) VALUES ('legacy-valid-after-corrupt', 1, 'p', 'w', 'valid-u', \
                               'valid-policy', ?, 11, 11)",
                    params![valid],
                )
                .unwrap();
            }

            assert_eq!(
                store.migrate_legacy_canonical_projections(1).await.unwrap(),
                1,
                "quarantine-only progress must keep a batch drainer running"
            );
            assert_eq!(
                store.migrate_legacy_canonical_projections(1).await.unwrap(),
                1,
                "the valid successor must migrate in the next bounded batch"
            );
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            let states: (i64, i64, i64, String) = conn
                .query_row(
                    "SELECT \
                         (SELECT schema_version FROM attention_canonical_projections \
                          WHERE projection_id = 'legacy-corrupt-first'), \
                         (SELECT schema_version FROM attention_canonical_projections \
                          WHERE projection_id = 'legacy-valid-after-corrupt'), \
                         COUNT(*), MIN(error_code) \
                     FROM attention_legacy_migration_quarantine \
                     WHERE migration_kind = ? AND source_key = 'legacy-corrupt-first'",
                    params![LEGACY_CANONICAL_MIGRATION_KIND],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .unwrap();
            assert_eq!(
                states,
                (
                    1,
                    CURRENT_CANONICAL_PROJECTION_SCHEMA_VERSION,
                    1,
                    "invalid_contract".to_string()
                )
            );
        }

        let reopened = AttentionLearningStore::open(directory.path()).unwrap();
        assert_eq!(
            reopened
                .migrate_legacy_canonical_projections(2)
                .await
                .unwrap(),
            0,
            "durable quarantine and normalized successor must remain idempotent after restart"
        );
        let conn = reopened.conn.lock().unwrap_or_else(|p| p.into_inner());
        let quarantine_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM attention_legacy_migration_quarantine \
                 WHERE migration_kind = ? AND source_key = 'legacy-corrupt-first'",
                params![LEGACY_CANONICAL_MIGRATION_KIND],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(quarantine_count, 1);
        drop(conn);
        reopened.delete_scope("p", "w", true).await.unwrap();
        let conn = reopened.conn.lock().unwrap_or_else(|p| p.into_inner());
        let scoped_residue: i64 = conn
            .query_row(
                "SELECT \
                    (SELECT COUNT(*) FROM attention_legacy_migration_quarantine \
                     WHERE principal = 'p' AND workspace = 'w') + \
                    (SELECT COUNT(*) FROM attention_canonical_projections \
                     WHERE principal = 'p' AND workspace = 'w')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(scoped_residue, 0);
    }

    #[tokio::test]
    async fn repaired_quarantined_projection_reenters_migration_by_payload_fingerprint() {
        let store = store();
        {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.execute(
                "INSERT INTO attention_canonical_projections ( \
                    projection_id, schema_version, principal, workspace, universe_digest, \
                    policy_identity, projection_json, created_at, updated_at \
                 ) VALUES ('legacy-repaired-projection', 1, 'p', 'w', 'repair-u', \
                           'repair-policy', '{}', 10, 10)",
                [],
            )
            .unwrap();
        }
        assert_eq!(
            store.migrate_legacy_canonical_projections(1).await.unwrap(),
            1
        );
        let repaired = canonical_projection_fixture("legacy-repaired-projection", 2);
        {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.execute(
                "UPDATE attention_canonical_projections SET projection_json = ?, updated_at = 11 \
                 WHERE projection_id = 'legacy-repaired-projection'",
                params![repaired],
            )
            .unwrap();
        }
        assert_eq!(
            store.migrate_legacy_canonical_projections(1).await.unwrap(),
            1,
            "a changed payload fingerprint must invalidate the durable quarantine decision"
        );
        let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
        let state: (i64, i64) = conn
            .query_row(
                "SELECT schema_version, length(projection_json) \
                 FROM attention_canonical_projections \
                 WHERE projection_id = 'legacy-repaired-projection'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, (CURRENT_CANONICAL_PROJECTION_SCHEMA_VERSION, 0));
    }

    #[tokio::test]
    async fn legacy_projection_migration_rolls_back_every_normalized_body_before_anchor_switch() {
        let store = store();
        let legacy = canonical_projection_fixture("legacy-rollback", 2);
        {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.execute(
                "INSERT INTO attention_canonical_projections (projection_id, schema_version, principal, workspace, universe_digest, policy_identity, projection_json, created_at, updated_at) VALUES ('legacy-rollback', 1, 'p', 'w', 'rollback-u', 'policy', ?, 10, 10)",
                params![legacy],
            )
            .unwrap();
        }
        let prepared = prepare_normalized_canonical_projection(&legacy).unwrap();
        {
            let mut conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            insert_normalized_canonical_projection(&tx, "legacy-rollback", 10, &prepared).unwrap();
            tx.rollback().unwrap();
        }
        {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            let anchor: (i64, String) = conn
                .query_row(
                    "SELECT schema_version, projection_json FROM attention_canonical_projections WHERE projection_id = 'legacy-rollback'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            let manifest_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM attention_canonical_projection_manifests WHERE projection_id = 'legacy-rollback'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(anchor, (1, legacy.clone()));
            assert_eq!(manifest_count, 0);
        }
        assert_eq!(
            store.migrate_legacy_canonical_projections(1).await.unwrap(),
            1
        );
        let restored = store
            .get_canonical_projection_json("p", "w", "rollback-u", "policy")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&restored).unwrap(),
            serde_json::from_str::<serde_json::Value>(&legacy).unwrap()
        );
    }

    #[tokio::test]
    async fn historical_decision_resolves_its_exact_normalized_projection_after_restart() {
        let directory = tempfile::TempDir::new().unwrap();
        let projection_id = "projection-restart";
        let projection = canonical_projection_fixture(projection_id, 1);
        {
            let store = AttentionLearningStore::open(directory.path()).unwrap();
            store
                .persist_canonical_projection_json(
                    "p",
                    "w",
                    "restart-u",
                    "restart-policy",
                    projection_id,
                    10,
                    &projection,
                )
                .await
                .unwrap();
            let mut evaluation = routing_evaluation();
            evaluation.decision_id = projection_id.to_string();
            for item in &mut evaluation.items {
                item.decision_id = projection_id.to_string();
            }
            store
                .record_decision(
                    "p",
                    "w",
                    "restart-u",
                    AttentionDecisionContext {
                        queue_size: 1,
                        ..Default::default()
                    },
                    1,
                    1,
                    &evaluation,
                )
                .await
                .unwrap();
        }
        let reopened = AttentionLearningStore::open(directory.path()).unwrap();
        let restored = reopened
            .get_decision_projection_json("p", "w", projection_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&restored).unwrap(),
            serde_json::from_str::<serde_json::Value>(&projection).unwrap()
        );
    }

    #[tokio::test]
    async fn normalized_lane_paging_reads_only_the_requested_window() {
        let store = store();
        let projection = canonical_projection_fixture("projection-page", 500);
        store
            .persist_canonical_projection_json(
                "p",
                "w",
                "page-u",
                "page-policy",
                "projection-page",
                10,
                &projection,
            )
            .await
            .unwrap();
        let page = store
            .canonical_projection_lane_page("projection-page", "follow_up", 120, 25)
            .await
            .unwrap();
        assert_eq!(page.total, 500);
        assert_eq!(page.items_json.len(), 25);
        assert!(page.items_json[0].contains("follow_up:item-120"));
    }

    #[tokio::test]
    async fn normalized_delivery_references_projection_bodies_and_materializes_only_page_zero() {
        let store = store();
        let projection = canonical_projection_fixture("projection-delivery-page", 50);
        store
            .persist_canonical_projection_json(
                "p",
                "w",
                "delivery-page-u",
                "policy",
                "projection-delivery-page",
                10,
                &projection,
            )
            .await
            .unwrap();
        let ordered_items = (0..50)
            .map(|index| super::super::delivery::AttentionDeliveryCandidate {
                candidate_id: format!("follow_up:item-{index}"),
                source_revision: Some(format!("revision-{index}")),
                root_policy_propensity: 1.0,
                item_json: String::new(),
                attribution_item_json: None,
            })
            .collect::<Vec<_>>();
        let delivery = store
            .create_attention_delivery(
                "p",
                "w",
                &CreateAttentionDelivery {
                    status: AttentionDeliveryStatus::BaselineFallback,
                    fallback_reason: Some("fixture".to_string()),
                    root_decision: super::super::delivery::AttentionDeliveryRootDecision {
                        decision_id: "delivery-normalized-page".to_string(),
                        lane: AttentionSurface::FollowUp,
                        projection_id: "projection-delivery-page".to_string(),
                        universe_digest: "delivery-page-u".to_string(),
                        source_generation_token: None,
                        policy_snapshot_id: None,
                        policy_model_version: None,
                        posterior_version: 0,
                        seed_identity: "baseline".to_string(),
                        universe_size: 50,
                        created_at: 20,
                        expires_at: 10_000,
                    },
                    page_size: 10,
                    ordered_items,
                    health: super::super::delivery::AttentionDeliveryHealth {
                        bandit_mode: crate::config::AttentionBanditMode::Disabled,
                        canary_assigned: false,
                        applied: false,
                        baseline_preserved: true,
                        complete_universe_recorded: true,
                        propensity_coverage: 1.0,
                        degradation_reason: Some("fixture".to_string()),
                        root_sample_count: 0,
                        delivered_count: 0,
                        remaining_count: 50,
                        exact_revision_match: true,
                        replay: false,
                    },
                    policy_snapshot_json: None,
                    min_visible_ms: 1,
                    visibility_rule_version: "visible-v1".to_string(),
                    context: AttentionDecisionContext {
                        queue_size: 50,
                        ..Default::default()
                    },
                },
            )
            .await
            .unwrap();
        assert_eq!(delivery.items.len(), 10);
        assert!(delivery.items[0].item_json.contains("follow_up:item-0"));
        let cursor = delivery.page.next_cursor.as_deref().unwrap();
        assert_eq!(
            store
                .attention_delivery_cursor_source_generation_token(
                    "p",
                    "w",
                    AttentionSurface::FollowUp,
                    cursor,
                    30,
                )
                .await
                .unwrap(),
            None,
            "legacy deliveries must remain on the projection/digest compatibility path"
        );
        let second_page = store
            .read_attention_delivery_page(
                "p",
                "w",
                AttentionSurface::FollowUp,
                cursor,
                None,
                Some("projection-delivery-page"),
                Some("delivery-page-u"),
                30,
            )
            .await
            .unwrap();
        assert_eq!(second_page.items.len(), 10);
        assert!(second_page.items[0].item_json.contains("follow_up:item-10"));
        let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
        let duplicated_bytes: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(length(item_json)), 0) \
                 FROM attention_delivery_decision_items \
                 WHERE decision_id = 'delivery-normalized-page'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(duplicated_bytes, 0);
    }

    #[tokio::test]
    async fn tokenized_delivery_cursor_validates_generation_without_rebuilding_projection_identity()
    {
        let store = store();
        let projection = canonical_projection_fixture("projection-tokenized-cursor", 2);
        store
            .persist_canonical_projection_json(
                "p",
                "w",
                "tokenized-u",
                "policy",
                "projection-tokenized-cursor",
                10,
                &projection,
            )
            .await
            .unwrap();
        let ordered_items = (0..2)
            .map(|index| super::super::delivery::AttentionDeliveryCandidate {
                candidate_id: format!("follow_up:item-{index}"),
                source_revision: Some(format!("revision-{index}")),
                root_policy_propensity: 1.0,
                item_json: String::new(),
                attribution_item_json: None,
            })
            .collect::<Vec<_>>();
        let delivery = store
            .create_attention_delivery(
                "p",
                "w",
                &CreateAttentionDelivery {
                    status: AttentionDeliveryStatus::BaselineFallback,
                    fallback_reason: Some("fixture".to_string()),
                    root_decision: super::super::delivery::AttentionDeliveryRootDecision {
                        decision_id: "delivery-tokenized-cursor".to_string(),
                        lane: AttentionSurface::FollowUp,
                        projection_id: "projection-tokenized-cursor".to_string(),
                        universe_digest: "tokenized-u".to_string(),
                        source_generation_token: Some("source-generation-a".to_string()),
                        policy_snapshot_id: None,
                        policy_model_version: None,
                        posterior_version: 0,
                        seed_identity: "baseline".to_string(),
                        universe_size: 2,
                        created_at: 20,
                        expires_at: 10_000,
                    },
                    page_size: 1,
                    ordered_items,
                    health: super::super::delivery::AttentionDeliveryHealth {
                        bandit_mode: crate::config::AttentionBanditMode::Disabled,
                        canary_assigned: false,
                        applied: false,
                        baseline_preserved: true,
                        complete_universe_recorded: true,
                        propensity_coverage: 1.0,
                        degradation_reason: Some("fixture".to_string()),
                        root_sample_count: 0,
                        delivered_count: 0,
                        remaining_count: 2,
                        exact_revision_match: true,
                        replay: false,
                    },
                    policy_snapshot_json: None,
                    min_visible_ms: 1,
                    visibility_rule_version: "visible-v1".to_string(),
                    context: AttentionDecisionContext {
                        queue_size: 2,
                        ..Default::default()
                    },
                },
            )
            .await
            .unwrap();
        let cursor = delivery.page.next_cursor.as_deref().unwrap();
        assert_eq!(
            store
                .attention_delivery_cursor_source_generation_token(
                    "p",
                    "w",
                    AttentionSurface::FollowUp,
                    cursor,
                    30,
                )
                .await
                .unwrap()
                .as_deref(),
            Some("source-generation-a")
        );
        let replay = store
            .read_attention_delivery_page(
                "p",
                "w",
                AttentionSurface::FollowUp,
                cursor,
                Some("source-generation-a"),
                None,
                None,
                30,
            )
            .await
            .unwrap();
        assert_eq!(replay.items[0].candidate_id, "follow_up:item-1");
        assert!(matches!(
            store
                .read_attention_delivery_page(
                    "p",
                    "w",
                    AttentionSurface::FollowUp,
                    cursor,
                    Some("source-generation-b"),
                    None,
                    None,
                    30,
                )
                .await,
            Err(AttentionDeliveryReadError::RefreshRequired(
                AttentionDeliveryRefreshReason::ProjectionDrift
            ))
        ));
        {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let root_json: String = conn
                .query_row(
                    "SELECT decision_json FROM attention_delivery_decisions \
                     WHERE decision_id = 'delivery-tokenized-cursor'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let mut root: serde_json::Value = serde_json::from_str(&root_json).unwrap();
            root.as_object_mut()
                .unwrap()
                .remove("source_generation_token");
            conn.execute(
                "UPDATE attention_delivery_decisions SET decision_json = ? \
                 WHERE decision_id = 'delivery-tokenized-cursor'",
                params![root.to_string()],
            )
            .unwrap();
        }
        assert!(matches!(
            store
                .read_attention_delivery_page(
                    "p",
                    "w",
                    AttentionSurface::FollowUp,
                    cursor,
                    Some("source-generation-a"),
                    None,
                    None,
                    30,
                )
                .await,
            Err(AttentionDeliveryReadError::RefreshRequired(
                AttentionDeliveryRefreshReason::BindingMismatch
            ))
        ));
    }

    #[tokio::test]
    async fn projection_retention_preserves_referenced_rows_and_reclaims_unreferenced_bodies() {
        let store = store();
        let unreferenced = canonical_projection_fixture("projection-old", 1);
        let referenced = canonical_projection_fixture("projection-live", 1);
        store
            .persist_canonical_projection_json(
                "p",
                "w",
                "old-u",
                "policy",
                "projection-old",
                10,
                &unreferenced,
            )
            .await
            .unwrap();
        store
            .persist_canonical_projection_json(
                "p",
                "w",
                "live-u",
                "policy",
                "projection-live",
                11,
                &referenced,
            )
            .await
            .unwrap();
        {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.execute(
                "INSERT INTO attention_delivery_decisions (decision_id, schema_version, principal, workspace, lane, projection_id, universe_digest, policy_snapshot_json, posterior_version, seed_identity, universe_size, page_size, status, health_json, min_visible_ms, visibility_rule_version, context_json, decision_json, created_at, expires_at) VALUES ('delivery-live', 1, 'p', 'w', 'follow_up', 'projection-live', 'live-u', '{}', 0, 'seed', 1, 1, 'active', '{}', 1, 'v1', '{}', '{}', 11, 999999)",
                [],
            )
            .unwrap();
        }
        let preview = store
            .apply_scoped_retention("p", "w", 20, false)
            .await
            .unwrap();
        assert_eq!(
            preview.affected_rows.get("canonical_projections"),
            Some(&1),
            "preview and apply must use the same exact-reference semantics"
        );
        store
            .apply_scoped_retention("p", "w", 20, true)
            .await
            .unwrap();
        {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            let ids = conn
                .prepare("SELECT projection_id FROM attention_canonical_projections ORDER BY projection_id")
                .unwrap()
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(ids, vec!["projection-live"]);
            conn.execute(
                "DELETE FROM attention_delivery_decisions WHERE decision_id = 'delivery-live'",
                [],
            )
            .unwrap();
        }
        store
            .apply_scoped_retention("p", "w", 20, true)
            .await
            .unwrap();
        store
            .apply_scoped_retention("p", "w", 20, true)
            .await
            .unwrap();
        let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
        let projections: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM attention_canonical_projections",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let bodies: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM attention_canonical_item_revisions",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let diagnostic_bodies: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM attention_canonical_diagnostic_revisions",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!((projections, bodies, diagnostic_bodies), (0, 0, 0));
    }

    #[tokio::test]
    async fn outcome_projection_reference_survives_delivery_compaction() {
        let store = store();
        let projection = canonical_projection_fixture("projection-outcome", 1);
        store
            .persist_canonical_projection_json(
                "p",
                "w",
                "outcome-u",
                "policy",
                "projection-outcome",
                10,
                &projection,
            )
            .await
            .unwrap();
        {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.execute(
                "INSERT INTO attention_delivery_decisions (decision_id, schema_version, principal, workspace, lane, projection_id, universe_digest, policy_snapshot_json, posterior_version, seed_identity, universe_size, page_size, status, health_json, min_visible_ms, visibility_rule_version, context_json, decision_json, created_at, expires_at) VALUES ('delivery-outcome', 1, 'p', 'w', 'follow_up', 'projection-outcome', 'outcome-u', '{}', 0, 'seed', 1, 1, 'active', '{}', 1, 'v1', '{}', '{}', 11, 100)",
                [],
            )
            .unwrap();
        }
        let mut outcome = request("outcome-projection-event", AttentionOutcomeKind::Useful);
        outcome.attribution = Some(AttentionOutcomeAttribution {
            decision_id: "delivery-outcome".to_string(),
            candidate_id: "candidate-1".to_string(),
            source_revision: Some("revision-1".to_string()),
            impression_id: None,
            delivery_id: None,
        });
        store
            .record_outcome("p", "w", AttentionSurface::FollowUp, &outcome, None)
            .await
            .unwrap();
        {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.execute(
                "UPDATE attention_outcomes SET projection_id = NULL \
                 WHERE event_id = 'outcome-projection-event'",
                [],
            )
            .unwrap();
        }
        assert_eq!(
            store.migrate_legacy_projection_references(1).await.unwrap(),
            1
        );
        assert_eq!(
            store.migrate_legacy_projection_references(1).await.unwrap(),
            0
        );
        store
            .compact_attention_deliveries("p", "w", 200, true)
            .await
            .unwrap();
        store
            .apply_scoped_retention("p", "w", 500, true)
            .await
            .unwrap();
        let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
        let stored_projection: Option<String> = conn
            .query_row(
                "SELECT projection_id FROM attention_outcomes WHERE event_id = 'outcome-projection-event'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_projection.as_deref(), Some("projection-outcome"));
        let projection_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM attention_canonical_projections WHERE projection_id = 'projection-outcome'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(projection_count, 1);
    }

    #[tokio::test]
    async fn an_independent_read_is_not_parked_behind_the_writer_mutex() {
        let store = store();
        let writer_store = store.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let _guard = writer_store.conn.lock().unwrap_or_else(|p| p.into_inner());
            ready_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        ready_rx.recv().unwrap();
        let scopes =
            tokio::time::timeout(std::time::Duration::from_millis(250), store.list_scopes())
                .await
                .expect("read must not wait for the writer boundary")
                .unwrap();
        assert!(scopes.is_empty());
        assert_eq!(store.connection_telemetry().writer_wait_count, 0);
        release_tx.send(()).unwrap();
        writer.join().unwrap();
    }

    #[test]
    fn concurrent_openers_serialize_idempotent_schema_bootstrap() {
        let directory = tempfile::TempDir::new().unwrap();
        let path = Arc::new(directory.path().to_path_buf());
        let barrier = Arc::new(std::sync::Barrier::new(4));
        let handles = (0..4)
            .map(|_| {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    AttentionLearningStore::open(&path)
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap().expect("concurrent store opener");
        }
    }

    #[test]
    fn read_pool_exhaustion_is_bounded_and_observable() {
        let store = store();
        let guards = (0..ATTENTION_READ_POOL_SIZE)
            .map(|_| store.reads.acquire("pool_exhaustion_owner").unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            store.connection_telemetry().read_in_use,
            ATTENTION_READ_POOL_SIZE
        );
        let waiting_store = store.clone();
        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let _guard = waiting_store
                .reads
                .acquire("pool_exhaustion_waiter")
                .unwrap();
            acquired_tx.send(()).unwrap();
        });
        assert!(acquired_rx
            .recv_timeout(std::time::Duration::from_millis(30))
            .is_err());
        drop(guards);
        acquired_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        waiter.join().unwrap();
        let telemetry = store.connection_telemetry();
        assert_eq!(telemetry.read_capacity, ATTENTION_READ_POOL_SIZE);
        assert_eq!(telemetry.read_peak_in_use, ATTENTION_READ_POOL_SIZE);
        assert_eq!(telemetry.read_wait_count, 1);
    }

    #[test]
    fn read_pool_reports_connection_hold_time_separately_from_wait_time() {
        let store = store();
        let guard = store.reads.acquire("hold_telemetry_fixture").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        drop(guard);

        let telemetry = store.connection_telemetry();
        assert_eq!(telemetry.read_hold_count, 1);
        assert!(telemetry.read_hold_micros >= 1_000);
        assert_eq!(telemetry.read_max_hold_micros, telemetry.read_hold_micros);
        assert_eq!(telemetry.read_wait_count, 0);
    }

    #[test]
    fn read_pool_exhaustion_fails_within_one_bounded_deadline() {
        let store = store();
        let _guards = (0..ATTENTION_READ_POOL_SIZE)
            .map(|_| store.reads.acquire("bounded_pool_owner").unwrap())
            .collect::<Vec<_>>();
        let started = Instant::now();
        let error = match store.reads.acquire("bounded_pool_waiter") {
            Ok(_) => panic!("exhausted pool unexpectedly opened another connection"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("read pool exhausted"));
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        assert_eq!(store.connection_telemetry().read_wait_count, 1);
    }

    #[test]
    fn read_pool_recovers_poison_and_returns_a_connection_after_consumer_panic() {
        let store = store();
        let poison_store = store.clone();
        assert!(std::thread::spawn(move || {
            let _state = poison_store.reads.state.lock().unwrap();
            panic!("poison read-pool state fixture");
        })
        .join()
        .is_err());

        let panic_store = store.clone();
        assert!(std::thread::spawn(move || {
            let _guard = panic_store.reads.acquire("consumer_panic").unwrap();
            panic!("drop read connection during unwind");
        })
        .join()
        .is_err());

        let guards = (0..ATTENTION_READ_POOL_SIZE)
            .map(|_| store.reads.acquire("post_poison_capacity").unwrap())
            .collect::<Vec<_>>();
        assert_eq!(guards.len(), ATTENTION_READ_POOL_SIZE);
        assert_eq!(
            store.connection_telemetry().read_in_use,
            ATTENTION_READ_POOL_SIZE
        );
    }

    #[test]
    fn writer_wait_telemetry_counts_only_actual_contention() {
        let store = store();
        let held_store = store.clone();
        let waiting_store = store.clone();
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let _guard = held_store.conn.lock().unwrap_or_else(|p| p.into_inner());
            held_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        held_rx.recv().unwrap();
        let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            waiting_tx.send(()).unwrap();
            let _guard = waiting_store.conn.lock().unwrap_or_else(|p| p.into_inner());
        });
        waiting_rx.recv().unwrap();
        wait_for_writer_ticket_count(&store, 2);
        release_tx.send(()).unwrap();
        holder.join().unwrap();
        waiter.join().unwrap();
        let telemetry = store.connection_telemetry();
        assert_eq!(telemetry.writer_wait_count, 1);
        assert!(telemetry.writer_wait_micros > 0);
    }

    #[test]
    fn writer_ticket_gate_acquires_in_issue_order() {
        let store = store();
        let owner = store.conn.lock().unwrap_or_else(|p| p.into_inner());
        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
        let mut waiters = Vec::new();
        for index in 0..4 {
            let waiting_store = store.clone();
            let acquired_tx = acquired_tx.clone();
            waiters.push(std::thread::spawn(move || {
                let _guard = waiting_store.conn.lock().unwrap_or_else(|p| p.into_inner());
                acquired_tx.send(index).unwrap();
            }));
            // Issue each ticket before starting the next contender. This makes
            // the expected queue order deterministic without timing sleeps.
            wait_for_writer_ticket_count(&store, index + 2);
        }
        drop(acquired_tx);
        drop(owner);
        let acquired = acquired_rx.into_iter().collect::<Vec<_>>();
        for waiter in waiters {
            waiter.join().unwrap();
        }
        assert_eq!(acquired, vec![0, 1, 2, 3]);
    }

    #[test]
    fn writer_ticket_gate_advances_and_preserves_poison_after_owner_panic() {
        let store = store();
        let owner_store = store.clone();
        let (owner_ready_tx, owner_ready_rx) = std::sync::mpsc::channel();
        let (panic_tx, panic_rx) = std::sync::mpsc::channel();
        let owner = std::thread::spawn(move || {
            let _guard = owner_store.conn.lock().unwrap_or_else(|p| p.into_inner());
            owner_ready_tx.send(()).unwrap();
            panic_rx.recv().unwrap();
            panic!("poison attention writer fixture");
        });
        owner_ready_rx.recv().unwrap();

        let waiting_store = store.clone();
        let (recovered_tx, recovered_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let poisoned = match waiting_store.conn.lock() {
                Ok(_) => panic!("owner panic must retain connection poison telemetry"),
                Err(poisoned) => poisoned,
            };
            let _guard = poisoned.into_inner();
            recovered_tx.send(()).unwrap();
        });
        wait_for_writer_ticket_count(&store, 2);
        panic_tx.send(()).unwrap();
        assert!(owner.join().is_err());
        recovered_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("queued writer must advance after owner unwind");
        waiter.join().unwrap();
    }

    fn wait_for_writer_ticket_count(store: &AttentionLearningStore, expected: usize) {
        let expected = u64::try_from(expected).unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(1);
        while store.conn.issued_ticket_count() < expected {
            assert!(
                Instant::now() < deadline,
                "writer ticket {expected} was not issued before the bounded test deadline"
            );
            std::thread::yield_now();
        }
    }

    fn routing_evaluation() -> AttentionRoutingEvaluation {
        AttentionRoutingEvaluation {
            decision_id: "decision-1".to_string(),
            decided_at: 10,
            surface: AttentionSurface::FollowUp,
            mode: AttentionRoutingMode::Baseline,
            snapshot_id: None,
            model_version: None,
            policy_seed_identity: "slice4".to_string(),
            canary_assigned: false,
            complete_cross_lane_universe: false,
            degradation_reason: None,
            bandit_health: None,
            items: vec![AttentionDecisionItem {
                decision_id: "decision-1".to_string(),
                candidate_id: "follow_up:candidate-1".to_string(),
                source_revision: Some("distill:7".to_string()),
                feature_values: Some(BTreeMap::from([(
                    "semantic.direct_request_probability".to_string(),
                    0.75,
                )])),
                source_family: "comms_ingest".to_string(),
                hard_eligible: true,
                ineligibility_reason: None,
                baseline_route: AttentionRoute::FollowUp,
                learned_route: AttentionRoute::FollowUp,
                served_route: AttentionRoute::FollowUp,
                routing_mode: AttentionRoutingMode::Baseline,
                routing_snapshot_id: None,
                routing_model_version: None,
                learned_route_confidence: None,
                utility_margin: None,
                route_reason: "baseline_mode".to_string(),
                route_applied: false,
                canary_assigned: false,
                owner_action_required_probability: None,
                information_value_probability: None,
                follow_up_utility: None,
                worth_a_look_utility: None,
                uncertainty: None,
                cluster_id: "cluster-1".to_string(),
                cluster_size: 1,
                representative: true,
                baseline_rank: 1,
                learned_rank: 1,
                served_rank: 1,
                selected: true,
                selection_probability: 1.0,
                exploration: false,
                feature_snapshot_digest: None,
                bandit_decision: None,
                extraction_status: super::super::SemanticExtractionStatus::Missing,
                feature_contracts: AttentionDecisionFeatureContracts {
                    routing_feature_contract: super::super::ATTENTION_ROUTING_FEATURE_CONTRACT
                        .to_string(),
                    routing_snapshot_id: None,
                    actionability_snapshot_id: None,
                    actionability_model_version: None,
                    grouping_snapshot_id: None,
                    grouping_model_version: None,
                    semantic_schema_version: ATTENTION_SEMANTIC_SCHEMA_VERSION,
                    semantic_extractor_contract: ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT.to_string(),
                    semantic_prompt_version: None,
                    semantic_model: None,
                    semantic_profile: None,
                },
            }],
        }
    }

    /// The same decision carrying both lanes, which is how serving actually
    /// works: one decision covers the whole cross-lane universe and its own
    /// `surface` is always `follow_up`.
    fn cross_lane_routing_evaluation() -> AttentionRoutingEvaluation {
        let mut evaluation = routing_evaluation();
        let follow_up = evaluation.items[0].clone();
        evaluation.items.push(AttentionDecisionItem {
            candidate_id: "worth_a_look:candidate-2".to_string(),
            served_route: AttentionRoute::WorthALook,
            cluster_id: "cluster-2".to_string(),
            feature_values: Some(BTreeMap::from([(
                "semantic.information_value_probability".to_string(),
                0.4,
            )])),
            ..follow_up
        });
        evaluation
    }

    #[tokio::test]
    async fn serving_captures_the_feature_vector_once_per_candidate_revision() {
        // Without this capture there is no training set at all. Acting on an
        // item is what produces a label, and acting on it is also what retires
        // the source row its semantics live in — so a trainer running later
        // finds labels with no features. Measured on the live store before this
        // existed: 3 positives with 0 usable feature vectors, 2,291 negatives
        // with 51.
        let store = store();
        for index in 0..3 {
            // These are three distinct serving decisions over the same
            // candidate revision. Reusing one decision id would correctly hit
            // the decision table's primary-key constraint before exercising
            // feature-snapshot deduplication.
            let mut evaluation = routing_evaluation();
            let decision_id = format!("decision-{index}");
            evaluation.decision_id = decision_id.clone();
            for item in &mut evaluation.items {
                item.decision_id = decision_id.clone();
            }
            store
                .record_decision(
                    "owner",
                    "default",
                    "candidate-set",
                    AttentionDecisionContext {
                        queue_size: 1,
                        ..Default::default()
                    },
                    4,
                    1,
                    &evaluation,
                )
                .await
                .unwrap();
        }

        let captured = store
            .list_candidate_feature_snapshots("owner", "default")
            .await
            .unwrap();
        // Three decisions over the same universe, one stored vector: the same
        // candidate at the same revision is one observation however often it is
        // re-served.
        assert_eq!(captured.len(), 1);
        let row = &captured[0];
        assert_eq!(row.candidate_id, "follow_up:candidate-1");
        assert_eq!(row.source_revision, "distill:7");
        assert_eq!(
            row.features
                .get("semantic.direct_request_probability")
                .copied(),
            Some(0.75),
            "the stored vector must be the one serving actually used"
        );
    }

    #[tokio::test]
    async fn a_changed_vector_at_the_same_revision_is_kept_as_its_own_variant() {
        // `source_revision` binds the classifier's INPUT, not its output.
        // Re-running the classifier over an unchanged message can yield a
        // different label, reason and summary, and the thread-state join moves
        // subject and received_at when a newer message lands. Measured live:
        // 10 of 464 candidate/revision pairs served more than one payload
        // within an hour, one of them twelve. Keyed on the revision alone the
        // first vector wins forever and every later label silently inherits
        // features describing something the owner never saw.
        let store = store();
        for (index, probability) in [(0usize, 0.75f64), (1, 0.10)] {
            let mut evaluation = routing_evaluation();
            let decision_id = format!("decision-{index}");
            evaluation.decision_id = decision_id.clone();
            evaluation.decided_at = 10 + index as i64;
            for item in &mut evaluation.items {
                item.decision_id = decision_id.clone();
                // Same candidate, same revision, different features.
                item.feature_values = Some(BTreeMap::from([(
                    "semantic.direct_request_probability".to_string(),
                    probability,
                )]));
            }
            store
                .record_decision(
                    "owner",
                    "default",
                    "candidate-set",
                    AttentionDecisionContext {
                        queue_size: 1,
                        ..Default::default()
                    },
                    4,
                    1,
                    &evaluation,
                )
                .await
                .unwrap();
        }

        let captured = store
            .list_candidate_feature_snapshots("owner", "default")
            .await
            .unwrap();
        assert_eq!(
            captured.len(),
            2,
            "both variants must survive; the revision cannot tell them apart"
        );
        let mut probabilities: Vec<f64> = captured
            .iter()
            .map(|row| {
                row.features
                    .get("semantic.direct_request_probability")
                    .copied()
                    .unwrap()
            })
            .collect();
        probabilities.sort_by(|left, right| left.partial_cmp(right).unwrap());
        assert_eq!(probabilities, vec![0.10, 0.75]);
        assert_ne!(
            captured[0].content_digest, captured[1].content_digest,
            "the digest is what separates them"
        );
        let first = store
            .candidate_features_for_decision(
                "owner",
                "default",
                "decision-0",
                "follow_up:candidate-1",
            )
            .await
            .unwrap()
            .unwrap();
        let second = store
            .candidate_features_for_decision(
                "owner",
                "default",
                "decision-1",
                "follow_up:candidate-1",
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.features["semantic.direct_request_probability"], 0.75);
        assert_eq!(second.features["semantic.direct_request_probability"], 0.10);
        assert_ne!(first.content_digest, second.content_digest);
    }

    #[tokio::test]
    async fn identical_values_from_different_semantic_producers_are_distinct_vectors() {
        let store = store();
        for (index, (model, profile)) in [
            ("semantic-model-a", "profile-a"),
            ("semantic-model-b", "profile-b"),
        ]
        .into_iter()
        .enumerate()
        {
            let mut evaluation = routing_evaluation();
            evaluation.decision_id = format!("producer-decision-{index}");
            evaluation.decided_at = 100 + index as i64;
            for item in &mut evaluation.items {
                item.decision_id = evaluation.decision_id.clone();
                item.feature_contracts.semantic_model = Some(model.to_string());
                item.feature_contracts.semantic_profile = Some(profile.to_string());
            }
            store
                .record_decision(
                    "owner",
                    "default",
                    "same-source-universe",
                    AttentionDecisionContext {
                        queue_size: 1,
                        ..Default::default()
                    },
                    1,
                    1,
                    &evaluation,
                )
                .await
                .unwrap();
        }

        let captured = store
            .list_candidate_feature_snapshots("owner", "default")
            .await
            .unwrap();
        assert_eq!(captured.len(), 2);
        assert_ne!(captured[0].content_digest, captured[1].content_digest);
        assert_eq!(
            captured
                .iter()
                .map(|row| row.semantic_schema_version)
                .collect::<HashSet<_>>(),
            HashSet::from([ATTENTION_SEMANTIC_SCHEMA_VERSION])
        );
        assert_eq!(
            captured
                .iter()
                .filter_map(|row| row.semantic_model.as_deref())
                .collect::<HashSet<_>>(),
            HashSet::from(["semantic-model-a", "semantic-model-b"])
        );
        let exact = store
            .candidate_features_for_decision(
                "owner",
                "default",
                "producer-decision-1",
                "follow_up:candidate-1",
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(exact.semantic_model.as_deref(), Some("semantic-model-b"));
        assert_eq!(exact.semantic_profile.as_deref(), Some("profile-b"));
    }

    #[tokio::test]
    async fn repeated_complete_universe_decisions_reuse_one_feature_payload_and_binding() {
        let store = store();
        for index in 0..50 {
            let mut evaluation = routing_evaluation();
            evaluation.decision_id = format!("bounded-decision-{index}");
            evaluation.decided_at = 100 + index;
            for item in &mut evaluation.items {
                item.decision_id = evaluation.decision_id.clone();
            }
            store
                .record_decision(
                    "owner",
                    "default",
                    "same-universe",
                    AttentionDecisionContext {
                        queue_size: 1,
                        ..Default::default()
                    },
                    1,
                    1,
                    &evaluation,
                )
                .await
                .unwrap();
        }
        let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
        let vectors: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM attention_feature_vectors",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let bindings: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM attention_candidate_feature_bindings",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!((vectors, bindings), (1, 1));
    }

    #[tokio::test]
    async fn feature_vector_digest_collision_fails_before_decision_commit() {
        let store = store();
        let first = routing_evaluation();
        store
            .record_decision(
                "owner",
                "default",
                "same-universe",
                AttentionDecisionContext {
                    queue_size: 1,
                    ..Default::default()
                },
                1,
                1,
                &first,
            )
            .await
            .unwrap();
        {
            let conn = store
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            conn.execute(
                "UPDATE attention_feature_vectors SET features_json = '{\"tampered\":1}'",
                [],
            )
            .unwrap();
        }
        let mut second = routing_evaluation();
        second.decision_id = "decision-collision".to_string();
        second.items[0].decision_id = second.decision_id.clone();
        let error = store
            .record_decision(
                "owner",
                "default",
                "same-universe",
                AttentionDecisionContext {
                    queue_size: 1,
                    ..Default::default()
                },
                1,
                1,
                &second,
            )
            .await
            .expect_err("content digest collision must fail closed");
        assert!(error
            .to_string()
            .contains("attention feature digest collision"));
        let conn = store
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let committed: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM attention_decisions \
                 WHERE decision_id = 'decision-collision'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(committed, 0);
    }

    #[tokio::test]
    async fn evaluated_but_unserved_candidates_do_not_create_training_vectors() {
        let store = store();
        let mut evaluation = routing_evaluation();
        evaluation.items[0].selected = false;
        evaluation.items[0].selection_probability = 0.0;
        evaluation.items[0].served_rank = 0;
        store
            .record_decision(
                "owner",
                "default",
                "unserved-universe",
                AttentionDecisionContext {
                    queue_size: 1,
                    ..Default::default()
                },
                1,
                0,
                &evaluation,
            )
            .await
            .unwrap();
        assert!(store
            .list_candidate_feature_snapshots("owner", "default")
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .get_decision("owner", "default", "decision-1")
                .await
                .unwrap()
                .unwrap()
                .items
                .len(),
            1,
            "feature capture must not remove the complete evaluated decision set"
        );
    }

    #[tokio::test]
    async fn legacy_feature_migration_is_bounded_idempotent_and_backward_readable() {
        let store = store();
        {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            for index in 0..2 {
                conn.execute(
                    "INSERT INTO attention_candidate_feature_snapshots (principal, workspace, surface, candidate_id, source_revision, feature_contract, semantic_extractor_contract, semantic_prompt_version, features_json, content_digest, first_served_at) VALUES ('p', 'w', 'follow_up', ?, ?, ?, ?, '1.1.0', ?, '', ?)",
                    params![
                        format!("candidate-{index}"),
                        format!("revision-{index}"),
                        ACTIONABILITY_FEATURE_CONTRACT,
                        ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT,
                        serde_json::json!({"age_days_log1p": index as f64}).to_string(),
                        10 + index,
                    ],
                )
                .unwrap();
            }
        }
        assert_eq!(
            store
                .list_candidate_feature_snapshots("p", "w")
                .await
                .unwrap()
                .len(),
            2
        );
        assert_eq!(store.migrate_legacy_feature_snapshots(1).await.unwrap(), 1);
        assert_eq!(store.migrate_legacy_feature_snapshots(1).await.unwrap(), 1);
        assert_eq!(store.migrate_legacy_feature_snapshots(1).await.unwrap(), 0);
        let rows = store
            .list_candidate_feature_snapshots("p", "w")
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| !row.content_digest.is_empty()));
    }

    #[tokio::test]
    async fn oversized_legacy_feature_is_quarantined_without_starving_later_rows() {
        let directory = tempfile::TempDir::new().unwrap();
        let oversized = format!(
            "{{\"oversized\":\"{}\"}}",
            "x".repeat(MAX_ATTENTION_FEATURE_VECTOR_BYTES)
        );
        {
            let store = AttentionLearningStore::open(directory.path()).unwrap();
            {
                let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
                conn.execute(
                    "INSERT INTO attention_candidate_feature_snapshots ( \
                        principal, workspace, surface, candidate_id, source_revision, \
                        feature_contract, semantic_extractor_contract, semantic_prompt_version, \
                        features_json, content_digest, first_served_at \
                     ) VALUES ('p', 'w', 'follow_up', 'oversized-first', 'revision-bad', \
                               ?, ?, '1.1.0', ?, '', 10)",
                    params![
                        ACTIONABILITY_FEATURE_CONTRACT,
                        ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT,
                        oversized,
                    ],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO attention_candidate_feature_snapshots ( \
                        principal, workspace, surface, candidate_id, source_revision, \
                        feature_contract, semantic_extractor_contract, semantic_prompt_version, \
                        features_json, content_digest, first_served_at \
                     ) VALUES ('p', 'w', 'follow_up', 'valid-second', 'revision-good', \
                               ?, ?, '1.1.0', '{\"age_days_log1p\":1.0}', '', 11)",
                    params![
                        ACTIONABILITY_FEATURE_CONTRACT,
                        ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT,
                    ],
                )
                .unwrap();
            }

            assert_eq!(
                store.migrate_legacy_feature_snapshots(1).await.unwrap(),
                1,
                "quarantine-only progress must keep a batch drainer running"
            );
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            let quarantine: (i64, String, String) = conn
                .query_row(
                    "SELECT COUNT(*), MIN(error_code), MIN(source_fingerprint) \
                     FROM attention_legacy_migration_quarantine \
                     WHERE migration_kind = ?",
                    params![LEGACY_FEATURE_MIGRATION_KIND],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(quarantine.0, 1);
            assert_eq!(quarantine.1, "payload_too_large");
            assert_eq!(quarantine.2.len(), 64);
            let remaining_legacy: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM attention_candidate_feature_snapshots",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let migrated_valid: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM attention_candidate_feature_bindings \
                     WHERE principal = 'p' AND workspace = 'w' \
                       AND candidate_id = 'valid-second'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!((remaining_legacy, migrated_valid), (2, 0));
        }

        // A durable quarantine key must be based on the logical legacy row,
        // not SQLite's mutable rowid: VACUUM may renumber rowids in tables
        // without an INTEGER PRIMARY KEY.
        let database = directory.path().join("attention_learning.db");
        Connection::open(&database)
            .unwrap()
            .execute_batch("VACUUM")
            .unwrap();
        {
            let reopened = AttentionLearningStore::open(directory.path()).unwrap();
            assert_eq!(
                reopened.migrate_legacy_feature_snapshots(1).await.unwrap(),
                1,
                "the valid successor must migrate after restart and rowid compaction"
            );
            let conn = reopened.conn.lock().unwrap_or_else(|p| p.into_inner());
            let remaining_legacy: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM attention_candidate_feature_snapshots",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let migrated_valid: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM attention_candidate_feature_bindings \
                     WHERE principal = 'p' AND workspace = 'w' \
                       AND candidate_id = 'valid-second'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!((remaining_legacy, migrated_valid), (1, 1));
        }

        let reopened = AttentionLearningStore::open(directory.path()).unwrap();
        assert_eq!(
            reopened.migrate_legacy_feature_snapshots(2).await.unwrap(),
            0,
            "the quarantined head row must remain non-blocking after restart"
        );
        let conn = reopened.conn.lock().unwrap_or_else(|p| p.into_inner());
        let quarantine_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM attention_legacy_migration_quarantine \
                 WHERE migration_kind = ?",
                params![LEGACY_FEATURE_MIGRATION_KIND],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(quarantine_count, 1);
    }

    #[tokio::test]
    async fn repaired_quarantined_feature_reenters_migration_by_payload_fingerprint() {
        let store = store();
        {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.execute(
                "INSERT INTO attention_candidate_feature_snapshots ( \
                    principal, workspace, surface, candidate_id, source_revision, \
                    feature_contract, semantic_extractor_contract, semantic_prompt_version, \
                    features_json, content_digest, first_served_at \
                 ) VALUES ('p', 'w', 'follow_up', 'repaired-feature', 'revision-1', \
                           ?, ?, '1.1.0', 'not-json', '', 10)",
                params![
                    ACTIONABILITY_FEATURE_CONTRACT,
                    ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT,
                ],
            )
            .unwrap();
        }
        assert_eq!(store.migrate_legacy_feature_snapshots(1).await.unwrap(), 1);
        {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.execute(
                "UPDATE attention_candidate_feature_snapshots \
                 SET features_json = '{\"age_days_log1p\":1.0}' \
                 WHERE principal = 'p' AND workspace = 'w' \
                   AND candidate_id = 'repaired-feature'",
                [],
            )
            .unwrap();
        }
        assert_eq!(
            store.migrate_legacy_feature_snapshots(1).await.unwrap(),
            1,
            "a changed payload fingerprint must invalidate the durable quarantine decision"
        );
        let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
        let state: (i64, i64) = conn
            .query_row(
                "SELECT \
                    (SELECT COUNT(*) FROM attention_candidate_feature_snapshots \
                     WHERE candidate_id = 'repaired-feature'), \
                    (SELECT COUNT(*) FROM attention_candidate_feature_bindings \
                     WHERE candidate_id = 'repaired-feature')",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, (0, 1));
    }

    #[tokio::test]
    async fn legacy_feature_migration_rebinds_the_exact_decision_and_served_lane() {
        let store = store();
        let mut evaluation = routing_evaluation();
        evaluation.decision_id = "legacy-feature-decision".to_string();
        evaluation.decided_at = 77;
        for item in &mut evaluation.items {
            item.decision_id = evaluation.decision_id.clone();
        }
        let features_json =
            serde_json::to_string(evaluation.items[0].feature_values.as_ref().unwrap()).unwrap();
        store
            .record_decision(
                "owner",
                "default",
                "legacy-feature-universe",
                AttentionDecisionContext {
                    queue_size: 1,
                    ..Default::default()
                },
                1,
                1,
                &evaluation,
            )
            .await
            .unwrap();
        {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.execute(
                "DELETE FROM attention_candidate_feature_bindings WHERE principal = 'owner'",
                [],
            )
            .unwrap();
            conn.execute("DELETE FROM attention_feature_vectors", [])
                .unwrap();
            conn.execute(
                "UPDATE attention_decision_items SET feature_snapshot_digest = 'legacy-routing-digest' \
                 WHERE decision_id = 'legacy-feature-decision'",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO attention_candidate_feature_snapshots (principal, workspace, surface, candidate_id, source_revision, feature_contract, semantic_extractor_contract, semantic_prompt_version, features_json, content_digest, first_served_at) VALUES ('owner', 'default', 'worth_a_look', 'follow_up:candidate-1', 'distill:7', ?, ?, '1.1.0', ?, '', 77)",
                params![
                    ACTIONABILITY_FEATURE_CONTRACT,
                    ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT,
                    features_json,
                ],
            )
            .unwrap();
        }
        assert_eq!(store.migrate_legacy_feature_snapshots(1).await.unwrap(), 1);
        let exact = store
            .candidate_features_for_decision(
                "owner",
                "default",
                "legacy-feature-decision",
                "follow_up:candidate-1",
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(exact.surface, "follow_up");
        assert_eq!(exact.features["semantic.direct_request_probability"], 0.75);
    }

    #[tokio::test]
    async fn each_captured_vector_is_filed_under_the_lane_it_was_served_into() {
        // A decision covers the whole cross-lane universe and its own `surface`
        // is always `follow_up`. Filing every vector under the decision's
        // surface therefore mislabels every Worth-a-look candidate, and an
        // outcome joins features on its own surface — so the mislabelled rows
        // are unreachable and half the training set silently disappears.
        // Observed live: 139 of 282 captured vectors were filed wrong.
        let store = store();
        let evaluation = cross_lane_routing_evaluation();
        assert_eq!(
            evaluation.surface,
            AttentionSurface::FollowUp,
            "the decision's own surface is the misleading value this guards against"
        );
        store
            .record_decision(
                "owner",
                "default",
                "candidate-set",
                AttentionDecisionContext {
                    queue_size: 1,
                    ..Default::default()
                },
                4,
                2,
                &evaluation,
            )
            .await
            .unwrap();

        let captured = store
            .list_candidate_feature_snapshots("owner", "default")
            .await
            .unwrap();
        let mut filed: Vec<(String, String)> = captured
            .into_iter()
            .map(|row| (row.candidate_id, row.surface))
            .collect();
        filed.sort();
        assert_eq!(
            filed,
            vec![
                ("follow_up:candidate-1".to_string(), "follow_up".to_string()),
                (
                    "worth_a_look:candidate-2".to_string(),
                    "worth_a_look".to_string()
                ),
            ]
        );
    }

    #[tokio::test]
    async fn decision_commit_persists_the_complete_evaluated_item_set() {
        let store = store();
        let evaluation = routing_evaluation();
        let decision = store
            .record_decision(
                "owner",
                "default",
                "candidate-set",
                AttentionDecisionContext {
                    queue_size: 1,
                    ..Default::default()
                },
                4,
                1,
                &evaluation,
            )
            .await
            .unwrap();
        assert!(decision.complete_universe_recorded);
        let detail = store
            .get_decision("owner", "default", "decision-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(detail.items.len(), 1);
        assert_eq!(detail.items[0].candidate_id, "follow_up:candidate-1");
        assert!(store
            .get_decision("other", "default", "decision-1")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn decision_commit_rejects_a_returned_count_that_differs_from_selection() {
        let store = store();
        let error = store
            .record_decision(
                "owner",
                "default",
                "candidate-set",
                AttentionDecisionContext {
                    queue_size: 1,
                    ..Default::default()
                },
                4,
                0,
                &routing_evaluation(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("must equal selected item count"));
    }

    #[tokio::test]
    async fn impression_replay_uses_monotonic_dwell_without_double_counting() {
        let store = store();
        let evaluation = routing_evaluation();
        store
            .record_decision(
                "owner",
                "default",
                "candidate-set",
                AttentionDecisionContext {
                    queue_size: 1,
                    ..Default::default()
                },
                4,
                1,
                &evaluation,
            )
            .await
            .unwrap();
        let delivery = store
            .create_attention_delivery(
                "owner",
                "default",
                &CreateAttentionDelivery {
                    status: AttentionDeliveryStatus::BaselineFallback,
                    fallback_reason: Some("bandit_disabled".to_string()),
                    root_decision: super::super::delivery::AttentionDeliveryRootDecision {
                        decision_id: "delivery-root-1".to_string(),
                        lane: AttentionSurface::FollowUp,
                        projection_id: "decision-1".to_string(),
                        universe_digest: "universe-1".to_string(),
                        source_generation_token: None,
                        policy_snapshot_id: None,
                        policy_model_version: None,
                        posterior_version: 0,
                        seed_identity: "baseline".to_string(),
                        universe_size: 1,
                        created_at: 10,
                        expires_at: 10_000,
                    },
                    page_size: 1,
                    ordered_items: vec![super::super::delivery::AttentionDeliveryCandidate {
                        candidate_id: "follow_up:candidate-1".to_string(),
                        source_revision: Some("distill:7".to_string()),
                        root_policy_propensity: 1.0,
                        item_json: "{}".to_string(),
                        attribution_item_json: Some(
                            serde_json::to_string(&evaluation.items[0]).unwrap(),
                        ),
                    }],
                    health: super::super::delivery::AttentionDeliveryHealth {
                        bandit_mode: crate::config::AttentionBanditMode::Disabled,
                        canary_assigned: false,
                        applied: false,
                        baseline_preserved: true,
                        complete_universe_recorded: true,
                        propensity_coverage: 1.0,
                        degradation_reason: Some("bandit_disabled".to_string()),
                        root_sample_count: 0,
                        delivered_count: 0,
                        remaining_count: 1,
                        exact_revision_match: true,
                        replay: false,
                    },
                    policy_snapshot_json: None,
                    min_visible_ms: 1_000,
                    visibility_rule_version: "visible-v1".to_string(),
                    context: AttentionDecisionContext {
                        queue_size: 1,
                        ..Default::default()
                    },
                },
            )
            .await
            .unwrap();
        let mut request = RecordAttentionImpression {
            event_id: "visible-1".to_string(),
            decision_id: delivery.root_decision.decision_id,
            delivery_id: delivery.page.delivery_id,
            page_index: 0,
            position: 1,
            exposure_token: delivery.items[0].exposure_token.clone(),
            candidate_id: "follow_up:candidate-1".to_string(),
            source_revision: Some("distill:7".to_string()),
            surface: AttentionSurface::FollowUp,
            visible_ms: 400,
            visibility_rule_version: "visible-v1".to_string(),
            client_type: "web".to_string(),
            client_version: "1".to_string(),
            viewport_class: "desktop".to_string(),
        };
        let first = store
            .record_impression("owner", "default", &request, 1_000, "visible-v1", 20)
            .await
            .unwrap();
        assert!(!first.verified);
        request.visible_ms = 1_200;
        let replay = store
            .record_impression("owner", "default", &request, 1_000, "visible-v1", 30)
            .await
            .unwrap();
        assert!(replay.verified);
        assert!(replay.deduplicated);
        assert_eq!(replay.accumulated_visible_ms, 1_200);

        request.candidate_id = "different".to_string();
        assert!(matches!(
            store
                .record_impression("owner", "default", &request, 1_000, "visible-v1", 40)
                .await,
            Err(AttentionImpressionError::DecisionItemNotFound)
                | Err(AttentionImpressionError::EventIdentityConflict)
        ));
    }

    #[tokio::test]
    async fn impression_bounds_are_rejected_before_store_lookup() {
        let store = store();
        let request = RecordAttentionImpression {
            event_id: "visible-1".to_string(),
            decision_id: "missing-decision".to_string(),
            delivery_id: "missing-delivery".to_string(),
            page_index: 0,
            position: 1,
            exposure_token: "missing-exposure".to_string(),
            candidate_id: "missing-candidate".to_string(),
            source_revision: None,
            surface: AttentionSurface::FollowUp,
            visible_ms: ATTENTION_VISIBLE_MS_MAX + 1,
            visibility_rule_version: "visible-v1".to_string(),
            client_type: "web".to_string(),
            client_version: "1".to_string(),
            viewport_class: "desktop".to_string(),
        };
        assert!(matches!(
            store
                .record_impression("owner", "default", &request, 1_000, "visible-v1", 20)
                .await,
            Err(AttentionImpressionError::InvalidRequest(_))
        ));

        let mut bounded_visible = request;
        bounded_visible.visible_ms = 1;
        bounded_visible.client_type = "x".repeat(ATTENTION_CLIENT_TYPE_MAX_CHARS + 1);
        assert!(matches!(
            store
                .record_impression(
                    "owner",
                    "default",
                    &bounded_visible,
                    1_000,
                    "visible-v1",
                    20,
                )
                .await,
            Err(AttentionImpressionError::InvalidRequest(_))
        ));
    }

    #[tokio::test]
    async fn impression_policy_dwell_has_a_positive_finite_upper_bound() {
        let store = store();
        let request = RecordAttentionImpression {
            event_id: "visible-1".to_string(),
            decision_id: "missing-decision".to_string(),
            delivery_id: "missing-delivery".to_string(),
            page_index: 0,
            position: 1,
            exposure_token: "missing-exposure".to_string(),
            candidate_id: "missing-candidate".to_string(),
            source_revision: None,
            surface: AttentionSurface::FollowUp,
            visible_ms: 1,
            visibility_rule_version: "visible-v1".to_string(),
            client_type: "web".to_string(),
            client_version: "1".to_string(),
            viewport_class: "desktop".to_string(),
        };
        for invalid in [0, ATTENTION_MIN_VISIBLE_MS_MAX + 1] {
            assert!(matches!(
                store
                    .record_impression("owner", "default", &request, invalid, "visible-v1", 20,)
                    .await,
                Err(AttentionImpressionError::InvalidRequest(_))
            ));
        }
    }

    #[tokio::test]
    async fn retention_and_scope_deletion_default_to_read_only_preview() {
        let store = store();
        let retention = store
            .apply_scoped_retention("owner", "default", 1, false)
            .await
            .unwrap();
        assert!(!retention.apply);
        assert_eq!(retention.cutoff_at, Some(1));
        let deletion = store.delete_scope("owner", "default", false).await.unwrap();
        assert!(!deletion.apply);
        assert_eq!(deletion.cutoff_at, None);
    }

    #[test]
    fn impression_client_metadata_uses_field_specific_upper_bounds() {
        let base = RecordAttentionImpression {
            event_id: "visible-1".to_string(),
            decision_id: "decision-1".to_string(),
            delivery_id: "delivery-1".to_string(),
            page_index: 0,
            position: 1,
            exposure_token: "exposure-token-1".to_string(),
            candidate_id: "candidate-1".to_string(),
            source_revision: Some("distill:7".to_string()),
            surface: AttentionSurface::FollowUp,
            visible_ms: 1,
            visibility_rule_version: "visible-v1".to_string(),
            client_type: "w".repeat(ATTENTION_CLIENT_TYPE_MAX_CHARS),
            client_version: "v".repeat(ATTENTION_CLIENT_VERSION_MAX_CHARS),
            viewport_class: "d".repeat(ATTENTION_VIEWPORT_CLASS_MAX_CHARS),
        };
        assert!(validate_impression_request(&base, "visible-v1").is_ok());

        let mut too_long = base.clone();
        too_long.client_type.push('x');
        assert!(matches!(
            validate_impression_request(&too_long, "visible-v1"),
            Err(AttentionImpressionError::InvalidRequest(_))
        ));
        let mut too_long = base.clone();
        too_long.client_version.push('x');
        assert!(matches!(
            validate_impression_request(&too_long, "visible-v1"),
            Err(AttentionImpressionError::InvalidRequest(_))
        ));
        let mut too_long = base;
        too_long.viewport_class.push('x');
        assert!(matches!(
            validate_impression_request(&too_long, "visible-v1"),
            Err(AttentionImpressionError::InvalidRequest(_))
        ));
    }

    fn actionability_snapshot() -> ActionabilityModelSnapshot {
        ActionabilityModelSnapshot {
            snapshot_id: "snapshot-v1".to_string(),
            model_version: "logistic-v1".to_string(),
            feature_contract: ACTIONABILITY_FEATURE_CONTRACT.to_string(),
            semantic_schema_version: ATTENTION_SEMANTIC_SCHEMA_VERSION,
            semantic_extractor_contract: ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT.to_string(),
            semantic_prompt_version: "1.1.0".to_string(),
            semantic_model: None,
            semantic_profile: None,
            feature_names: vec!["semantic.direct_request_probability".to_string()],
            coefficients: vec![1.0],
            intercept: 0.0,
            l2_lambda: 1.0,
            platt_a: 1.0,
            platt_b: 0.0,
            trained_at: 1,
            training_manifest: ActionabilityTrainingManifest {
                dataset_digest: "fixture".to_string(),
                data_cutoff_at: 1,
                split_strategy: "grouped_temporal".to_string(),
                group_keys: vec!["sender".to_string()],
                positive_outcomes: vec!["action_completed".to_string()],
                negative_outcomes: vec!["not_actionable".to_string()],
                excluded_outcomes: vec!["useful".to_string()],
                metrics: std::collections::BTreeMap::new(),
            },
        }
    }

    #[tokio::test]
    async fn snapshot_install_is_idempotent_and_rejects_id_reuse() {
        let store = store();
        let snapshot = actionability_snapshot();
        store
            .install_actionability_snapshot(&snapshot)
            .await
            .unwrap();
        store
            .install_actionability_snapshot(&snapshot)
            .await
            .unwrap();
        assert_eq!(
            store
                .get_actionability_snapshot(&snapshot.snapshot_id)
                .await
                .unwrap(),
            Some(snapshot.clone())
        );

        let mut collision = snapshot;
        collision.coefficients = vec![2.0];
        assert!(store
            .install_actionability_snapshot(&collision)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn a_first_install_is_forced_to_shadow_whatever_was_requested() {
        let store = store();
        let mode = store
            .install_actionability_snapshot_for_scope(
                "p",
                "w",
                &actionability_snapshot(),
                crate::config::AttentionActionabilityMode::Enforced,
            )
            .await
            .unwrap();
        // Training metrics measure fit. Only shadow measures behaviour, and a
        // scope with no prior snapshot has no behavioural evidence at all.
        assert_eq!(mode, crate::config::AttentionActionabilityMode::Shadow);
    }

    #[tokio::test]
    async fn a_later_install_honours_the_requested_mode() {
        let store = store();
        store
            .install_actionability_snapshot_for_scope(
                "p",
                "w",
                &actionability_snapshot(),
                crate::config::AttentionActionabilityMode::Shadow,
            )
            .await
            .unwrap();
        let mut next = actionability_snapshot();
        next.snapshot_id = "snapshot-v2".to_string();
        let mode = store
            .install_actionability_snapshot_for_scope(
                "p",
                "w",
                &next,
                crate::config::AttentionActionabilityMode::Enforced,
            )
            .await
            .unwrap();
        assert_eq!(mode, crate::config::AttentionActionabilityMode::Enforced);
    }

    #[tokio::test]
    async fn event_id_is_idempotent_only_for_the_same_payload() {
        let store = store();
        let original = request("event-1", AttentionOutcomeKind::Useful);
        let first = store
            .record_outcome(
                "owner",
                "default",
                AttentionSurface::FollowUp,
                &original,
                None,
            )
            .await
            .unwrap();
        let replay = store
            .record_outcome(
                "owner",
                "default",
                AttentionSurface::FollowUp,
                &original,
                None,
            )
            .await
            .unwrap();
        assert!(first.inserted);
        assert!(!replay.inserted);
        assert_eq!(first.outcome_id, replay.outcome_id);
        assert_eq!(replay.outcome, AttentionOutcomeKind::Useful);
    }

    #[tokio::test]
    async fn outcome_and_missing_embedding_repair_are_committed_together() {
        let store = store();
        let persisted = store
            .record_outcome(
                "owner",
                "default",
                AttentionSurface::FollowUp,
                &request("repair-event", AttentionOutcomeKind::Useful),
                None,
            )
            .await
            .unwrap();

        assert!(store
            .outcome_exists_for_event("owner", "default", "repair-event")
            .await
            .unwrap());
        let work = store.list_pending_embedding_binds(10).await.unwrap();
        assert_eq!(work.len(), 1);
        assert_eq!(work[0].outcome_id, persisted.outcome_id);
        assert_eq!(work[0].semantic_text, "safe summary");
        assert_eq!(work[0].attempts, 0);
        let preview = store
            .apply_scoped_retention("owner", "default", i64::MAX, false)
            .await
            .unwrap();
        assert_eq!(preview.affected_rows.get("outcomes"), Some(&0));
        store
            .apply_scoped_retention("owner", "default", i64::MAX, true)
            .await
            .unwrap();
        assert!(store
            .outcome_exists_for_event("owner", "default", "repair-event")
            .await
            .unwrap());
        assert_eq!(
            store.list_pending_embedding_binds(10).await.unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn embedding_bind_queue_is_scope_fair_and_classifies_retryable_failures() {
        let store = store();
        for (principal, event_id) in [
            ("scope-a", "event-a-1"),
            ("scope-a", "event-a-2"),
            ("scope-b", "event-b-1"),
        ] {
            store
                .record_outcome(
                    principal,
                    "default",
                    AttentionSurface::FollowUp,
                    &request(event_id, AttentionOutcomeKind::Useful),
                    None,
                )
                .await
                .unwrap();
        }

        let first_page = store
            .claim_pending_embedding_binds(2, "test-worker", 300)
            .await
            .unwrap();
        assert_eq!(first_page.len(), 2);
        assert_ne!(first_page[0].principal, first_page[1].principal);

        let scope_b = first_page
            .iter()
            .find(|item| item.principal == "scope-b")
            .unwrap();
        for attempt in 0..ATTENTION_EMBEDDING_BIND_MAX_ATTEMPTS {
            if attempt > 0 {
                let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
                conn.execute(
                    "UPDATE attention_embedding_bind_work \
                     SET status = 'in_flight', lease_owner = 'test-worker', lease_expires_at = ? \
                     WHERE outcome_id = ?",
                    params![i64::MAX, scope_b.outcome_id],
                )
                .unwrap();
            }
            store
                .fail_claimed_embedding_bind(
                    &scope_b.outcome_id,
                    "test-worker",
                    "invalid_semantic_snapshot",
                )
                .await
                .unwrap();
        }
        let counts = store
            .embedding_bind_queue_counts("scope-b", "default")
            .await
            .unwrap();
        assert_eq!(counts.pending, 0);
        assert_eq!(counts.retry, 0);
        assert_eq!(counts.dead, 1);
        assert_eq!(
            counts.error_counts.get("invalid_semantic_snapshot"),
            Some(&1)
        );
        assert_eq!(
            store
                .embedding_bind_status_for_outcome(&scope_b.outcome_id)
                .await
                .unwrap()
                .as_deref(),
            Some("dead")
        );
        assert!(store
            .list_pending_embedding_binds(10)
            .await
            .unwrap()
            .iter()
            .all(|item| item.outcome_id != scope_b.outcome_id));

        let scope_a = first_page
            .iter()
            .find(|item| item.principal == "scope-a")
            .unwrap();
        for attempt in 0..=ATTENTION_EMBEDDING_BIND_MAX_ATTEMPTS {
            if attempt > 0 {
                let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
                conn.execute(
                    "UPDATE attention_embedding_bind_work \
                     SET status = 'in_flight', lease_owner = 'test-worker', lease_expires_at = ? \
                     WHERE outcome_id = ?",
                    params![i64::MAX, scope_a.outcome_id],
                )
                .unwrap();
            }
            store
                .fail_claimed_embedding_bind(
                    &scope_a.outcome_id,
                    "test-worker",
                    "embedding_unavailable",
                )
                .await
                .unwrap();
        }
        assert_eq!(
            store
                .embedding_bind_status_for_outcome(&scope_a.outcome_id)
                .await
                .unwrap()
                .as_deref(),
            Some("retry"),
            "temporary provider outages must remain recoverable after the bounded fast retries"
        );
    }

    #[tokio::test]
    async fn event_id_collision_with_a_different_payload_is_rejected() {
        let store = store();
        store
            .record_outcome(
                "owner",
                "default",
                AttentionSurface::FollowUp,
                &request("event-1", AttentionOutcomeKind::Useful),
                None,
            )
            .await
            .unwrap();
        let collision = store
            .record_outcome(
                "owner",
                "default",
                AttentionSurface::FollowUp,
                &request("event-1", AttentionOutcomeKind::Irrelevant),
                None,
            )
            .await;
        assert!(collision.is_err());
    }

    #[tokio::test]
    async fn embeddings_and_scores_are_isolated_by_scope_and_surface() {
        let store = store();
        let candidate = request("event", AttentionOutcomeKind::Useful).candidate;
        let embedding = SemanticEmbedding {
            contract: "contract-v1".to_string(),
            vector: vec![1.0, 0.0],
        };
        store
            .upsert_embedding(
                "owner",
                "default",
                AttentionSurface::FollowUp,
                &candidate,
                &embedding,
                1,
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .count_embedded_candidates(
                    "owner",
                    "default",
                    AttentionSurface::FollowUp,
                    &[candidate.candidate_id.clone()],
                )
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .count_embedded_candidates(
                    "other",
                    "default",
                    AttentionSurface::FollowUp,
                    &[candidate.candidate_id],
                )
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn score_wave_uses_one_writer_ticket_and_commits_every_candidate() {
        let store = store();
        let first = request("score-a", AttentionOutcomeKind::Useful).candidate;
        let second = SemanticAttentionCandidate {
            candidate_id: "candidate-2".to_string(),
            source_revision: Some("revision-2".to_string()),
            ..first.clone()
        };
        let probability = crate::magician_v2::attention::learning::model::BayesianProbability {
            probability: 0.8,
            evidence_weight: 2.0,
            neighbor_count: 2,
        };
        let estimate = BayesianKnnEstimate {
            usefulness: probability,
            actionability: probability,
        };
        let candidate_keys = vec![
            (first.candidate_id.clone(), first.source_revision.clone()),
            (second.candidate_id.clone(), second.source_revision.clone()),
        ];
        let tickets_before = store.conn.issued_ticket_count();

        assert!(store
            .upsert_scores(
                "owner",
                "default",
                AttentionSurface::FollowUp,
                vec![(first, estimate), (second, estimate)],
                "contract-v1",
                10,
            )
            .await
            .unwrap());
        assert_eq!(store.conn.issued_ticket_count(), tickets_before + 1);
        assert_eq!(
            store
                .list_scores(
                    "owner",
                    "default",
                    AttentionSurface::FollowUp,
                    &candidate_keys,
                )
                .await
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn actionability_wave_validates_before_the_single_atomic_write() {
        let store = store();
        let valid = ActionabilityInference {
            probability: 0.8,
            explanation: ActionabilityExplanation {
                code: "direct_request".to_string(),
                label: "Direct request for you".to_string(),
            },
            model_version: "model-v1".to_string(),
            snapshot_id: "snapshot-v1".to_string(),
            input_digest: "digest-a".to_string(),
        };
        let invalid = ActionabilityInference {
            probability: f64::NAN,
            ..valid.clone()
        };
        let tickets_before = store.conn.issued_ticket_count();
        assert!(store
            .upsert_actionability_scores(
                "owner",
                "default",
                AttentionSurface::FollowUp,
                vec![
                    ("candidate-1".to_string(), None, valid.clone()),
                    ("candidate-2".to_string(), None, invalid),
                ],
                10,
            )
            .await
            .is_err());
        assert_eq!(store.conn.issued_ticket_count(), tickets_before);

        store
            .upsert_actionability_scores(
                "owner",
                "default",
                AttentionSurface::FollowUp,
                vec![
                    ("candidate-1".to_string(), None, valid.clone()),
                    ("candidate-2".to_string(), None, valid),
                ],
                11,
            )
            .await
            .unwrap();
        assert_eq!(store.conn.issued_ticket_count(), tickets_before + 1);
    }

    #[tokio::test]
    async fn cached_actionability_scores_require_the_exact_source_revision_and_input_digest() {
        let store = store();
        let inference = ActionabilityInference {
            probability: 0.8,
            explanation: ActionabilityExplanation {
                code: "direct_request".to_string(),
                label: "Direct request for you".to_string(),
            },
            model_version: "model-v1".to_string(),
            snapshot_id: "snapshot-v1".to_string(),
            input_digest: "digest-a".to_string(),
        };
        store
            .upsert_actionability_score(
                "owner",
                "default",
                AttentionSurface::FollowUp,
                "candidate-1",
                Some("distill:7"),
                &inference,
                1,
            )
            .await
            .unwrap();

        let exact = store
            .list_actionability_scores(
                "owner",
                "default",
                AttentionSurface::FollowUp,
                "snapshot-v1",
                &[(
                    "candidate-1".to_string(),
                    Some("distill:7".to_string()),
                    "digest-a".to_string(),
                )],
            )
            .await
            .unwrap();
        assert!(exact.contains_key("candidate-1"));

        let stale = store
            .list_actionability_scores(
                "owner",
                "default",
                AttentionSurface::FollowUp,
                "snapshot-v1",
                &[(
                    "candidate-1".to_string(),
                    Some("distill:8".to_string()),
                    "digest-a".to_string(),
                )],
            )
            .await
            .unwrap();
        assert!(stale.is_empty());

        let changed_input = store
            .list_actionability_scores(
                "owner",
                "default",
                AttentionSurface::FollowUp,
                "snapshot-v1",
                &[(
                    "candidate-1".to_string(),
                    Some("distill:7".to_string()),
                    "digest-b".to_string(),
                )],
            )
            .await
            .unwrap();
        assert!(changed_input.is_empty());
    }

    #[tokio::test]
    async fn legacy_actionability_cache_rows_migrate_to_a_stale_sentinel() {
        let directory = tempfile::TempDir::new().unwrap();
        let database = directory.path().join("attention_learning.db");
        let conn = Connection::open(&database).unwrap();
        conn.execute_batch(
            "CREATE TABLE attention_actionability_scores (principal TEXT NOT NULL, workspace TEXT NOT NULL, surface TEXT NOT NULL, candidate_id TEXT NOT NULL, source_revision TEXT, snapshot_id TEXT NOT NULL, model_version TEXT NOT NULL, probability REAL NOT NULL, explanation_code TEXT NOT NULL, explanation_label TEXT NOT NULL, updated_at INTEGER NOT NULL, PRIMARY KEY(principal, workspace, surface, candidate_id, snapshot_id));
             INSERT INTO attention_actionability_scores VALUES ('owner','default','follow_up','candidate-1','distill:7','snapshot-v1','model-v1',0.8,'direct_request','Direct request for you',1);",
        )
        .unwrap();
        drop(conn);

        let store = AttentionLearningStore::open(directory.path()).unwrap();
        let cached = store
            .list_actionability_scores(
                "owner",
                "default",
                AttentionSurface::FollowUp,
                "snapshot-v1",
                &[(
                    "candidate-1".to_string(),
                    Some("distill:7".to_string()),
                    "real-digest".to_string(),
                )],
            )
            .await
            .unwrap();
        assert!(cached.is_empty());
    }

    #[tokio::test]
    async fn legacy_embedding_bind_rows_migrate_and_poison_surfaces_are_quarantined() {
        let directory = tempfile::TempDir::new().unwrap();
        let database = directory.path().join("attention_learning.db");
        let conn = Connection::open(&database).unwrap();
        conn.execute_batch(
            "CREATE TABLE attention_embedding_bind_work ( \
                outcome_id TEXT NOT NULL PRIMARY KEY, principal TEXT NOT NULL, \
                workspace TEXT NOT NULL, surface TEXT NOT NULL, candidate_id TEXT NOT NULL, \
                source_revision TEXT, semantic_text TEXT NOT NULL, created_at INTEGER NOT NULL, \
                updated_at INTEGER NOT NULL \
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO attention_embedding_bind_work ( \
                outcome_id, principal, workspace, surface, candidate_id, semantic_text, \
                created_at, updated_at \
             ) VALUES ('poison', 'owner', 'default', 'unknown', 'candidate', 'brief', 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO attention_embedding_bind_work ( \
                outcome_id, principal, workspace, surface, candidate_id, semantic_text, \
                created_at, updated_at \
             ) VALUES ('healthy', 'other', 'default', 'follow_up', 'candidate', 'brief', 2, 2)",
            [],
        )
        .unwrap();
        drop(conn);

        let store = AttentionLearningStore::open(directory.path()).unwrap();
        {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            let mut statement = conn
                .prepare("PRAGMA table_info(attention_embedding_bind_work)")
                .unwrap();
            let columns = statement
                .query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .collect::<std::result::Result<HashSet<_>, _>>()
                .unwrap();
            for expected in [
                "status",
                "attempts",
                "next_retry_at",
                "lease_owner",
                "lease_expires_at",
                "last_error_code",
                "last_attempt_at",
            ] {
                assert!(columns.contains(expected));
            }
        }
        let claimed = store
            .claim_pending_embedding_binds(10, "migration-worker", 300)
            .await
            .unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].outcome_id, "healthy");
        assert_eq!(
            store
                .embedding_bind_status_for_outcome("poison")
                .await
                .unwrap()
                .as_deref(),
            Some("dead")
        );
        let counts = store
            .embedding_bind_queue_counts("owner", "default")
            .await
            .unwrap();
        assert_eq!(counts.dead, 1);
        assert_eq!(counts.error_counts.get("invalid_surface"), Some(&1));
    }

    #[tokio::test]
    async fn pair_labels_are_unordered_revision_bound_and_event_idempotent() {
        let store = store();
        let left = AttentionPairCandidateRef {
            candidate_id: "candidate-a".to_string(),
            source_revision: Some("revision-a".to_string()),
        };
        let right = AttentionPairCandidateRef {
            candidate_id: "candidate-b".to_string(),
            source_revision: Some("revision-b".to_string()),
        };
        let request = RecordAttentionPairLabel {
            event_id: "pair-event-1".to_string(),
            surface: AttentionSurface::FollowUp,
            left: right.clone(),
            right: left.clone(),
            label: AttentionPairLabelKind::NotDuplicate,
            source: AttentionPairLabelSource::Owner,
            label_quality: AttentionLabelQuality::Strong,
            confidence: 1.0,
            occurred_at: 10,
        };
        let first = store
            .record_pair_label("owner", "default", &request)
            .await
            .unwrap();
        let replay = store
            .record_pair_label("owner", "default", &request)
            .await
            .unwrap();
        assert!(first.inserted);
        assert!(!replay.inserted);
        assert_eq!(first.pair_label_id, replay.pair_label_id);
        assert_eq!(first.left, left);
        assert_eq!(first.right, right);

        let evidence = store
            .list_pair_evidence("owner", "default", AttentionSurface::FollowUp)
            .await
            .unwrap();
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].label, AttentionPairLabelKind::NotDuplicate);

        let mut collision = request;
        collision.label = AttentionPairLabelKind::SameUnderlyingItem;
        assert!(store
            .record_pair_label("owner", "default", &collision)
            .await
            .is_err());
    }

    async fn schedule_rank_job(
        store: &AttentionLearningStore,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
        event_id: &str,
    ) -> AttentionRankRecomputeJob {
        let request = request(event_id, AttentionOutcomeKind::Useful);
        let persisted = store
            .record_outcome(principal, workspace, surface, &request, None)
            .await
            .unwrap();
        store
            .schedule_rank_recompute(
                principal,
                workspace,
                &ScheduleAttentionRankRecompute {
                    outcome_id: persisted.outcome_id,
                    origin_surface: surface,
                    canonical_candidate_id: format!(
                        "{}:{}",
                        surface.as_str(),
                        request.candidate.candidate_id
                    ),
                    raw_candidate_id: request.candidate.candidate_id,
                    source_revision: request.candidate.source_revision,
                    outcome: request.outcome,
                    decision_id: None,
                    delivery_id: None,
                    impression_id: None,
                    affected_rank_before: None,
                    enqueue_policy_snapshot_id: None,
                    enqueue_posterior_version: None,
                },
                10,
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn rank_recompute_enqueue_never_exposes_an_unvalidated_delivery_claim() {
        let store = store();
        let request = request("event-invalid-delivery", AttentionOutcomeKind::Useful);
        let persisted = store
            .record_outcome(
                "owner",
                "default",
                AttentionSurface::FollowUp,
                &request,
                None,
            )
            .await
            .unwrap();

        let job = store
            .schedule_rank_recompute(
                "owner",
                "default",
                &ScheduleAttentionRankRecompute {
                    outcome_id: persisted.outcome_id.clone(),
                    origin_surface: AttentionSurface::FollowUp,
                    canonical_candidate_id: "follow_up:candidate-1".to_string(),
                    raw_candidate_id: "candidate-1".to_string(),
                    source_revision: Some("revision-1".to_string()),
                    outcome: AttentionOutcomeKind::Useful,
                    decision_id: Some("unvalidated-decision".to_string()),
                    delivery_id: Some("forged-delivery".to_string()),
                    impression_id: Some("forged-impression".to_string()),
                    affected_rank_before: None,
                    enqueue_policy_snapshot_id: None,
                    enqueue_posterior_version: None,
                },
                10,
            )
            .await
            .unwrap();

        assert_eq!(job.outcome_id, persisted.outcome_id);
        assert_eq!(job.status, AttentionRankRecomputeStatus::Pending);
        assert!(job.decision_id.is_none());
        assert!(job.delivery_id.is_none());
        assert!(job.impression_id.is_none());
    }

    #[tokio::test]
    async fn rank_recompute_reconciliation_drops_raw_unvalidated_outcome_attribution() {
        let store = store();
        let mut request = request(
            "event-invalid-reconciled-attribution",
            AttentionOutcomeKind::Useful,
        );
        request.attribution = Some(AttentionOutcomeAttribution {
            decision_id: "forged-decision".to_string(),
            candidate_id: "candidate-1".to_string(),
            source_revision: Some("revision-1".to_string()),
            impression_id: Some("forged-impression".to_string()),
            delivery_id: Some("forged-delivery".to_string()),
        });
        let persisted = store
            .record_outcome(
                "owner",
                "default",
                AttentionSurface::FollowUp,
                &request,
                None,
            )
            .await
            .expect("capture remains independent of attribution validity");

        assert_eq!(
            store
                .schedule_missing_rank_recompute_jobs("owner", "default", 10, 20, true)
                .await
                .expect("reconcile missing rank job"),
            1
        );
        let attribution: (Option<String>, Option<String>, Option<String>) = {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.query_row(
                "SELECT decision_id, delivery_id, impression_id \
                 FROM attention_rank_recompute_jobs WHERE outcome_id = ?",
                params![persisted.outcome_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("reconciled job")
        };
        assert_eq!(attribution, (None, None, None));
    }

    #[tokio::test]
    async fn rank_recompute_reconciliation_rejects_a_decision_for_another_candidate() {
        let store = store();
        let persisted = store
            .record_outcome(
                "owner",
                "default",
                AttentionSurface::FollowUp,
                &request(
                    "event-wrong-candidate-decision",
                    AttentionOutcomeKind::Useful,
                ),
                None,
            )
            .await
            .expect("record outcome");
        {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.execute(
                "INSERT INTO attention_bandit_policy_snapshots \
                    (snapshot_id, snapshot_json, content_digest, created_at) \
                 VALUES ('snapshot-wrong-candidate', '{}', 'digest', 1)",
                [],
            )
            .expect("insert snapshot anchor");
            conn.execute(
                "INSERT INTO attention_delivery_decisions \
                    (decision_id, schema_version, principal, workspace, lane, projection_id, \
                     universe_digest, policy_snapshot_json, posterior_version, seed_identity, \
                     universe_size, page_size, status, health_json, min_visible_ms, \
                     visibility_rule_version, context_json, decision_json, created_at, expires_at) \
                 VALUES ('decision-wrong-candidate', 1, 'owner', 'default', 'follow_up', \
                         'projection', 'universe', '{}', 0, 'seed', 1, 1, 'active', '{}', 1, \
                         'v1', '{}', '{}', 1, 1000)",
                [],
            )
            .expect("insert decision");
            conn.execute(
                "INSERT INTO attention_delivery_decision_items \
                    (decision_id, position, candidate_id, source_revision, \
                     root_policy_propensity, exposure_token, item_json, attribution_item_json) \
                 VALUES ('decision-wrong-candidate', 1, 'other-candidate', 'revision-1', \
                         1.0, 'token', '{}', '{}')",
                [],
            )
            .expect("insert mismatched decision item");
            conn.execute(
                "INSERT INTO attention_bandit_updates \
                    (outcome_id, principal, workspace, surface, snapshot_id, decision_id, \
                     candidate_id, source_revision, attribution_quality, reward_strength, \
                     update_applied, occurred_at, created_at) \
                 VALUES (?, 'owner', 'default', 'follow_up', 'snapshot-wrong-candidate', \
                         'decision-wrong-candidate', 'candidate-1', 'revision-1', 'mismatch', \
                         1.0, 0, 1, 1)",
                params![persisted.outcome_id.as_str()],
            )
            .expect("insert degraded attribution update");
        }

        store
            .schedule_missing_rank_recompute_jobs("owner", "default", 10, 20, true)
            .await
            .expect("reconcile rank job");
        let decision_id: Option<String> = {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.query_row(
                "SELECT decision_id FROM attention_rank_recompute_jobs WHERE outcome_id = ?",
                params![persisted.outcome_id],
                |row| row.get(0),
            )
            .expect("reconciled job")
        };
        assert_eq!(decision_id, None);
    }

    #[tokio::test]
    async fn rank_recompute_reconciliation_recovers_a_verified_impression_binding() {
        let store = store();
        let persisted = store
            .record_outcome(
                "owner",
                "default",
                AttentionSurface::FollowUp,
                &request("event-verified-impression", AttentionOutcomeKind::Useful),
                None,
            )
            .await
            .expect("record outcome");
        {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.execute(
                "INSERT INTO attention_bandit_policy_snapshots \
                    (snapshot_id, snapshot_json, content_digest, created_at) \
                 VALUES ('snapshot-verified-impression', '{}', 'digest', 1)",
                [],
            )
            .expect("insert snapshot anchor");
            conn.execute(
                "INSERT INTO attention_impressions ( \
                    impression_id, schema_version, event_id, principal, workspace, decision_id, \
                    delivery_id, page_index, position, exposure_token, candidate_id, \
                    source_revision, cluster_id, surface, rank, first_visible_at, \
                    accumulated_visible_ms, visibility_rule_version, client_type, client_version, \
                    viewport_class, root_policy_propensity, conditional_delivery_propensity, \
                    verified, dedupe_count, last_recorded_at \
                 ) VALUES ( \
                    'impression-verified', 1, 'impression-event', 'owner', 'default', \
                    'decision-verified', 'delivery-verified', 0, 1, 'token-verified', \
                    'candidate-1', 'revision-1', 'cluster', 'follow_up', 1, 1, 1000, \
                    'v1', 'test', '1', 'wide', 1.0, 1.0, 1, 0, 1 \
                 )",
                [],
            )
            .expect("insert verified impression");
            conn.execute(
                "INSERT INTO attention_bandit_updates ( \
                    outcome_id, principal, workspace, surface, snapshot_id, decision_id, \
                    candidate_id, source_revision, impression_id, attribution_quality, \
                    reward_strength, update_applied, occurred_at, created_at \
                 ) VALUES (?, 'owner', 'default', 'follow_up', \
                    'snapshot-verified-impression', 'decision-verified', 'candidate-1', \
                    'revision-1', 'impression-verified', 'exact', 1.0, 1, 1, 1)",
                params![persisted.outcome_id.as_str()],
            )
            .expect("insert validated bandit update");
        }

        store
            .schedule_missing_rank_recompute_jobs("owner", "default", 10, 20, true)
            .await
            .expect("reconcile rank job");
        let attribution: (Option<String>, Option<String>, Option<String>) = {
            let conn = store.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.query_row(
                "SELECT decision_id, delivery_id, impression_id \
                 FROM attention_rank_recompute_jobs WHERE outcome_id = ?",
                params![persisted.outcome_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("reconciled job")
        };
        assert_eq!(
            attribution,
            (
                Some("decision-verified".to_string()),
                Some("delivery-verified".to_string()),
                Some("impression-verified".to_string()),
            )
        );
    }

    #[tokio::test]
    async fn global_rank_recompute_lease_drains_jobs_from_distinct_scopes() {
        let store = store();
        schedule_rank_job(
            &store,
            "owner-a",
            "workspace-a",
            AttentionSurface::FollowUp,
            "event-a",
        )
        .await;
        schedule_rank_job(
            &store,
            "owner-b",
            "workspace-b",
            AttentionSurface::WorthALook,
            "event-b",
        )
        .await;

        let leased = store
            .lease_rank_recompute_jobs_globally("worker-1", 20, 120, 10)
            .await
            .unwrap();
        let scopes = leased
            .iter()
            .map(|job| (job.principal.as_str(), job.workspace.as_str()))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            scopes,
            std::collections::BTreeSet::from([
                ("owner-a", "workspace-a"),
                ("owner-b", "workspace-b"),
            ])
        );
        assert!(leased.iter().all(|job| {
            job.status == AttentionRankRecomputeStatus::InFlight
                && job.lease_owner.as_deref() == Some("worker-1")
        }));
    }

    #[tokio::test]
    async fn reconciliation_discovers_outcomes_without_jobs_across_scopes() {
        let store = store();
        for (principal, workspace, surface, event_id) in [
            (
                "owner-a",
                "workspace-a",
                AttentionSurface::FollowUp,
                "event-a",
            ),
            (
                "owner-b",
                "workspace-b",
                AttentionSurface::WorthALook,
                "event-b",
            ),
        ] {
            store
                .record_outcome(
                    principal,
                    workspace,
                    surface,
                    &request(event_id, AttentionOutcomeKind::Useful),
                    None,
                )
                .await
                .unwrap();
        }

        let scopes = store
            .rank_recompute_reconciliation_scopes(10)
            .await
            .unwrap();
        assert_eq!(
            scopes
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from([
                ("owner-a".to_string(), "workspace-a".to_string()),
                ("owner-b".to_string(), "workspace-b".to_string()),
            ])
        );
        assert_eq!(
            store
                .schedule_missing_rank_recompute_jobs("owner-a", "workspace-a", 10, 20, true,)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .rank_recompute_reconciliation_scopes(10)
                .await
                .unwrap(),
            vec![("owner-b".to_string(), "workspace-b".to_string())]
        );
    }

    #[tokio::test]
    async fn rank_recompute_terminal_commit_rejects_an_expired_lease_at_commit_time() {
        let store = store();
        schedule_rank_job(
            &store,
            "owner",
            "workspace",
            AttentionSurface::FollowUp,
            "event",
        )
        .await;
        let job = store
            .lease_rank_recompute_jobs_globally("worker-1", 20, 30, 1)
            .await
            .unwrap()
            .pop()
            .unwrap();
        let result = AttentionRankRecomputeResult {
            semantics: crate::magician_v2::attention::learning::rank_recompute::ATTENTION_RANK_RECOMPUTE_RESULT_SEMANTICS.to_string(),
            affected_rank_after: 1,
            affected_rank_delta: None,
            current_source_revision: job.source_revision.clone(),
            universe_digest: "universe-1".to_string(),
            recompute_generation: crate::magician_v2::attention::learning::rank_recompute::AttentionRankRecomputeGeneration {
                follow_up: 1,
                worth_a_look: 2,
            },
            policy_snapshot_id: None,
            posterior_version: None,
            completed_at: 30,
        };
        assert!(store
            .finish_rank_recompute_job(&job.job_id, "worker-1", &result, 30)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn rank_recompute_heartbeat_extends_only_the_current_live_lease() {
        let store = store();
        schedule_rank_job(
            &store,
            "owner",
            "workspace",
            AttentionSurface::FollowUp,
            "heartbeat-event",
        )
        .await;
        let job = store
            .lease_rank_recompute_jobs_globally("worker-1", 20, 30, 1)
            .await
            .unwrap()
            .pop()
            .unwrap();

        assert!(store
            .renew_rank_recompute_job_lease(&job.job_id, "worker-1", 25, 50)
            .await
            .unwrap());
        assert!(!store
            .renew_rank_recompute_job_lease(&job.job_id, "worker-2", 26, 60)
            .await
            .unwrap());

        let result = AttentionRankRecomputeResult {
            semantics: crate::magician_v2::attention::learning::rank_recompute::ATTENTION_RANK_RECOMPUTE_RESULT_SEMANTICS.to_string(),
            affected_rank_after: 1,
            affected_rank_delta: None,
            current_source_revision: job.source_revision.clone(),
            universe_digest: "universe-heartbeat".to_string(),
            recompute_generation: crate::magician_v2::attention::learning::rank_recompute::AttentionRankRecomputeGeneration {
                follow_up: 1,
                worth_a_look: 2,
            },
            policy_snapshot_id: None,
            posterior_version: None,
            completed_at: 40,
        };
        store
            .finish_rank_recompute_job(&job.job_id, "worker-1", &result, 40)
            .await
            .expect("renewed lease remains valid past its original expiry");
        assert!(!store
            .renew_rank_recompute_job_lease(&job.job_id, "worker-1", 41, 70)
            .await
            .unwrap());
    }
}
