//! Ambient browser-capture ingestion + consent control (WEG Phase 2, Slice P2.1).
//!
//! User-owned, opt-in capture of normal browsing. The browser extension (P2.2)
//! batches sanitized page signals to `POST /ambient/signals/batch`; the server
//! gates on the per-scope consent flag, applies an ingress policy (denylist,
//! secret-field redaction, in-batch idempotency), and persists only sanitized
//! signals to the analytics raw store (`events`, `event_type=ambient_signal`)
//! for later distillation into user-owned evidence (P2.3).
//!
//! The `/observe` page's "Observe tabs" card drives `GET /ambient/status` +
//! `PUT /ambient/config` (the design's required visible pause/suppress control).
//!
//! Batch ingestion uses the standard workspace-bound bearer. The authentication
//! middleware replaces any caller-supplied compatibility headers with the
//! verified identity before these handlers resolve scope.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use actix_web::{web, HttpRequest, HttpResponse};
use magicllm::LLMProviderKind;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, instrument, warn};

use crate::scope::resolve_required_scope;
use magician::magician_v2::agents::AgentMemoryResolver;
use magician::magician_v2::analytics::event_sink::AnalyticsEvent;
use magician::magician_v2::analytics::runtime_activity_layer::{KIND_BACKGROUND, WORKLOAD_AMBIENT};
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::artifacts::durable_store::{
    open_local_durable_artifacts, DurableArtifactStore, DurableFrontmatter,
};
use magician::magician_v2::chat::models::{
    ChatChannel, ChatMessage, ChatMessageContent, ChatMessageDirection, ChatSession,
    ChatSessionStatus,
};
use magician::magician_v2::chat::storage::{ChatStore, FileChatStore};
use magician::magician_v2::chat::DEFAULT_AGENT_ID;
use magician::magician_v2::evidence::{
    cluster_signals, distill_ambient_cluster_pinned_with_response, entity_candidates_from_evidence,
    is_cluster_salient, is_salient, stamp_ambient_evidence, AmbientDistillLlmOutcome,
    AmbientSignalRow, SignalCluster,
};
use magician::magician_v2::prompts::PromptManager;
use magician::magician_v2::query_analysis::operation_llm_router::{
    OperationLlmRouter, SimplifiedLLMResponse,
};
use magician::magician_v2::realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent};
use uuid::Uuid;

const AMBIENT_NAMESPACE: &str = "ambient";
const CONFIG_NAME: &str = "config.json";
/// Paired-browser registry (`ambient/collectors.json`) + the bearer-token header
/// the extension presents on every batch. The server resolves scope FROM the
/// token (P2.1b), so the untrusted extension cannot self-assert a principal.
const COLLECTORS_NAME: &str = "collectors.json";
const COLLECTOR_TOKEN_HEADER: &str = "x-collector-token";
/// Bounded scoped buffer of sanitized signals the distiller reads. Kept separate
/// from the analytics raw store because DuckDB allows a single read-write handle
/// per file, and the server's analytics sink already holds it — so distilling
/// can't open it concurrently. `emit()` to analytics still happens (queryable
/// raw store); this JSON buffer is the distill input.
const SIGNALS_NAME: &str = "signals.json";
const MAX_BUFFERED_SIGNALS: usize = 2000;
const DISTILL_STATE_NAME: &str = "distill_state.json";
const AMBIENT_DISTILL_OPERATION: &str = "ambient_distill";
const MAX_DISTILL_RECEIPTS: usize = 5000;
const DEFAULT_DISTILL_INTERVAL_SECS: u64 = 15 * 60;
const DEFAULT_DISTILL_STARTUP_DELAY_SECS: u64 = 3 * 60;
const DEFAULT_DISTILL_MAX_CLUSTERS_PER_TICK: usize = 4;
const DEFAULT_DISTILL_MIN_CLUSTER_INTERVAL_SECS: u64 = 30 * 60;
const DEFAULT_DISTILL_LOOKBACK_DAYS: i64 = 1;
/// Server-owned receipt ledger (`ambient/receipts.json`): accepted `signal_id`s
/// for cross-batch idempotency (a retried upload can't double-process) + a
/// bounded, content-free rejection audit trail (signal id + reason + ts, never
/// the offending content). Both are capped.
const RECEIPTS_NAME: &str = "receipts.json";
const MAX_RECEIPT_IDS: usize = 5000;
const MAX_REJECTION_STUBS: usize = 500;
const STATS_OVERVIEW_NAME: &str = "stats/overview.json";
const STATS_BATCHES_PREFIX: &str = "stats/batches";
const STATS_SIGNALS_PREFIX: &str = "stats/signals_index";
const STATS_RUNS_PREFIX: &str = "stats/distill_runs";
const STATS_CLUSTERS_PREFIX: &str = "stats/cluster_journal";
const STATS_DAILY_PREFIX: &str = "stats/daily";
const TABS_THREAD_ID: &str = "tabs";
const TABS_SUMMARY_SOURCE_SURFACE: &str = "ambient-tabs-summary";
const DEFAULT_STATS_SIGNAL_DETAIL_RETENTION_DAYS: i64 = 14;
const DEFAULT_STATS_BATCH_DETAIL_RETENTION_DAYS: i64 = 30;
const DEFAULT_STATS_RUN_DETAIL_RETENTION_DAYS: i64 = 90;
const DEFAULT_STATS_CLUSTER_DETAIL_RETENTION_DAYS: i64 = 90;
const MAX_STATS_QUERY_DAYS: i64 = 90;

/// Secret-class key substrings: matching fields are dropped before any persist.
const SECRET_KEY_HINTS: &[&str] = &[
    "token", "secret", "password", "passwd", "auth", "apikey", "api_key", "key", "session", "csrf",
    "xsrf", "cookie", "otp", "cvv", "card",
];

#[derive(Clone)]
pub struct AmbientApi {
    workspace_layout: ArtifactV2Workspace,
    memory_resolver: AgentMemoryResolver,
    event_broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
}

impl AmbientApi {
    pub fn new(
        workspace_layout: ArtifactV2Workspace,
        memory_resolver: AgentMemoryResolver,
        event_broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
    ) -> Self {
        Self {
            workspace_layout,
            memory_resolver,
            event_broadcaster,
        }
    }
}

/// Per-scope ambient capture consent + counters. Persisted as a durable
/// artifact (`ambient/config.json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AmbientConfig {
    /// Capture is off until the user enables it from the Observe page.
    #[serde(default)]
    pub enabled: bool,
    /// Origins never captured (suffix-matched). Private/incognito windows are
    /// always excluded client-side regardless of this list.
    #[serde(default)]
    pub denylist: Vec<String>,
    #[serde(default)]
    pub total_signals: u64,
    #[serde(default)]
    pub total_rejected: u64,
    #[serde(default)]
    pub last_signal_at: Option<String>,
}

/// A sanitized signal in the distill buffer (`ambient/signals.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredAmbientSignal {
    signal_id: String,
    origin: String,
    #[serde(default)]
    surface: Option<String>,
    #[serde(default)]
    event_kind: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    safe_url: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    page_key: Option<String>,
    #[serde(default)]
    dedupe_key: Option<String>,
    #[serde(default)]
    sensitivity: Option<String>,
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default)]
    payload_bytes: u64,
    #[serde(default)]
    metadata_bytes: u64,
    #[serde(default)]
    dom_estimated_bytes: u64,
    #[serde(default)]
    has_password_field: bool,
    #[serde(default)]
    heading_count: u64,
    /// Arrival time (ms since epoch) — used for the distill window filter.
    ts_ms: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AmbientByteTotals {
    #[serde(default)]
    batch_raw_bytes: u64,
    #[serde(default)]
    signal_payload_bytes: u64,
    #[serde(default)]
    signal_metadata_bytes: u64,
    #[serde(default)]
    dom_estimated_bytes: u64,
}

impl AmbientByteTotals {
    fn add_assign(&mut self, other: &AmbientByteTotals) {
        self.batch_raw_bytes = self.batch_raw_bytes.saturating_add(other.batch_raw_bytes);
        self.signal_payload_bytes = self
            .signal_payload_bytes
            .saturating_add(other.signal_payload_bytes);
        self.signal_metadata_bytes = self
            .signal_metadata_bytes
            .saturating_add(other.signal_metadata_bytes);
        self.dom_estimated_bytes = self
            .dom_estimated_bytes
            .saturating_add(other.dom_estimated_bytes);
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AmbientWorkerStats {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    running: bool,
    #[serde(default)]
    current_run_id: Option<String>,
    #[serde(default)]
    last_tick_at_ms: Option<i64>,
    #[serde(default)]
    last_run_id: Option<String>,
    #[serde(default)]
    last_run_status: Option<String>,
    #[serde(default)]
    interval_secs: Option<u64>,
    #[serde(default)]
    lookback_days: Option<i64>,
    #[serde(default)]
    max_clusters_per_tick: Option<usize>,
    #[serde(default)]
    min_cluster_interval_secs: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AmbientPendingStats {
    #[serde(default)]
    clusters_due: u64,
    #[serde(default)]
    clusters_processing: u64,
    #[serde(default)]
    clusters_skipped_recent: u64,
    #[serde(default)]
    clusters_failed: u64,
    #[serde(default)]
    review_pending: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AmbientStatsOverview {
    #[serde(default = "ambient_schema_version")]
    schema_version: u32,
    #[serde(default)]
    updated_at_ms: i64,
    #[serde(default)]
    last_batch_at_ms: Option<i64>,
    #[serde(default)]
    last_signal_at_ms: Option<i64>,
    #[serde(default)]
    total_batches: u64,
    #[serde(default)]
    total_signals: u64,
    #[serde(default)]
    total_rejected: u64,
    #[serde(default)]
    buffered_signals: u64,
    #[serde(default)]
    accepted_today: u64,
    #[serde(default)]
    pages_today: u64,
    #[serde(default)]
    distinct_pages_today: u64,
    #[serde(default)]
    origins_today: u64,
    #[serde(default)]
    bytes_today: AmbientByteTotals,
    #[serde(default)]
    page_keys_today: HashSet<String>,
    #[serde(default)]
    origins_set_today: HashSet<String>,
    #[serde(default)]
    by_type_today: HashMap<String, u64>,
    #[serde(default)]
    by_event_kind_today: HashMap<String, u64>,
    #[serde(default)]
    today_detail_backfilled_day: Option<String>,
    #[serde(default)]
    llm_today_day: Option<String>,
    #[serde(default)]
    llm_calls_today: u64,
    #[serde(default)]
    llm_input_tokens_today: u64,
    #[serde(default)]
    llm_output_tokens_today: u64,
    #[serde(default)]
    llm_cost_usd_today: f64,
    #[serde(default)]
    worker: AmbientWorkerStats,
    #[serde(default)]
    pending: AmbientPendingStats,
    #[serde(default)]
    retention_last_swept_day: Option<String>,
    #[serde(default)]
    retention_last_swept_at_ms: Option<i64>,
    #[serde(default)]
    retention_last_sweep_status: Option<String>,
    #[serde(default)]
    retention_last_sweep_deleted_files: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AmbientBatchStatsRow {
    schema_version: u32,
    batch_id: String,
    received_at_ms: i64,
    #[serde(default)]
    raw_signal_count: u64,
    #[serde(default)]
    accepted_count: u64,
    #[serde(default)]
    rejected_count: u64,
    #[serde(default)]
    duplicate_count: u64,
    #[serde(default)]
    batch_raw_bytes: u64,
    #[serde(default)]
    accepted_payload_bytes: u64,
    #[serde(default)]
    stored_metadata_bytes: u64,
    #[serde(default)]
    rejections_by_reason: HashMap<String, u64>,
    #[serde(default)]
    oldest_signal_ts_ms: Option<i64>,
    #[serde(default)]
    newest_signal_ts_ms: Option<i64>,
    #[serde(default)]
    client_queue_depth: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AmbientSignalIndexRow {
    schema_version: u32,
    signal_id: String,
    batch_id: String,
    ts_ms: i64,
    origin: String,
    #[serde(default)]
    safe_url: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    page_key: Option<String>,
    #[serde(default)]
    surface: Option<String>,
    #[serde(default)]
    event_kind: Option<String>,
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default)]
    sensitivity: Option<String>,
    #[serde(default)]
    payload_bytes: u64,
    #[serde(default)]
    metadata_bytes: u64,
    #[serde(default)]
    dom_estimated_bytes: u64,
    #[serde(default)]
    summary_len: u64,
    #[serde(default)]
    heading_count: u64,
    #[serde(default)]
    has_password_field: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AmbientDistillRunStatsRow {
    schema_version: u32,
    run_id: String,
    source: String,
    started_at_ms: i64,
    #[serde(default)]
    finished_at_ms: Option<i64>,
    status: String,
    lookback_days: i64,
    signals_considered: u64,
    pages_considered: u64,
    distinct_pages_considered: u64,
    origins_considered: u64,
    clusters_total: u64,
    clusters_promotable: u64,
    clusters_due: u64,
    clusters_skipped_recent: u64,
    clusters_skipped_batch_limit: u64,
    llm_started: u64,
    llm_succeeded: u64,
    llm_failed: u64,
    evidence_created: u64,
    memory_candidates: u64,
    review_pending: u64,
    #[serde(default)]
    errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AmbientClusterJournalRow {
    schema_version: u32,
    run_id: String,
    cluster_id: String,
    cluster_key: String,
    state: String,
    updated_at_ms: i64,
    host: String,
    window_day: String,
    signal_count: u64,
    page_count: u64,
    distinct_page_count: u64,
    #[serde(default)]
    bytes: AmbientByteTotals,
    #[serde(default)]
    types: serde_json::Value,
    #[serde(default)]
    salience: f64,
    #[serde(default)]
    llm: Option<serde_json::Value>,
    #[serde(default)]
    evidence_id: Option<String>,
    #[serde(default)]
    memory: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<String>,
}

fn ambient_schema_version() -> u32 {
    1
}

/// Per-scope worker receipt ledger (`ambient/distill_state.json`). It prevents
/// the lifecycle worker from re-running the same host/day cluster every tick.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AmbientDistillState {
    #[serde(default)]
    receipts: Vec<AmbientDistillReceipt>,
    #[serde(default)]
    last_run_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AmbientDistillReceipt {
    cluster_key: String,
    signal_hash: String,
    signal_count: usize,
    last_distilled_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VerifiedAmbientLocalBinding {
    profile: String,
    kind: LLMProviderKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AmbientDistillUnavailable {
    OperationUnbound,
    NonLocalProvider(String),
}

impl std::fmt::Display for AmbientDistillUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OperationUnbound => write!(
                f,
                "operation '{AMBIENT_DISTILL_OPERATION}' is not explicitly bound in \
                 operation_mapping"
            ),
            Self::NonLocalProvider(kind) => write!(
                f,
                "operation '{AMBIENT_DISTILL_OPERATION}' is bound to provider kind '{kind}', \
                 not local ollama"
            ),
        }
    }
}

fn resolve_ambient_local_provider(
    router: &OperationLlmRouter,
) -> Result<VerifiedAmbientLocalBinding, AmbientDistillUnavailable> {
    // One shared policy-aware core: Ollama under `privacy.processing.mode:
    // local`, the `when_cloud` arm under cloud, refusal when unbound.
    let verified = magician::magician_v2::llm_dispatch_seam::resolve_local_provider_for_operation(
        Some(router),
        AMBIENT_DISTILL_OPERATION,
    )
    .map_err(|reason| match reason {
        magician::magician_v2::llm_dispatch_seam::DistillUnavailable::RouterUnavailable => {
            AmbientDistillUnavailable::OperationUnbound
        },
        magician::magician_v2::llm_dispatch_seam::DistillUnavailable::OperationUnbound => {
            AmbientDistillUnavailable::OperationUnbound
        },
        magician::magician_v2::llm_dispatch_seam::DistillUnavailable::NonLocalProvider(kind) => {
            AmbientDistillUnavailable::NonLocalProvider(kind)
        },
    })?;
    Ok(VerifiedAmbientLocalBinding {
        profile: verified.profile,
        kind: verified.kind,
    })
}

async fn load_signal_buffer(store: &DurableArtifactStore) -> Vec<StoredAmbientSignal> {
    match store.read(AMBIENT_NAMESPACE, SIGNALS_NAME).await {
        Ok((_, body)) => serde_json::from_str(&body).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

async fn save_signal_buffer(
    store: &DurableArtifactStore,
    buffer: &[StoredAmbientSignal],
) -> Result<(), String> {
    let body = serde_json::to_string(buffer).map_err(|e| e.to_string())?;
    let frontmatter = DurableFrontmatter {
        namespace: AMBIENT_NAMESPACE.to_string(),
        name: SIGNALS_NAME.to_string(),
        created_by: "ambient".to_string(),
        last_updated_by: "ambient".to_string(),
        last_updated: chrono::Utc::now(),
        content_type: Some("application/json".to_string()),
        source_execution_id: None,
        source_task_id: None,
        source_workflow_instance_id: None,
        source_run_id: None,
        source_cycle_id: None,
        source_agent_id: None,
        producer_stage: Some("ambient_signal_buffer".to_string()),
    };
    store
        .write(AMBIENT_NAMESPACE, SIGNALS_NAME, &body, frontmatter)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn stats_day_key_from_ms(ts_ms: i64) -> String {
    let dt = match chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ts_ms) {
        Some(dt) => dt,
        None => chrono::Utc::now(),
    };
    dt.format("%Y-%m-%d").to_string()
}

fn stats_jsonl_name(prefix: &str, day_key: &str, file_name: &str) -> String {
    format!("{prefix}/dt={day_key}/{file_name}")
}

fn stats_daily_summary_name(day_key: &str) -> String {
    format!("{STATS_DAILY_PREFIX}/dt={day_key}/summary.json")
}

fn parse_ambient_stats_query_day_value(value: &str) -> Option<chrono::NaiveDate> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(day) = chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        return Some(day);
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(trimmed) {
        return Some(dt.with_timezone(&chrono::Utc).date_naive());
    }
    let ts = trimmed.parse::<i64>().ok()?;
    let ms = if (-10_000_000_000..=10_000_000_000).contains(&ts) {
        ts.saturating_mul(1000)
    } else {
        ts
    };
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms).map(|dt| dt.date_naive())
}

fn ambient_stats_query_range(
    from: Option<&str>,
    to: Option<&str>,
) -> (chrono::NaiveDate, chrono::NaiveDate, bool) {
    let today = chrono::Utc::now().date_naive();
    let mut to_day = to
        .and_then(parse_ambient_stats_query_day_value)
        .unwrap_or(today);
    let mut from_day = from
        .and_then(parse_ambient_stats_query_day_value)
        .unwrap_or(to_day);
    if from_day > to_day {
        std::mem::swap(&mut from_day, &mut to_day);
    }
    let mut capped = false;
    let max_span = MAX_STATS_QUERY_DAYS.saturating_sub(1);
    if (to_day - from_day).num_days() > max_span {
        from_day = to_day - chrono::Duration::days(max_span);
        capped = true;
    }
    (from_day, to_day, capped)
}

fn ambient_stats_day_keys(from_day: chrono::NaiveDate, to_day: chrono::NaiveDate) -> Vec<String> {
    let days = (to_day - from_day).num_days().max(0);
    (0..=days)
        .map(|offset| {
            (from_day + chrono::Duration::days(offset))
                .format("%Y-%m-%d")
                .to_string()
        })
        .collect()
}

fn ambient_frontmatter(name: &str, stage: &str) -> DurableFrontmatter {
    DurableFrontmatter {
        namespace: AMBIENT_NAMESPACE.to_string(),
        name: name.to_string(),
        created_by: "ambient".to_string(),
        last_updated_by: "ambient".to_string(),
        last_updated: chrono::Utc::now(),
        content_type: Some("application/json".to_string()),
        source_execution_id: None,
        source_task_id: None,
        source_workflow_instance_id: None,
        source_run_id: None,
        source_cycle_id: None,
        source_agent_id: None,
        producer_stage: Some(stage.to_string()),
    }
}

async fn load_stats_overview(store: &DurableArtifactStore) -> AmbientStatsOverview {
    match store.read(AMBIENT_NAMESPACE, STATS_OVERVIEW_NAME).await {
        Ok((_, body)) => serde_json::from_str(&body).unwrap_or_default(),
        Err(_) => AmbientStatsOverview::default(),
    }
}

async fn save_stats_overview(
    store: &DurableArtifactStore,
    overview: &AmbientStatsOverview,
) -> Result<(), String> {
    let body = serde_json::to_string_pretty(overview).map_err(|e| e.to_string())?;
    store
        .write(
            AMBIENT_NAMESPACE,
            STATS_OVERVIEW_NAME,
            &body,
            ambient_frontmatter(STATS_OVERVIEW_NAME, "ambient_stats_overview"),
        )
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn overview_batch_day(overview: &AmbientStatsOverview) -> Option<String> {
    overview.last_batch_at_ms.map(stats_day_key_from_ms)
}

fn overview_needs_today_detail_backfill(overview: &AmbientStatsOverview, day_key: &str) -> bool {
    overview.today_detail_backfilled_day.as_deref() != Some(day_key)
        || overview.llm_today_day.as_deref() != Some(day_key)
}

async fn refresh_today_overview_from_detail(
    store: &DurableArtifactStore,
    overview: &mut AmbientStatsOverview,
    day_key: &str,
) -> Result<(), String> {
    let signal_name = stats_jsonl_name(STATS_SIGNALS_PREFIX, day_key, "signals.jsonl");
    let batch_name = stats_jsonl_name(STATS_BATCHES_PREFIX, day_key, "batches.jsonl");
    let cluster_name = stats_jsonl_name(STATS_CLUSTERS_PREFIX, day_key, "clusters.jsonl");
    let signal_rows: Vec<AmbientSignalIndexRow> = read_stats_jsonl(store, &signal_name).await;
    let batch_rows: Vec<AmbientBatchStatsRow> = read_stats_jsonl(store, &batch_name).await;
    let cluster_rows: Vec<AmbientClusterJournalRow> = read_stats_jsonl(store, &cluster_name).await;

    overview.buffered_signals = load_signal_buffer(store).await.len() as u64;

    if !signal_rows.is_empty() || !batch_rows.is_empty() {
        let mut bytes_today = AmbientByteTotals::default();
        let mut by_type_today = HashMap::<String, u64>::new();
        let mut by_event_kind_today = HashMap::<String, u64>::new();
        let mut origins = HashSet::<String>::new();
        let mut page_keys = HashSet::<String>::new();

        for row in &signal_rows {
            bytes_today.signal_payload_bytes = bytes_today
                .signal_payload_bytes
                .saturating_add(row.payload_bytes);
            bytes_today.signal_metadata_bytes = bytes_today
                .signal_metadata_bytes
                .saturating_add(row.metadata_bytes);
            bytes_today.dom_estimated_bytes = bytes_today
                .dom_estimated_bytes
                .saturating_add(row.dom_estimated_bytes);
            incr(
                &mut by_type_today,
                row.content_type
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                1,
            );
            incr(
                &mut by_event_kind_today,
                row.event_kind
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                1,
            );
            origins.insert(row.origin.clone());
            if let Some(page_key) = &row.page_key {
                page_keys.insert(page_key.clone());
            }
        }

        let accepted_from_batches = batch_rows
            .iter()
            .map(|row| row.accepted_count)
            .fold(0u64, |acc, value| acc.saturating_add(value));
        let accepted_payload_bytes = batch_rows
            .iter()
            .map(|row| row.accepted_payload_bytes)
            .fold(0u64, |acc, value| acc.saturating_add(value));
        let stored_metadata_bytes = batch_rows
            .iter()
            .map(|row| row.stored_metadata_bytes)
            .fold(0u64, |acc, value| acc.saturating_add(value));
        bytes_today.batch_raw_bytes = batch_rows
            .iter()
            .map(|row| row.batch_raw_bytes)
            .fold(0u64, |acc, value| acc.saturating_add(value));
        if bytes_today.signal_payload_bytes == 0 {
            bytes_today.signal_payload_bytes = accepted_payload_bytes;
        }
        if bytes_today.signal_metadata_bytes == 0 {
            bytes_today.signal_metadata_bytes = stored_metadata_bytes;
        }

        let accepted_today = if signal_rows.is_empty() {
            accepted_from_batches
        } else {
            signal_rows.len() as u64
        };
        overview.accepted_today = accepted_today;
        overview.pages_today = accepted_today;
        overview.distinct_pages_today = page_keys.len() as u64;
        overview.origins_today = origins.len() as u64;
        overview.bytes_today = bytes_today;
        overview.page_keys_today = page_keys;
        overview.origins_set_today = origins;
        overview.by_type_today = by_type_today;
        overview.by_event_kind_today = by_event_kind_today;
        if let Some(last_batch_at_ms) = batch_rows.iter().map(|row| row.received_at_ms).max() {
            overview.last_batch_at_ms = Some(last_batch_at_ms);
        }
        if let Some(last_signal_at_ms) = signal_rows.iter().map(|row| row.ts_ms).max() {
            overview.last_signal_at_ms = Some(last_signal_at_ms);
        }
    }

    let mut llm_calls = 0u64;
    let mut llm_input_tokens = 0u64;
    let mut llm_output_tokens = 0u64;
    let mut llm_cost_usd = 0.0f64;
    for row in &cluster_rows {
        if row.state != "llm_succeeded" {
            continue;
        }
        llm_calls = llm_calls.saturating_add(1);
        if let Some(llm) = &row.llm {
            llm_input_tokens = llm_input_tokens.saturating_add(json_u64(llm, &["input_tokens"]));
            llm_output_tokens = llm_output_tokens.saturating_add(json_u64(llm, &["output_tokens"]));
            llm_cost_usd += json_f64(llm, &["cost_usd"]);
        }
    }
    overview.today_detail_backfilled_day = Some(day_key.to_string());
    overview.llm_today_day = Some(day_key.to_string());
    overview.llm_calls_today = llm_calls;
    overview.llm_input_tokens_today = llm_input_tokens;
    overview.llm_output_tokens_today = llm_output_tokens;
    overview.llm_cost_usd_today = llm_cost_usd;
    Ok(())
}

async fn append_stats_jsonl<T: Serialize>(
    store: &DurableArtifactStore,
    name: &str,
    row: &T,
    stage: &str,
) -> Result<(), String> {
    let mut body = match store.read(AMBIENT_NAMESPACE, name).await {
        Ok((_, body)) => body,
        Err(_) => String::new(),
    };
    let line = serde_json::to_string(row).map_err(|e| e.to_string())?;
    body.push_str(&line);
    body.push('\n');
    store
        .write(
            AMBIENT_NAMESPACE,
            name,
            &body,
            ambient_frontmatter(name, stage),
        )
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

async fn read_stats_jsonl<T: DeserializeOwned>(store: &DurableArtifactStore, name: &str) -> Vec<T> {
    let Ok((_, body)) = store.read(AMBIENT_NAMESPACE, name).await else {
        return Vec::new();
    };
    body.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                None
            } else {
                serde_json::from_str::<T>(trimmed).ok()
            }
        })
        .collect()
}

#[derive(Debug, Default)]
struct AmbientStatsRetentionSweep {
    compacted_days: u64,
    deleted_files: u64,
    errors: Vec<String>,
}

fn ambient_stats_retention_days(env_key: &str, default_days: i64) -> i64 {
    std::env::var(env_key)
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|days| *days >= 1)
        .unwrap_or(default_days)
}

fn stats_day_exceeds_retention(day_key: &str, current_day: &str, retention_days: i64) -> bool {
    if day_key == current_day {
        return false;
    }
    let Some(day) = chrono::NaiveDate::parse_from_str(day_key, "%Y-%m-%d").ok() else {
        return false;
    };
    let Some(current) = chrono::NaiveDate::parse_from_str(current_day, "%Y-%m-%d").ok() else {
        return false;
    };
    day < current - chrono::Duration::days(retention_days.max(1))
}

async fn list_stats_partition_days(store: &DurableArtifactStore, prefix: &str) -> Vec<String> {
    let Ok(dir) = store.resolve_path_safe(AMBIENT_NAMESPACE, prefix) else {
        return Vec::new();
    };
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return Vec::new();
    };
    let mut days = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let Ok(file_type) = entry.file_type().await else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str().map(ToOwned::to_owned) else {
            continue;
        };
        let Some(day_key) = name.strip_prefix("dt=") else {
            continue;
        };
        if chrono::NaiveDate::parse_from_str(day_key, "%Y-%m-%d").is_ok() {
            days.push(day_key.to_string());
        }
    }
    days
}

fn ambient_daily_summary_value(
    day_key: &str,
    compacted_at_ms: i64,
    signal_rows: &[AmbientSignalIndexRow],
    batch_rows: &[AmbientBatchStatsRow],
    run_rows: &[AmbientDistillRunStatsRow],
    cluster_rows: &[AmbientClusterJournalRow],
) -> serde_json::Value {
    let mut bytes = AmbientByteTotals::default();
    let mut by_type = HashMap::<String, u64>::new();
    let mut by_event_kind = HashMap::<String, u64>::new();
    let mut by_origin = HashMap::<String, u64>::new();
    let mut by_sensitivity = HashMap::<String, u64>::new();
    let mut page_keys = HashSet::<String>::new();
    for row in signal_rows {
        bytes.signal_payload_bytes = bytes.signal_payload_bytes.saturating_add(row.payload_bytes);
        bytes.signal_metadata_bytes = bytes
            .signal_metadata_bytes
            .saturating_add(row.metadata_bytes);
        bytes.dom_estimated_bytes = bytes
            .dom_estimated_bytes
            .saturating_add(row.dom_estimated_bytes);
        incr(
            &mut by_type,
            row.content_type
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            1,
        );
        incr(
            &mut by_event_kind,
            row.event_kind
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            1,
        );
        incr(&mut by_origin, row.origin.clone(), 1);
        incr(
            &mut by_sensitivity,
            row.sensitivity
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            1,
        );
        if let Some(page_key) = &row.page_key {
            page_keys.insert(page_key.clone());
        }
    }

    let raw_signals = batch_rows
        .iter()
        .map(|row| row.raw_signal_count)
        .fold(0u64, |acc, value| acc.saturating_add(value));
    let accepted = batch_rows
        .iter()
        .map(|row| row.accepted_count)
        .fold(0u64, |acc, value| acc.saturating_add(value));
    let rejected = batch_rows
        .iter()
        .map(|row| row.rejected_count)
        .fold(0u64, |acc, value| acc.saturating_add(value));
    let duplicates = batch_rows
        .iter()
        .map(|row| row.duplicate_count)
        .fold(0u64, |acc, value| acc.saturating_add(value));
    let accepted_payload_bytes = batch_rows
        .iter()
        .map(|row| row.accepted_payload_bytes)
        .fold(0u64, |acc, value| acc.saturating_add(value));
    let stored_metadata_bytes = batch_rows
        .iter()
        .map(|row| row.stored_metadata_bytes)
        .fold(0u64, |acc, value| acc.saturating_add(value));
    bytes.batch_raw_bytes = batch_rows
        .iter()
        .map(|row| row.batch_raw_bytes)
        .fold(0u64, |acc, value| acc.saturating_add(value));
    if bytes.signal_payload_bytes == 0 {
        bytes.signal_payload_bytes = accepted_payload_bytes;
    }
    if bytes.signal_metadata_bytes == 0 {
        bytes.signal_metadata_bytes = stored_metadata_bytes;
    }
    let mut rejections_by_reason = HashMap::<String, u64>::new();
    for row in batch_rows {
        for (reason, count) in &row.rejections_by_reason {
            incr(&mut rejections_by_reason, reason.clone(), *count);
        }
    }

    let mut llm_calls = 0u64;
    let mut llm_input_tokens = 0u64;
    let mut llm_output_tokens = 0u64;
    let mut llm_cost_usd = 0.0f64;
    let mut evidence_created = 0u64;
    let mut memory_review_pending = 0u64;
    let mut memory_approved = 0u64;
    let mut memory_rejected = 0u64;
    let mut memory_indexed = 0u64;
    for row in cluster_rows {
        if row.state == "llm_succeeded" {
            llm_calls = llm_calls.saturating_add(1);
            if let Some(llm) = &row.llm {
                llm_input_tokens = llm_input_tokens.saturating_add(
                    llm.get("input_tokens")
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0),
                );
                llm_output_tokens = llm_output_tokens.saturating_add(
                    llm.get("output_tokens")
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0),
                );
                llm_cost_usd += llm
                    .get("cost_usd")
                    .and_then(|value| value.as_f64())
                    .unwrap_or(0.0);
            }
        }
        match row.state.as_str() {
            "evidence_created" => evidence_created = evidence_created.saturating_add(1),
            "memory_review_pending" => {
                memory_review_pending = memory_review_pending.saturating_add(1)
            },
            "memory_approved" => memory_approved = memory_approved.saturating_add(1),
            "memory_rejected" => memory_rejected = memory_rejected.saturating_add(1),
            "memory_indexed" => memory_indexed = memory_indexed.saturating_add(1),
            _ => {},
        }
    }

    serde_json::json!({
        "schema_version": ambient_schema_version(),
        "day": day_key,
        "compacted_at_ms": compacted_at_ms,
        "detail_present": {
            "signals": !signal_rows.is_empty(),
            "batches": !batch_rows.is_empty(),
            "runs": !run_rows.is_empty(),
            "clusters": !cluster_rows.is_empty(),
        },
        "summary": {
            "signals": signal_rows.len(),
            "pages": signal_rows.len(),
            "distinct_pages": page_keys.len(),
            "origins": by_origin.len(),
            "bytes": bytes,
        },
        "ingestion_funnel": {
            "received": raw_signals,
            "accepted": accepted,
            "rejected": rejected,
            "duplicates": duplicates,
            "rejections_by_reason": rejections_by_reason,
        },
        "page_metrics": {
            "total_pages": signal_rows.len(),
            "distinct_pages": page_keys.len(),
            "origins": by_origin.len(),
            "top_origins": by_origin,
        },
        "types": {
            "content_type": by_type,
            "event_kind": by_event_kind,
            "sensitivity": by_sensitivity,
        },
        "distill_pipeline": {
            "runs": run_rows.len(),
            "clusters": cluster_rows.len(),
            "evidence_created": evidence_created,
            "memory_review_pending": memory_review_pending,
            "memory_approved": memory_approved,
            "memory_rejected": memory_rejected,
            "memory_indexed": memory_indexed,
        },
        "llm": {
            "operation": AMBIENT_DISTILL_OPERATION,
            "calls": llm_calls,
            "input_tokens": llm_input_tokens,
            "output_tokens": llm_output_tokens,
            "cost_usd": llm_cost_usd,
        },
    })
}

fn preserve_daily_summary_field(
    summary: &mut serde_json::Value,
    existing: &serde_json::Value,
    field: &str,
) {
    let Some(existing_value) = existing.get(field).cloned() else {
        return;
    };
    if let Some(obj) = summary.as_object_mut() {
        obj.insert(field.to_string(), existing_value);
    }
}

fn merge_daily_summary_detail_flags(summary: &mut serde_json::Value, existing: &serde_json::Value) {
    let Some(existing_detail) = existing
        .get("detail_present")
        .and_then(|value| value.as_object())
    else {
        return;
    };
    let Some(summary_detail) = summary
        .get_mut("detail_present")
        .and_then(|value| value.as_object_mut())
    else {
        return;
    };
    for key in ["signals", "batches", "runs", "clusters"] {
        let existing_true = existing_detail
            .get(key)
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let current_true = summary_detail
            .get(key)
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        if existing_true && !current_true {
            summary_detail.insert(key.to_string(), serde_json::Value::Bool(true));
        }
    }
}

fn merge_existing_daily_summary_sections(
    summary: &mut serde_json::Value,
    existing: &serde_json::Value,
    signal_rows_empty: bool,
    batch_rows_empty: bool,
    distill_rows_empty: bool,
) {
    if signal_rows_empty {
        preserve_daily_summary_field(summary, existing, "summary");
        preserve_daily_summary_field(summary, existing, "page_metrics");
        preserve_daily_summary_field(summary, existing, "types");
    }
    if batch_rows_empty {
        preserve_daily_summary_field(summary, existing, "ingestion_funnel");
    }
    if distill_rows_empty {
        preserve_daily_summary_field(summary, existing, "distill_pipeline");
        preserve_daily_summary_field(summary, existing, "llm");
    }
    merge_daily_summary_detail_flags(summary, existing);
}

fn json_path_value<'a>(
    value: &'a serde_json::Value,
    path: &[&str],
) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

fn json_u64(value: &serde_json::Value, path: &[&str]) -> u64 {
    let Some(value) = json_path_value(value, path) else {
        return 0;
    };
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|v| u64::try_from(v).ok()))
        .unwrap_or(0)
}

fn json_f64(value: &serde_json::Value, path: &[&str]) -> f64 {
    json_path_value(value, path)
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0)
}

fn add_json_u64_map(target: &mut HashMap<String, u64>, value: Option<&serde_json::Value>) -> usize {
    let Some(obj) = value.and_then(|value| value.as_object()) else {
        return 0;
    };
    let mut added = 0usize;
    for (key, value) in obj {
        let count = value
            .as_u64()
            .or_else(|| value.as_i64().and_then(|v| u64::try_from(v).ok()))
            .unwrap_or(0);
        if key.trim().is_empty() || count == 0 {
            continue;
        }
        incr(target, key.clone(), count);
        added += 1;
    }
    added
}

fn add_json_bytes(target: &mut AmbientByteTotals, value: Option<&serde_json::Value>) {
    let Some(value) = value else {
        return;
    };
    target.batch_raw_bytes = target
        .batch_raw_bytes
        .saturating_add(json_u64(value, &["batch_raw_bytes"]));
    target.signal_payload_bytes = target
        .signal_payload_bytes
        .saturating_add(json_u64(value, &["signal_payload_bytes"]));
    target.signal_metadata_bytes = target
        .signal_metadata_bytes
        .saturating_add(json_u64(value, &["signal_metadata_bytes"]));
    target.dom_estimated_bytes = target
        .dom_estimated_bytes
        .saturating_add(json_u64(value, &["dom_estimated_bytes"]));
}

fn daily_summary_has_data(summary: &serde_json::Value) -> bool {
    json_u64(summary, &["summary", "signals"]) > 0
        || json_u64(summary, &["ingestion_funnel", "received"]) > 0
        || json_u64(summary, &["distill_pipeline", "runs"]) > 0
        || json_u64(summary, &["distill_pipeline", "clusters"]) > 0
}

#[derive(Debug, Default)]
struct AmbientStatsRangeAggregate {
    signals: u64,
    pages: u64,
    distinct_pages: u64,
    origin_count_without_map: u64,
    bytes: AmbientByteTotals,
    received: u64,
    accepted: u64,
    rejected: u64,
    duplicates: u64,
    rejections_by_reason: HashMap<String, u64>,
    by_origin: HashMap<String, u64>,
    by_type: HashMap<String, u64>,
    by_event_kind: HashMap<String, u64>,
    by_sensitivity: HashMap<String, u64>,
    runs: u64,
    clusters: u64,
    evidence_created: u64,
    memory_review_pending: u64,
    memory_approved: u64,
    memory_rejected: u64,
    memory_indexed: u64,
    llm_calls: u64,
    llm_input_tokens: u64,
    llm_output_tokens: u64,
    llm_cost_usd: f64,
}

impl AmbientStatsRangeAggregate {
    fn absorb_summary(&mut self, summary: &serde_json::Value) {
        self.signals = self
            .signals
            .saturating_add(json_u64(summary, &["summary", "signals"]));
        self.pages = self
            .pages
            .saturating_add(json_u64(summary, &["summary", "pages"]));
        let distinct_pages = json_u64(summary, &["page_metrics", "distinct_pages"])
            .max(json_u64(summary, &["summary", "distinct_pages"]));
        self.distinct_pages = self.distinct_pages.saturating_add(distinct_pages);
        add_json_bytes(
            &mut self.bytes,
            json_path_value(summary, &["summary", "bytes"]),
        );

        self.received = self
            .received
            .saturating_add(json_u64(summary, &["ingestion_funnel", "received"]));
        self.accepted = self
            .accepted
            .saturating_add(json_u64(summary, &["ingestion_funnel", "accepted"]));
        self.rejected = self
            .rejected
            .saturating_add(json_u64(summary, &["ingestion_funnel", "rejected"]));
        self.duplicates = self
            .duplicates
            .saturating_add(json_u64(summary, &["ingestion_funnel", "duplicates"]));
        add_json_u64_map(
            &mut self.rejections_by_reason,
            json_path_value(summary, &["ingestion_funnel", "rejections_by_reason"]),
        );

        let origin_entries = add_json_u64_map(
            &mut self.by_origin,
            json_path_value(summary, &["page_metrics", "top_origins"]),
        );
        if origin_entries == 0 {
            let origins = json_u64(summary, &["page_metrics", "origins"])
                .max(json_u64(summary, &["summary", "origins"]));
            self.origin_count_without_map = self.origin_count_without_map.saturating_add(origins);
        }
        add_json_u64_map(
            &mut self.by_type,
            json_path_value(summary, &["types", "content_type"]),
        );
        add_json_u64_map(
            &mut self.by_event_kind,
            json_path_value(summary, &["types", "event_kind"]),
        );
        add_json_u64_map(
            &mut self.by_sensitivity,
            json_path_value(summary, &["types", "sensitivity"]),
        );

        self.runs = self
            .runs
            .saturating_add(json_u64(summary, &["distill_pipeline", "runs"]));
        self.clusters = self
            .clusters
            .saturating_add(json_u64(summary, &["distill_pipeline", "clusters"]));
        self.evidence_created = self
            .evidence_created
            .saturating_add(json_u64(summary, &["distill_pipeline", "evidence_created"]));
        self.memory_review_pending = self.memory_review_pending.saturating_add(json_u64(
            summary,
            &["distill_pipeline", "memory_review_pending"],
        ));
        self.memory_approved = self
            .memory_approved
            .saturating_add(json_u64(summary, &["distill_pipeline", "memory_approved"]));
        self.memory_rejected = self
            .memory_rejected
            .saturating_add(json_u64(summary, &["distill_pipeline", "memory_rejected"]));
        self.memory_indexed = self
            .memory_indexed
            .saturating_add(json_u64(summary, &["distill_pipeline", "memory_indexed"]));

        self.llm_calls = self
            .llm_calls
            .saturating_add(json_u64(summary, &["llm", "calls"]));
        self.llm_input_tokens = self
            .llm_input_tokens
            .saturating_add(json_u64(summary, &["llm", "input_tokens"]));
        self.llm_output_tokens = self
            .llm_output_tokens
            .saturating_add(json_u64(summary, &["llm", "output_tokens"]));
        self.llm_cost_usd += json_f64(summary, &["llm", "cost_usd"]);
    }

    fn origins(&self) -> u64 {
        (self.by_origin.len() as u64).saturating_add(self.origin_count_without_map)
    }
}

async fn read_ambient_daily_stats_summary(
    store: &DurableArtifactStore,
    day_key: &str,
) -> Option<serde_json::Value> {
    let name = stats_daily_summary_name(day_key);
    let Ok((_, body)) = store.read(AMBIENT_NAMESPACE, &name).await else {
        return None;
    };
    serde_json::from_str::<serde_json::Value>(&body).ok()
}

async fn write_ambient_daily_stats_summary(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    store: &DurableArtifactStore,
    day_key: &str,
) -> Result<bool, String> {
    let signal_name = stats_jsonl_name(STATS_SIGNALS_PREFIX, day_key, "signals.jsonl");
    let batch_name = stats_jsonl_name(STATS_BATCHES_PREFIX, day_key, "batches.jsonl");
    let run_name = stats_jsonl_name(STATS_RUNS_PREFIX, day_key, "runs.jsonl");
    let cluster_name = stats_jsonl_name(STATS_CLUSTERS_PREFIX, day_key, "clusters.jsonl");
    let signal_rows: Vec<AmbientSignalIndexRow> = read_stats_jsonl(store, &signal_name).await;
    let batch_rows: Vec<AmbientBatchStatsRow> = read_stats_jsonl(store, &batch_name).await;
    let run_rows: Vec<AmbientDistillRunStatsRow> = read_stats_jsonl(store, &run_name).await;
    let mut cluster_rows: Vec<AmbientClusterJournalRow> =
        read_stats_jsonl(store, &cluster_name).await;
    if signal_rows.is_empty()
        && batch_rows.is_empty()
        && run_rows.is_empty()
        && cluster_rows.is_empty()
    {
        let detail_exists = [&signal_name, &batch_name, &run_name, &cluster_name]
            .iter()
            .any(|name| store.exists(AMBIENT_NAMESPACE, name));
        if detail_exists {
            return Err(format!(
                "detail partition for {day_key} exists but no JSONL rows parsed"
            ));
        }
        return Ok(false);
    }
    refresh_ambient_cluster_memory_states(
        workspace_layout,
        principal,
        workspace,
        &mut cluster_rows,
    );
    let compacted_at_ms = chrono::Utc::now().timestamp_millis();
    let name = stats_daily_summary_name(day_key);
    let mut summary = ambient_daily_summary_value(
        day_key,
        compacted_at_ms,
        &signal_rows,
        &batch_rows,
        &run_rows,
        &cluster_rows,
    );
    if let Ok((_, existing_body)) = store.read(AMBIENT_NAMESPACE, &name).await {
        if let Ok(existing) = serde_json::from_str::<serde_json::Value>(&existing_body) {
            merge_existing_daily_summary_sections(
                &mut summary,
                &existing,
                signal_rows.is_empty(),
                batch_rows.is_empty(),
                run_rows.is_empty() && cluster_rows.is_empty(),
            );
        }
    }
    let body = serde_json::to_string_pretty(&summary).map_err(|err| err.to_string())?;
    store
        .write(
            AMBIENT_NAMESPACE,
            &name,
            &body,
            ambient_frontmatter(&name, "ambient_daily_stats_summary"),
        )
        .await
        .map(|_| true)
        .map_err(|err| err.to_string())
}

async fn delete_stats_partition_file(
    store: &DurableArtifactStore,
    prefix: &str,
    day_key: &str,
    file_name: &str,
) -> Result<bool, String> {
    let name = stats_jsonl_name(prefix, day_key, file_name);
    if !store.exists(AMBIENT_NAMESPACE, &name) {
        return Ok(false);
    }
    store
        .delete(AMBIENT_NAMESPACE, &name)
        .await
        .map(|_| true)
        .map_err(|err| err.to_string())
}

async fn run_ambient_stats_retention_sweep(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    store: &DurableArtifactStore,
    current_day: &str,
) -> AmbientStatsRetentionSweep {
    let signal_retention_days = ambient_stats_retention_days(
        "AMBIENT_STATS_SIGNAL_DETAIL_RETENTION_DAYS",
        DEFAULT_STATS_SIGNAL_DETAIL_RETENTION_DAYS,
    );
    let batch_retention_days = ambient_stats_retention_days(
        "AMBIENT_STATS_BATCH_DETAIL_RETENTION_DAYS",
        DEFAULT_STATS_BATCH_DETAIL_RETENTION_DAYS,
    );
    let run_retention_days = ambient_stats_retention_days(
        "AMBIENT_STATS_RUN_DETAIL_RETENTION_DAYS",
        DEFAULT_STATS_RUN_DETAIL_RETENTION_DAYS,
    );
    let cluster_retention_days = ambient_stats_retention_days(
        "AMBIENT_STATS_CLUSTER_DETAIL_RETENTION_DAYS",
        DEFAULT_STATS_CLUSTER_DETAIL_RETENTION_DAYS,
    );

    let mut days = BTreeSet::<String>::new();
    for prefix in [
        STATS_SIGNALS_PREFIX,
        STATS_BATCHES_PREFIX,
        STATS_RUNS_PREFIX,
        STATS_CLUSTERS_PREFIX,
    ] {
        for day in list_stats_partition_days(store, prefix).await {
            days.insert(day);
        }
    }

    let mut sweep = AmbientStatsRetentionSweep::default();
    for day in days {
        let delete_signals = stats_day_exceeds_retention(&day, current_day, signal_retention_days);
        let delete_batches = stats_day_exceeds_retention(&day, current_day, batch_retention_days);
        let delete_runs = stats_day_exceeds_retention(&day, current_day, run_retention_days);
        let delete_clusters =
            stats_day_exceeds_retention(&day, current_day, cluster_retention_days);
        if !(delete_signals || delete_batches || delete_runs || delete_clusters) {
            continue;
        }

        match write_ambient_daily_stats_summary(workspace_layout, principal, workspace, store, &day)
            .await
        {
            Ok(true) => sweep.compacted_days = sweep.compacted_days.saturating_add(1),
            Ok(false) => {},
            Err(err) => {
                sweep.errors.push(format!("compact {day}: {err}"));
                continue;
            },
        }

        for (should_delete, prefix, file_name) in [
            (delete_signals, STATS_SIGNALS_PREFIX, "signals.jsonl"),
            (delete_batches, STATS_BATCHES_PREFIX, "batches.jsonl"),
            (delete_runs, STATS_RUNS_PREFIX, "runs.jsonl"),
            (delete_clusters, STATS_CLUSTERS_PREFIX, "clusters.jsonl"),
        ] {
            if !should_delete {
                continue;
            }
            match delete_stats_partition_file(store, prefix, &day, file_name).await {
                Ok(true) => sweep.deleted_files = sweep.deleted_files.saturating_add(1),
                Ok(false) => {},
                Err(err) => sweep
                    .errors
                    .push(format!("delete {prefix}/dt={day}/{file_name}: {err}")),
            }
        }
    }
    sweep
}

async fn maybe_run_ambient_stats_retention_sweep(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    store: &DurableArtifactStore,
    overview: &mut AmbientStatsOverview,
    current_day: &str,
) {
    if overview.retention_last_swept_day.as_deref() == Some(current_day) {
        return;
    }
    let sweep = run_ambient_stats_retention_sweep(
        workspace_layout,
        principal,
        workspace,
        store,
        current_day,
    )
    .await;
    overview.retention_last_swept_day = Some(current_day.to_string());
    overview.retention_last_swept_at_ms = Some(chrono::Utc::now().timestamp_millis());
    overview.retention_last_sweep_deleted_files = sweep.deleted_files;
    overview.retention_last_sweep_status = Some(if sweep.errors.is_empty() {
        format!(
            "ok: compacted {} day(s), deleted {} file(s)",
            sweep.compacted_days, sweep.deleted_files
        )
    } else {
        format!(
            "partial: compacted {} day(s), deleted {} file(s), {} error(s)",
            sweep.compacted_days,
            sweep.deleted_files,
            sweep.errors.len()
        )
    });
    for err in sweep.errors {
        warn!(error = %err, "ambient stats retention sweep issue");
    }
}

fn incr(map: &mut HashMap<String, u64>, key: impl Into<String>, amount: u64) {
    let key = key.into();
    if key.trim().is_empty() {
        return;
    }
    *map.entry(key).or_insert(0) += amount;
}

async fn load_distill_state(store: &DurableArtifactStore) -> AmbientDistillState {
    match store.read(AMBIENT_NAMESPACE, DISTILL_STATE_NAME).await {
        Ok((_, body)) => serde_json::from_str(&body).unwrap_or_default(),
        Err(_) => AmbientDistillState::default(),
    }
}

async fn save_distill_state(
    store: &DurableArtifactStore,
    state: &AmbientDistillState,
) -> Result<(), String> {
    let body = serde_json::to_string(state).map_err(|e| e.to_string())?;
    let frontmatter = DurableFrontmatter {
        namespace: AMBIENT_NAMESPACE.to_string(),
        name: DISTILL_STATE_NAME.to_string(),
        created_by: "ambient".to_string(),
        last_updated_by: "ambient".to_string(),
        last_updated: chrono::Utc::now(),
        content_type: Some("application/json".to_string()),
        source_execution_id: None,
        source_task_id: None,
        source_workflow_instance_id: None,
        source_run_id: None,
        source_cycle_id: None,
        source_agent_id: None,
        producer_stage: Some("ambient_distill_state".to_string()),
    };
    store
        .write(AMBIENT_NAMESPACE, DISTILL_STATE_NAME, &body, frontmatter)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn distill_cluster_key(cluster: &SignalCluster, day_key: &str) -> String {
    format!("{}:{day_key}", cluster.host)
}

fn distill_signal_hash(cluster: &SignalCluster) -> String {
    let mut ids = cluster.signal_ids.clone();
    ids.sort();
    blake3::hash(ids.join("\n").as_bytes()).to_hex().to_string()
}

fn cluster_is_due_for_worker(
    state: &AmbientDistillState,
    cluster: &SignalCluster,
    day_key: &str,
    now_ms: i64,
    min_cluster_interval: Duration,
) -> bool {
    let key = distill_cluster_key(cluster, day_key);
    let hash = distill_signal_hash(cluster);
    let Some(receipt) = state.receipts.iter().find(|r| r.cluster_key == key) else {
        return true;
    };
    if receipt.signal_hash == hash {
        return false;
    }
    let min_elapsed_ms = i64::try_from(min_cluster_interval.as_millis()).unwrap_or(i64::MAX);
    now_ms.saturating_sub(receipt.last_distilled_at_ms) >= min_elapsed_ms
}

fn mark_cluster_distilled(
    state: &mut AmbientDistillState,
    cluster: &SignalCluster,
    day_key: &str,
    now_ms: i64,
) {
    let key = distill_cluster_key(cluster, day_key);
    let hash = distill_signal_hash(cluster);
    match state.receipts.iter_mut().find(|r| r.cluster_key == key) {
        Some(receipt) => {
            receipt.signal_hash = hash;
            receipt.signal_count = cluster.count;
            receipt.last_distilled_at_ms = now_ms;
        },
        None => state.receipts.push(AmbientDistillReceipt {
            cluster_key: key,
            signal_hash: hash,
            signal_count: cluster.count,
            last_distilled_at_ms: now_ms,
        }),
    }
    if state.receipts.len() > MAX_DISTILL_RECEIPTS {
        let drop = state.receipts.len() - MAX_DISTILL_RECEIPTS;
        state.receipts.drain(0..drop);
    }
}

/// A content-free record of one rejected signal: id + reason + arrival time.
/// Never stores the signal's content (that is the whole point — the thing that
/// got it rejected, e.g. a secret or a denylisted origin, is not retained).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RejectionStub {
    signal_id: String,
    reason: String,
    ts_ms: i64,
}

/// Server-owned receipt ledger for cross-batch idempotency + rejection audit.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AmbientReceipts {
    /// Accepted signal ids (bounded, newest-last). Cross-batch idempotency:
    /// a signal whose id is already here is skipped on a retried batch.
    #[serde(default)]
    accepted_ids: Vec<String>,
    /// Bounded, content-free audit trail of rejected signals.
    #[serde(default)]
    rejections: Vec<RejectionStub>,
}

async fn load_receipts(store: &DurableArtifactStore) -> AmbientReceipts {
    match store.read(AMBIENT_NAMESPACE, RECEIPTS_NAME).await {
        Ok((_, body)) => serde_json::from_str(&body).unwrap_or_default(),
        Err(_) => AmbientReceipts::default(),
    }
}

async fn save_receipts(
    store: &DurableArtifactStore,
    receipts: &AmbientReceipts,
) -> Result<(), String> {
    let body = serde_json::to_string(receipts).map_err(|e| e.to_string())?;
    let frontmatter = DurableFrontmatter {
        namespace: AMBIENT_NAMESPACE.to_string(),
        name: RECEIPTS_NAME.to_string(),
        created_by: "ambient".to_string(),
        last_updated_by: "ambient".to_string(),
        last_updated: chrono::Utc::now(),
        content_type: Some("application/json".to_string()),
        source_execution_id: None,
        source_task_id: None,
        source_workflow_instance_id: None,
        source_run_id: None,
        source_cycle_id: None,
        source_agent_id: None,
        producer_stage: Some("ambient_receipts".to_string()),
    };
    store
        .write(AMBIENT_NAMESPACE, RECEIPTS_NAME, &body, frontmatter)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ─── collector token (P2.1b): principal-bound pairing ───────────────────────

/// One paired browser collector: the blake3 hash of the issued bearer token
/// (the plaintext is shown once at enrollment and never stored) + a label.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CollectorRecord {
    token_hash: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    created_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CollectorsFile {
    #[serde(default)]
    collectors: Vec<CollectorRecord>,
}

fn hash_collector_token(token: &str) -> String {
    blake3::hash(token.trim().as_bytes()).to_hex().to_string()
}

fn collector_token_header(req: &HttpRequest) -> Option<String> {
    req.headers()
        .get(COLLECTOR_TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

async fn load_collectors(store: &DurableArtifactStore) -> CollectorsFile {
    match store.read(AMBIENT_NAMESPACE, COLLECTORS_NAME).await {
        Ok((_, body)) => serde_json::from_str(&body).unwrap_or_default(),
        Err(_) => CollectorsFile::default(),
    }
}

async fn save_collectors(
    store: &DurableArtifactStore,
    file: &CollectorsFile,
) -> Result<(), String> {
    let body = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    let frontmatter = DurableFrontmatter {
        namespace: AMBIENT_NAMESPACE.to_string(),
        name: COLLECTORS_NAME.to_string(),
        created_by: "ambient".to_string(),
        last_updated_by: "ambient".to_string(),
        last_updated: chrono::Utc::now(),
        content_type: Some("application/json".to_string()),
        source_execution_id: None,
        source_task_id: None,
        source_workflow_instance_id: None,
        source_run_id: None,
        source_cycle_id: None,
        source_agent_id: None,
        producer_stage: Some("ambient_collectors".to_string()),
    };
    store
        .write(AMBIENT_NAMESPACE, COLLECTORS_NAME, &body, frontmatter)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Resolve the scope a collector token was issued for by scanning scopes for a
/// matching token hash (mirrors the enrollment resolver). The untrusted
/// extension therefore cannot self-assert a principal — the server owns the
/// token→scope binding.
async fn resolve_scope_from_collector_token(
    workspace_layout: &ArtifactV2Workspace,
    token: &str,
) -> Option<(String, String)> {
    let hash = hash_collector_token(token);
    let segments = workspace_layout.list_scope_segments().await.ok()?;
    for (principal, workspace) in segments {
        let Ok(store) = open_local_durable_artifacts(workspace_layout, &principal, &workspace)
        else {
            continue;
        };
        if load_collectors(&store)
            .await
            .collectors
            .iter()
            .any(|c| c.token_hash == hash)
        {
            return Some((principal, workspace));
        }
    }
    None
}

fn err_json(status: actix_web::http::StatusCode, message: impl std::fmt::Display) -> HttpResponse {
    HttpResponse::build(status).json(serde_json::json!({ "error": message.to_string() }))
}

fn emit_ambient_llm_call(
    broadcaster: Option<&Arc<RuntimeTransportBroadcaster>>,
    run_id: &str,
    cluster_id: &str,
    principal: &str,
    workspace: &str,
    response: &SimplifiedLLMResponse,
    success: bool,
    latency_ms: u64,
) {
    let Some(broadcaster) = broadcaster else {
        return;
    };
    let Some(tel) = response.telemetry.as_ref() else {
        return;
    };
    broadcaster.emit_transport_only(RuntimeTransportEvent::LLMResponseReceived {
        execution_id: format!("ambient:{run_id}:{cluster_id}"),
        principal: Some(principal.to_string()),
        workspace: Some(workspace.to_string()),
        correlation: tel.trace_receipt.as_ref().and_then(|receipt| {
            magician::magician_v2::realtime_events::LlmEventCorrelation::scoped(
                receipt,
                principal,
                workspace,
                magicllm::LlmWorkloadClass::Ambient,
            )
        }),
        plan_id: String::new(),
        step_id: None,
        step_index: None,
        capability: "ambient_browser".to_string(),
        success,
        decision_summary: String::new(),
        cost: tel.cost_usd,
        latency_ms,
        error: None,
        provider: tel.provider.clone(),
        model: tel.model.clone(),
        usage_reported: tel.usage_reported,
        input_tokens: tel.input_tokens,
        output_tokens: tel.output_tokens,
        reasoning_tokens: tel.reasoning_tokens,
        reasoning_summary: tel.reasoning_summary.clone(),
        cache_read_tokens: tel.cache_read_tokens,
        cache_creation_tokens: tel.cache_creation_tokens,
        audio_input_tokens: None,
        audio_output_tokens: None,
        audio_cached_tokens: None,
        search_calls: tel.search_calls,
        ttft_ms: None,
        task_id: None,
        agent_id: None,
        delegated_agent_id: None,
        chat_session_id: None,
        operation: tel
            .operation
            .clone()
            .unwrap_or_else(|| AMBIENT_DISTILL_OPERATION.to_string()),
        profile: tel.profile.clone(),
        attempt: 1,
        response_kind: "text".to_string(),
        started_at_ms: tel.started_at_ms,
        timestamp: chrono::Utc::now().timestamp_millis(),
    });
}

fn llm_json(response: &SimplifiedLLMResponse) -> Option<serde_json::Value> {
    response.telemetry.as_ref().map(|tel| {
        serde_json::json!({
            "operation": tel
                .operation
                .clone()
                .unwrap_or_else(|| AMBIENT_DISTILL_OPERATION.to_string()),
            "provider": tel.provider.clone(),
            "model": tel.model.clone(),
            "profile": tel.profile.clone(),
            "input_tokens": tel.input_tokens,
            "output_tokens": tel.output_tokens,
            "reasoning_tokens": tel.reasoning_tokens,
            "cache_read_tokens": tel.cache_read_tokens,
            "cache_creation_tokens": tel.cache_creation_tokens,
            "cost_usd": tel.cost_usd,
            "started_at_ms": tel.started_at_ms,
        })
    })
}

async fn load_config(store: &DurableArtifactStore) -> AmbientConfig {
    match store.read(AMBIENT_NAMESPACE, CONFIG_NAME).await {
        Ok((_, body)) => serde_json::from_str(&body).unwrap_or_default(),
        Err(_) => AmbientConfig::default(),
    }
}

async fn save_config(
    store: &DurableArtifactStore,
    agent_hint: &str,
    config: &AmbientConfig,
) -> Result<(), String> {
    let body = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    let frontmatter = DurableFrontmatter {
        namespace: AMBIENT_NAMESPACE.to_string(),
        name: CONFIG_NAME.to_string(),
        created_by: "ambient".to_string(),
        last_updated_by: "ambient".to_string(),
        last_updated: chrono::Utc::now(),
        content_type: Some("application/json".to_string()),
        source_execution_id: None,
        source_task_id: None,
        source_workflow_instance_id: None,
        source_run_id: None,
        source_cycle_id: None,
        source_agent_id: Some(agent_hint.to_string()),
        producer_stage: Some("ambient_config".to_string()),
    };
    store
        .write(AMBIENT_NAMESPACE, CONFIG_NAME, &body, frontmatter)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// `origin` is denylisted when it equals or is a subdomain of a list entry.
fn is_denylisted(origin: &str, denylist: &[String]) -> bool {
    let host = origin
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .to_lowercase();
    denylist.iter().any(|raw| {
        let entry = raw.trim().to_lowercase();
        !entry.is_empty()
            && (host == entry
                || host.ends_with(&format!(".{entry}"))
                || origin.to_lowercase().contains(&entry))
    })
}

/// Drop secret-class fields from the LLM-bound extracted fields before persist.
fn redact_fields(fields: &serde_json::Value) -> serde_json::Value {
    match fields {
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                let lk = k.to_lowercase();
                if SECRET_KEY_HINTS.iter().any(|h| lk.contains(h)) {
                    out.insert(k.clone(), serde_json::json!("[REDACTED]"));
                } else {
                    out.insert(k.clone(), redact_fields(v));
                }
            }
            serde_json::Value::Object(out)
        },
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(redact_fields).collect())
        },
        other => other.clone(),
    }
}

fn value_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

fn value_u64(value: &serde_json::Value, key: &str) -> Option<u64> {
    value.get(key).and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_i64().and_then(|n| u64::try_from(n).ok()))
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
    })
}

fn dom_summary_value<'a>(
    extracted_fields: &'a serde_json::Value,
    key: &str,
) -> Option<&'a serde_json::Value> {
    extracted_fields.get("dom_summary").and_then(|v| v.get(key))
}

fn value_or_dom_u64(value: &serde_json::Value, key: &str, dom_key: &str) -> Option<u64> {
    value_u64(value, key).or_else(|| {
        dom_summary_value(value, dom_key).and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_i64().and_then(|n| u64::try_from(n).ok()))
                .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u64>().ok()))
        })
    })
}

fn value_bool(value: &serde_json::Value, key: &str) -> Option<bool> {
    value.get(key).and_then(|v| {
        v.as_bool().or_else(|| {
            v.as_u64()
                .map(|n| n > 0)
                .or_else(|| v.as_i64().map(|n| n > 0))
        })
    })
}

fn canonical_page_key(
    origin: &str,
    path: Option<&str>,
    title: Option<&str>,
    dedupe: Option<&str>,
) -> String {
    let seed = dedupe
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            format!(
                "{}\n{}\n{}",
                origin.trim().to_lowercase(),
                path.unwrap_or("/").trim(),
                title.unwrap_or("").trim().to_lowercase()
            )
        });
    format!("sha256:{}", blake3::hash(seed.as_bytes()).to_hex())
}

fn count_distinct(values: impl Iterator<Item = Option<String>>) -> u64 {
    values
        .filter_map(|v| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
        .collect::<HashSet<_>>()
        .len() as u64
}

fn rows_for_cluster<'a>(
    rows: &'a [AmbientSignalRow],
    cluster: &SignalCluster,
) -> Vec<&'a AmbientSignalRow> {
    let ids: HashSet<&str> = cluster.signal_ids.iter().map(String::as_str).collect();
    rows.iter()
        .filter(|row| ids.contains(row.signal_id.as_str()))
        .collect()
}

fn cluster_byte_totals(rows: &[&AmbientSignalRow]) -> AmbientByteTotals {
    let mut totals = AmbientByteTotals::default();
    for row in rows {
        totals.signal_payload_bytes = totals
            .signal_payload_bytes
            .saturating_add(row.payload_bytes);
        totals.signal_metadata_bytes = totals
            .signal_metadata_bytes
            .saturating_add(row.metadata_bytes);
        totals.dom_estimated_bytes = totals
            .dom_estimated_bytes
            .saturating_add(row.dom_estimated_bytes);
    }
    totals
}

fn cluster_type_breakdown(rows: &[&AmbientSignalRow]) -> serde_json::Value {
    let mut content_type = HashMap::<String, u64>::new();
    let mut event_kind = HashMap::<String, u64>::new();
    let mut sensitivity = HashMap::<String, u64>::new();
    for row in rows {
        incr(
            &mut content_type,
            row.content_type
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            1,
        );
        incr(
            &mut event_kind,
            row.event_kind
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            1,
        );
        incr(
            &mut sensitivity,
            row.sensitivity
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            1,
        );
    }
    serde_json::json!({
        "content_type": content_type,
        "event_kind": event_kind,
        "sensitivity": sensitivity,
    })
}

// ─── status + config (the Observe "Observe tabs" card) ──────────────────────

#[derive(Deserialize)]
pub struct ScopeQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

pub async fn get_ambient_status_handler(
    api: web::Data<AmbientApi>,
    req: HttpRequest,
    query: web::Query<ScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let store = match open_local_durable_artifacts(&api.workspace_layout, &principal, &workspace) {
        Ok(store) => store,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let config = load_config(&store).await;
    let collectors = load_collectors(&store).await;
    let mut overview = load_stats_overview(&store).await;
    let today_key = stats_day_key_from_ms(chrono::Utc::now().timestamp_millis());
    if overview_needs_today_detail_backfill(&overview, &today_key) {
        if let Err(err) =
            refresh_today_overview_from_detail(&store, &mut overview, &today_key).await
        {
            warn!(day = %today_key, error = %err, "failed to backfill ambient stats overview");
        } else if let Err(err) = save_stats_overview(&store, &overview).await {
            warn!(day = %today_key, error = %err, "failed to save backfilled ambient stats overview");
        }
    }
    let overview_ingest_is_today =
        overview_batch_day(&overview).as_deref() == Some(today_key.as_str());
    let overview_llm_is_today = overview.llm_today_day.as_deref() == Some(today_key.as_str());
    HttpResponse::Ok().json(serde_json::json!({
        "enabled": config.enabled,
        "denylist": config.denylist,
        "total_signals": config.total_signals,
        "total_rejected": config.total_rejected,
        "last_signal_at": config.last_signal_at,
        "buffered_signals": overview.buffered_signals,
        "accepted_today": if overview_ingest_is_today {
            overview.accepted_today
        } else {
            0
        },
        "pages_today": if overview_ingest_is_today {
            overview.pages_today
        } else {
            0
        },
        "distinct_pages_today": if overview_ingest_is_today {
            overview.distinct_pages_today
        } else {
            0
        },
        "origins_today": if overview_ingest_is_today {
            overview.origins_today
        } else {
            0
        },
        "bytes_today": if overview_ingest_is_today {
            overview.bytes_today.clone()
        } else {
            AmbientByteTotals::default()
        },
        "by_type_today": if overview_ingest_is_today {
            overview.by_type_today.clone()
        } else {
            HashMap::<String, u64>::new()
        },
        "by_event_kind_today": if overview_ingest_is_today {
            overview.by_event_kind_today.clone()
        } else {
            HashMap::<String, u64>::new()
        },
        "worker": overview.worker.clone(),
        "pending": overview.pending.clone(),
        "llm_today": {
            "operation": AMBIENT_DISTILL_OPERATION,
            "calls": if overview_llm_is_today { overview.llm_calls_today } else { 0 },
            "input_tokens": if overview_llm_is_today { overview.llm_input_tokens_today } else { 0 },
            "output_tokens": if overview_llm_is_today { overview.llm_output_tokens_today } else { 0 },
            "cost_usd": if overview_llm_is_today { overview.llm_cost_usd_today } else { 0.0 },
        },
        "retention": {
            "last_swept_day": overview.retention_last_swept_day.clone(),
            "last_swept_at_ms": overview.retention_last_swept_at_ms,
            "last_sweep_status": overview.retention_last_sweep_status.clone(),
            "last_sweep_deleted_files": overview.retention_last_sweep_deleted_files,
        },
        "paired": !collectors.collectors.is_empty(),
        "collector_count": collectors.collectors.len(),
        "collectors": collectors
            .collectors
            .iter()
            .map(|c| serde_json::json!({ "label": c.label, "created_at": c.created_at }))
            .collect::<Vec<_>>(),
    }))
}

#[derive(Deserialize)]
pub struct AmbientStatsQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
}

pub async fn get_ambient_stats_handler(
    api: web::Data<AmbientApi>,
    req: HttpRequest,
    query: web::Query<AmbientStatsQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        };
    let store = match open_local_durable_artifacts(&api.workspace_layout, &principal, &workspace) {
        Ok(store) => store,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let overview = load_stats_overview(&store).await;
    let limit = query.limit.unwrap_or(50).clamp(1, 500);
    let (from_day, to_day, range_capped) =
        ambient_stats_query_range(query.from.as_deref(), query.to.as_deref());
    let day_keys = ambient_stats_day_keys(from_day, to_day);
    let mut aggregate = AmbientStatsRangeAggregate::default();
    let mut recent_signal_rows: Vec<AmbientSignalIndexRow> = Vec::new();
    let mut recent_batch_rows: Vec<AmbientBatchStatsRow> = Vec::new();
    let mut recent_run_rows: Vec<AmbientDistillRunStatsRow> = Vec::new();
    let mut recent_cluster_rows: Vec<AmbientClusterJournalRow> = Vec::new();
    let mut days_with_data = 0u64;
    let mut compacted_summary_days = 0u64;
    let mut live_detail_days = 0u64;
    let current_day = stats_day_key_from_ms(chrono::Utc::now().timestamp_millis());

    for day_key in &day_keys {
        let signal_stats_name = stats_jsonl_name(STATS_SIGNALS_PREFIX, day_key, "signals.jsonl");
        let batch_stats_name = stats_jsonl_name(STATS_BATCHES_PREFIX, day_key, "batches.jsonl");
        let run_stats_name = stats_jsonl_name(STATS_RUNS_PREFIX, day_key, "runs.jsonl");
        let cluster_stats_name = stats_jsonl_name(STATS_CLUSTERS_PREFIX, day_key, "clusters.jsonl");
        let existing_summary = read_ambient_daily_stats_summary(&store, day_key).await;
        if existing_summary.is_some() {
            compacted_summary_days = compacted_summary_days.saturating_add(1);
        }
        let read_detail = existing_summary.is_none() || day_key == &current_day;

        let signal_rows: Vec<AmbientSignalIndexRow> = if read_detail {
            read_stats_jsonl(&store, &signal_stats_name).await
        } else {
            Vec::new()
        };
        let batch_rows: Vec<AmbientBatchStatsRow> = if read_detail {
            read_stats_jsonl(&store, &batch_stats_name).await
        } else {
            Vec::new()
        };
        let run_rows: Vec<AmbientDistillRunStatsRow> = if read_detail {
            read_stats_jsonl(&store, &run_stats_name).await
        } else {
            Vec::new()
        };
        let mut cluster_rows: Vec<AmbientClusterJournalRow> = if read_detail {
            read_stats_jsonl(&store, &cluster_stats_name).await
        } else {
            Vec::new()
        };
        if !cluster_rows.is_empty() {
            refresh_ambient_cluster_memory_states(
                &api.workspace_layout,
                &principal,
                &workspace,
                &mut cluster_rows,
            );
        }

        let detail_has_rows = !signal_rows.is_empty()
            || !batch_rows.is_empty()
            || !run_rows.is_empty()
            || !cluster_rows.is_empty();

        let mut day_summary = if detail_has_rows {
            live_detail_days = live_detail_days.saturating_add(1);
            Some(ambient_daily_summary_value(
                day_key,
                chrono::Utc::now().timestamp_millis(),
                &signal_rows,
                &batch_rows,
                &run_rows,
                &cluster_rows,
            ))
        } else {
            existing_summary.clone()
        };

        if let (Some(summary), Some(existing)) = (day_summary.as_mut(), existing_summary.as_ref()) {
            merge_existing_daily_summary_sections(
                summary,
                existing,
                signal_rows.is_empty(),
                batch_rows.is_empty(),
                run_rows.is_empty() && cluster_rows.is_empty(),
            );
        }
        if let Some(summary) = day_summary {
            if daily_summary_has_data(&summary) {
                days_with_data = days_with_data.saturating_add(1);
            }
            aggregate.absorb_summary(&summary);
        }

        recent_signal_rows.extend(signal_rows);
        recent_batch_rows.extend(batch_rows);
        recent_run_rows.extend(run_rows);
        recent_cluster_rows.extend(cluster_rows);
    }

    recent_signal_rows.sort_by_key(|row| row.ts_ms);
    recent_batch_rows.sort_by_key(|row| row.received_at_ms);
    recent_run_rows.sort_by_key(|row| row.started_at_ms);
    recent_cluster_rows.sort_by_key(|row| row.updated_at_ms);
    let recent_signals: Vec<_> = recent_signal_rows.into_iter().rev().take(limit).collect();
    let recent_batches: Vec<_> = recent_batch_rows.into_iter().rev().take(limit).collect();
    let recent_runs: Vec<_> = recent_run_rows.into_iter().rev().take(limit).collect();
    let recent_clusters: Vec<_> = recent_cluster_rows.into_iter().rev().take(limit).collect();
    let origin_count = aggregate.origins();
    let response_bytes = aggregate.bytes.clone();
    let rejections_by_reason = aggregate.rejections_by_reason.clone();
    let top_origins = aggregate.by_origin.clone();
    let by_type = aggregate.by_type.clone();
    let by_event_kind = aggregate.by_event_kind.clone();
    let by_sensitivity = aggregate.by_sensitivity.clone();

    HttpResponse::Ok().json(serde_json::json!({
        "range": {
            "from_day": from_day.format("%Y-%m-%d").to_string(),
            "to_day": to_day.format("%Y-%m-%d").to_string(),
            "days": day_keys.len(),
            "days_with_data": days_with_data,
            "capped": range_capped,
            "compacted_days": compacted_summary_days,
            "live_detail_days": live_detail_days,
        },
        "summary": {
            "signals": aggregate.signals,
            "pages": aggregate.pages,
            "distinct_pages": aggregate.distinct_pages,
            "origins": origin_count,
            "bytes": response_bytes,
        },
        "ingestion_funnel": {
            "received": aggregate.received,
            "accepted": aggregate.accepted,
            "rejected": aggregate.rejected,
            "duplicates": aggregate.duplicates,
            "rejections_by_reason": rejections_by_reason,
        },
        "page_metrics": {
            "total_pages": aggregate.pages,
            "distinct_pages": aggregate.distinct_pages,
            "origins": origin_count,
            "top_origins": top_origins,
        },
        "types": {
            "content_type": by_type,
            "event_kind": by_event_kind,
            "sensitivity": by_sensitivity,
        },
        "distill_pipeline": {
            "runs": aggregate.runs,
            "clusters": aggregate.clusters,
            "evidence_created": aggregate.evidence_created,
            "memory_review_pending": aggregate.memory_review_pending,
            "memory_approved": aggregate.memory_approved,
            "memory_rejected": aggregate.memory_rejected,
            "memory_indexed": aggregate.memory_indexed,
        },
        "llm": {
            "operation": AMBIENT_DISTILL_OPERATION,
            "calls": aggregate.llm_calls,
            "input_tokens": aggregate.llm_input_tokens,
            "output_tokens": aggregate.llm_output_tokens,
            "cost_usd": aggregate.llm_cost_usd,
        },
        "retention": {
            "last_swept_day": overview.retention_last_swept_day.clone(),
            "last_swept_at_ms": overview.retention_last_swept_at_ms,
            "last_sweep_status": overview.retention_last_sweep_status.clone(),
            "last_sweep_deleted_files": overview.retention_last_sweep_deleted_files,
        },
        "recent_batches": recent_batches,
        "recent_signals": recent_signals,
        "recent_runs": recent_runs,
        "recent_clusters": recent_clusters,
    }))
}

#[derive(Deserialize)]
pub struct AmbientEnrollRequest {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
}

/// Issue a collector token bound to the caller's (header-authenticated) scope.
/// Triggered by the Observe page's "Pair browser" control. The plaintext token
/// is returned ONCE; only its hash is persisted.
pub async fn post_ambient_enroll_handler(
    api: web::Data<AmbientApi>,
    req: HttpRequest,
    body: web::Json<AmbientEnrollRequest>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(resp) => return resp,
    };
    let store = match open_local_durable_artifacts(&api.workspace_layout, &principal, &workspace) {
        Ok(store) => store,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let token = format!("ctk_{}", Uuid::new_v4().simple());
    let label = body
        .label
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "browser".to_string());
    let mut file = load_collectors(&store).await;
    file.collectors.push(CollectorRecord {
        token_hash: hash_collector_token(&token),
        label: label.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
    });
    if let Err(err) = save_collectors(&store, &file).await {
        return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err);
    }
    HttpResponse::Ok().json(serde_json::json!({
        "token": token,
        "label": label,
        "note": "Paste this token into the browser extension's pairing field. It is shown only once.",
    }))
}

#[derive(Deserialize)]
pub struct AmbientConfigUpdate {
    pub enabled: bool,
    #[serde(default)]
    pub denylist: Vec<String>,
    #[serde(default)]
    pub workspace: Option<String>,
}

pub async fn put_ambient_config_handler(
    api: web::Data<AmbientApi>,
    req: HttpRequest,
    body: web::Json<AmbientConfigUpdate>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(resp) => return resp,
    };
    let store = match open_local_durable_artifacts(&api.workspace_layout, &principal, &workspace) {
        Ok(store) => store,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let mut config = load_config(&store).await;
    config.enabled = body.enabled;
    config.denylist = body
        .denylist
        .into_iter()
        .map(|d| d.trim().to_lowercase())
        .filter(|d| !d.is_empty())
        .collect();
    if let Err(err) = save_config(&store, &principal, &config).await {
        return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err);
    }
    HttpResponse::Ok().json(serde_json::json!({
        "enabled": config.enabled,
        "denylist": config.denylist,
    }))
}

// ─── batch ingestion ────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct AmbientSignalIn {
    pub signal_id: String,
    pub origin: String,
    #[serde(default)]
    pub surface: Option<String>,
    #[serde(default)]
    pub event_kind: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub extracted_fields: serde_json::Value,
    #[serde(default)]
    pub dedupe_key: Option<String>,
    #[serde(default)]
    pub sensitivity: Option<String>,
    #[serde(default)]
    pub consent_scope: Option<String>,
    #[serde(default)]
    pub safe_url: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub page_key: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub dom_estimated_bytes: Option<u64>,
    #[serde(default)]
    pub heading_count: Option<u64>,
    #[serde(default)]
    pub has_password_field: Option<bool>,
}

#[derive(Deserialize)]
pub struct AmbientBatchRequest {
    pub signals: Vec<AmbientSignalIn>,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub client_queue_depth: Option<u64>,
}

pub async fn post_ambient_signals_batch_handler(
    api: web::Data<AmbientApi>,
    req: HttpRequest,
    body: web::Bytes,
) -> HttpResponse {
    let batch_raw_bytes = body.len() as u64;
    let body: AmbientBatchRequest = match serde_json::from_slice(&body) {
        Ok(body) => body,
        Err(err) => {
            return err_json(
                actix_web::http::StatusCode::BAD_REQUEST,
                format!("invalid ambient batch JSON: {err}"),
            )
        },
    };
    // Scope resolution: a presented collector token is authoritative (the server
    // resolves scope from it, so the extension can't self-assert a principal). An
    // invalid token is rejected; an absent token falls back to header-asserted
    // scope (local/dev path — lands in the default scope).
    let (principal, workspace) = match collector_token_header(&req) {
        Some(token) => {
            match resolve_scope_from_collector_token(&api.workspace_layout, &token).await {
                Some(scope) => scope,
                None => {
                    return err_json(
                        actix_web::http::StatusCode::UNAUTHORIZED,
                        "invalid or unknown collector token",
                    )
                },
            }
        },
        None => match resolve_required_scope(req.headers(), body.workspace.clone()) {
            Ok(scope) => scope,
            Err(resp) => return resp,
        },
    };
    let store = match open_local_durable_artifacts(&api.workspace_layout, &principal, &workspace) {
        Ok(store) => store,
        Err(err) => return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err),
    };
    let mut config = load_config(&store).await;

    // Consent gate: capture only runs when the user has enabled it.
    if !config.enabled {
        return HttpResponse::Ok().json(serde_json::json!({
            "accepted": 0,
            "rejected": body.signals.len(),
            "reason": "ambient capture is disabled for this scope",
        }));
    }

    let raw_signal_count = body.signals.len();
    let client_queue_depth = body.client_queue_depth;
    let mut receipts = load_receipts(&store).await;
    let known: HashSet<String> = receipts.accepted_ids.iter().cloned().collect();

    let batch_id = format!("amb_batch_{}", Uuid::new_v4().simple());
    let day_key = stats_day_key_from_ms(chrono::Utc::now().timestamp_millis());
    let mut accepted: Vec<String> = Vec::new();
    let mut rejected = 0usize;
    let mut duplicates = 0usize;
    let mut seen: HashSet<String> = HashSet::new();
    let mut buffered: Vec<StoredAmbientSignal> = Vec::new();
    let mut signal_index_rows: Vec<AmbientSignalIndexRow> = Vec::new();
    let mut rejections_by_reason = HashMap::<String, u64>::new();
    let mut accepted_payload_bytes = 0u64;
    let mut stored_metadata_bytes = 0u64;
    let mut dom_estimated_bytes = 0u64;
    let mut by_type_today = HashMap::<String, u64>::new();
    let mut by_event_kind_today = HashMap::<String, u64>::new();
    let mut origins = HashSet::<String>::new();
    let mut page_keys = HashSet::<String>::new();
    let mut oldest_signal_ts_ms: Option<i64> = None;
    let mut newest_signal_ts_ms: Option<i64> = None;
    let now_ms = chrono::Utc::now().timestamp_millis();

    for signal in body.signals {
        // In-batch idempotency.
        if !seen.insert(signal.signal_id.clone()) {
            duplicates += 1;
            continue;
        }
        // Cross-batch idempotency (server-owned receipt ledger): a retried upload
        // of an already-accepted signal is a silent no-op, not a re-ingest.
        if known.contains(&signal.signal_id) {
            duplicates += 1;
            continue;
        }
        // Denylist / private-window exclusion (client also excludes incognito).
        // Rejections leave a content-free audit stub (id + reason + ts), never
        // the offending content.
        let reject_reason = if signal.origin.trim().is_empty() {
            Some("empty_origin")
        } else if is_denylisted(&signal.origin, &config.denylist) {
            Some("denylist")
        } else {
            None
        };
        if let Some(reason) = reject_reason {
            rejected += 1;
            incr(&mut rejections_by_reason, reason, 1);
            receipts.rejections.push(RejectionStub {
                signal_id: signal.signal_id.clone(),
                reason: reason.to_string(),
                ts_ms: now_ms,
            });
            continue;
        }
        let sanitized = redact_fields(&signal.extracted_fields);
        let safe_url = signal
            .safe_url
            .clone()
            .or_else(|| value_string(&sanitized, "safe_url"))
            .or_else(|| value_string(&sanitized, "url"));
        let path = signal
            .path
            .clone()
            .or_else(|| value_string(&sanitized, "path"));
        let title = signal
            .title
            .clone()
            .or_else(|| value_string(&sanitized, "title"))
            .or_else(|| signal.summary.clone());
        let content_type = signal
            .content_type
            .clone()
            .or_else(|| value_string(&sanitized, "content_type"))
            .unwrap_or_else(|| "text/html".to_string());
        let page_key = signal.page_key.clone().unwrap_or_else(|| {
            canonical_page_key(
                &signal.origin,
                path.as_deref(),
                title.as_deref(),
                signal.dedupe_key.as_deref(),
            )
        });
        let heading_count = signal
            .heading_count
            .or_else(|| value_or_dom_u64(&sanitized, "heading_count", "headingCount"))
            .unwrap_or(0);
        let has_password_field = signal.has_password_field.unwrap_or_else(|| {
            value_bool(&sanitized, "has_password_field")
                .or_else(|| {
                    dom_summary_value(&sanitized, "passwordInputCount")
                        .and_then(|v| v.as_u64().map(|n| n > 0))
                })
                .unwrap_or(false)
        });
        let dom_bytes = signal
            .dom_estimated_bytes
            .or_else(|| value_u64(&sanitized, "dom_estimated_bytes"))
            .unwrap_or(0);
        let payload_value = serde_json::json!({
            "signal_id": signal.signal_id,
            "origin": signal.origin,
            "surface": signal.surface,
            "event_kind": signal.event_kind,
            "summary": signal.summary,
            "extracted_fields": sanitized,
            "dedupe_key": signal.dedupe_key,
            "sensitivity": signal.sensitivity,
            "consent_scope": signal.consent_scope,
            "safe_url": safe_url.clone(),
            "path": path.clone(),
            "title": title.clone(),
            "page_key": page_key.clone(),
            "content_type": content_type.clone(),
            "dom_estimated_bytes": dom_bytes,
            "heading_count": heading_count,
            "has_password_field": has_password_field,
        });
        let payload_bytes = serde_json::to_vec(&payload_value)
            .map(|bytes| bytes.len() as u64)
            .unwrap_or(0);
        let mut index_row = AmbientSignalIndexRow {
            schema_version: ambient_schema_version(),
            signal_id: payload_value["signal_id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            batch_id: batch_id.clone(),
            ts_ms: now_ms,
            origin: payload_value["origin"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            safe_url: safe_url.clone(),
            path: path.clone(),
            title: title.clone(),
            page_key: Some(page_key.clone()),
            surface: payload_value["surface"].as_str().map(ToOwned::to_owned),
            event_kind: payload_value["event_kind"].as_str().map(ToOwned::to_owned),
            content_type: Some(content_type.clone()),
            sensitivity: payload_value["sensitivity"].as_str().map(ToOwned::to_owned),
            payload_bytes,
            metadata_bytes: 0,
            dom_estimated_bytes: dom_bytes,
            summary_len: payload_value["summary"]
                .as_str()
                .map(|s| s.chars().count() as u64)
                .unwrap_or(0),
            heading_count,
            has_password_field,
        };
        index_row.metadata_bytes = serde_json::to_vec(&index_row)
            .map(|bytes| bytes.len() as u64)
            .unwrap_or(0);

        buffered.push(StoredAmbientSignal {
            signal_id: index_row.signal_id.clone(),
            origin: index_row.origin.clone(),
            surface: index_row.surface.clone(),
            event_kind: index_row.event_kind.clone(),
            summary: payload_value["summary"].as_str().map(ToOwned::to_owned),
            safe_url: safe_url.clone(),
            path: path.clone(),
            title: title.clone(),
            page_key: Some(page_key.clone()),
            dedupe_key: payload_value["dedupe_key"].as_str().map(ToOwned::to_owned),
            sensitivity: index_row.sensitivity.clone(),
            content_type: Some(content_type.clone()),
            payload_bytes,
            metadata_bytes: index_row.metadata_bytes,
            dom_estimated_bytes: dom_bytes,
            has_password_field,
            heading_count,
            ts_ms: now_ms,
        });
        let event = AnalyticsEvent {
            timestamp: chrono::Utc::now(),
            event_type: "ambient_signal".to_string(),
            source: index_row.origin.clone(),
            principal: None,
            workspace: None,
            payload: payload_value,
        }
        .in_scope(&principal, &workspace);
        magician::magician_v2::analytics::emit(event);

        accepted_payload_bytes = accepted_payload_bytes.saturating_add(payload_bytes);
        stored_metadata_bytes = stored_metadata_bytes.saturating_add(index_row.metadata_bytes);
        dom_estimated_bytes = dom_estimated_bytes.saturating_add(dom_bytes);
        incr(&mut by_type_today, content_type, 1);
        incr(
            &mut by_event_kind_today,
            index_row
                .event_kind
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            1,
        );
        origins.insert(index_row.origin.clone());
        page_keys.insert(page_key);
        oldest_signal_ts_ms = Some(oldest_signal_ts_ms.map_or(now_ms, |ts| ts.min(now_ms)));
        newest_signal_ts_ms = Some(newest_signal_ts_ms.map_or(now_ms, |ts| ts.max(now_ms)));
        receipts.accepted_ids.push(index_row.signal_id.clone());
        accepted.push(index_row.signal_id.clone());
        signal_index_rows.push(index_row);
    }

    config.total_signals = config.total_signals.saturating_add(accepted.len() as u64);
    config.total_rejected = config.total_rejected.saturating_add(rejected as u64);
    if !accepted.is_empty() {
        config.last_signal_at = Some(chrono::Utc::now().to_rfc3339());
    }
    if let Err(err) = save_config(&store, &principal, &config).await {
        return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err);
    }

    // Persist the bounded receipt ledger (idempotency + rejection audit).
    if accepted.is_empty() && rejected == 0 {
        // nothing changed; skip the write
    } else {
        if receipts.accepted_ids.len() > MAX_RECEIPT_IDS {
            let drop = receipts.accepted_ids.len() - MAX_RECEIPT_IDS;
            receipts.accepted_ids.drain(0..drop);
        }
        if receipts.rejections.len() > MAX_REJECTION_STUBS {
            let drop = receipts.rejections.len() - MAX_REJECTION_STUBS;
            receipts.rejections.drain(0..drop);
        }
        if let Err(err) = save_receipts(&store, &receipts).await {
            return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err);
        }
    }

    let mut next_buffered_signals = None;
    // Append to the bounded distill buffer (the distiller's read source).
    if !buffered.is_empty() {
        let mut all = load_signal_buffer(&store).await;
        all.extend(buffered);
        if all.len() > MAX_BUFFERED_SIGNALS {
            let drop = all.len() - MAX_BUFFERED_SIGNALS;
            all.drain(0..drop);
        }
        next_buffered_signals = Some(all.len() as u64);
        if let Err(err) = save_signal_buffer(&store, &all).await {
            return err_json(actix_web::http::StatusCode::INTERNAL_SERVER_ERROR, err);
        }
    }

    let batch_row = AmbientBatchStatsRow {
        schema_version: ambient_schema_version(),
        batch_id: batch_id.clone(),
        received_at_ms: now_ms,
        raw_signal_count: raw_signal_count as u64,
        accepted_count: accepted.len() as u64,
        rejected_count: rejected as u64,
        duplicate_count: duplicates as u64,
        batch_raw_bytes,
        accepted_payload_bytes,
        stored_metadata_bytes,
        rejections_by_reason,
        oldest_signal_ts_ms,
        newest_signal_ts_ms,
        client_queue_depth,
    };
    let batch_stats_name = stats_jsonl_name(STATS_BATCHES_PREFIX, &day_key, "batches.jsonl");
    if let Err(err) =
        append_stats_jsonl(&store, &batch_stats_name, &batch_row, "ambient_batch_stats").await
    {
        warn!(error = %err, "failed to append ambient batch stats");
    }
    let signal_stats_name = stats_jsonl_name(STATS_SIGNALS_PREFIX, &day_key, "signals.jsonl");
    for row in &signal_index_rows {
        if let Err(err) =
            append_stats_jsonl(&store, &signal_stats_name, row, "ambient_signal_index").await
        {
            warn!(signal_id = %row.signal_id, error = %err, "failed to append ambient signal index");
        }
    }
    let mut overview = load_stats_overview(&store).await;
    let current_day = stats_day_key_from_ms(now_ms);
    let previous_day = overview
        .last_batch_at_ms
        .map(stats_day_key_from_ms)
        .unwrap_or_else(|| current_day.clone());
    if previous_day != current_day {
        overview.accepted_today = 0;
        overview.pages_today = 0;
        overview.distinct_pages_today = 0;
        overview.origins_today = 0;
        overview.bytes_today = AmbientByteTotals::default();
        overview.page_keys_today.clear();
        overview.origins_set_today.clear();
        overview.by_type_today.clear();
        overview.by_event_kind_today.clear();
        overview.today_detail_backfilled_day = None;
    }
    let rebuilt_today = if overview_needs_today_detail_backfill(&overview, &current_day) {
        match refresh_today_overview_from_detail(&store, &mut overview, &current_day).await {
            Ok(()) => true,
            Err(err) => {
                warn!(day = %current_day, error = %err, "failed to rebuild ambient stats overview");
                false
            },
        }
    } else {
        false
    };
    overview.schema_version = ambient_schema_version();
    overview.updated_at_ms = now_ms;
    overview.last_batch_at_ms = Some(now_ms);
    if !accepted.is_empty() {
        overview.last_signal_at_ms = Some(now_ms);
    }
    overview.total_batches = overview.total_batches.saturating_add(1);
    overview.total_signals = config.total_signals;
    overview.total_rejected = config.total_rejected;
    if let Some(buffered_signals) = next_buffered_signals {
        overview.buffered_signals = buffered_signals;
    }
    if !rebuilt_today {
        overview.accepted_today = overview
            .accepted_today
            .saturating_add(accepted.len() as u64);
        overview.pages_today = overview.pages_today.saturating_add(accepted.len() as u64);
        overview.page_keys_today.extend(page_keys.iter().cloned());
        overview.distinct_pages_today = overview.page_keys_today.len() as u64;
        overview.origins_set_today.extend(origins.iter().cloned());
        overview.origins_today = overview.origins_set_today.len() as u64;
        overview.bytes_today.add_assign(&AmbientByteTotals {
            batch_raw_bytes,
            signal_payload_bytes: accepted_payload_bytes,
            signal_metadata_bytes: stored_metadata_bytes,
            dom_estimated_bytes,
        });
        for (kind, count) in by_type_today {
            incr(&mut overview.by_type_today, kind, count);
        }
        for (kind, count) in by_event_kind_today {
            incr(&mut overview.by_event_kind_today, kind, count);
        }
    } else {
        overview.distinct_pages_today = overview.page_keys_today.len() as u64;
        overview.origins_today = overview.origins_set_today.len() as u64;
    }
    maybe_run_ambient_stats_retention_sweep(
        &api.workspace_layout,
        &principal,
        &workspace,
        &store,
        &mut overview,
        &current_day,
    )
    .await;
    if let Err(err) = save_stats_overview(&store, &overview).await {
        warn!(error = %err, "failed to update ambient stats overview");
    }

    HttpResponse::Ok().json(serde_json::json!({
        "accepted": accepted.len(),
        "rejected": rejected,
        "duplicates": duplicates,
        "batch_id": batch_id,
        "accepted_ids": accepted,
    }))
}

// ─── distillation (P2.3b): raw signals → user-owned evidence ────────────────

#[derive(Deserialize)]
pub struct AmbientDistillRequest {
    /// Look-back window in days (default 1). Bounded in practice by analytics
    /// retention, but kept explicit.
    #[serde(default)]
    pub days: Option<i64>,
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Clone)]
struct AmbientDistillRunOptions {
    days: i64,
    max_clusters: Option<usize>,
    respect_worker_receipts: bool,
    min_cluster_interval: Duration,
    source: &'static str,
}

#[derive(Debug, Default, Clone, Serialize)]
struct AmbientDistillRunOutcome {
    run_id: String,
    signals: usize,
    clusters: usize,
    promotable: usize,
    processed_clusters: usize,
    distilled: usize,
    skipped_recent: usize,
    skipped_batch_limit: usize,
    llm_started: usize,
    llm_succeeded: usize,
    llm_failed: usize,
    llm_input_tokens: u64,
    llm_output_tokens: u64,
    llm_cost_usd: f64,
    evidence_created: usize,
    memory_candidates: usize,
    review_pending: usize,
}

/// Read the scope's buffered ambient signals within the window. Reads the
/// `ambient/signals.json` buffer (not analytics.duckdb — the analytics sink holds
/// a read-write handle on that file, so a concurrent open would conflict).
async fn read_ambient_signals(
    store: &DurableArtifactStore,
    since_ms: i64,
) -> Vec<AmbientSignalRow> {
    load_signal_buffer(store)
        .await
        .into_iter()
        .filter(|s| s.ts_ms >= since_ms)
        .map(|s| AmbientSignalRow {
            signal_id: s.signal_id,
            origin: s.origin,
            surface: s.surface,
            event_kind: s.event_kind,
            summary: s.summary,
            safe_url: s.safe_url,
            path: s.path,
            title: s.title,
            page_key: s.page_key,
            dedupe_key: s.dedupe_key,
            sensitivity: s.sensitivity,
            content_type: s.content_type,
            payload_bytes: s.payload_bytes,
            metadata_bytes: s.metadata_bytes,
            dom_estimated_bytes: s.dom_estimated_bytes,
            has_password_field: s.has_password_field,
            heading_count: s.heading_count,
        })
        .collect()
}

fn cluster_journal_row(
    run_id: &str,
    cluster: &SignalCluster,
    rows: &[AmbientSignalRow],
    day_key: &str,
    state: &str,
    llm: Option<serde_json::Value>,
    evidence_id: Option<String>,
    memory: Option<serde_json::Value>,
    error: Option<String>,
) -> AmbientClusterJournalRow {
    let cluster_rows = rows_for_cluster(rows, cluster);
    let distinct_page_count = count_distinct(cluster_rows.iter().map(|row| row.page_key.clone()));
    AmbientClusterJournalRow {
        schema_version: ambient_schema_version(),
        run_id: run_id.to_string(),
        cluster_id: format!(
            "amb_cluster_{}",
            blake3::hash(format!("{}:{day_key}", cluster.host).as_bytes()).to_hex()
        ),
        cluster_key: distill_cluster_key(cluster, day_key),
        state: state.to_string(),
        updated_at_ms: chrono::Utc::now().timestamp_millis(),
        host: cluster.host.clone(),
        window_day: day_key.to_string(),
        signal_count: cluster.count as u64,
        page_count: cluster_rows.len() as u64,
        distinct_page_count,
        bytes: cluster_byte_totals(&cluster_rows),
        types: cluster_type_breakdown(&cluster_rows),
        salience: cluster.salience,
        llm,
        evidence_id,
        memory,
        error,
    }
}

fn ambient_memory_review_state(candidate_state: &str) -> &'static str {
    match candidate_state {
        "approved" | "implemented" | "evaluated" => "approved",
        "promoted" => "indexed",
        "rejected" | "superseded" | "archived" => "rejected",
        _ => "pending",
    }
}

fn ambient_memory_result_state(review_state: &str) -> &'static str {
    match review_state {
        "approved" => "memory_approved",
        "indexed" => "memory_indexed",
        "rejected" => "memory_rejected",
        _ => "memory_review_pending",
    }
}

fn refresh_ambient_cluster_memory_states(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    rows: &mut [AmbientClusterJournalRow],
) {
    let learning_scope = magician::magician_v2::learning::LearningScope::new(
        principal.to_string(),
        workspace.to_string(),
    );
    let learning_store =
        magician::magician_v2::learning::LearningStore::new(workspace_layout.clone());
    for row in rows {
        let candidate_id = row
            .memory
            .as_ref()
            .and_then(|memory| memory.get("candidate_id"))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let Some(candidate_id) = candidate_id else {
            continue;
        };
        match learning_store.read_candidate(&learning_scope, &candidate_id) {
            Ok(candidate) => {
                let candidate_state = candidate.state.as_str();
                let review_state = ambient_memory_review_state(candidate_state);
                if let Some(memory) = row.memory.as_mut().and_then(|value| value.as_object_mut()) {
                    memory.insert(
                        "candidate_state".to_string(),
                        serde_json::Value::String(candidate_state.to_string()),
                    );
                    memory.insert(
                        "review_state".to_string(),
                        serde_json::Value::String(review_state.to_string()),
                    );
                    memory.insert(
                        "result".to_string(),
                        serde_json::Value::String(
                            ambient_memory_result_state(review_state).to_string(),
                        ),
                    );
                    memory.insert(
                        "candidate_updated_at".to_string(),
                        serde_json::Value::String(candidate.updated_at.to_rfc3339()),
                    );
                    if let Some(target) = candidate.proposed_target.clone() {
                        memory.insert("target".to_string(), serde_json::Value::String(target));
                    }
                }
                row.state = ambient_memory_result_state(review_state).to_string();
            },
            Err(err) => {
                if let Some(memory) = row.memory.as_mut().and_then(|value| value.as_object_mut()) {
                    memory.insert(
                        "refresh_error".to_string(),
                        serde_json::Value::String(err.to_string()),
                    );
                }
            },
        }
    }
}

fn tabs_run_marker(run_id: &str) -> String {
    format!("ambient run: {run_id}")
}

fn ambient_tabs_summary_text(outcome: &AmbientDistillRunOutcome) -> String {
    let marker = tabs_run_marker(&outcome.run_id);
    format!(
        "Browser Tabs observation summary\n\n\
{marker}\n\n\
- Signals considered: {}\n\
- Clusters: {} total, {} promotable, {} processed\n\
- Skipped: {} recent, {} batch-limit\n\
- LLM: {} started, {} succeeded, {} failed\n\
- Evidence created: {}\n\
- Memory candidates: {} pending review\n\n\
See /observe/stats for detailed pages, bytes, types, LLM cost, and memory state.",
        outcome.signals,
        outcome.clusters,
        outcome.promotable,
        outcome.processed_clusters,
        outcome.skipped_recent,
        outcome.skipped_batch_limit,
        outcome.llm_started,
        outcome.llm_succeeded,
        outcome.llm_failed,
        outcome.evidence_created,
        outcome.review_pending,
    )
}

async fn resolve_daily_tabs_session(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    today_title: &str,
) -> anyhow::Result<(FileChatStore, ChatSession)> {
    let chat_store = FileChatStore::with_workspace_layout_index(workspace_layout.clone()).await?;
    let mut session = chat_store
        .get_or_create_active_session_with_history_lane(
            principal,
            workspace,
            TABS_THREAD_ID,
            &ChatChannel::web(),
            DEFAULT_AGENT_ID,
            magician::magician_v2::history::HistoryLane::Automated,
        )
        .await?;
    if session.title.as_deref() == Some(today_title) {
        return Ok((chat_store, session));
    }

    let has_messages = !chat_store.get_messages(&session.id, 1).await?.is_empty();
    let already_titled = session
        .title
        .as_deref()
        .is_some_and(|title| !title.trim().is_empty());
    if has_messages || already_titled || session.status == ChatSessionStatus::Archived {
        session = chat_store
            .new_session_with_history_lane(
                principal,
                workspace,
                TABS_THREAD_ID,
                &ChatChannel::web(),
                DEFAULT_AGENT_ID,
                magician::magician_v2::history::HistoryLane::Automated,
            )
            .await?;
    }
    chat_store
        .update_session_title(&session.id, today_title)
        .await?;
    session.title = Some(today_title.to_string());
    Ok((chat_store, session))
}

async fn append_daily_tabs_thread_summary(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
    outcome: &AmbientDistillRunOutcome,
) -> anyhow::Result<()> {
    if outcome.signals == 0 {
        return Ok(());
    }
    let today_title = format!("Tabs — {}", chrono::Local::now().format("%Y-%m-%d"));
    let (chat_store, session) =
        resolve_daily_tabs_session(workspace_layout, principal, workspace, &today_title).await?;
    let marker = tabs_run_marker(&outcome.run_id);
    let recent = chat_store.get_messages(&session.id, 50).await?;
    let already_posted = recent.iter().any(|message| {
        matches!(
            &message.content,
            ChatMessageContent::Text { text, .. } if text.contains(&marker)
        )
    });
    if already_posted {
        return Ok(());
    }

    let message = ChatMessage::new(
        Uuid::new_v4().to_string(),
        session.id.clone(),
        ChatMessageDirection::System,
        ChatMessageContent::Text {
            text: ambient_tabs_summary_text(outcome),
            plan_reply: None,
        },
        chrono::Utc::now().timestamp_millis(),
    )
    .with_chat_turn_id(None)
    .with_source_surface(Some(TABS_SUMMARY_SOURCE_SURFACE.to_string()))
    .with_presence_session_id(None)
    .with_voice_origin(None)
    .with_speech_segments(None);
    chat_store.append_message(&session.id, message).await?;
    Ok(())
}

async fn append_cluster_journal(
    store: &DurableArtifactStore,
    day_key: &str,
    row: &AmbientClusterJournalRow,
) {
    let name = stats_jsonl_name(STATS_CLUSTERS_PREFIX, day_key, "clusters.jsonl");
    if let Err(err) = append_stats_jsonl(store, &name, row, "ambient_cluster_journal").await {
        warn!(
            run_id = %row.run_id,
            cluster_key = %row.cluster_key,
            error = %err,
            "failed to append ambient cluster journal"
        );
    }
}

async fn append_distill_run_row(
    store: &DurableArtifactStore,
    day_key: &str,
    row: &AmbientDistillRunStatsRow,
) {
    let name = stats_jsonl_name(STATS_RUNS_PREFIX, day_key, "runs.jsonl");
    if let Err(err) = append_stats_jsonl(store, &name, row, "ambient_distill_run").await {
        warn!(run_id = %row.run_id, error = %err, "failed to append ambient distill run");
    }
}

/// One distill pass for one scope — the unit an operator reads as "the ambient
/// worker ran over this scope", with the per-cluster local model calls nested
/// underneath it.
///
/// `ambient` regardless of `options.source`. The manual `POST /ambient/distill`
/// route reaches the same function as the 15-minute worker, but what makes this
/// work ambient is the work itself: it distills passively captured browsing
/// into evidence, and nobody is blocked on the answer either way.
/// `workload_for_operation` already stamps `Ambient` on these calls in
/// `llm_dispatch_batch` on both routes, so declaring anything else here would
/// split one lane into two that cannot be joined.
///
/// `skip_all` because `options` and the resolved binding are not identifiers
/// and span fields reach a browser unredacted; only the scope is named.
#[instrument(
    name = "ambient_distill_pass",
    skip_all,
    fields(
        activity_kind = KIND_BACKGROUND,
        workload_class = WORKLOAD_AMBIENT,
        principal = %principal,
        workspace = %workspace,
    )
)]
async fn run_ambient_distill_for_scope(
    workspace_layout: &ArtifactV2Workspace,
    memory_resolver: &AgentMemoryResolver,
    event_broadcaster: Option<&Arc<RuntimeTransportBroadcaster>>,
    operation_router: &OperationLlmRouter,
    prompt_manager: &PromptManager,
    principal: &str,
    workspace: &str,
    options: AmbientDistillRunOptions,
) -> Result<AmbientDistillRunOutcome, String> {
    let scoped_operation_router =
        operation_router.with_scope_context(Some(magicllm::LlmScope::new(principal, workspace)));
    let operation_router = &scoped_operation_router;
    let binding = resolve_ambient_local_provider(operation_router).map_err(|reason| {
        format!("ambient distillation is local-only and unavailable: {reason}")
    })?;
    let days = options.days.max(1);
    let since_ms = (chrono::Utc::now() - chrono::Duration::days(days)).timestamp_millis();
    let run_id = format!("amb_run_{}", Uuid::new_v4().simple());
    let started_at_ms = chrono::Utc::now().timestamp_millis();
    let day_key = chrono::Utc::now().format("%Y-%m-%d").to_string();

    let store = open_local_durable_artifacts(workspace_layout, principal, workspace)
        .map_err(|err| err.to_string())?;
    let rows = read_ambient_signals(&store, since_ms).await;
    let pages_considered = rows.len() as u64;
    let distinct_pages_considered = count_distinct(rows.iter().map(|row| row.page_key.clone()));
    let origins_considered = rows
        .iter()
        .map(|row| row.origin.clone())
        .collect::<HashSet<_>>()
        .len() as u64;
    let mut outcome = AmbientDistillRunOutcome {
        run_id: run_id.clone(),
        signals: rows.len(),
        ..Default::default()
    };
    let mut overview = load_stats_overview(&store).await;
    overview.updated_at_ms = started_at_ms;
    overview.worker.running = true;
    overview.worker.current_run_id = Some(run_id.clone());
    overview.worker.last_tick_at_ms = Some(started_at_ms);
    overview.worker.enabled = true;
    overview.worker.lookback_days = Some(days);
    overview.worker.max_clusters_per_tick = options.max_clusters;
    overview.worker.min_cluster_interval_secs = Some(options.min_cluster_interval.as_secs());
    overview.worker.last_run_id = Some(run_id.clone());
    overview.worker.last_run_status = Some("running".to_string());
    maybe_run_ambient_stats_retention_sweep(
        workspace_layout,
        principal,
        workspace,
        &store,
        &mut overview,
        &day_key,
    )
    .await;
    let started_worker = overview.worker.clone();
    let retention_last_swept_day = overview.retention_last_swept_day.clone();
    let retention_last_swept_at_ms = overview.retention_last_swept_at_ms;
    let retention_last_sweep_status = overview.retention_last_sweep_status.clone();
    let retention_last_sweep_deleted_files = overview.retention_last_sweep_deleted_files;
    let mut latest_overview = load_stats_overview(&store).await;
    latest_overview.updated_at_ms = started_at_ms;
    latest_overview.worker = started_worker;
    latest_overview.retention_last_swept_day = retention_last_swept_day;
    latest_overview.retention_last_swept_at_ms = retention_last_swept_at_ms;
    latest_overview.retention_last_sweep_status = retention_last_sweep_status;
    latest_overview.retention_last_sweep_deleted_files = retention_last_sweep_deleted_files;
    let _ = save_stats_overview(&store, &latest_overview).await;

    if rows.is_empty() {
        let finished_at_ms = chrono::Utc::now().timestamp_millis();
        append_distill_run_row(
            &store,
            &day_key,
            &AmbientDistillRunStatsRow {
                schema_version: ambient_schema_version(),
                run_id: run_id.clone(),
                source: options.source.to_string(),
                started_at_ms,
                finished_at_ms: Some(finished_at_ms),
                status: "completed".to_string(),
                lookback_days: days,
                signals_considered: 0,
                pages_considered: 0,
                distinct_pages_considered: 0,
                origins_considered: 0,
                clusters_total: 0,
                clusters_promotable: 0,
                clusters_due: 0,
                clusters_skipped_recent: 0,
                clusters_skipped_batch_limit: 0,
                llm_started: 0,
                llm_succeeded: 0,
                llm_failed: 0,
                evidence_created: 0,
                memory_candidates: 0,
                review_pending: 0,
                errors: Vec::new(),
            },
        )
        .await;
        let mut overview = load_stats_overview(&store).await;
        overview.worker.running = false;
        overview.worker.current_run_id = None;
        overview.worker.last_run_status = Some("completed".to_string());
        overview.updated_at_ms = finished_at_ms;
        let _ = save_stats_overview(&store, &overview).await;
        return Ok(outcome);
    }

    let clusters = cluster_signals(&rows);
    let promotable: Vec<_> = clusters
        .iter()
        .filter(|c| is_cluster_salient(c))
        .cloned()
        .collect();
    outcome.clusters = clusters.len();
    outcome.promotable = promotable.len();
    append_distill_run_row(
        &store,
        &day_key,
        &AmbientDistillRunStatsRow {
            schema_version: ambient_schema_version(),
            run_id: run_id.clone(),
            source: options.source.to_string(),
            started_at_ms,
            finished_at_ms: None,
            status: "running".to_string(),
            lookback_days: days,
            signals_considered: rows.len() as u64,
            pages_considered,
            distinct_pages_considered,
            origins_considered,
            clusters_total: clusters.len() as u64,
            clusters_promotable: promotable.len() as u64,
            clusters_due: 0,
            clusters_skipped_recent: 0,
            clusters_skipped_batch_limit: 0,
            llm_started: 0,
            llm_succeeded: 0,
            llm_failed: 0,
            evidence_created: 0,
            memory_candidates: 0,
            review_pending: 0,
            errors: Vec::new(),
        },
    )
    .await;

    let memory = memory_resolver
        .resolve_for_scope(principal, workspace)
        .map_err(|err| err.to_string())?;
    let now = chrono::Utc::now().to_rfc3339();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut state = if options.respect_worker_receipts {
        load_distill_state(&store).await
    } else {
        AmbientDistillState::default()
    };
    let mut state_changed = false;

    for cluster in &promotable {
        if options
            .max_clusters
            .is_some_and(|max| outcome.processed_clusters >= max)
        {
            outcome.skipped_batch_limit += 1;
            append_cluster_journal(
                &store,
                &day_key,
                &cluster_journal_row(
                    &run_id,
                    cluster,
                    &rows,
                    &day_key,
                    "skipped_batch_limit",
                    None,
                    None,
                    None,
                    None,
                ),
            )
            .await;
            continue;
        }
        if options.respect_worker_receipts
            && !cluster_is_due_for_worker(
                &state,
                cluster,
                &day_key,
                now_ms,
                options.min_cluster_interval,
            )
        {
            outcome.skipped_recent += 1;
            append_cluster_journal(
                &store,
                &day_key,
                &cluster_journal_row(
                    &run_id,
                    cluster,
                    &rows,
                    &day_key,
                    "recent_skipped",
                    None,
                    None,
                    None,
                    None,
                ),
            )
            .await;
            continue;
        }
        append_cluster_journal(
            &store,
            &day_key,
            &cluster_journal_row(
                &run_id, cluster, &rows, &day_key, "queued", None, None, None, None,
            ),
        )
        .await;
        outcome.processed_clusters += 1;
        let running_row = cluster_journal_row(
            &run_id,
            cluster,
            &rows,
            &day_key,
            "llm_running",
            None,
            None,
            None,
            None,
        );
        let cluster_id = running_row.cluster_id.clone();
        append_cluster_journal(&store, &day_key, &running_row).await;
        outcome.llm_started += 1;
        let llm_started = Instant::now();
        let llm_outcome = match distill_ambient_cluster(
            cluster,
            operation_router,
            prompt_manager,
            &binding,
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(err) => {
                outcome.llm_failed += 1;
                append_cluster_journal(
                    &store,
                    &day_key,
                    &cluster_journal_row(
                        &run_id,
                        cluster,
                        &rows,
                        &day_key,
                        "llm_failed",
                        None,
                        None,
                        None,
                        Some(err.to_string()),
                    ),
                )
                .await;
                tracing::warn!(host = %cluster.host, error = %err, "ambient distill failed (skipped)");
                continue;
            },
        };
        let latency_ms = u64::try_from(llm_started.elapsed().as_millis()).unwrap_or(u64::MAX);
        emit_ambient_llm_call(
            event_broadcaster,
            &run_id,
            &cluster_id,
            principal,
            workspace,
            &llm_outcome.response,
            true,
            latency_ms,
        );
        outcome.llm_succeeded += 1;
        let llm = llm_json(&llm_outcome.response);
        if let Some(llm) = &llm {
            outcome.llm_input_tokens = outcome
                .llm_input_tokens
                .saturating_add(json_u64(llm, &["input_tokens"]));
            outcome.llm_output_tokens = outcome
                .llm_output_tokens
                .saturating_add(json_u64(llm, &["output_tokens"]));
            outcome.llm_cost_usd += json_f64(llm, &["cost_usd"]);
        }
        append_cluster_journal(
            &store,
            &day_key,
            &cluster_journal_row(
                &run_id,
                cluster,
                &rows,
                &day_key,
                "llm_succeeded",
                llm.clone(),
                None,
                None,
                None,
            ),
        )
        .await;
        if options.respect_worker_receipts {
            mark_cluster_distilled(&mut state, cluster, &day_key, now_ms);
            state_changed = true;
        }
        if let Some(record) = stamp_ambient_evidence(&llm_outcome.proposal, cluster, &day_key, &now)
        {
            if is_salient(&record) {
                let mut candidates = entity_candidates_from_evidence(&record);
                for c in &mut candidates {
                    c.producer = "ambient_browser".to_string();
                }
                // WEG retrieval bridge: promote this user-owned observation into a
                // review-gated `user.knowledge` memory candidate so that, once the
                // user approves it, it becomes retrievable agent memory. Routed
                // before the record is consumed by the evidence append; fail-soft.
                let learning_scope = magician::magician_v2::learning::LearningScope::new(
                    principal.to_string(),
                    workspace.to_string(),
                );
                let evidence_id = record.evidence_id.clone();
                outcome.evidence_created += 1;
                append_cluster_journal(
                    &store,
                    &day_key,
                    &cluster_journal_row(
                        &run_id,
                        cluster,
                        &rows,
                        &day_key,
                        "evidence_created",
                        llm.clone(),
                        Some(evidence_id.clone()),
                        None,
                        None,
                    ),
                )
                .await;
                let memory_result = magician::magician_v2::learning::route_user_evidence_to_memory(
                    workspace_layout,
                    &learning_scope,
                    &record,
                )
                .await
                .map(|candidate_id| {
                    if candidate_id.trim().is_empty() {
                        serde_json::json!({
                            "attempted": false,
                            "result": "sensitive_suppressed",
                            "review_required": false,
                            "target": "user.knowledge",
                        })
                    } else {
                        outcome.memory_candidates += 1;
                        outcome.review_pending += 1;
                        serde_json::json!({
                            "attempted": true,
                            "result": "memory_review_pending",
                            "candidate_id": candidate_id,
                            "review_state": "pending",
                            "review_required": true,
                            "target": "user.knowledge",
                        })
                    }
                })
                .map_err(|err| {
                    tracing::warn!(
                        evidence_id = %record.evidence_id,
                        error = %err,
                        "ambient evidence → memory candidate route failed (non-fatal, skipped)"
                    );
                    err
                });
                let memory_payload = match memory_result {
                    Ok(payload) => payload,
                    Err(err) => serde_json::json!({
                        "attempted": true,
                        "result": "failed",
                        "blocked_reason": err.to_string(),
                    }),
                };
                append_cluster_journal(
                    &store,
                    &day_key,
                    &cluster_journal_row(
                        &run_id,
                        cluster,
                        &rows,
                        &day_key,
                        memory_payload
                            .get("result")
                            .and_then(|v| v.as_str())
                            .unwrap_or("memory_route_attempted"),
                        llm.clone(),
                        Some(evidence_id),
                        Some(memory_payload.clone()),
                        None,
                    ),
                )
                .await;
                if let Err(err) = memory.append_user_work_evidence(record).await {
                    return Err(err.to_string());
                }
                if !candidates.is_empty() {
                    let _ = memory.resolve_user_work_entities(candidates).await;
                }
                outcome.distilled += 1;
            } else {
                append_cluster_journal(
                    &store,
                    &day_key,
                    &cluster_journal_row(
                        &run_id,
                        cluster,
                        &rows,
                        &day_key,
                        "evidence_skipped",
                        llm,
                        Some(record.evidence_id),
                        None,
                        Some("not_salient".to_string()),
                    ),
                )
                .await;
            }
        } else {
            append_cluster_journal(
                &store,
                &day_key,
                &cluster_journal_row(
                    &run_id,
                    cluster,
                    &rows,
                    &day_key,
                    "evidence_skipped",
                    llm,
                    None,
                    None,
                    Some("proposal_not_promoted".to_string()),
                ),
            )
            .await;
        }
    }
    if options.respect_worker_receipts {
        state.last_run_at = Some(chrono::Utc::now().to_rfc3339());
        state_changed = true;
    }
    if state_changed {
        save_distill_state(&store, &state).await?;
    }
    let finished_at_ms = chrono::Utc::now().timestamp_millis();
    append_distill_run_row(
        &store,
        &day_key,
        &AmbientDistillRunStatsRow {
            schema_version: ambient_schema_version(),
            run_id: run_id.clone(),
            source: options.source.to_string(),
            started_at_ms,
            finished_at_ms: Some(finished_at_ms),
            status: "completed".to_string(),
            lookback_days: days,
            signals_considered: rows.len() as u64,
            pages_considered,
            distinct_pages_considered,
            origins_considered,
            clusters_total: clusters.len() as u64,
            clusters_promotable: promotable.len() as u64,
            clusters_due: outcome.processed_clusters as u64,
            clusters_skipped_recent: outcome.skipped_recent as u64,
            clusters_skipped_batch_limit: outcome.skipped_batch_limit as u64,
            llm_started: outcome.llm_started as u64,
            llm_succeeded: outcome.llm_succeeded as u64,
            llm_failed: outcome.llm_failed as u64,
            evidence_created: outcome.evidence_created as u64,
            memory_candidates: outcome.memory_candidates as u64,
            review_pending: outcome.review_pending as u64,
            errors: Vec::new(),
        },
    )
    .await;
    let mut overview = load_stats_overview(&store).await;
    let refreshed_today = match refresh_today_overview_from_detail(&store, &mut overview, &day_key)
        .await
    {
        Ok(()) => true,
        Err(err) => {
            warn!(day = %day_key, error = %err, "failed to refresh ambient stats overview after distill");
            false
        },
    };
    overview.worker.running = false;
    overview.worker.current_run_id = None;
    overview.worker.last_run_status = Some("completed".to_string());
    overview.updated_at_ms = finished_at_ms;
    overview.pending.clusters_due = outcome.processed_clusters as u64;
    overview.pending.clusters_processing = 0;
    overview.pending.clusters_skipped_recent = outcome.skipped_recent as u64;
    overview.pending.clusters_failed = outcome.llm_failed as u64;
    overview.pending.review_pending = outcome.review_pending as u64;
    if !refreshed_today && overview.llm_today_day.as_deref() != Some(day_key.as_str()) {
        overview.llm_today_day = Some(day_key.clone());
        overview.llm_calls_today = 0;
        overview.llm_input_tokens_today = 0;
        overview.llm_output_tokens_today = 0;
        overview.llm_cost_usd_today = 0.0;
    }
    if !refreshed_today {
        overview.llm_calls_today = overview
            .llm_calls_today
            .saturating_add(outcome.llm_succeeded as u64);
        overview.llm_input_tokens_today = overview
            .llm_input_tokens_today
            .saturating_add(outcome.llm_input_tokens);
        overview.llm_output_tokens_today = overview
            .llm_output_tokens_today
            .saturating_add(outcome.llm_output_tokens);
        overview.llm_cost_usd_today += outcome.llm_cost_usd;
    }
    let _ = save_stats_overview(&store, &overview).await;
    if let Err(err) =
        append_daily_tabs_thread_summary(workspace_layout, principal, workspace, &outcome).await
    {
        warn!(
            run_id = %outcome.run_id,
            error = %err,
            "failed to append daily tabs thread summary"
        );
    }
    Ok(outcome)
}

async fn distill_ambient_cluster(
    cluster: &SignalCluster,
    operation_router: &OperationLlmRouter,
    prompt_manager: &PromptManager,
    binding: &VerifiedAmbientLocalBinding,
) -> anyhow::Result<AmbientDistillLlmOutcome> {
    distill_ambient_cluster_pinned_with_response(
        cluster,
        operation_router,
        prompt_manager,
        &binding.profile,
        binding.kind.clone(),
    )
    .await
}

/// Distill the window's ambient signals into user-owned evidence: cluster by
/// origin/day, salience-gate, distill promotable clusters (one local LLM call
/// each), persist to the user-owned lane (`producer = ambient_browser`).
pub async fn post_ambient_distill_handler(
    api: web::Data<AmbientApi>,
    operation_router: web::Data<OperationLlmRouter>,
    prompt_manager: web::Data<Arc<PromptManager>>,
    req: HttpRequest,
    body: web::Json<AmbientDistillRequest>,
) -> HttpResponse {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(resp) => return resp,
    };
    match run_ambient_distill_for_scope(
        &api.workspace_layout,
        &api.memory_resolver,
        api.event_broadcaster.as_ref(),
        operation_router.get_ref(),
        &**prompt_manager,
        &principal,
        &workspace,
        AmbientDistillRunOptions {
            days: body.days.unwrap_or(DEFAULT_DISTILL_LOOKBACK_DAYS).max(1),
            max_clusters: None,
            respect_worker_receipts: false,
            min_cluster_interval: Duration::ZERO,
            source: "manual",
        },
    )
    .await
    {
        Ok(outcome) => HttpResponse::Ok().json(outcome),
        Err(err) => err_json(actix_web::http::StatusCode::SERVICE_UNAVAILABLE, err),
    }
}

// ─── lifecycle worker: enabled Observe-tabs scopes → local distill ──────────

#[derive(Debug, Clone)]
pub struct AmbientDistillConfig {
    /// `AMBIENT_DISTILL_ENABLED` kill-switch. The worker still additionally
    /// checks each scope's Observe-tabs `enabled` flag on every tick.
    pub enabled: bool,
    /// `AMBIENT_DISTILL_INTERVAL_SECS` (default 15m): lifecycle distillation
    /// cadence. Browser collection remains on its independent ~30s flush.
    pub interval: Duration,
    /// `AMBIENT_DISTILL_STARTUP_DELAY_SECS` (default 3m): lets the collector
    /// land an initial batch after startup before the first distill pass.
    pub startup_delay: Duration,
    /// `AMBIENT_DISTILL_MAX_CLUSTERS_PER_TICK` (default 4): local LLM calls per
    /// scope per tick.
    pub max_clusters_per_tick: usize,
    /// `AMBIENT_DISTILL_MIN_CLUSTER_INTERVAL_SECS` (default 30m): minimum time
    /// before a changed host/day cluster can be re-distilled by the worker.
    pub min_cluster_interval: Duration,
    /// `AMBIENT_DISTILL_LOOKBACK_DAYS` (default 1): signal lookback window.
    pub lookback_days: i64,
}

impl Default for AmbientDistillConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval: Duration::from_secs(DEFAULT_DISTILL_INTERVAL_SECS),
            startup_delay: Duration::from_secs(DEFAULT_DISTILL_STARTUP_DELAY_SECS),
            max_clusters_per_tick: DEFAULT_DISTILL_MAX_CLUSTERS_PER_TICK,
            min_cluster_interval: Duration::from_secs(DEFAULT_DISTILL_MIN_CLUSTER_INTERVAL_SECS),
            lookback_days: DEFAULT_DISTILL_LOOKBACK_DAYS,
        }
    }
}

impl AmbientDistillConfig {
    pub fn from_env() -> Self {
        let mut config = Self::default();
        if let Ok(raw) = std::env::var("AMBIENT_DISTILL_ENABLED") {
            config.enabled = !matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no" | "disabled"
            );
        }
        if let Some(secs) = env_parse::<u64>("AMBIENT_DISTILL_INTERVAL_SECS") {
            if secs > 0 {
                config.interval = Duration::from_secs(secs);
            }
        }
        if let Some(secs) = env_parse::<u64>("AMBIENT_DISTILL_STARTUP_DELAY_SECS") {
            config.startup_delay = Duration::from_secs(secs);
        }
        if let Some(max) = env_parse::<usize>("AMBIENT_DISTILL_MAX_CLUSTERS_PER_TICK") {
            if max > 0 {
                config.max_clusters_per_tick = max;
            }
        }
        if let Some(secs) = env_parse::<u64>("AMBIENT_DISTILL_MIN_CLUSTER_INTERVAL_SECS") {
            config.min_cluster_interval = Duration::from_secs(secs);
        }
        if let Some(days) = env_parse::<i64>("AMBIENT_DISTILL_LOOKBACK_DAYS") {
            if days > 0 {
                config.lookback_days = days;
            }
        }
        config
    }
}

fn env_parse<T: std::str::FromStr>(name: &str) -> Option<T> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok())
}

#[derive(Debug)]
pub struct AmbientDistillWorker {
    handle: JoinHandle<()>,
    cancel: CancellationToken,
}

impl AmbientDistillWorker {
    pub fn spawn(
        workspace_layout: ArtifactV2Workspace,
        memory_resolver: AgentMemoryResolver,
        event_broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
        operation_router: Option<Arc<OperationLlmRouter>>,
        prompt_manager: Arc<PromptManager>,
        config: AmbientDistillConfig,
    ) -> Self {
        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();
        let handle = tokio::spawn(async move {
            run_ambient_distill_periodic(
                workspace_layout,
                memory_resolver,
                event_broadcaster,
                operation_router,
                prompt_manager,
                config,
                cancel_for_task,
            )
            .await;
        });
        Self { handle, cancel }
    }

    pub async fn shutdown(self) {
        self.cancel.cancel();
        let _ = self.handle.await;
    }
}

async fn run_ambient_distill_periodic(
    workspace_layout: ArtifactV2Workspace,
    memory_resolver: AgentMemoryResolver,
    event_broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
    operation_router: Option<Arc<OperationLlmRouter>>,
    prompt_manager: Arc<PromptManager>,
    config: AmbientDistillConfig,
    cancel: CancellationToken,
) {
    if !magician::magician_v2::runtime::startup::wait_for_http_or_cancel(&cancel).await {
        return;
    }
    if !config.enabled {
        info!(target: "ambient::distill_worker", "ambient distill worker disabled");
        return;
    }
    let Some(router) = operation_router else {
        warn!(
            target: "ambient::distill_worker",
            "ambient distill worker disabled because operation LLM router is unavailable"
        );
        return;
    };
    if let Err(reason) = resolve_ambient_local_provider(router.as_ref()) {
        warn!(
            target: "ambient::distill_worker",
            reason = %reason,
            "ambient distill worker disabled; ambient distillation is local-only"
        );
        return;
    }
    if !config.startup_delay.is_zero() {
        tokio::select! {
            _ = tokio::time::sleep(config.startup_delay) => {},
            _ = cancel.cancelled() => return,
        }
    }

    run_ambient_distill_tick(
        &workspace_layout,
        &memory_resolver,
        event_broadcaster.as_ref(),
        router.as_ref(),
        prompt_manager.as_ref(),
        &config,
    )
    .await;

    loop {
        tokio::select! {
            _ = tokio::time::sleep(config.interval) => {
                run_ambient_distill_tick(
                    &workspace_layout,
                    &memory_resolver,
                    event_broadcaster.as_ref(),
                    router.as_ref(),
                    prompt_manager.as_ref(),
                    &config,
                )
                .await;
            }
            _ = cancel.cancelled() => break,
        }
    }
}

async fn run_ambient_distill_tick(
    workspace_layout: &ArtifactV2Workspace,
    memory_resolver: &AgentMemoryResolver,
    event_broadcaster: Option<&Arc<RuntimeTransportBroadcaster>>,
    operation_router: &OperationLlmRouter,
    prompt_manager: &PromptManager,
    config: &AmbientDistillConfig,
) {
    // Calls produced by this tick use the bounded background lane. Do not
    // suppress the whole tick based on an instantaneous queue snapshot: under
    // continuous maintenance traffic that becomes permanent starvation.
    let scopes = match workspace_layout.list_scope_segments().await {
        Ok(scopes) => scopes,
        Err(error) => {
            warn!(
                target: "ambient::distill_worker",
                error = %error,
                "failed to list scopes for ambient distill worker"
            );
            return;
        },
    };
    let mut checked = 0usize;
    let mut enabled = 0usize;
    let mut distilled = 0usize;
    for (principal, workspace) in scopes {
        let store = match open_local_durable_artifacts(workspace_layout, &principal, &workspace) {
            Ok(store) => store,
            Err(error) => {
                warn!(
                    target: "ambient::distill_worker",
                    principal = %principal,
                    workspace = %workspace,
                    error = %error,
                    "failed to open ambient store for scope"
                );
                continue;
            },
        };
        checked += 1;
        let ambient_config = load_config(&store).await;
        if !ambient_config.enabled {
            continue;
        }
        enabled += 1;
        match run_ambient_distill_for_scope(
            workspace_layout,
            memory_resolver,
            event_broadcaster,
            operation_router,
            prompt_manager,
            &principal,
            &workspace,
            AmbientDistillRunOptions {
                days: config.lookback_days,
                max_clusters: Some(config.max_clusters_per_tick),
                respect_worker_receipts: true,
                min_cluster_interval: config.min_cluster_interval,
                source: "worker",
            },
        )
        .await
        {
            Ok(outcome) => {
                distilled += outcome.distilled;
                if outcome.distilled > 0 {
                    info!(
                        target: "ambient::distill_worker",
                        principal = %principal,
                        workspace = %workspace,
                        signals = outcome.signals,
                        clusters = outcome.clusters,
                        promotable = outcome.promotable,
                        processed_clusters = outcome.processed_clusters,
                        distilled = outcome.distilled,
                        skipped_recent = outcome.skipped_recent,
                        skipped_batch_limit = outcome.skipped_batch_limit,
                        "ambient distill worker completed scope"
                    );
                }
            },
            Err(error) => {
                warn!(
                    target: "ambient::distill_worker",
                    principal = %principal,
                    workspace = %workspace,
                    error = %error,
                    "ambient distill worker failed scope"
                );
            },
        }
    }
    debug!(
        target: "ambient::distill_worker",
        checked,
        enabled,
        distilled,
        "ambient distill worker tick complete"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cluster(signal_ids: &[&str]) -> SignalCluster {
        SignalCluster {
            host: "example.com".to_string(),
            origin: "https://example.com".to_string(),
            signal_ids: signal_ids.iter().map(|id| (*id).to_string()).collect(),
            event_kinds: vec!["page_change".to_string()],
            summaries: vec!["Example page".to_string()],
            count: signal_ids.len(),
            salience: 0.9,
        }
    }

    #[test]
    fn ambient_distill_receipts_skip_unchanged_and_delay_changed_clusters() {
        let day_key = "2026-07-06";
        let min_interval = Duration::from_secs(30 * 60);
        let mut state = AmbientDistillState::default();
        let cluster = test_cluster(&["sig-a", "sig-b"]);

        assert!(cluster_is_due_for_worker(
            &state,
            &cluster,
            day_key,
            1_000,
            min_interval,
        ));

        mark_cluster_distilled(&mut state, &cluster, day_key, 1_000);
        assert!(!cluster_is_due_for_worker(
            &state,
            &cluster,
            day_key,
            1_000 + i64::try_from(min_interval.as_millis()).unwrap() + 1,
            min_interval,
        ));

        let changed_cluster = test_cluster(&["sig-a", "sig-b", "sig-c"]);
        assert!(!cluster_is_due_for_worker(
            &state,
            &changed_cluster,
            day_key,
            1_000 + 1,
            min_interval,
        ));
        assert!(cluster_is_due_for_worker(
            &state,
            &changed_cluster,
            day_key,
            1_000 + i64::try_from(min_interval.as_millis()).unwrap() + 1,
            min_interval,
        ));
    }

    #[test]
    fn ambient_stats_query_range_swaps_and_caps_days() {
        let day = |year, month, day| chrono::NaiveDate::from_ymd_opt(year, month, day).unwrap();
        let (from_day, to_day, capped) =
            ambient_stats_query_range(Some("2026-07-06"), Some("2026-07-01"));
        assert_eq!(from_day, day(2026, 7, 1));
        assert_eq!(to_day, day(2026, 7, 6));
        assert!(!capped);

        let (from_day, to_day, capped) =
            ambient_stats_query_range(Some("2026-01-01"), Some("2026-07-06"));
        assert_eq!(to_day, day(2026, 7, 6));
        assert_eq!((to_day - from_day).num_days(), MAX_STATS_QUERY_DAYS - 1);
        assert!(capped);
    }

    #[test]
    fn ambient_stats_range_aggregate_absorbs_compacted_summary() {
        let summary = serde_json::json!({
            "summary": {
                "signals": 5,
                "pages": 5,
                "distinct_pages": 3,
                "origins": 2,
                "bytes": {
                    "batch_raw_bytes": 1000,
                    "signal_payload_bytes": 600,
                    "signal_metadata_bytes": 120,
                    "dom_estimated_bytes": 9000
                }
            },
            "ingestion_funnel": {
                "received": 6,
                "accepted": 5,
                "rejected": 1,
                "duplicates": 0,
                "rejections_by_reason": { "denylisted_origin": 1 }
            },
            "page_metrics": {
                "total_pages": 5,
                "distinct_pages": 3,
                "origins": 2,
                "top_origins": { "https://example.com": 4, "https://docs.example.com": 1 }
            },
            "types": {
                "content_type": { "text/html": 5 },
                "event_kind": { "view": 4, "route_change": 1 },
                "sensitivity": { "work": 5 }
            },
            "distill_pipeline": {
                "runs": 1,
                "clusters": 3,
                "evidence_created": 1,
                "memory_review_pending": 1,
                "memory_approved": 0,
                "memory_rejected": 0,
                "memory_indexed": 0
            },
            "llm": {
                "calls": 1,
                "input_tokens": 111,
                "output_tokens": 22,
                "cost_usd": 0.0
            }
        });
        let mut aggregate = AmbientStatsRangeAggregate::default();
        aggregate.absorb_summary(&summary);

        assert_eq!(aggregate.signals, 5);
        assert_eq!(aggregate.distinct_pages, 3);
        assert_eq!(aggregate.origins(), 2);
        assert_eq!(aggregate.bytes.signal_metadata_bytes, 120);
        assert_eq!(aggregate.received, 6);
        assert_eq!(aggregate.rejections_by_reason["denylisted_origin"], 1);
        assert_eq!(aggregate.by_type["text/html"], 5);
        assert_eq!(aggregate.llm_calls, 1);
        assert_eq!(aggregate.llm_input_tokens, 111);
        assert_eq!(aggregate.memory_review_pending, 1);
    }

    #[test]
    fn ambient_visibility_rows_do_not_serialize_raw_page_body_fields() {
        let row = AmbientSignalIndexRow {
            schema_version: ambient_schema_version(),
            signal_id: "amb_sig_test".to_string(),
            batch_id: "amb_batch_test".to_string(),
            ts_ms: 1_783_353_599_000,
            origin: "https://example.com".to_string(),
            safe_url: Some("https://example.com/docs".to_string()),
            path: Some("/docs".to_string()),
            title: Some("Example Docs".to_string()),
            page_key: Some("sha256:test".to_string()),
            surface: Some("browser".to_string()),
            event_kind: Some("view".to_string()),
            content_type: Some("text/html".to_string()),
            sensitivity: Some("work".to_string()),
            payload_bytes: 512,
            metadata_bytes: 128,
            dom_estimated_bytes: 4096,
            summary_len: 3,
            heading_count: 2,
            has_password_field: false,
        };
        let serialized = serde_json::to_string(&row).unwrap();
        for forbidden in [
            "raw_html",
            "outer_html",
            "inner_html",
            "page_body",
            "body_text",
            "document_html",
            "extracted_fields",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "serialized ambient stats row leaked forbidden field {forbidden}"
            );
        }

        let summary =
            ambient_daily_summary_value("2026-07-06", 1_783_353_600_000, &[row], &[], &[], &[]);
        let summary_body = serde_json::to_string(&summary).unwrap();
        assert!(!summary_body.contains("Example Docs"));
        assert!(!summary_body.contains("/docs"));
        assert!(!summary_body.contains("<html"));
    }
}
