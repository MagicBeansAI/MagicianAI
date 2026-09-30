use std::{
    collections::{HashMap, VecDeque},
    fs::{File, OpenOptions},
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use duckdb::{params, params_from_iter, types::Value as DuckValue, Connection};
use fs2::FileExt;
use magician::magician_v2::storage_governance::database_maintenance::{
    configure_connection, inspect_fragmentation, DatabaseGate, DatabaseMaintenanceConfig,
    DatabasePermit, DatabaseReadConnection, Fragmentation,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

mod distill_queue;
pub use distill_queue::{DistillQueueCounts, DISTILL_HISTORY_BATCH};

use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

use super::types::{
    ChannelDetailStatus, ChannelFollowUpHint, ChannelInformationBrief, ChannelLane, DistillState,
    MailAnnotationState, MailAssistActor, MailAssistEvent, MailAssistEventType,
    MailAssistUserFeedback, MailFeedbackVerdict, MailMessageMeta, MailRecordOrigin,
    MailThreadAnnotation, MailThreadRecord, MessageDirection, ProviderThreadChange,
    ProviderThreadChangeKind, SyncWatermark, CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION,
    MAIL_ASSIST_SCHEMA_VERSION,
};

/// Bootstrap DDL — kept in sync with the seed template at
/// `magician_data_v3/system/db_templates/mail_assist/schema.sql` (the feed
/// store follows the same duplication). Everything is `IF NOT EXISTS` so
/// re-running at scope materialization is idempotent; additive schema
/// changes go through [`apply_schema_migrations`] keyed on the version row
/// in `mail_assist_meta`.
const BOOTSTRAP_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS mail_threads (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    provider TEXT NOT NULL,
    account_alias TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    account_email TEXT NULL,
    lane TEXT NOT NULL DEFAULT 'user_assist',
    subject TEXT NULL,
    latest_summary TEXT NULL,
    latest_from_name TEXT NULL,
    latest_from_address TEXT NULL,
    recipient_domains_json JSON NOT NULL,
    label_ids_json JSON NOT NULL,
    message_count BIGINT NOT NULL,
    last_message_at BIGINT NULL,
    provider_cursor TEXT NULL,
    sensitive_suppressed BOOLEAN NOT NULL,
    origin TEXT NOT NULL,
    first_observed_at BIGINT NOT NULL,
    last_observed_at BIGINT NOT NULL,
    schema_version INTEGER NOT NULL,
    PRIMARY KEY (principal, workspace, provider, account_alias, thread_id)
);
CREATE INDEX IF NOT EXISTS idx_mail_threads_scope_last_message
    ON mail_threads (principal, workspace, provider, account_alias, last_message_at DESC);
CREATE TABLE IF NOT EXISTS mail_messages (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    provider TEXT NOT NULL,
    account_alias TEXT NOT NULL,
    message_id TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    account_email TEXT NULL,
    provider_cursor TEXT NULL,
    label_ids_json JSON NOT NULL,
    subject TEXT NULL,
    from_name TEXT NULL,
    from_address TEXT NULL,
    to_domains_json JSON NOT NULL,
    cc_domains_json JSON NOT NULL,
    internal_date BIGINT NOT NULL,
    observed_at BIGINT NOT NULL,
    direction TEXT NULL,
    summary TEXT NULL,
    intent TEXT NULL,
    needs_reply_hint BOOLEAN NOT NULL DEFAULT FALSE,
    follow_up_hint_json JSON NULL,
    distill_evidence_message_ids_json JSON NULL,
    distill_brief_json JSON NULL,
    distill_contract_version INTEGER NULL,
    distilled_at BIGINT NULL,
    distill_revision BIGINT NULL,
    distill_backfill_attempts INTEGER NOT NULL DEFAULT 0,
    distill_backfill_next_retry_at BIGINT NULL,
    distill_backfill_last_error TEXT NULL,
    distill_state TEXT NOT NULL DEFAULT 'pending',
    distill_attempts INTEGER NOT NULL DEFAULT 0,
    classify_attempts INTEGER NOT NULL DEFAULT 0,
    classify_next_retry_at BIGINT NULL,
    classify_last_error TEXT NULL,
    classify_failed_at BIGINT NULL,
    sensitive_suppressed BOOLEAN NOT NULL,
    origin TEXT NOT NULL,
    schema_version INTEGER NOT NULL,
    PRIMARY KEY (principal, workspace, provider, account_alias, message_id)
);
CREATE INDEX IF NOT EXISTS idx_mail_messages_scope_thread
    ON mail_messages (principal, workspace, provider, account_alias, thread_id,
                      internal_date DESC);
CREATE INDEX IF NOT EXISTS idx_mail_messages_reconcile
    ON mail_messages (principal, workspace, provider, account_alias, thread_id,
                      distill_state, sensitive_suppressed, internal_date, message_id);
CREATE INDEX IF NOT EXISTS idx_mail_messages_distill_queue
    ON mail_messages (principal, workspace, distill_state, internal_date);
CREATE INDEX IF NOT EXISTS idx_mail_messages_distill_revision
    ON mail_messages (principal, workspace, distill_revision);
CREATE INDEX IF NOT EXISTS idx_mail_messages_distill_backfill
    ON mail_messages (principal, workspace, distill_state, sensitive_suppressed,
                      distill_contract_version, internal_date);
CREATE TABLE IF NOT EXISTS mail_distill_revision_counters (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    last_revision BIGINT NOT NULL,
    PRIMARY KEY (principal, workspace)
);
CREATE TABLE IF NOT EXISTS mail_annotations (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    id TEXT NOT NULL,
    provider TEXT NOT NULL,
    account_alias TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    lane TEXT NOT NULL DEFAULT 'user_assist',
    state TEXT NOT NULL,
    label TEXT NULL,
    confidence DOUBLE NULL,
    reason TEXT NULL,
    evidence_refs_json JSON NOT NULL,
    evidence_message_id TEXT NULL,
    evidence_message_at BIGINT NULL,
    classification_input_revision BIGINT NULL,
    semantic_features_json JSON NULL,
    proposed_action_json JSON NULL,
    attention_lane TEXT NOT NULL DEFAULT 'follow_up',
    provenance TEXT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    schema_version INTEGER NOT NULL,
    PRIMARY KEY (principal, workspace, id)
);
CREATE INDEX IF NOT EXISTS idx_mail_annotations_scope_thread
    ON mail_annotations (principal, workspace, provider, account_alias, thread_id,
                         updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_mail_annotations_reconcile
    ON mail_annotations (principal, workspace, state, updated_at, id);
CREATE INDEX IF NOT EXISTS idx_mail_annotations_today
    ON mail_annotations (principal, workspace, attention_lane, state,
                         created_at DESC, id DESC);
CREATE TABLE IF NOT EXISTS mail_assist_events (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    id TEXT NOT NULL,
    annotation_id TEXT NULL,
    provider TEXT NOT NULL,
    account_alias TEXT NOT NULL,
    thread_id TEXT NULL,
    event_type TEXT NOT NULL,
    actor TEXT NOT NULL,
    from_state TEXT NULL,
    to_state TEXT NULL,
    detail_json JSON NULL,
    created_at BIGINT NOT NULL,
    schema_version INTEGER NOT NULL,
    PRIMARY KEY (principal, workspace, id)
);
CREATE INDEX IF NOT EXISTS idx_mail_events_scope_annotation
    ON mail_assist_events (principal, workspace, annotation_id, created_at, id);
CREATE INDEX IF NOT EXISTS idx_mail_events_annotation_state
    ON mail_assist_events (principal, workspace, annotation_id, to_state, created_at);
CREATE TABLE IF NOT EXISTS mail_annotation_action_claims (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    annotation_id TEXT NOT NULL,
    action TEXT NOT NULL,
    claim_id TEXT NOT NULL,
    task_id TEXT NULL,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    schema_version INTEGER NOT NULL,
    PRIMARY KEY (principal, workspace, annotation_id, action)
);
CREATE TABLE IF NOT EXISTS mail_sync_watermarks (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    provider TEXT NOT NULL,
    account_alias TEXT NOT NULL,
    last_internal_date BIGINT NULL,
    provider_cursor TEXT NULL,
    last_synced_at BIGINT NOT NULL,
    last_error TEXT NULL,
    schema_version INTEGER NOT NULL,
    PRIMARY KEY (principal, workspace, provider, account_alias)
);
CREATE TABLE IF NOT EXISTS channel_writing_preferences (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    id TEXT NOT NULL,
    provider TEXT NOT NULL,
    account_alias TEXT NOT NULL,
    scope_kind TEXT NOT NULL,
    scope_value TEXT NOT NULL,
    statement TEXT NOT NULL,
    status TEXT NOT NULL,
    source_annotation_id TEXT NULL,
    evidence_count BIGINT NOT NULL DEFAULT 1,
    created_at BIGINT NOT NULL,
    updated_at BIGINT NOT NULL,
    schema_version INTEGER NOT NULL,
    PRIMARY KEY (principal, workspace, id),
    UNIQUE (principal, workspace, provider, account_alias, scope_kind, scope_value, statement)
);
CREATE INDEX IF NOT EXISTS idx_channel_writing_preferences_scope
    ON channel_writing_preferences (
        principal, workspace, provider, account_alias, scope_kind, scope_value, status
    );
CREATE TABLE IF NOT EXISTS channel_action_drafts (
    principal TEXT NOT NULL,
    workspace TEXT NOT NULL,
    compose_id TEXT NOT NULL,
    annotation_id TEXT NOT NULL,
    action_id TEXT NOT NULL,
    text TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    schema_version INTEGER NOT NULL,
    PRIMARY KEY (principal, workspace, compose_id)
);
CREATE INDEX IF NOT EXISTS idx_channel_action_drafts_annotation
    ON channel_action_drafts (principal, workspace, annotation_id, action_id, created_at DESC);
CREATE TABLE IF NOT EXISTS mail_assist_meta (
    meta_key TEXT NOT NULL,
    meta_value TEXT NOT NULL,
    PRIMARY KEY (meta_key)
);
"#;

/// Current on-disk schema version recorded in `mail_assist_meta`.
/// v3 adds persisted local follow-up hints on messages plus exact
/// message-evidence columns on annotations. v4 stores the complete message-id
/// batch that fed a coalesced distillation. v5 adds durable classifier retry
/// metadata. v6 adds per-annotation action claims for idempotent UI actions.
/// v7 adds the V2 safe brief and scope-local monotonic distill revision. v8
/// adds durable, non-destructive backfill retry state. v9 binds each
/// annotation classification to the exact distill revision it consumed. v10
/// adds per-sender/domain exact writing-preference candidates and promotions.
/// v11 materializes the Today attention lane on annotations and adds the
/// supporting annotation/event indexes. v12 adds the `channel_action_drafts`
/// scratch table backing the generic channel-action compose/commit API (a
/// composed draft the owner reviews before `commit` sends it). v13 stores the
/// independently validated, revision-bound Slice-2 semantic feature envelope.
/// Fresh bootstraps create the current shape directly; older stores migrate
/// additively.
const MAIL_ASSIST_DB_SCHEMA_VERSION: u32 = 13;
pub const CHANNEL_ASSIST_DB_SCHEMA_VERSION: u32 = MAIL_ASSIST_DB_SCHEMA_VERSION;

/// Approval claims cover a non-transactional side effect (task creation). Fresh
/// claims block duplicate clicks, but an uncompleted claim must not wedge the
/// annotation forever if the process dies between claiming and completion.
const ANNOTATION_ACTION_CLAIM_STALE_MS: i64 = 10 * 60 * 1000;

/// Same coalescing rationale as `FeedStore::CHECKPOINT_THROTTLE`: sync
/// passes can append hundreds of message rows per cycle; per-write
/// CHECKPOINTs bloat and eventually corrupt the DuckDB file. Writes stay
/// durable in the WAL between checkpoints.
const CHECKPOINT_THROTTLE: Duration = Duration::from_secs(30);
const MAX_CLASSIFY_ATTEMPTS: i64 = 3;
const CLASSIFY_ERROR_MAX_CHARS: usize = 512;
const CLASSIFY_RETRY_BACKOFF_MS: [i64; 2] = [5 * 60 * 1000, 30 * 60 * 1000];
const DISTILL_BACKFILL_ERROR_MAX_CHARS: usize = 512;
const DISTILL_BACKFILL_RETRY_BACKOFF_MS: [i64; 4] = [
    5 * 60 * 1000,
    30 * 60 * 1000,
    2 * 60 * 60 * 1000,
    6 * 60 * 60 * 1000,
];

fn current_epoch_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// Shared SELECT lists — every reader of a table uses ONE column order so
/// the positional `map_*_row` decoders can't drift between queries. New
/// columns append at the end.
const THREAD_COLUMNS: &str = "provider, account_alias, thread_id, account_email, \
     subject, latest_from_name, latest_from_address, recipient_domains_json, \
     label_ids_json, message_count, last_message_at, provider_cursor, \
     sensitive_suppressed, origin, first_observed_at, last_observed_at, \
     schema_version, lane, latest_summary";
const MESSAGE_COLUMNS: &str = "provider, account_alias, message_id, thread_id, \
     account_email, provider_cursor, label_ids_json, subject, from_name, \
     from_address, to_domains_json, cc_domains_json, internal_date, observed_at, \
     sensitive_suppressed, origin, schema_version, direction, summary, intent, \
     needs_reply_hint, follow_up_hint_json, distill_state, distill_attempts, \
     distill_brief_json, distill_contract_version, distilled_at, distill_revision";
const ANNOTATION_COLUMNS: &str = "id, provider, account_alias, thread_id, state, \
     label, confidence, reason, evidence_refs_json, proposed_action_json, \
     provenance, created_at, updated_at, schema_version, lane, evidence_message_id, \
     evidence_message_at, classification_input_revision, semantic_features_json";

/// One entry in the in-memory "live distillation" feed: the message that was
/// just distilled (input identity — metadata only, no raw body is ever kept)
/// paired with the local model's derived output. Powers `/observe/stats`'s
/// realtime input→output view. Deliberately NOT persisted: it's a rolling
/// window of recent activity, not history (which the store already holds as
/// `summary`/`intent`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentDistillEntry {
    /// Scope the distillation ran in — used to filter the feed; not sent to
    /// the client.
    #[serde(skip)]
    pub principal: String,
    #[serde(skip)]
    pub workspace: String,
    pub provider: String,
    pub account_alias: String,
    pub thread_id: String,
    pub message_id: String,
    // ── input identity (metadata only) ──
    pub subject: Option<String>,
    pub from_name: Option<String>,
    pub from_address: Option<String>,
    /// When the message ARRIVED (epoch ms, from `internal_date`) — the real
    /// received time, distinct from `at_ms` (when distillation ran).
    pub received_at: i64,
    // ── output (the local model's derived result) ──
    pub summary: String,
    pub intent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brief_contract_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail_status: Option<ChannelDetailStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distill_revision: Option<i64>,
    /// Wall-clock ms when the distillation completed (client renders relative).
    pub at_ms: i64,
    /// End-to-end distill latency for this message, if measured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, serde::Serialize, PartialEq, Eq)]
pub struct DistillBackfillCounts {
    pub total: u64,
    pub ready: u64,
    pub cooling: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DistillBackfillRuntimeState {
    pub paused: bool,
    pub manual_requests: u64,
    pub manual_completed: u64,
    pub runs: u64,
    pub selected: u64,
    pub distilled: u64,
    pub failed: u64,
    pub yielded_pending: u64,
    pub yielded_dispatch_pressure: u64,
    pub last_run_at_ms: Option<i64>,
}

const DISTILL_BACKFILL_RUNTIME_META_KEY: &str = "distill_backfill_runtime_v1";
const MALFORMED_BRIDGE_ROW_METRIC_CAP: u64 = 10_000;
static MALFORMED_BRIDGE_ROWS: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy)]
enum DistillBackfillRuntimeMutation {
    Request,
    SetPaused(bool),
    Yield {
        dispatch_pressure: bool,
    },
    RecordRun {
        manual_generation: Option<u64>,
        selected: u64,
        distilled: u64,
        failed: u64,
        at_ms: i64,
    },
}

#[derive(Debug, Clone, Copy, Default, serde::Serialize, PartialEq, Eq)]
pub struct ChannelBriefCoverage {
    pub done: u64,
    pub v2: u64,
    pub legacy: u64,
    pub complete: u64,
    pub partial: u64,
    pub source_omits_details: u64,
}

pub type ChannelRecentDistillEntry = RecentDistillEntry;

/// Rolling window size for the live distillation feed (across all scopes).
const RECENT_DISTILL_CAP: usize = 50;

#[derive(Debug, Clone)]
pub struct MailAssistStore {
    base_root: PathBuf,
    workspace_layout: ArtifactV2Workspace,
    scoped: Arc<Mutex<HashMap<(String, String), Arc<MailAssistStoreInner>>>>,
    /// In-memory live distillation feed (newest first), shared across clones.
    recent_distill: Arc<Mutex<VecDeque<RecentDistillEntry>>>,
    maintenance: DatabaseMaintenanceConfig,
}

pub type ChannelAssistStore = MailAssistStore;

#[derive(Debug)]
struct MailAssistStoreInner {
    admission: Arc<DatabaseGate>,
    db_path: PathBuf,
    write_conn: Mutex<Connection>,
    /// Reader template on the SAME DuckDB instance as `write_conn`; each read
    /// clones a short-lived connection off it (see `read_connection`).
    read_conn: Mutex<Connection>,
    write_lock: Mutex<File>,
    last_checkpoint_at: Mutex<Instant>,
}

struct CrossProcessWriteGuard<'a> {
    _admission: Option<DatabasePermit>,
    file: std::sync::MutexGuard<'a, File>,
}

/// Per-account thread/message totals for `sync/status`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MailAccountCounts {
    pub provider: String,
    pub account_alias: String,
    pub thread_count: u64,
    pub message_count: u64,
}

pub type ChannelAccountCounts = MailAccountCounts;

/// A composed channel-action draft persisted between the generic
/// `action/{action_id}/compose` and `.../commit` API calls. The owner reviews
/// (and may edit) `text`; `commit` loads it by `compose_id` when the request
/// doesn't carry an edited body. Scoped, scratch-only — never provider content.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChannelActionDraftRow {
    pub compose_id: String,
    pub annotation_id: String,
    pub action_id: String,
    pub text: String,
    pub created_at: i64,
}

/// Optional feedback to record atomically with an annotation state transition.
#[derive(Debug, Clone)]
pub struct AnnotationTransitionFeedback {
    /// Shared idempotency key for the legacy audit event and canonical
    /// attention outcome. This makes the audit log a durable repair outbox.
    pub event_id: Option<String>,
    pub verdict: MailFeedbackVerdict,
    pub comment: Option<String>,
}

pub type ChannelAnnotationTransitionFeedback = AnnotationTransitionFeedback;

/// Result of an expected-state annotation transition.
#[derive(Debug, Clone)]
pub enum AnnotationTransitionResult {
    Applied(MailThreadAnnotation),
    NotFound,
    UnexpectedState {
        current: MailThreadAnnotation,
        expected: MailAnnotationState,
    },
    ActionInProgress {
        current: MailThreadAnnotation,
        action: String,
    },
}

pub type ChannelAnnotationTransitionResult = AnnotationTransitionResult;

/// Result of claiming an annotation action before performing side effects.
#[derive(Debug, Clone)]
pub enum AnnotationActionClaimResult {
    Claimed {
        annotation: MailThreadAnnotation,
        claim_id: String,
    },
    Existing {
        annotation: MailThreadAnnotation,
        task_id: Option<String>,
    },
    NotFound,
    UnexpectedState {
        current: MailThreadAnnotation,
        expected: MailAnnotationState,
    },
}

pub type ChannelAnnotationActionClaimResult = AnnotationActionClaimResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassificationAnnotationDisposition {
    Created,
    Reclassified,
    RefreshedNeedsApproval,
    PreservedLifecycle,
    PreservedDismissal,
}

#[derive(Debug, Clone)]
pub enum ClassificationAnnotationApplyResult {
    Applied {
        annotation: MailThreadAnnotation,
        disposition: ClassificationAnnotationDisposition,
    },
    /// The message was re-distilled or suppressed while its LLM call was in
    /// flight. No annotation fields were written.
    StaleInput { current_revision: Option<i64> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequiredActionAnnotationDisposition {
    Created,
    PromotedPassive,
    RefreshedNeedsApproval,
    PreservedLifecycle,
    PreservedDismissal,
}

#[derive(Debug, Clone)]
pub enum RequiredActionAnnotationResult {
    Applied {
        annotation: MailThreadAnnotation,
        disposition: RequiredActionAnnotationDisposition,
    },
    StaleInput {
        current_revision: Option<i64>,
    },
}

fn classification_disposition_key(
    disposition: ClassificationAnnotationDisposition,
) -> &'static str {
    match disposition {
        ClassificationAnnotationDisposition::Created => "created",
        ClassificationAnnotationDisposition::Reclassified => "reclassified",
        ClassificationAnnotationDisposition::RefreshedNeedsApproval => "refreshed_needs_approval",
        ClassificationAnnotationDisposition::PreservedLifecycle => "preserved_lifecycle",
        ClassificationAnnotationDisposition::PreservedDismissal => "preserved_dismissal",
    }
}

fn required_action_disposition_key(
    disposition: RequiredActionAnnotationDisposition,
) -> &'static str {
    match disposition {
        RequiredActionAnnotationDisposition::Created => "created",
        RequiredActionAnnotationDisposition::PromotedPassive => "promoted_passive",
        RequiredActionAnnotationDisposition::RefreshedNeedsApproval => "refreshed_needs_approval",
        RequiredActionAnnotationDisposition::PreservedLifecycle => "preserved_lifecycle",
        RequiredActionAnnotationDisposition::PreservedDismissal => "preserved_dismissal",
    }
}

fn annotation_action_revision(annotation: &MailThreadAnnotation) -> Option<i64> {
    annotation
        .proposed_action
        .as_ref()
        .and_then(serde_json::Value::as_object)
        .and_then(|action| action.get("distill_revision"))
        .and_then(serde_json::Value::as_i64)
}

fn annotation_action_bool(annotation: &MailThreadAnnotation, key: &str) -> Option<bool> {
    annotation
        .proposed_action
        .as_ref()
        .and_then(serde_json::Value::as_object)
        .and_then(|action| action.get(key))
        .and_then(serde_json::Value::as_bool)
}

fn update_mail_generation_bytes(digest: &mut blake3::Hasher, value: Option<&[u8]>) {
    match value {
        Some(value) => {
            digest.update(&[1]);
            digest.update(&(value.len() as u64).to_le_bytes());
            digest.update(value);
        },
        None => {
            digest.update(&[0]);
        },
    }
}

fn update_mail_generation_string(digest: &mut blake3::Hasher, value: Option<&str>) {
    update_mail_generation_bytes(digest, value.map(str::as_bytes));
}

fn update_mail_generation_i64(digest: &mut blake3::Hasher, value: Option<i64>) {
    match value {
        Some(value) => {
            digest.update(&[1]);
            digest.update(&value.to_le_bytes());
        },
        None => {
            digest.update(&[0]);
        },
    }
}

fn update_mail_generation_f64(digest: &mut blake3::Hasher, value: Option<f64>) {
    match value {
        Some(value) => {
            digest.update(&[1]);
            digest.update(&value.to_bits().to_le_bytes());
        },
        None => {
            digest.update(&[0]);
        },
    }
}

#[derive(Debug, Clone)]
struct AnnotationActionClaimRow {
    claim_id: String,
    task_id: Option<String>,
    updated_at: i64,
}

impl MailAssistStore {
    pub fn with_database_maintenance(mut self, config: DatabaseMaintenanceConfig) -> Self {
        self.maintenance = config;
        self
    }

    pub fn open(base_root: &Path) -> Result<Self> {
        Self::open_workspace(ArtifactV2Workspace::new(base_root))
    }

    pub fn open_workspace(workspace_layout: ArtifactV2Workspace) -> Result<Self> {
        let base_root = workspace_layout.base_root().to_path_buf();
        workspace_layout.ensure_root_sync().with_context(|| {
            format!(
                "creating mail assist workspace root: {}",
                base_root.display()
            )
        })?;
        Ok(Self {
            base_root,
            workspace_layout,
            scoped: Arc::new(Mutex::new(HashMap::new())),
            recent_distill: Arc::new(Mutex::new(VecDeque::with_capacity(RECENT_DISTILL_CAP))),
            maintenance: DatabaseMaintenanceConfig::default(),
        })
    }

    /// Push one just-distilled message onto the live feed (newest first),
    /// evicting the oldest past [`RECENT_DISTILL_CAP`]. Non-blocking and
    /// best-effort — a poisoned lock is ignored (the feed is observability,
    /// never correctness).
    pub fn record_recent_distill(&self, entry: RecentDistillEntry) {
        if let Ok(mut ring) = self.recent_distill.lock() {
            ring.push_front(entry);
            while ring.len() > RECENT_DISTILL_CAP {
                ring.pop_back();
            }
        }
    }

    /// The most recent distillations for a scope, newest first, capped at
    /// `limit`. Reads the in-memory ring — no DB round-trip.
    pub fn recent_distill(
        &self,
        principal: &str,
        workspace: &str,
        limit: usize,
    ) -> Vec<RecentDistillEntry> {
        let Ok(ring) = self.recent_distill.lock() else {
            return Vec::new();
        };
        ring.iter()
            .filter(|e| e.principal == principal && e.workspace == workspace)
            .take(limit)
            .cloned()
            .collect()
    }

    pub async fn distill_backfill_runtime(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<DistillBackfillRuntimeState> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            load_distill_backfill_runtime(&conn)
        })
        .await
        .context("mail assist distill_backfill_runtime task panicked")?
    }

    pub async fn request_distill_backfill(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<DistillBackfillRuntimeState> {
        self.mutate_distill_backfill_runtime(
            principal,
            workspace,
            DistillBackfillRuntimeMutation::Request,
        )
        .await
    }

    pub async fn set_distill_backfill_paused(
        &self,
        principal: &str,
        workspace: &str,
        paused: bool,
    ) -> Result<DistillBackfillRuntimeState> {
        self.mutate_distill_backfill_runtime(
            principal,
            workspace,
            DistillBackfillRuntimeMutation::SetPaused(paused),
        )
        .await
    }

    pub async fn record_distill_backfill_yield(
        &self,
        principal: &str,
        workspace: &str,
        dispatch_pressure: bool,
    ) -> Result<DistillBackfillRuntimeState> {
        self.mutate_distill_backfill_runtime(
            principal,
            workspace,
            DistillBackfillRuntimeMutation::Yield { dispatch_pressure },
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn record_distill_backfill_run(
        &self,
        principal: &str,
        workspace: &str,
        manual_generation: Option<u64>,
        selected: u64,
        distilled: u64,
        failed: u64,
        at_ms: i64,
    ) -> Result<DistillBackfillRuntimeState> {
        self.mutate_distill_backfill_runtime(
            principal,
            workspace,
            DistillBackfillRuntimeMutation::RecordRun {
                manual_generation,
                selected,
                distilled,
                failed,
                at_ms,
            },
        )
        .await
    }

    async fn mutate_distill_backfill_runtime(
        &self,
        principal: &str,
        workspace: &str,
        mutation: DistillBackfillRuntimeMutation,
    ) -> Result<DistillBackfillRuntimeState> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let _file_guard = inner.acquire_write_guard()?;
            let conn = inner
                .write_conn
                .lock()
                .expect("mail assist write connection mutex poisoned");
            let mut state = load_distill_backfill_runtime(&conn)?;
            match mutation {
                DistillBackfillRuntimeMutation::Request => {
                    state.manual_requests = state.manual_requests.saturating_add(1);
                },
                DistillBackfillRuntimeMutation::SetPaused(paused) => state.paused = paused,
                DistillBackfillRuntimeMutation::Yield { dispatch_pressure } => {
                    if dispatch_pressure {
                        state.yielded_dispatch_pressure =
                            state.yielded_dispatch_pressure.saturating_add(1);
                    } else {
                        state.yielded_pending = state.yielded_pending.saturating_add(1);
                    }
                },
                DistillBackfillRuntimeMutation::RecordRun {
                    manual_generation,
                    selected,
                    distilled,
                    failed,
                    at_ms,
                } => {
                    if let Some(generation) = manual_generation {
                        state.manual_completed = state.manual_completed.max(generation);
                    }
                    state.runs = state.runs.saturating_add(1);
                    state.selected = state.selected.saturating_add(selected);
                    state.distilled = state.distilled.saturating_add(distilled);
                    state.failed = state.failed.saturating_add(failed);
                    state.last_run_at_ms = Some(at_ms);
                },
            }
            persist_distill_backfill_runtime(&conn, &state)?;
            inner
                .maybe_checkpoint(&conn)
                .context("checkpointing distill backfill runtime state")?;
            Ok(state)
        })
        .await
        .context("mail assist mutate_distill_backfill_runtime task panicked")?
    }

    pub fn malformed_bridge_row_count() -> u64 {
        MALFORMED_BRIDGE_ROWS.load(Ordering::Relaxed)
    }

    pub async fn materialize_scope(&self, principal: &str, workspace: &str) -> Result<()> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let _ = store.scope_inner(&principal, &workspace)?;
            Ok(())
        })
        .await
        .context("mail assist materialize_scope task panicked")?
    }

    /// Physically rewrite a scope's mail-assist DuckDB without deleting any
    /// lifecycle, message, annotation, feedback, or reconciliation rows.
    /// Mail retention is intentionally provider/lifecycle-owned; storage
    /// maintenance must never guess that old mail metadata is disposable.
    pub async fn compact_scope(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<magician::magician_v2::storage_governance::DuckDbCompactionReport> {
        self.compact_scope_with_wait(principal, workspace, Duration::from_secs(30))
            .await
    }

    pub async fn inspect_maintenance(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Fragmentation> {
        let store = self.clone();
        let principal = principal.to_owned();
        let workspace = workspace.to_owned();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            inspect_fragmentation(
                &conn,
                std::fs::metadata(&inner.db_path)?.len(),
                &store.maintenance,
            )
        })
        .await?
    }

    pub async fn compact_scope_with_wait(
        &self,
        principal: &str,
        workspace: &str,
        wait: Duration,
    ) -> Result<magician::magician_v2::storage_governance::DuckDbCompactionReport> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let template_schema = std::fs::read_to_string(
                store
                    .workspace_layout
                    .channel_assist_db_template_schema_path(),
            )
            .context("reading channel-assist template for compaction")?;
            let _exclusive = inner.admission.maintain(wait)?;
            let _write_guard = inner.acquire_file_guard(None)?;
            // Block new read clones while the live DuckDB file is replaced.
            // `compact_open_database` reopens `write_conn` on the published
            // file, so the reader template must be cloned from that new
            // instance before any subsequent Follow-ups projection runs.
            let mut read_template = inner
                .read_conn
                .lock()
                .expect("mail assist read connection mutex poisoned");
            let mut connection = inner
                .write_conn
                .lock()
                .expect("mail assist write connection mutex poisoned");
            configure_connection(&connection, store.maintenance.maintenance_memory_mib)?;
            // Admission drained every cloned reader; close the template too so
            // no handle retains the old database instance across publication.
            let old_reader = std::mem::replace(&mut *read_template, Connection::open_in_memory()?);
            drop(old_reader);
            let compaction = magician::magician_v2::storage_governance::duckdb_compaction::compact_recoverable_database(
                &mut connection,
                &inner.db_path,
                |published| { configure_connection(published, store.maintenance.maintenance_memory_mib)?; initialize_mail_assist_schema(published, &template_schema) },
            );
            if compaction.as_ref().err().is_some_and(|error| error.is::<magician::magician_v2::storage_governance::duckdb_compaction::RecoveryRequired>()) {
                inner.admission.require_recovery();
                return compaction;
            }
            // Rebind even when compaction reports an error: late publication
            // failures restore/reopen the original database, and retaining the
            // pre-swap reader would still create a split-brain read snapshot.
            let restore_budget = configure_connection(&connection, store.maintenance.channel_memory_mib);
            let reader_rebind = connection
                .try_clone()
                .context("rebinding mail assist reader after database compaction");
            match reader_rebind {
                Ok(reader) => *read_template = reader,
                Err(rebind_error) => {
                    inner.admission.require_recovery();
                    if let Err(compaction_error) = compaction {
                        return Err(compaction_error.context(format!(
                            "mail assist reader rebind also failed: {rebind_error:#}"
                        )));
                    }
                    return Err(rebind_error);
                },
            }
            if let Err(error) = restore_budget {
                inner.admission.require_recovery();
                return Err(error);
            }
            let report = compaction?;
            *inner
                .last_checkpoint_at
                .lock()
                .expect("mail assist checkpoint timestamp mutex poisoned") = Instant::now();
            Ok(report)
        })
        .await
        .context("mail assist compaction task panicked")?
    }

    fn scope_inner(&self, principal: &str, workspace: &str) -> Result<Arc<MailAssistStoreInner>> {
        let key = (principal.to_string(), workspace.to_string());
        if let Some(existing) = self
            .scoped
            .lock()
            .expect("mail assist scoped store mutex poisoned")
            .get(&key)
            .cloned()
        {
            return Ok(existing);
        }

        let db_path = magician::magician_v2::database_owners::database_file_path(
            &self.workspace_layout,
            principal,
            workspace,
            magician::magician_v2::database_owners::DatabaseOwner::ChannelAssistDuckdb,
        );
        if let Some(data_dir) = db_path.parent() {
            self.workspace_layout
                .create_dir_all_path_sync(data_dir)
                .with_context(|| {
                    format!(
                        "creating scoped mail assist data dir: {}",
                        data_dir.display()
                    )
                })?;
        }
        let lock_path = self
            .workspace_layout
            .channel_assist_lock_path(principal, workspace);
        self.ensure_template_seeded()?;
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("opening mail assist lock file at {}", lock_path.display()))?;
        lock_file
            .lock_exclusive()
            .context("acquiring mail assist scope bootstrap lock")?;
        if let Some(existing) = self
            .scoped
            .lock()
            .expect("mail assist scoped store mutex poisoned")
            .get(&key)
            .cloned()
        {
            let _ = lock_file.unlock();
            return Ok(existing);
        }
        magician::magician_v2::storage_governance::duckdb_compaction::recover_interrupted_compaction(&db_path)?;
        let conn = Connection::open(&db_path)
            .with_context(|| format!("opening mail assist store at {}", db_path.display()))?;
        // Bootstrap template schemas live in the seed root, OUTSIDE the
        // runtime file provider root — read directly via `std::fs`, exactly
        // like `feed::store` / `ui_threads::store`.
        let template_schema = std::fs::read_to_string(
            self.workspace_layout
                .channel_assist_db_template_schema_path(),
        )
        .with_context(|| {
            format!(
                "reading mail assist template schema: {}",
                self.workspace_layout
                    .channel_assist_db_template_schema_path()
                    .display()
            )
        })?;
        configure_connection(&conn, self.maintenance.channel_memory_mib)?;
        initialize_mail_assist_schema(&conn, &template_schema)?;
        checkpoint_connection(&conn).context("checkpointing mail assist schema bootstrap")?;
        lock_file
            .unlock()
            .context("releasing mail assist scope bootstrap lock")?;
        // Reader template: a second connection onto the SAME DuckDB instance as
        // `write_conn`. Every read clones from this rather than from `write_conn`
        // so the read path never contends with an in-flight write for the write
        // mutex. See `read_connection` for why sharing the instance is both fast
        // and safe.
        let read_conn = conn
            .try_clone()
            .context("cloning mail assist read connection from the write instance")?;
        let inner = Arc::new(MailAssistStoreInner {
            admission: Arc::new(DatabaseGate::default()),
            db_path,
            write_conn: Mutex::new(conn),
            read_conn: Mutex::new(read_conn),
            write_lock: Mutex::new(lock_file),
            last_checkpoint_at: Mutex::new(Instant::now()),
        });
        self.scoped
            .lock()
            .expect("mail assist scoped store mutex poisoned")
            .insert(key, Arc::clone(&inner));
        Ok(inner)
    }

    fn ensure_template_seeded(&self) -> Result<()> {
        // A read-only seed (container/deployment) ships the schema; never
        // write into it. The repo seed carries
        // `system/db_templates/mail_assist/schema.sql` for that case.
        if self.workspace_layout.templates_are_read_only() {
            return Ok(());
        }
        let template_dir = self.workspace_layout.channel_assist_db_template_dir();
        std::fs::create_dir_all(&template_dir).with_context(|| {
            format!(
                "creating mail assist template dir: {}",
                template_dir.display()
            )
        })?;
        let schema_path = self
            .workspace_layout
            .channel_assist_db_template_schema_path();
        let exists = match std::fs::metadata(&schema_path) {
            Ok(_) => true,
            Err(error) if error.kind() == ErrorKind::NotFound => false,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "checking mail assist template schema: {}",
                        schema_path.display()
                    )
                });
            },
        };
        if !exists {
            std::fs::write(&schema_path, BOOTSTRAP_DDL).with_context(|| {
                format!(
                    "writing mail assist template schema: {}",
                    schema_path.display()
                )
            })?;
        }
        Ok(())
    }

    /// Insert or update the current-state row for a thread. On update the
    /// original `first_observed_at` is preserved (same idiom as the feed
    /// store preserving `created_at`), and a NULL incoming
    /// `latest_summary` never clobbers a distilled one (sync re-upserts
    /// carry no summary; only the distiller writes it).
    pub async fn upsert_thread(
        &self,
        principal: &str,
        workspace: &str,
        record: MailThreadRecord,
    ) -> Result<()> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let recipient_domains_json = serde_json::to_string(&record.recipient_domains)
                .context("serializing mail thread recipient domains")?;
            let label_ids_json = serde_json::to_string(&record.label_ids)
                .context("serializing mail thread label ids")?;
            let _write_guard = inner.acquire_write_guard()?;
            let conn = inner
                .write_conn
                .lock()
                .expect("mail assist write connection mutex poisoned");
            conn.execute(
                "INSERT INTO mail_threads (
                    principal, workspace, provider, account_alias, thread_id,
                    account_email, lane, subject, latest_summary, latest_from_name,
                    latest_from_address, recipient_domains_json, label_ids_json,
                    message_count, last_message_at, provider_cursor,
                    sensitive_suppressed, origin, first_observed_at,
                    last_observed_at, schema_version
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT (principal, workspace, provider, account_alias, thread_id)
                DO UPDATE SET
                    account_email = COALESCE(NULLIF(excluded.account_email, ''),
                                             mail_threads.account_email),
                    lane = excluded.lane,
                    subject = excluded.subject,
                    latest_summary = CASE
                        WHEN excluded.sensitive_suppressed THEN NULL
                        ELSE COALESCE(excluded.latest_summary,
                                      mail_threads.latest_summary)
                    END,
                    latest_from_name = excluded.latest_from_name,
                    latest_from_address = excluded.latest_from_address,
                    recipient_domains_json = excluded.recipient_domains_json,
                    label_ids_json = excluded.label_ids_json,
                    message_count = excluded.message_count,
                    last_message_at = excluded.last_message_at,
                    provider_cursor = excluded.provider_cursor,
                    sensitive_suppressed = excluded.sensitive_suppressed,
                    origin = excluded.origin,
                    first_observed_at = mail_threads.first_observed_at,
                    last_observed_at = excluded.last_observed_at,
                    schema_version = excluded.schema_version",
                params![
                    principal,
                    workspace,
                    record.provider,
                    record.account_alias,
                    record.thread_id,
                    record.account_email,
                    record.lane.as_db_str(),
                    record.subject,
                    record.latest_summary,
                    record.latest_from_name,
                    record.latest_from_address,
                    recipient_domains_json,
                    label_ids_json,
                    record.message_count,
                    record.last_message_at,
                    record.provider_cursor,
                    record.sensitive_suppressed,
                    record.origin.as_db_str(),
                    record.first_observed_at,
                    record.last_observed_at,
                    record.schema_version,
                ],
            )
            .context("upserting mail thread")?;
            inner
                .maybe_checkpoint(&conn)
                .context("throttled checkpoint after mail thread upsert")?;
            Ok(())
        })
        .await
        .context("mail assist upsert_thread task panicked")?
    }

    /// Reconcile the provider-resolved owner email across all durable rows for
    /// one account alias. Gmail calls this on every successful profile fetch,
    /// including empty incremental passes, so rows created by older builds with
    /// a NULL account email become correctly deep-linkable without a re-backfill.
    /// The operation is scoped and idempotent; a credential intentionally moved
    /// to a different mailbox updates the alias's rows to the new authoritative
    /// identity rather than retaining a stale browser route.
    pub async fn reconcile_account_email(
        &self,
        principal: &str,
        workspace: &str,
        provider: &str,
        account_alias: &str,
        account_email: &str,
    ) -> Result<usize> {
        let account_email = account_email.trim();
        if account_email.is_empty() {
            anyhow::bail!("account email cannot be empty");
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let provider = provider.to_string();
        let account_alias = account_alias.to_string();
        let account_email = account_email.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let _write_guard = inner.acquire_write_guard()?;
            let conn = inner
                .write_conn
                .lock()
                .expect("mail assist write connection mutex poisoned");
            let changed = with_transaction(&conn, || {
                let threads = conn.execute(
                    "UPDATE mail_threads SET account_email = ? \
                     WHERE principal = ? AND workspace = ? AND provider = ? \
                       AND account_alias = ? \
                       AND (account_email IS NULL OR account_email <> ?)",
                    params![
                        account_email.as_str(),
                        principal.as_str(),
                        workspace.as_str(),
                        provider.as_str(),
                        account_alias.as_str(),
                        account_email.as_str()
                    ],
                )?;
                let messages = conn.execute(
                    "UPDATE mail_messages SET account_email = ? \
                     WHERE principal = ? AND workspace = ? AND provider = ? \
                       AND account_alias = ? \
                       AND (account_email IS NULL OR account_email <> ?)",
                    params![
                        account_email.as_str(),
                        principal.as_str(),
                        workspace.as_str(),
                        provider.as_str(),
                        account_alias.as_str(),
                        account_email.as_str()
                    ],
                )?;
                Ok(threads + messages)
            })
            .context("reconciling channel account email")?;
            inner
                .maybe_checkpoint(&conn)
                .context("throttled checkpoint after account email reconciliation")?;
            Ok(changed)
        })
        .await
        .context("mail assist reconcile_account_email task panicked")?
    }

    /// Append message metadata rows, deduplicating on
    /// (provider, account_alias, message_id). Returns the number of NEW rows
    /// inserted — re-appending an already-known non-sensitive message is a
    /// no-op, while a later sensitive row redacts the stored metadata and
    /// suppresses distillation.
    pub async fn append_messages(
        &self,
        principal: &str,
        workspace: &str,
        messages: Vec<MailMessageMeta>,
    ) -> Result<usize> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let _write_guard = inner.acquire_write_guard()?;
            let conn = inner
                .write_conn
                .lock()
                .expect("mail assist write connection mutex poisoned");
            let mut stmt = conn.prepare(
                "INSERT INTO mail_messages (
                    principal, workspace, provider, account_alias, message_id,
                    thread_id, account_email, provider_cursor, label_ids_json,
                    subject, from_name, from_address, to_domains_json, cc_domains_json,
                    internal_date, observed_at, direction, summary, intent,
                    needs_reply_hint, follow_up_hint_json, distill_brief_json,
                    distill_contract_version, distilled_at, distill_revision,
                    distill_state, distill_attempts, sensitive_suppressed,
                    origin, schema_version
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT (principal, workspace, provider, account_alias, message_id)
                DO NOTHING",
            )?;
            let mut redact_stmt = conn.prepare(
                "UPDATE mail_messages \
                 SET thread_id = ?, account_email = ?, provider_cursor = ?, \
                     label_ids_json = ?, subject = ?, from_name = ?, from_address = ?, \
                     to_domains_json = ?, cc_domains_json = ?, internal_date = ?, \
                     observed_at = ?, direction = ?, summary = NULL, intent = NULL, \
                     needs_reply_hint = FALSE, follow_up_hint_json = NULL, \
                     distill_evidence_message_ids_json = NULL, \
                     distill_brief_json = NULL, distill_contract_version = NULL, \
                     distilled_at = NULL, distill_revision = NULL, \
                     distill_backfill_attempts = 0, \
                     distill_backfill_next_retry_at = NULL, \
                     distill_backfill_last_error = NULL, \
                     distill_state = ?, distill_attempts = 0, \
                     sensitive_suppressed = TRUE, origin = ?, schema_version = ? \
                 WHERE principal = ? AND workspace = ? AND provider = ? \
                   AND account_alias = ? AND message_id = ?",
            )?;
            let mut inserted = 0usize;
            for batch in messages.chunks(128) {
                with_transaction(&conn, || {
            for message in batch {
                let label_ids_json = serde_json::to_string(&message.label_ids)
                    .context("serializing mail message label ids")?;
                let to_domains_json = serde_json::to_string(&message.to_domains)
                    .context("serializing mail message to domains")?;
                let cc_domains_json = serde_json::to_string(&message.cc_domains)
                    .context("serializing mail message cc domains")?;
                let follow_up_hint_json = message
                    .follow_up_hint
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .context("serializing mail message follow-up hint")?;
                let distill_brief_json = message
                    .distill_brief
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()
                    .context("serializing mail message information brief")?;
                let changed = stmt
                    .execute(params![
                        principal,
                        workspace,
                        message.provider,
                        message.account_alias,
                        message.message_id,
                        message.thread_id,
                        message.account_email,
                        message.provider_cursor,
                        label_ids_json,
                        message.subject,
                        message.from_name,
                        message.from_address,
                        to_domains_json,
                        cc_domains_json,
                        message.internal_date,
                        message.observed_at,
                        message.direction.as_ref().map(MessageDirection::as_db_str),
                        message.summary,
                        message.intent,
                        message.needs_reply_hint,
                        follow_up_hint_json,
                        distill_brief_json,
                        message.distill_contract_version,
                        message.distilled_at,
                        message.distill_revision,
                        message.distill_state.as_db_str(),
                        message.distill_attempts,
                        message.sensitive_suppressed,
                        message.origin.as_db_str(),
                        message.schema_version,
                    ])
                    .context("appending mail message metadata")?;
                inserted += changed;
                if changed == 0 && message.sensitive_suppressed {
                    (|| -> Result<()> {
                        let prior_thread_id = query_message_thread_id(
                            &conn,
                            &principal,
                            &workspace,
                            &message.provider,
                            &message.account_alias,
                            &message.message_id,
                        )?;
                        let redacted = redact_stmt.execute(params![
                            message.thread_id,
                            message.account_email,
                            message.provider_cursor,
                            label_ids_json,
                            message.subject,
                            message.from_name,
                            message.from_address,
                            to_domains_json,
                            cc_domains_json,
                            message.internal_date,
                            message.observed_at,
                            message.direction.as_ref().map(MessageDirection::as_db_str),
                            DistillState::Suppressed.as_db_str(),
                            message.origin.as_db_str(),
                            message.schema_version,
                            principal,
                            workspace,
                            message.provider,
                            message.account_alias,
                            message.message_id,
                        ])?;
                        if redacted == 0 {
                            anyhow::bail!(
                                "mail message disappeared while applying sensitive redaction: {}",
                                message.message_id
                            );
                        }
                        refresh_thread_latest_summary(
                            &conn,
                            &principal,
                            &workspace,
                            &message.provider,
                            &message.account_alias,
                            &message.thread_id,
                        )?;
                        if let Some(prior_thread_id) = prior_thread_id.as_deref() {
                            if prior_thread_id != message.thread_id.as_str() {
                                refresh_thread_latest_summary(
                                    &conn,
                                    &principal,
                                    &workspace,
                                    &message.provider,
                                    &message.account_alias,
                                    prior_thread_id,
                                )?;
                            }
                        }
                        Ok(())
                    })()
                    .context("redacting existing sensitive mail message")?;
                }
            }
                Ok(())
                })?;
            }
            drop(redact_stmt);
            drop(stmt);
            inner
                .maybe_checkpoint(&conn)
                .context("throttled checkpoint after mail message append")?;
            Ok(inserted)
        })
        .await
        .context("mail assist append_messages task panicked")?
    }

    /// Append normalized provider-side thread changes. Stable ids make replay
    /// idempotent when a later write fails before the provider watermark can
    /// advance.
    pub async fn append_provider_changes(
        &self,
        principal: &str,
        workspace: &str,
        changes: Vec<ProviderThreadChange>,
    ) -> Result<usize> {
        if changes.is_empty() {
            return Ok(0);
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let _write_guard = inner.acquire_write_guard()?;
            let conn = inner
                .write_conn
                .lock()
                .expect("mail assist write connection mutex poisoned");
            let mut stmt = conn.prepare(
                "INSERT INTO mail_assist_events (
                    principal, workspace, id, annotation_id, provider, account_alias,
                    thread_id, event_type, actor, from_state, to_state, detail_json,
                    created_at, schema_version
                ) VALUES (?, ?, ?, NULL, ?, ?, ?, 'provider_change', 'worker', NULL, NULL, ?, ?, ?)
                ON CONFLICT (principal, workspace, id) DO NOTHING",
            )?;
            let mut inserted = 0usize;
            for change in changes {
                let detail = serde_json::to_string(&serde_json::json!({
                    "kind": change.kind.as_db_str(),
                    "message_id": change.message_id,
                    "thread_removed": change.thread_removed,
                    "label_ids": change.label_ids,
                    "current_label_ids": change.current_label_ids,
                    "provider_cursor": change.provider_cursor,
                }))
                .context("serializing provider thread change")?;
                inserted += stmt
                    .execute(params![
                        principal,
                        workspace,
                        change.id,
                        change.provider,
                        change.account_alias,
                        change.thread_id,
                        detail,
                        change.observed_at,
                        change.schema_version,
                    ])
                    .context("appending provider thread change")?;
            }
            drop(stmt);
            inner
                .maybe_checkpoint(&conn)
                .context("throttled checkpoint after provider change append")?;
            Ok(inserted)
        })
        .await
        .context("mail assist append_provider_changes task panicked")?
    }

    /// Newest-first slice of the distillation queue: message rows with
    /// `distill_state = 'pending'` across ALL providers/accounts of the
    /// scope, ordered by `internal_date` so recent correspondence catches
    /// up before stale backfill, capped at `limit`. Suppressed/skipped/done/failed
    /// rows never appear here — the queue worker retries `failed` rows via
    /// its own cap-aware pass.
    pub async fn list_pending_distill(
        &self,
        principal: &str,
        workspace: &str,
        limit: usize,
    ) -> Result<Vec<MailMessageMeta>> {
        self.list_pending_distill_since(principal, workspace, limit, i64::MIN)
            .await
    }

    pub async fn list_pending_distill_since(
        &self,
        principal: &str,
        workspace: &str,
        limit: usize,
        floor: i64,
    ) -> Result<Vec<MailMessageMeta>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(&format!(
                "SELECT {MESSAGE_COLUMNS} FROM mail_messages \
                 WHERE principal = ? AND workspace = ? AND distill_state = 'pending' \
                   AND internal_date >= ? \
                 ORDER BY internal_date DESC, message_id \
                 LIMIT ?",
            ))?;
            let mut rows = stmt.query(params![principal, workspace, floor, limit as i64])?;
            let mut messages = Vec::new();
            while let Some(row) = rows.next()? {
                messages.push(map_message_row(row)?);
            }
            Ok(messages)
        })
        .await
        .context("mail assist list_pending_distill task panicked")?
    }

    /// Cap-aware retry slice of the distillation queue: `failed` message
    /// rows whose `distill_attempts` is still below `max_attempts`,
    /// newest first, capped at `limit`. This is the "own cap-aware pass"
    /// the pending-queue read refers to — the worker drains pending rows
    /// first, then retries failed ones with the remaining batch budget.
    /// Rows at the attempt cap never reappear (the worker marks them
    /// `skipped` at the final failure).
    pub async fn list_retryable_distill(
        &self,
        principal: &str,
        workspace: &str,
        max_attempts: i64,
        limit: usize,
    ) -> Result<Vec<MailMessageMeta>> {
        self.list_retryable_distill_since(principal, workspace, max_attempts, limit, i64::MIN)
            .await
    }

    pub async fn list_retryable_distill_since(
        &self,
        principal: &str,
        workspace: &str,
        max_attempts: i64,
        limit: usize,
        floor: i64,
    ) -> Result<Vec<MailMessageMeta>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(&format!(
                "SELECT {MESSAGE_COLUMNS} FROM mail_messages \
                 WHERE principal = ? AND workspace = ? AND distill_state = 'failed' \
                   AND distill_attempts < ? \
                   AND internal_date >= ? \
                 ORDER BY internal_date DESC, message_id \
                 LIMIT ?",
            ))?;
            let mut rows = stmt.query(params![
                principal,
                workspace,
                max_attempts,
                floor,
                limit as i64
            ])?;
            let mut messages = Vec::new();
            while let Some(row) = rows.next()? {
                messages.push(map_message_row(row)?);
            }
            Ok(messages)
        })
        .await
        .context("mail assist list_retryable_distill task panicked")?
    }

    /// Newest-first, bounded historical rows whose durable distill contract is
    /// older than `target_contract_version`. Existing safe output remains in
    /// place while these rows are retried; only a successful replacement gets
    /// a new monotonic revision.
    pub async fn list_distill_backfill_candidates(
        &self,
        principal: &str,
        workspace: &str,
        target_contract_version: u32,
        cutoff_internal_date: i64,
        now_ms: i64,
        limit: usize,
    ) -> Result<Vec<MailMessageMeta>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(&format!(
                "SELECT {MESSAGE_COLUMNS} FROM mail_messages \
                 WHERE principal = ? AND workspace = ? \
                   AND distill_state = 'done' AND sensitive_suppressed = FALSE \
                   AND internal_date >= ? \
                   AND (COALESCE(distill_contract_version, 0) < ? \
                        OR (? >= 2 AND distill_brief_json IS NULL)) \
                   AND (distill_backfill_next_retry_at IS NULL \
                        OR distill_backfill_next_retry_at <= ?) \
                 ORDER BY internal_date DESC, provider, account_alias, message_id \
                 LIMIT ?",
            ))?;
            let mut rows = stmt.query(params![
                principal,
                workspace,
                cutoff_internal_date,
                target_contract_version,
                target_contract_version,
                now_ms,
                limit as i64
            ])?;
            let mut messages = Vec::new();
            while let Some(row) = rows.next()? {
                messages.push(map_message_row(row)?);
            }
            Ok(messages)
        })
        .await
        .context("mail assist list_distill_backfill_candidates task panicked")?
    }

    /// Resolve a bounded priority list of exact channel message identities
    /// against the same historical-repair eligibility predicate. Input order
    /// is preserved so the resurfacing store can put currently surfaced cards
    /// ahead of the generic newest-first fill without a cross-database join.
    pub async fn list_distill_backfill_candidates_by_keys(
        &self,
        principal: &str,
        workspace: &str,
        target_contract_version: u32,
        cutoff_internal_date: i64,
        now_ms: i64,
        keys: &[(String, String, String)],
        limit: usize,
    ) -> Result<Vec<MailMessageMeta>> {
        if keys.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let keys = keys.to_vec();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let value_rows = (0..keys.len())
                .map(|_| "(?, ?, ?, ?)")
                .collect::<Vec<_>>()
                .join(", ");
            let qualified_columns = MESSAGE_COLUMNS
                .split(',')
                .map(|column| format!("m.{}", column.trim()))
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "WITH priority(provider, account_alias, message_id, priority_order) AS ( \
                     VALUES {value_rows} \
                 ) \
                 SELECT {qualified_columns} FROM mail_messages m \
                 INNER JOIN priority p \
                   ON p.provider = m.provider \
                  AND p.account_alias = m.account_alias \
                  AND p.message_id = m.message_id \
                 WHERE m.principal = ? AND m.workspace = ? \
                   AND m.distill_state = 'done' AND m.sensitive_suppressed = FALSE \
                   AND m.internal_date >= ? \
                   AND (COALESCE(m.distill_contract_version, 0) < ? \
                        OR (? >= 2 AND m.distill_brief_json IS NULL)) \
                   AND (m.distill_backfill_next_retry_at IS NULL \
                        OR m.distill_backfill_next_retry_at <= ?) \
                 ORDER BY p.priority_order ASC LIMIT ?"
            );
            let mut values = Vec::with_capacity(keys.len() * 4 + 7);
            for (priority_order, (provider, account_alias, message_id)) in
                keys.into_iter().enumerate()
            {
                values.push(DuckValue::Text(provider));
                values.push(DuckValue::Text(account_alias));
                values.push(DuckValue::Text(message_id));
                values.push(DuckValue::BigInt(priority_order as i64));
            }
            values.push(DuckValue::Text(principal));
            values.push(DuckValue::Text(workspace));
            values.push(DuckValue::BigInt(cutoff_internal_date));
            values.push(DuckValue::BigInt(i64::from(target_contract_version)));
            values.push(DuckValue::BigInt(i64::from(target_contract_version)));
            values.push(DuckValue::BigInt(now_ms));
            values.push(DuckValue::BigInt(limit as i64));
            let mut stmt = conn.prepare(&sql)?;
            let mut rows = stmt.query(params_from_iter(values.iter()))?;
            let mut messages = Vec::new();
            while let Some(row) = rows.next()? {
                messages.push(map_message_row(row)?);
            }
            Ok(messages)
        })
        .await
        .context("mail assist list_distill_backfill_candidates_by_keys task panicked")?
    }

    pub async fn count_distill_backfill(
        &self,
        principal: &str,
        workspace: &str,
        target_contract_version: u32,
        cutoff_internal_date: i64,
        now_ms: i64,
    ) -> Result<DistillBackfillCounts> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let (total, ready): (i64, i64) = conn.query_row(
                "SELECT COUNT(*), \
                        COALESCE(SUM(CASE \
                            WHEN distill_backfill_next_retry_at IS NULL \
                              OR distill_backfill_next_retry_at <= ? THEN 1 ELSE 0 END), 0) \
                 FROM mail_messages \
                 WHERE principal = ? AND workspace = ? \
                   AND distill_state = 'done' AND sensitive_suppressed = FALSE \
                   AND internal_date >= ? \
                   AND (COALESCE(distill_contract_version, 0) < ? \
                        OR (? >= 2 AND distill_brief_json IS NULL))",
                params![
                    now_ms,
                    principal,
                    workspace,
                    cutoff_internal_date,
                    target_contract_version,
                    target_contract_version
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let total = total.max(0) as u64;
            let ready = ready.max(0) as u64;
            Ok(DistillBackfillCounts {
                total,
                ready,
                cooling: total.saturating_sub(ready),
            })
        })
        .await
        .context("mail assist count_distill_backfill task panicked")?
    }

    /// Reconciled safe-brief coverage for completed, non-suppressed messages.
    /// Legacy means either an older contract or a missing structured brief.
    pub async fn brief_coverage(
        &self,
        principal: &str,
        workspace: &str,
        target_contract_version: u32,
    ) -> Result<ChannelBriefCoverage> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let tuple: (i64, i64, i64, i64, i64, i64) = conn.query_row(
                "SELECT COUNT(*), \
                    COALESCE(SUM(CASE WHEN COALESCE(distill_contract_version, 0) >= ? \
                        AND distill_brief_json IS NOT NULL THEN 1 ELSE 0 END), 0), \
                    COALESCE(SUM(CASE WHEN COALESCE(distill_contract_version, 0) < ? \
                        OR distill_brief_json IS NULL THEN 1 ELSE 0 END), 0), \
                    COALESCE(SUM(CASE WHEN json_extract_string(distill_brief_json, '$.detail_status') = 'complete' THEN 1 ELSE 0 END), 0), \
                    COALESCE(SUM(CASE WHEN json_extract_string(distill_brief_json, '$.detail_status') = 'partial' THEN 1 ELSE 0 END), 0), \
                    COALESCE(SUM(CASE WHEN json_extract_string(distill_brief_json, '$.detail_status') = 'source_omits_details' THEN 1 ELSE 0 END), 0) \
                 FROM mail_messages WHERE principal = ? AND workspace = ? \
                   AND distill_state = 'done' AND sensitive_suppressed = FALSE",
                params![
                    i64::from(target_contract_version),
                    i64::from(target_contract_version),
                    principal,
                    workspace
                ],
                |row| {
                    Ok((
                        row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
                        row.get(4)?, row.get(5)?,
                    ))
                },
            )?;
            Ok(ChannelBriefCoverage {
                done: tuple.0.max(0) as u64,
                v2: tuple.1.max(0) as u64,
                legacy: tuple.2.max(0) as u64,
                complete: tuple.3.max(0) as u64,
                partial: tuple.4.max(0) as u64,
                source_omits_details: tuple.5.max(0) as u64,
            })
        })
        .await
        .context("mail assist brief_coverage task panicked")?
    }

    /// Record a historical repair failure without changing `distill_state` or
    /// deleting the existing brief. The durable retry deadline prevents a
    /// permanently unavailable source from consuming every worker tick.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_distill_backfill_failure(
        &self,
        principal: &str,
        workspace: &str,
        provider: &str,
        account_alias: &str,
        message_id: &str,
        expected_revision: Option<i64>,
        error: &str,
        now_ms: i64,
    ) -> Result<()> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let provider = provider.to_string();
        let account_alias = account_alias.to_string();
        let message_id = message_id.to_string();
        let error = trim_for_storage(error, DISTILL_BACKFILL_ERROR_MAX_CHARS);
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let _write_guard = inner.acquire_write_guard()?;
            let conn = inner
                .write_conn
                .lock()
                .expect("mail assist write connection mutex poisoned");
            let attempts: i64 = conn
                .query_row(
                    "SELECT COALESCE(distill_backfill_attempts, 0) FROM mail_messages \
                     WHERE principal = ? AND workspace = ? AND provider = ? \
                       AND account_alias = ? AND message_id = ? \
                       AND (distill_revision = ? \
                            OR (distill_revision IS NULL AND ? IS NULL))",
                    params![
                        principal,
                        workspace,
                        provider,
                        account_alias,
                        message_id,
                        expected_revision,
                        expected_revision
                    ],
                    |row| row.get(0),
                )
                .context("reading distill backfill attempts")?;
            let backoff_index =
                (attempts.max(0) as usize).min(DISTILL_BACKFILL_RETRY_BACKOFF_MS.len() - 1);
            let next_retry_at =
                now_ms.saturating_add(DISTILL_BACKFILL_RETRY_BACKOFF_MS[backoff_index]);
            let changed = conn.execute(
                "UPDATE mail_messages \
                 SET distill_backfill_attempts = COALESCE(distill_backfill_attempts, 0) + 1, \
                     distill_backfill_next_retry_at = ?, \
                     distill_backfill_last_error = ? \
                 WHERE principal = ? AND workspace = ? AND provider = ? \
                   AND account_alias = ? AND message_id = ? \
                   AND distill_state = 'done' AND sensitive_suppressed = FALSE \
                   AND (distill_revision = ? \
                        OR (distill_revision IS NULL AND ? IS NULL))",
                params![
                    next_retry_at,
                    error,
                    principal,
                    workspace,
                    provider,
                    account_alias,
                    message_id,
                    expected_revision,
                    expected_revision
                ],
            )?;
            if changed == 0 {
                anyhow::bail!("mail message is no longer eligible for distill backfill");
            }
            inner.maybe_checkpoint(&conn)?;
            Ok(())
        })
        .await
        .context("mail assist record_distill_backfill_failure task panicked")?
    }

    /// Single-message lookup (the distill queue worker re-reads row state
    /// before/after a run; tests verify distill writes through it).
    pub async fn get_message(
        &self,
        principal: &str,
        workspace: &str,
        provider: &str,
        account_alias: &str,
        message_id: &str,
    ) -> Result<Option<MailMessageMeta>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let provider = provider.to_string();
        let account_alias = account_alias.to_string();
        let message_id = message_id.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(&format!(
                "SELECT {MESSAGE_COLUMNS} FROM mail_messages \
                 WHERE principal = ? AND workspace = ? AND provider = ? \
                   AND account_alias = ? AND message_id = ?",
            ))?;
            let mut rows = stmt.query(params![
                principal,
                workspace,
                provider,
                account_alias,
                message_id.clone()
            ])?;
            match rows.next()? {
                Some(row) => Ok(Some(map_message_row(row)?)),
                None => Ok(None),
            }
        })
        .await
        .context("mail assist get_message task panicked")?
    }

    /// Persist a composed channel-action draft (the `compose` output the owner
    /// reviews before `commit`). Idempotent on `compose_id` — a re-`compose`
    /// that reuses the id replaces the text. Scratch-only; never persists
    /// provider content beyond the locally-derived draft text.
    pub async fn put_channel_action_draft(
        &self,
        principal: &str,
        workspace: &str,
        draft: ChannelActionDraftRow,
    ) -> Result<()> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let _write_guard = inner.acquire_write_guard()?;
            let conn = inner
                .write_conn
                .lock()
                .expect("mail assist write connection mutex poisoned");
            conn.execute(
                "INSERT INTO channel_action_drafts (
                    principal, workspace, compose_id, annotation_id, action_id,
                    text, created_at, schema_version
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT (principal, workspace, compose_id) DO UPDATE SET
                    annotation_id = excluded.annotation_id,
                    action_id = excluded.action_id,
                    text = excluded.text,
                    created_at = excluded.created_at,
                    schema_version = excluded.schema_version",
                params![
                    principal,
                    workspace,
                    draft.compose_id,
                    draft.annotation_id,
                    draft.action_id,
                    draft.text,
                    draft.created_at,
                    MAIL_ASSIST_SCHEMA_VERSION,
                ],
            )
            .context("persisting channel action draft")?;
            inner
                .maybe_checkpoint(&conn)
                .context("throttled checkpoint after channel action draft write")?;
            Ok(())
        })
        .await
        .context("mail assist put_channel_action_draft task panicked")?
    }

    /// Load a composed channel-action draft by `compose_id` within a scope.
    pub async fn get_channel_action_draft(
        &self,
        principal: &str,
        workspace: &str,
        compose_id: &str,
    ) -> Result<Option<ChannelActionDraftRow>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let compose_id = compose_id.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(
                "SELECT compose_id, annotation_id, action_id, text, created_at \
                 FROM channel_action_drafts \
                 WHERE principal = ? AND workspace = ? AND compose_id = ?",
            )?;
            let mut rows = stmt.query(params![principal, workspace, compose_id])?;
            match rows.next()? {
                Some(row) => Ok(Some(ChannelActionDraftRow {
                    compose_id: row.get(0)?,
                    annotation_id: row.get(1)?,
                    action_id: row.get(2)?,
                    text: row.get(3)?,
                    created_at: row.get(4)?,
                })),
                None => Ok(None),
            }
        })
        .await
        .context("mail assist get_channel_action_draft task panicked")?
    }

    /// Exact message ids whose live content fed the target message's current
    /// distillation. A single-message distillation returns the target id;
    /// coalesced same-thread distillation preserves its bounded ordered batch.
    /// This is metadata-only and never reads or persists source bodies.
    pub async fn get_distill_evidence_message_ids(
        &self,
        principal: &str,
        workspace: &str,
        provider: &str,
        account_alias: &str,
        message_id: &str,
    ) -> Result<Option<Vec<String>>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let provider = provider.to_string();
        let account_alias = account_alias.to_string();
        let message_id = message_id.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(
                "SELECT distill_evidence_message_ids_json FROM mail_messages \
                 WHERE principal = ? AND workspace = ? AND provider = ? \
                   AND account_alias = ? AND message_id = ?",
            )?;
            let mut rows = stmt.query(params![
                principal,
                workspace,
                provider,
                account_alias,
                message_id.clone()
            ])?;
            let Some(row) = rows.next()? else {
                return Ok(None);
            };
            let encoded: Option<String> = row.get(0)?;
            let mut ids = encoded
                .as_deref()
                .map(serde_json::from_str::<Vec<String>>)
                .transpose()
                .context("parsing mail message distill evidence ids JSON")?
                .unwrap_or_default();
            ids.retain(|id| !id.trim().is_empty());
            if !ids.iter().any(|id| id == &message_id) {
                ids.insert(0, message_id);
            }
            ids.dedup();
            Ok(Some(ids))
        })
        .await
        .context("mail assist get_distill_evidence_message_ids task panicked")?
    }

    /// A compact corpus for passive pattern synthesis: distilled, non-suppressed
    /// messages `internal_date >= since` with their subject/sender/summary. Read
    /// PASSIVELY (no user action needed) so the synthesizer can surface recurring
    /// topics + their timing. Newest first, capped.
    pub async fn list_pattern_corpus(
        &self,
        principal: &str,
        workspace: &str,
        since: i64,
        limit: usize,
    ) -> Result<Vec<PatternCorpusRow>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(
                "SELECT internal_date, subject, from_address, summary \
                 FROM mail_messages \
                 WHERE principal = ? AND workspace = ? \
                   AND sensitive_suppressed = FALSE AND internal_date >= ? \
                   AND summary IS NOT NULL \
                 ORDER BY internal_date DESC LIMIT ?",
            )?;
            let mut rows = stmt.query(params![principal, workspace, since, limit as i64])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push(PatternCorpusRow {
                    internal_date: row.get(0)?,
                    subject: row.get(1)?,
                    from_address: row.get(2)?,
                    summary: row.get(3)?,
                });
            }
            Ok(out)
        })
        .await
        .context("mail assist list_pattern_corpus task panicked")?
    }

    /// Whether the thread has a message NEWER than `internal_date` — used to
    /// tell the owner "a newer message arrived after the one we summarized."
    pub async fn has_message_after(
        &self,
        principal: &str,
        workspace: &str,
        provider: &str,
        account_alias: &str,
        thread_id: &str,
        internal_date: i64,
    ) -> Result<bool> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let provider = provider.to_string();
        let account_alias = account_alias.to_string();
        let thread_id = thread_id.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM mail_messages \
                 WHERE principal = ? AND workspace = ? AND provider = ? \
                   AND account_alias = ? AND thread_id = ? AND internal_date > ?",
                params![
                    principal,
                    workspace,
                    provider,
                    account_alias,
                    thread_id,
                    internal_date
                ],
                |row| row.get(0),
            )?;
            Ok(count > 0)
        })
        .await
        .context("mail assist has_message_after task panicked")?
    }

    /// The message a thread's `latest_summary`/classification was actually
    /// derived from — the newest DISTILLED (`distill_state = 'done'`) message,
    /// which is exactly what `set_distill_result` rolls `latest_summary` from
    /// (`internal_date >= MAX(internal_date among done)`). This is what the
    /// "show actual message" fetch must resolve, NOT max(`internal_date`) — the
    /// literal newest message may be undistilled/suppressed and NOT the one the
    /// summary + Follow-up card are about. Falls back to the newest message when
    /// (defensively) none is done.
    pub async fn distilled_message_for_thread(
        &self,
        principal: &str,
        workspace: &str,
        provider: &str,
        account_alias: &str,
        thread_id: &str,
    ) -> Result<Option<MailMessageMeta>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let provider = provider.to_string();
        let account_alias = account_alias.to_string();
        let thread_id = thread_id.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(&format!(
                "SELECT {MESSAGE_COLUMNS} FROM mail_messages \
                 WHERE principal = ? AND workspace = ? AND provider = ? \
                   AND account_alias = ? AND thread_id = ? \
                 ORDER BY (distill_state = 'done') DESC, internal_date DESC LIMIT 1",
            ))?;
            let mut rows = stmt.query(params![
                principal,
                workspace,
                provider,
                account_alias,
                thread_id
            ])?;
            match rows.next()? {
                Some(row) => Ok(Some(map_message_row(row)?)),
                None => Ok(None),
            }
        })
        .await
        .context("mail assist distilled_message_for_thread task panicked")?
    }

    /// Record a successful distillation: set the message's
    /// `summary`/`intent`, mark it `done`, and roll the parent thread's
    /// `latest_summary` when this message is the thread's newest DISTILLED
    /// one (so the thread summary always reflects the freshest distilled
    /// content, even when the true latest message ends up
    /// suppressed/skipped). Atomic; errors if the message is unknown.
    pub async fn set_distill_result(
        &self,
        principal: &str,
        workspace: &str,
        provider: &str,
        account_alias: &str,
        message_id: &str,
        summary: &str,
        intent: &str,
        needs_reply_hint: bool,
        follow_up_hint: Option<&ChannelFollowUpHint>,
    ) -> Result<()> {
        let evidence_message_ids = vec![message_id.to_string()];
        self.set_distill_result_with_evidence_ids(
            principal,
            workspace,
            provider,
            account_alias,
            message_id,
            summary,
            intent,
            needs_reply_hint,
            follow_up_hint,
            &evidence_message_ids,
        )
        .await
    }

    /// Same as [`Self::set_distill_result`], but preserves the complete message
    /// id batch used by a coalesced same-thread distillation. The target
    /// `message_id` remains the primary evidence id; the JSON list lets
    /// classification and "show message" expose all inputs that shaped the
    /// summary.
    pub async fn set_distill_result_with_evidence_ids(
        &self,
        principal: &str,
        workspace: &str,
        provider: &str,
        account_alias: &str,
        message_id: &str,
        summary: &str,
        intent: &str,
        needs_reply_hint: bool,
        follow_up_hint: Option<&ChannelFollowUpHint>,
        evidence_message_ids: &[String],
    ) -> Result<()> {
        self.set_distill_result_with_brief_and_evidence_ids(
            principal,
            workspace,
            provider,
            account_alias,
            message_id,
            summary,
            intent,
            needs_reply_hint,
            follow_up_hint,
            None,
            1,
            current_epoch_ms(),
            evidence_message_ids,
        )
        .await
        .map(|_| ())
    }

    /// Atomically persist the compatibility result and V2 safe brief, then
    /// allocate a scope-local monotonic revision in the same transaction. The
    /// revision is returned so downstream observability can correlate the
    /// exact durable write without another lookup.
    #[allow(clippy::too_many_arguments)]
    pub async fn set_distill_result_with_brief_and_evidence_ids(
        &self,
        principal: &str,
        workspace: &str,
        provider: &str,
        account_alias: &str,
        message_id: &str,
        summary: &str,
        intent: &str,
        needs_reply_hint: bool,
        follow_up_hint: Option<&ChannelFollowUpHint>,
        brief: Option<&ChannelInformationBrief>,
        distill_contract_version: u32,
        distilled_at: i64,
        evidence_message_ids: &[String],
    ) -> Result<i64> {
        if !matches!(
            distill_contract_version,
            1 | CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION
        ) {
            anyhow::bail!("unsupported distill contract version: {distill_contract_version}");
        }
        if distill_contract_version >= CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION && brief.is_none() {
            anyhow::bail!("distill contract v2 requires an information brief");
        }
        if let Some(brief) = brief {
            if distill_contract_version != CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION {
                anyhow::bail!("an information brief requires distill contract v2");
            }
            if brief.schema_version != CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION {
                anyhow::bail!("information brief schema does not match the supported version");
            }
            if brief.summary != summary {
                anyhow::bail!("information brief summary must match the compatibility summary");
            }
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let provider = provider.to_string();
        let account_alias = account_alias.to_string();
        let message_id = message_id.to_string();
        let summary = summary.to_string();
        let intent = intent.to_string();
        let mut evidence_ids = Vec::new();
        if evidence_message_ids.is_empty() {
            evidence_ids.push(message_id.clone());
        } else {
            for id in evidence_message_ids {
                if !id.trim().is_empty() && !evidence_ids.iter().any(|seen| seen == id) {
                    evidence_ids.push(id.clone());
                }
            }
            if !evidence_ids.iter().any(|seen| seen == &message_id) {
                evidence_ids.insert(0, message_id.clone());
            }
        }
        let distill_evidence_message_ids_json =
            serde_json::to_string(&evidence_ids).context("serializing distill evidence ids")?;
        let follow_up_hint_json = follow_up_hint
            .map(serde_json::to_string)
            .transpose()
            .context("serializing mail message follow-up hint")?;
        let distill_brief_json = brief
            .map(serde_json::to_string)
            .transpose()
            .context("serializing mail message information brief")?;
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let _write_guard = inner.acquire_write_guard()?;
            let conn = inner
                .write_conn
                .lock()
                .expect("mail assist write connection mutex poisoned");
            let distill_revision = with_transaction(&conn, || {
                let distill_revision = allocate_distill_revision(&conn, &principal, &workspace)?;
                let changed = conn
                    .execute(
                        "UPDATE mail_messages \
                         SET summary = ?, intent = ?, needs_reply_hint = ?, \
                             follow_up_hint_json = ?, \
                             distill_evidence_message_ids_json = ?, \
                             distill_brief_json = ?, distill_contract_version = ?, \
                             distilled_at = ?, distill_revision = ?, \
                             distill_backfill_attempts = 0, \
                             distill_backfill_next_retry_at = NULL, \
                             distill_backfill_last_error = NULL, \
                             classify_attempts = 0, classify_next_retry_at = NULL, \
                             classify_last_error = NULL, classify_failed_at = NULL, \
                             distill_state = 'done' \
                         WHERE principal = ? AND workspace = ? AND provider = ? \
                           AND account_alias = ? AND message_id = ? \
                           AND sensitive_suppressed = FALSE \
                           AND distill_state != 'suppressed'",
                        params![
                            summary,
                            intent,
                            needs_reply_hint,
                            follow_up_hint_json,
                            distill_evidence_message_ids_json,
                            distill_brief_json,
                            distill_contract_version,
                            distilled_at,
                            distill_revision,
                            principal,
                            workspace,
                            provider,
                            account_alias,
                            message_id
                        ],
                    )
                    .context("recording mail message distill result")?;
                if changed == 0 {
                    anyhow::bail!(
                        "mail message not found or no longer eligible for distill result: \
                         {message_id}"
                    );
                }
                // Roll the thread summary only when this message is the
                // newest distilled one in its thread (the update above is
                // already visible inside this transaction, so the MAX
                // includes it).
                let (thread_id, internal_date): (String, i64) = {
                    let mut stmt = conn.prepare(
                        "SELECT thread_id, internal_date FROM mail_messages \
                         WHERE principal = ? AND workspace = ? AND provider = ? \
                           AND account_alias = ? AND message_id = ?",
                    )?;
                    let mut rows = stmt.query(params![
                        principal.as_str(),
                        workspace.as_str(),
                        provider.as_str(),
                        account_alias.as_str(),
                        message_id.as_str()
                    ])?;
                    let row = rows
                        .next()?
                        .context("distilled mail message row disappeared mid-transaction")?;
                    (row.get(0)?, row.get(1)?)
                };
                let newest_done: Option<i64> = {
                    let mut stmt = conn.prepare(
                        "SELECT MAX(internal_date) FROM mail_messages \
                         WHERE principal = ? AND workspace = ? AND provider = ? \
                           AND account_alias = ? AND thread_id = ? \
                           AND distill_state = 'done'",
                    )?;
                    let mut rows = stmt.query(params![
                        principal,
                        workspace,
                        provider,
                        account_alias,
                        thread_id
                    ])?;
                    match rows.next()? {
                        Some(row) => row.get(0)?,
                        None => None,
                    }
                };
                if newest_done.is_none_or(|max| internal_date >= max) {
                    conn.execute(
                        "UPDATE mail_threads SET latest_summary = ? \
                         WHERE principal = ? AND workspace = ? AND provider = ? \
                           AND account_alias = ? AND thread_id = ?",
                        params![
                            summary,
                            principal,
                            workspace,
                            provider,
                            account_alias,
                            thread_id
                        ],
                    )
                    .context("rolling mail thread latest summary")?;
                }
                Ok(distill_revision)
            })?;
            inner
                .maybe_checkpoint(&conn)
                .context("throttled checkpoint after mail distill result")?;
            Ok(distill_revision)
        })
        .await
        .context("mail assist set_distill_result task panicked")?
    }

    /// Move a message's distill state (skipped/suppressed/failed/pending
    /// re-queue). A transition to `failed` also increments
    /// `distill_attempts` so the queue's retry cap is enforceable. Errors
    /// if the message is unknown.
    pub async fn set_distill_state(
        &self,
        principal: &str,
        workspace: &str,
        provider: &str,
        account_alias: &str,
        message_id: &str,
        state: DistillState,
    ) -> Result<()> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let provider = provider.to_string();
        let account_alias = account_alias.to_string();
        let message_id = message_id.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let attempt_increment: i64 = i64::from(state == DistillState::Failed);
            let _write_guard = inner.acquire_write_guard()?;
            let conn = inner
                .write_conn
                .lock()
                .expect("mail assist write connection mutex poisoned");
            let changed = if state == DistillState::Suppressed {
                with_transaction(&conn, || {
                    let changed = conn.execute(
                        "UPDATE mail_messages \
                         SET distill_state = 'suppressed', sensitive_suppressed = TRUE, \
                             summary = NULL, intent = NULL, needs_reply_hint = FALSE, \
                             follow_up_hint_json = NULL, \
                             distill_evidence_message_ids_json = NULL, \
                             distill_brief_json = NULL, distill_contract_version = NULL, \
                             distilled_at = NULL, distill_revision = NULL \
                             , distill_backfill_attempts = 0 \
                             , distill_backfill_next_retry_at = NULL \
                             , distill_backfill_last_error = NULL \
                         WHERE principal = ? AND workspace = ? AND provider = ? \
                           AND account_alias = ? AND message_id = ?",
                        params![principal, workspace, provider, account_alias, message_id],
                    )?;
                    if changed == 0 {
                        anyhow::bail!("mail message not found for distill state: {message_id}");
                    }
                    let thread_id = query_message_thread_id(
                        &conn,
                        &principal,
                        &workspace,
                        &provider,
                        &account_alias,
                        &message_id,
                    )?
                    .context("suppressed mail message disappeared mid-transaction")?;
                    refresh_thread_latest_summary(
                        &conn,
                        &principal,
                        &workspace,
                        &provider,
                        &account_alias,
                        &thread_id,
                    )?;
                    Ok(changed)
                })?
            } else {
                conn.execute(
                    "UPDATE mail_messages \
                     SET distill_state = ?, distill_attempts = distill_attempts + ? \
                     WHERE principal = ? AND workspace = ? AND provider = ? \
                       AND account_alias = ? AND message_id = ? \
                       AND sensitive_suppressed = FALSE \
                       AND distill_state != 'suppressed'",
                    params![
                        state.as_db_str(),
                        attempt_increment,
                        principal,
                        workspace,
                        provider,
                        account_alias,
                        message_id
                    ],
                )
                .context("updating mail message distill state")?
            };
            if changed == 0 {
                anyhow::bail!("mail message not found for distill state: {message_id}");
            }
            inner
                .maybe_checkpoint(&conn)
                .context("throttled checkpoint after mail distill state update")?;
            Ok(())
        })
        .await
        .context("mail assist set_distill_state task panicked")?
    }

    /// Batch thread lookup for the annotations API. Unknown thread ids are
    /// simply absent from the result — an empty answer is valid.
    pub async fn get_threads_by_ids(
        &self,
        principal: &str,
        workspace: &str,
        provider: &str,
        account_alias: &str,
        thread_ids: &[String],
    ) -> Result<Vec<MailThreadRecord>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let provider = provider.to_string();
        let account_alias = account_alias.to_string();
        let thread_ids = thread_ids.to_vec();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(&format!(
                "SELECT {THREAD_COLUMNS} \
                 FROM mail_threads \
                 WHERE principal = ? AND workspace = ? AND provider = ? \
                   AND account_alias = ? AND thread_id = ?",
            ))?;
            let mut records = Vec::new();
            for thread_id in &thread_ids {
                let mut rows = stmt.query(params![
                    principal.clone(),
                    workspace.clone(),
                    provider.clone(),
                    account_alias.clone(),
                    thread_id.clone()
                ])?;
                if let Some(row) = rows.next()? {
                    records.push(map_thread_row(row)?);
                }
            }
            Ok(records)
        })
        .await
        .context("mail assist get_threads_by_ids task panicked")?
    }

    /// Most-recently-active threads for the fixtures exporter, newest
    /// `last_message_at` first (threads with no message timestamp sort
    /// last), optionally filtered to one account alias.
    pub async fn list_recent_threads(
        &self,
        principal: &str,
        workspace: &str,
        provider: &str,
        account_alias: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MailThreadRecord>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let provider = provider.to_string();
        let account_alias = account_alias.map(str::to_string);
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut records = Vec::new();
            if let Some(alias) = &account_alias {
                let mut stmt = conn.prepare(&format!(
                    "SELECT {THREAD_COLUMNS} FROM mail_threads \
                     WHERE principal = ? AND workspace = ? AND provider = ? \
                       AND account_alias = ? \
                     ORDER BY last_message_at DESC NULLS LAST, thread_id \
                     LIMIT ?",
                ))?;
                let mut rows =
                    stmt.query(params![principal, workspace, provider, alias, limit as i64])?;
                while let Some(row) = rows.next()? {
                    records.push(map_thread_row(row)?);
                }
            } else {
                let mut stmt = conn.prepare(&format!(
                    "SELECT {THREAD_COLUMNS} FROM mail_threads \
                     WHERE principal = ? AND workspace = ? AND provider = ? \
                     ORDER BY last_message_at DESC NULLS LAST, thread_id \
                     LIMIT ?",
                ))?;
                let mut rows = stmt.query(params![principal, workspace, provider, limit as i64])?;
                while let Some(row) = rows.next()? {
                    records.push(map_thread_row(row)?);
                }
            }
            Ok(records)
        })
        .await
        .context("mail assist list_recent_threads task panicked")?
    }

    /// Create an annotation and append its `annotation_created` audit
    /// event atomically. The annotation's lane is INHERITED from its
    /// thread row when one exists (lane flows account → thread →
    /// annotation; callers don't pick it) — the caller's `lane` field is
    /// only used when the thread is unknown. Returns the annotation as
    /// stored.
    pub async fn create_annotation(
        &self,
        principal: &str,
        workspace: &str,
        annotation: MailThreadAnnotation,
        actor: MailAssistActor,
    ) -> Result<MailThreadAnnotation> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let mut annotation = annotation;
            let inner = store.scope_inner(&principal, &workspace)?;
            let _write_guard = inner.acquire_write_guard()?;
            let conn = inner
                .write_conn
                .lock()
                .expect("mail assist write connection mutex poisoned");
            with_transaction(&conn, || {
                insert_annotation_record(
                    &conn,
                    &principal,
                    &workspace,
                    &mut annotation,
                    actor,
                    None,
                )
            })?;
            inner
                .maybe_checkpoint(&conn)
                .context("throttled checkpoint after mail annotation create")?;
            Ok(annotation)
        })
        .await
        .context("mail assist create_annotation task panicked")?
    }

    /// Persist a classifier result only when the exact message still carries
    /// the revision used to build the prompt. Reclassification updates the
    /// existing exact-message annotation in place, and a new actionable item
    /// refreshes an unclaimed `needs_approval` card for the same thread instead
    /// of creating a duplicate.
    pub async fn apply_classification_annotation(
        &self,
        principal: &str,
        workspace: &str,
        annotation: MailThreadAnnotation,
    ) -> Result<ClassificationAnnotationApplyResult> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let mut incoming = annotation;
            let expected_revision = incoming
                .classification_input_revision
                .context("classification annotation is missing its input revision")?;
            let evidence_message_id = incoming
                .evidence_message_id
                .clone()
                .context("classification annotation is missing exact message evidence")?;
            let inner = store.scope_inner(&principal, &workspace)?;
            let _write_guard = inner.acquire_write_guard()?;
            let conn = inner
                .write_conn
                .lock()
                .expect("mail assist write connection mutex poisoned");
            let result = with_transaction(&conn, || {
                let (current_revision, eligible) = query_message_classification_eligibility(
                    &conn, &principal, &workspace, &incoming,
                )?;
                if !eligible || current_revision != Some(expected_revision) {
                    return Ok(ClassificationAnnotationApplyResult::StaleInput {
                        current_revision,
                    });
                }

                let exact = query_annotation_for_evidence(
                    &conn,
                    &principal,
                    &workspace,
                    &incoming.provider,
                    &incoming.account_alias,
                    &incoming.thread_id,
                    &evidence_message_id,
                )?;
                if exact.as_ref().is_some_and(|current| {
                    current.classification_input_revision.unwrap_or(0) >= expected_revision
                }) {
                    return Ok(ClassificationAnnotationApplyResult::StaleInput {
                        current_revision: Some(expected_revision),
                    });
                }
                let exact_match = exact.is_some();
                let actionable = incoming.state == MailAnnotationState::NeedsApproval;
                let mut current = match exact {
                    Some(annotation) => Some(annotation),
                    None if actionable => query_refreshable_annotation_for_thread(
                        &conn,
                        &principal,
                        &workspace,
                        &incoming.provider,
                        &incoming.account_alias,
                        &incoming.thread_id,
                        true,
                    )?,
                    None => query_refreshable_annotation_for_thread(
                        &conn,
                        &principal,
                        &workspace,
                        &incoming.provider,
                        &incoming.account_alias,
                        &incoming.thread_id,
                        false,
                    )?,
                };
                if !exact_match {
                    if let Some(annotation) = current.as_ref() {
                        let active_claim = annotation.state == MailAnnotationState::NeedsApproval
                            && query_annotation_refresh_blocking_claim(
                                &conn,
                                &principal,
                                &workspace,
                                &annotation.id,
                                incoming.updated_at,
                            )?
                            .is_some();
                        if active_claim {
                            current = None;
                        }
                    }
                }

                let (stored, disposition) = if let Some(previous) = current {
                    let previous_state = previous.state;
                    let active_claim = previous_state == MailAnnotationState::NeedsApproval
                        && query_annotation_refresh_blocking_claim(
                            &conn,
                            &principal,
                            &workspace,
                            &previous.id,
                            incoming.updated_at,
                        )?
                        .is_some();
                    let provisional_repeat_suppression = previous_state
                        == MailAnnotationState::NeedsApproval
                        && !active_claim
                        && incoming.state == MailAnnotationState::Classified
                        && previous.provenance.as_deref() == Some("resurfacing_required_action:v1")
                        && annotation_action_bool(&incoming, "repeat_of_recently_handled")
                            == Some(true);
                    let mut updated = match previous_state {
                        MailAnnotationState::Observed | MailAnnotationState::Classified => {
                            incoming.id = previous.id.clone();
                            incoming.lane = previous.lane;
                            incoming.created_at = previous.created_at;
                            incoming
                        },
                        MailAnnotationState::NeedsApproval if actionable && !active_claim => {
                            incoming.id = previous.id.clone();
                            incoming.lane = previous.lane;
                            incoming.state = previous.state;
                            incoming.created_at = previous.created_at;
                            incoming
                        },
                        MailAnnotationState::NeedsApproval if provisional_repeat_suppression => {
                            incoming.id = previous.id.clone();
                            incoming.lane = previous.lane;
                            incoming.created_at = previous.created_at;
                            incoming
                        },
                        _ => {
                            let mut preserved = previous.clone();
                            preserved.evidence_refs = incoming.evidence_refs.clone();
                            preserved.evidence_message_id = incoming.evidence_message_id.clone();
                            preserved.evidence_message_at = incoming.evidence_message_at;
                            preserved.classification_input_revision =
                                incoming.classification_input_revision;
                            preserved.semantic_features = incoming.semantic_features.clone();
                            preserved.provenance = incoming.provenance.clone();
                            preserved.schema_version = incoming.schema_version;
                            preserved.updated_at = incoming.updated_at;
                            preserved
                        },
                    };
                    preserve_successful_semantics_on_refresh(&previous, &mut updated);
                    let disposition = match previous_state {
                        MailAnnotationState::Observed | MailAnnotationState::Classified => {
                            ClassificationAnnotationDisposition::Reclassified
                        },
                        MailAnnotationState::NeedsApproval if actionable && !active_claim => {
                            ClassificationAnnotationDisposition::RefreshedNeedsApproval
                        },
                        MailAnnotationState::NeedsApproval if provisional_repeat_suppression => {
                            ClassificationAnnotationDisposition::Reclassified
                        },
                        MailAnnotationState::Dismissed => {
                            ClassificationAnnotationDisposition::PreservedDismissal
                        },
                        _ => ClassificationAnnotationDisposition::PreservedLifecycle,
                    };
                    update_annotation_record(
                        &conn,
                        &principal,
                        &workspace,
                        &previous,
                        &updated,
                        MailAssistActor::Worker,
                        serde_json::json!({
                            "action": "classification_revision_refresh",
                            "disposition": classification_disposition_key(disposition),
                            "classification_input_revision": expected_revision,
                            "previous_state": previous.state.as_db_str(),
                            "result_state": updated.state.as_db_str(),
                            "previous_label": previous.label,
                            "result_label": updated.label,
                        }),
                    )?;
                    (updated, disposition)
                } else {
                    insert_annotation_record(
                        &conn,
                        &principal,
                        &workspace,
                        &mut incoming,
                        MailAssistActor::Worker,
                        Some(serde_json::json!({
                            "action": "classification_revision_create",
                            "classification_input_revision": expected_revision,
                        })),
                    )?;
                    (incoming, ClassificationAnnotationDisposition::Created)
                };

                conn.execute(
                    "UPDATE mail_messages SET classify_attempts = 0, \
                         classify_next_retry_at = NULL, classify_last_error = NULL, \
                         classify_failed_at = NULL \
                     WHERE principal = ? AND workspace = ? AND provider = ? \
                       AND account_alias = ? AND message_id = ? \
                       AND distill_revision = ?",
                    params![
                        principal,
                        workspace,
                        stored.provider,
                        stored.account_alias,
                        evidence_message_id,
                        expected_revision
                    ],
                )?;
                Ok(ClassificationAnnotationApplyResult::Applied {
                    annotation: stored,
                    disposition,
                })
            })?;
            inner
                .maybe_checkpoint(&conn)
                .context("throttled checkpoint after classification annotation apply")?;
            Ok(result)
        })
        .await
        .context("mail assist apply_classification_annotation task panicked")?
    }

    /// Ensure deterministic required-action evidence is represented in the
    /// Follow-up lane before a resurfacing candidate is withheld. The record is
    /// provisional (`classification_input_revision = NULL`) when newly created,
    /// so the ordinary classifier can enrich it later without duplication.
    pub async fn ensure_required_action_annotation(
        &self,
        principal: &str,
        workspace: &str,
        annotation: MailThreadAnnotation,
        evidence_revision: i64,
    ) -> Result<RequiredActionAnnotationResult> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let mut incoming = annotation;
            let evidence_message_id = incoming
                .evidence_message_id
                .clone()
                .context("required-action annotation is missing exact message evidence")?;
            let inner = store.scope_inner(&principal, &workspace)?;
            let _write_guard = inner.acquire_write_guard()?;
            let conn = inner
                .write_conn
                .lock()
                .expect("mail assist write connection mutex poisoned");
            let result = with_transaction(&conn, || {
                let (current_revision, eligible) = query_message_classification_eligibility(
                    &conn,
                    &principal,
                    &workspace,
                    &MailThreadAnnotation {
                        classification_input_revision: Some(evidence_revision),
                        ..incoming.clone()
                    },
                )?;
                if !eligible || current_revision != Some(evidence_revision) {
                    return Ok(RequiredActionAnnotationResult::StaleInput { current_revision });
                }
                let exact = query_annotation_for_evidence(
                    &conn,
                    &principal,
                    &workspace,
                    &incoming.provider,
                    &incoming.account_alias,
                    &incoming.thread_id,
                    &evidence_message_id,
                )?;
                let exact_match = exact.is_some();
                let mut current = match exact {
                    Some(annotation) => Some(annotation),
                    None => query_refreshable_annotation_for_thread(
                        &conn,
                        &principal,
                        &workspace,
                        &incoming.provider,
                        &incoming.account_alias,
                        &incoming.thread_id,
                        true,
                    )?
                    .or(query_refreshable_annotation_for_thread(
                        &conn,
                        &principal,
                        &workspace,
                        &incoming.provider,
                        &incoming.account_alias,
                        &incoming.thread_id,
                        false,
                    )?),
                };
                if !exact_match {
                    if let Some(annotation) = current.as_ref() {
                        let active_claim = annotation.state == MailAnnotationState::NeedsApproval
                            && query_annotation_refresh_blocking_claim(
                                &conn,
                                &principal,
                                &workspace,
                                &annotation.id,
                                incoming.updated_at,
                            )?
                            .is_some();
                        if active_claim {
                            current = None;
                        }
                    }
                }

                if let Some(previous) = current {
                    if exact_match
                        && previous.state == MailAnnotationState::NeedsApproval
                        && query_annotation_refresh_blocking_claim(
                            &conn,
                            &principal,
                            &workspace,
                            &previous.id,
                            incoming.updated_at,
                        )?
                        .is_some()
                    {
                        return Ok(RequiredActionAnnotationResult::Applied {
                            annotation: previous,
                            disposition: RequiredActionAnnotationDisposition::PreservedLifecycle,
                        });
                    }
                    if exact_match
                        && previous.state == MailAnnotationState::Classified
                        && annotation_action_bool(&previous, "repeat_of_recently_handled")
                            == Some(true)
                    {
                        return Ok(RequiredActionAnnotationResult::Applied {
                            annotation: previous,
                            disposition: RequiredActionAnnotationDisposition::PreservedLifecycle,
                        });
                    }
                    if exact_match
                        && previous.state == MailAnnotationState::NeedsApproval
                        && (previous.classification_input_revision == Some(evidence_revision)
                            || annotation_action_revision(&previous) == Some(evidence_revision))
                    {
                        return Ok(RequiredActionAnnotationResult::Applied {
                            annotation: previous,
                            disposition:
                                RequiredActionAnnotationDisposition::RefreshedNeedsApproval,
                        });
                    }
                    let disposition = match previous.state {
                        MailAnnotationState::Observed | MailAnnotationState::Classified => {
                            RequiredActionAnnotationDisposition::PromotedPassive
                        },
                        MailAnnotationState::NeedsApproval => {
                            RequiredActionAnnotationDisposition::RefreshedNeedsApproval
                        },
                        MailAnnotationState::Dismissed => {
                            return Ok(RequiredActionAnnotationResult::Applied {
                                annotation: previous,
                                disposition:
                                    RequiredActionAnnotationDisposition::PreservedDismissal,
                            });
                        },
                        _ => {
                            return Ok(RequiredActionAnnotationResult::Applied {
                                annotation: previous,
                                disposition:
                                    RequiredActionAnnotationDisposition::PreservedLifecycle,
                            });
                        },
                    };
                    incoming.id = previous.id.clone();
                    incoming.lane = previous.lane;
                    incoming.created_at = previous.created_at;
                    // A provisional refresh cannot claim that the classifier
                    // consumed the new revision. Preserve the prior receipt.
                    incoming.classification_input_revision = previous.classification_input_revision;
                    incoming.semantic_features = previous.semantic_features.clone();
                    update_annotation_record(
                        &conn,
                        &principal,
                        &workspace,
                        &previous,
                        &incoming,
                        MailAssistActor::Worker,
                        serde_json::json!({
                            "action": "required_action_reroute",
                            "disposition": required_action_disposition_key(disposition),
                            "evidence_revision": evidence_revision,
                            "previous_state": previous.state.as_db_str(),
                            "result_state": incoming.state.as_db_str(),
                        }),
                    )?;
                    Ok(RequiredActionAnnotationResult::Applied {
                        annotation: incoming,
                        disposition,
                    })
                } else {
                    incoming.classification_input_revision = None;
                    insert_annotation_record(
                        &conn,
                        &principal,
                        &workspace,
                        &mut incoming,
                        MailAssistActor::Worker,
                        Some(serde_json::json!({
                            "action": "required_action_reroute",
                            "disposition": "created",
                            "evidence_revision": evidence_revision,
                            "provisional": true,
                        })),
                    )?;
                    Ok(RequiredActionAnnotationResult::Applied {
                        annotation: incoming,
                        disposition: RequiredActionAnnotationDisposition::Created,
                    })
                }
            })?;
            inner
                .maybe_checkpoint(&conn)
                .context("throttled checkpoint after required-action annotation ensure")?;
            Ok(result)
        })
        .await
        .context("mail assist ensure_required_action_annotation task panicked")?
    }

    /// Transition an annotation's lifecycle state and append the
    /// corresponding audit event atomically. Audit rows are NEVER deleted
    /// or overwritten — dismissal is a transition to `dismissed`, not a
    /// delete. Returns the updated annotation.
    pub async fn transition_annotation(
        &self,
        principal: &str,
        workspace: &str,
        annotation_id: &str,
        new_state: MailAnnotationState,
        actor: MailAssistActor,
        detail: Option<serde_json::Value>,
        occurred_at: i64,
    ) -> Result<MailThreadAnnotation> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let annotation_id = annotation_id.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let _write_guard = inner.acquire_write_guard()?;
            let conn = inner
                .write_conn
                .lock()
                .expect("mail assist write connection mutex poisoned");
            let updated = with_transaction(&conn, || {
                // Read `from_state` INSIDE the write transaction (and under
                // the cross-process write lock) so a concurrent transition
                // can't slip between the read and the update and forge the
                // audit trail (TOCTOU).
                let current = query_annotation(&conn, &principal, &workspace, &annotation_id)?
                    .with_context(|| {
                        format!("mail annotation not found for transition: {annotation_id}")
                    })?;
                let event_type = if new_state == MailAnnotationState::Dismissed {
                    MailAssistEventType::Dismissed
                } else {
                    MailAssistEventType::StateTransition
                };
                let event = MailAssistEvent {
                    schema_version: current.schema_version,
                    id: Uuid::new_v4().to_string(),
                    annotation_id: Some(current.id.clone()),
                    provider: current.provider.clone(),
                    account_alias: current.account_alias.clone(),
                    thread_id: Some(current.thread_id.clone()),
                    event_type,
                    actor,
                    from_state: Some(current.state),
                    to_state: Some(new_state),
                    detail,
                    created_at: occurred_at,
                };
                conn.execute(
                    "UPDATE mail_annotations SET state = ?, updated_at = ? \
                     WHERE principal = ? AND workspace = ? AND id = ?",
                    params![
                        new_state.as_db_str(),
                        occurred_at,
                        principal,
                        workspace,
                        annotation_id
                    ],
                )
                .context("updating mail annotation state")?;
                insert_event(&conn, &principal, &workspace, &event)?;
                let mut updated = current;
                updated.state = new_state;
                updated.updated_at = occurred_at;
                Ok(updated)
            })?;
            inner
                .maybe_checkpoint(&conn)
                .context("throttled checkpoint after mail annotation transition")?;
            Ok(updated)
        })
        .await
        .context("mail assist transition_annotation task panicked")?
    }

    /// Transition an annotation only if it is still in `expected_state`.
    /// Feedback, when supplied, is appended in the same transaction so stale
    /// actions cannot leave behind misleading learning signals.
    pub async fn transition_annotation_if_state(
        &self,
        principal: &str,
        workspace: &str,
        annotation_id: &str,
        expected_state: MailAnnotationState,
        new_state: MailAnnotationState,
        actor: MailAssistActor,
        detail: Option<serde_json::Value>,
        feedback: Option<AnnotationTransitionFeedback>,
        occurred_at: i64,
    ) -> Result<AnnotationTransitionResult> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let annotation_id = annotation_id.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let _write_guard = inner.acquire_write_guard()?;
            let conn = inner
                .write_conn
                .lock()
                .expect("mail assist write connection mutex poisoned");
            let result = with_transaction(&conn, || {
                let current = match query_annotation(&conn, &principal, &workspace, &annotation_id)?
                {
                    Some(current) => current,
                    None => return Ok(AnnotationTransitionResult::NotFound),
                };
                if let Some(action) = query_active_annotation_action_claim(
                    &conn,
                    &principal,
                    &workspace,
                    &annotation_id,
                    occurred_at,
                )? {
                    return Ok(AnnotationTransitionResult::ActionInProgress { current, action });
                }
                if current.state != expected_state {
                    return Ok(AnnotationTransitionResult::UnexpectedState {
                        current,
                        expected: expected_state,
                    });
                }
                let event_type = if new_state == MailAnnotationState::Dismissed {
                    MailAssistEventType::Dismissed
                } else {
                    MailAssistEventType::StateTransition
                };
                let event = MailAssistEvent {
                    schema_version: current.schema_version,
                    id: Uuid::new_v4().to_string(),
                    annotation_id: Some(current.id.clone()),
                    provider: current.provider.clone(),
                    account_alias: current.account_alias.clone(),
                    thread_id: Some(current.thread_id.clone()),
                    event_type,
                    actor,
                    from_state: Some(current.state),
                    to_state: Some(new_state),
                    detail,
                    created_at: occurred_at,
                };
                let changed = conn
                    .execute(
                        "UPDATE mail_annotations SET state = ?, updated_at = ? \
                         WHERE principal = ? AND workspace = ? AND id = ? AND state = ?",
                        params![
                            new_state.as_db_str(),
                            occurred_at,
                            principal,
                            workspace,
                            annotation_id,
                            expected_state.as_db_str()
                        ],
                    )
                    .context("updating mail annotation state with expected state")?;
                if changed == 0 {
                    let current = query_annotation(&conn, &principal, &workspace, &annotation_id)?
                        .context("mail annotation disappeared during transition")?;
                    return Ok(AnnotationTransitionResult::UnexpectedState {
                        current,
                        expected: expected_state,
                    });
                }
                if let Some(feedback) = feedback.as_ref() {
                    insert_transition_feedback_event(
                        &conn,
                        &principal,
                        &workspace,
                        &current,
                        actor,
                        feedback,
                        occurred_at,
                    )?;
                }
                insert_event(&conn, &principal, &workspace, &event)?;
                let mut updated = current;
                updated.state = new_state;
                updated.updated_at = occurred_at;
                Ok(AnnotationTransitionResult::Applied(updated))
            })?;
            inner
                .maybe_checkpoint(&conn)
                .context("throttled checkpoint after guarded mail annotation transition")?;
            Ok(result)
        })
        .await
        .context("mail assist transition_annotation_if_state task panicked")?
    }

    /// Claim an annotation action before performing side effects outside the
    /// channel-assist transaction. Only the first caller gets `Claimed`; later
    /// callers observe the existing claim and must not repeat the side effect.
    pub async fn begin_annotation_action_claim(
        &self,
        principal: &str,
        workspace: &str,
        annotation_id: &str,
        action: &str,
        expected_state: MailAnnotationState,
        occurred_at: i64,
    ) -> Result<AnnotationActionClaimResult> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let annotation_id = annotation_id.to_string();
        let action = action.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let _write_guard = inner.acquire_write_guard()?;
            let conn = inner
                .write_conn
                .lock()
                .expect("mail assist write connection mutex poisoned");
            let result = with_transaction(&conn, || {
                let current = match query_annotation(&conn, &principal, &workspace, &annotation_id)?
                {
                    Some(current) => current,
                    None => return Ok(AnnotationActionClaimResult::NotFound),
                };
                if let Some(claim) = query_annotation_action_claim(
                    &conn,
                    &principal,
                    &workspace,
                    &annotation_id,
                    &action,
                )? {
                    if claim.task_id.is_some()
                        || occurred_at.saturating_sub(claim.updated_at)
                            < ANNOTATION_ACTION_CLAIM_STALE_MS
                    {
                        return Ok(AnnotationActionClaimResult::Existing {
                            annotation: current,
                            task_id: claim.task_id,
                        });
                    }
                    conn.execute(
                        "DELETE FROM mail_annotation_action_claims \
                         WHERE principal = ? AND workspace = ? AND annotation_id = ? \
                           AND action = ? AND claim_id = ? AND task_id IS NULL",
                        params![
                            principal.as_str(),
                            workspace.as_str(),
                            annotation_id.as_str(),
                            action.as_str(),
                            claim.claim_id.as_str()
                        ],
                    )
                    .context("reclaiming stale mail annotation action claim")?;
                }
                if current.state != expected_state {
                    return Ok(AnnotationActionClaimResult::UnexpectedState {
                        current,
                        expected: expected_state,
                    });
                }
                let claim_id = Uuid::new_v4().to_string();
                conn.execute(
                    "INSERT INTO mail_annotation_action_claims (
                        principal, workspace, annotation_id, action, claim_id,
                        task_id, created_at, updated_at, schema_version
                    ) VALUES (?, ?, ?, ?, ?, NULL, ?, ?, ?)",
                    params![
                        principal.as_str(),
                        workspace.as_str(),
                        annotation_id.as_str(),
                        action.as_str(),
                        claim_id.as_str(),
                        occurred_at,
                        occurred_at,
                        current.schema_version,
                    ],
                )
                .context("inserting mail annotation action claim")?;
                Ok(AnnotationActionClaimResult::Claimed {
                    annotation: current,
                    claim_id,
                })
            })?;
            inner
                .maybe_checkpoint(&conn)
                .context("throttled checkpoint after mail annotation action claim")?;
            Ok(result)
        })
        .await
        .context("mail assist begin_annotation_action_claim task panicked")?
    }

    /// Complete a claimed action by writing its side-effect id and applying the
    /// expected-state transition atomically.
    pub async fn complete_annotation_action_claim(
        &self,
        principal: &str,
        workspace: &str,
        annotation_id: &str,
        action: &str,
        claim_id: &str,
        task_id: &str,
        expected_state: MailAnnotationState,
        new_state: MailAnnotationState,
        actor: MailAssistActor,
        detail: Option<serde_json::Value>,
        feedback: Option<AnnotationTransitionFeedback>,
        occurred_at: i64,
    ) -> Result<AnnotationTransitionResult> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let annotation_id = annotation_id.to_string();
        let action = action.to_string();
        let claim_id = claim_id.to_string();
        let task_id = task_id.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let _write_guard = inner.acquire_write_guard()?;
            let conn = inner
                .write_conn
                .lock()
                .expect("mail assist write connection mutex poisoned");
            let result = with_transaction(&conn, || {
                let current = match query_annotation(&conn, &principal, &workspace, &annotation_id)?
                {
                    Some(current) => current,
                    None => return Ok(AnnotationTransitionResult::NotFound),
                };
                let Some(stored_claim) = query_annotation_action_claim(
                    &conn,
                    &principal,
                    &workspace,
                    &annotation_id,
                    &action,
                )?
                else {
                    anyhow::bail!("mail annotation action claim missing: {annotation_id}/{action}");
                };
                if stored_claim.claim_id != claim_id {
                    anyhow::bail!(
                        "mail annotation action claim mismatch: {annotation_id}/{action}"
                    );
                }
                if current.state != expected_state {
                    return Ok(AnnotationTransitionResult::UnexpectedState {
                        current,
                        expected: expected_state,
                    });
                }
                conn.execute(
                    "UPDATE mail_annotation_action_claims SET task_id = ?, updated_at = ? \
                     WHERE principal = ? AND workspace = ? AND annotation_id = ? \
                       AND action = ? AND claim_id = ?",
                    params![
                        task_id,
                        occurred_at,
                        principal,
                        workspace,
                        annotation_id,
                        action,
                        claim_id,
                    ],
                )
                .context("completing mail annotation action claim")?;
                let changed = conn
                    .execute(
                        "UPDATE mail_annotations SET state = ?, updated_at = ? \
                         WHERE principal = ? AND workspace = ? AND id = ? AND state = ?",
                        params![
                            new_state.as_db_str(),
                            occurred_at,
                            principal,
                            workspace,
                            annotation_id,
                            expected_state.as_db_str()
                        ],
                    )
                    .context("updating claimed mail annotation state")?;
                if changed == 0 {
                    let current = query_annotation(&conn, &principal, &workspace, &annotation_id)?
                        .context("mail annotation disappeared during claim completion")?;
                    return Ok(AnnotationTransitionResult::UnexpectedState {
                        current,
                        expected: expected_state,
                    });
                }
                if let Some(feedback) = feedback.as_ref() {
                    insert_transition_feedback_event(
                        &conn,
                        &principal,
                        &workspace,
                        &current,
                        actor,
                        feedback,
                        occurred_at,
                    )?;
                }
                let event = MailAssistEvent {
                    schema_version: current.schema_version,
                    id: Uuid::new_v4().to_string(),
                    annotation_id: Some(current.id.clone()),
                    provider: current.provider.clone(),
                    account_alias: current.account_alias.clone(),
                    thread_id: Some(current.thread_id.clone()),
                    event_type: MailAssistEventType::StateTransition,
                    actor,
                    from_state: Some(current.state),
                    to_state: Some(new_state),
                    detail,
                    created_at: occurred_at,
                };
                insert_event(&conn, &principal, &workspace, &event)?;
                let mut updated = current;
                updated.state = new_state;
                updated.updated_at = occurred_at;
                Ok(AnnotationTransitionResult::Applied(updated))
            })?;
            inner
                .maybe_checkpoint(&conn)
                .context("throttled checkpoint after mail annotation action completion")?;
            Ok(result)
        })
        .await
        .context("mail assist complete_annotation_action_claim task panicked")?
    }

    /// Remove an uncompleted claim after its side effect failed before any
    /// state transition, allowing a later retry.
    pub async fn abort_annotation_action_claim(
        &self,
        principal: &str,
        workspace: &str,
        annotation_id: &str,
        action: &str,
        claim_id: &str,
    ) -> Result<()> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let annotation_id = annotation_id.to_string();
        let action = action.to_string();
        let claim_id = claim_id.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let _write_guard = inner.acquire_write_guard()?;
            let conn = inner
                .write_conn
                .lock()
                .expect("mail assist write connection mutex poisoned");
            conn.execute(
                "DELETE FROM mail_annotation_action_claims \
                 WHERE principal = ? AND workspace = ? AND annotation_id = ? \
                   AND action = ? AND claim_id = ? AND task_id IS NULL",
                params![principal, workspace, annotation_id, action, claim_id],
            )
            .context("aborting mail annotation action claim")?;
            inner
                .maybe_checkpoint(&conn)
                .context("throttled checkpoint after mail annotation action abort")?;
            Ok(())
        })
        .await
        .context("mail assist abort_annotation_action_claim task panicked")?
    }

    /// Append a typed feedback record as a `feedback` audit event (the
    /// record itself travels in the event detail).
    pub async fn append_feedback(
        &self,
        principal: &str,
        workspace: &str,
        feedback: MailAssistUserFeedback,
    ) -> Result<()> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let detail =
                serde_json::to_value(&feedback).context("serializing mail assist feedback")?;
            let event = MailAssistEvent {
                schema_version: feedback.schema_version,
                id: feedback.id.clone(),
                annotation_id: Some(feedback.annotation_id.clone()),
                provider: feedback.provider.clone(),
                account_alias: feedback.account_alias.clone(),
                thread_id: feedback.thread_id.clone(),
                event_type: MailAssistEventType::Feedback,
                actor: feedback.actor,
                from_state: None,
                to_state: None,
                detail: Some(detail),
                created_at: feedback.created_at,
            };
            let _write_guard = inner.acquire_write_guard()?;
            let conn = inner
                .write_conn
                .lock()
                .expect("mail assist write connection mutex poisoned");
            insert_event(&conn, &principal, &workspace, &event)?;
            inner
                .maybe_checkpoint(&conn)
                .context("throttled checkpoint after mail assist feedback")?;
            Ok(())
        })
        .await
        .context("mail assist append_feedback task panicked")?
    }

    pub async fn get_annotation(
        &self,
        principal: &str,
        workspace: &str,
        annotation_id: &str,
    ) -> Result<Option<MailThreadAnnotation>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let annotation_id = annotation_id.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            query_annotation(&conn, &principal, &workspace, &annotation_id)
        })
        .await
        .context("mail assist get_annotation task panicked")?
    }

    /// Semantics-only compare-and-set used by background coverage repair.
    /// It never rewrites the legacy label, state, required action, lane,
    /// provenance, or classification revision. False means the source was
    /// revised, completed, or removed while extraction was in flight.
    pub async fn update_semantic_features_if_revision(
        &self,
        principal: &str,
        workspace: &str,
        annotation_id: &str,
        expected_classification_revision: i64,
        semantic_envelope: &serde_json::Value,
    ) -> Result<bool> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let annotation_id = annotation_id.to_string();
        let semantic_envelope = semantic_envelope.clone();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let _write_guard = inner.acquire_write_guard()?;
            let conn = inner
                .write_conn
                .lock()
                .expect("mail assist write connection mutex poisoned");
            let mut incoming =
                match query_annotation(&conn, &principal, &workspace, &annotation_id)? {
                    Some(current) => {
                        let mut incoming = current.clone();
                        incoming.classification_input_revision =
                            Some(expected_classification_revision);
                        incoming.semantic_features = Some(semantic_envelope);
                        preserve_successful_semantics_on_refresh(&current, &mut incoming);
                        incoming
                    },
                    None => return Ok(false),
                };
            let semantic_envelope = serde_json::to_string(&incoming.semantic_features.take())
                .context("serializing mail semantic extraction envelope")?;
            let changed = conn.execute(
                "UPDATE mail_annotations SET semantic_features_json = ?
                 WHERE principal = ? AND workspace = ? AND id = ?
                   AND classification_input_revision = ?
                   AND state IN ('needs_approval', 'approved', 'scheduled',
                                 'draft_requested', 'draft_ready', 'inserted',
                                 'sent_detected')",
                params![
                    semantic_envelope,
                    principal,
                    workspace,
                    annotation_id,
                    expected_classification_revision,
                ],
            )?;
            inner.maybe_checkpoint(&conn)?;
            Ok(changed > 0)
        })
        .await
        .context("mail semantic extraction compare-and-set task panicked")?
    }

    /// Batch annotation lookup for the annotations API. Threads without
    /// annotations (or unknown thread ids) contribute nothing — an empty
    /// set is a valid answer.
    pub async fn list_annotations_by_thread_ids(
        &self,
        principal: &str,
        workspace: &str,
        provider: &str,
        account_alias: &str,
        thread_ids: &[String],
    ) -> Result<Vec<MailThreadAnnotation>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let provider = provider.to_string();
        let account_alias = account_alias.to_string();
        let thread_ids = thread_ids.to_vec();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let unique: std::collections::BTreeSet<_> = thread_ids.iter().cloned().collect();
            let unique: Vec<_> = unique.into_iter().collect();
            let mut by_thread: HashMap<String, Vec<MailThreadAnnotation>> = HashMap::new();
            for batch in unique.chunks(128) {
                let placeholders = vec!["?"; batch.len()].join(",");
                let sql = format!("SELECT {ANNOTATION_COLUMNS} FROM mail_annotations WHERE principal=? AND workspace=? AND provider=? AND account_alias=? AND thread_id IN ({placeholders}) ORDER BY updated_at DESC,id");
                let mut values = vec![principal.clone(), workspace.clone(), provider.clone(), account_alias.clone()];
                values.extend(batch.iter().cloned());
                let mut statement = conn.prepare(&sql)?;
                let mut rows = statement.query(params_from_iter(values))?;
                while let Some(row) = rows.next()? {
                    let annotation = map_annotation_row(row)?;
                    by_thread.entry(annotation.thread_id.clone()).or_default().push(annotation);
                }
            }
            let mut annotations = Vec::new();
            for thread_id in thread_ids { if let Some(rows) = by_thread.get(&thread_id) { annotations.extend(rows.iter().cloned()); } }

            Ok(annotations)
        })
        .await
        .context("mail assist list_annotations_by_thread_ids task panicked")?
    }

    /// Fallback annotation lookup by evidence message. This is intentionally
    /// provider-scoped but account/thread-agnostic so legacy source refs whose
    /// account alias or thread id contained raw `/` can still suppress duplicate
    /// Worth-a-look resurfacing.
    pub async fn list_annotations_by_evidence_message_id(
        &self,
        principal: &str,
        workspace: &str,
        provider: &str,
        evidence_message_id: &str,
    ) -> Result<Vec<MailThreadAnnotation>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let provider = provider.to_string();
        let evidence_message_id = evidence_message_id.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(&format!(
                "SELECT {ANNOTATION_COLUMNS} \
                 FROM mail_annotations \
                 WHERE principal = ? AND workspace = ? AND provider = ? \
                   AND evidence_message_id = ? \
                 ORDER BY updated_at DESC, id",
            ))?;
            let mut rows =
                stmt.query(params![principal, workspace, provider, evidence_message_id])?;
            let mut annotations = Vec::new();
            while let Some(row) = rows.next()? {
                annotations.push(map_annotation_row(row)?);
            }
            Ok(annotations)
        })
        .await
        .context("mail assist list_annotations_by_evidence_message_id task panicked")?
    }

    /// Audit trail for one annotation, oldest first.
    pub async fn list_events_for_annotation(
        &self,
        principal: &str,
        workspace: &str,
        annotation_id: &str,
    ) -> Result<Vec<MailAssistEvent>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let annotation_id = annotation_id.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(
                "SELECT id, annotation_id, provider, account_alias, thread_id, event_type, \
                 actor, from_state, to_state, detail_json, created_at, schema_version \
                 FROM mail_assist_events \
                 WHERE principal = ? AND workspace = ? AND annotation_id = ? \
                 ORDER BY created_at, id",
            )?;
            let mut rows = stmt.query(params![principal, workspace, annotation_id])?;
            let mut events = Vec::new();
            while let Some(row) = rows.next()? {
                events.push(map_event_row(row)?);
            }
            Ok(events)
        })
        .await
        .context("mail assist list_events_for_annotation task panicked")?
    }

    /// Active annotations that may be reconciled as newer channel evidence
    /// arrives. This is provider-neutral: every current/future adapter writes
    /// the same annotation state machine and message table.
    pub async fn list_active_annotations_for_reconcile(
        &self,
        principal: &str,
        workspace: &str,
        stale_before: Option<i64>,
        limit: usize,
    ) -> Result<Vec<MailThreadAnnotation>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(&format!(
                "SELECT {ANNOTATION_COLUMNS} \
                 FROM mail_annotations \
                 WHERE principal = ? AND workspace = ? \
                   AND state IN ( \
                     'needs_approval', 'approved', 'scheduled', 'draft_requested', \
                     'draft_ready', 'inserted', 'sent_detected' \
                   ) \
                   AND ( \
                     EXISTS ( \
                     SELECT 1 FROM mail_messages m \
                       WHERE m.principal = mail_annotations.principal \
                         AND m.workspace = mail_annotations.workspace \
                         AND m.provider = mail_annotations.provider \
                         AND m.account_alias = mail_annotations.account_alias \
                         AND m.thread_id = mail_annotations.thread_id \
                         AND (m.internal_date > COALESCE(mail_annotations.evidence_message_at, mail_annotations.created_at) \
                              OR (m.internal_date = COALESCE(mail_annotations.evidence_message_at, mail_annotations.created_at) \
                                  AND m.message_id > COALESCE(mail_annotations.evidence_message_id, ''))) \
                       LIMIT 1 \
                     ) \
                     OR EXISTS ( \
                       SELECT 1 FROM mail_assist_events e \
                       WHERE e.principal = mail_annotations.principal \
                         AND e.workspace = mail_annotations.workspace \
                         AND e.provider = mail_annotations.provider \
                         AND e.account_alias = mail_annotations.account_alias \
                         AND e.thread_id = mail_annotations.thread_id \
                         AND e.event_type = 'provider_change' \
                         AND e.created_at >= mail_annotations.created_at \
                       LIMIT 1 \
                     ) \
                     OR (? IS NOT NULL \
                         AND COALESCE(evidence_message_at, created_at) <= ?) \
                   ) \
                 ORDER BY updated_at ASC, id ASC LIMIT ?",
            ))?;
            let mut rows =
                stmt.query(params![principal, workspace, stale_before, stale_before, limit as i64])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push(map_annotation_row(row)?);
            }
            Ok(out)
        })
        .await
        .context("mail assist list_active_annotations_for_reconcile task panicked")?
    }

    /// Active annotations that were promoted by the information brief alone.
    ///
    /// The reconcile scan only surfaces threads with newer messages, a provider
    /// change, or 30 days of age, so a brief-routed annotation on a quiet thread
    /// would never be revisited. A routing-rule change needs to reach exactly
    /// those rows, so this selects them directly and pages by `updated_at` —
    /// each repaired row leaves the active set, so the sweep drains.
    pub async fn list_active_brief_routed_annotations(
        &self,
        principal: &str,
        workspace: &str,
        limit: usize,
    ) -> Result<Vec<MailThreadAnnotation>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(&format!(
                "SELECT {ANNOTATION_COLUMNS} \
                 FROM mail_annotations \
                 WHERE principal = ? AND workspace = ? \
                   AND state IN ( \
                     'needs_approval', 'approved', 'scheduled', 'draft_requested', \
                     'draft_ready', 'inserted', 'sent_detected' \
                   ) \
                   AND json_extract_string(proposed_action_json, '$.required_action_source') \
                       = 'information_brief' \
                 ORDER BY updated_at ASC, id ASC LIMIT ?",
            ))?;
            let mut rows = stmt.query(params![principal, workspace, limit as i64])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push(map_annotation_row(row)?);
            }
            Ok(out)
        })
        .await
        .context("mail assist list_active_brief_routed_annotations task panicked")?
    }

    /// Stable full-active scan used only to discover revision-bound semantic
    /// coverage work. Existing labels and lifecycle state are never changed.
    pub async fn list_active_semantic_annotations(
        &self,
        principal: &str,
        workspace: &str,
        after_annotation_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MailThreadAnnotation>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let after_annotation_id = after_annotation_id.unwrap_or_default().to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(&format!(
                "SELECT {ANNOTATION_COLUMNS} FROM mail_annotations
                 WHERE principal = ? AND workspace = ?
                   AND state IN ('needs_approval', 'approved', 'scheduled',
                                 'draft_requested', 'draft_ready', 'inserted',
                                 'sent_detected')
                   AND id > ?
                 ORDER BY id ASC LIMIT ?"
            ))?;
            let mut rows = stmt.query(params![
                principal,
                workspace,
                after_annotation_id,
                limit as i64
            ])?;
            let mut annotations = Vec::new();
            while let Some(row) = rows.next()? {
                annotations.push(map_annotation_row(row)?);
            }
            Ok(annotations)
        })
        .await
        .context("mail active semantic annotation scan task panicked")?
    }

    pub async fn count_active_semantic_annotations(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<u64> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM mail_annotations
                 WHERE principal = ? AND workspace = ?
                   AND state IN ('needs_approval', 'approved', 'scheduled',
                                 'draft_requested', 'draft_ready', 'inserted',
                                 'sent_detected')
                 ",
                params![principal, workspace],
                |row| row.get(0),
            )?;
            Ok(count.max(0) as u64)
        })
        .await
        .context("mail active semantic annotation count task panicked")?
    }

    /// Messages in the same thread after an annotation's evidence cursor.
    /// Direction and provider metadata are available immediately, so manual
    /// sends and draft invalidation do not wait for distillation. Summaries and
    /// hints are used when already present; raw bodies are never read.
    pub async fn list_reconcile_messages_after(
        &self,
        principal: &str,
        workspace: &str,
        annotation: &MailThreadAnnotation,
        limit: usize,
    ) -> Result<Vec<MailMessageMeta>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let provider = annotation.provider.clone();
        let account_alias = annotation.account_alias.clone();
        let thread_id = annotation.thread_id.clone();
        let evidence_at = annotation
            .evidence_message_at
            .unwrap_or(annotation.created_at);
        let evidence_message_id = annotation.evidence_message_id.clone().unwrap_or_default();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(&format!(
                "SELECT {MESSAGE_COLUMNS} \
                 FROM mail_messages \
                 WHERE principal = ? AND workspace = ? AND provider = ? \
                   AND account_alias = ? AND thread_id = ? \
                   AND (internal_date > ? \
                        OR (internal_date = ? AND message_id > ?)) \
                 ORDER BY internal_date ASC, message_id ASC LIMIT ?",
            ))?;
            let mut rows = stmt.query(params![
                principal,
                workspace,
                provider,
                account_alias,
                thread_id,
                evidence_at,
                evidence_at,
                evidence_message_id,
                limit as i64
            ])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push(map_message_row(row)?);
            }
            Ok(out)
        })
        .await
        .context("mail assist list_reconcile_messages_after task panicked")?
    }

    /// The message an annotation was derived from.
    ///
    /// The routing repair re-derives the required action from this message's
    /// stored distill brief, so it needs the evidence itself rather than the
    /// newer messages `list_reconcile_messages_after` returns.
    pub async fn load_annotation_evidence_message(
        &self,
        principal: &str,
        workspace: &str,
        annotation: &MailThreadAnnotation,
    ) -> Result<Option<MailMessageMeta>> {
        let Some(evidence_message_id) = annotation
            .evidence_message_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
        else {
            return Ok(None);
        };
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let provider = annotation.provider.clone();
        let account_alias = annotation.account_alias.clone();
        let thread_id = annotation.thread_id.clone();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(&format!(
                "SELECT {MESSAGE_COLUMNS} \
                 FROM mail_messages \
                 WHERE principal = ? AND workspace = ? AND provider = ? \
                   AND account_alias = ? AND thread_id = ? AND message_id = ? \
                 LIMIT 1",
            ))?;
            let mut rows = stmt.query(params![
                principal,
                workspace,
                provider,
                account_alias,
                thread_id,
                evidence_message_id
            ])?;
            match rows.next()? {
                Some(row) => Ok(Some(map_message_row(row)?)),
                None => Ok(None),
            }
        })
        .await
        .context("mail assist load_annotation_evidence_message task panicked")?
    }

    /// Provider-side deltas in this thread after the annotation was created.
    /// This covers archive/delete/trash changes that do not produce a new
    /// message row. Malformed audit details are ignored rather than breaking
    /// the reconciliation pass.
    pub async fn list_provider_changes_after(
        &self,
        principal: &str,
        workspace: &str,
        annotation: &MailThreadAnnotation,
        limit: usize,
    ) -> Result<Vec<ProviderThreadChange>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let provider = annotation.provider.clone();
        let account_alias = annotation.account_alias.clone();
        let thread_id = annotation.thread_id.clone();
        let created_at = annotation.created_at;
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(
                "SELECT id, detail_json, created_at, schema_version \
                 FROM mail_assist_events \
                 WHERE principal = ? AND workspace = ? AND provider = ? \
                   AND account_alias = ? AND thread_id = ? \
                   AND event_type = 'provider_change' AND created_at >= ? \
                 ORDER BY created_at ASC, id ASC LIMIT ?",
            )?;
            let mut rows = stmt.query(params![
                principal,
                workspace,
                provider,
                account_alias,
                thread_id,
                created_at,
                limit as i64,
            ])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                let detail_json: Option<String> = row.get(1)?;
                let Some(detail) = detail_json
                    .as_deref()
                    .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
                else {
                    continue;
                };
                let Some(kind) = detail
                    .get("kind")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|value| ProviderThreadChangeKind::from_db_str(value).ok())
                else {
                    continue;
                };
                out.push(ProviderThreadChange {
                    schema_version: row.get::<_, i64>(3)? as u32,
                    id: row.get(0)?,
                    provider: provider.clone(),
                    account_alias: account_alias.clone(),
                    thread_id: thread_id.clone(),
                    message_id: detail
                        .get("message_id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    kind,
                    thread_removed: detail
                        .get("thread_removed")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    label_ids: detail
                        .get("label_ids")
                        .and_then(serde_json::Value::as_array)
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(serde_json::Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default(),
                    current_label_ids: detail
                        .get("current_label_ids")
                        .and_then(serde_json::Value::as_array)
                        .map(|values| {
                            values
                                .iter()
                                .filter_map(serde_json::Value::as_str)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default(),
                    provider_cursor: detail
                        .get("provider_cursor")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    observed_at: row.get(2)?,
                });
            }
            Ok(out)
        })
        .await
        .context("mail assist list_provider_changes_after task panicked")?
    }

    /// Whether a newer message already has any annotation, including quiet
    /// `classified`. Reconciliation waits for this before superseding an older
    /// active card so classifier delays do not make follow-ups disappear.
    pub async fn annotation_exists_for_evidence(
        &self,
        principal: &str,
        workspace: &str,
        provider: &str,
        account_alias: &str,
        thread_id: &str,
        evidence_message_id: &str,
    ) -> Result<bool> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let provider = provider.to_string();
        let account_alias = account_alias.to_string();
        let thread_id = thread_id.to_string();
        let evidence_message_id = evidence_message_id.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let exists: bool = conn.query_row(
                "SELECT EXISTS ( \
                    SELECT 1 FROM mail_annotations \
                    WHERE principal = ? AND workspace = ? AND provider = ? \
                      AND account_alias = ? AND thread_id = ? \
                      AND evidence_message_id = ? \
                 )",
                params![
                    principal,
                    workspace,
                    provider,
                    account_alias,
                    thread_id,
                    evidence_message_id
                ],
                |row| row.get(0),
            )?;
            Ok(exists)
        })
        .await
        .context("mail assist annotation_exists_for_evidence task panicked")?
    }

    /// Recently handled follow-ups in this provider-neutral thread. The
    /// classifier sees this bounded context and decides whether the new message
    /// is merely a repeat or materially new enough to surface again.
    pub async fn recent_handled_follow_ups(
        &self,
        principal: &str,
        workspace: &str,
        provider: &str,
        account_alias: &str,
        thread_id: &str,
        since_ms: i64,
        limit: usize,
    ) -> Result<Vec<RecentHandledFollowUp>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let provider = provider.to_string();
        let account_alias = account_alias.to_string();
        let thread_id = thread_id.to_string();
        let limit = limit.clamp(1, 5) as i64;
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(
                "SELECT state, label, reason, proposed_action_json, updated_at \
                 FROM mail_annotations \
                 WHERE principal = ? AND workspace = ? AND provider = ? \
                   AND account_alias = ? AND thread_id = ? \
                   AND state IN ('acknowledged', 'dismissed') \
                   AND updated_at >= ? \
                 ORDER BY updated_at DESC, id DESC LIMIT ?",
            )?;
            let mut rows = stmt.query(params![
                principal,
                workspace,
                provider,
                account_alias,
                thread_id,
                since_ms,
                limit
            ])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                let state: String = row.get(0)?;
                let proposed_action_json: Option<String> = row.get(3)?;
                out.push(RecentHandledFollowUp {
                    state: MailAnnotationState::from_db_str(&state)?,
                    label: row.get(1)?,
                    reason: row.get(2)?,
                    proposed_action: proposed_action_json
                        .as_deref()
                        .map(serde_json::from_str)
                        .transpose()
                        .context("parsing recent handled follow-up action JSON")?,
                    updated_at: row.get(4)?,
                });
            }
            Ok(out)
        })
        .await
        .context("mail assist recent_handled_follow_ups task panicked")?
    }

    pub async fn get_watermark(
        &self,
        principal: &str,
        workspace: &str,
        provider: &str,
        account_alias: &str,
    ) -> Result<Option<SyncWatermark>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let provider = provider.to_string();
        let account_alias = account_alias.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(
                "SELECT provider, account_alias, last_internal_date, provider_cursor, \
                 last_synced_at, last_error, schema_version \
                 FROM mail_sync_watermarks \
                 WHERE principal = ? AND workspace = ? AND provider = ? AND account_alias = ?",
            )?;
            let mut rows = stmt.query(params![principal, workspace, provider, account_alias])?;
            if let Some(row) = rows.next()? {
                Ok(Some(SyncWatermark {
                    provider: row.get(0)?,
                    account_alias: row.get(1)?,
                    last_internal_date: row.get(2)?,
                    provider_cursor: row.get(3)?,
                    last_synced_at: row.get(4)?,
                    last_error: row.get(5)?,
                    schema_version: row.get::<_, u32>(6)?,
                }))
            } else {
                Ok(None)
            }
        })
        .await
        .context("mail assist get_watermark task panicked")?
    }

    pub async fn set_watermark(
        &self,
        principal: &str,
        workspace: &str,
        watermark: SyncWatermark,
    ) -> Result<()> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let _write_guard = inner.acquire_write_guard()?;
            let conn = inner
                .write_conn
                .lock()
                .expect("mail assist write connection mutex poisoned");
            conn.execute(
                "INSERT INTO mail_sync_watermarks (
                    principal, workspace, provider, account_alias,
                    last_internal_date, provider_cursor, last_synced_at, last_error,
                    schema_version
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT (principal, workspace, provider, account_alias) DO UPDATE SET
                    last_internal_date = excluded.last_internal_date,
                    provider_cursor = excluded.provider_cursor,
                    last_synced_at = excluded.last_synced_at,
                    last_error = excluded.last_error,
                    schema_version = excluded.schema_version",
                params![
                    principal,
                    workspace,
                    watermark.provider,
                    watermark.account_alias,
                    watermark.last_internal_date,
                    watermark.provider_cursor,
                    watermark.last_synced_at,
                    watermark.last_error,
                    watermark.schema_version,
                ],
            )
            .context("upserting mail sync watermark")?;
            inner
                .maybe_checkpoint(&conn)
                .context("throttled checkpoint after mail watermark set")?;
            Ok(())
        })
        .await
        .context("mail assist set_watermark task panicked")?
    }

    /// Per-account thread/message totals for `sync/status`, ordered by
    /// (provider, account_alias).
    pub async fn count_summary(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<MailAccountCounts>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut totals: HashMap<(String, String), (u64, u64)> = HashMap::new();
            {
                let mut stmt = conn.prepare(
                    "SELECT provider, account_alias, COUNT(*) FROM mail_threads \
                     WHERE principal = ? AND workspace = ? GROUP BY provider, account_alias",
                )?;
                let mut rows = stmt.query(params![principal.clone(), workspace.clone()])?;
                while let Some(row) = rows.next()? {
                    let key = (row.get::<_, String>(0)?, row.get::<_, String>(1)?);
                    totals.entry(key).or_default().0 = row.get::<_, i64>(2)? as u64;
                }
            }
            {
                let mut stmt = conn.prepare(
                    "SELECT provider, account_alias, COUNT(*) FROM mail_messages \
                     WHERE principal = ? AND workspace = ? GROUP BY provider, account_alias",
                )?;
                let mut rows = stmt.query(params![principal, workspace])?;
                while let Some(row) = rows.next()? {
                    let key = (row.get::<_, String>(0)?, row.get::<_, String>(1)?);
                    totals.entry(key).or_default().1 = row.get::<_, i64>(2)? as u64;
                }
            }
            let mut counts: Vec<MailAccountCounts> = totals
                .into_iter()
                .map(
                    |((provider, account_alias), (thread_count, message_count))| {
                        MailAccountCounts {
                            provider,
                            account_alias,
                            thread_count,
                            message_count,
                        }
                    },
                )
                .collect();
            counts.sort_by(|a, b| {
                (a.provider.as_str(), a.account_alias.as_str())
                    .cmp(&(b.provider.as_str(), b.account_alias.as_str()))
            });
            Ok(counts)
        })
        .await
        .context("mail assist count_summary task panicked")?
    }

    /// Total messages awaiting local distillation (`distill_state =
    /// 'pending'`) across all channels for the scope — the distill queue
    /// depth surfaced by `sync/status`.
    pub async fn count_pending_distill(&self, principal: &str, workspace: &str) -> Result<u64> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM mail_messages \
                 WHERE principal = ? AND workspace = ? AND distill_state = 'pending'",
                params![principal, workspace],
                |row| row.get(0),
            )?;
            Ok(count as u64)
        })
        .await
        .context("mail assist count_pending_distill task panicked")?
    }

    /// Distilled, non-suppressed messages completed after a scope-local
    /// distill revision. The revision is allocated atomically with each result,
    /// so correcting an old provider message is visible even though its
    /// `internal_date` is unchanged. Joins the thread subject as a fallback.
    pub async fn list_distilled_for_bridge(
        &self,
        principal: &str,
        workspace: &str,
        after_revision: i64,
        limit: usize,
    ) -> Result<Vec<DistilledBridgeRow>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(
                "SELECT m.provider, m.account_alias, m.thread_id, m.internal_date, \
                        m.summary, m.intent, COALESCE(m.subject, t.subject), \
                        m.from_name, m.from_address, m.needs_reply_hint, \
                        m.follow_up_hint_json, m.message_id, \
                        m.distill_brief_json, m.distill_revision \
                 FROM mail_messages m \
                 LEFT JOIN mail_threads t \
                   ON t.principal = m.principal AND t.workspace = m.workspace \
                   AND t.provider = m.provider AND t.account_alias = m.account_alias \
                   AND t.thread_id = m.thread_id \
                 WHERE m.principal = ? AND m.workspace = ? \
                   AND m.distill_state = 'done' AND m.sensitive_suppressed = FALSE \
                   AND m.distill_revision > ? \
                 ORDER BY m.distill_revision ASC LIMIT ?",
            )?;
            let mut rows =
                stmt.query(params![principal, workspace, after_revision, limit as i64])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push(DistilledBridgeRow {
                    provider: row.get(0)?,
                    account_alias: row.get(1)?,
                    thread_id: row.get(2)?,
                    internal_date: row.get(3)?,
                    summary: row.get(4)?,
                    intent: row.get(5)?,
                    subject: row.get(6)?,
                    from_name: row.get(7)?,
                    from_address: row.get(8)?,
                    needs_reply_hint: row.get(9)?,
                    follow_up_hint: {
                        let follow_up_hint_json: Option<String> = row.get(10)?;
                        follow_up_hint_json.as_deref().and_then(|json| {
                            decode_bridge_json::<ChannelFollowUpHint>(
                                json,
                                "follow_up_hint",
                                row.get::<_, i64>(13).unwrap_or_default(),
                            )
                        })
                    },
                    message_id: row.get(11)?,
                    distill_brief: {
                        let brief_json: Option<String> = row.get(12)?;
                        brief_json.as_deref().and_then(|json| {
                            decode_bridge_json::<ChannelInformationBrief>(
                                json,
                                "information_brief",
                                row.get::<_, i64>(13).unwrap_or_default(),
                            )
                        })
                    },
                    distill_revision: row.get(13)?,
                });
            }
            Ok(out)
        })
        .await
        .context("mail assist list_distilled_for_bridge task panicked")?
    }

    /// Attention hints for one distilled message. Resurfacing uses this to
    /// rehydrate comm/promise lane intent at routing time without copying those
    /// hints into the resurfacing corpus schema.
    pub async fn get_message_attention_hints(
        &self,
        principal: &str,
        workspace: &str,
        provider: &str,
        account_alias: &str,
        thread_id: &str,
        message_id: &str,
    ) -> Result<Option<MessageAttentionHints>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let provider = provider.to_string();
        let account_alias = account_alias.to_string();
        let thread_id = thread_id.to_string();
        let message_id = message_id.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(
                "SELECT intent, needs_reply_hint, follow_up_hint_json \
                 FROM mail_messages \
                 WHERE principal = ? AND workspace = ? \
                   AND provider = ? AND account_alias = ? \
                   AND thread_id = ? AND message_id = ? \
                 LIMIT 1",
            )?;
            let mut rows = stmt.query(params![
                principal,
                workspace,
                provider,
                account_alias,
                thread_id,
                message_id
            ])?;
            let Some(row) = rows.next()? else {
                return Ok(None);
            };
            let follow_up_hint_json: Option<String> = row.get(2)?;
            Ok(Some(MessageAttentionHints {
                intent: row.get(0)?,
                needs_reply_hint: row.get(1)?,
                follow_up_hint: follow_up_hint_json
                    .as_deref()
                    .map(serde_json::from_str::<ChannelFollowUpHint>)
                    .transpose()
                    .context("parsing message attention follow-up hint JSON")?,
            }))
        })
        .await
        .context("mail assist get_message_attention_hints task panicked")?
    }

    /// Threads ready for Phase-2 classification: one row per thread's newest
    /// distilled message, non-suppressed, and not yet classified for that exact
    /// message evidence. Newest-first.
    pub async fn list_threads_to_classify(
        &self,
        principal: &str,
        workspace: &str,
        limit: usize,
    ) -> Result<Vec<ThreadClassifyRow>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(
                "WITH ranked_done AS ( \
                    SELECT m.*, ROW_NUMBER() OVER ( \
                        PARTITION BY m.principal, m.workspace, m.provider, m.account_alias, m.thread_id \
                        ORDER BY m.internal_date DESC, m.message_id DESC \
                    ) AS rn \
                    FROM mail_messages m \
                    WHERE m.principal = ? AND m.workspace = ? \
                      AND m.distill_state = 'done' \
                      AND m.sensitive_suppressed = FALSE \
                      AND m.summary IS NOT NULL \
                 ) \
                 SELECT t.provider, t.account_alias, t.thread_id, t.subject, \
                        t.latest_from_name, t.latest_from_address, \
                        t.recipient_domains_json, t.label_ids_json, \
                        t.message_count, t.last_message_at, t.lane, m.summary, \
                        m.message_id, m.internal_date, m.direction, m.intent, \
                        m.needs_reply_hint, m.follow_up_hint_json, \
                        m.distill_evidence_message_ids_json, m.distill_brief_json, \
                        m.distill_revision \
                 FROM ranked_done m \
                 JOIN mail_threads t \
                   ON t.principal = m.principal AND t.workspace = m.workspace \
                   AND t.provider = m.provider AND t.account_alias = m.account_alias \
                   AND t.thread_id = m.thread_id \
                 WHERE t.sensitive_suppressed = FALSE \
                   AND m.distill_revision IS NOT NULL \
                   AND (m.rn = 1 OR EXISTS ( \
                     SELECT 1 FROM mail_annotations stale \
                     WHERE stale.principal = t.principal AND stale.workspace = t.workspace \
                       AND stale.provider = t.provider AND stale.account_alias = t.account_alias \
                       AND stale.thread_id = t.thread_id \
                       AND stale.evidence_message_id = m.message_id \
                       AND COALESCE(stale.classification_input_revision, 0) < m.distill_revision \
                   )) \
                   AND COALESCE(m.classify_attempts, 0) < ? \
                   AND (m.classify_next_retry_at IS NULL OR m.classify_next_retry_at <= ?) \
                   AND NOT EXISTS ( \
                     SELECT 1 FROM mail_annotations a \
                     WHERE a.principal = t.principal AND a.workspace = t.workspace \
                       AND a.provider = t.provider AND a.account_alias = t.account_alias \
                       AND a.thread_id = t.thread_id \
                       AND a.evidence_message_id = m.message_id \
                       AND COALESCE(a.classification_input_revision, 0) >= m.distill_revision) \
                 ORDER BY m.distill_revision DESC, m.internal_date DESC, m.message_id DESC LIMIT ?",
            )?;
            let now_ms = chrono::Utc::now().timestamp_millis();
            let mut rows = stmt.query(params![
                principal,
                workspace,
                MAX_CLASSIFY_ATTEMPTS,
                now_ms,
                limit as i64
            ])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                let recipient_domains_json: String = row.get(6)?;
                let label_ids_json: String = row.get(7)?;
                let follow_up_hint_json: Option<String> = row.get(17)?;
                let distill_evidence_json: Option<String> = row.get(18)?;
                let distill_brief_json: Option<String> = row.get(19)?;
                let latest_message_id: String = row.get(12)?;
                let mut evidence_message_ids = distill_evidence_json
                    .as_deref()
                    .map(serde_json::from_str::<Vec<String>>)
                    .transpose()
                    .context("parsing distill evidence ids JSON")?
                    .unwrap_or_else(|| vec![latest_message_id.clone()]);
                evidence_message_ids.retain(|id| !id.trim().is_empty());
                if evidence_message_ids.is_empty() {
                    evidence_message_ids.push(latest_message_id.clone());
                }
                if !evidence_message_ids
                    .iter()
                    .any(|id| id == &latest_message_id)
                {
                    evidence_message_ids.insert(0, latest_message_id.clone());
                }
                out.push(ThreadClassifyRow {
                    provider: row.get(0)?,
                    account_alias: row.get(1)?,
                    thread_id: row.get(2)?,
                    latest_message_id,
                    latest_message_at: row.get(13)?,
                    evidence_message_ids,
                    subject: row.get(3)?,
                    from_name: row.get(4)?,
                    from_address: row.get(5)?,
                    recipient_domains: serde_json::from_str(&recipient_domains_json)
                        .unwrap_or_default(),
                    label_ids: serde_json::from_str(&label_ids_json).unwrap_or_default(),
                    message_count: row.get(8)?,
                    last_message_at: row.get(9)?,
                    lane: row.get(10)?,
                    latest_summary: row.get(11)?,
                    latest_direction: row.get(14)?,
                    latest_intent: row.get(15)?,
                    needs_reply_hint: row.get(16)?,
                    follow_up_hint: follow_up_hint_json
                        .as_deref()
                        .map(serde_json::from_str::<ChannelFollowUpHint>)
                        .transpose()
                        .context("parsing classify follow-up hint JSON")?,
                    distill_brief: distill_brief_json
                        .as_deref()
                        .map(serde_json::from_str::<ChannelInformationBrief>)
                        .transpose()
                        .context("parsing classify information brief JSON")?,
                    distill_revision: row.get(20)?,
                });
            }
            Ok(out)
        })
        .await
        .context("mail assist list_threads_to_classify task panicked")?
    }

    /// Count of threads awaiting classification (same predicate as
    /// [`Self::list_threads_to_classify`]) — surfaced in `sync/status`.
    pub async fn count_pending_classify(&self, principal: &str, workspace: &str) -> Result<u64> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let count: i64 = conn.query_row(
                "WITH ranked_done AS ( \
                    SELECT m.*, ROW_NUMBER() OVER ( \
                        PARTITION BY m.principal, m.workspace, m.provider, m.account_alias, m.thread_id \
                        ORDER BY m.internal_date DESC, m.message_id DESC \
                    ) AS rn \
                    FROM mail_messages m \
                    WHERE m.principal = ? AND m.workspace = ? \
                      AND m.distill_state = 'done' \
                      AND m.sensitive_suppressed = FALSE \
                      AND m.summary IS NOT NULL \
                 ) \
                 SELECT COUNT(*) \
                 FROM ranked_done m \
                 JOIN mail_threads t \
                   ON t.principal = m.principal AND t.workspace = m.workspace \
                   AND t.provider = m.provider AND t.account_alias = m.account_alias \
                   AND t.thread_id = m.thread_id \
                 WHERE t.sensitive_suppressed = FALSE \
                   AND m.distill_revision IS NOT NULL \
                   AND (m.rn = 1 OR EXISTS ( \
                     SELECT 1 FROM mail_annotations stale \
                     WHERE stale.principal = t.principal AND stale.workspace = t.workspace \
                       AND stale.provider = t.provider AND stale.account_alias = t.account_alias \
                       AND stale.thread_id = t.thread_id \
                       AND stale.evidence_message_id = m.message_id \
                       AND COALESCE(stale.classification_input_revision, 0) < m.distill_revision \
                   )) \
                   AND COALESCE(m.classify_attempts, 0) < ? \
                   AND (m.classify_next_retry_at IS NULL OR m.classify_next_retry_at <= ?) \
                   AND NOT EXISTS ( \
                     SELECT 1 FROM mail_annotations a \
                     WHERE a.principal = t.principal AND a.workspace = t.workspace \
                       AND a.provider = t.provider AND a.account_alias = t.account_alias \
                       AND a.thread_id = t.thread_id \
                       AND a.evidence_message_id = m.message_id \
                       AND COALESCE(a.classification_input_revision, 0) >= m.distill_revision)",
                params![
                    principal,
                    workspace,
                    MAX_CLASSIFY_ATTEMPTS,
                    chrono::Utc::now().timestamp_millis()
                ],
                |row| row.get(0),
            )?;
            Ok(count as u64)
        })
        .await
        .context("mail assist count_pending_classify task panicked")?
    }

    /// Durable classifier retry/failure counts for pipeline visibility.
    pub async fn classify_retry_counts(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<(i64, i64)> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(
                "WITH latest_done AS ( \
                    SELECT m.*, ROW_NUMBER() OVER ( \
                        PARTITION BY m.principal, m.workspace, m.provider, m.account_alias, m.thread_id \
                        ORDER BY m.internal_date DESC, m.message_id DESC \
                    ) AS rn \
                    FROM mail_messages m \
                    WHERE m.principal = ? AND m.workspace = ? \
                      AND m.distill_state = 'done' \
                      AND m.sensitive_suppressed = FALSE \
                      AND m.summary IS NOT NULL \
                 ) \
                 SELECT \
                    COALESCE(SUM(CASE \
                        WHEN COALESCE(m.classify_attempts, 0) > 0 \
                         AND m.classify_failed_at IS NULL \
                         AND m.classify_next_retry_at IS NOT NULL \
                         AND m.classify_next_retry_at > ? THEN 1 ELSE 0 END), 0), \
                    COALESCE(SUM(CASE \
                        WHEN m.classify_failed_at IS NOT NULL THEN 1 ELSE 0 END), 0) \
                 FROM latest_done m \
                 JOIN mail_threads t \
                   ON t.principal = m.principal AND t.workspace = m.workspace \
                   AND t.provider = m.provider AND t.account_alias = m.account_alias \
                   AND t.thread_id = m.thread_id \
                 WHERE m.rn = 1 \
                   AND t.sensitive_suppressed = FALSE \
                   AND NOT EXISTS ( \
                     SELECT 1 FROM mail_annotations a \
                     WHERE a.principal = t.principal AND a.workspace = t.workspace \
                       AND a.provider = t.provider AND a.account_alias = t.account_alias \
                       AND a.thread_id = t.thread_id \
                       AND (a.evidence_message_id = m.message_id \
                            OR (a.evidence_message_id IS NULL \
                                AND a.created_at >= m.observed_at)))",
            )?;
            let now_ms = chrono::Utc::now().timestamp_millis();
            let mut rows = stmt.query(params![principal, workspace, now_ms])?;
            let row = rows
                .next()?
                .context("classify retry count aggregate returned no row")?;
            Ok((row.get(0)?, row.get(1)?))
        })
        .await
        .context("mail assist classify_retry_counts task panicked")?
    }

    /// Persist one classifier failure for a message evidence row. Non-terminal
    /// failures get a durable backoff timestamp; the final failure is visible
    /// via `classify_failed_at` and falls out of the classify queue.
    pub async fn record_classify_failure(
        &self,
        principal: &str,
        workspace: &str,
        provider: &str,
        account_alias: &str,
        message_id: &str,
        occurred_at: i64,
        error: &str,
    ) -> Result<(i64, bool)> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let provider = provider.to_string();
        let account_alias = account_alias.to_string();
        let message_id = message_id.to_string();
        let error = trim_for_storage(error, CLASSIFY_ERROR_MAX_CHARS);
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let _write_guard = inner.acquire_write_guard()?;
            let conn = inner
                .write_conn
                .lock()
                .expect("mail assist write connection mutex poisoned");
            let (attempts, terminal) = with_transaction(&conn, || {
                let previous_attempts = {
                    let mut stmt = conn.prepare(
                        "SELECT COALESCE(classify_attempts, 0) FROM mail_messages \
                         WHERE principal = ? AND workspace = ? AND provider = ? \
                           AND account_alias = ? AND message_id = ?",
                    )?;
                    let mut rows = stmt.query(params![
                        principal.as_str(),
                        workspace.as_str(),
                        provider.as_str(),
                        account_alias.as_str(),
                        message_id.as_str()
                    ])?;
                    let row = rows.next()?.with_context(|| {
                        format!("mail message not found for classify failure: {message_id}")
                    })?;
                    row.get::<_, i64>(0)?
                };
                let attempts = previous_attempts.saturating_add(1);
                let terminal = attempts >= MAX_CLASSIFY_ATTEMPTS;
                let next_retry_at = if terminal {
                    None
                } else {
                    Some(occurred_at.saturating_add(classify_retry_backoff_ms(attempts)))
                };
                let failed_at = terminal.then_some(occurred_at);
                conn.execute(
                    "UPDATE mail_messages \
                     SET classify_attempts = ?, classify_next_retry_at = ?, \
                         classify_last_error = ?, classify_failed_at = ? \
                     WHERE principal = ? AND workspace = ? AND provider = ? \
                       AND account_alias = ? AND message_id = ?",
                    params![
                        attempts,
                        next_retry_at,
                        error,
                        failed_at,
                        principal.as_str(),
                        workspace.as_str(),
                        provider.as_str(),
                        account_alias.as_str(),
                        message_id.as_str()
                    ],
                )
                .context("updating mail message classify failure")?;
                Ok((attempts, terminal))
            })?;
            inner
                .maybe_checkpoint(&conn)
                .context("throttled checkpoint after mail classify failure")?;
            Ok((attempts, terminal))
        })
        .await
        .context("mail assist record_classify_failure task panicked")?
    }

    /// Total `needs_approval` annotations for the scope — the paginated
    /// Follow-ups list's `total`, so callers can size pages without pulling
    /// every row.
    pub async fn count_needs_approval(&self, principal: &str, workspace: &str) -> Result<u64> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let count: i64 = conn.query_row(
                "WITH latest_annotations AS ( \
                    SELECT a.id, ROW_NUMBER() OVER ( \
                        PARTITION BY a.principal, a.workspace, a.provider, a.account_alias, a.thread_id \
                        ORDER BY a.created_at DESC, a.id DESC \
                    ) AS rn \
                    FROM mail_annotations a \
                    WHERE a.principal = ? AND a.workspace = ? \
                      AND (a.state = 'needs_approval' OR (a.state = 'stale' AND EXISTS ( \
                        SELECT 1 FROM mail_assist_events re \
                        WHERE re.principal = a.principal AND re.workspace = a.workspace \
                          AND re.annotation_id = a.id AND re.to_state = 'stale' \
                          AND json_extract_string(re.detail_json, '$.prior_state') IN ( \
                            'draft_requested', 'draft_ready', 'inserted') \
                      ))) \
                 ) SELECT COUNT(*) FROM latest_annotations WHERE rn = 1",
                params![principal, workspace],
                |row| row.get(0),
            )?;
            Ok(count as u64)
        })
        .await
        .context("mail assist count_needs_approval task panicked")?
    }

    /// Cursor-capable, attention-lane-filtered view over active channel
    /// annotations. Used by the attention lane facade and the public
    /// `/channel-assist/follow-ups` cursor-paginated route. `offset` remains
    /// for first-page/back-compat callers that do not pass a cursor.
    pub async fn list_needs_approval_attention_lane_page(
        &self,
        principal: &str,
        workspace: &str,
        attention_lane: &str,
        limit: usize,
        offset: usize,
        cursor: Option<NeedsApprovalCursor>,
    ) -> Result<NeedsApprovalPage> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let attention_lane = attention_lane.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let requested = limit.max(1);
            let fetch_limit = requested.saturating_add(1);
            let positioned_page = cursor.is_some() || offset > 0;
            let mut stmt;
            let mut rows = if let Some(cursor) = cursor {
                stmt = conn.prepare(
                    // Project only the columns the outer SELECT (and its window
                    // partition/order + join/cursor keys) consume, instead of
                    // `a.*`. Dragging all ~20 mail_annotations columns (incl. the
                    // 3 heavy JSON cols evidence_refs_json/detail/proposed_action_json)
                    // through the ROW_NUMBER window + COUNT(*) OVER() is what made
                    // the Today page ~12x slower. Behaviour is identical.
                    "WITH latest_annotations AS ( \
                        SELECT a.id, a.principal, a.workspace, a.provider, a.account_alias, \
                               a.thread_id, a.lane, a.state, a.label, a.confidence, a.reason, \
                               a.proposed_action_json, a.created_at, a.evidence_message_id, \
                               a.evidence_message_at, a.semantic_features_json, \
                               a.classification_input_revision, \
                               ROW_NUMBER() OVER ( \
                            PARTITION BY a.principal, a.workspace, a.provider, a.account_alias, a.thread_id \
                            ORDER BY a.created_at DESC, a.id DESC \
                        ) AS rn \
                        FROM mail_annotations a \
                        WHERE a.principal = ? AND a.workspace = ? \
                          AND (a.state = 'needs_approval' OR (a.state = 'stale' AND EXISTS ( \
                            SELECT 1 FROM mail_assist_events re \
                            WHERE re.principal = a.principal AND re.workspace = a.workspace \
                              AND re.annotation_id = a.id AND re.to_state = 'stale' \
                              AND json_extract_string(re.detail_json, '$.prior_state') IN ( \
                                'draft_requested', 'draft_ready', 'inserted') \
                          ))) \
                          AND ( \
                            a.attention_lane = ? \
                            OR (? = 'follow_up' AND a.attention_lane IS NULL) \
                          ) \
                     ), eligible AS ( \
                        SELECT a.*, COUNT(*) OVER () AS total_count \
                        FROM latest_annotations a WHERE a.rn = 1 \
                     ) \
                     SELECT a.id, a.provider, a.account_alias, a.thread_id, a.lane, a.state, \
                            a.label, a.confidence, a.reason, a.proposed_action_json, a.created_at, \
                            t.subject, t.latest_from_name, t.latest_from_address, \
                            COALESCE(t.account_email, em.account_email), \
                            COALESCE(em.internal_date, a.evidence_message_at, t.last_message_at), \
                            COALESCE(em.summary, t.latest_summary), \
                            a.evidence_message_id, a.evidence_message_at, \
                            a.semantic_features_json, a.classification_input_revision, a.total_count \
                     FROM eligible a \
                     LEFT JOIN mail_threads t \
                       ON t.principal = a.principal AND t.workspace = a.workspace \
                       AND t.provider = a.provider AND t.account_alias = a.account_alias \
                       AND t.thread_id = a.thread_id \
                     LEFT JOIN mail_messages em \
                       ON em.principal = a.principal AND em.workspace = a.workspace \
                       AND em.provider = a.provider AND em.account_alias = a.account_alias \
                       AND em.message_id = a.evidence_message_id \
                     WHERE (a.created_at < ? OR (a.created_at = ? AND a.id < ?)) \
                     ORDER BY a.created_at DESC, a.id DESC LIMIT ?",
                )?;
                stmt.query(params![
                    principal.as_str(),
                    workspace.as_str(),
                    attention_lane.as_str(),
                    attention_lane.as_str(),
                    cursor.created_at,
                    cursor.created_at,
                    cursor.annotation_id.as_str(),
                    fetch_limit as i64,
                ])?
            } else {
                stmt = conn.prepare(
                    // Same projection-narrowing as the cursor branch above: only
                    // the columns the outer SELECT / window / joins consume flow
                    // through ROW_NUMBER + COUNT(*) OVER(), not all of `a.*`.
                    "WITH latest_annotations AS ( \
                        SELECT a.id, a.principal, a.workspace, a.provider, a.account_alias, \
                               a.thread_id, a.lane, a.state, a.label, a.confidence, a.reason, \
                               a.proposed_action_json, a.created_at, a.evidence_message_id, \
                               a.evidence_message_at, a.semantic_features_json, \
                               a.classification_input_revision, \
                               ROW_NUMBER() OVER ( \
                            PARTITION BY a.principal, a.workspace, a.provider, a.account_alias, a.thread_id \
                            ORDER BY a.created_at DESC, a.id DESC \
                        ) AS rn \
                        FROM mail_annotations a \
                        WHERE a.principal = ? AND a.workspace = ? \
                          AND (a.state = 'needs_approval' OR (a.state = 'stale' AND EXISTS ( \
                            SELECT 1 FROM mail_assist_events re \
                            WHERE re.principal = a.principal AND re.workspace = a.workspace \
                              AND re.annotation_id = a.id AND re.to_state = 'stale' \
                              AND json_extract_string(re.detail_json, '$.prior_state') IN ( \
                                'draft_requested', 'draft_ready', 'inserted') \
                          ))) \
                          AND ( \
                            a.attention_lane = ? \
                            OR (? = 'follow_up' AND a.attention_lane IS NULL) \
                          ) \
                     ), eligible AS ( \
                        SELECT a.*, COUNT(*) OVER () AS total_count \
                        FROM latest_annotations a WHERE a.rn = 1 \
                     ) \
                     SELECT a.id, a.provider, a.account_alias, a.thread_id, a.lane, a.state, \
                            a.label, a.confidence, a.reason, a.proposed_action_json, a.created_at, \
                            t.subject, t.latest_from_name, t.latest_from_address, \
                            COALESCE(t.account_email, em.account_email), \
                            COALESCE(em.internal_date, a.evidence_message_at, t.last_message_at), \
                            COALESCE(em.summary, t.latest_summary), \
                            a.evidence_message_id, a.evidence_message_at, \
                            a.semantic_features_json, a.classification_input_revision, a.total_count \
                     FROM eligible a \
                     LEFT JOIN mail_threads t \
                       ON t.principal = a.principal AND t.workspace = a.workspace \
                       AND t.provider = a.provider AND t.account_alias = a.account_alias \
                       AND t.thread_id = a.thread_id \
                     LEFT JOIN mail_messages em \
                       ON em.principal = a.principal AND em.workspace = a.workspace \
                       AND em.provider = a.provider AND em.account_alias = a.account_alias \
                       AND em.message_id = a.evidence_message_id \
                     ORDER BY a.created_at DESC, a.id DESC LIMIT ? OFFSET ?",
                )?;
                stmt.query(params![
                    principal.as_str(),
                    workspace.as_str(),
                    attention_lane.as_str(),
                    attention_lane.as_str(),
                    fetch_limit as i64,
                    offset as i64,
                ])?
            };

            let mut out = Vec::new();
            let mut total = 0_i64;
            while let Some(row) = rows.next()? {
                total = row.get(21)?;
                let proposed_action_json: Option<String> = row.get(9)?;
                let semantic_features_json: Option<String> = row.get(19)?;
                out.push(NeedsApprovalRow {
                    annotation_id: row.get(0)?,
                    provider: row.get(1)?,
                    account_alias: row.get(2)?,
                    thread_id: row.get(3)?,
                    lane: row.get(4)?,
                    state: MailAnnotationState::from_db_str(&row.get::<_, String>(5)?)?,
                    label: row.get(6)?,
                    confidence: row.get(7)?,
                    reason: row.get(8)?,
                    proposed_action: proposed_action_json
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    created_at: row.get(10)?,
                    subject: row.get(11)?,
                    from_name: row.get(12)?,
                    from_address: row.get(13)?,
                    account_email: row.get(14)?,
                    last_message_at: row.get(15)?,
                    latest_summary: row.get(16)?,
                    evidence_message_id: row.get(17)?,
                    evidence_message_at: row.get(18)?,
                    semantic_features: semantic_features_json
                        .and_then(|value| serde_json::from_str(&value).ok()),
                    classification_input_revision: row.get(20)?,
                });
            }
            if out.is_empty() && positioned_page {
                total = conn.query_row(
                    "WITH latest_annotations AS ( \
                        SELECT a.id, ROW_NUMBER() OVER ( \
                            PARTITION BY a.principal, a.workspace, a.provider, a.account_alias, a.thread_id \
                            ORDER BY a.created_at DESC, a.id DESC \
                        ) AS rn FROM mail_annotations a \
                        WHERE a.principal = ? AND a.workspace = ? \
                          AND (a.state = 'needs_approval' OR (a.state = 'stale' AND EXISTS ( \
                            SELECT 1 FROM mail_assist_events re \
                            WHERE re.principal = a.principal AND re.workspace = a.workspace \
                              AND re.annotation_id = a.id AND re.to_state = 'stale' \
                              AND json_extract_string(re.detail_json, '$.prior_state') IN ( \
                                'draft_requested', 'draft_ready', 'inserted') \
                          ))) AND a.attention_lane = ? \
                     ) SELECT COUNT(*) FROM latest_annotations WHERE rn = 1",
                    params![principal.as_str(), workspace.as_str(), attention_lane.as_str()],
                    |row| row.get(0),
                )?;
            }
            let has_more = out.len() > requested;
            if has_more {
                out.truncate(requested);
            }
            let next_cursor = if has_more {
                out.last().map(|last| NeedsApprovalCursor {
                    created_at: last.created_at,
                    annotation_id: last.annotation_id.clone(),
                })
            } else {
                None
            };
            Ok(NeedsApprovalPage {
                rows: out,
                total: total.max(0) as u64,
                limit: requested,
                offset,
                has_more,
                next_cursor,
            })
        })
        .await
        .context("mail assist list_needs_approval_attention_lane_page task panicked")?
    }

    /// Compact, authoritative identity for the complete active attention lane.
    /// The DuckDB reader streams one stable-id-ordered row at a time; no caller
    /// has to retain or deserialize the complete Follow-up universe merely to
    /// decide whether a frozen delivery cursor is still current.
    pub async fn needs_approval_source_generation_token(
        &self,
        principal: &str,
        workspace: &str,
        attention_lane: &str,
        as_of_ms: i64,
    ) -> Result<String> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let attention_lane = attention_lane.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut statement = conn.prepare(
                "WITH latest_annotations AS ( \
                    SELECT a.id, a.principal, a.workspace, a.provider, a.account_alias, \
                           a.thread_id, a.lane, a.state, a.label, a.confidence, a.reason, \
                           a.proposed_action_json, a.created_at, a.evidence_message_id, \
                           a.evidence_message_at, a.semantic_features_json, \
                           a.classification_input_revision, a.attention_lane, \
                           ROW_NUMBER() OVER ( \
                        PARTITION BY a.principal, a.workspace, a.provider, a.account_alias, a.thread_id \
                        ORDER BY a.created_at DESC, a.id DESC \
                    ) AS rn \
                    FROM mail_annotations a \
                    WHERE a.principal = ? AND a.workspace = ? \
                      AND (a.state = 'needs_approval' OR (a.state = 'stale' AND EXISTS ( \
                        SELECT 1 FROM mail_assist_events re \
                        WHERE re.principal = a.principal AND re.workspace = a.workspace \
                          AND re.annotation_id = a.id AND re.to_state = 'stale' \
                          AND json_extract_string(re.detail_json, '$.prior_state') IN ( \
                            'draft_requested', 'draft_ready', 'inserted') \
                      ))) \
                      AND (a.attention_lane = ? OR (? = 'follow_up' AND a.attention_lane IS NULL)) \
                 ), eligible AS (SELECT * FROM latest_annotations WHERE rn = 1) \
                 SELECT a.id, a.provider, a.account_alias, a.thread_id, a.lane, a.state, \
                        a.label, a.confidence, a.reason, a.proposed_action_json, a.created_at, \
                        t.subject, t.latest_from_name, t.latest_from_address, \
                        COALESCE(t.account_email, em.account_email), \
                        COALESCE(em.internal_date, a.evidence_message_at, t.last_message_at), \
                        COALESCE(em.summary, t.latest_summary), \
                        a.evidence_message_id, a.evidence_message_at, \
                        a.semantic_features_json, a.classification_input_revision \
                 FROM eligible a \
                 LEFT JOIN mail_threads t \
                   ON t.principal = a.principal AND t.workspace = a.workspace \
                  AND t.provider = a.provider AND t.account_alias = a.account_alias \
                  AND t.thread_id = a.thread_id \
                 LEFT JOIN mail_messages em \
                   ON em.principal = a.principal AND em.workspace = a.workspace \
                  AND em.provider = a.provider AND em.account_alias = a.account_alias \
                  AND em.message_id = a.evidence_message_id \
                 ORDER BY a.id",
            )?;
            let mut rows = statement.query(params![
                principal.as_str(),
                workspace.as_str(),
                attention_lane.as_str(),
                attention_lane.as_str(),
            ])?;
            let mut digest = blake3::Hasher::new();
            digest.update(b"mail-needs-approval-source-generation-v1\0");
            let mut row_count = 0_u64;
            while let Some(row) = rows.next()? {
                row_count = row_count.saturating_add(1);
                for index in [0usize, 1, 2, 3, 4, 5] {
                    let value: String = row.get(index)?;
                    update_mail_generation_string(&mut digest, Some(&value));
                }
                update_mail_generation_string(
                    &mut digest,
                    row.get::<_, Option<String>>(6)?.as_deref(),
                );
                update_mail_generation_f64(&mut digest, row.get(7)?);
                update_mail_generation_string(
                    &mut digest,
                    row.get::<_, Option<String>>(8)?.as_deref(),
                );
                update_mail_generation_string(
                    &mut digest,
                    row.get::<_, Option<String>>(9)?.as_deref(),
                );
                update_mail_generation_i64(&mut digest, Some(row.get(10)?));
                for index in [11usize, 12, 13, 14] {
                    update_mail_generation_string(
                        &mut digest,
                        row.get::<_, Option<String>>(index)?.as_deref(),
                    );
                }
                let last_message_at: Option<i64> = row.get(15)?;
                update_mail_generation_i64(&mut digest, last_message_at);
                update_mail_generation_f64(
                    &mut digest,
                    last_message_at.map(|source_at| {
                        let age_days = as_of_ms.saturating_sub(source_at).max(0) as f64
                            / 86_400_000.0;
                        magician::magician_v2::attention::learning::actionability::bucket_age_days(
                            age_days,
                        )
                    }),
                );
                update_mail_generation_string(
                    &mut digest,
                    row.get::<_, Option<String>>(16)?.as_deref(),
                );
                update_mail_generation_string(
                    &mut digest,
                    row.get::<_, Option<String>>(17)?.as_deref(),
                );
                update_mail_generation_i64(&mut digest, row.get(18)?);
                update_mail_generation_string(
                    &mut digest,
                    row.get::<_, Option<String>>(19)?.as_deref(),
                );
                update_mail_generation_i64(&mut digest, row.get(20)?);
                digest.update(&[0xff]);
            }
            digest.update(&row_count.to_le_bytes());
            Ok(format!(
                "mail-needs-approval-v1:{}",
                digest.finalize().to_hex()
            ))
        })
        .await
        .context("mail assist source-generation task panicked")?
    }

    /// Exact active-row lookup used to retain the acted card's semantic
    /// exemplar even when it falls outside the bounded ranking cohort.
    pub async fn get_needs_approval_attention_row(
        &self,
        principal: &str,
        workspace: &str,
        annotation_id: &str,
    ) -> Result<Option<NeedsApprovalRow>> {
        self.get_attention_row(principal, workspace, annotation_id, true)
            .await
    }

    /// The same projection, optionally without the pending-approval filter.
    ///
    /// Reconciliation records its learning outcome *after* it retires an
    /// annotation, so by then the row has left the approval queue by
    /// definition. Requiring `needs_approval` there would return `None` and
    /// drop the label silently — the outcome would simply never be written.
    pub async fn get_attention_row(
        &self,
        principal: &str,
        workspace: &str,
        annotation_id: &str,
        require_needs_approval: bool,
    ) -> Result<Option<NeedsApprovalRow>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let annotation_id = annotation_id.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut statement = conn.prepare(&format!(
                "SELECT a.id, a.provider, a.account_alias, a.thread_id, a.lane, a.state, \
                        a.label, a.confidence, a.reason, a.proposed_action_json, a.created_at, \
                        t.subject, t.latest_from_name, t.latest_from_address, \
                        COALESCE(t.account_email, em.account_email), \
                        COALESCE(em.internal_date, a.evidence_message_at, t.last_message_at), \
                        COALESCE(em.summary, t.latest_summary), a.evidence_message_id, \
                        a.evidence_message_at, a.semantic_features_json, \
                        a.classification_input_revision \
                 FROM mail_annotations a \
                 LEFT JOIN mail_threads t \
                   ON t.principal = a.principal AND t.workspace = a.workspace \
                  AND t.provider = a.provider AND t.account_alias = a.account_alias \
                  AND t.thread_id = a.thread_id \
                 LEFT JOIN mail_messages em \
                   ON em.principal = a.principal AND em.workspace = a.workspace \
                  AND em.provider = a.provider AND em.account_alias = a.account_alias \
                  AND em.message_id = a.evidence_message_id \
                 WHERE a.principal = ? AND a.workspace = ? AND a.id = ? {} LIMIT 1",
                if require_needs_approval {
                    "AND a.state = 'needs_approval'"
                } else {
                    ""
                }
            ))?;
            let row =
                match statement.query_row(params![principal, workspace, annotation_id], |row| {
                    let proposed_action_json: Option<String> = row.get(9)?;
                    let semantic_features_json: Option<String> = row.get(19)?;
                    Ok(NeedsApprovalRow {
                        annotation_id: row.get(0)?,
                        provider: row.get(1)?,
                        account_alias: row.get(2)?,
                        thread_id: row.get(3)?,
                        lane: row.get(4)?,
                        state: MailAnnotationState::NeedsApproval,
                        label: row.get(6)?,
                        confidence: row.get(7)?,
                        reason: row.get(8)?,
                        proposed_action: proposed_action_json
                            .and_then(|value| serde_json::from_str(&value).ok()),
                        created_at: row.get(10)?,
                        subject: row.get(11)?,
                        from_name: row.get(12)?,
                        from_address: row.get(13)?,
                        account_email: row.get(14)?,
                        last_message_at: row.get(15)?,
                        latest_summary: row.get(16)?,
                        evidence_message_id: row.get(17)?,
                        evidence_message_at: row.get(18)?,
                        semantic_features: semantic_features_json
                            .and_then(|value| serde_json::from_str(&value).ok()),
                        classification_input_revision: row.get(20)?,
                    })
                }) {
                    Ok(row) => Some(row),
                    Err(duckdb::Error::QueryReturnedNoRows) => None,
                    Err(error) => return Err(error.into()),
                };
            Ok(row)
        })
        .await
        .context("mail assist exact attention row task panicked")?
    }

    /// Feedback events after `since` (epoch-ms `created_at`), joined with the
    /// annotation's current state + label and the thread subject — the input to
    /// the feedback→memory bridge. Ordered oldest-first so the watermark can
    /// advance monotonically.
    pub async fn list_feedback_since(
        &self,
        principal: &str,
        workspace: &str,
        since: i64,
        after_cursor: Option<&str>,
        limit: usize,
    ) -> Result<Vec<FeedbackBridgeRow>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let cursor = decode_bridge_cursor(after_cursor, 1);
        let use_cursor = if cursor.iter().any(|part| !part.is_empty()) {
            1_i64
        } else {
            0_i64
        };
        let cursor_event_id = cursor[0].clone();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(
                "SELECT e.provider, e.account_alias, e.thread_id, e.created_at, \
                        e.detail_json, a.state, a.label, t.subject, e.id \
                 FROM mail_assist_events e \
                 LEFT JOIN mail_annotations a \
                   ON a.principal = e.principal AND a.workspace = e.workspace \
                   AND a.id = e.annotation_id \
                 LEFT JOIN mail_threads t \
                   ON t.principal = e.principal AND t.workspace = e.workspace \
                   AND t.provider = e.provider AND t.account_alias = e.account_alias \
                   AND t.thread_id = e.thread_id \
                 WHERE e.principal = ? AND e.workspace = ? \
                   AND e.event_type = 'feedback' \
                   AND (e.created_at > ? \
                        OR (? = 1 AND e.created_at = ? AND e.id > ?)) \
                 ORDER BY e.created_at ASC, e.id ASC LIMIT ?",
            )?;
            let mut rows = stmt.query(params![
                principal,
                workspace,
                since,
                use_cursor,
                since,
                cursor_event_id,
                limit as i64
            ])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                let detail_json: Option<String> = row.get(4)?;
                let detail: Option<serde_json::Value> =
                    detail_json.and_then(|s| serde_json::from_str(&s).ok());
                let verdict = detail
                    .as_ref()
                    .and_then(|d| d.get("verdict"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("other")
                    .to_string();
                let comment = detail
                    .as_ref()
                    .and_then(|d| d.get("comment"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                out.push(FeedbackBridgeRow {
                    provider: row.get(0)?,
                    account_alias: row.get(1)?,
                    thread_id: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    created_at: row.get(3)?,
                    state: row.get(5)?,
                    label: row.get(6)?,
                    subject: row.get(7)?,
                    verdict,
                    comment,
                    event_id: row.get(8)?,
                });
            }
            Ok(out)
        })
        .await
        .context("mail assist list_feedback_since task panicked")?
    }

    /// Cutoff-bound, stable-cursor source for the one-time canonical attention
    /// migration. A same-timestamp lifecycle event is returned only to
    /// distinguish completed work from a positive "useful" label; no raw body
    /// or free-form transition result leaves this boundary.
    pub async fn list_attention_feedback_since(
        &self,
        principal: &str,
        workspace: &str,
        since: i64,
        after_event_id: &str,
        cutoff_at: i64,
        limit: usize,
    ) -> Result<Vec<AttentionFeedbackHistoryRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let after_event_id = after_event_id.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut statement = conn.prepare(
                "SELECT e.id, e.annotation_id, e.provider, e.account_alias, e.thread_id, e.created_at, e.detail_json, t.subject, \
                        (SELECT x.to_state FROM mail_assist_events x WHERE x.principal = e.principal AND x.workspace = e.workspace AND x.annotation_id = e.annotation_id AND x.created_at = e.created_at AND x.event_type IN ('state_transition','dismissed') ORDER BY x.id ASC LIMIT 1), \
                        (SELECT x.detail_json FROM mail_assist_events x WHERE x.principal = e.principal AND x.workspace = e.workspace AND x.annotation_id = e.annotation_id AND x.created_at = e.created_at AND x.event_type IN ('state_transition','dismissed') ORDER BY x.id ASC LIMIT 1) \
                 FROM mail_assist_events e \
                 LEFT JOIN mail_threads t ON t.principal = e.principal AND t.workspace = e.workspace AND t.provider = e.provider AND t.account_alias = e.account_alias AND t.thread_id = e.thread_id \
                 WHERE e.principal = ? AND e.workspace = ? AND e.event_type = 'feedback' AND e.created_at < ? \
                   AND (e.created_at > ? OR (e.created_at = ? AND e.id > ?)) \
                 ORDER BY e.created_at ASC, e.id ASC LIMIT ?",
            )?;
            let mut rows = statement.query(params![
                principal,
                workspace,
                cutoff_at,
                since,
                since,
                after_event_id,
                limit as i64,
            ])?;
            let mut output = Vec::new();
            while let Some(row) = rows.next()? {
                let detail = row
                    .get::<_, Option<String>>(6)?
                    .and_then(|encoded| serde_json::from_str::<serde_json::Value>(&encoded).ok());
                let transition_detail = row
                    .get::<_, Option<String>>(9)?
                    .and_then(|encoded| serde_json::from_str::<serde_json::Value>(&encoded).ok());
                output.push(AttentionFeedbackHistoryRow {
                    event_id: row.get(0)?,
                    annotation_id: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    provider: row.get(2)?,
                    account_alias: row.get(3)?,
                    thread_id: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    created_at: row.get(5)?,
                    verdict: detail
                        .as_ref()
                        .and_then(|value| value.get("verdict"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("other")
                        .to_string(),
                    comment: detail
                        .as_ref()
                        .and_then(|value| value.get("comment"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    subject: row.get(7)?,
                    paired_to_state: row.get(8)?,
                    paired_action: transition_detail
                        .as_ref()
                        .and_then(|value| value.get("action"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                });
            }
            Ok(output)
        })
        .await
        .context("mail assist historical attention feedback task panicked")?
    }

    /// Aggregate explicit owner verdicts for the current sender and sender
    /// domain. The classifier uses this as a deterministic cooldown for
    /// model-only recommendations; locally-derived required actions always
    /// bypass the cooldown.
    pub async fn feedback_tuning_profile(
        &self,
        principal: &str,
        workspace: &str,
        provider: &str,
        account_alias: &str,
        sender_address: Option<&str>,
        since: i64,
        limit: usize,
    ) -> Result<FeedbackTuningProfile> {
        let Some(sender_address) = sender_address
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(FeedbackTuningProfile::default());
        };
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let provider = provider.to_string();
        let account_alias = account_alias.to_string();
        let sender_address = sender_address.to_ascii_lowercase();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(
                "SELECT LOWER(COALESCE(m.from_address, t.latest_from_address)), \
                        json_extract_string(e.detail_json, '$.verdict') \
                 FROM mail_assist_events e \
                 JOIN mail_annotations a \
                   ON a.principal = e.principal AND a.workspace = e.workspace \
                   AND a.id = e.annotation_id \
                 JOIN mail_threads t \
                   ON t.principal = e.principal AND t.workspace = e.workspace \
                   AND t.provider = e.provider AND t.account_alias = e.account_alias \
                   AND t.thread_id = e.thread_id \
                 LEFT JOIN mail_messages m \
                   ON m.principal = a.principal AND m.workspace = a.workspace \
                   AND m.provider = a.provider AND m.account_alias = a.account_alias \
                   AND m.message_id = a.evidence_message_id \
                 WHERE e.principal = ? AND e.workspace = ? AND e.provider = ? \
                   AND e.account_alias = ? AND e.event_type = 'feedback' \
                   AND e.created_at >= ? \
                   AND COALESCE(m.from_address, t.latest_from_address) IS NOT NULL \
                 ORDER BY e.created_at DESC, e.id DESC LIMIT ?",
            )?;
            let mut rows = stmt.query(params![
                principal,
                workspace,
                provider,
                account_alias,
                since,
                limit as i64,
            ])?;
            let sender_domain = address_domain(&sender_address);
            let mut profile = FeedbackTuningProfile::default();
            while let Some(row) = rows.next()? {
                let address: String = row.get(0)?;
                let verdict: Option<String> = row.get(1)?;
                let sender_match = address == sender_address;
                let domain_match =
                    sender_domain.is_some() && address_domain(&address) == sender_domain;
                match verdict.as_deref() {
                    Some("not_helpful") => {
                        if sender_match {
                            profile.sender_dismissed += 1;
                        }
                        if domain_match {
                            profile.domain_dismissed += 1;
                        }
                    },
                    Some("helpful") => {
                        if sender_match {
                            profile.sender_helpful += 1;
                        }
                        if domain_match {
                            profile.domain_helpful += 1;
                        }
                    },
                    _ => {},
                }
            }
            Ok(profile)
        })
        .await
        .context("mail assist feedback_tuning_profile task panicked")?
    }

    pub async fn upsert_writing_preference(
        &self,
        principal: &str,
        workspace: &str,
        provider: &str,
        account_alias: &str,
        scope_kind: WritingPreferenceScopeKind,
        scope_value: &str,
        statement: &str,
        status: WritingPreferenceStatus,
        source_annotation_id: Option<&str>,
        now: i64,
    ) -> Result<WritingPreference> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let provider = provider.to_string();
        let account_alias = account_alias.to_string();
        let scope_value = scope_value.to_ascii_lowercase();
        let statement = statement.to_string();
        let source_annotation_id = source_annotation_id.map(str::to_string);
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let _write_guard = inner.acquire_write_guard()?;
            let conn = inner
                .write_conn
                .lock()
                .expect("mail assist write connection mutex poisoned");
            conn.execute(
                "INSERT INTO channel_writing_preferences (
                    principal, workspace, id, provider, account_alias, scope_kind,
                    scope_value, statement, status, source_annotation_id,
                    evidence_count, created_at, updated_at, schema_version
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?)
                 ON CONFLICT (
                    principal, workspace, provider, account_alias, scope_kind,
                    scope_value, statement
                 ) DO UPDATE SET
                    status = CASE
                      WHEN channel_writing_preferences.status = 'promoted' THEN 'promoted'
                      ELSE excluded.status
                    END,
                    source_annotation_id = COALESCE(
                      excluded.source_annotation_id,
                      channel_writing_preferences.source_annotation_id
                    ),
                    evidence_count = channel_writing_preferences.evidence_count + 1,
                    updated_at = excluded.updated_at,
                    schema_version = excluded.schema_version",
                params![
                    principal,
                    workspace,
                    Uuid::new_v4().to_string(),
                    provider,
                    account_alias,
                    scope_kind.as_db_str(),
                    scope_value,
                    statement,
                    status.as_db_str(),
                    source_annotation_id,
                    now,
                    now,
                    MAIL_ASSIST_SCHEMA_VERSION,
                ],
            )
            .context("upserting channel writing preference")?;
            let preference = query_writing_preference_by_key(
                &conn,
                &principal,
                &workspace,
                &provider,
                &account_alias,
                scope_kind,
                &scope_value,
                &statement,
            )?
            .context("writing preference missing after upsert")?;
            inner
                .maybe_checkpoint(&conn)
                .context("throttled checkpoint after writing preference upsert")?;
            Ok(preference)
        })
        .await
        .context("mail assist upsert_writing_preference task panicked")?
    }

    pub async fn list_writing_preferences(
        &self,
        principal: &str,
        workspace: &str,
        provider: &str,
        account_alias: &str,
        sender_address: Option<&str>,
        sender_domain: Option<&str>,
    ) -> Result<Vec<WritingPreference>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let provider = provider.to_string();
        let account_alias = account_alias.to_string();
        let sender_address = sender_address.unwrap_or_default().to_ascii_lowercase();
        let sender_domain = sender_domain.unwrap_or_default().to_ascii_lowercase();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(
                "SELECT id, provider, account_alias, scope_kind, scope_value,
                        statement, status, source_annotation_id, evidence_count,
                        created_at, updated_at, schema_version
                 FROM channel_writing_preferences
                 WHERE principal = ? AND workspace = ? AND provider = ?
                   AND account_alias = ? AND status <> 'dismissed'
                   AND ((scope_kind = 'sender' AND scope_value = ?)
                     OR (scope_kind = 'domain' AND scope_value = ?))
                 ORDER BY CASE status WHEN 'promoted' THEN 0 ELSE 1 END,
                          updated_at DESC, id ASC",
            )?;
            let mut rows = stmt.query(params![
                principal,
                workspace,
                provider,
                account_alias,
                sender_address,
                sender_domain,
            ])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push(map_writing_preference_row(row)?);
            }
            Ok(out)
        })
        .await
        .context("mail assist list_writing_preferences task panicked")?
    }

    pub async fn set_writing_preference_status(
        &self,
        principal: &str,
        workspace: &str,
        id: &str,
        status: WritingPreferenceStatus,
        now: i64,
    ) -> Result<Option<WritingPreference>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let _write_guard = inner.acquire_write_guard()?;
            let conn = inner
                .write_conn
                .lock()
                .expect("mail assist write connection mutex poisoned");
            let changed = conn.execute(
                "UPDATE channel_writing_preferences SET status = ?, updated_at = ?
                 WHERE principal = ? AND workspace = ? AND id = ?",
                params![status.as_db_str(), now, principal, workspace, id],
            )?;
            let preference = if changed == 0 {
                None
            } else {
                query_writing_preference_by_id(&conn, &principal, &workspace, &id)?
            };
            inner
                .maybe_checkpoint(&conn)
                .context("throttled checkpoint after writing preference status")?;
            Ok(preference)
        })
        .await
        .context("mail assist set_writing_preference_status task panicked")?
    }

    pub async fn get_writing_preference(
        &self,
        principal: &str,
        workspace: &str,
        id: &str,
    ) -> Result<Option<WritingPreference>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        let id = id.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            query_writing_preference_by_id(&conn, &principal, &workspace, &id)
        })
        .await
        .context("mail assist get_writing_preference task panicked")?
    }

    /// Histogram of `mail_messages.distill_state` (pending / done / suppressed
    /// / skipped) for the pipeline stats page. `(state, count)` pairs.
    pub async fn distill_state_histogram(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<(String, i64)>> {
        self.group_count(
            principal,
            workspace,
            "SELECT distill_state, COUNT(*) FROM mail_messages \
             WHERE principal = ? AND workspace = ? GROUP BY distill_state",
        )
        .await
    }

    /// Histogram of `mail_annotations.state` (needs_approval / classified /
    /// approved / dismissed / …). `(state, count)` pairs.
    pub async fn annotation_state_histogram(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<(String, i64)>> {
        self.group_count(
            principal,
            workspace,
            "SELECT state, COUNT(*) FROM mail_annotations \
             WHERE principal = ? AND workspace = ? GROUP BY state",
        )
        .await
    }

    /// Histogram of the classifier `label` (needs_reply / follow_up / fyi /
    /// no_action; null → `(unlabeled)`). `(label, count)` pairs.
    pub async fn annotation_label_histogram(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<(String, i64)>> {
        self.group_count(
            principal,
            workspace,
            "SELECT COALESCE(label, '(unlabeled)'), COUNT(*) FROM mail_annotations \
             WHERE principal = ? AND workspace = ? GROUP BY label",
        )
        .await
    }

    /// Label + structured action signals for annotations currently on the
    /// owner-facing `needs_approval` surface. Used by observability to derive
    /// the same source-family split as the card API without paging through card
    /// joins.
    pub async fn needs_approval_source_signals(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<NeedsApprovalSourceSignal>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(
                "SELECT label, proposed_action_json FROM mail_annotations \
                 WHERE principal = ? AND workspace = ? AND state = 'needs_approval'",
            )?;
            let mut rows = stmt.query(params![principal, workspace])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                let proposed_action_json: Option<String> = row.get(1)?;
                out.push(NeedsApprovalSourceSignal {
                    label: row.get(0)?,
                    proposed_action: proposed_action_json
                        .and_then(|s| serde_json::from_str(&s).ok()),
                });
            }
            Ok(out)
        })
        .await
        .context("mail assist needs_approval_source_signals task panicked")?
    }

    /// Group currently active owner-facing follow-ups by source family without
    /// materializing every annotation row in Rust. New rows use authoritative
    /// router metadata; label/action inference is retained only for legacy rows
    /// that predate `attention_source_family`.
    pub async fn needs_approval_source_family_histogram(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<(String, i64)>> {
        self.group_count(
            principal,
            workspace,
            "SELECT COALESCE( \
                NULLIF(lower(COALESCE(json_extract_string(proposed_action_json, '$.attention_source_family'), '')), ''), \
                CASE \
                    WHEN lower(COALESCE(label, '')) IN \
                        ('follow_up', 'promise', 'owner_owes', 'other_owes', \
                         'waiting_on', 'check_back', 'schedule') \
                      OR lower(COALESCE(json_extract_string(proposed_action_json, '$.follow_up_kind'), '')) \
                          IN ('owner_owes', 'other_owes', 'waiting_on', 'check_back', 'schedule') \
                    THEN 'promise' ELSE 'comms_ingest' END \
                ) AS source_family, \
                COUNT(*) \
             FROM mail_annotations \
             WHERE principal = ? AND workspace = ? AND state = 'needs_approval' \
             GROUP BY source_family",
        )
        .await
    }

    /// Shared `GROUP BY … COUNT(*)` runner for the stats histograms — the
    /// query MUST select exactly `(TEXT, COUNT)` and bind `(principal,
    /// workspace)`.
    async fn group_count(
        &self,
        principal: &str,
        workspace: &str,
        sql: &'static str,
    ) -> Result<Vec<(String, i64)>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(sql)?;
            let mut rows = stmt.query(params![principal, workspace])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                let key: Option<String> = row.get(0)?;
                out.push((key.unwrap_or_else(|| "(none)".to_string()), row.get(1)?));
            }
            Ok(out)
        })
        .await
        .context("mail assist group_count task panicked")?
    }

    /// `needs_approval` annotations (the Phase-2 classifier's actionable,
    /// high-confidence output) joined with their thread's subject/sender/lane,
    /// for the channel Follow-ups surface. Newest-first.
    pub async fn list_needs_approval(
        &self,
        principal: &str,
        workspace: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<NeedsApprovalRow>> {
        let store = self.clone();
        let principal = principal.to_string();
        let workspace = workspace.to_string();
        tokio::task::spawn_blocking(move || {
            let inner = store.scope_inner(&principal, &workspace)?;
            let conn = inner.read_connection()?;
            let mut stmt = conn.prepare(
                // account_email + arrival time can be NULL on the thread row
                // (stale rows / incremental-only sync), so recover them from the
                // messages — `internal_date` is NOT NULL and at least one message
                // carries the account_email from a full-sync pass.
                "WITH latest_annotations AS ( \
                    SELECT a.*, ROW_NUMBER() OVER ( \
                        PARTITION BY a.principal, a.workspace, a.provider, a.account_alias, a.thread_id \
                        ORDER BY a.created_at DESC, a.id DESC \
                    ) AS rn \
                    FROM mail_annotations a \
                    WHERE a.principal = ? AND a.workspace = ? \
                      AND a.state = 'needs_approval' \
                 ) \
                 SELECT a.id, a.provider, a.account_alias, a.thread_id, a.lane, \
                        a.label, a.confidence, a.reason, a.proposed_action_json, a.created_at, \
                        t.subject, t.latest_from_name, t.latest_from_address, \
                        COALESCE(t.account_email, em.account_email, ( \
                            SELECT m.account_email FROM mail_messages m \
                            WHERE m.principal = a.principal AND m.workspace = a.workspace \
                              AND m.provider = a.provider AND m.account_alias = a.account_alias \
                              AND m.thread_id = a.thread_id AND m.account_email IS NOT NULL LIMIT 1)), \
                        COALESCE(em.internal_date, a.evidence_message_at, ( \
                            SELECT MAX(m2.internal_date) FROM mail_messages m2 \
                            WHERE m2.principal = a.principal AND m2.workspace = a.workspace \
                              AND m2.provider = a.provider AND m2.account_alias = a.account_alias \
                              AND m2.thread_id = a.thread_id), t.last_message_at), \
                        COALESCE(em.summary, t.latest_summary), \
                        a.evidence_message_id, a.evidence_message_at, \
                        a.semantic_features_json, a.classification_input_revision \
                 FROM latest_annotations a \
                 LEFT JOIN mail_threads t \
                   ON t.principal = a.principal AND t.workspace = a.workspace \
                   AND t.provider = a.provider AND t.account_alias = a.account_alias \
                   AND t.thread_id = a.thread_id \
                 LEFT JOIN mail_messages em \
                   ON em.principal = a.principal AND em.workspace = a.workspace \
                   AND em.provider = a.provider AND em.account_alias = a.account_alias \
                   AND em.message_id = a.evidence_message_id \
                 WHERE a.rn = 1 \
                 ORDER BY a.created_at DESC, a.id DESC LIMIT ? OFFSET ?",
            )?;
            let mut rows = stmt.query(params![principal, workspace, limit as i64, offset as i64])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                let proposed_action_json: Option<String> = row.get(8)?;
                let semantic_features_json: Option<String> = row.get(18)?;
                out.push(NeedsApprovalRow {
                    annotation_id: row.get(0)?,
                    provider: row.get(1)?,
                    account_alias: row.get(2)?,
                    thread_id: row.get(3)?,
                    lane: row.get(4)?,
                    state: MailAnnotationState::NeedsApproval,
                    label: row.get(5)?,
                    confidence: row.get(6)?,
                    reason: row.get(7)?,
                    proposed_action: proposed_action_json
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    created_at: row.get(9)?,
                    subject: row.get(10)?,
                    from_name: row.get(11)?,
                    from_address: row.get(12)?,
                    account_email: row.get(13)?,
                    last_message_at: row.get(14)?,
                    latest_summary: row.get(15)?,
                    evidence_message_id: row.get(16)?,
                    evidence_message_at: row.get(17)?,
                    semantic_features: semantic_features_json
                        .and_then(|value| serde_json::from_str(&value).ok()),
                    classification_input_revision: row.get(19)?,
                });
            }
            Ok(out)
        })
        .await
        .context("mail assist list_needs_approval task panicked")?
    }

    pub fn base_root(&self) -> &Path {
        &self.base_root
    }

    pub fn list_scopes(&self) -> Vec<(String, String)> {
        self.workspace_layout.list_scopes()
    }
}

fn decode_bridge_json<T: serde::de::DeserializeOwned>(
    encoded: &str,
    field: &str,
    distill_revision: i64,
) -> Option<T> {
    match serde_json::from_str(encoded) {
        Ok(value) => Some(value),
        Err(error) => {
            let previous = MALFORMED_BRIDGE_ROWS
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                    Some(
                        current
                            .saturating_add(1)
                            .min(MALFORMED_BRIDGE_ROW_METRIC_CAP),
                    )
                })
                .unwrap_or_else(|current| current);
            let count = previous
                .saturating_add(1)
                .min(MALFORMED_BRIDGE_ROW_METRIC_CAP);
            if count <= 10 {
                tracing::warn!(
                    field,
                    distill_revision,
                    malformed_bridge_rows = count,
                    error = %error,
                    "downgrading malformed persisted distill field while advancing bridge"
                );
            }
            None
        },
    }
}

/// One `needs_approval` annotation joined with its thread — a channel Follow-up
/// card (metadata + the classifier's label/reason; never a body).
#[derive(Debug, Clone)]
pub struct NeedsApprovalRow {
    pub annotation_id: String,
    pub provider: String,
    pub account_alias: String,
    pub thread_id: String,
    pub lane: String,
    pub state: MailAnnotationState,
    pub label: Option<String>,
    pub confidence: Option<f64>,
    pub reason: Option<String>,
    pub proposed_action: Option<serde_json::Value>,
    pub created_at: i64,
    pub subject: Option<String>,
    pub from_name: Option<String>,
    pub from_address: Option<String>,
    /// Evidence message summary when available; legacy rows fall back to the
    /// thread's rolling latest summary.
    pub latest_summary: Option<String>,
    /// The mailbox's own address (e.g. the gmail account the thread lives in),
    /// used to route the "open thread" deep link to the correct account rather
    /// than the browser's default.
    pub account_email: Option<String>,
    /// When the evidence message ARRIVED (epoch ms), falling back to thread
    /// latest-message time for legacy rows — surfaced so the card shows the
    /// real received time, not `created_at` (annotation time).
    pub last_message_at: Option<i64>,
    pub evidence_message_id: Option<String>,
    pub evidence_message_at: Option<i64>,
    pub semantic_features: Option<serde_json::Value>,
    pub classification_input_revision: Option<i64>,
}

/// Cursor for active channel annotation pages ordered by
/// `(created_at DESC, annotation_id DESC)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeedsApprovalCursor {
    pub created_at: i64,
    pub annotation_id: String,
}

/// Cursor-capable active annotation page for the internal attention facade.
#[derive(Debug, Clone)]
pub struct NeedsApprovalPage {
    pub rows: Vec<NeedsApprovalRow>,
    pub total: u64,
    pub limit: usize,
    pub offset: usize,
    pub has_more: bool,
    pub next_cursor: Option<NeedsApprovalCursor>,
}

/// Bounded prior owner decisions in a thread, passed to the body-blind
/// classifier so it can distinguish a pure repeat from materially new evidence.
#[derive(Debug, Clone, PartialEq)]
pub struct RecentHandledFollowUp {
    pub state: MailAnnotationState,
    pub label: Option<String>,
    pub reason: Option<String>,
    pub proposed_action: Option<serde_json::Value>,
    pub updated_at: i64,
}

/// Minimal signal row for aggregating source-family observability over active
/// needs-you annotations.
#[derive(Debug, Clone)]
pub struct NeedsApprovalSourceSignal {
    pub label: Option<String>,
    pub proposed_action: Option<serde_json::Value>,
}

pub type ChannelNeedsApprovalRow = NeedsApprovalRow;

/// One message in the passive pattern-synthesis corpus (metadata + the
/// locally-derived summary — never a raw body).
#[derive(Debug, Clone)]
pub struct PatternCorpusRow {
    pub internal_date: i64,
    pub subject: Option<String>,
    pub from_address: Option<String>,
    pub summary: Option<String>,
}

pub type ChannelPatternCorpusRow = PatternCorpusRow;

/// One feedback event for the feedback→memory bridge: the verdict + the
/// annotation's current state (which distinguishes did/acknowledged/dismissed
/// — the verdict alone can't) + label/subject for legible memory notes.
#[derive(Debug, Clone)]
pub struct FeedbackBridgeRow {
    pub provider: String,
    pub account_alias: String,
    pub thread_id: String,
    pub created_at: i64,
    /// Annotation state at read time (approved / acknowledged / dismissed / …).
    pub state: Option<String>,
    pub label: Option<String>,
    pub subject: Option<String>,
    pub verdict: String,
    /// Dismiss reason (for `not_helpful`), if any.
    pub comment: Option<String>,
    pub event_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttentionFeedbackHistoryRow {
    pub event_id: String,
    pub annotation_id: String,
    pub provider: String,
    pub account_alias: String,
    pub thread_id: String,
    pub created_at: i64,
    pub verdict: String,
    pub comment: Option<String>,
    pub subject: Option<String>,
    pub paired_to_state: Option<String>,
    pub paired_action: Option<String>,
}

pub type ChannelFeedbackBridgeRow = FeedbackBridgeRow;

impl FeedbackBridgeRow {
    pub fn bridge_cursor(&self) -> String {
        encode_bridge_cursor(&[self.event_id.as_str()])
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FeedbackTuningProfile {
    pub sender_helpful: u64,
    pub sender_dismissed: u64,
    pub domain_helpful: u64,
    pub domain_dismissed: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WritingPreferenceScopeKind {
    Sender,
    Domain,
}

impl WritingPreferenceScopeKind {
    pub fn as_db_str(&self) -> &'static str {
        match self {
            Self::Sender => "sender",
            Self::Domain => "domain",
        }
    }

    fn from_db_str(value: &str) -> Result<Self> {
        match value {
            "sender" => Ok(Self::Sender),
            "domain" => Ok(Self::Domain),
            other => anyhow::bail!("unknown writing preference scope: {other}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WritingPreferenceStatus {
    Candidate,
    Promoted,
    Dismissed,
}

impl WritingPreferenceStatus {
    pub fn as_db_str(&self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Promoted => "promoted",
            Self::Dismissed => "dismissed",
        }
    }

    fn from_db_str(value: &str) -> Result<Self> {
        match value {
            "candidate" => Ok(Self::Candidate),
            "promoted" => Ok(Self::Promoted),
            "dismissed" => Ok(Self::Dismissed),
            other => anyhow::bail!("unknown writing preference status: {other}"),
        }
    }
}

/// An exact, user-visible writing instruction learned for one sender/domain.
/// Raw drafts are never persisted; edit-derived candidates store only these
/// bounded statements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WritingPreference {
    pub id: String,
    pub provider: String,
    pub account_alias: String,
    pub scope_kind: WritingPreferenceScopeKind,
    pub scope_value: String,
    pub statement: String,
    pub status: WritingPreferenceStatus,
    pub source_annotation_id: Option<String>,
    pub evidence_count: u64,
    pub created_at: i64,
    pub updated_at: i64,
    pub schema_version: u32,
}

impl FeedbackTuningProfile {
    const MIN_DISMISSALS: u64 = 3;

    pub fn reduces_noise(&self) -> bool {
        noisy_ratio(self.sender_dismissed, self.sender_helpful)
            || noisy_ratio(self.domain_dismissed, self.domain_helpful)
    }

    pub fn dominant_scope(&self) -> Option<&'static str> {
        if noisy_ratio(self.sender_dismissed, self.sender_helpful) {
            Some("sender")
        } else if noisy_ratio(self.domain_dismissed, self.domain_helpful) {
            Some("domain")
        } else {
            None
        }
    }
}

fn noisy_ratio(dismissed: u64, helpful: u64) -> bool {
    dismissed >= FeedbackTuningProfile::MIN_DISMISSALS
        && dismissed >= helpful.saturating_mul(2).saturating_add(1)
}

fn address_domain(address: &str) -> Option<&str> {
    address
        .rsplit_once('@')
        .map(|(_, domain)| domain.trim())
        .filter(|domain| !domain.is_empty())
}

/// One thread awaiting Phase-2 classification — the neutral, body-blind
/// `ThreadContext` the classifier reads (metadata + the locally-derived
/// `latest_summary`, never a raw body).
#[derive(Debug, Clone)]
pub struct ThreadClassifyRow {
    pub provider: String,
    pub account_alias: String,
    pub thread_id: String,
    pub latest_message_id: String,
    pub latest_message_at: i64,
    pub evidence_message_ids: Vec<String>,
    pub subject: Option<String>,
    pub from_name: Option<String>,
    pub from_address: Option<String>,
    pub recipient_domains: Vec<String>,
    pub label_ids: Vec<String>,
    pub message_count: i64,
    pub last_message_at: Option<i64>,
    /// `user_assist` | `envoy` (whose mailbox this is).
    pub lane: String,
    pub latest_summary: Option<String>,
    pub latest_direction: Option<String>,
    pub latest_intent: Option<String>,
    pub needs_reply_hint: bool,
    pub follow_up_hint: Option<ChannelFollowUpHint>,
    pub distill_brief: Option<ChannelInformationBrief>,
    /// Exact scope-local revision consumed by this classifier input.
    pub distill_revision: i64,
}

pub type ChannelThreadClassifyRow = ThreadClassifyRow;

/// One distilled message row for the evidence bridge (U1).
#[derive(Debug, Clone)]
pub struct DistilledBridgeRow {
    pub provider: String,
    pub account_alias: String,
    pub thread_id: String,
    pub internal_date: i64,
    pub summary: Option<String>,
    pub intent: Option<String>,
    pub subject: Option<String>,
    pub from_name: Option<String>,
    pub from_address: Option<String>,
    pub needs_reply_hint: bool,
    pub follow_up_hint: Option<ChannelFollowUpHint>,
    pub message_id: String,
    pub distill_brief: Option<ChannelInformationBrief>,
    pub distill_revision: i64,
}

pub type ChannelDistilledBridgeRow = DistilledBridgeRow;

/// Distilled-message lane hints used outside the channel store. These are
/// derived outputs from local distillation, not raw message content.
#[derive(Debug, Clone)]
pub struct MessageAttentionHints {
    pub intent: Option<String>,
    pub needs_reply_hint: bool,
    pub follow_up_hint: Option<ChannelFollowUpHint>,
}

pub type ChannelMessageAttentionHints = MessageAttentionHints;

impl MailAssistStoreInner {
    fn acquire_write_guard(&self) -> Result<CrossProcessWriteGuard<'_>> {
        self.acquire_file_guard(Some(self.admission.enter()?))
    }

    fn acquire_file_guard(
        &self,
        permit: Option<DatabasePermit>,
    ) -> Result<CrossProcessWriteGuard<'_>> {
        let file = self
            .write_lock
            .lock()
            .expect("mail assist file lock mutex poisoned");
        file.lock_exclusive()
            .context("acquiring cross-process mail assist write lock")?;
        Ok(CrossProcessWriteGuard {
            file,
            _admission: permit,
        })
    }

    // Reads clone a short-lived connection off `read_conn`, which shares the
    // process's ONE DuckDB instance with `write_conn`.
    //
    // The earlier note here rejected read-connection pooling, and it was right
    // about the thing it measured: a SEPARATE read-only handle
    // (`open_with_flags(.., AccessMode::ReadOnly)`) observes the file+WAL
    // snapshot fixed at open, so caching one would serve stale annotations under
    // the throttled 30s CHECKPOINT window. That hazard is specific to a separate
    // instance. A second connection on the SAME instance sees committed writes
    // immediately through DuckDB's MVCC — checkpoint state is irrelevant — so
    // freshness is preserved here (verified: a same-instance reader observes an
    // uncheckpointed insert made after it was created).
    //
    // Opening a fresh instance per read was costing ~300ms on the Today/
    // follow-ups path: the open itself is only ~7ms, but each new instance
    // starts with an EMPTY buffer pool and re-reads/re-decompresses column data
    // out of the multi-GB file on every request. Sharing the instance keeps the
    // buffer pool warm (~40ms measured for the same query, ~7x faster).
    //
    // Cloning off `read_conn` rather than `write_conn` keeps the read path off
    // the write mutex, so a reader never queues behind an in-flight write. The
    // clone is cheap; the lock is held only for the clone, never for the query.
    //
    // Trade-off: clones are read-write capable, so the `AccessMode::ReadOnly`
    // guard no longer blocks a stray write on a "read" connection. All callers
    // here issue SELECTs; writes must still go through `write_conn` under
    // `acquire_write_guard` so the cross-process lock is honored.
    fn read_connection(&self) -> Result<DatabaseReadConnection> {
        let permit = self.admission.enter()?;
        let connection = self
            .read_conn
            .lock()
            .map_err(|_| anyhow::anyhow!("mail reader lock poisoned"))?
            .try_clone()?;
        Ok(DatabaseReadConnection::new(connection, permit))
    }

    /// Same throttled-checkpoint contract as `FeedStoreInner::maybe_checkpoint`:
    /// writes inside the window stay in the WAL (durable, replayed on next
    /// open); only page compaction is deferred. This method is called only
    /// after the mutation has committed, so checkpoint maintenance must never
    /// retroactively report that durable mutation as failed. Besides lying to
    /// the caller, that would make retries double-apply counters such as
    /// backfill yield/run totals.
    fn maybe_checkpoint(&self, conn: &Connection) -> Result<()> {
        self.maybe_checkpoint_with(|| {
            conn.execute_batch("CHECKPOINT")
                .context("running duckdb checkpoint")
        })
    }

    fn maybe_checkpoint_with<F>(&self, checkpoint: F) -> Result<()>
    where
        F: FnOnce() -> Result<()>,
    {
        let mut last = self
            .last_checkpoint_at
            .lock()
            .expect("mail assist checkpoint timestamp mutex poisoned");
        if last.elapsed() < CHECKPOINT_THROTTLE {
            return Ok(());
        }
        if let Err(error) = checkpoint() {
            // A busy reader/transaction can temporarily reject CHECKPOINT.
            // The WAL already owns the committed bytes, so defer compaction
            // and throttle the next attempt instead of failing the write.
            // Expected under concurrent writers, so it is not a warning: the
            // next throttled attempt compacts once the other transaction ends.
            tracing::debug!(
                database = %self.db_path.display(),
                error = %format!("{error:#}"),
                "mail assist checkpoint deferred; committed write remains durable"
            );
            *last = Instant::now();
            return Ok(());
        }
        *last = Instant::now();
        Ok(())
    }
}

impl Drop for CrossProcessWriteGuard<'_> {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn checkpoint_connection(conn: &Connection) -> Result<()> {
    conn.execute_batch("CHECKPOINT")
        .context("running duckdb checkpoint")
}

fn load_distill_backfill_runtime(conn: &Connection) -> Result<DistillBackfillRuntimeState> {
    let encoded = {
        let mut stmt = conn
            .prepare("SELECT meta_value FROM mail_assist_meta WHERE meta_key = ?")
            .context("preparing distill backfill runtime state query")?;
        let mut rows = stmt
            .query(params![DISTILL_BACKFILL_RUNTIME_META_KEY])
            .context("reading distill backfill runtime state")?;
        match rows
            .next()
            .context("advancing distill backfill runtime state query")?
        {
            Some(row) => Some(
                row.get::<_, String>(0)
                    .context("decoding distill backfill runtime state")?,
            ),
            None => None,
        }
    };
    encoded
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .context("decoding distill backfill runtime state")
        .map(Option::unwrap_or_default)
}

fn persist_distill_backfill_runtime(
    conn: &Connection,
    state: &DistillBackfillRuntimeState,
) -> Result<()> {
    let encoded =
        serde_json::to_string(state).context("encoding distill backfill runtime state")?;
    conn.execute(
        "INSERT INTO mail_assist_meta (meta_key, meta_value) VALUES (?, ?) \
         ON CONFLICT (meta_key) DO UPDATE SET meta_value = excluded.meta_value",
        params![DISTILL_BACKFILL_RUNTIME_META_KEY, encoded],
    )
    .context("persisting distill backfill runtime state")?;
    Ok(())
}

fn initialize_mail_assist_schema(conn: &Connection, template_schema: &str) -> Result<()> {
    let schema_initialized: bool = conn
        .prepare(
            "SELECT COUNT(*) > 0 FROM information_schema.tables \
             WHERE table_schema = 'main' AND table_name = 'mail_assist_meta'",
        )?
        .query_row([], |row| row.get(0))
        .context("checking whether the mail assist schema is initialized")?;

    if schema_initialized {
        // Existing tables do not gain columns from CREATE TABLE IF NOT EXISTS.
        // Migrate first so indexes in the current template can safely reference
        // columns introduced after the database was created.
        apply_schema_migrations(conn).context("applying mail assist schema migrations")?;
        conn.execute_batch(template_schema)
            .context("running mail assist template schema")?;
    } else {
        conn.execute_batch(template_schema)
            .context("running mail assist template schema")?;
        apply_schema_migrations(conn).context("applying mail assist schema migrations")?;
    }
    Ok(())
}

/// Validate the stored schema version in `mail_assist_meta` against
/// [`MAIL_ASSIST_DB_SCHEMA_VERSION`].
///
/// - No version row (fresh bootstrap — [`BOOTSTRAP_DDL`] creates the
///   current shape directly) → record the current version.
/// - A v2+ older stored version → add current columns/tables in place.
/// - Any other LOWER stored version → loud unsupported error. The worker
///   never ran before the Phase 1b (v2) shape, so no v1 database is expected.
/// - A HIGHER stored version → loud error too (a newer deployment's DB
///   opened by an older binary).
fn apply_schema_migrations(conn: &Connection) -> Result<()> {
    let stored: Option<String> = {
        let mut stmt = conn
            .prepare("SELECT meta_value FROM mail_assist_meta WHERE meta_key = 'schema_version'")?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(row) => Some(row.get(0)?),
            None => None,
        }
    };
    let stored_version = stored
        .as_deref()
        .map(str::parse::<u32>)
        .transpose()
        .context("parsing stored mail assist schema version")?
        .unwrap_or(0);
    if stored_version == 0 {
        ensure_current_columns(conn)?;
        conn.execute(
            "INSERT INTO mail_assist_meta (meta_key, meta_value) VALUES ('schema_version', ?)
             ON CONFLICT (meta_key) DO UPDATE SET meta_value = excluded.meta_value",
            params![MAIL_ASSIST_DB_SCHEMA_VERSION.to_string()],
        )
        .context("recording mail assist schema version")?;
        return Ok(());
    }
    if (2..MAIL_ASSIST_DB_SCHEMA_VERSION).contains(&stored_version) {
        ensure_current_columns(conn)?;
        conn.execute(
            "UPDATE mail_assist_meta SET meta_value = ? WHERE meta_key = 'schema_version'",
            params![MAIL_ASSIST_DB_SCHEMA_VERSION.to_string()],
        )
        .context("recording migrated mail assist schema version")?;
        return Ok(());
    }
    if stored_version != MAIL_ASSIST_DB_SCHEMA_VERSION {
        anyhow::bail!(
            "mail assist store schema version {stored_version} is unsupported (this build \
             requires v{MAIL_ASSIST_DB_SCHEMA_VERSION})"
        );
    }
    Ok(())
}

fn ensure_current_columns(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "ALTER TABLE mail_messages ADD COLUMN IF NOT EXISTS needs_reply_hint BOOLEAN DEFAULT FALSE;
         ALTER TABLE mail_messages ADD COLUMN IF NOT EXISTS follow_up_hint_json JSON NULL;
         ALTER TABLE mail_messages ADD COLUMN IF NOT EXISTS distill_evidence_message_ids_json JSON NULL;
         ALTER TABLE mail_messages ADD COLUMN IF NOT EXISTS distill_brief_json JSON;
         ALTER TABLE mail_messages ADD COLUMN IF NOT EXISTS distill_contract_version INTEGER;
         ALTER TABLE mail_messages ADD COLUMN IF NOT EXISTS distilled_at BIGINT;
         ALTER TABLE mail_messages ADD COLUMN IF NOT EXISTS distill_revision BIGINT;
         ALTER TABLE mail_messages ADD COLUMN IF NOT EXISTS distill_backfill_attempts INTEGER DEFAULT 0;
         ALTER TABLE mail_messages ADD COLUMN IF NOT EXISTS distill_backfill_next_retry_at BIGINT;
         ALTER TABLE mail_messages ADD COLUMN IF NOT EXISTS distill_backfill_last_error TEXT;
         ALTER TABLE mail_messages ADD COLUMN IF NOT EXISTS classify_attempts INTEGER DEFAULT 0;
         ALTER TABLE mail_messages ADD COLUMN IF NOT EXISTS classify_next_retry_at BIGINT NULL;
         ALTER TABLE mail_messages ADD COLUMN IF NOT EXISTS classify_last_error TEXT NULL;
         ALTER TABLE mail_messages ADD COLUMN IF NOT EXISTS classify_failed_at BIGINT NULL;
         ALTER TABLE mail_annotations ADD COLUMN IF NOT EXISTS evidence_message_id TEXT NULL;
         ALTER TABLE mail_annotations ADD COLUMN IF NOT EXISTS evidence_message_at BIGINT NULL;
         ALTER TABLE mail_annotations ADD COLUMN IF NOT EXISTS classification_input_revision BIGINT NULL;
         ALTER TABLE mail_annotations ADD COLUMN IF NOT EXISTS semantic_features_json JSON NULL;
         ALTER TABLE mail_annotations ADD COLUMN IF NOT EXISTS attention_lane TEXT DEFAULT 'follow_up';
         UPDATE mail_annotations SET attention_lane = COALESCE(
             json_extract_string(proposed_action_json, '$.attention_lane'), 'follow_up'
         );
         CREATE INDEX IF NOT EXISTS idx_mail_messages_distill_revision
             ON mail_messages (principal, workspace, distill_revision);
         CREATE INDEX IF NOT EXISTS idx_mail_messages_distill_backfill
             ON mail_messages (principal, workspace, distill_state, sensitive_suppressed,
                               distill_contract_version, internal_date);
         CREATE INDEX IF NOT EXISTS idx_mail_annotations_today
             ON mail_annotations (principal, workspace, attention_lane, state,
                                  created_at DESC, id DESC);
         CREATE INDEX IF NOT EXISTS idx_mail_events_annotation_state
             ON mail_assist_events (principal, workspace, annotation_id, to_state, created_at);
         CREATE TABLE IF NOT EXISTS mail_distill_revision_counters (
             principal TEXT NOT NULL,
             workspace TEXT NOT NULL,
             last_revision BIGINT NOT NULL,
             PRIMARY KEY (principal, workspace)
         );
         CREATE TABLE IF NOT EXISTS mail_annotation_action_claims (
             principal TEXT NOT NULL,
             workspace TEXT NOT NULL,
             annotation_id TEXT NOT NULL,
             action TEXT NOT NULL,
             claim_id TEXT NOT NULL,
             task_id TEXT NULL,
             created_at BIGINT NOT NULL,
             updated_at BIGINT NOT NULL,
             schema_version INTEGER NOT NULL,
             PRIMARY KEY (principal, workspace, annotation_id, action)
         );
         CREATE TABLE IF NOT EXISTS channel_writing_preferences (
             principal TEXT NOT NULL,
             workspace TEXT NOT NULL,
             id TEXT NOT NULL,
             provider TEXT NOT NULL,
             account_alias TEXT NOT NULL,
             scope_kind TEXT NOT NULL,
             scope_value TEXT NOT NULL,
             statement TEXT NOT NULL,
             status TEXT NOT NULL,
             source_annotation_id TEXT NULL,
             evidence_count BIGINT NOT NULL DEFAULT 1,
             created_at BIGINT NOT NULL,
             updated_at BIGINT NOT NULL,
             schema_version INTEGER NOT NULL,
             PRIMARY KEY (principal, workspace, id),
             UNIQUE (principal, workspace, provider, account_alias, scope_kind, scope_value, statement)
         );
         CREATE INDEX IF NOT EXISTS idx_channel_writing_preferences_scope
             ON channel_writing_preferences (
                 principal, workspace, provider, account_alias, scope_kind, scope_value, status
             );
         CREATE TABLE IF NOT EXISTS channel_action_drafts (
             principal TEXT NOT NULL,
             workspace TEXT NOT NULL,
             compose_id TEXT NOT NULL,
             annotation_id TEXT NOT NULL,
             action_id TEXT NOT NULL,
             text TEXT NOT NULL,
             created_at BIGINT NOT NULL,
             schema_version INTEGER NOT NULL,
             PRIMARY KEY (principal, workspace, compose_id)
         );
         CREATE INDEX IF NOT EXISTS idx_channel_action_drafts_annotation
             ON channel_action_drafts (principal, workspace, annotation_id, action_id, created_at DESC);",
    )
    .context("applying mail assist schema columns")
}

/// Allocate the next scope-local distill revision. Callers hold the store's
/// write guard and invoke this inside the same transaction as the result
/// update, so concurrent completions cannot receive the same value and a
/// failed result write rolls the allocation back. The durable counter remains
/// monotonic even when suppression clears the highest message revision.
fn allocate_distill_revision(conn: &Connection, principal: &str, workspace: &str) -> Result<i64> {
    let stored: Option<i64> = {
        let mut stmt = conn.prepare(
            "SELECT last_revision FROM mail_distill_revision_counters \
             WHERE principal = ? AND workspace = ?",
        )?;
        let mut rows = stmt.query(params![principal, workspace])?;
        rows.next()?.map(|row| row.get(0)).transpose()?
    };
    let row_max: i64 = {
        let mut stmt = conn.prepare(
            "SELECT COALESCE(MAX(distill_revision), 0) FROM mail_messages \
             WHERE principal = ? AND workspace = ?",
        )?;
        let mut rows = stmt.query(params![principal, workspace])?;
        rows.next()?.map(|row| row.get(0)).transpose()?.unwrap_or(0)
    };
    let current = stored.unwrap_or(0).max(row_max);
    let next = current
        .checked_add(1)
        .context("distill revision counter overflow")?;
    conn.execute(
        "INSERT INTO mail_distill_revision_counters \
             (principal, workspace, last_revision) VALUES (?, ?, ?) \
         ON CONFLICT (principal, workspace) DO UPDATE \
             SET last_revision = excluded.last_revision",
        params![principal, workspace, next],
    )
    .context("advancing distill revision counter")?;
    Ok(next)
}

/// Run `body` inside an explicit transaction so multi-statement writes
/// (annotation row + its audit event) commit or roll back together.
fn with_transaction<T>(conn: &Connection, body: impl FnOnce() -> Result<T>) -> Result<T> {
    conn.execute_batch("BEGIN TRANSACTION")
        .context("beginning mail assist transaction")?;
    match body() {
        Ok(value) => {
            conn.execute_batch("COMMIT")
                .context("committing mail assist transaction")?;
            Ok(value)
        },
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        },
    }
}

fn classify_retry_backoff_ms(attempts: i64) -> i64 {
    match attempts {
        i if i <= 1 => CLASSIFY_RETRY_BACKOFF_MS[0],
        2 => CLASSIFY_RETRY_BACKOFF_MS[1],
        _ => CLASSIFY_RETRY_BACKOFF_MS[1],
    }
}

fn trim_for_storage(value: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in value.chars().enumerate() {
        if idx >= max_chars {
            out.push_str("...");
            return out;
        }
        out.push(ch);
    }
    out
}

fn encode_bridge_cursor(parts: &[&str]) -> String {
    serde_json::json!(parts).to_string()
}

fn decode_bridge_cursor(cursor: Option<&str>, arity: usize) -> Vec<String> {
    let empty = || vec![String::new(); arity];
    let Some(cursor) = cursor else {
        return empty();
    };
    let Ok(parts) = serde_json::from_str::<Vec<String>>(cursor) else {
        return empty();
    };
    if parts.len() == arity {
        parts
    } else {
        empty()
    }
}

fn insert_annotation_record(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    annotation: &mut MailThreadAnnotation,
    actor: MailAssistActor,
    detail: Option<serde_json::Value>,
) -> Result<()> {
    if let Some(lane) = query_thread_lane(
        conn,
        principal,
        workspace,
        &annotation.provider,
        &annotation.account_alias,
        &annotation.thread_id,
    )? {
        annotation.lane = lane;
    }
    let evidence_refs_json = serde_json::to_string(&annotation.evidence_refs)
        .context("serializing mail annotation evidence refs")?;
    let proposed_action_json = annotation
        .proposed_action
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .context("serializing mail annotation proposed action")?;
    let semantic_features_json = annotation
        .semantic_features
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .context("serializing mail annotation semantic features")?;
    let attention_lane = annotation
        .proposed_action
        .as_ref()
        .and_then(|action| action.get("attention_lane"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|lane| !lane.is_empty())
        .unwrap_or("follow_up")
        .to_string();
    conn.execute(
        "INSERT INTO mail_annotations (
            principal, workspace, id, provider, account_alias, thread_id,
            lane, state, label, confidence, reason, evidence_refs_json,
            evidence_message_id, evidence_message_at, classification_input_revision,
            semantic_features_json, proposed_action_json, attention_lane, provenance,
            created_at, updated_at, schema_version
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            principal,
            workspace,
            annotation.id,
            annotation.provider,
            annotation.account_alias,
            annotation.thread_id,
            annotation.lane.as_db_str(),
            annotation.state.as_db_str(),
            annotation.label,
            annotation.confidence,
            annotation.reason,
            evidence_refs_json,
            annotation.evidence_message_id,
            annotation.evidence_message_at,
            annotation.classification_input_revision,
            semantic_features_json,
            proposed_action_json,
            attention_lane,
            annotation.provenance,
            annotation.created_at,
            annotation.updated_at,
            annotation.schema_version,
        ],
    )
    .context("inserting mail annotation")?;
    insert_event(
        conn,
        principal,
        workspace,
        &MailAssistEvent {
            schema_version: annotation.schema_version,
            id: Uuid::new_v4().to_string(),
            annotation_id: Some(annotation.id.clone()),
            provider: annotation.provider.clone(),
            account_alias: annotation.account_alias.clone(),
            thread_id: Some(annotation.thread_id.clone()),
            event_type: MailAssistEventType::AnnotationCreated,
            actor,
            from_state: None,
            to_state: Some(annotation.state),
            detail,
            created_at: annotation.created_at,
        },
    )
}

fn update_annotation_record(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    previous: &MailThreadAnnotation,
    annotation: &MailThreadAnnotation,
    actor: MailAssistActor,
    detail: serde_json::Value,
) -> Result<()> {
    let evidence_refs_json = serde_json::to_string(&annotation.evidence_refs)
        .context("serializing refreshed annotation evidence refs")?;
    let proposed_action_json = annotation
        .proposed_action
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .context("serializing refreshed annotation action")?;
    let semantic_features_json = annotation
        .semantic_features
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .context("serializing refreshed annotation semantic features")?;
    let attention_lane = annotation
        .proposed_action
        .as_ref()
        .and_then(|action| action.get("attention_lane"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|lane| !lane.is_empty())
        .unwrap_or("follow_up");
    let changed = conn.execute(
        "UPDATE mail_annotations SET state = ?, label = ?, confidence = ?, reason = ?, \
             evidence_refs_json = ?, evidence_message_id = ?, evidence_message_at = ?, \
             classification_input_revision = ?, semantic_features_json = ?, proposed_action_json = ?, attention_lane = ?, provenance = ?, \
             updated_at = ?, schema_version = ? \
         WHERE principal = ? AND workspace = ? AND id = ?",
        params![
            annotation.state.as_db_str(),
            annotation.label,
            annotation.confidence,
            annotation.reason,
            evidence_refs_json,
            annotation.evidence_message_id,
            annotation.evidence_message_at,
            annotation.classification_input_revision,
            semantic_features_json,
            proposed_action_json,
            attention_lane,
            annotation.provenance,
            annotation.updated_at,
            annotation.schema_version,
            principal,
            workspace,
            annotation.id,
        ],
    )?;
    if changed != 1 {
        anyhow::bail!("annotation disappeared during revision-aware refresh")
    }
    insert_event(
        conn,
        principal,
        workspace,
        &MailAssistEvent {
            schema_version: annotation.schema_version,
            id: Uuid::new_v4().to_string(),
            annotation_id: Some(annotation.id.clone()),
            provider: annotation.provider.clone(),
            account_alias: annotation.account_alias.clone(),
            thread_id: Some(annotation.thread_id.clone()),
            event_type: MailAssistEventType::AnnotationUpdated,
            actor,
            from_state: Some(previous.state),
            to_state: Some(annotation.state),
            detail: Some(detail),
            created_at: annotation.updated_at,
        },
    )
}

// A same-revision classifier refresh can arrive after background extraction.
// Keep the successful source receipt when the same producer returns invalid or
// missing features. A new source revision or extractor contract still replaces it.
fn preserve_successful_semantics_on_refresh(
    previous: &MailThreadAnnotation,
    incoming: &mut MailThreadAnnotation,
) {
    use magician::magician_v2::attention::learning::{
        deserialize_semantic_envelope, SemanticExtractionStatus, SemanticExtractorIdentity,
    };
    let Some(revision) = incoming.classification_input_revision else {
        return;
    };
    if previous.classification_input_revision != Some(revision) {
        return;
    }
    let Some(next) = deserialize_semantic_envelope(incoming.semantic_features.as_ref()) else {
        return;
    };
    if next.status == SemanticExtractionStatus::Succeeded {
        return;
    }
    let Some(current) = deserialize_semantic_envelope(previous.semantic_features.as_ref()) else {
        return;
    };
    if current.input_revision == revision
        && current.is_compatible_with_extractor(
            Some(&format!("distill:{revision}")),
            &next.prompt_version,
            &SemanticExtractorIdentity {
                model: next.model,
                profile: next.profile,
            },
        )
        && current.schema_version == next.schema_version
        && current.extractor_contract == next.extractor_contract
    {
        incoming.semantic_features = previous.semantic_features.clone();
    }
}

/// Append-only insert into `mail_assist_events` — the ONLY write this
/// module ever performs on the audit table (no UPDATE/DELETE paths exist).
fn insert_event(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    event: &MailAssistEvent,
) -> Result<()> {
    let detail_json = event
        .detail
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .context("serializing mail assist event detail")?;
    conn.execute(
        "INSERT INTO mail_assist_events (
            principal, workspace, id, annotation_id, provider, account_alias,
            thread_id, event_type, actor, from_state, to_state, detail_json,
            created_at, schema_version
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            principal,
            workspace,
            event.id,
            event.annotation_id,
            event.provider,
            event.account_alias,
            event.thread_id,
            event.event_type.as_db_str(),
            event.actor.as_db_str(),
            event
                .from_state
                .as_ref()
                .map(MailAnnotationState::as_db_str),
            event.to_state.as_ref().map(MailAnnotationState::as_db_str),
            detail_json,
            event.created_at,
            event.schema_version,
        ],
    )
    .context("appending mail assist audit event")?;
    Ok(())
}

fn insert_transition_feedback_event(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    annotation: &MailThreadAnnotation,
    actor: MailAssistActor,
    feedback: &AnnotationTransitionFeedback,
    created_at: i64,
) -> Result<()> {
    let feedback = MailAssistUserFeedback {
        schema_version: annotation.schema_version,
        id: feedback
            .event_id
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        annotation_id: annotation.id.clone(),
        provider: annotation.provider.clone(),
        account_alias: annotation.account_alias.clone(),
        thread_id: Some(annotation.thread_id.clone()),
        verdict: feedback.verdict,
        comment: feedback.comment.clone(),
        actor,
        created_at,
    };
    let detail = serde_json::to_value(&feedback).context("serializing mail assist feedback")?;
    let event = MailAssistEvent {
        schema_version: feedback.schema_version,
        id: feedback.id.clone(),
        annotation_id: Some(feedback.annotation_id.clone()),
        provider: feedback.provider.clone(),
        account_alias: feedback.account_alias.clone(),
        thread_id: feedback.thread_id.clone(),
        event_type: MailAssistEventType::Feedback,
        actor: feedback.actor,
        from_state: None,
        to_state: None,
        detail: Some(detail),
        created_at: feedback.created_at,
    };
    insert_event(conn, principal, workspace, &event)
}

fn query_message_thread_id(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    provider: &str,
    account_alias: &str,
    message_id: &str,
) -> Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT thread_id FROM mail_messages \
         WHERE principal = ? AND workspace = ? AND provider = ? \
           AND account_alias = ? AND message_id = ?",
    )?;
    let mut rows = stmt.query(params![
        principal,
        workspace,
        provider,
        account_alias,
        message_id
    ])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

fn refresh_thread_latest_summary(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    provider: &str,
    account_alias: &str,
    thread_id: &str,
) -> Result<()> {
    let latest_summary: Option<String> = {
        let mut stmt = conn.prepare(
            "SELECT summary FROM mail_messages \
             WHERE principal = ? AND workspace = ? AND provider = ? \
               AND account_alias = ? AND thread_id = ? \
               AND distill_state = 'done' AND sensitive_suppressed = FALSE \
               AND summary IS NOT NULL \
             ORDER BY internal_date DESC, message_id DESC LIMIT 1",
        )?;
        let mut rows = stmt.query(params![
            principal,
            workspace,
            provider,
            account_alias,
            thread_id
        ])?;
        match rows.next()? {
            Some(row) => row.get(0)?,
            None => None,
        }
    };
    conn.execute(
        "UPDATE mail_threads SET latest_summary = ? \
         WHERE principal = ? AND workspace = ? AND provider = ? \
           AND account_alias = ? AND thread_id = ?",
        params![
            latest_summary,
            principal,
            workspace,
            provider,
            account_alias,
            thread_id
        ],
    )
    .context("refreshing mail thread latest safe summary")?;
    Ok(())
}

/// Lane of a thread row, if the thread exists — the annotation-create
/// inheritance read.
fn query_thread_lane(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    provider: &str,
    account_alias: &str,
    thread_id: &str,
) -> Result<Option<ChannelLane>> {
    let mut stmt = conn.prepare(
        "SELECT lane FROM mail_threads \
         WHERE principal = ? AND workspace = ? AND provider = ? \
           AND account_alias = ? AND thread_id = ?",
    )?;
    let mut rows = stmt.query(params![
        principal,
        workspace,
        provider,
        account_alias,
        thread_id
    ])?;
    match rows.next()? {
        Some(row) => {
            let lane: String = row.get(0)?;
            Ok(Some(ChannelLane::from_db_str(&lane)?))
        },
        None => Ok(None),
    }
}

fn query_annotation_action_claim(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    annotation_id: &str,
    action: &str,
) -> Result<Option<AnnotationActionClaimRow>> {
    let mut stmt = conn.prepare(
        "SELECT claim_id, task_id, updated_at FROM mail_annotation_action_claims \
         WHERE principal = ? AND workspace = ? AND annotation_id = ? AND action = ?",
    )?;
    let mut rows = stmt.query(params![principal, workspace, annotation_id, action])?;
    match rows.next()? {
        Some(row) => Ok(Some(AnnotationActionClaimRow {
            claim_id: row.get(0)?,
            task_id: row.get(1)?,
            updated_at: row.get(2)?,
        })),
        None => Ok(None),
    }
}

fn query_active_annotation_action_claim(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    annotation_id: &str,
    occurred_at: i64,
) -> Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT action FROM mail_annotation_action_claims \
         WHERE principal = ? AND workspace = ? AND annotation_id = ? AND task_id IS NULL \
           AND ? - updated_at < ? \
         ORDER BY created_at ASC LIMIT 1",
    )?;
    let mut rows = stmt.query(params![
        principal,
        workspace,
        annotation_id,
        occurred_at,
        ANNOTATION_ACTION_CLAIM_STALE_MS
    ])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

/// Claims that make classifier/resurfacing detail refresh unsafe. A completed
/// claim remains protected even if the process died before transitioning the
/// annotation state; a fresh incomplete claim is protected for the ordinary
/// stale-claim window.
fn query_annotation_refresh_blocking_claim(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    annotation_id: &str,
    occurred_at: i64,
) -> Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT action FROM mail_annotation_action_claims \
         WHERE principal = ? AND workspace = ? AND annotation_id = ? \
           AND (task_id IS NOT NULL OR ? - updated_at < ?) \
         ORDER BY created_at ASC LIMIT 1",
    )?;
    let mut rows = stmt.query(params![
        principal,
        workspace,
        annotation_id,
        occurred_at,
        ANNOTATION_ACTION_CLAIM_STALE_MS
    ])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

fn query_annotation(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    annotation_id: &str,
) -> Result<Option<MailThreadAnnotation>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ANNOTATION_COLUMNS} \
         FROM mail_annotations WHERE principal = ? AND workspace = ? AND id = ?",
    ))?;
    let mut rows = stmt.query(params![principal, workspace, annotation_id])?;
    if let Some(row) = rows.next()? {
        Ok(Some(map_annotation_row(row)?))
    } else {
        Ok(None)
    }
}

fn query_annotation_for_evidence(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    provider: &str,
    account_alias: &str,
    thread_id: &str,
    evidence_message_id: &str,
) -> Result<Option<MailThreadAnnotation>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ANNOTATION_COLUMNS} FROM mail_annotations \
         WHERE principal = ? AND workspace = ? AND provider = ? AND account_alias = ? \
           AND thread_id = ? AND evidence_message_id = ? \
         ORDER BY updated_at DESC, id DESC LIMIT 1"
    ))?;
    let mut rows = stmt.query(params![
        principal,
        workspace,
        provider,
        account_alias,
        thread_id,
        evidence_message_id
    ])?;
    rows.next()?.map(map_annotation_row).transpose()
}

fn query_refreshable_annotation_for_thread(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    provider: &str,
    account_alias: &str,
    thread_id: &str,
    actionable: bool,
) -> Result<Option<MailThreadAnnotation>> {
    let state_predicate = if actionable {
        "state = 'needs_approval'"
    } else {
        "evidence_message_id IS NULL AND state IN ('observed', 'classified')"
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT {ANNOTATION_COLUMNS} FROM mail_annotations \
         WHERE principal = ? AND workspace = ? AND provider = ? AND account_alias = ? \
           AND thread_id = ? AND {state_predicate} \
         ORDER BY updated_at DESC, id DESC LIMIT 1"
    ))?;
    let mut rows = stmt.query(params![
        principal,
        workspace,
        provider,
        account_alias,
        thread_id
    ])?;
    rows.next()?.map(map_annotation_row).transpose()
}

fn query_message_classification_eligibility(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    annotation: &MailThreadAnnotation,
) -> Result<(Option<i64>, bool)> {
    let Some(message_id) = annotation.evidence_message_id.as_deref() else {
        return Ok((None, false));
    };
    let mut stmt = conn.prepare(
        "SELECT distill_revision, distill_state, sensitive_suppressed, thread_id \
         FROM mail_messages WHERE principal = ? AND workspace = ? AND provider = ? \
           AND account_alias = ? AND message_id = ? LIMIT 1",
    )?;
    let mut rows = stmt.query(params![
        principal,
        workspace,
        annotation.provider,
        annotation.account_alias,
        message_id
    ])?;
    let Some(row) = rows.next()? else {
        return Ok((None, false));
    };
    let revision: Option<i64> = row.get(0)?;
    let state: String = row.get(1)?;
    let suppressed: bool = row.get(2)?;
    let thread_id: String = row.get(3)?;
    Ok((
        revision,
        state == "done" && !suppressed && thread_id == annotation.thread_id,
    ))
}

/// Decode one [`THREAD_COLUMNS`]-ordered row.
fn map_thread_row(row: &duckdb::Row<'_>) -> Result<MailThreadRecord> {
    let recipient_domains_json: String = row.get(7)?;
    let label_ids_json: String = row.get(8)?;
    let origin: String = row.get(13)?;
    let lane: String = row.get(17)?;
    Ok(MailThreadRecord {
        provider: row.get(0)?,
        account_alias: row.get(1)?,
        thread_id: row.get(2)?,
        account_email: row.get(3)?,
        subject: row.get(4)?,
        latest_from_name: row.get(5)?,
        latest_from_address: row.get(6)?,
        recipient_domains: serde_json::from_str(&recipient_domains_json)
            .context("parsing mail thread recipient domains JSON")?,
        label_ids: serde_json::from_str(&label_ids_json)
            .context("parsing mail thread label ids JSON")?,
        message_count: row.get(9)?,
        last_message_at: row.get(10)?,
        provider_cursor: row.get(11)?,
        sensitive_suppressed: row.get(12)?,
        origin: MailRecordOrigin::from_db_str(&origin)?,
        first_observed_at: row.get(14)?,
        last_observed_at: row.get(15)?,
        schema_version: row.get::<_, u32>(16)?,
        lane: ChannelLane::from_db_str(&lane)?,
        latest_summary: row.get(18)?,
    })
}

/// Decode one [`MESSAGE_COLUMNS`]-ordered row.
fn map_message_row(row: &duckdb::Row<'_>) -> Result<MailMessageMeta> {
    let label_ids_json: String = row.get(6)?;
    let to_domains_json: String = row.get(10)?;
    let cc_domains_json: String = row.get(11)?;
    let origin: String = row.get(15)?;
    let direction: Option<String> = row.get(17)?;
    let follow_up_hint_json: Option<String> = row.get(21)?;
    let distill_state: String = row.get(22)?;
    let distill_brief_json: Option<String> = row.get(24)?;
    Ok(MailMessageMeta {
        provider: row.get(0)?,
        account_alias: row.get(1)?,
        message_id: row.get(2)?,
        thread_id: row.get(3)?,
        account_email: row.get(4)?,
        provider_cursor: row.get(5)?,
        label_ids: serde_json::from_str(&label_ids_json)
            .context("parsing mail message label ids JSON")?,
        subject: row.get(7)?,
        from_name: row.get(8)?,
        from_address: row.get(9)?,
        to_domains: serde_json::from_str(&to_domains_json)
            .context("parsing mail message to domains JSON")?,
        cc_domains: serde_json::from_str(&cc_domains_json)
            .context("parsing mail message cc domains JSON")?,
        internal_date: row.get(12)?,
        observed_at: row.get(13)?,
        sensitive_suppressed: row.get(14)?,
        origin: MailRecordOrigin::from_db_str(&origin)?,
        schema_version: row.get::<_, u32>(16)?,
        direction: direction
            .as_deref()
            .map(MessageDirection::from_db_str)
            .transpose()?,
        summary: row.get(18)?,
        intent: row.get(19)?,
        needs_reply_hint: row.get(20)?,
        follow_up_hint: follow_up_hint_json
            .as_deref()
            .map(serde_json::from_str::<ChannelFollowUpHint>)
            .transpose()
            .context("parsing mail message follow-up hint JSON")?,
        distill_brief: distill_brief_json
            .as_deref()
            .map(serde_json::from_str::<ChannelInformationBrief>)
            .transpose()
            .context("parsing mail message information brief JSON")?,
        distill_contract_version: row.get::<_, Option<u32>>(25)?,
        distilled_at: row.get(26)?,
        distill_revision: row.get(27)?,
        distill_state: DistillState::from_db_str(&distill_state)?,
        distill_attempts: row.get(23)?,
    })
}

/// Decode one [`ANNOTATION_COLUMNS`]-ordered row.
fn map_annotation_row(row: &duckdb::Row<'_>) -> Result<MailThreadAnnotation> {
    let state: String = row.get(4)?;
    let evidence_refs_json: String = row.get(8)?;
    let proposed_action_json: Option<String> = row.get(9)?;
    let lane: String = row.get(14)?;
    let semantic_features_json: Option<String> = row.get(18)?;
    Ok(MailThreadAnnotation {
        id: row.get(0)?,
        provider: row.get(1)?,
        account_alias: row.get(2)?,
        thread_id: row.get(3)?,
        state: MailAnnotationState::from_db_str(&state)?,
        label: row.get(5)?,
        confidence: row.get(6)?,
        reason: row.get(7)?,
        evidence_refs: serde_json::from_str(&evidence_refs_json)
            .context("parsing mail annotation evidence refs JSON")?,
        evidence_message_id: row.get(15)?,
        evidence_message_at: row.get(16)?,
        classification_input_revision: row.get(17)?,
        semantic_features: semantic_features_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .context("parsing mail annotation semantic features JSON")?,
        proposed_action: proposed_action_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .context("parsing mail annotation proposed action JSON")?,
        provenance: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
        schema_version: row.get::<_, u32>(13)?,
        lane: ChannelLane::from_db_str(&lane)?,
    })
}

fn map_writing_preference_row(row: &duckdb::Row<'_>) -> Result<WritingPreference> {
    let scope_kind: String = row.get(3)?;
    let status: String = row.get(6)?;
    Ok(WritingPreference {
        id: row.get(0)?,
        provider: row.get(1)?,
        account_alias: row.get(2)?,
        scope_kind: WritingPreferenceScopeKind::from_db_str(&scope_kind)?,
        scope_value: row.get(4)?,
        statement: row.get(5)?,
        status: WritingPreferenceStatus::from_db_str(&status)?,
        source_annotation_id: row.get(7)?,
        evidence_count: row.get::<_, i64>(8)?.max(0) as u64,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
        schema_version: row.get::<_, i64>(11)? as u32,
    })
}

fn query_writing_preference_by_key(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    provider: &str,
    account_alias: &str,
    scope_kind: WritingPreferenceScopeKind,
    scope_value: &str,
    statement: &str,
) -> Result<Option<WritingPreference>> {
    let mut stmt = conn.prepare(
        "SELECT id, provider, account_alias, scope_kind, scope_value,
                statement, status, source_annotation_id, evidence_count,
                created_at, updated_at, schema_version
         FROM channel_writing_preferences
         WHERE principal = ? AND workspace = ? AND provider = ?
           AND account_alias = ? AND scope_kind = ? AND scope_value = ?
           AND statement = ?",
    )?;
    let mut rows = stmt.query(params![
        principal,
        workspace,
        provider,
        account_alias,
        scope_kind.as_db_str(),
        scope_value,
        statement,
    ])?;
    rows.next()?.map(map_writing_preference_row).transpose()
}

fn query_writing_preference_by_id(
    conn: &Connection,
    principal: &str,
    workspace: &str,
    id: &str,
) -> Result<Option<WritingPreference>> {
    let mut stmt = conn.prepare(
        "SELECT id, provider, account_alias, scope_kind, scope_value,
                statement, status, source_annotation_id, evidence_count,
                created_at, updated_at, schema_version
         FROM channel_writing_preferences
         WHERE principal = ? AND workspace = ? AND id = ?",
    )?;
    let mut rows = stmt.query(params![principal, workspace, id])?;
    rows.next()?.map(map_writing_preference_row).transpose()
}

fn map_event_row(row: &duckdb::Row<'_>) -> Result<MailAssistEvent> {
    let event_type: String = row.get(5)?;
    let actor: String = row.get(6)?;
    let from_state: Option<String> = row.get(7)?;
    let to_state: Option<String> = row.get(8)?;
    let detail_json: Option<String> = row.get(9)?;
    Ok(MailAssistEvent {
        id: row.get(0)?,
        annotation_id: row.get(1)?,
        provider: row.get(2)?,
        account_alias: row.get(3)?,
        thread_id: row.get(4)?,
        event_type: MailAssistEventType::from_db_str(&event_type)?,
        actor: MailAssistActor::from_db_str(&actor)?,
        from_state: from_state
            .as_deref()
            .map(MailAnnotationState::from_db_str)
            .transpose()?,
        to_state: to_state
            .as_deref()
            .map(MailAnnotationState::from_db_str)
            .transpose()?,
        detail: detail_json
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .context("parsing mail assist event detail JSON")?,
        created_at: row.get(10)?,
        schema_version: row.get::<_, u32>(11)?,
    })
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::channel_assist::types::{
        ChannelInformationType, MailFeedbackVerdict, CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION,
        MAIL_ASSIST_SCHEMA_VERSION, REDACTED_SUBJECT_PLACEHOLDER,
    };

    #[tokio::test]
    async fn attention_recovery_valid_source_survives_same_producer_invalid_refresh() {
        use magician::magician_v2::attention::learning::{
            serialize_semantic_envelope, ChannelAttentionSemanticEnvelope,
            SemanticExtractorIdentity,
        };
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        let identity = SemanticExtractorIdentity {
            model: Some("luna".into()),
            profile: Some("remote".into()),
        };
        let valid = serde_json::json!({"communication_type":"direct_request", "requested_action":"reply", "action_owner":"owner",
            "direct_request_probability":0.8,"broadcast_probability":0.1,"personal_obligation_probability":0.8,
            "information_value_probability":0.5,"deadline":{"kind":"none","value":null},
            "campaign_or_event_identity":null,"evidence_refs":["summary"]});
        let good = serialize_semantic_envelope(
            &ChannelAttentionSemanticEnvelope::from_optional_value_for_source(
                Some(&valid),
                "distill:1",
                1,
                "1.1.0",
                &identity,
            ),
        )
        .unwrap();
        let bad =
            serialize_semantic_envelope(&ChannelAttentionSemanticEnvelope::from_optional_value(
                Some(&serde_json::json!({})),
                1,
                "1.1.0",
                &identity,
            ))
            .unwrap();
        let mut annotation = sample_annotation("ann-source", "acct-a", "thread-1");
        annotation.state = MailAnnotationState::NeedsApproval;
        annotation.classification_input_revision = Some(1);
        annotation.semantic_features = Some(good.clone());
        store
            .create_annotation("p", "w", annotation.clone(), MailAssistActor::Worker)
            .await
            .unwrap();
        assert!(store
            .update_semantic_features_if_revision("p", "w", &annotation.id, 1, &bad)
            .await
            .unwrap());
        assert_eq!(
            store
                .get_annotation("p", "w", &annotation.id)
                .await
                .unwrap()
                .unwrap()
                .semantic_features,
            Some(good.clone())
        );
        let mut refresh = annotation.clone();
        refresh.semantic_features = Some(bad.clone());
        preserve_successful_semantics_on_refresh(&annotation, &mut refresh);
        assert_eq!(refresh.semantic_features, Some(good));
        refresh.classification_input_revision = Some(2);
        refresh.semantic_features = Some(bad.clone());
        preserve_successful_semantics_on_refresh(&annotation, &mut refresh);
        assert_eq!(refresh.semantic_features, Some(bad.clone()));
        let mut other_producer = bad;
        other_producer["prompt_version"] = serde_json::json!("2.0.0");
        assert!(store
            .update_semantic_features_if_revision("p", "w", &annotation.id, 1, &other_producer)
            .await
            .unwrap());
        assert_eq!(
            store
                .get_annotation("p", "w", &annotation.id)
                .await
                .unwrap()
                .unwrap()
                .semantic_features,
            Some(other_producer)
        );
        assert!(!store
            .update_semantic_features_if_revision(
                "p",
                "w",
                &annotation.id,
                2,
                &serde_json::json!({})
            )
            .await
            .unwrap());
    }

    fn sample_brief(summary: &str) -> ChannelInformationBrief {
        ChannelInformationBrief {
            schema_version: CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION,
            information_type: ChannelInformationType::GeneralInformation,
            summary: summary.to_string(),
            key_facts: vec!["Plan: Standard".to_string()],
            changes: Vec::new(),
            temporal_facts: Vec::new(),
            stated_action: None,
            detail_status: ChannelDetailStatus::Complete,
            missing_details: Vec::new(),
        }
    }

    fn sample_thread(alias: &str, thread_id: &str, observed_at: i64) -> MailThreadRecord {
        MailThreadRecord {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            provider: "gmail".to_string(),
            account_alias: alias.to_string(),
            account_email: Some(format!("{alias}-owner@example.com")),
            thread_id: thread_id.to_string(),
            lane: ChannelLane::UserAssist,
            subject: Some(format!("subject-{thread_id}")),
            latest_summary: None,
            latest_from_name: Some("Sender One".to_string()),
            latest_from_address: Some("sender-one@example.com".to_string()),
            recipient_domains: vec!["example.org".to_string()],
            label_ids: vec!["INBOX".to_string()],
            message_count: 1,
            last_message_at: Some(observed_at - 10),
            provider_cursor: None,
            sensitive_suppressed: false,
            origin: MailRecordOrigin::MetadataSync,
            first_observed_at: observed_at,
            last_observed_at: observed_at,
        }
    }

    pub(super) fn sample_message(
        alias: &str,
        thread_id: &str,
        message_id: &str,
    ) -> MailMessageMeta {
        MailMessageMeta {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            provider: "gmail".to_string(),
            account_alias: alias.to_string(),
            account_email: Some(format!("{alias}-owner@example.com")),
            thread_id: thread_id.to_string(),
            message_id: message_id.to_string(),
            provider_cursor: None,
            label_ids: vec!["INBOX".to_string()],
            subject: Some(format!("subject-{thread_id}")),
            from_name: Some("Sender One".to_string()),
            from_address: Some("sender-one@example.com".to_string()),
            to_domains: vec!["example.org".to_string()],
            cc_domains: Vec::new(),
            internal_date: 1_000,
            observed_at: 1_100,
            direction: Some(MessageDirection::Inbound),
            summary: None,
            intent: None,
            needs_reply_hint: false,
            follow_up_hint: None,
            distill_brief: None,
            distill_contract_version: None,
            distilled_at: None,
            distill_revision: None,
            distill_state: DistillState::Pending,
            distill_attempts: 0,
            sensitive_suppressed: false,
            origin: MailRecordOrigin::MetadataSync,
        }
    }

    fn sample_annotation(id: &str, alias: &str, thread_id: &str) -> MailThreadAnnotation {
        MailThreadAnnotation {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            id: id.to_string(),
            provider: "gmail".to_string(),
            account_alias: alias.to_string(),
            thread_id: thread_id.to_string(),
            lane: ChannelLane::default(),
            state: MailAnnotationState::Observed,
            label: Some("follow_up".to_string()),
            confidence: Some(0.5),
            reason: Some("seeded fixture".to_string()),
            evidence_refs: vec![format!("msg-ref-{thread_id}")],
            evidence_message_id: None,
            evidence_message_at: None,
            classification_input_revision: None,
            semantic_features: None,
            proposed_action: None,
            provenance: None,
            created_at: 100,
            updated_at: 100,
        }
    }

    fn classified_annotation(
        id: &str,
        message_id: &str,
        message_at: i64,
        revision: i64,
        state: MailAnnotationState,
    ) -> MailThreadAnnotation {
        let mut annotation = sample_annotation(id, "acct-a", "thread-1");
        annotation.state = state;
        annotation.label = Some(
            if state == MailAnnotationState::Classified {
                "fyi"
            } else {
                "follow_up"
            }
            .to_string(),
        );
        annotation.evidence_refs = vec![
            "thread:thread-1".to_string(),
            format!("message:{message_id}"),
        ];
        annotation.evidence_message_id = Some(message_id.to_string());
        annotation.evidence_message_at = Some(message_at);
        annotation.classification_input_revision = Some(revision);
        annotation.proposed_action = (state == MailAnnotationState::NeedsApproval).then(|| {
            serde_json::json!({
                "follow_up_kind": "owner_owes",
                "distill_revision": revision,
            })
        });
        annotation.provenance = Some("test:classification".to_string());
        annotation
    }

    #[tokio::test]
    async fn channel_action_draft_round_trips_and_is_scoped() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        let draft = ChannelActionDraftRow {
            compose_id: "compose-1".to_string(),
            annotation_id: "anno-1".to_string(),
            action_id: "reply".to_string(),
            text: "Sounds good, see you then!".to_string(),
            created_at: 1_700,
        };
        store
            .put_channel_action_draft("alpha", "prod", draft.clone())
            .await
            .unwrap();

        let loaded = store
            .get_channel_action_draft("alpha", "prod", "compose-1")
            .await
            .unwrap();
        assert_eq!(loaded.as_ref(), Some(&draft));

        // Unknown id -> None.
        assert!(store
            .get_channel_action_draft("alpha", "prod", "missing")
            .await
            .unwrap()
            .is_none());
        // Wrong scope cannot read another scope's draft.
        assert!(store
            .get_channel_action_draft("beta", "prod", "compose-1")
            .await
            .unwrap()
            .is_none());

        // Re-compose with the same id replaces the text (idempotent).
        let mut edited = draft.clone();
        edited.text = "Actually, let's push to Monday.".to_string();
        store
            .put_channel_action_draft("alpha", "prod", edited.clone())
            .await
            .unwrap();
        let reloaded = store
            .get_channel_action_draft("alpha", "prod", "compose-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reloaded.text, "Actually, let's push to Monday.");
    }

    #[tokio::test]
    async fn upsert_thread_updates_state_and_preserves_first_observed_at() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        let initial = sample_thread("acct-a", "thread-1", 100);
        store
            .upsert_thread("alpha", "prod", initial.clone())
            .await
            .unwrap();

        let mut updated = initial.clone();
        updated.subject = Some("subject-thread-1-reply".to_string());
        updated.message_count = 3;
        updated.first_observed_at = 999; // must be ignored on update
        updated.last_observed_at = 200;
        store.upsert_thread("alpha", "prod", updated).await.unwrap();

        let threads = store
            .get_threads_by_ids(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                &["thread-1".to_string()],
            )
            .await
            .unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(
            threads[0].subject.as_deref(),
            Some("subject-thread-1-reply")
        );
        assert_eq!(threads[0].message_count, 3);
        assert_eq!(threads[0].first_observed_at, 100);
        assert_eq!(threads[0].last_observed_at, 200);
    }

    #[tokio::test]
    async fn upsert_thread_does_not_erase_a_known_account_email() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        let initial = sample_thread("acct-a", "thread-1", 100);
        store.upsert_thread("alpha", "prod", initial).await.unwrap();

        let mut incremental = sample_thread("acct-a", "thread-1", 200);
        incremental.account_email = None;
        store
            .upsert_thread("alpha", "prod", incremental)
            .await
            .unwrap();

        let threads = store
            .get_threads_by_ids(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                &["thread-1".to_string()],
            )
            .await
            .unwrap();
        assert_eq!(
            threads[0].account_email.as_deref(),
            Some("acct-a-owner@example.com")
        );
    }

    #[tokio::test]
    async fn account_email_reconciliation_repairs_threads_and_messages_scope_safely() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();

        let mut missing_thread = sample_thread("acct-a", "thread-1", 100);
        missing_thread.account_email = None;
        store
            .upsert_thread("alpha", "prod", missing_thread)
            .await
            .unwrap();
        let mut missing_message = sample_message("acct-a", "thread-1", "msg-1");
        missing_message.account_email = None;
        store
            .append_messages("alpha", "prod", vec![missing_message])
            .await
            .unwrap();

        let foreign_thread = sample_thread("acct-b", "thread-2", 100);
        store
            .upsert_thread("alpha", "prod", foreign_thread)
            .await
            .unwrap();
        let foreign_message = sample_message("acct-b", "thread-2", "msg-2");
        store
            .append_messages("alpha", "prod", vec![foreign_message])
            .await
            .unwrap();

        assert_eq!(
            store
                .reconcile_account_email("alpha", "prod", "gmail", "acct-a", " owner@example.com ",)
                .await
                .unwrap(),
            2
        );

        let repaired_thread = store
            .get_threads_by_ids(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                &["thread-1".to_string()],
            )
            .await
            .unwrap();
        let repaired_message = store
            .get_message("alpha", "prod", "gmail", "acct-a", "msg-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            repaired_thread[0].account_email.as_deref(),
            Some("owner@example.com")
        );
        assert_eq!(
            repaired_message.account_email.as_deref(),
            Some("owner@example.com")
        );

        let untouched = store
            .get_threads_by_ids(
                "alpha",
                "prod",
                "gmail",
                "acct-b",
                &["thread-2".to_string()],
            )
            .await
            .unwrap();
        assert_eq!(
            untouched[0].account_email.as_deref(),
            Some("acct-b-owner@example.com")
        );
        assert_eq!(
            store
                .reconcile_account_email("alpha", "prod", "gmail", "acct-a", "owner@example.com",)
                .await
                .unwrap(),
            0
        );
        assert!(store
            .reconcile_account_email("alpha", "prod", "gmail", "acct-a", "  ")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn append_messages_dedups_by_message_id_on_reappend() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        let first_batch = vec![
            sample_message("acct-a", "thread-1", "msg-1"),
            sample_message("acct-a", "thread-1", "msg-2"),
        ];
        let inserted = store
            .append_messages("alpha", "prod", first_batch.clone())
            .await
            .unwrap();
        assert_eq!(inserted, 2);

        // Re-append the same batch plus one new message: only the new row
        // lands — this is what makes sync re-runs incremental.
        let mut second_batch = first_batch;
        second_batch.push(sample_message("acct-a", "thread-1", "msg-3"));
        let inserted = store
            .append_messages("alpha", "prod", second_batch)
            .await
            .unwrap();
        assert_eq!(inserted, 1);

        let counts = store.count_summary("alpha", "prod").await.unwrap();
        assert_eq!(counts.len(), 1);
        assert_eq!(counts[0].account_alias, "acct-a");
        assert_eq!(counts[0].message_count, 3);
    }

    #[tokio::test]
    async fn information_brief_write_is_atomic_and_revision_follows_completion_order() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        let mut thread = sample_thread("acct-a", "thread-1", 2_100);
        thread.last_message_at = Some(2_000);
        store.upsert_thread("alpha", "prod", thread).await.unwrap();
        let mut older = sample_message("acct-a", "thread-1", "msg-old");
        older.internal_date = 1_000;
        let mut newer = sample_message("acct-a", "thread-1", "msg-new");
        newer.internal_date = 2_000;
        store
            .append_messages("alpha", "prod", vec![older, newer])
            .await
            .unwrap();

        let new_summary = "The Standard plan is available.";
        let revision_1 = store
            .set_distill_result_with_brief_and_evidence_ids(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                "msg-new",
                new_summary,
                "fyi",
                false,
                None,
                Some(&sample_brief(new_summary)),
                CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION,
                10_000,
                &["msg-new".to_string()],
            )
            .await
            .unwrap();
        let old_summary = "An older message was corrected later.";
        let revision_2 = store
            .set_distill_result_with_brief_and_evidence_ids(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                "msg-old",
                old_summary,
                "fyi",
                false,
                None,
                Some(&sample_brief(old_summary)),
                CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION,
                10_000,
                &["msg-old".to_string()],
            )
            .await
            .unwrap();
        assert_eq!(
            (revision_1, revision_2),
            (1, 2),
            "equal completion timestamps must still receive distinct ordered revisions"
        );

        let old_row = store
            .get_message("alpha", "prod", "gmail", "acct-a", "msg-old")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(old_row.summary.as_deref(), Some(old_summary));
        assert_eq!(old_row.distill_contract_version, Some(2));
        assert_eq!(old_row.distilled_at, Some(10_000));
        assert_eq!(old_row.distill_revision, Some(2));
        assert_eq!(old_row.distill_brief.unwrap(), sample_brief(old_summary));
        assert_eq!(old_row.distill_state, DistillState::Done);

        let failed = store
            .set_distill_result_with_brief_and_evidence_ids(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                "missing",
                "Missing row",
                "fyi",
                false,
                None,
                Some(&sample_brief("Missing row")),
                CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION,
                12_000,
                &[],
            )
            .await;
        assert!(failed.is_err());

        let revision_3 = store
            .set_distill_result_with_brief_and_evidence_ids(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                "msg-new",
                "The Standard plan details were refreshed.",
                "fyi",
                false,
                None,
                Some(&sample_brief("The Standard plan details were refreshed.")),
                CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION,
                13_000,
                &["msg-new".to_string()],
            )
            .await
            .unwrap();
        assert_eq!(
            revision_3, 3,
            "rolled-back failures must not consume revisions"
        );

        store
            .set_distill_state(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                "msg-new",
                DistillState::Suppressed,
            )
            .await
            .unwrap();
        let suppressed = store
            .get_message("alpha", "prod", "gmail", "acct-a", "msg-new")
            .await
            .unwrap()
            .unwrap();
        assert!(suppressed.distill_brief.is_none());
        assert_eq!(suppressed.distill_revision, None);
        let threads = store
            .get_threads_by_ids(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                &["thread-1".to_string()],
            )
            .await
            .unwrap();
        assert_eq!(
            threads[0].latest_summary.as_deref(),
            Some(old_summary),
            "suppression must remove the sensitive derived thread summary"
        );

        assert!(store
            .set_distill_result_with_brief_and_evidence_ids(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                "msg-new",
                "must not return",
                "fyi",
                false,
                None,
                Some(&sample_brief("must not return")),
                CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION,
                13_500,
                &["msg-new".to_string()],
            )
            .await
            .is_err());

        let revision_4 = store
            .set_distill_result_with_brief_and_evidence_ids(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                "msg-old",
                "The older row was refreshed again.",
                "fyi",
                false,
                None,
                Some(&sample_brief("The older row was refreshed again.")),
                CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION,
                14_000,
                &["msg-old".to_string()],
            )
            .await
            .unwrap();
        assert_eq!(
            revision_4, 4,
            "clearing the highest sensitive-row revision must not reuse it"
        );

        let mut sensitive_reappend = sample_message("acct-a", "thread-1", "msg-old");
        sensitive_reappend.subject = Some(REDACTED_SUBJECT_PLACEHOLDER.to_string());
        sensitive_reappend.sensitive_suppressed = true;
        sensitive_reappend.distill_state = DistillState::Suppressed;
        assert_eq!(
            store
                .append_messages("alpha", "prod", vec![sensitive_reappend])
                .await
                .unwrap(),
            0
        );
        let threads = store
            .get_threads_by_ids(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                &["thread-1".to_string()],
            )
            .await
            .unwrap();
        assert_eq!(threads[0].latest_summary, None);
    }

    #[tokio::test]
    async fn distilled_bridge_scan_is_strictly_newer_than_distill_revision() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        let mut boundary = sample_message("acct-a", "thread-1", "msg-boundary");
        boundary.internal_date = 1_000;
        let mut newer = sample_message("acct-a", "thread-1", "msg-newer");
        newer.internal_date = 2_000;
        store
            .append_messages("alpha", "prod", vec![boundary, newer])
            .await
            .unwrap();
        for message_id in ["msg-boundary", "msg-newer"] {
            store
                .set_distill_result(
                    "alpha",
                    "prod",
                    "gmail",
                    "acct-a",
                    message_id,
                    &format!("summary-{message_id}"),
                    "coordination",
                    false,
                    None,
                )
                .await
                .unwrap();
        }

        let rows = store
            .list_distilled_for_bridge("alpha", "prod", 1, 10)
            .await
            .unwrap();

        assert_eq!(
            rows.iter()
                .map(|row| row.message_id.as_str())
                .collect::<Vec<_>>(),
            vec!["msg-newer"]
        );
    }

    #[tokio::test]
    async fn malformed_bridge_brief_is_downgraded_without_stalling_later_revision() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        store
            .append_messages(
                "alpha",
                "prod",
                vec![
                    sample_message("acct-a", "thread-1", "msg-bad"),
                    sample_message("acct-a", "thread-1", "msg-good"),
                ],
            )
            .await
            .unwrap();
        for message_id in ["msg-bad", "msg-good"] {
            let summary = format!("summary-{message_id}");
            store
                .set_distill_result_with_brief_and_evidence_ids(
                    "alpha",
                    "prod",
                    "gmail",
                    "acct-a",
                    message_id,
                    &summary,
                    "fyi",
                    false,
                    None,
                    Some(&sample_brief(&summary)),
                    CHANNEL_INFORMATION_BRIEF_SCHEMA_VERSION,
                    10_000,
                    &[message_id.to_string()],
                )
                .await
                .unwrap();
        }
        {
            let inner = store.scope_inner("alpha", "prod").unwrap();
            let _guard = inner.acquire_write_guard().unwrap();
            let conn = inner.write_conn.lock().unwrap();
            conn.execute(
                "UPDATE mail_messages SET distill_brief_json = ? WHERE message_id = ?",
                params![r#"{"schema_version":"bad"}"#, "msg-bad"],
            )
            .unwrap();
        }

        let before = MailAssistStore::malformed_bridge_row_count();
        let rows = store
            .list_distilled_for_bridge("alpha", "prod", 0, 10)
            .await
            .unwrap();

        assert_eq!(rows.len(), 2);
        assert!(rows[0].distill_brief.is_none());
        assert!(rows[1].distill_brief.is_some());
        assert_eq!(rows[1].distill_revision, 2);
        assert!(MailAssistStore::malformed_bridge_row_count() > before);
    }

    #[test]
    fn post_commit_checkpoint_failure_does_not_fail_the_durable_mutation() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        let inner = store.scope_inner("alpha", "prod").unwrap();
        *inner.last_checkpoint_at.lock().unwrap() =
            Instant::now() - CHECKPOINT_THROTTLE - Duration::from_secs(1);
        let attempted = std::sync::atomic::AtomicBool::new(false);

        assert!(inner
            .maybe_checkpoint_with(|| {
                attempted.store(true, std::sync::atomic::Ordering::Relaxed);
                anyhow::bail!("deterministic busy checkpoint")
            })
            .is_ok());

        assert!(attempted.load(std::sync::atomic::Ordering::Relaxed));
        assert!(inner.last_checkpoint_at.lock().unwrap().elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn distill_backfill_runtime_state_survives_store_restart() {
        let tmp = TempDir::new().unwrap();
        {
            let store = MailAssistStore::open(tmp.path()).unwrap();
            store
                .request_distill_backfill("alpha", "prod")
                .await
                .unwrap();
            store
                .set_distill_backfill_paused("alpha", "prod", true)
                .await
                .unwrap();
            store
                .record_distill_backfill_yield("alpha", "prod", true)
                .await
                .unwrap();
            store
                .record_distill_backfill_run("alpha", "prod", None, 3, 2, 1, 42)
                .await
                .unwrap();
        }

        let reopened = MailAssistStore::open(tmp.path()).unwrap();
        let runtime = reopened
            .distill_backfill_runtime("alpha", "prod")
            .await
            .unwrap();
        assert!(runtime.paused);
        assert_eq!(runtime.manual_requests, 1);
        assert_eq!(runtime.runs, 1);
        assert_eq!(
            (runtime.selected, runtime.distilled, runtime.failed),
            (3, 2, 1)
        );
        assert_eq!(runtime.yielded_dispatch_pressure, 1);
        assert_eq!(runtime.last_run_at_ms, Some(42));
    }

    #[tokio::test]
    async fn distill_backfill_is_newest_first_bounded_and_non_destructive() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        let mut older = sample_message("acct-a", "thread-1", "msg-old");
        older.internal_date = 1_000;
        let mut newer = sample_message("acct-a", "thread-2", "msg-new");
        newer.internal_date = 2_000;
        store
            .append_messages("alpha", "prod", vec![older, newer])
            .await
            .unwrap();
        for message_id in ["msg-old", "msg-new"] {
            store
                .set_distill_result(
                    "alpha",
                    "prod",
                    "gmail",
                    "acct-a",
                    message_id,
                    &format!("legacy-{message_id}"),
                    "fyi",
                    false,
                    None,
                )
                .await
                .unwrap();
        }

        let counts = store
            .count_distill_backfill("alpha", "prod", 2, 0, 10_000)
            .await
            .unwrap();
        assert_eq!(
            counts,
            DistillBackfillCounts {
                total: 2,
                ready: 2,
                cooling: 0,
            }
        );
        assert_eq!(
            store.brief_coverage("alpha", "prod", 2).await.unwrap(),
            ChannelBriefCoverage {
                done: 2,
                v2: 0,
                legacy: 2,
                complete: 0,
                partial: 0,
                source_omits_details: 0,
            }
        );
        let rows = store
            .list_distill_backfill_candidates("alpha", "prod", 2, 0, 10_000, 1)
            .await
            .unwrap();
        assert_eq!(rows[0].message_id, "msg-new");
        let priority = store
            .list_distill_backfill_candidates_by_keys(
                "alpha",
                "prod",
                2,
                0,
                10_000,
                &[
                    (
                        "gmail".to_string(),
                        "acct-a".to_string(),
                        "msg-old".to_string(),
                    ),
                    (
                        "gmail".to_string(),
                        "acct-a".to_string(),
                        "msg-new".to_string(),
                    ),
                ],
                2,
            )
            .await
            .unwrap();
        assert_eq!(
            priority
                .iter()
                .map(|row| row.message_id.as_str())
                .collect::<Vec<_>>(),
            vec!["msg-old", "msg-new"],
            "exact surfaced priority order must win over provider timestamp"
        );

        store
            .record_distill_backfill_failure(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                "msg-new",
                Some(2),
                "temporary source error",
                10_000,
            )
            .await
            .unwrap();
        let still_done = store
            .get_message("alpha", "prod", "gmail", "acct-a", "msg-new")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(still_done.distill_state, DistillState::Done);
        assert_eq!(still_done.summary.as_deref(), Some("legacy-msg-new"));
        let counts = store
            .count_distill_backfill("alpha", "prod", 2, 0, 10_001)
            .await
            .unwrap();
        assert_eq!((counts.total, counts.ready, counts.cooling), (2, 1, 1));
        let rows = store
            .list_distill_backfill_candidates("alpha", "prod", 2, 0, 10_001, 2)
            .await
            .unwrap();
        assert_eq!(rows[0].message_id, "msg-old");

        store
            .set_distill_state(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                "msg-old",
                DistillState::Suppressed,
            )
            .await
            .unwrap();
        let summary = "The monthly limit changed from 5 to 10.";
        store
            .set_distill_result_with_brief_and_evidence_ids(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                "msg-new",
                summary,
                "fyi",
                false,
                None,
                Some(&sample_brief(summary)),
                2,
                20_000,
                &["msg-new".to_string()],
            )
            .await
            .unwrap();
        assert!(store
            .record_distill_backfill_failure(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                "msg-new",
                Some(2),
                "late stale failure",
                20_001,
            )
            .await
            .is_err());
        assert_eq!(
            store
                .count_distill_backfill("alpha", "prod", 2, 0, 20_001)
                .await
                .unwrap(),
            DistillBackfillCounts::default()
        );
        assert_eq!(
            store.brief_coverage("alpha", "prod", 2).await.unwrap(),
            ChannelBriefCoverage {
                done: 1,
                v2: 1,
                legacy: 0,
                complete: 1,
                partial: 0,
                source_omits_details: 0,
            }
        );
    }

    #[tokio::test]
    async fn annotation_transitions_accumulate_audit_events_without_rewrites() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        store
            .create_annotation(
                "alpha",
                "prod",
                sample_annotation("ann-1", "acct-a", "thread-1"),
                MailAssistActor::Worker,
            )
            .await
            .unwrap();

        store
            .transition_annotation(
                "alpha",
                "prod",
                "ann-1",
                MailAnnotationState::Classified,
                MailAssistActor::Worker,
                None,
                200,
            )
            .await
            .unwrap();
        let events_after_first = store
            .list_events_for_annotation("alpha", "prod", "ann-1")
            .await
            .unwrap();
        assert_eq!(events_after_first.len(), 2);

        let updated = store
            .transition_annotation(
                "alpha",
                "prod",
                "ann-1",
                MailAnnotationState::Dismissed,
                MailAssistActor::User,
                None,
                300,
            )
            .await
            .unwrap();
        assert_eq!(updated.state, MailAnnotationState::Dismissed);
        assert_eq!(updated.updated_at, 300);

        // Audit grows and earlier rows are byte-identical — never
        // rewritten or deleted.
        let events = store
            .list_events_for_annotation("alpha", "prod", "ann-1")
            .await
            .unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0..2], events_after_first[0..2]);
        assert_eq!(events[0].event_type, MailAssistEventType::AnnotationCreated);
        assert_eq!(events[0].to_state, Some(MailAnnotationState::Observed));
        assert_eq!(events[1].event_type, MailAssistEventType::StateTransition);
        assert_eq!(events[1].from_state, Some(MailAnnotationState::Observed));
        assert_eq!(events[1].to_state, Some(MailAnnotationState::Classified));
        assert_eq!(events[2].event_type, MailAssistEventType::Dismissed);
        assert_eq!(events[2].from_state, Some(MailAnnotationState::Classified));
        assert_eq!(events[2].to_state, Some(MailAnnotationState::Dismissed));
        assert_eq!(events[2].actor, MailAssistActor::User);

        // The annotation row still exists — dismissal is a transition,
        // not a delete.
        let annotation = store
            .get_annotation("alpha", "prod", "ann-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(annotation.state, MailAnnotationState::Dismissed);
    }

    #[tokio::test]
    async fn feedback_appends_typed_audit_event() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        store
            .create_annotation(
                "alpha",
                "prod",
                sample_annotation("ann-1", "acct-a", "thread-1"),
                MailAssistActor::Worker,
            )
            .await
            .unwrap();

        let feedback = MailAssistUserFeedback {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            id: "fb-1".to_string(),
            annotation_id: "ann-1".to_string(),
            provider: "gmail".to_string(),
            account_alias: "acct-a".to_string(),
            thread_id: Some("thread-1".to_string()),
            verdict: MailFeedbackVerdict::WrongLabel,
            comment: Some("synthetic feedback comment".to_string()),
            actor: MailAssistActor::User,
            created_at: 400,
        };
        store
            .append_feedback("alpha", "prod", feedback.clone())
            .await
            .unwrap();

        let events = store
            .list_events_for_annotation("alpha", "prod", "ann-1")
            .await
            .unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].event_type, MailAssistEventType::Feedback);
        let detail = events[1].detail.clone().unwrap();
        let roundtrip: MailAssistUserFeedback = serde_json::from_value(detail).unwrap();
        assert_eq!(roundtrip, feedback);
    }

    #[tokio::test]
    async fn batch_gets_skip_missing_thread_ids() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        store
            .upsert_thread("alpha", "prod", sample_thread("acct-a", "thread-1", 100))
            .await
            .unwrap();
        store
            .upsert_thread("alpha", "prod", sample_thread("acct-a", "thread-2", 110))
            .await
            .unwrap();
        store
            .create_annotation(
                "alpha",
                "prod",
                sample_annotation("ann-1", "acct-a", "thread-1"),
                MailAssistActor::Worker,
            )
            .await
            .unwrap();
        store
            .create_annotation(
                "alpha",
                "prod",
                sample_annotation("ann-2", "acct-a", "thread-2"),
                MailAssistActor::Worker,
            )
            .await
            .unwrap();

        let requested = vec![
            "thread-1".to_string(),
            "thread-2".to_string(),
            "thread-missing".to_string(),
        ];
        let threads = store
            .get_threads_by_ids("alpha", "prod", "gmail", "acct-a", &requested)
            .await
            .unwrap();
        assert_eq!(threads.len(), 2);

        let annotations = store
            .list_annotations_by_thread_ids("alpha", "prod", "gmail", "acct-a", &requested)
            .await
            .unwrap();
        assert_eq!(annotations.len(), 2);
        // A fully-unknown batch yields an empty (valid) answer.
        let none = store
            .list_annotations_by_thread_ids(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                &["thread-missing".to_string()],
            )
            .await
            .unwrap();
        assert!(none.is_empty());
    }

    #[tokio::test]
    async fn list_recent_threads_orders_newest_first_and_filters_by_account() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        let mut older = sample_thread("acct-a", "thread-old", 100);
        older.last_message_at = Some(1_000);
        let mut newer = sample_thread("acct-a", "thread-new", 110);
        newer.last_message_at = Some(2_000);
        let mut dateless = sample_thread("acct-a", "thread-dateless", 120);
        dateless.last_message_at = None;
        let other_account = sample_thread("acct-b", "thread-b", 130);
        for record in [older, newer, dateless, other_account] {
            store.upsert_thread("alpha", "prod", record).await.unwrap();
        }

        let all = store
            .list_recent_threads("alpha", "prod", "gmail", None, 10)
            .await
            .unwrap();
        let ids: Vec<&str> = all.iter().map(|t| t.thread_id.as_str()).collect();
        // Newest first (thread-b's sample last_message_at is 120, the
        // oldest dated row); NULL last_message_at sorts last.
        assert_eq!(
            ids,
            vec!["thread-new", "thread-old", "thread-b", "thread-dateless"]
        );

        let filtered = store
            .list_recent_threads("alpha", "prod", "gmail", Some("acct-a"), 10)
            .await
            .unwrap();
        assert!(filtered.iter().all(|t| t.account_alias == "acct-a"));
        assert_eq!(filtered.len(), 3);

        let limited = store
            .list_recent_threads("alpha", "prod", "gmail", None, 2)
            .await
            .unwrap();
        assert_eq!(limited.len(), 2);
        assert_eq!(limited[0].thread_id, "thread-new");
    }

    #[tokio::test]
    async fn watermark_roundtrips_per_account() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        assert!(store
            .get_watermark("alpha", "prod", "gmail", "acct-a")
            .await
            .unwrap()
            .is_none());

        let watermark = SyncWatermark {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            provider: "gmail".to_string(),
            account_alias: "acct-a".to_string(),
            last_internal_date: Some(1_000),
            provider_cursor: Some("hist-1".to_string()),
            last_synced_at: 1_100,
            last_error: None,
        };
        store
            .set_watermark("alpha", "prod", watermark.clone())
            .await
            .unwrap();
        let fetched = store
            .get_watermark("alpha", "prod", "gmail", "acct-a")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fetched, watermark);

        // Advance + record an error; the upsert replaces cursor fields.
        let mut advanced = watermark;
        advanced.last_internal_date = Some(2_000);
        advanced.provider_cursor = None;
        advanced.last_error = Some("synthetic sync error".to_string());
        advanced.last_synced_at = 2_100;
        store
            .set_watermark("alpha", "prod", advanced.clone())
            .await
            .unwrap();
        let fetched = store
            .get_watermark("alpha", "prod", "gmail", "acct-a")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fetched, advanced);
        // Other accounts are unaffected.
        assert!(store
            .get_watermark("alpha", "prod", "gmail", "acct-b")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn sensitive_thread_row_stores_redacted_subject_with_ids_retained() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        let mut sensitive = sample_thread("acct-a", "thread-otp", 100);
        sensitive.sensitive_suppressed = true;
        sensitive.subject = Some(REDACTED_SUBJECT_PLACEHOLDER.to_string());
        store
            .upsert_thread("alpha", "prod", sensitive)
            .await
            .unwrap();

        let threads = store
            .get_threads_by_ids(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                &["thread-otp".to_string()],
            )
            .await
            .unwrap();
        assert_eq!(threads.len(), 1);
        assert!(threads[0].sensitive_suppressed);
        assert_eq!(
            threads[0].subject.as_deref(),
            Some(REDACTED_SUBJECT_PLACEHOLDER)
        );
        // Ids and sender domain survive redaction so reconciliation works.
        assert_eq!(threads[0].thread_id, "thread-otp");
        assert_eq!(
            threads[0].latest_from_address.as_deref(),
            Some("sender-one@example.com")
        );
    }

    #[tokio::test]
    async fn pending_distill_queue_is_newest_first_capped_and_skips_non_pending() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        let mut newest = sample_message("acct-a", "thread-1", "msg-new");
        newest.internal_date = 3_000;
        let mut middle = sample_message("acct-a", "thread-1", "msg-mid");
        middle.internal_date = 2_000;
        let mut oldest = sample_message("acct-a", "thread-1", "msg-old");
        oldest.internal_date = 1_000;
        // Suppressed rows are appended with distill_state=suppressed
        // directly (the appender-side short-circuit) and never queue.
        let mut suppressed = sample_message("acct-a", "thread-1", "msg-otp");
        suppressed.internal_date = 500;
        suppressed.sensitive_suppressed = true;
        suppressed.distill_state = DistillState::Suppressed;
        store
            .append_messages("alpha", "prod", vec![newest, middle, oldest, suppressed])
            .await
            .unwrap();

        let queue = store
            .list_pending_distill("alpha", "prod", 10)
            .await
            .unwrap();
        let ids: Vec<&str> = queue.iter().map(|m| m.message_id.as_str()).collect();
        assert_eq!(ids, vec!["msg-new", "msg-mid", "msg-old"]);

        let capped = store
            .list_pending_distill("alpha", "prod", 2)
            .await
            .unwrap();
        assert_eq!(capped.len(), 2);
        assert_eq!(capped[0].message_id, "msg-new");

        // The suppressed row exists with its short-circuit state intact.
        let row = store
            .get_message("alpha", "prod", "gmail", "acct-a", "msg-otp")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.distill_state, DistillState::Suppressed);
        assert!(row.summary.is_none());
    }

    #[tokio::test]
    async fn distill_result_roundtrips_and_rolls_thread_latest_summary() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        store
            .upsert_thread("alpha", "prod", sample_thread("acct-a", "thread-1", 100))
            .await
            .unwrap();
        let mut older = sample_message("acct-a", "thread-1", "msg-old");
        older.internal_date = 1_000;
        let mut newer = sample_message("acct-a", "thread-1", "msg-new");
        newer.internal_date = 2_000;
        store
            .append_messages("alpha", "prod", vec![older, newer])
            .await
            .unwrap();

        // Oldest-first drain: the older message distills first and rolls
        // the thread summary (it is the newest DISTILLED message so far).
        store
            .set_distill_result(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                "msg-old",
                "synthetic older summary",
                "synthetic-intent-a",
                false,
                None,
            )
            .await
            .unwrap();
        let row = store
            .get_message("alpha", "prod", "gmail", "acct-a", "msg-old")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.distill_state, DistillState::Done);
        assert_eq!(row.summary.as_deref(), Some("synthetic older summary"));
        assert_eq!(row.intent.as_deref(), Some("synthetic-intent-a"));
        let threads = store
            .get_threads_by_ids(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                &["thread-1".to_string()],
            )
            .await
            .unwrap();
        assert_eq!(
            threads[0].latest_summary.as_deref(),
            Some("synthetic older summary")
        );

        // The newer message rolls the thread summary forward…
        store
            .set_distill_result(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                "msg-new",
                "synthetic newer summary",
                "synthetic-intent-b",
                true,
                None,
            )
            .await
            .unwrap();
        let threads = store
            .get_threads_by_ids(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                &["thread-1".to_string()],
            )
            .await
            .unwrap();
        assert_eq!(
            threads[0].latest_summary.as_deref(),
            Some("synthetic newer summary")
        );

        // …and an out-of-order re-distill of the OLDER message must not
        // regress it.
        store
            .set_distill_result(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                "msg-old",
                "synthetic older rewrite",
                "synthetic-intent-a",
                false,
                None,
            )
            .await
            .unwrap();
        let threads = store
            .get_threads_by_ids(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                &["thread-1".to_string()],
            )
            .await
            .unwrap();
        assert_eq!(
            threads[0].latest_summary.as_deref(),
            Some("synthetic newer summary")
        );

        // Distilled rows leave the pending queue; a sync re-upsert of the
        // thread (which carries no summary) must not clobber the rolling
        // summary.
        assert!(store
            .list_pending_distill("alpha", "prod", 10)
            .await
            .unwrap()
            .is_empty());
        store
            .upsert_thread("alpha", "prod", sample_thread("acct-a", "thread-1", 300))
            .await
            .unwrap();
        let threads = store
            .get_threads_by_ids(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                &["thread-1".to_string()],
            )
            .await
            .unwrap();
        assert_eq!(
            threads[0].latest_summary.as_deref(),
            Some("synthetic newer summary")
        );

        // Unknown messages are an error, not a silent no-op.
        assert!(store
            .set_distill_result(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                "msg-missing",
                "s",
                "i",
                false,
                None,
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn classify_queue_reopens_for_newer_distilled_message_evidence() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        store
            .upsert_thread("alpha", "prod", sample_thread("acct-a", "thread-1", 100))
            .await
            .unwrap();

        let mut older = sample_message("acct-a", "thread-1", "msg-old");
        older.internal_date = 1_000;
        let mut newer = sample_message("acct-a", "thread-1", "msg-new");
        newer.internal_date = 2_000;
        newer.direction = Some(MessageDirection::Outbound);
        store
            .append_messages("alpha", "prod", vec![older, newer])
            .await
            .unwrap();

        let hint = ChannelFollowUpHint {
            kind: "needs_reply".to_string(),
            actor: Some("owner".to_string()),
            counterparty: Some("sender-one@example.com".to_string()),
            due_text: Some("tomorrow".to_string()),
            urgency: Some("high".to_string()),
            rationale: Some("sender asked for confirmation".to_string()),
            key_details: vec!["Due tomorrow".to_string()],
        };
        store
            .set_distill_result(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                "msg-old",
                "older summary",
                "question",
                true,
                Some(&hint),
            )
            .await
            .unwrap();

        let rows = store
            .list_threads_to_classify("alpha", "prod", 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].thread_id, "thread-1");
        assert_eq!(rows[0].latest_message_id, "msg-old");
        assert_eq!(rows[0].latest_message_at, 1_000);
        assert_eq!(rows[0].latest_intent.as_deref(), Some("question"));
        assert!(rows[0].needs_reply_hint);
        assert_eq!(
            rows[0]
                .follow_up_hint
                .as_ref()
                .map(|hint| hint.kind.as_str()),
            Some("needs_reply")
        );
        let old_revision = rows[0].distill_revision;

        let mut annotation = sample_annotation("ann-old", "acct-a", "thread-1");
        annotation.state = MailAnnotationState::Dismissed;
        annotation.evidence_message_id = Some("msg-old".to_string());
        annotation.evidence_message_at = Some(1_000);
        annotation.classification_input_revision = Some(old_revision);
        store
            .create_annotation("alpha", "prod", annotation, MailAssistActor::Worker)
            .await
            .unwrap();
        assert!(store
            .list_threads_to_classify("alpha", "prod", 10)
            .await
            .unwrap()
            .is_empty());

        store
            .set_distill_result(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                "msg-new",
                "newer summary",
                "owner-promised",
                false,
                None,
            )
            .await
            .unwrap();

        let rows = store
            .list_threads_to_classify("alpha", "prod", 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].latest_message_id, "msg-new");
        assert_eq!(rows[0].latest_message_at, 2_000);
        assert_eq!(rows[0].latest_direction.as_deref(), Some("outbound"));
        assert_eq!(rows[0].latest_summary.as_deref(), Some("newer summary"));
    }

    #[tokio::test]
    async fn redistilled_exact_message_reclassifies_in_place_and_is_restart_idempotent() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        store
            .upsert_thread("alpha", "prod", sample_thread("acct-a", "thread-1", 100))
            .await
            .unwrap();
        store
            .append_messages(
                "alpha",
                "prod",
                vec![sample_message("acct-a", "thread-1", "msg-1")],
            )
            .await
            .unwrap();
        store
            .set_distill_result(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                "msg-1",
                "Informational summary",
                "fyi",
                false,
                None,
            )
            .await
            .unwrap();
        let first_revision = store
            .get_message("alpha", "prod", "gmail", "acct-a", "msg-1")
            .await
            .unwrap()
            .unwrap()
            .distill_revision
            .unwrap();
        let first = classified_annotation(
            "ann-1",
            "msg-1",
            1_000,
            first_revision,
            MailAnnotationState::Classified,
        );
        let created = store
            .apply_classification_annotation("alpha", "prod", first)
            .await
            .unwrap();
        assert!(matches!(
            created,
            ClassificationAnnotationApplyResult::Applied {
                disposition: ClassificationAnnotationDisposition::Created,
                ..
            }
        ));

        store
            .set_distill_result(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                "msg-1",
                "Invoice is due July 15, 2026",
                "action_request",
                false,
                None,
            )
            .await
            .unwrap();
        let second_revision = store
            .get_message("alpha", "prod", "gmail", "acct-a", "msg-1")
            .await
            .unwrap()
            .unwrap()
            .distill_revision
            .unwrap();
        assert!(second_revision > first_revision);
        let rows = store
            .list_threads_to_classify("alpha", "prod", 10)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].distill_revision, second_revision);

        let mut corrected = classified_annotation(
            "new-id-is-ignored",
            "msg-1",
            1_000,
            second_revision,
            MailAnnotationState::NeedsApproval,
        );
        corrected.reason = Some("Invoice payment is due".to_string());
        let refreshed = store
            .apply_classification_annotation("alpha", "prod", corrected.clone())
            .await
            .unwrap();
        let stored = match refreshed {
            ClassificationAnnotationApplyResult::Applied {
                annotation,
                disposition: ClassificationAnnotationDisposition::Reclassified,
            } => annotation,
            other => panic!("unexpected reclassification result: {other:?}"),
        };
        assert_eq!(stored.id, "ann-1");
        assert_eq!(stored.state, MailAnnotationState::NeedsApproval);
        assert_eq!(stored.classification_input_revision, Some(second_revision));
        assert_eq!(
            store
                .list_annotations_by_thread_ids(
                    "alpha",
                    "prod",
                    "gmail",
                    "acct-a",
                    &["thread-1".to_string()],
                )
                .await
                .unwrap()
                .len(),
            1
        );

        assert!(matches!(
            store
                .apply_classification_annotation("alpha", "prod", corrected)
                .await
                .unwrap(),
            ClassificationAnnotationApplyResult::StaleInput { .. }
        ));
    }

    #[tokio::test]
    async fn revised_action_refreshes_active_card_but_preserves_dismissal() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        store
            .upsert_thread("alpha", "prod", sample_thread("acct-a", "thread-1", 100))
            .await
            .unwrap();
        let mut first_message = sample_message("acct-a", "thread-1", "msg-1");
        first_message.internal_date = 1_000;
        let mut second_message = sample_message("acct-a", "thread-1", "msg-2");
        second_message.internal_date = 2_000;
        store
            .append_messages("alpha", "prod", vec![first_message, second_message])
            .await
            .unwrap();
        for (message_id, summary) in [("msg-1", "First due item"), ("msg-2", "Updated due item")] {
            store
                .set_distill_result(
                    "alpha",
                    "prod",
                    "gmail",
                    "acct-a",
                    message_id,
                    summary,
                    "action_request",
                    false,
                    None,
                )
                .await
                .unwrap();
        }
        let first_revision = store
            .get_message("alpha", "prod", "gmail", "acct-a", "msg-1")
            .await
            .unwrap()
            .unwrap()
            .distill_revision
            .unwrap();
        let second_revision = store
            .get_message("alpha", "prod", "gmail", "acct-a", "msg-2")
            .await
            .unwrap()
            .unwrap()
            .distill_revision
            .unwrap();
        let active = classified_annotation(
            "ann-active",
            "msg-1",
            1_000,
            first_revision,
            MailAnnotationState::NeedsApproval,
        );
        store
            .apply_classification_annotation("alpha", "prod", active)
            .await
            .unwrap();
        let newer = classified_annotation(
            "ann-new",
            "msg-2",
            2_000,
            second_revision,
            MailAnnotationState::NeedsApproval,
        );
        let refreshed = store
            .apply_classification_annotation("alpha", "prod", newer)
            .await
            .unwrap();
        let refreshed = match refreshed {
            ClassificationAnnotationApplyResult::Applied {
                annotation,
                disposition: ClassificationAnnotationDisposition::RefreshedNeedsApproval,
            } => annotation,
            other => panic!("unexpected active refresh result: {other:?}"),
        };
        assert_eq!(refreshed.id, "ann-active");
        assert_eq!(refreshed.evidence_message_id.as_deref(), Some("msg-2"));

        store
            .transition_annotation(
                "alpha",
                "prod",
                "ann-active",
                MailAnnotationState::Dismissed,
                MailAssistActor::User,
                None,
                3_000,
            )
            .await
            .unwrap();
        store
            .set_distill_result(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                "msg-2",
                "Corrected updated due item",
                "action_request",
                false,
                None,
            )
            .await
            .unwrap();
        let revised = store
            .get_message("alpha", "prod", "gmail", "acct-a", "msg-2")
            .await
            .unwrap()
            .unwrap()
            .distill_revision
            .unwrap();
        let attempted = classified_annotation(
            "ann-ignored",
            "msg-2",
            2_000,
            revised,
            MailAnnotationState::NeedsApproval,
        );
        let preserved = store
            .apply_classification_annotation("alpha", "prod", attempted)
            .await
            .unwrap();
        assert!(matches!(
            preserved,
            ClassificationAnnotationApplyResult::Applied {
                annotation: MailThreadAnnotation {
                    state: MailAnnotationState::Dismissed,
                    ..
                },
                disposition: ClassificationAnnotationDisposition::PreservedDismissal,
            }
        ));
    }

    #[tokio::test]
    async fn required_action_materialization_is_provisional_and_duplicate_free() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        store
            .upsert_thread("alpha", "prod", sample_thread("acct-a", "thread-1", 100))
            .await
            .unwrap();
        store
            .append_messages(
                "alpha",
                "prod",
                vec![sample_message("acct-a", "thread-1", "msg-1")],
            )
            .await
            .unwrap();
        store
            .set_distill_result(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                "msg-1",
                "Invoice is due July 15, 2026",
                "action_request",
                false,
                None,
            )
            .await
            .unwrap();
        let revision = store
            .get_message("alpha", "prod", "gmail", "acct-a", "msg-1")
            .await
            .unwrap()
            .unwrap()
            .distill_revision
            .unwrap();
        let mut provisional = classified_annotation(
            "ann-provisional",
            "msg-1",
            1_000,
            revision,
            MailAnnotationState::NeedsApproval,
        );
        provisional.classification_input_revision = None;
        provisional.provenance = Some("resurfacing_required_action:v1".to_string());

        for expected in [
            RequiredActionAnnotationDisposition::Created,
            RequiredActionAnnotationDisposition::RefreshedNeedsApproval,
        ] {
            let result = store
                .ensure_required_action_annotation("alpha", "prod", provisional.clone(), revision)
                .await
                .unwrap();
            assert!(matches!(
                result,
                RequiredActionAnnotationResult::Applied { disposition, .. }
                    if disposition == expected
            ));
        }
        let annotations = store
            .list_annotations_by_thread_ids(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                &["thread-1".to_string()],
            )
            .await
            .unwrap();
        assert_eq!(annotations.len(), 1);
        assert_eq!(annotations[0].classification_input_revision, None);
        assert_eq!(
            store
                .list_threads_to_classify("alpha", "prod", 10)
                .await
                .unwrap()
                .len(),
            1,
            "provisional action must remain queued for classifier enrichment"
        );

        let mut repeated = classified_annotation(
            "classifier-id-is-ignored",
            "msg-1",
            1_000,
            revision,
            MailAnnotationState::Classified,
        );
        repeated.proposed_action = Some(serde_json::json!({
            "repeat_of_recently_handled": true,
            "attention_drop_reason": "cooldown_active",
            "distill_revision": revision,
        }));
        let demoted = store
            .apply_classification_annotation("alpha", "prod", repeated)
            .await
            .unwrap();
        assert!(matches!(
            demoted,
            ClassificationAnnotationApplyResult::Applied {
                annotation: MailThreadAnnotation {
                    state: MailAnnotationState::Classified,
                    ..
                },
                disposition: ClassificationAnnotationDisposition::Reclassified,
            }
        ));
    }

    #[tokio::test]
    async fn recent_handled_follow_ups_returns_recent_thread_context() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        store
            .upsert_thread("alpha", "prod", sample_thread("acct-a", "thread-1", 10_000))
            .await
            .unwrap();

        let mut annotation = sample_annotation("ann-ack", "acct-a", "thread-1");
        annotation.state = MailAnnotationState::Acknowledged;
        annotation.label = Some("needs_reply".to_string());
        annotation.proposed_action = Some(serde_json::json!({
            "follow_up_kind": "needs_reply",
            "action_owner": "owner"
        }));
        annotation.updated_at = 10_000;
        store
            .create_annotation("alpha", "prod", annotation, MailAssistActor::User)
            .await
            .unwrap();

        let recent = store
            .recent_handled_follow_ups("alpha", "prod", "gmail", "acct-a", "thread-1", 9_000, 3)
            .await
            .unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].state, MailAnnotationState::Acknowledged);
        assert_eq!(recent[0].label.as_deref(), Some("needs_reply"));
        assert_eq!(
            recent[0].proposed_action.as_ref().unwrap()["follow_up_kind"],
            "needs_reply"
        );
        assert!(store
            .recent_handled_follow_ups("alpha", "prod", "gmail", "acct-a", "thread-1", 11_000, 3,)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn set_distill_state_counts_failures_only() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        store
            .append_messages(
                "alpha",
                "prod",
                vec![sample_message("acct-a", "thread-1", "msg-1")],
            )
            .await
            .unwrap();

        for _ in 0..2 {
            store
                .set_distill_state(
                    "alpha",
                    "prod",
                    "gmail",
                    "acct-a",
                    "msg-1",
                    DistillState::Failed,
                )
                .await
                .unwrap();
        }
        let row = store
            .get_message("alpha", "prod", "gmail", "acct-a", "msg-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.distill_state, DistillState::Failed);
        assert_eq!(row.distill_attempts, 2);

        // Non-failure transitions never touch the counter.
        store
            .set_distill_state(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                "msg-1",
                DistillState::Skipped,
            )
            .await
            .unwrap();
        let row = store
            .get_message("alpha", "prod", "gmail", "acct-a", "msg-1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.distill_state, DistillState::Skipped);
        assert_eq!(row.distill_attempts, 2);

        assert!(store
            .set_distill_state(
                "alpha",
                "prod",
                "gmail",
                "acct-a",
                "msg-missing",
                DistillState::Failed,
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn retryable_distill_listing_is_cap_aware_and_newest_first() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        let mut newer_failed = sample_message("acct-a", "thread-1", "msg-fail-new");
        newer_failed.internal_date = 2_000;
        let mut older_failed = sample_message("acct-a", "thread-1", "msg-fail-old");
        older_failed.internal_date = 1_000;
        let pending = sample_message("acct-a", "thread-1", "msg-pending");
        store
            .append_messages("alpha", "prod", vec![newer_failed, older_failed, pending])
            .await
            .unwrap();
        for id in ["msg-fail-new", "msg-fail-old"] {
            store
                .set_distill_state("alpha", "prod", "gmail", "acct-a", id, DistillState::Failed)
                .await
                .unwrap();
        }

        // Newest first; pending rows never appear on the retry pass.
        let retryable = store
            .list_retryable_distill("alpha", "prod", 3, 10)
            .await
            .unwrap();
        let ids: Vec<&str> = retryable.iter().map(|m| m.message_id.as_str()).collect();
        assert_eq!(ids, vec!["msg-fail-new", "msg-fail-old"]);

        // Rows at (or past) the attempt cap drop out.
        for _ in 0..2 {
            store
                .set_distill_state(
                    "alpha",
                    "prod",
                    "gmail",
                    "acct-a",
                    "msg-fail-old",
                    DistillState::Failed,
                )
                .await
                .unwrap();
        }
        let retryable = store
            .list_retryable_distill("alpha", "prod", 3, 10)
            .await
            .unwrap();
        let ids: Vec<&str> = retryable.iter().map(|m| m.message_id.as_str()).collect();
        assert_eq!(ids, vec!["msg-fail-new"]);
        // Cap 1 excludes every failed row (all carry >= 1 attempt).
        assert!(store
            .list_retryable_distill("alpha", "prod", 1, 10)
            .await
            .unwrap()
            .is_empty());
    }

    #[test]
    fn schema_version_guard_records_fresh_migrates_v2_and_rejects_mismatches() {
        // Fresh bootstrap: no version row -> current version recorded.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(BOOTSTRAP_DDL).unwrap();
        apply_schema_migrations(&conn).unwrap();
        let stored: String = conn
            .prepare("SELECT meta_value FROM mail_assist_meta WHERE meta_key = 'schema_version'")
            .unwrap()
            .query_row([], |row| row.get(0))
            .unwrap();
        assert_eq!(stored, MAIL_ASSIST_DB_SCHEMA_VERSION.to_string());
        // Idempotent at the current version.
        apply_schema_migrations(&conn).unwrap();

        // v2 stores migrate additively and record the current version.
        conn.execute(
            "UPDATE mail_assist_meta SET meta_value = '2' WHERE meta_key = 'schema_version'",
            [],
        )
        .unwrap();
        apply_schema_migrations(&conn).unwrap();
        let stored: String = conn
            .prepare("SELECT meta_value FROM mail_assist_meta WHERE meta_key = 'schema_version'")
            .unwrap()
            .query_row([], |row| row.get(0))
            .unwrap();
        assert_eq!(stored, MAIL_ASSIST_DB_SCHEMA_VERSION.to_string());

        // A lower stored version is UNSUPPORTED, loudly — no silent
        // continue, no pretend-migration (no v1 DB was ever deployed).
        conn.execute(
            "UPDATE mail_assist_meta SET meta_value = '1' WHERE meta_key = 'schema_version'",
            [],
        )
        .unwrap();
        let error = apply_schema_migrations(&conn).unwrap_err().to_string();
        assert!(error.contains("unsupported"), "unexpected error: {error}");

        // A higher stored version (newer deployment's DB) fails too.
        conn.execute(
            "UPDATE mail_assist_meta SET meta_value = '99' WHERE meta_key = 'schema_version'",
            [],
        )
        .unwrap();
        assert!(apply_schema_migrations(&conn).is_err());
    }

    #[test]
    fn v6_schema_migrates_information_brief_columns_without_constraints() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            // Keep the fixture minimal, but include the baseline queue columns
            // that every valid v6 store already had. Later migrations create
            // indexes over these columns after adding the v7-v9 fields.
            "CREATE TABLE mail_messages (
                 principal TEXT,
                 workspace TEXT,
                 provider TEXT,
                 account_alias TEXT,
                 thread_id TEXT,
                 message_id TEXT,
                 distill_state TEXT,
                 sensitive_suppressed BOOLEAN,
                 internal_date BIGINT
             );
             CREATE TABLE mail_annotations (
                 principal TEXT,
                 workspace TEXT,
                 provider TEXT,
                 account_alias TEXT,
                 thread_id TEXT,
                 id TEXT,
                 state TEXT,
                 proposed_action_json JSON,
                 created_at BIGINT,
                 updated_at BIGINT
             );
             CREATE TABLE mail_assist_events (
                 principal TEXT,
                 workspace TEXT,
                 id TEXT,
                 annotation_id TEXT,
                 to_state TEXT,
                 created_at BIGINT
             );
             CREATE TABLE mail_assist_meta (
                 meta_key TEXT PRIMARY KEY,
                 meta_value TEXT NOT NULL
             );
             INSERT INTO mail_assist_meta VALUES ('schema_version', '6');",
        )
        .unwrap();

        apply_schema_migrations(&conn).unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT column_name FROM information_schema.columns \
                 WHERE table_name = 'mail_messages'",
            )
            .unwrap();
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<duckdb::Result<std::collections::HashSet<_>>>()
            .unwrap();
        for required in [
            "distill_brief_json",
            "distill_contract_version",
            "distilled_at",
            "distill_revision",
            "distill_backfill_attempts",
            "distill_backfill_next_retry_at",
            "distill_backfill_last_error",
        ] {
            assert!(
                columns.contains(required),
                "missing migrated column {required}"
            );
        }
        let annotation_revision_columns: i64 = conn
            .prepare(
                "SELECT COUNT(*) FROM information_schema.columns \
                 WHERE table_name = 'mail_annotations' \
                   AND column_name = 'classification_input_revision'",
            )
            .unwrap()
            .query_row([], |row| row.get(0))
            .unwrap();
        assert_eq!(annotation_revision_columns, 1);
        let counter_tables: i64 = conn
            .prepare(
                "SELECT COUNT(*) FROM information_schema.tables \
                 WHERE table_name = 'mail_distill_revision_counters'",
            )
            .unwrap()
            .query_row([], |row| row.get(0))
            .unwrap();
        assert_eq!(counter_tables, 1);
    }

    #[test]
    fn initialized_v10_schema_migrates_attention_lane_before_current_template_indexes() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE mail_messages (
                 principal TEXT,
                 workspace TEXT,
                 provider TEXT,
                 account_alias TEXT,
                 thread_id TEXT,
                 message_id TEXT,
                 distill_state TEXT,
                 sensitive_suppressed BOOLEAN,
                 internal_date BIGINT
             );
             CREATE TABLE mail_annotations (
                 principal TEXT,
                 workspace TEXT,
                 provider TEXT,
                 account_alias TEXT,
                 thread_id TEXT,
                 id TEXT,
                 state TEXT,
                 proposed_action_json JSON,
                 created_at BIGINT,
                 updated_at BIGINT
             );
             CREATE TABLE mail_assist_events (
                 principal TEXT,
                 workspace TEXT,
                 id TEXT,
                 annotation_id TEXT,
                 to_state TEXT,
                 created_at BIGINT
             );
             CREATE TABLE mail_assist_meta (
                 meta_key TEXT PRIMARY KEY,
                 meta_value TEXT NOT NULL
             );
             INSERT INTO mail_assist_meta VALUES ('schema_version', '10');",
        )
        .unwrap();

        initialize_mail_assist_schema(&conn, BOOTSTRAP_DDL).unwrap();

        let stored: String = conn
            .prepare("SELECT meta_value FROM mail_assist_meta WHERE meta_key = 'schema_version'")
            .unwrap()
            .query_row([], |row| row.get(0))
            .unwrap();
        assert_eq!(stored, MAIL_ASSIST_DB_SCHEMA_VERSION.to_string());
        let attention_lane_columns: i64 = conn
            .prepare(
                "SELECT COUNT(*) FROM information_schema.columns \
                 WHERE table_name = 'mail_annotations' AND column_name = 'attention_lane'",
            )
            .unwrap()
            .query_row([], |row| row.get(0))
            .unwrap();
        assert_eq!(attention_lane_columns, 1);
        conn.prepare(
            "SELECT COUNT(*) FROM mail_messages \
             WHERE distill_contract_version IS NULL AND distill_revision IS NULL",
        )
        .unwrap();
    }

    #[test]
    fn fresh_schema_runs_current_template_before_recording_schema_version() {
        let conn = Connection::open_in_memory().unwrap();

        initialize_mail_assist_schema(&conn, BOOTSTRAP_DDL).unwrap();

        let stored: String = conn
            .prepare("SELECT meta_value FROM mail_assist_meta WHERE meta_key = 'schema_version'")
            .unwrap()
            .query_row([], |row| row.get(0))
            .unwrap();
        assert_eq!(stored, MAIL_ASSIST_DB_SCHEMA_VERSION.to_string());
    }

    #[tokio::test]
    async fn annotation_inherits_lane_from_its_thread_row() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        let mut envoy_thread = sample_thread("presto", "thread-e", 100);
        envoy_thread.lane = ChannelLane::Envoy;
        store
            .upsert_thread("alpha", "prod", envoy_thread)
            .await
            .unwrap();

        // Caller passes the default lane; the store overrides it from the
        // thread row.
        let mut annotation = sample_annotation("ann-e", "presto", "thread-e");
        annotation.lane = ChannelLane::UserAssist;
        let created = store
            .create_annotation("alpha", "prod", annotation, MailAssistActor::Worker)
            .await
            .unwrap();
        assert_eq!(created.lane, ChannelLane::Envoy);
        let fetched = store
            .get_annotation("alpha", "prod", "ann-e")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fetched.lane, ChannelLane::Envoy);

        // No thread row → the caller's lane stands.
        let created = store
            .create_annotation(
                "alpha",
                "prod",
                sample_annotation("ann-orphan", "acct-a", "thread-unknown"),
                MailAssistActor::Worker,
            )
            .await
            .unwrap();
        assert_eq!(created.lane, ChannelLane::UserAssist);
    }

    #[test]
    fn feedback_tuning_requires_repeated_net_negative_signal() {
        let mut profile = FeedbackTuningProfile {
            sender_dismissed: 2,
            ..FeedbackTuningProfile::default()
        };
        assert!(!profile.reduces_noise());
        profile.sender_dismissed = 3;
        assert!(profile.reduces_noise());
        profile.sender_helpful = 2;
        assert!(!profile.reduces_noise());

        let domain_profile = FeedbackTuningProfile {
            domain_dismissed: 5,
            domain_helpful: 1,
            ..FeedbackTuningProfile::default()
        };
        assert!(domain_profile.reduces_noise());
        assert_eq!(domain_profile.dominant_scope(), Some("domain"));
    }

    #[tokio::test]
    async fn provider_changes_are_idempotent_and_preserve_thread_removal_evidence() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        let annotation = sample_annotation("ann-provider", "business", "thread-provider");
        let change = ProviderThreadChange {
            schema_version: MAIL_ASSIST_SCHEMA_VERSION,
            id: "provider-change:gmail:business:101:message_deleted:thread-provider:m1".to_string(),
            provider: "gmail".to_string(),
            account_alias: "business".to_string(),
            thread_id: "thread-provider".to_string(),
            message_id: Some("m1".to_string()),
            kind: ProviderThreadChangeKind::MessageDeleted,
            thread_removed: true,
            label_ids: Vec::new(),
            current_label_ids: Vec::new(),
            provider_cursor: Some("101".to_string()),
            // Equal to the annotation creation time exercises the inclusive
            // reconciliation boundary.
            observed_at: annotation.created_at,
        };

        assert_eq!(
            store
                .append_provider_changes("alpha", "prod", vec![change.clone()])
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .append_provider_changes("alpha", "prod", vec![change.clone()])
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .list_provider_changes_after("alpha", "prod", &annotation, 10)
                .await
                .unwrap(),
            vec![change]
        );
    }

    #[tokio::test]
    async fn feedback_tuning_attributes_feedback_to_the_evidence_sender() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        let mut thread = sample_thread("business", "thread-feedback", 2_000);
        thread.latest_from_address = Some("later-sender@other.example".to_string());
        store.upsert_thread("alpha", "prod", thread).await.unwrap();
        let mut evidence = sample_message("business", "thread-feedback", "message-evidence");
        evidence.from_address = Some("original-sender@example.com".to_string());
        store
            .append_messages("alpha", "prod", vec![evidence])
            .await
            .unwrap();
        let mut annotation = sample_annotation("ann-feedback", "business", "thread-feedback");
        annotation.evidence_message_id = Some("message-evidence".to_string());
        annotation.evidence_message_at = Some(1_000);
        store
            .create_annotation("alpha", "prod", annotation, MailAssistActor::Worker)
            .await
            .unwrap();
        for index in 0..3 {
            store
                .append_feedback(
                    "alpha",
                    "prod",
                    MailAssistUserFeedback {
                        schema_version: MAIL_ASSIST_SCHEMA_VERSION,
                        id: format!("feedback-{index}"),
                        annotation_id: "ann-feedback".to_string(),
                        provider: "gmail".to_string(),
                        account_alias: "business".to_string(),
                        thread_id: Some("thread-feedback".to_string()),
                        verdict: MailFeedbackVerdict::NotHelpful,
                        comment: None,
                        actor: MailAssistActor::User,
                        created_at: 3_000 + index,
                    },
                )
                .await
                .unwrap();
        }

        let original = store
            .feedback_tuning_profile(
                "alpha",
                "prod",
                "gmail",
                "business",
                Some("original-sender@example.com"),
                0,
                20,
            )
            .await
            .unwrap();
        assert_eq!(original.sender_dismissed, 3);
        assert!(original.reduces_noise());

        let later = store
            .feedback_tuning_profile(
                "alpha",
                "prod",
                "gmail",
                "business",
                Some("later-sender@other.example"),
                0,
                20,
            )
            .await
            .unwrap();
        assert_eq!(later.sender_dismissed, 0);
    }

    #[tokio::test]
    async fn stale_drafts_remain_in_follow_ups_until_owner_review_reopens_them() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        store
            .upsert_thread(
                "alpha",
                "prod",
                sample_thread("business", "thread-draft", 100),
            )
            .await
            .unwrap();
        let mut annotation = sample_annotation("ann-draft", "business", "thread-draft");
        annotation.state = MailAnnotationState::DraftReady;
        store
            .create_annotation("alpha", "prod", annotation, MailAssistActor::Worker)
            .await
            .unwrap();
        store
            .transition_annotation(
                "alpha",
                "prod",
                "ann-draft",
                MailAnnotationState::Stale,
                MailAssistActor::Worker,
                Some(serde_json::json!({
                    "action": "reconcile",
                    "prior_state": "draft_ready",
                    "verdict": "newer_message_requires_draft_review",
                })),
                200,
            )
            .await
            .unwrap();

        assert_eq!(
            store.count_needs_approval("alpha", "prod").await.unwrap(),
            1
        );
        let stale_page = store
            .list_needs_approval_attention_lane_page("alpha", "prod", "follow_up", 10, 0, None)
            .await
            .unwrap();
        assert_eq!(stale_page.rows.len(), 1);
        assert_eq!(stale_page.rows[0].state, MailAnnotationState::Stale);

        store
            .transition_annotation(
                "alpha",
                "prod",
                "ann-draft",
                MailAnnotationState::NeedsApproval,
                MailAssistActor::User,
                Some(serde_json::json!({ "action": "review_stale_draft" })),
                300,
            )
            .await
            .unwrap();
        let reopened = store
            .list_needs_approval_attention_lane_page("alpha", "prod", "follow_up", 10, 0, None)
            .await
            .unwrap();
        assert_eq!(reopened.rows.len(), 1);
        assert_eq!(reopened.rows[0].state, MailAnnotationState::NeedsApproval);
    }

    #[tokio::test]
    async fn needs_approval_source_generation_token_is_stable_and_tracks_lane_membership() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        store
            .upsert_thread(
                "alpha",
                "prod",
                sample_thread("business", "thread-generation", 100),
            )
            .await
            .unwrap();
        let mut annotation = sample_annotation("ann-generation", "business", "thread-generation");
        annotation.state = MailAnnotationState::NeedsApproval;
        store
            .create_annotation("alpha", "prod", annotation, MailAssistActor::Worker)
            .await
            .unwrap();
        let first = store
            .needs_approval_source_generation_token("alpha", "prod", "follow_up", 1_000)
            .await
            .unwrap();
        assert_eq!(
            first,
            store
                .needs_approval_source_generation_token("alpha", "prod", "follow_up", 1_000)
                .await
                .unwrap()
        );
        assert_eq!(
            first,
            store
                .needs_approval_source_generation_token("alpha", "prod", "follow_up", 10_000,)
                .await
                .unwrap(),
            "clock movement inside one temporal bucket must preserve source identity"
        );
        assert_ne!(
            first,
            store
                .needs_approval_source_generation_token("alpha", "prod", "follow_up", 1_000_000,)
                .await
                .unwrap(),
            "crossing the model's temporal bucket must invalidate projection identity"
        );

        store
            .transition_annotation(
                "alpha",
                "prod",
                "ann-generation",
                MailAnnotationState::Dismissed,
                MailAssistActor::User,
                Some(serde_json::json!({ "action": "dismiss" })),
                200,
            )
            .await
            .unwrap();
        assert_ne!(
            first,
            store
                .needs_approval_source_generation_token("alpha", "prod", "follow_up", 1_000)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn today_page_reads_only_latest_actionable_annotation_per_thread() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        store
            .upsert_thread(
                "alpha",
                "prod",
                sample_thread("business", "thread-current", 500),
            )
            .await
            .unwrap();

        let mut older = sample_annotation("ann-older", "business", "thread-current");
        older.state = MailAnnotationState::NeedsApproval;
        older.created_at = 100;
        older.updated_at = 100;
        store
            .create_annotation("alpha", "prod", older, MailAssistActor::Worker)
            .await
            .unwrap();

        let mut newer = sample_annotation("ann-newer", "business", "thread-current");
        newer.state = MailAnnotationState::NeedsApproval;
        newer.created_at = 200;
        newer.updated_at = 200;
        store
            .create_annotation("alpha", "prod", newer, MailAssistActor::Worker)
            .await
            .unwrap();

        let page = store
            .list_needs_approval_attention_lane_page("alpha", "prod", "follow_up", 10, 0, None)
            .await
            .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].annotation_id, "ann-newer");
    }

    #[tokio::test]
    async fn writing_preferences_dedupe_promote_and_filter_dismissed() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        let first = store
            .upsert_writing_preference(
                "alpha",
                "prod",
                "gmail",
                "business",
                WritingPreferenceScopeKind::Sender,
                "person@example.com",
                "Keep replies concise.",
                WritingPreferenceStatus::Candidate,
                Some("ann-1"),
                100,
            )
            .await
            .unwrap();
        let repeated = store
            .upsert_writing_preference(
                "alpha",
                "prod",
                "gmail",
                "business",
                WritingPreferenceScopeKind::Sender,
                "PERSON@example.com",
                "Keep replies concise.",
                WritingPreferenceStatus::Candidate,
                Some("ann-2"),
                200,
            )
            .await
            .unwrap();
        assert_eq!(first.id, repeated.id);
        assert_eq!(repeated.evidence_count, 2);
        let promoted = store
            .set_writing_preference_status(
                "alpha",
                "prod",
                &first.id,
                WritingPreferenceStatus::Promoted,
                300,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(promoted.status, WritingPreferenceStatus::Promoted);
        assert_eq!(
            store
                .list_writing_preferences(
                    "alpha",
                    "prod",
                    "gmail",
                    "business",
                    Some("person@example.com"),
                    Some("example.com"),
                )
                .await
                .unwrap()
                .len(),
            1
        );
        store
            .set_writing_preference_status(
                "alpha",
                "prod",
                &first.id,
                WritingPreferenceStatus::Dismissed,
                400,
            )
            .await
            .unwrap();
        assert!(store
            .list_writing_preferences(
                "alpha",
                "prod",
                "gmail",
                "business",
                Some("person@example.com"),
                Some("example.com"),
            )
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn compaction_drains_reader_clones_and_preserves_writes_in_new_generation() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        store
            .append_messages(
                "alpha",
                "prod",
                vec![sample_message("acct-a", "t", "before")],
            )
            .await
            .unwrap();
        let inner = store.scope_inner("alpha", "prod").unwrap();
        let reader = inner.read_connection().unwrap();
        assert!(store
            .compact_scope_with_wait("alpha", "prod", Duration::ZERO)
            .await
            .unwrap_err()
            .to_string()
            .contains("deferred"));
        drop(reader);
        store
            .compact_scope_with_wait("alpha", "prod", Duration::from_secs(1))
            .await
            .unwrap();
        store
            .append_messages(
                "alpha",
                "prod",
                vec![sample_message("acct-a", "t", "after")],
            )
            .await
            .unwrap();
        let reader = inner.read_connection().unwrap();
        assert_eq!(
            reader
                .query_row("SELECT count(*) FROM mail_messages", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
        let threads: i64 = reader
            .query_row("SELECT current_setting('threads')", [], |r| r.get(0))
            .unwrap();
        assert_eq!(threads, 1);
    }

    #[tokio::test]
    async fn annotation_batch_preserves_input_order_duplicates_and_scope() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        for (id, account, thread) in [
            ("a", "acct-a", "t1"),
            ("b", "acct-a", "t2"),
            ("hidden", "acct-b", "t1"),
        ] {
            store
                .create_annotation(
                    "alpha",
                    "prod",
                    sample_annotation(id, account, thread),
                    MailAssistActor::Worker,
                )
                .await
                .unwrap();
        }
        store
            .create_annotation(
                "other",
                "prod",
                sample_annotation("other-scope", "acct-a", "t1"),
                MailAssistActor::Worker,
            )
            .await
            .unwrap();
        let mut ids = (0..260).map(|i| format!("unknown-{i}")).collect::<Vec<_>>();
        ids.extend(["t2".to_owned(), "t1".to_owned(), "t2".to_owned()]);
        let rows = store
            .list_annotations_by_thread_ids("alpha", "prod", "gmail", "acct-a", &ids)
            .await
            .unwrap();
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["b", "a", "b"]
        );
    }

    #[tokio::test]
    async fn copy_compaction_preserves_mail_lifecycle_rows_and_audit_history() {
        let tmp = TempDir::new().unwrap();
        let store = MailAssistStore::open(tmp.path()).unwrap();
        store
            .upsert_thread(
                "alpha",
                "prod",
                sample_thread("business", "thread-compact", 1_200),
            )
            .await
            .unwrap();
        store
            .append_messages(
                "alpha",
                "prod",
                vec![sample_message(
                    "business",
                    "thread-compact",
                    "message-compact",
                )],
            )
            .await
            .unwrap();
        let mut annotation = sample_annotation("annotation-compact", "business", "thread-compact");
        annotation.state = MailAnnotationState::NeedsApproval;
        annotation.evidence_message_id = Some("message-compact".to_string());
        store
            .create_annotation("alpha", "prod", annotation, MailAssistActor::Worker)
            .await
            .unwrap();

        let before_events = store
            .list_events_for_annotation("alpha", "prod", "annotation-compact")
            .await
            .unwrap();
        let report = store.compact_scope("alpha", "prod").await.unwrap();
        assert!(report.row_count >= 4);

        let threads = store
            .get_threads_by_ids(
                "alpha",
                "prod",
                "gmail",
                "business",
                &["thread-compact".to_string()],
            )
            .await
            .unwrap();
        assert_eq!(threads.len(), 1);
        assert!(store
            .get_message("alpha", "prod", "gmail", "business", "message-compact")
            .await
            .unwrap()
            .is_some());
        assert_eq!(
            store
                .get_annotation("alpha", "prod", "annotation-compact")
                .await
                .unwrap()
                .unwrap()
                .state,
            MailAnnotationState::NeedsApproval
        );
        assert_eq!(
            store
                .list_events_for_annotation("alpha", "prod", "annotation-compact")
                .await
                .unwrap(),
            before_events
        );

        // Regression: compaction replaces the physical DuckDB and therefore
        // must also replace the cached same-instance reader template. Before
        // the rebind, the guarded dismissal was committed through write_conn
        // while Follow-ups/get_annotation continued reading the pre-compaction
        // snapshot forever.
        let transition = store
            .transition_annotation_if_state(
                "alpha",
                "prod",
                "annotation-compact",
                MailAnnotationState::NeedsApproval,
                MailAnnotationState::Dismissed,
                MailAssistActor::User,
                None,
                None,
                2_000,
            )
            .await
            .unwrap();
        assert!(matches!(
            transition,
            AnnotationTransitionResult::Applied(ref annotation)
                if annotation.state == MailAnnotationState::Dismissed
        ));
        assert_eq!(
            store
                .get_annotation("alpha", "prod", "annotation-compact")
                .await
                .unwrap()
                .unwrap()
                .state,
            MailAnnotationState::Dismissed
        );
        assert!(store
            .list_needs_approval_attention_lane_page("alpha", "prod", "follow_up", 10, 0, None)
            .await
            .unwrap()
            .rows
            .is_empty());
    }
}
