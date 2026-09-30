//! Derived memory index metadata and rebuild support.
//!
//! The tier JSON files remain the source of truth. This module writes a
//! disposable JSONL document index plus manifest under `memory/index/` so later
//! retrieval backends can detect staleness before trusting derived data.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    env,
    fs::{File, OpenOptions},
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, OnceLock, RwLock,
    },
    time::{Duration, Instant, SystemTime},
};

use anyhow::{anyhow, Context, Result};
use arrow_array::{
    Array, ArrayRef, FixedSizeListArray, Float32Array, Float64Array, RecordBatch,
    RecordBatchIterator, RecordBatchReader, StringArray, UInt32Array,
};
use arrow_schema::{DataType, Field, Schema};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use futures_util::TryStreamExt;
use lancedb::{
    connect,
    index::{
        scalar::BTreeIndexBuilder, scalar::FullTextSearchQuery, vector::IvfPqIndexBuilder, Index,
        IndexConfig, IndexType,
    },
    query::{ExecutableQuery, QueryBase, Select},
    table::OptimizeAction,
    Table,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::{
    fs,
    sync::{broadcast, oneshot, Mutex},
    time::{sleep, timeout},
};
use tracing::{debug, info, warn};

use crate::definition_trait::{DefinitionLookup, MoveableDefinitionRecord};
use crate::embedding_scheduler::{acquire_embedding_permit, EmbeddingPriority};
use crate::episode_candidates::load_episode_candidate_documents;
use crate::hol_stats;
use crate::hybrid_result_cache::{
    cached_live_embedding_contract, get_or_load, hybrid_result_cache_enabled,
    invalidate_hybrid_results_for_lancedb_dir, invalidate_hybrid_results_for_root,
    publish_hybrid_index_generation, published_hybrid_index_generation, CachedHybridScore,
    HybridScoreCacheKey, MEMORY_HYBRID_SCORE_CONTRACT_VERSION,
};
use crate::key_encoding::{
    length_prefixed_key_segment, optional_key_segment, parse_length_prefixed_key_segment,
};
use crate::lance_runtime::spawn_on_lance_runtime;
use crate::lance_table_pool::{
    checkout_search_table, checkout_search_table_pair, invalidate_lance_table_pool,
    SEARCH_TABLE_NAME,
};
use crate::lancedb_scheduler::run_lancedb_search_at_scheduler_root_with_timeout;
use crate::memory_candidates::{
    load_app_memory_index_projection, load_memory_candidate_documents, MemoryCandidateDocument,
    MemoryCandidateRequest,
};
use crate::memory_hot_projections::migrate_memory_hot_projection_keys_for_scope;
use crate::memory_temperature::{
    memory_temperature_candidate_key_renames, resync_memory_temperature_overlay_full_scope,
    sync_memory_temperature_overlay, MemoryTemperatureRetentionPolicy,
};
use crate::memory_tiers::{MemoryTierDefinition, TierScope};
use crate::ollama_keep_alive;
use crate::retrieval_scope::RetrievalScope;
use crate::storage_trait::{sanitize_segment, MemoryStorage, MemoryStorageError};
use crate::vector_search_mode::{
    l2_squared_distance, notify_vector_search_ranking_changed, recall_at_k,
    served_hybrid_scoring_contract_for, vector_search_ranking_epoch, vector_search_settings,
    VectorSearchPlan, VectorSearchSettings,
};
use crate::vector_toolkit::{OllamaEmbedder, OllamaEmbedderConfig, OllamaHttpStatusError};

pub const MEMORY_INDEX_VERSION: &str = "memory-derived-index-v1";
// v9 binds persisted vectors to the exact embedding/input contract and stores
// revision-bearing candidate source hashes for race-safe score handoff.
pub const MEMORY_INDEX_SCHEMA_VERSION: u32 = 9;
pub const MEMORY_INDEX_BACKEND: &str = "lancedb-hybrid-v2";
pub const MEMORY_INDEX_DOCUMENTS_BACKEND: &str = "jsonl-direct";

const MEMORY_INDEX_SEARCH_LIMIT: usize = 128;
#[cfg(feature = "test-hash-embeddings")]
const MEMORY_TEST_HASH_EMBEDDING_DIMS: usize = 384;
const MEMORY_LANCEDB_TABLE: &str = SEARCH_TABLE_NAME;
const MEMORY_LANCEDB_CHUNK_KEY_COLUMN: &str = "chunk_key";
const MEMORY_LANCEDB_CANDIDATE_KEY_COLUMN: &str = "candidate_key";
const MEMORY_LANCEDB_ROW_HASH_COLUMN: &str = "row_hash";
const MEMORY_LANCEDB_EMBEDDING_INPUT_HASH_COLUMN: &str = "embedding_input_hash";
const MEMORY_LANCEDB_FTS_COLUMN: &str = "search_text";
const MEMORY_LANCEDB_VECTOR_COLUMN: &str = "embedding";
const MEMORY_LANCEDB_SCORE_COLUMN: &str = "_score";
const MEMORY_LANCEDB_DISTANCE_COLUMN: &str = "_distance";
const MEMORY_LANCEDB_SCOPE_COLUMN: &str = "scope";
const MEMORY_LANCEDB_AGENT_ID_COLUMN: &str = "agent_id";
const MEMORY_LANCEDB_TIER_NAME_COLUMN: &str = "tier_name";
/// k in Reciprocal Rank Fusion — the same constant (and 0-based rank
/// indexing) as lancedb's `RRFReranker` default, so the in-house hybrid
/// fusion below produces the exact scores the built-in hybrid path did.
const MEMORY_HYBRID_RRF_K: f32 = 60.0;
const MEMORY_HYBRID_SCORE_SCALE: f32 = 100.0;
const MEMORY_INDEX_SINGLE_CHUNK_CHAR_LIMIT: usize = 4_000;
const MEMORY_INDEX_CHUNK_TARGET_CHARS: usize = 2_000;
const MEMORY_INDEX_CHUNK_OVERLAP_CHARS: usize = 200;
const MEMORY_INDEX_TASK_PROGRESS_DIGEST_CHARS: usize = 4_000;
const MEMORY_INDEX_CHUNK_SEARCH_MULTIPLIER: usize = 4;
const MEMORY_EMBEDDING_CACHE_DIR: &str = "embedding_cache";
const MEMORY_EMBEDDING_CACHE_FILE_EXT: &str = "f32";
// Bump the cache namespace whenever the logical-to-physical embedding input
// contract changes. `full-input-v4` sizes physical fragments from the smaller
// of the model context and llama.cpp's physical prompt-evaluation `num_batch`.
const MEMORY_OLLAMA_EMBEDDING_INPUT_VERSION: &str = "full-input-v4";
const OLLAMA_EMBEDDING_SPECIAL_TOKEN_RESERVE: usize = 8;
const DEFAULT_OLLAMA_BASE_URL: &str = "http://127.0.0.1:11435";
const DEFAULT_OLLAMA_TIMEOUT_MS: u64 = 180_000;
const DEFAULT_OLLAMA_QUERY_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_OLLAMA_WARMUP_TIMEOUT_MS: u64 = 300_000;
const DEFAULT_OLLAMA_MAX_BATCH_CHARS: usize = 6_000;
const DEFAULT_OLLAMA_SINGLE_BATCH_RETRIES: usize = 2;
const DEFAULT_OLLAMA_RETRY_BACKOFF_MS: u64 = 1_500;
const DEFAULT_LANCEDB_RETRIEVAL_TIMEOUT_MS: u64 = 750;
const DEFAULT_LANCEDB_HEALTH_CHECK_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_LANCEDB_OPTIMIZE_TIMEOUT_MS: u64 = 60_000;
const DEFAULT_MEMORY_INDEX_WRITE_LOCK_TIMEOUT_MS: u64 = 120_000;
const DEFAULT_MEMORY_INDEX_WRITE_LOCK_POLL_MS: u64 = 100;
const MEMORY_INDEX_CHANGE_JOURNAL_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const MEMORY_INDEX_WRITE_LOCK_FILE: &str = ".lancedb-write.lock";
const MEMORY_INDEX_CHANGE_JOURNAL_LOCK_FILE: &str = ".changes.lock";
const MEMORY_INDEX_CHANGE_JOURNAL_FILE: &str = "changes.json";
const MEMORY_INDEX_CHANGE_JOURNAL_SCHEMA_VERSION: u32 = 1;
const MEMORY_LANCEDB_DELETE_KEY_BATCH_SIZE: usize = 128;
static TEMP_INDEX_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static MEMORY_INDEX_REBUILD_LOCK: Mutex<()> = Mutex::const_new(());
static MEMORY_INDEX_CHANGE_JOURNAL_LOCK: Mutex<()> = Mutex::const_new(());
static MEMORY_INDEX_READ_EPOCH: AtomicU64 = AtomicU64::new(1);

pub const MEMORY_INDEX_STALE_REASON_LANCEDB_HEALTH_CHECK_TIMED_OUT: &str =
    "lancedb_health_check_timed_out";
pub const MEMORY_INDEX_STALE_REASON_LANCEDB_HEALTH_CHECK_FAILED: &str =
    "lancedb_health_check_failed";

struct MemoryIndexFileLock {
    file: File,
    path: PathBuf,
    operation: &'static str,
}

impl Drop for MemoryIndexFileLock {
    fn drop(&mut self) {
        if let Err(error) = self.file.unlock() {
            warn!(
                target: "memory_index",
                lock_path = %self.path.display(),
                error = %error,
                operation = self.operation,
                "failed to release memory index file lock"
            );
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryIndexAgentSummary {
    pub agent_id: String,
    pub document_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryIndexManifest {
    pub index_version: String,
    pub schema_version: u32,
    pub backend: String,
    pub backend_status: String,
    #[serde(default = "default_embedding_provider")]
    pub embedding_provider: String,
    #[serde(default)]
    pub embedding_model: Option<String>,
    #[serde(default = "default_embedding_dimensions")]
    pub embedding_dimensions: usize,
    /// Exact persisted-vector compatibility identity. This includes the
    /// toolkit provider/model/dimension/context/physical-batch contract plus
    /// this index's logical input preprocessing version; execution-only tuning
    /// that cannot change a vector is excluded.
    #[serde(default)]
    pub embedding_contract_id: String,
    #[serde(default)]
    pub embedding_fallback_reason: Option<String>,
    pub principal: Option<String>,
    pub workspace: Option<String>,
    pub rebuilt_at: DateTime<Utc>,
    pub document_count: usize,
    #[serde(default)]
    pub chunk_count: usize,
    pub source_count: usize,
    pub source_hashes: BTreeMap<String, String>,
    pub agents: Vec<MemoryIndexAgentSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryIndexRebuildOutcome {
    pub manifest: MemoryIndexManifest,
    pub manifest_path: PathBuf,
    pub documents_path: PathBuf,
    pub lancedb_write: MemoryLanceDbWriteReport,
}

/// A durable, source-addressable canonical-memory change. Normal writes record
/// one of these after their canonical file is atomically committed; the index
/// maintainer can then update only the affected candidates. `FullScope` is the
/// conservative escape hatch for definition changes and unknown external paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemoryIndexChange {
    UserKnowledge,
    NativeTier {
        agent_id: String,
        tier_name: String,
        scope: TierScope,
        goal_id: Option<String>,
    },
    /// One agent's episodic surface changed. Episodes are not a declared tier
    /// and each file carries its own goal, so the whole surface for the agent
    /// is the smallest thing worth re-reading — reloading it is a directory
    /// walk, not a rebuild of the scope.
    Episodes {
        agent_id: String,
    },
    FullScope {
        reason: String,
    },
}

/// In-process notification emitted after a canonical-memory mutation has been
/// durably appended to the change journal. Derived readers use this only to
/// refresh eagerly; source-file stamps and the journal remain the correctness
/// boundary across process restarts and external writers.
#[derive(Debug, Clone)]
pub struct MemoryIndexChangeNotification {
    pub root: PathBuf,
    pub change: MemoryIndexChange,
}

fn memory_index_change_notifications() -> &'static broadcast::Sender<MemoryIndexChangeNotification>
{
    static CHANNEL: OnceLock<broadcast::Sender<MemoryIndexChangeNotification>> = OnceLock::new();
    CHANNEL.get_or_init(|| {
        let (sender, _) = broadcast::channel(256);
        sender
    })
}

pub fn subscribe_memory_index_changes() -> broadcast::Receiver<MemoryIndexChangeNotification> {
    memory_index_change_notifications().subscribe()
}

fn memory_index_read_snapshots() -> &'static RwLock<HashMap<PathBuf, Arc<MemoryIndexReadSnapshot>>>
{
    static SNAPSHOTS: OnceLock<RwLock<HashMap<PathBuf, Arc<MemoryIndexReadSnapshot>>>> =
        OnceLock::new();
    SNAPSHOTS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Derived request-path view of pending journal state. Durable files remain
/// restart and external-writer authority; this snapshot is a read accelerator.
#[derive(Debug, Clone)]
pub struct MemoryIndexReadSnapshot {
    pub epoch: u64,
    pending: MemoryIndexChangeSnapshot,
    // Retained for diagnostics/tests; request-path hits deliberately avoid statting it.
    _journal_identity: JournalFileIdentity,
}

impl MemoryIndexReadSnapshot {
    pub fn pending(&self) -> &MemoryIndexChangeSnapshot {
        &self.pending
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct JournalFileIdentity {
    len: u64,
    modified: Option<SystemTime>,
}

fn memory_index_read_snapshot_pass_through() -> bool {
    env::var("MAGICIAN_MEMORY_INDEX_READ_SNAPSHOT")
        .map(|value| value.eq_ignore_ascii_case("pass_through"))
        .unwrap_or(false)
}

async fn acquire_memory_index_change_journal_mutex() -> tokio::sync::MutexGuard<'static, ()> {
    let wait = hol_stats::OccupancyWait::journal();
    let guard = MEMORY_INDEX_CHANGE_JOURNAL_LOCK.lock().await;
    wait.finish();
    guard
}

async fn journal_file_identity(storage: &dyn MemoryStorage) -> JournalFileIdentity {
    let path = memory_index_change_journal_path(storage);
    match fs::metadata(&path).await {
        Ok(metadata) => JournalFileIdentity {
            len: metadata.len(),
            modified: metadata.modified().ok(),
        },
        Err(_) => JournalFileIdentity {
            len: 0,
            modified: None,
        },
    }
}

fn publish_memory_index_read_snapshot(
    root: PathBuf,
    pending: MemoryIndexChangeSnapshot,
    journal_identity: JournalFileIdentity,
) -> Arc<MemoryIndexReadSnapshot> {
    let snapshot = Arc::new(MemoryIndexReadSnapshot {
        epoch: MEMORY_INDEX_READ_EPOCH.fetch_add(1, Ordering::Relaxed),
        pending,
        _journal_identity: journal_identity,
    });
    match memory_index_read_snapshots().write() {
        Ok(mut guard) => {
            guard.insert(root, Arc::clone(&snapshot));
        },
        Err(poisoned) => {
            poisoned.into_inner().insert(root, Arc::clone(&snapshot));
        },
    }
    snapshot
}

fn cached_memory_index_read_snapshot(root: &Path) -> Option<Arc<MemoryIndexReadSnapshot>> {
    let guard = memory_index_read_snapshots()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.get(root).cloned()
}

#[cfg(test)]
pub fn reset_memory_index_read_snapshots_for_tests() {
    match memory_index_read_snapshots().write() {
        Ok(mut guard) => guard.clear(),
        Err(poisoned) => poisoned.into_inner().clear(),
    }
}

fn change_snapshot_from_journal(journal: &MemoryIndexChangeJournal) -> MemoryIndexChangeSnapshot {
    MemoryIndexChangeSnapshot {
        entries: journal
            .entries
            .iter()
            .map(|(key, entry)| (key.clone(), entry.generation, entry.change.clone()))
            .collect(),
    }
}

async fn lock_load_journal_and_publish(
    storage: &dyn MemoryStorage,
) -> Result<Arc<MemoryIndexReadSnapshot>> {
    hol_stats::record_read_snapshot_hydrate();
    let _guard = acquire_memory_index_change_journal_mutex().await;
    let _file_lock = acquire_memory_index_change_journal_lock(storage).await?;
    let journal = load_memory_index_change_journal(storage).await?;
    let pending = change_snapshot_from_journal(&journal);
    // Identity must be captured before releasing the journal locks. Statting
    // after unlock can pair a new file identity with an older pending set and
    // then serve that stale set as a cache hit.
    let identity = journal_file_identity(storage).await;
    Ok(publish_memory_index_read_snapshot(
        storage.root().to_path_buf(),
        pending,
        identity,
    ))
}

async fn load_memory_index_read_snapshot(
    storage: &dyn MemoryStorage,
) -> Result<Arc<MemoryIndexReadSnapshot>> {
    // Request-path hits must not stat the journal file. A metadata probe on
    // every prompt reintroduces filesystem/iCloud HOL, which is the failure
    // this snapshot exists to avoid. In-process writers publish after a
    // durable commit; a cold process hydrates from disk on the first miss.
    // External writers without an in-process `record`/`acknowledge` are not
    // live-invalidated; revision-bound score handoff remains the last guard.
    if let Some(existing) = cached_memory_index_read_snapshot(storage.root()) {
        hol_stats::record_read_snapshot_hit();
        return Ok(existing);
    }
    lock_load_journal_and_publish(storage).await
}

async fn retrieval_journal_view(
    storage: &dyn MemoryStorage,
) -> Result<Arc<MemoryIndexReadSnapshot>> {
    if memory_index_read_snapshot_pass_through() {
        return lock_load_journal_and_publish(storage).await;
    }
    load_memory_index_read_snapshot(storage).await
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MemoryIndexChangeJournalEntry {
    generation: u64,
    change: MemoryIndexChange,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MemoryIndexChangeJournal {
    schema_version: u32,
    next_generation: u64,
    #[serde(default)]
    entries: BTreeMap<String, MemoryIndexChangeJournalEntry>,
}

impl Default for MemoryIndexChangeJournal {
    fn default() -> Self {
        Self {
            schema_version: MEMORY_INDEX_CHANGE_JOURNAL_SCHEMA_VERSION,
            next_generation: 1,
            entries: BTreeMap::new(),
        }
    }
}

/// A stable snapshot of journal entries. Acknowledgement removes an entry only
/// when it still has the captured generation, so a write arriving while an
/// incremental update is in flight is never lost.
#[derive(Debug, Clone)]
pub struct MemoryIndexChangeSnapshot {
    entries: Vec<(String, u64, MemoryIndexChange)>,
}

impl MemoryIndexChangeSnapshot {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn changes(&self) -> impl Iterator<Item = &MemoryIndexChange> {
        self.entries.iter().map(|(_, _, change)| change)
    }

    /// Content identity of the pending set. Used as the result-cache
    /// pending-change component so pass-through snapshot loads that republish
    /// the same journal still hit.
    pub fn identity_token(&self) -> String {
        if self.entries.is_empty() {
            return "empty".to_string();
        }
        let mut hasher = blake3::Hasher::new();
        for (key, generation, change) in &self.entries {
            hasher.update(&(key.len() as u64).to_le_bytes());
            hasher.update(key.as_bytes());
            hasher.update(&generation.to_le_bytes());
            match serde_json::to_vec(change) {
                Ok(bytes) => {
                    hasher.update(&(bytes.len() as u64).to_le_bytes());
                    hasher.update(&bytes);
                },
                // Braced so this arm evaluates to `()` like its sibling.
                // `Hasher::update` returns `&mut Hasher` for chaining, so a
                // bare call here is an expression arm and the two disagree.
                Err(_) => {
                    hasher.update(&0u64.to_le_bytes());
                },
            }
        }
        hasher.finalize().to_hex().to_string()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryIndexIncrementalUpdateOutcome {
    pub manifest: MemoryIndexManifest,
    pub manifest_path: PathBuf,
    pub documents_path: PathBuf,
    pub lancedb_write: MemoryLanceDbWriteReport,
    pub changed_source_count: usize,
}

/// Result of attempting a source-addressable index update.
#[derive(Debug, Clone)]
pub enum MemoryIndexIncrementalUpdateResult {
    NoChanges,
    Applied(MemoryIndexIncrementalUpdateOutcome),
    FullRebuildRequired { reason: String },
}

fn memory_index_change_journal_path(storage: &dyn MemoryStorage) -> PathBuf {
    storage
        .root()
        .join("index")
        .join(MEMORY_INDEX_CHANGE_JOURNAL_FILE)
}

fn memory_index_change_key(change: &MemoryIndexChange) -> String {
    match change {
        MemoryIndexChange::UserKnowledge => "user_knowledge".to_string(),
        MemoryIndexChange::Episodes { agent_id } => {
            format!("episodes:{}", length_prefixed_key_segment(agent_id))
        },
        MemoryIndexChange::NativeTier {
            agent_id,
            tier_name,
            scope,
            goal_id,
        } => {
            // Identifiers are persisted data and can contain separator-looking
            // characters. Length-prefix every component so two distinct sources
            // can never coalesce into the same durable journal entry.
            let goal = match goal_id {
                Some(goal_id) => format!("some:{}", length_prefixed_key_segment(goal_id)),
                None => "none".to_string(),
            };
            format!(
                "tier:{}:{}:{}:{}",
                length_prefixed_key_segment(agent_id),
                length_prefixed_key_segment(scope_label(scope)),
                length_prefixed_key_segment(tier_name),
                goal,
            )
        },
        MemoryIndexChange::FullScope { .. } => "full_scope".to_string(),
    }
}

async fn load_memory_index_change_journal(
    storage: &dyn MemoryStorage,
) -> Result<MemoryIndexChangeJournal> {
    let path = memory_index_change_journal_path(storage);
    let value = match storage.read_json_value(&path).await {
        Ok(value) => value,
        Err(MemoryStorageError::Io(error)) if error.kind() == ErrorKind::NotFound => {
            return Ok(MemoryIndexChangeJournal::default());
        },
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let journal: MemoryIndexChangeJournal =
        serde_json::from_value(value).with_context(|| format!("parsing {}", path.display()))?;
    if journal.schema_version != MEMORY_INDEX_CHANGE_JOURNAL_SCHEMA_VERSION {
        anyhow::bail!(
            "memory index change journal schema {} is unsupported (expected {})",
            journal.schema_version,
            MEMORY_INDEX_CHANGE_JOURNAL_SCHEMA_VERSION
        );
    }
    Ok(journal)
}

async fn save_memory_index_change_journal(
    storage: &dyn MemoryStorage,
    journal: &MemoryIndexChangeJournal,
) -> Result<()> {
    let path = memory_index_change_journal_path(storage);
    let value =
        serde_json::to_value(journal).with_context(|| format!("serializing {}", path.display()))?;
    storage
        .write_json_value_atomic(&path, &value)
        .await
        .with_context(|| format!("writing {}", path.display()))
}

/// Record a canonical-memory mutation after its source file is committed.
/// Repeated writes to the same source coalesce by key and replace only that
/// entry's generation; unrelated sources remain independently pending.
pub async fn record_memory_index_change(
    storage: &dyn MemoryStorage,
    change: MemoryIndexChange,
) -> Result<()> {
    let _guard = acquire_memory_index_change_journal_mutex().await;
    let _file_lock = acquire_memory_index_change_journal_lock(storage).await?;
    let mut journal = load_memory_index_change_journal(storage).await?;
    let key = memory_index_change_key(&change);
    let generation = journal.next_generation.max(1);
    // Never reuse a generation. If this counter were allowed to saturate, a
    // write arriving during an in-flight snapshot could be acknowledged as if
    // it were the earlier mutation. Returning an error is deliberately
    // fail-closed: the caller still marks the scope dirty, which takes the
    // conservative full-rebuild path without losing canonical source data.
    journal.next_generation = generation.checked_add(1).context(
        "memory index change journal generation exhausted; full rebuild fallback required",
    )?;
    journal.entries.insert(
        key,
        MemoryIndexChangeJournalEntry {
            generation,
            change: change.clone(),
        },
    );
    save_memory_index_change_journal(storage, &journal).await?;
    let pending = change_snapshot_from_journal(&journal);
    let identity = journal_file_identity(storage).await;
    publish_memory_index_read_snapshot(storage.root().to_path_buf(), pending, identity);
    let _ = memory_index_change_notifications().send(MemoryIndexChangeNotification {
        root: storage.root().to_path_buf(),
        change,
    });
    Ok(())
}

/// Snapshot pending source mutations without consuming them. Callers must
/// acknowledge the snapshot only after the corresponding derived update
/// succeeds.
pub async fn snapshot_memory_index_changes(
    storage: &dyn MemoryStorage,
) -> Result<MemoryIndexChangeSnapshot> {
    let _guard = acquire_memory_index_change_journal_mutex().await;
    let _file_lock = acquire_memory_index_change_journal_lock(storage).await?;
    let journal = load_memory_index_change_journal(storage).await?;
    Ok(MemoryIndexChangeSnapshot {
        entries: journal
            .entries
            .into_iter()
            .map(|(key, entry)| (key, entry.generation, entry.change))
            .collect(),
    })
}

/// Acknowledge a successful snapshot. Entries rewritten while the update was
/// in flight carry a newer generation and are intentionally retained.
pub async fn acknowledge_memory_index_changes(
    storage: &dyn MemoryStorage,
    snapshot: &MemoryIndexChangeSnapshot,
) -> Result<()> {
    if snapshot.is_empty() {
        return Ok(());
    }
    let _guard = acquire_memory_index_change_journal_mutex().await;
    let _file_lock = acquire_memory_index_change_journal_lock(storage).await?;
    let mut journal = load_memory_index_change_journal(storage).await?;
    for (key, generation, _) in &snapshot.entries {
        if journal
            .entries
            .get(key)
            .is_some_and(|entry| entry.generation == *generation)
        {
            journal.entries.remove(key);
        }
    }
    save_memory_index_change_journal(storage, &journal).await?;
    let pending = change_snapshot_from_journal(&journal);
    let identity = journal_file_identity(storage).await;
    publish_memory_index_read_snapshot(storage.root().to_path_buf(), pending, identity);
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryLanceDbWriteReport {
    pub mode: String,
    pub chunk_count: usize,
    pub source_rows: Option<u64>,
    /// Chunks whose vectors were obtained for this write, including cache hits.
    pub embedded_rows: Option<u64>,
    /// Existing rows retained without materializing or re-embedding their vector.
    pub reused_rows: Option<u64>,
    pub inserted_rows: Option<u64>,
    pub updated_rows: Option<u64>,
    pub deleted_rows: Option<u64>,
    pub replacement_reason: Option<String>,
    pub optimize: MemoryLanceDbOptimizeReport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryLanceDbOptimizeReport {
    pub attempted: bool,
    pub completed: bool,
    pub reason: Option<String>,
    pub error: Option<String>,
    pub mutated_rows: u64,
    pub current_rows: u64,
    pub compaction_ran: bool,
    pub prune_ran: bool,
}

impl MemoryLanceDbOptimizeReport {
    fn not_attempted(reason: impl Into<String>, mutated_rows: u64, current_rows: u64) -> Self {
        Self {
            attempted: false,
            completed: false,
            reason: Some(reason.into()),
            error: None,
            mutated_rows,
            current_rows,
            compaction_ran: false,
            prune_ran: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryIndexOptimizeOutcome {
    pub optimized: bool,
    pub skipped_reason: Option<String>,
    pub index_dir: PathBuf,
    pub table_name: String,
    pub compaction_ran: bool,
    pub prune_ran: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryIndexStatus {
    pub manifest: Option<MemoryIndexManifest>,
    pub stale: bool,
    pub reason: String,
    pub current_document_count: usize,
    pub current_source_count: usize,
}

#[derive(Debug, Clone)]
pub struct MemoryHybridIndexScoreResult {
    /// Revision-bound relevance keyed by
    /// [`memory_candidate_index_score_key`]. Stable candidate identity keys
    /// alone must never be used for canonical handoff.
    pub scores: Option<BTreeMap<String, f32>>,
    pub fallback_reason: Option<String>,
    pub stale: bool,
    pub current_document_count: usize,
    /// Vector leg was requested as ANN but served exhaustive KNN. Usable for
    /// this request; the hybrid result cache must not retain it under the ANN
    /// scoring contract.
    served_ann_fallback: bool,
}

impl From<MemoryHybridIndexScoreResult> for CachedHybridScore {
    fn from(value: MemoryHybridIndexScoreResult) -> Self {
        Self {
            scores: value.scores,
            fallback_reason: value.fallback_reason,
            stale: value.stale,
            current_document_count: value.current_document_count,
            retain: !value.served_ann_fallback,
        }
    }
}

impl From<CachedHybridScore> for MemoryHybridIndexScoreResult {
    fn from(value: CachedHybridScore) -> Self {
        Self {
            scores: value.scores,
            fallback_reason: value.fallback_reason,
            stale: value.stale,
            current_document_count: value.current_document_count,
            served_ann_fallback: false,
        }
    }
}

fn memory_index_generation_token(manifest: &MemoryIndexManifest) -> String {
    let mut hasher = blake3::Hasher::new();
    let rebuilt_at = manifest.rebuilt_at.to_rfc3339();
    hasher.update(&(rebuilt_at.len() as u64).to_le_bytes());
    hasher.update(rebuilt_at.as_bytes());
    hasher.update(&manifest.document_count.to_le_bytes());
    hasher.update(&manifest.source_count.to_le_bytes());
    hasher.update(&manifest.schema_version.to_le_bytes());
    hasher.update(&(manifest.index_version.len() as u64).to_le_bytes());
    hasher.update(manifest.index_version.as_bytes());
    hasher.update(&(manifest.backend.len() as u64).to_le_bytes());
    hasher.update(manifest.backend.as_bytes());
    hasher.update(&(manifest.embedding_contract_id.len() as u64).to_le_bytes());
    hasher.update(manifest.embedding_contract_id.as_bytes());
    hasher.update(&(manifest.source_hashes.len() as u64).to_le_bytes());
    for (key, value) in &manifest.source_hashes {
        hasher.update(&(key.len() as u64).to_le_bytes());
        hasher.update(key.as_bytes());
        hasher.update(&(value.len() as u64).to_le_bytes());
        hasher.update(value.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

fn publish_memory_index_generation(storage: &dyn MemoryStorage, manifest: &MemoryIndexManifest) {
    publish_hybrid_index_generation(
        storage.root().to_path_buf(),
        memory_index_generation_token(manifest),
        manifest.rebuilt_at.timestamp_nanos_opt().unwrap_or(0),
    );
}

fn live_memory_embedding_contract_id() -> String {
    cached_live_embedding_contract(|| {
        MemoryEmbeddingConfig::from_env()
            .empty_manifest()
            .contract_id
    })
}

/// Exact physical identity used by app-memory index partitions. Endpoint is
/// part of this identity even though ordinary memory embeddings historically
/// treated it as execution-only configuration: protected app content must not
/// inherit vectors or scores across an endpoint substitution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryEmbeddingPhysicalIdentity {
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub dimensions: usize,
    pub embedding_contract_id: String,
}

impl MemoryEmbeddingPhysicalIdentity {
    pub fn partition_digest(&self) -> String {
        let dimensions = self.dimensions.to_string();
        let mut hasher = blake3::Hasher::new();
        for field in [
            "magician.memory-embedding-physical-partition.v1",
            self.provider.as_str(),
            self.model.as_str(),
            self.base_url.as_str(),
            self.embedding_contract_id.as_str(),
            dimensions.as_str(),
        ] {
            hasher.update(&(field.len() as u64).to_le_bytes());
            hasher.update(field.as_bytes());
        }
        format!("blake3:{}", hasher.finalize().to_hex())
    }
}

pub fn current_memory_embedding_physical_identity() -> MemoryEmbeddingPhysicalIdentity {
    let config = MemoryEmbeddingConfig::from_env();
    let manifest = config.empty_manifest();
    MemoryEmbeddingPhysicalIdentity {
        provider: manifest.provider,
        model: manifest.model.unwrap_or_else(|| config.model.clone()),
        base_url: config.base_url,
        dimensions: manifest.dimensions,
        embedding_contract_id: manifest.contract_id,
    }
}

fn hybrid_score_cache_key(
    storage: &dyn MemoryStorage,
    query: &str,
    predicate: Option<&str>,
    snapshot: &MemoryIndexReadSnapshot,
    index_generation: &str,
    settings: VectorSearchSettings,
    ranking_epoch: u32,
) -> HybridScoreCacheKey {
    HybridScoreCacheKey {
        storage_root: storage.root().to_path_buf(),
        predicate: predicate.unwrap_or("").to_string(),
        predicate_present: predicate.is_some(),
        expanded_query: expand_memory_retrieval_query(query),
        embedding_contract_id: live_memory_embedding_contract_id(),
        index_generation: index_generation.to_string(),
        pending_identity: snapshot.pending().identity_token(),
        search_limit: MEMORY_INDEX_SEARCH_LIMIT,
        scoring_contract: served_hybrid_scoring_contract_for(
            MEMORY_HYBRID_SCORE_CONTRACT_VERSION,
            settings.mode,
        ),
        ranking_epoch,
        candidate_multiplier: settings.candidate_multiplier,
        nprobes: settings.nprobes,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryIndexStalenessClass {
    Fresh,
    Soft,
    Hard,
}

pub fn memory_index_staleness_class(reason: &str) -> MemoryIndexStalenessClass {
    match reason {
        "fresh" => MemoryIndexStalenessClass::Fresh,
        "document_count_changed" => MemoryIndexStalenessClass::Soft,
        _ => MemoryIndexStalenessClass::Hard,
    }
}

pub fn memory_index_stale_reason_is_soft(reason: &str) -> bool {
    memory_index_staleness_class(reason) == MemoryIndexStalenessClass::Soft
}

fn memory_index_status_allows_derived_retrieval(status: &MemoryIndexStatus) -> bool {
    !status.stale || memory_index_stale_reason_is_soft(&status.reason)
}

fn memory_index_status_allows_snapshot_overlay(
    status: &MemoryIndexStatus,
    pending: &MemoryIndexChangeSnapshot,
) -> bool {
    memory_index_status_allows_derived_retrieval(status)
        || (status.reason == "pending_changes" && !pending.is_empty())
}

fn memory_index_stale_reason_is_repairable_lancedb(reason: &str) -> bool {
    matches!(
        reason,
        "missing_lancedb_table"
            | "lancedb_schema_incompatible"
            | "missing_lancedb_chunk_key_index"
            | "missing_lancedb_candidate_key_index"
            | "missing_lancedb_fts_index"
            | MEMORY_INDEX_STALE_REASON_LANCEDB_HEALTH_CHECK_FAILED
    )
}

pub fn memory_index_stale_reason_is_transient_lancedb(reason: &str) -> bool {
    reason == MEMORY_INDEX_STALE_REASON_LANCEDB_HEALTH_CHECK_TIMED_OUT
}

pub async fn rebuild_scope_memory_index(
    storage: &dyn MemoryStorage,
    definition_store: &dyn DefinitionLookup,
) -> Result<MemoryIndexRebuildOutcome> {
    let pending_changes = snapshot_memory_index_changes(storage).await?;
    let _rebuild_guard = MEMORY_INDEX_REBUILD_LOCK.lock().await;
    let _write_lock = acquire_memory_index_write_lock(storage).await?;
    let outcome = rebuild_scope_memory_index_locked(storage, definition_store, true).await?;
    acknowledge_memory_index_changes(storage, &pending_changes).await?;
    Ok(outcome)
}

/// Reconcile canonical sources without ever replacing an owned LanceDB table.
/// Runtime maintenance uses this path. It may create the first derived index
/// only when both the manifest and every LanceDB table are absent; an
/// incompatible index, an unreadable manifest, or table data without a
/// manifest requires the explicit rebuild command.
pub async fn reconcile_scope_memory_index(
    storage: &dyn MemoryStorage,
    definition_store: &dyn DefinitionLookup,
) -> Result<MemoryIndexRebuildOutcome> {
    let _rebuild_guard = MEMORY_INDEX_REBUILD_LOCK.lock().await;
    let _write_lock = acquire_memory_index_write_lock(storage).await?;
    sweep_lancedb_quarantine_dirs(storage).await;
    rebuild_scope_memory_index_locked(storage, definition_store, false).await
}

/// Bound the quarantine pile from the maintenance path, under the same locks a
/// quarantine itself takes. Pruning only when a *new* quarantine is written
/// leaves every copy made before that bound existed — or by a process that died
/// between the rename and the prune — on disk forever, because the trigger
/// stops firing exactly when the index stops corrupting. Best-effort: reclaiming
/// disk must never fail the reconcile it rides along with.
async fn sweep_lancedb_quarantine_dirs(storage: &dyn MemoryStorage) {
    let index_dir = storage.memory_lancedb_index_dir();
    let Some(parent) = index_dir.parent() else {
        return;
    };
    prune_lancedb_quarantine_dirs(parent, None).await;
}

pub async fn rebuild_scope_memory_index_with_lancedb_quarantine(
    storage: &dyn MemoryStorage,
    definition_store: &dyn DefinitionLookup,
) -> Result<MemoryIndexRebuildOutcome> {
    let pending_changes = snapshot_memory_index_changes(storage).await?;
    let _rebuild_guard = MEMORY_INDEX_REBUILD_LOCK.lock().await;
    let _write_lock = acquire_memory_index_write_lock(storage).await?;
    if let Some(quarantine_dir) = quarantine_scope_memory_lancedb_index_locked(storage).await? {
        info!(
            target: "memory_index",
            quarantine_dir = %quarantine_dir.display(),
            "quarantined LanceDB memory index before repair rebuild"
        );
    }
    let outcome = rebuild_scope_memory_index_locked(storage, definition_store, true).await?;
    acknowledge_memory_index_changes(storage, &pending_changes).await?;
    Ok(outcome)
}

/// Apply a snapshot of known canonical-memory source mutations without
/// rescanning every memory tier. The caller acknowledges `snapshot` only after
/// this returns `Applied`; unknown sources and definition-level changes return
/// `FullRebuildRequired` before mutating derived state.
pub async fn apply_memory_index_change_snapshot(
    storage: &dyn MemoryStorage,
    definition_store: &dyn DefinitionLookup,
    snapshot: &MemoryIndexChangeSnapshot,
) -> Result<MemoryIndexIncrementalUpdateResult> {
    if snapshot.is_empty() {
        return Ok(MemoryIndexIncrementalUpdateResult::NoChanges);
    }
    if snapshot
        .changes()
        .any(|change| matches!(change, MemoryIndexChange::FullScope { .. }))
    {
        return Ok(MemoryIndexIncrementalUpdateResult::FullRebuildRequired {
            reason: "journal_requested_full_scope_refresh".to_string(),
        });
    }

    let _rebuild_guard = MEMORY_INDEX_REBUILD_LOCK.lock().await;
    let _write_lock = acquire_memory_index_write_lock(storage).await?;
    let Some(previous_manifest) = load_memory_index_manifest(storage).await? else {
        return Ok(MemoryIndexIncrementalUpdateResult::FullRebuildRequired {
            reason: "missing_manifest".to_string(),
        });
    };
    let documents_path = storage.memory_index_documents_path();
    if !fs::try_exists(&documents_path).await.with_context(|| {
        format!(
            "checking memory index documents {}",
            documents_path.display()
        )
    })? {
        return Ok(MemoryIndexIncrementalUpdateResult::FullRebuildRequired {
            reason: "missing_documents".to_string(),
        });
    }

    let targets = match resolve_incremental_targets(storage, definition_store, snapshot).await? {
        Some(targets) => targets,
        None => {
            return Ok(MemoryIndexIncrementalUpdateResult::FullRebuildRequired {
                reason: "unresolvable_incremental_source".to_string(),
            });
        },
    };
    let existing_documents = load_memory_index_documents(storage).await?;
    let mut prior_target_documents = Vec::new();
    let mut retained_documents = Vec::new();
    for candidate in existing_documents {
        if targets.iter().any(|target| target.matches(&candidate)) {
            prior_target_documents.push(candidate);
        } else {
            retained_documents.push(candidate);
        }
    }

    let mut current_target_documents = Vec::new();
    for target in &targets {
        current_target_documents.extend(target.load_current_candidates(storage).await?);
    }
    current_target_documents = dedupe_memory_candidates(current_target_documents);
    let affected_candidate_keys = prior_target_documents
        .iter()
        .chain(current_target_documents.iter())
        .map(memory_candidate_index_key)
        .collect::<BTreeSet<_>>();
    if affected_candidate_keys.is_empty() {
        // A journal entry that renders no candidates still needs a conservative
        // derived-state check. Falling back here keeps a missing/corrupt table
        // repairable instead of acknowledging the source mutation while an
        // unrelated broken index remains hidden.
        return Ok(MemoryIndexIncrementalUpdateResult::FullRebuildRequired {
            reason: "empty_source_delta".to_string(),
        });
    }

    let Some(lancedb_write) = write_lancedb_index_delta(
        storage,
        &previous_manifest,
        &current_target_documents,
        &affected_candidate_keys,
    )
    .await?
    else {
        return Ok(MemoryIndexIncrementalUpdateResult::FullRebuildRequired {
            reason: "derived_index_not_incrementally_compatible".to_string(),
        });
    };

    retained_documents.extend(current_target_documents.clone());
    let merged_documents = dedupe_memory_candidates(retained_documents);
    write_documents(storage, &documents_path, &merged_documents)
        .await
        .context("writing incrementally updated memory index documents")?;
    if let Err(error) = sync_memory_temperature_overlay(storage, &current_target_documents).await {
        warn!(
            error = %error,
            "failed to sync memory temperature overlay during incremental memory index update"
        );
    }

    let mut manifest = previous_manifest;
    // Recreate the candidate hash portion from the already-loaded JSONL set.
    // This avoids encoding candidate-key parsing rules in the manifest update:
    // a mistaken prefix match could otherwise retain deleted hashes or remove an
    // unrelated source, causing persistent false staleness after a successful
    // delta. Definitions and future non-candidate sources remain untouched.
    replace_manifest_candidate_source_hashes(&mut manifest.source_hashes, &merged_documents)?;
    manifest.rebuilt_at = Utc::now();
    manifest.document_count = merged_documents.len();
    manifest.chunk_count = lancedb_write.chunk_count;
    manifest.source_count = manifest.source_hashes.len();
    manifest.agents = memory_index_agent_summaries(&merged_documents, &manifest.agents);
    let manifest_path = storage.memory_index_manifest_path();
    let manifest_value = serde_json::to_value(&manifest)
        .context("serializing incrementally updated memory index manifest")?;
    storage
        .write_json_value_atomic(&manifest_path, &manifest_value)
        .await
        .context("writing incrementally updated memory index manifest")?;
    publish_memory_index_generation(storage, &manifest);

    Ok(MemoryIndexIncrementalUpdateResult::Applied(
        MemoryIndexIncrementalUpdateOutcome {
            manifest,
            manifest_path,
            documents_path,
            lancedb_write,
            changed_source_count: targets.len(),
        },
    ))
}

async fn rebuild_scope_memory_index_locked(
    storage: &dyn MemoryStorage,
    definition_store: &dyn DefinitionLookup,
    allow_lancedb_replacement: bool,
) -> Result<MemoryIndexRebuildOutcome> {
    let app_projection_revision = current_app_memory_projection_revision(storage).await?;
    let records = definition_store
        .list_moveable_definitions()
        .await
        .context("listing agent definitions for memory index rebuild")?;
    let candidates = collect_scope_memory_candidates(storage, &records)
        .await
        .context("collecting memory candidates for index rebuild")?;
    ensure_app_memory_projection_revision(storage, &app_projection_revision).await?;
    let source_hashes = source_hashes_for_candidates(storage, &records, &candidates)
        .await
        .context("hashing memory index sources")?;

    let documents_path = storage.memory_index_documents_path();
    let manifest_path = storage.memory_index_manifest_path();
    let lancedb_outcome = write_lancedb_index(
        storage,
        &candidates,
        allow_lancedb_replacement,
        &app_projection_revision,
    )
    .await
    .context("writing LanceDB memory index")?;
    ensure_app_memory_projection_revision(storage, &app_projection_revision).await?;
    write_documents(storage, &documents_path, &candidates)
        .await
        .context("writing memory index documents")?;
    // A full rebuild is the one place inside the index that holds every
    // candidate in the scope, so it is where key migration and bounded
    // retention can safely run. Incremental updates above see only their own
    // targets and must keep using the non-evicting sync.
    // Projections share the overlay's key space, so they must migrate in the
    // same pass. Doing the overlay alone strands every projection under a key
    // its source no longer has: the next maintenance sees no matching source
    // hash, never refreshes `last_verified_at`, and eventually deactivates a
    // projection that was still correct. The API handler already pairs these;
    // this path did not, and it is the path a rebuild actually takes.
    let projection_renames = memory_temperature_candidate_key_renames(&candidates);
    match migrate_memory_hot_projection_keys_for_scope(storage, &projection_renames).await {
        Ok(migrated) if migrated > 0 => info!(
            migrated,
            "migrated memory hot projection keys during memory index rebuild"
        ),
        Ok(_) => {},
        Err(error) => warn!(
            error = %error,
            "failed to migrate memory hot projection keys during memory index rebuild"
        ),
    }
    match resync_memory_temperature_overlay_full_scope(
        storage,
        &candidates,
        MemoryTemperatureRetentionPolicy::default(),
    )
    .await
    {
        Ok((_, compaction)) => {
            if compaction.changed() {
                info!(
                    migrated_keys = compaction.migrated_keys,
                    evicted_dead = compaction.evicted_dead,
                    evicted_over_cap = compaction.evicted_over_cap,
                    retained = compaction.retained,
                    "reconciled memory temperature overlay during memory index rebuild"
                );
            }
        },
        Err(error) => warn!(
            error = %error,
            "failed to sync memory temperature overlay during memory index rebuild"
        ),
    }
    let manifest = build_manifest(
        storage,
        &records,
        &candidates,
        source_hashes,
        lancedb_outcome.embedding,
        lancedb_outcome.chunk_count,
    );
    let manifest_value =
        serde_json::to_value(&manifest).context("serializing memory index manifest")?;
    storage
        .write_json_value_atomic(&manifest_path, &manifest_value)
        .await
        .context("writing memory index manifest")?;
    publish_memory_index_generation(storage, &manifest);

    Ok(MemoryIndexRebuildOutcome {
        manifest,
        manifest_path,
        documents_path,
        lancedb_write: lancedb_outcome.report,
    })
}

#[derive(Debug, Clone)]
enum MemoryIndexIncrementalTarget {
    UserMemory {
        tiers: Vec<MemoryTierDefinition>,
    },
    NativeTier {
        agent_id: String,
        tier: MemoryTierDefinition,
        goal_id: Option<String>,
    },
    /// Every episode belonging to one agent.
    Episodes {
        agent_id: String,
    },
}

impl MemoryIndexIncrementalTarget {
    fn matches(&self, candidate: &MemoryCandidateDocument) -> bool {
        match self {
            Self::UserMemory { .. } => matches!(&candidate.scope, TierScope::User),
            // Every episode candidate for this agent, whatever goal it carries:
            // the loader below re-reads the whole directory, so the set it
            // replaces has to be the whole surface too.
            Self::Episodes { agent_id } => {
                candidate.agent_id.as_deref() == Some(agent_id.as_str())
                    && matches!(candidate.scope, TierScope::Agent)
                    && candidate.tier_name == crate::episode_candidates::EPISODE_TIER_NAME
            },
            Self::NativeTier {
                agent_id,
                tier,
                goal_id,
            } => {
                candidate.agent_id.as_deref() == Some(agent_id.as_str())
                    && same_tier_scope(&candidate.scope, &tier.scope)
                    && candidate.goal_id.as_deref() == goal_id.as_deref()
                    && (candidate.tier_name == tier.name
                        || candidate
                            .tier_name
                            .strip_prefix(&tier.name)
                            .is_some_and(|suffix| suffix.starts_with('.')))
            },
        }
    }

    async fn load_current_candidates(
        &self,
        storage: &dyn MemoryStorage,
    ) -> Result<Vec<MemoryCandidateDocument>> {
        match self {
            Self::UserMemory { tiers } => load_memory_candidate_documents(
                storage,
                "__user__",
                tiers,
                &MemoryCandidateRequest {
                    scope: TierScope::User,
                    goal_id: None,
                    recency_cutoff: None,
                    include_environment_knowledge: true,
                    retrieval_scope: RetrievalScope::Unbound,
                },
            )
            .await
            .context("loading incrementally changed user memory candidates"),
            Self::Episodes { agent_id } => load_episode_candidate_documents(storage, agent_id)
                .await
                .with_context(|| format!("loading changed episode candidates for {agent_id}")),
            Self::NativeTier {
                agent_id,
                tier,
                goal_id,
            } => load_memory_candidate_documents(
                storage,
                agent_id,
                std::slice::from_ref(tier),
                &MemoryCandidateRequest {
                    scope: tier.scope.clone(),
                    goal_id: goal_id.as_deref(),
                    recency_cutoff: None,
                    include_environment_knowledge: true,
                    retrieval_scope: RetrievalScope::Unbound,
                },
            )
            .await
            .with_context(|| {
                format!(
                    "loading incrementally changed memory candidates for {agent_id}/{}",
                    tier.name
                )
            }),
        }
    }

    fn candidate_key_prefix(&self) -> String {
        let (scope, agent_id, goal_id) = match self {
            Self::UserMemory { .. } => (TierScope::User, None, None),
            // Episodes span every goal, so there is no single goal segment to
            // put in a prefix. `matches_candidate_key` handles the variant
            // directly; this value only completes the match and is the widest
            // honest prefix for the agent's episodic surface.
            Self::Episodes { agent_id } => (TierScope::Agent, Some(agent_id.as_str()), None),
            Self::NativeTier {
                agent_id,
                tier,
                goal_id,
            } => (
                tier.scope.clone(),
                Some(agent_id.as_str()),
                goal_id.as_deref(),
            ),
        };
        format!(
            "v2:{}:{}:{}:",
            length_prefixed_key_segment(scope_label(&scope)),
            optional_key_segment(agent_id),
            optional_key_segment(goal_id),
        )
    }

    fn matches_indexed_tier_name(&self, tier_name: &str) -> bool {
        match self {
            Self::UserMemory { .. } => true,
            Self::Episodes { .. } => tier_name == crate::episode_candidates::EPISODE_TIER_NAME,
            Self::NativeTier { tier, .. } => {
                tier_name == tier.name
                    || tier_name
                        .strip_prefix(&tier.name)
                        .is_some_and(|suffix| suffix.starts_with('.'))
            },
        }
    }

    fn matches_candidate_key(&self, candidate_key: &str) -> bool {
        if let Self::Episodes { agent_id } = self {
            return episode_candidate_key_matches(agent_id, candidate_key);
        }
        let prefix = self.candidate_key_prefix();
        let Some(tail) = candidate_key.strip_prefix(&prefix) else {
            return false;
        };
        if matches!(self, Self::UserMemory { .. }) {
            return true;
        }
        let Some((tier_name, item_tail)) = parse_length_prefixed_key_segment(tail) else {
            return false;
        };
        // Validate the remaining item-key segment as well. Failing closed on a
        // malformed derived key is safer than retaining an old relevance score
        // for a pending native tier.
        if parse_length_prefixed_key_segment(item_tail)
            .is_none_or(|(_, remainder)| !remainder.is_empty())
        {
            return false;
        }
        self.matches_indexed_tier_name(tier_name)
    }
}

/// Whether a scored candidate key belongs to one agent's episodic surface.
///
/// Episodes carry a per-episode goal, so unlike a tier target there is no fixed
/// goal segment to match: the prefix stops at the agent and the goal segment is
/// skipped. Both spellings a goal segment can take are accepted, because the
/// unsafe direction here is matching too little — a missed key keeps a stale
/// relevance score for content that has just changed.
fn episode_candidate_key_matches(agent_id: &str, candidate_key: &str) -> bool {
    let prefix = format!(
        "v2:{}:{}:",
        length_prefixed_key_segment(scope_label(&TierScope::Agent)),
        optional_key_segment(Some(agent_id)),
    );
    let Some(tail) = candidate_key.strip_prefix(&prefix) else {
        return false;
    };
    let after_goal = if let Some(rest) = tail.strip_prefix("none:") {
        rest
    } else if let Some(rest) = tail.strip_prefix("some:") {
        match parse_length_prefixed_key_segment(rest) {
            Some((_, after_goal)) => after_goal,
            None => return false,
        }
    } else {
        return false;
    };
    let Some((tier_name, item_tail)) = parse_length_prefixed_key_segment(after_goal) else {
        return false;
    };
    if tier_name != crate::episode_candidates::EPISODE_TIER_NAME {
        return false;
    }
    // Validate the item segment too: a malformed derived key should fail the
    // same way the tier path fails it.
    parse_length_prefixed_key_segment(item_tail).is_some_and(|(_, remainder)| remainder.is_empty())
}

async fn resolve_incremental_targets(
    _storage: &dyn MemoryStorage,
    definition_store: &dyn DefinitionLookup,
    snapshot: &MemoryIndexChangeSnapshot,
) -> Result<Option<Vec<MemoryIndexIncrementalTarget>>> {
    // Episodes belong here too: the Episodes arm below checks the agent against
    // these records, so omitting it left `records` empty, made that check fail
    // for every agent, and returned `Ok(None)` — which is "cannot resolve
    // incrementally". That did not merely disable incremental episode indexing;
    // it made each append force a full rebuild and, until that rebuild, drop
    // hybrid scoring for every candidate in the scope.
    let needs_definition_records = snapshot.changes().any(|change| {
        matches!(
            change,
            MemoryIndexChange::UserKnowledge
                | MemoryIndexChange::NativeTier { .. }
                | MemoryIndexChange::Episodes { .. }
        )
    });
    let records = if !needs_definition_records {
        Vec::new()
    } else {
        definition_store
            .list_moveable_definitions()
            .await
            .context("listing definitions for incremental memory index update")?
    };
    let mut targets = Vec::new();
    let user_tiers = user_memory_tiers(&records);
    let indexable_user_tiers = user_tiers
        .iter()
        .filter(|tier| !is_builtin_user_memory_tier(&tier.name))
        .cloned()
        .collect::<Vec<_>>();
    let mut includes_user_memory = false;
    for change in snapshot.changes() {
        match change {
            MemoryIndexChange::UserKnowledge => includes_user_memory = true,
            MemoryIndexChange::NativeTier {
                agent_id,
                tier_name,
                scope,
                goal_id,
            } => {
                if matches!(scope, TierScope::User) {
                    if !user_tiers
                        .iter()
                        .any(|tier| tier.name == *tier_name && same_tier_scope(&tier.scope, scope))
                    {
                        return Ok(None);
                    }
                    includes_user_memory = true;
                    continue;
                }
                let Some(record) = records.iter().find(|record| record.agent_id == *agent_id)
                else {
                    return Ok(None);
                };
                let Some(tier) = record
                    .memory_tiers
                    .iter()
                    .find(|tier| tier.name == *tier_name && same_tier_scope(&tier.scope, scope))
                else {
                    return Ok(None);
                };
                if matches!(scope, TierScope::AgentGoal) && goal_id.as_deref().is_none() {
                    return Ok(None);
                }
                targets.push(MemoryIndexIncrementalTarget::NativeTier {
                    agent_id: agent_id.clone(),
                    tier: tier.clone(),
                    goal_id: goal_id.clone(),
                });
            },
            // An agent with no definition record in this scope has no
            // indexable surface to refresh; the conservative full rebuild is
            // the same answer the tier arm gives for an unknown agent.
            MemoryIndexChange::Episodes { agent_id } => {
                if !records.iter().any(|record| record.agent_id == *agent_id) {
                    return Ok(None);
                }
                targets.push(MemoryIndexIncrementalTarget::Episodes {
                    agent_id: agent_id.clone(),
                });
            },
            MemoryIndexChange::FullScope { .. } => return Ok(None),
        }
    }
    if includes_user_memory {
        targets.push(MemoryIndexIncrementalTarget::UserMemory {
            tiers: indexable_user_tiers,
        });
    }
    Ok(Some(targets))
}

/// Canonical user memory is shared across agents, but its schema can be
/// declared by more than one agent definition. Resolve each tier name once and
/// choose the lexicographically first declaring agent deterministically if
/// definitions disagree. The legacy files are extracted separately because
/// their historical JSON shape predates native tier records.
fn user_memory_tiers(records: &[MoveableDefinitionRecord]) -> Vec<MemoryTierDefinition> {
    let mut tiers_by_name = BTreeMap::<String, (String, MemoryTierDefinition)>::new();
    for record in records {
        for tier in record
            .memory_tiers
            .iter()
            .filter(|tier| matches!(&tier.scope, &TierScope::User))
        {
            let replace = tiers_by_name
                .get(&tier.name)
                .is_none_or(|(agent_id, _)| record.agent_id.as_str() < agent_id.as_str());
            if replace {
                tiers_by_name.insert(tier.name.clone(), (record.agent_id.clone(), tier.clone()));
            }
        }
    }
    tiers_by_name.into_values().map(|(_, tier)| tier).collect()
}

fn indexable_user_memory_tiers(records: &[MoveableDefinitionRecord]) -> Vec<MemoryTierDefinition> {
    user_memory_tiers(records)
        .into_iter()
        .filter(|tier| !is_builtin_user_memory_tier(&tier.name))
        .collect()
}

fn is_builtin_user_memory_tier(tier_name: &str) -> bool {
    matches!(
        tier_name,
        "knowledge" | "contacts" | "routines" | "research_findings"
    )
}

fn dedupe_memory_candidates(
    candidates: Vec<MemoryCandidateDocument>,
) -> Vec<MemoryCandidateDocument> {
    let mut by_key = BTreeMap::new();
    for candidate in candidates {
        by_key.insert(memory_candidate_index_key(&candidate), candidate);
    }
    by_key.into_values().collect()
}

fn replace_manifest_candidate_source_hashes(
    source_hashes: &mut BTreeMap<String, String>,
    candidates: &[MemoryCandidateDocument],
) -> Result<()> {
    source_hashes.retain(|key, _| !key.starts_with("candidate:"));
    for candidate in candidates {
        source_hashes.insert(
            format!("candidate:{}", memory_candidate_index_key(candidate)),
            memory_candidate_manifest_source_hash(candidate)?,
        );
    }
    Ok(())
}

fn same_tier_scope(left: &TierScope, right: &TierScope) -> bool {
    matches!(
        (left, right),
        (TierScope::User, TierScope::User)
            | (TierScope::Agent, TierScope::Agent)
            | (TierScope::AgentGoal, TierScope::AgentGoal)
    )
}

fn memory_index_agent_summaries(
    candidates: &[MemoryCandidateDocument],
    previous: &[MemoryIndexAgentSummary],
) -> Vec<MemoryIndexAgentSummary> {
    let mut counts = previous
        .iter()
        .map(|summary| (summary.agent_id.clone(), 0usize))
        .collect::<BTreeMap<_, _>>();
    for candidate in candidates {
        if let Some(agent_id) = candidate.agent_id.as_ref() {
            *counts.entry(agent_id.clone()).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .map(|(agent_id, document_count)| MemoryIndexAgentSummary {
            agent_id,
            document_count,
        })
        .collect()
}

pub async fn optimize_scope_memory_index(
    storage: &dyn MemoryStorage,
) -> Result<MemoryIndexOptimizeOutcome> {
    let _rebuild_guard = MEMORY_INDEX_REBUILD_LOCK.lock().await;
    let _write_lock = acquire_memory_index_write_lock(storage).await?;
    let index_dir = storage.memory_lancedb_index_dir();
    let Some(table) = open_existing_lancedb_memory_table(&index_dir).await? else {
        return Ok(MemoryIndexOptimizeOutcome {
            optimized: false,
            skipped_reason: Some("missing_lancedb_table".to_string()),
            index_dir,
            table_name: MEMORY_LANCEDB_TABLE.to_string(),
            compaction_ran: false,
            prune_ran: false,
        });
    };
    let optimize_timeout = lancedb_optimize_timeout();
    let stats = async {
        ensure_lancedb_memory_indexes(&table, &index_dir).await?;
        timeout(optimize_timeout, table.optimize(OptimizeAction::All))
            .await
            .map_err(|_| {
                anyhow!(
                    "timed out after {}ms optimizing LanceDB memory index",
                    optimize_timeout.as_millis()
                )
            })?
            .context("optimizing LanceDB memory index")
    }
    .await;
    // Ensure/optimize publish a new Lance version. Pooled search tables are
    // lazy snapshots; drop them even when the attempt fails, because a
    // version may already have committed.
    invalidate_lance_table_pool(&index_dir);
    let stats = stats?;
    Ok(MemoryIndexOptimizeOutcome {
        optimized: true,
        skipped_reason: None,
        index_dir,
        table_name: MEMORY_LANCEDB_TABLE.to_string(),
        compaction_ran: stats.compaction.is_some(),
        prune_ran: stats.prune.is_some(),
    })
}

pub async fn inspect_scope_memory_index(
    storage: &dyn MemoryStorage,
    definition_store: &dyn DefinitionLookup,
) -> Result<MemoryIndexStatus> {
    let records = definition_store
        .list_moveable_definitions()
        .await
        .context("listing agent definitions for memory index status")?;
    let candidates = collect_scope_memory_candidates(storage, &records)
        .await
        .context("collecting memory candidates for index status")?;
    let current_source_hashes = source_hashes_for_candidates(storage, &records, &candidates)
        .await
        .context("hashing memory index sources")?;
    let pending_changes = snapshot_memory_index_changes(storage).await?;
    let manifest = load_memory_index_manifest(storage).await?;
    let documents_path = storage.memory_index_documents_path();
    let lancedb_index_dir = storage.memory_lancedb_index_dir();
    let documents_exist = fs::try_exists(&documents_path).await.with_context(|| {
        format!(
            "checking memory index documents {}",
            documents_path.display()
        )
    })?;
    let lancedb_exists = fs::try_exists(&lancedb_index_dir).await.with_context(|| {
        format!(
            "checking memory LanceDB index {}",
            lancedb_index_dir.display()
        )
    })?;

    let embedding_config = MemoryEmbeddingConfig::from_env();
    let hard_stale_reason = match manifest.as_ref() {
        None => (true, "missing_manifest".to_string()),
        Some(_) if !documents_exist => (true, "missing_documents".to_string()),
        Some(_) if !lancedb_exists => (true, "missing_lancedb_index".to_string()),
        Some(manifest) if manifest.index_version != MEMORY_INDEX_VERSION => {
            (true, "index_version_changed".to_string())
        },
        Some(manifest) if manifest.schema_version != MEMORY_INDEX_SCHEMA_VERSION => {
            (true, "schema_version_changed".to_string())
        },
        Some(manifest) if manifest.backend != MEMORY_INDEX_BACKEND => {
            (true, "backend_changed".to_string())
        },
        Some(manifest) if !embedding_config.manifest_is_compatible(manifest) => {
            (true, "embedding_config_changed".to_string())
        },
        Some(_) if !pending_changes.is_empty() => (true, "pending_changes".to_string()),
        Some(manifest) if manifest.source_hashes != current_source_hashes => {
            (true, "source_hashes_changed".to_string())
        },
        Some(_) => (false, String::new()),
    };

    let (stale, reason) = if hard_stale_reason.0 {
        hard_stale_reason
    } else {
        let lancedb_health_reason = match manifest.as_ref() {
            Some(manifest) if manifest.document_count > 0 => {
                lancedb_memory_index_unhealthy_reason_with_timeout(&lancedb_index_dir, manifest)
                    .await
                    .with_context(|| {
                        format!(
                            "validating LanceDB memory index health {}",
                            lancedb_index_dir.display()
                        )
                    })?
            },
            _ => None,
        };
        match manifest.as_ref() {
            Some(_) if lancedb_health_reason.is_some() => {
                (true, lancedb_health_reason.unwrap_or_default())
            },
            Some(manifest) if manifest.document_count != candidates.len() => {
                (true, "document_count_changed".to_string())
            },
            Some(_) => (false, "fresh".to_string()),
            None => (true, "missing_manifest".to_string()),
        }
    };

    Ok(MemoryIndexStatus {
        manifest,
        stale,
        reason,
        current_document_count: candidates.len(),
        current_source_count: current_source_hashes.len(),
    })
}

/// Return a lightweight status suitable for UI/HTTP diagnostics.
///
/// This intentionally does not collect/hash all canonical memory candidates.
/// Full freshness inspection belongs to the maintainer/rebuild path; page-load
/// diagnostics must not block behind a rebuild-grade scan.
pub async fn inspect_scope_memory_index_fast(
    storage: &dyn MemoryStorage,
) -> Result<MemoryIndexStatus> {
    let manifest = load_memory_index_manifest(storage).await?;
    let documents_path = storage.memory_index_documents_path();
    let lancedb_index_dir = storage.memory_lancedb_index_dir();
    let documents_exist = fs::try_exists(&documents_path).await.with_context(|| {
        format!(
            "checking memory index documents {}",
            documents_path.display()
        )
    })?;
    let lancedb_exists = fs::try_exists(&lancedb_index_dir).await.with_context(|| {
        format!(
            "checking memory LanceDB index {}",
            lancedb_index_dir.display()
        )
    })?;

    let embedding_config = MemoryEmbeddingConfig::from_env();
    let (stale, reason) = match manifest.as_ref() {
        None => (true, "missing_manifest".to_string()),
        Some(_) if !documents_exist => (true, "missing_documents".to_string()),
        Some(_) if !lancedb_exists => (true, "missing_lancedb_index".to_string()),
        Some(manifest) if manifest.index_version != MEMORY_INDEX_VERSION => {
            (true, "index_version_changed".to_string())
        },
        Some(manifest) if manifest.schema_version != MEMORY_INDEX_SCHEMA_VERSION => {
            (true, "schema_version_changed".to_string())
        },
        Some(manifest) if manifest.backend != MEMORY_INDEX_BACKEND => {
            (true, "backend_changed".to_string())
        },
        Some(manifest) if !embedding_config.manifest_is_compatible(manifest) => {
            (true, "embedding_config_changed".to_string())
        },
        Some(_) => (false, "fresh_manifest_files".to_string()),
    };
    let current_document_count = manifest
        .as_ref()
        .map(|manifest| manifest.document_count)
        .unwrap_or(0);
    let current_source_count = manifest
        .as_ref()
        .map(|manifest| manifest.source_count)
        .unwrap_or(0);

    Ok(MemoryIndexStatus {
        manifest,
        stale,
        reason,
        current_document_count,
        current_source_count,
    })
}

/// Return indexed documents only when the manifest is fresh. Callers that get
/// `None` should fall back to direct candidate extraction and trigger rebuild.
pub async fn load_fresh_index_documents(
    storage: &dyn MemoryStorage,
    definition_store: &dyn DefinitionLookup,
) -> Result<Option<Vec<MemoryCandidateDocument>>> {
    let status = inspect_scope_memory_index(storage, definition_store).await?;
    if status.stale {
        return Ok(None);
    }

    let documents_path = storage.memory_index_documents_path();
    let content = match fs::read_to_string(&documents_path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("reading memory index documents"),
    };
    let mut documents = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let document = serde_json::from_str::<MemoryCandidateDocument>(line)
            .with_context(|| format!("parsing memory index document line {}", idx + 1))?;
        documents.push(document);
    }
    Ok(Some(documents))
}

/// Return revision-bound BM25 scores from the derived LanceDB FTS index. When
/// source-addressed changes are pending, retain the last-known-good scores only
/// for unaffected candidates. Changed/new canonical candidates keep the
/// caller's direct lexical ranking until reconciliation. Callers must look
/// scores up with
/// [`memory_candidate_index_score_key`], never the stable identity key alone.
pub async fn score_fresh_memory_index(
    storage: &dyn MemoryStorage,
    definition_store: &dyn DefinitionLookup,
    query: &str,
) -> Result<Option<BTreeMap<String, f32>>> {
    if query.trim().is_empty() {
        return Ok(None);
    }
    let before = retrieval_journal_view(storage).await?;
    let status = inspect_scope_memory_index(storage, definition_store).await?;
    if !memory_index_status_allows_snapshot_overlay(&status, before.pending()) {
        return Ok(None);
    }
    if status.current_document_count == 0 {
        return if before.pending().is_empty() {
            Ok(Some(BTreeMap::new()))
        } else {
            Ok(None)
        };
    }
    let manifest = status
        .manifest
        .as_ref()
        .context("derived memory index status allowed retrieval but had no manifest")?;
    let index_dir = storage.memory_lancedb_index_dir();
    let scores = score_lancedb_index(
        &index_dir,
        query,
        MEMORY_INDEX_SEARCH_LIMIT,
        &memory_index_generation_token(manifest),
    )
    .await?;
    let after = retrieval_journal_view(storage).await?;
    let mut affected_candidate_keys = BTreeSet::new();
    for pending in distinct_memory_index_change_snapshots(before.pending(), after.pending()) {
        if pending.is_empty() {
            continue;
        }
        let Some(keys) =
            pending_memory_index_affected_scored_keys(storage, definition_store, pending, &scores)
                .await?
        else {
            return Ok(None);
        };
        affected_candidate_keys.extend(keys);
    }
    let scores = retain_unaffected_memory_index_scores(scores, &affected_candidate_keys);
    Ok(Some(bind_memory_index_scores_to_revisions(
        scores,
        &manifest.source_hashes,
    )?))
}

/// Return revision-bound fused BM25 + vector scores from the derived LanceDB
/// index. Canonical memory tier JSON remains the source of truth; a pending
/// source-addressed delta removes only affected bounded hits without discarding
/// unrelated last-known-good scores. Changed/new candidates retain the
/// caller's direct lexical ranking until reconciliation. Callers must use
/// [`memory_candidate_index_score_key`] for canonical-candidate lookup.
pub async fn score_fresh_memory_hybrid_index(
    storage: &dyn MemoryStorage,
    definition_store: &dyn DefinitionLookup,
    query: &str,
) -> Result<Option<BTreeMap<String, f32>>> {
    Ok(score_fresh_memory_hybrid_index_with_status_filtered(
        storage,
        definition_store,
        query,
        None,
        true,
    )
    .await?
    .scores)
}

/// Return fused BM25 + vector scores plus the reason when the derived hybrid
/// index was skipped. This lets callers make direct fallback observable rather
/// than treating `None` as an opaque condition.
pub async fn score_fresh_memory_hybrid_index_with_status(
    storage: &dyn MemoryStorage,
    definition_store: &dyn DefinitionLookup,
    query: &str,
) -> Result<MemoryHybridIndexScoreResult> {
    score_fresh_memory_hybrid_index_with_status_filtered(
        storage,
        definition_store,
        query,
        None,
        false,
    )
    .await
}

/// Return hybrid scores from only the memory universe visible to `agent_id`:
/// user-scoped memory plus that agent's Agent and AgentGoal memory. Filtering
/// happens inside both LanceDB search legs before their bounded top-K, so
/// unrelated agents cannot crowd relevant visible memories out of the result.
/// The returned map can still be shared across all three visible prompt lanes.
pub async fn score_fresh_memory_hybrid_index_for_agent_with_status(
    storage: &dyn MemoryStorage,
    definition_store: &dyn DefinitionLookup,
    query: &str,
    agent_id: &str,
) -> Result<MemoryHybridIndexScoreResult> {
    let predicate = memory_index_agent_visibility_predicate(agent_id);
    score_fresh_memory_hybrid_index_with_status_filtered(
        storage,
        definition_store,
        query,
        Some(&predicate),
        false,
    )
    .await
}

/// Prompt-time variant of the agent-visible scorer. Automatic prompt injection
/// deliberately excludes search-only environment knowledge, so remove those
/// rows before the bounded hybrid top-K rather than letting candidates that can
/// never be packed crowd out injectable memory. Explicit `search_memory` keeps
/// using [`score_fresh_memory_hybrid_index_for_agent_with_status`] and can
/// therefore still retrieve the search-only tier.
pub async fn score_fresh_memory_hybrid_index_for_prompt_with_status(
    storage: &dyn MemoryStorage,
    definition_store: &dyn DefinitionLookup,
    query: &str,
    agent_id: &str,
) -> Result<MemoryHybridIndexScoreResult> {
    let predicate = memory_index_prompt_visibility_predicate(agent_id);
    score_fresh_memory_hybrid_index_with_status_filtered(
        storage,
        definition_store,
        query,
        Some(&predicate),
        false,
    )
    .await
}

/// Runtime-only authorization for embedding accepted `local_only` app memory.
/// Implementations live at the authenticated app/provider boundary. Returning
/// a partition digest authorizes only this exact physical identity; the scorer
/// revalidates it again after provider I/O and rejects any changed digest.
pub trait EphemeralAppMemoryEmbeddingAuthorizer: Send + Sync {
    fn authorize(
        &self,
        physical: &MemoryEmbeddingPhysicalIdentity,
    ) -> std::result::Result<String, String>;
}

#[derive(Debug, Clone)]
pub struct EphemeralAppMemoryHybridScoreResult {
    pub scores: BTreeMap<String, f32>,
    pub authority_partition_digest: String,
    pub physical_partition_digest: String,
}

/// Credential-scoped local hybrid ranking for accepted LocalOnly candidates.
///
/// Candidate vectors and score maps are deliberately ephemeral: there is no
/// background path capable of embedding protected text without the turn's
/// exact provider credential, and no score cache that could cross an endpoint,
/// model, trust, cohort, policy, or authority revision. The ordinary document
/// embedding cache is intentionally bypassed for the same reason.
pub async fn score_ephemeral_local_app_memory_candidates(
    query: &str,
    candidates: &[MemoryCandidateDocument],
    authorizer: &dyn EphemeralAppMemoryEmbeddingAuthorizer,
) -> Result<EphemeralAppMemoryHybridScoreResult> {
    const MAX_EPHEMERAL_APP_CANDIDATES: usize = 128;
    if query.trim().is_empty() || candidates.is_empty() {
        return Ok(EphemeralAppMemoryHybridScoreResult {
            scores: BTreeMap::new(),
            authority_partition_digest: String::new(),
            physical_partition_digest: String::new(),
        });
    }
    if candidates.len() > MAX_EPHEMERAL_APP_CANDIDATES
        || candidates.iter().any(|candidate| {
            !crate::memory_candidates::memory_candidate_has_app_source_envelope(
                &candidate.metadata_json,
            ) || candidate
                .metadata_json
                .get("app_model_processing")
                .and_then(serde_json::Value::as_str)
                != Some("local_only")
        })
    {
        anyhow::bail!("ephemeral app-memory hybrid candidates are not an exact LocalOnly set");
    }

    let config = MemoryEmbeddingConfig::from_env();
    let manifest = config.empty_manifest();
    let physical = MemoryEmbeddingPhysicalIdentity {
        provider: manifest.provider.clone(),
        model: manifest
            .model
            .clone()
            .unwrap_or_else(|| config.model.clone()),
        base_url: config.base_url.clone(),
        dimensions: manifest.dimensions,
        embedding_contract_id: manifest.contract_id,
    };
    let authority_partition_digest = authorizer
        .authorize(&physical)
        .map_err(|error| anyhow!("LocalOnly app-memory embedding denied: {error}"))?;
    if !authority_partition_digest.starts_with("blake3:") {
        anyhow::bail!("LocalOnly app-memory authority returned an invalid partition identity");
    }

    let mut texts = Vec::with_capacity(candidates.len() + 1);
    texts.push(expand_memory_retrieval_query(query));
    texts.extend(candidates.iter().map(memory_candidate_index_text));
    let embedded = match config.preference {
        #[cfg(feature = "test-hash-embeddings")]
        MemoryEmbeddingPreference::TestHash => config.embed_documents(&texts).await?,
        MemoryEmbeddingPreference::Auto | MemoryEmbeddingPreference::Ollama => {
            config
                .embed_documents_with_ollama_priority(&texts, EmbeddingPriority::Read)
                .await?
        },
    };
    let after_partition = authorizer
        .authorize(&physical)
        .map_err(|error| anyhow!("LocalOnly app-memory embedding became stale: {error}"))?;
    if after_partition != authority_partition_digest
        || embedded.manifest.contract_id != physical.embedding_contract_id
        || embedded.vectors.len() != texts.len()
    {
        anyhow::bail!("LocalOnly app-memory embedding partition changed during provider I/O");
    }

    let query_vector = embedded
        .vectors
        .first()
        .context("LocalOnly app-memory query embedding is missing")?;
    let mut vector_rank = candidates
        .iter()
        .zip(embedded.vectors.iter().skip(1))
        .map(|(candidate, vector)| {
            (
                memory_candidate_index_score_key(candidate),
                l2_squared_distance(query_vector, vector),
            )
        })
        .collect::<Vec<_>>();
    vector_rank.sort_by(|left, right| {
        left.1
            .partial_cmp(&right.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });

    let query_terms = app_memory_lexical_terms(query);
    let mut lexical_rank = candidates
        .iter()
        .map(|candidate| {
            let terms = app_memory_lexical_terms(&memory_candidate_index_text(candidate));
            let overlap = query_terms
                .iter()
                .map(|(term, query_count)| {
                    terms
                        .get(term)
                        .copied()
                        .unwrap_or(0)
                        .min(*query_count)
                        .min(3)
                })
                .sum::<usize>();
            (memory_candidate_index_score_key(candidate), overlap)
        })
        .collect::<Vec<_>>();
    lexical_rank.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));

    let mut scores = BTreeMap::<String, f32>::new();
    for (rank, (key, _)) in vector_rank.into_iter().enumerate() {
        *scores.entry(key).or_default() += 1.0 / (rank as f32 + MEMORY_HYBRID_RRF_K);
    }
    for (rank, (key, overlap)) in lexical_rank.into_iter().enumerate() {
        if overlap != 0 {
            *scores.entry(key).or_default() += 1.0 / (rank as f32 + MEMORY_HYBRID_RRF_K);
        }
    }
    for score in scores.values_mut() {
        *score *= MEMORY_HYBRID_SCORE_SCALE;
    }
    Ok(EphemeralAppMemoryHybridScoreResult {
        scores,
        authority_partition_digest,
        physical_partition_digest: physical.partition_digest(),
    })
}

fn app_memory_lexical_terms(text: &str) -> HashMap<String, usize> {
    let mut terms = HashMap::new();
    for term in text
        .split(|character: char| !character.is_alphanumeric())
        .map(str::trim)
        .filter(|term| term.len() > 1)
        .map(str::to_ascii_lowercase)
    {
        *terms.entry(term).or_default() += 1;
    }
    terms
}

async fn score_fresh_memory_hybrid_index_with_status_filtered(
    storage: &dyn MemoryStorage,
    definition_store: &dyn DefinitionLookup,
    query: &str,
    predicate: Option<&str>,
    verify_canonical_freshness: bool,
) -> Result<MemoryHybridIndexScoreResult> {
    if query.trim().is_empty() {
        return Ok(MemoryHybridIndexScoreResult {
            scores: None,
            fallback_reason: None,
            stale: false,
            current_document_count: 0,
            served_ann_fallback: false,
        });
    }
    // Full canonical hashing stays uncached: it is not the request path and
    // already pays a rebuild-grade scan. The inspect-fast request path is the
    // frozen-index 100× candidate and the only default-off cache admission.
    if !verify_canonical_freshness && hybrid_result_cache_enabled() {
        return score_fresh_memory_hybrid_index_cached(storage, definition_store, query, predicate)
            .await;
    }
    score_fresh_memory_hybrid_index_uncached(
        storage,
        definition_store,
        query,
        predicate,
        verify_canonical_freshness,
        crate::vector_search_mode::vector_search_snapshot().0,
    )
    .await
}

async fn score_fresh_memory_hybrid_index_cached(
    storage: &dyn MemoryStorage,
    definition_store: &dyn DefinitionLookup,
    query: &str,
    predicate: Option<&str>,
) -> Result<MemoryHybridIndexScoreResult> {
    let snapshot = retrieval_journal_view(storage).await?;
    // Capture after the journal await so the cache key and the loader's
    // vector leg share one post-I/O snapshot.
    let (settings, ranking_epoch) = crate::vector_search_mode::vector_search_snapshot();
    // Always singleflight, even before inspect publishes a generation or
    // after IVF drops the token. `unpublished` is not retained (`store_ready`
    // requires a matching published token); followers still share one load.
    let generation = published_hybrid_index_generation(storage.root())
        .unwrap_or_else(|| "unpublished".to_string());
    let key = hybrid_score_cache_key(
        storage,
        query,
        predicate,
        snapshot.as_ref(),
        &generation,
        settings,
        ranking_epoch,
    );
    let loaded = get_or_load(key, || {
        let query = query.to_string();
        let predicate = predicate.map(str::to_string);
        let expected_generation = generation.clone();
        async move {
            let result = score_fresh_memory_hybrid_index_uncached(
                storage,
                definition_store,
                &query,
                predicate.as_deref(),
                false,
                settings,
            )
            .await?;
            let mut cached = CachedHybridScore::from(result);
            if published_hybrid_index_generation(storage.root()).as_deref()
                != Some(expected_generation.as_str())
            {
                cached.retain = false;
            }
            if vector_search_ranking_epoch() != ranking_epoch {
                cached.retain = false;
            }
            Ok(cached)
        }
    })
    .await?;
    Ok(loaded.into())
}

async fn score_fresh_memory_hybrid_index_uncached(
    storage: &dyn MemoryStorage,
    definition_store: &dyn DefinitionLookup,
    query: &str,
    predicate: Option<&str>,
    verify_canonical_freshness: bool,
    settings: VectorSearchSettings,
) -> Result<MemoryHybridIndexScoreResult> {
    // See `score_fresh_memory_index`: prompt-time hybrid scoring is advisory
    // ordering over canonical candidates, so it uses a bounded manifest/file
    // check instead of hashing every source on every request.
    //
    // Pending journal state comes from the in-memory read snapshot so request
    // paths do not take the process-wide journal lock. `pass_through` restores
    // the locked file snapshot. Manifest/file freshness is still inspect_fast,
    // which does not take that mutex.
    let before = retrieval_journal_view(storage).await?;
    let status = if verify_canonical_freshness {
        inspect_scope_memory_index(storage, definition_store).await?
    } else {
        inspect_scope_memory_index_fast(storage).await?
    };
    if !memory_index_status_allows_snapshot_overlay(&status, before.pending()) {
        if memory_index_stale_reason_is_transient_lancedb(&status.reason) {
            anyhow::bail!(
                "LanceDB memory index transiently unavailable: {}",
                status.reason
            );
        }
        invalidate_hybrid_results_for_root(storage.root());
        invalidate_lance_table_pool(&storage.memory_lancedb_index_dir());
        if memory_index_stale_reason_is_repairable_lancedb(&status.reason) {
            anyhow::bail!("LanceDB memory index unhealthy: {}", status.reason);
        }
        return Ok(MemoryHybridIndexScoreResult {
            scores: None,
            fallback_reason: Some(format!("memory_index_stale:{}", status.reason)),
            stale: true,
            current_document_count: status.current_document_count,
            served_ann_fallback: false,
        });
    }
    if let Some(manifest) = status.manifest.as_ref() {
        publish_memory_index_generation(storage, manifest);
    }
    if status.current_document_count == 0 {
        return Ok(MemoryHybridIndexScoreResult {
            scores: if before.pending().is_empty() {
                Some(BTreeMap::new())
            } else {
                None
            },
            fallback_reason: (!before.pending().is_empty())
                .then(|| "memory_index_pending_initial_content".to_string()),
            stale: !before.pending().is_empty(),
            current_document_count: 0,
            served_ann_fallback: false,
        });
    }
    let manifest = status
        .manifest
        .as_ref()
        .context("derived memory index status allowed retrieval but had no manifest")?;
    let index_dir = storage.memory_lancedb_index_dir();
    let generation = published_hybrid_index_generation(storage.root())
        .unwrap_or_else(|| memory_index_generation_token(manifest));
    let (scores, served_ann_fallback) = score_lancedb_hybrid_index(
        &index_dir,
        query,
        MEMORY_INDEX_SEARCH_LIMIT,
        manifest,
        predicate,
        &generation,
        settings,
    )
    .await?;
    // The canonical tier files are always the source of truth. A journaled
    // write must therefore never inherit a relevance boost earned by the old
    // contents of the same candidate. At the same time, invalidating the
    // entire last-known-good snapshot for one changed source makes every
    // unrelated memory fall back for the full debounce window. Keep the
    // snapshot signal for unaffected keys and let the caller's canonical
    // direct scorer rank changed/new candidates until reconciliation lands.
    //
    // Snapshot once before the search and once afterwards so writes observed
    // during the Lance query can immediately shed their old score. Changed/new
    // candidates continue through the renderer's direct lexical ranking until
    // reconciliation. This second snapshot is an optimization, not
    // the correctness boundary: a write may still land after it. The final
    // revision binding below makes every score usable only by the exact indexed
    // candidate revision it ranked, so any later content/type write
    // automatically misses the stale score during candidate handoff.
    let after = retrieval_journal_view(storage).await?;
    let mut affected_candidate_keys = BTreeSet::new();
    for pending in distinct_memory_index_change_snapshots(before.pending(), after.pending()) {
        if pending.is_empty() {
            continue;
        }
        let Some(keys) =
            pending_memory_index_affected_scored_keys(storage, definition_store, pending, &scores)
                .await?
        else {
            return Ok(MemoryHybridIndexScoreResult {
                scores: None,
                fallback_reason: Some("memory_index_pending_full_scope_change".to_string()),
                stale: true,
                current_document_count: status.current_document_count,
                served_ann_fallback: false,
            });
        };
        affected_candidate_keys.extend(keys);
    }
    let scores = retain_unaffected_memory_index_scores(scores, &affected_candidate_keys);
    let scores = bind_memory_index_scores_to_revisions(scores, &manifest.source_hashes)?;
    Ok(MemoryHybridIndexScoreResult {
        scores: Some(scores),
        fallback_reason: None,
        stale: status.stale || !affected_candidate_keys.is_empty(),
        current_document_count: status.current_document_count,
        served_ann_fallback,
    })
}

fn distinct_memory_index_change_snapshots<'a>(
    before: &'a MemoryIndexChangeSnapshot,
    after: &'a MemoryIndexChangeSnapshot,
) -> Vec<&'a MemoryIndexChangeSnapshot> {
    let same_generations = before.entries.len() == after.entries.len()
        && before.entries.iter().zip(&after.entries).all(
            |((left_key, left_generation, _), (right_key, right_generation, _))| {
                left_key == right_key && left_generation == right_generation
            },
        );
    if same_generations {
        vec![before]
    } else {
        vec![before, after]
    }
}

/// Resolve only the bounded scored keys affected by a pending source mutation.
///
/// Prompt scoring already holds at most the fixed chunk-search result bound.
/// Filtering that set in memory avoids both the old all-target
/// Lance key scan and the duplicate canonical candidate load. The renderer is
/// about to load canonical candidates anyway and retains its normal direct
/// lexical signal for every affected/new candidate whose hybrid score is
/// deliberately omitted.
async fn pending_memory_index_affected_scored_keys(
    storage: &dyn MemoryStorage,
    definition_store: &dyn DefinitionLookup,
    snapshot: &MemoryIndexChangeSnapshot,
    scores: &BTreeMap<String, f32>,
) -> Result<Option<BTreeSet<String>>> {
    let Some(targets) = resolve_incremental_targets(storage, definition_store, snapshot).await?
    else {
        return Ok(None);
    };
    Ok(Some(affected_scored_candidate_keys(&targets, scores)))
}

fn affected_scored_candidate_keys(
    targets: &[MemoryIndexIncrementalTarget],
    scores: &BTreeMap<String, f32>,
) -> BTreeSet<String> {
    scores
        .keys()
        .filter(|candidate_key| {
            targets
                .iter()
                .any(|target| target.matches_candidate_key(candidate_key))
        })
        .cloned()
        .collect()
}

fn retain_unaffected_memory_index_scores(
    mut scores: BTreeMap<String, f32>,
    affected_candidate_keys: &BTreeSet<String>,
) -> BTreeMap<String, f32> {
    scores.retain(|candidate_key, _| !affected_candidate_keys.contains(candidate_key));
    scores
}

/// Bind advisory relevance to the exact canonical revision it ranked.
///
/// LanceDB returns stable candidate identity keys. Those are intentionally not
/// sufficient at the canonical handoff: the same logical item can be rewritten
/// after the final journal snapshot but before a renderer loads it. The
/// manifest supplies the compact indexed-revision hash represented by each
/// Lance row. Encoding that hash into the returned lookup key makes a
/// post-score write fail closed without a cross-layer lock.
fn bind_memory_index_scores_to_revisions(
    scores: BTreeMap<String, f32>,
    manifest_source_hashes: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, f32>> {
    let mut bound = BTreeMap::new();
    for (candidate_key, score) in scores {
        let manifest_key = format!("candidate:{candidate_key}");
        let Some(source_hash) = manifest_source_hashes.get(&manifest_key) else {
            warn!(
                target: "memory_index",
                candidate_key,
                "Ignoring memory-index score without a manifest candidate source hash"
            );
            continue;
        };
        let Some(revision_hash) = memory_candidate_revision_from_manifest_source_hash(source_hash)
        else {
            warn!(
                target: "memory_index",
                candidate_key,
                "Ignoring memory-index score with an incompatible manifest source hash"
            );
            continue;
        };
        bound.insert(
            memory_candidate_revision_score_key(&candidate_key, revision_hash),
            score,
        );
    }
    Ok(bound)
}

fn memory_index_agent_visibility_predicate(agent_id: &str) -> String {
    let escaped_agent_id = agent_id.replace('\'', "''");
    format!(
        "({MEMORY_LANCEDB_SCOPE_COLUMN} = 'user' OR {MEMORY_LANCEDB_AGENT_ID_COLUMN} = '{escaped_agent_id}')"
    )
}

fn memory_index_prompt_visibility_predicate(agent_id: &str) -> String {
    format!(
        "({}) AND {MEMORY_LANCEDB_TIER_NAME_COLUMN} NOT LIKE 'environment_knowledge%'",
        memory_index_agent_visibility_predicate(agent_id)
    )
}

/// # Why the index is built unbound (§5A.2)
///
/// The derived index is a relevance oracle, not a retrieval surface: it
/// answers with scores keyed by candidate identity, never with text. A bound
/// execution's candidate list is filtered by
/// [`crate::memory_candidates::retain_candidates_for_scope`] before any score
/// is applied, so a score belonging to another engagement's candidate has no
/// candidate left to attach to and drops out. Building the index per
/// engagement would instead give every engagement a partial, stale index and
/// buy nothing.
async fn collect_scope_memory_candidates(
    storage: &dyn MemoryStorage,
    records: &[MoveableDefinitionRecord],
) -> Result<Vec<MemoryCandidateDocument>> {
    let mut candidates = Vec::new();
    let user_tiers = indexable_user_memory_tiers(records);
    candidates.extend(
        load_memory_candidate_documents(
            storage,
            "__user__",
            &user_tiers,
            &MemoryCandidateRequest {
                scope: TierScope::User,
                goal_id: None,
                recency_cutoff: None,
                include_environment_knowledge: true,
                retrieval_scope: RetrievalScope::Unbound,
            },
        )
        .await
        .context("loading user memory candidates")?,
    );

    for record in records {
        let agent_id = &record.agent_id;
        candidates.extend(
            load_memory_candidate_documents(
                storage,
                agent_id,
                &record.memory_tiers,
                &MemoryCandidateRequest {
                    scope: TierScope::Agent,
                    goal_id: None,
                    recency_cutoff: None,
                    include_environment_knowledge: true,
                    retrieval_scope: RetrievalScope::Unbound,
                },
            )
            .await
            .with_context(|| format!("loading agent memory candidates for {agent_id}"))?,
        );

        // Episodes are a candidate producer the tier walk above cannot reach:
        // they live one directory across, one JSON file each, and are not a
        // declared tier. Indexing them is what gives episodic recall the same
        // BM25 + dense-vector scoring tier candidates already get; without a
        // row here they fall to the keyword path in
        // `rank_memory_candidates_hybrid`, which is what they did before.
        candidates.extend(
            load_episode_candidate_documents(storage, agent_id)
                .await
                .with_context(|| format!("loading episode candidates for {agent_id}"))?,
        );

        for tier in record
            .memory_tiers
            .iter()
            .filter(|tier| matches!(&tier.scope, &TierScope::AgentGoal))
        {
            for goal_id in discover_goal_ids_for_tier(storage, agent_id, tier).await? {
                candidates.extend(
                    load_memory_candidate_documents(
                        storage,
                        agent_id,
                        std::slice::from_ref(tier),
                        &MemoryCandidateRequest {
                            scope: TierScope::AgentGoal,
                            goal_id: Some(&goal_id),
                            recency_cutoff: None,
                            include_environment_knowledge: true,
                            retrieval_scope: RetrievalScope::Unbound,
                        },
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "loading agent-goal memory candidates for {agent_id}/{}/{}",
                            tier.name, goal_id
                        )
                    })?,
                );
            }
        }
    }

    // Candidate identity is the index's source-addressable contract. Collapse
    // accidental duplicate extraction before both full rebuilds and source
    // deltas persist JSONL/manifest state, so every derived surface agrees on
    // one document per candidate key.
    Ok(dedupe_memory_candidates(candidates))
}

/// Enumerate every goal id that has a saved tier record for the
/// given `AgentGoal`-scope tier under `agent_id`. Returns an empty
/// vec when the agent's tiers dir is missing or the tier has no
/// per-goal files.
///
/// `pub` so chat-side retrieval (`ChatService::rank_memory_candidates_hybrid`)
/// can mirror `rebuild_scope_memory_index`'s AgentGoal-coverage
/// pattern at search/forget time. Without this, runtime tools loaded
/// only User + Agent scope while the lancedb index covered
/// AgentGoal too, so any goal-scoped candidate the index would have
/// ranked got dropped on the floor because no runtime candidate
/// matched its index key.
pub async fn discover_goal_ids_for_tier(
    storage: &dyn MemoryStorage,
    agent_id: &str,
    tier: &MemoryTierDefinition,
) -> Result<Vec<String>> {
    let tiers_dir = match storage.agent_tiers_dir(agent_id) {
        Ok(path) => path,
        Err(_) => return Ok(Vec::new()),
    };
    let mut entries = match fs::read_dir(&tiers_dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("reading agent tier directory {}", tiers_dir.display()))
        },
    };

    let tier_prefix = format!("{}_", sanitize_segment(&tier.name));
    let mut goal_ids = BTreeSet::new();
    while let Some(entry) = entries.next_entry().await? {
        let Some(file_name) = entry.file_name().to_str().map(ToString::to_string) else {
            continue;
        };
        let Some(stem) = file_name.strip_suffix(".json") else {
            continue;
        };
        let Some(goal_id) = stem.strip_prefix(&tier_prefix) else {
            continue;
        };
        if !goal_id.is_empty() {
            goal_ids.insert(goal_id.to_string());
        }
    }
    Ok(goal_ids.into_iter().collect())
}

async fn source_hashes_for_candidates(
    _storage: &dyn MemoryStorage,
    records: &[MoveableDefinitionRecord],
    candidates: &[MemoryCandidateDocument],
) -> Result<BTreeMap<String, String>> {
    let mut hashes = BTreeMap::new();
    for record in records {
        hashes.insert(
            format!("definition:{}", record.agent_id),
            record.definition_source_hash.clone(),
        );
    }

    for candidate in candidates {
        hashes.insert(
            format!("candidate:{}", memory_candidate_index_key(candidate)),
            memory_candidate_manifest_source_hash(candidate)?,
        );
    }

    Ok(hashes)
}

fn stable_memory_candidate_source_hash(candidate: &MemoryCandidateDocument) -> Result<String> {
    let source_path = candidate
        .source_path
        .as_ref()
        .map(|path| path.display().to_string());
    let stable = json!({
        "principal": candidate.principal.as_ref(),
        "workspace": candidate.workspace.as_ref(),
        "agent_id": candidate.agent_id.as_ref(),
        "scope": &candidate.scope,
        "tier_name": &candidate.tier_name,
        "semantic_memory_type": candidate.semantic_memory_type,
        "goal_id": candidate.goal_id.as_ref(),
        "item_key": &candidate.item_key,
        "source_path": source_path,
        "json_pointer": &candidate.json_pointer,
        "content_hash": &candidate.content_hash,
        "confidence": candidate.confidence,
        "text": &candidate.text,
        "metadata_json": &candidate.metadata_json,
    });
    let bytes = serde_json::to_vec(&stable).with_context(|| {
        format!(
            "serializing stable memory candidate source hash {}",
            memory_candidate_index_key(candidate)
        )
    })?;
    Ok(hash_bytes(&bytes))
}

/// Persist both the complete canonical freshness hash and the compact revision
/// needed at score handoff. The full hash still detects every prompt-affecting
/// source mutation during index inspection; the revision hash covers exactly
/// the fields used to build indexed relevance and is cheap to reproduce from a
/// freshly loaded candidate without serializing its full text/metadata again.
fn memory_candidate_manifest_source_hash(candidate: &MemoryCandidateDocument) -> Result<String> {
    let revision_hash = memory_candidate_index_revision_hash(candidate);
    let source_hash = stable_memory_candidate_source_hash(candidate)?;
    Ok(format!(
        "candidate-source-v2:{}:{source_hash}",
        length_prefixed_key_segment(&revision_hash),
    ))
}

fn memory_candidate_revision_from_manifest_source_hash(source_hash: &str) -> Option<&str> {
    let encoded = source_hash.strip_prefix("candidate-source-v2:")?;
    let (revision_hash, full_source_hash) = parse_length_prefixed_key_segment(encoded)?;
    (!revision_hash.is_empty() && !full_source_hash.is_empty()).then_some(revision_hash)
}

fn memory_candidate_index_revision_hash(candidate: &MemoryCandidateDocument) -> String {
    let candidate_key = memory_candidate_index_key(candidate);
    let mut hasher = blake3::Hasher::new();
    for field in [
        "memory-index-candidate-revision-v1",
        candidate_key.as_str(),
        candidate.semantic_memory_type.as_str(),
        candidate.content_hash.as_str(),
    ] {
        hasher.update(&(field.len() as u64).to_le_bytes());
        hasher.update(field.as_bytes());
    }
    // App relevance is usable only for the exact accepted source head and
    // provider/policy partition that produced it. Canonical memory documents
    // have no such envelope and retain the compact historical revision key.
    if let Some(identity) = candidate.metadata_json.get("app_index_identity") {
        match serde_json::to_vec(identity) {
            Ok(bytes) => {
                hasher.update(&(bytes.len() as u64).to_le_bytes());
                hasher.update(&bytes);
            },
            Err(_) => {
                hasher.update(&0u64.to_le_bytes());
            },
        };
    }
    hasher.finalize().to_hex().to_string()
}

fn build_manifest(
    storage: &dyn MemoryStorage,
    records: &[MoveableDefinitionRecord],
    candidates: &[MemoryCandidateDocument],
    source_hashes: BTreeMap<String, String>,
    embedding: MemoryEmbeddingManifest,
    chunk_count: usize,
) -> MemoryIndexManifest {
    let (principal, workspace) = storage
        .scope_segments()
        .map(|(principal, workspace)| (Some(principal), Some(workspace)))
        .unwrap_or((None, None));
    let mut count_by_agent = BTreeMap::<String, usize>::new();
    for candidate in candidates {
        if let Some(agent_id) = candidate.agent_id.as_ref() {
            *count_by_agent.entry(agent_id.clone()).or_default() += 1;
        }
    }
    let agents = records
        .iter()
        .map(|record| MemoryIndexAgentSummary {
            agent_id: record.agent_id.clone(),
            document_count: count_by_agent.get(&record.agent_id).copied().unwrap_or(0),
        })
        .collect::<Vec<_>>();

    MemoryIndexManifest {
        index_version: MEMORY_INDEX_VERSION.to_string(),
        schema_version: MEMORY_INDEX_SCHEMA_VERSION,
        backend: MEMORY_INDEX_BACKEND.to_string(),
        backend_status: format!(
            "ready:{MEMORY_INDEX_DOCUMENTS_BACKEND}+lancedb_fts+{}_embedding",
            embedding.provider
        ),
        embedding_provider: embedding.provider,
        embedding_model: embedding.model,
        embedding_dimensions: embedding.dimensions,
        embedding_contract_id: embedding.contract_id,
        embedding_fallback_reason: embedding.fallback_reason,
        principal,
        workspace,
        rebuilt_at: Utc::now(),
        document_count: candidates.len(),
        chunk_count,
        source_count: source_hashes.len(),
        source_hashes,
        agents,
    }
}

fn manifest_embedding_matches(
    manifest: &MemoryIndexManifest,
    embedding: &MemoryEmbeddingManifest,
) -> bool {
    manifest.embedding_provider == embedding.provider
        && manifest.embedding_model == embedding.model
        && manifest.embedding_dimensions == embedding.dimensions
        && manifest.embedding_contract_id == embedding.contract_id
        && manifest.embedding_fallback_reason == embedding.fallback_reason
}

fn memory_embedding_manifests_match(
    left: &MemoryEmbeddingManifest,
    right: &MemoryEmbeddingManifest,
) -> bool {
    left.provider == right.provider
        && left.model == right.model
        && left.dimensions == right.dimensions
        && left.contract_id == right.contract_id
        && left.fallback_reason == right.fallback_reason
}

fn manifest_lancedb_merge_blocker(
    manifest: &MemoryIndexManifest,
    embedding: &MemoryEmbeddingManifest,
) -> Option<&'static str> {
    if manifest.index_version != MEMORY_INDEX_VERSION {
        return Some("index_version_changed");
    }
    if manifest.schema_version != MEMORY_INDEX_SCHEMA_VERSION {
        return Some("schema_version_changed");
    }
    if manifest.backend != MEMORY_INDEX_BACKEND {
        return Some("backend_changed");
    }
    if !manifest_embedding_matches(manifest, embedding) {
        return Some("embedding_config_changed");
    }
    None
}

async fn load_memory_index_manifest(
    storage: &dyn MemoryStorage,
) -> Result<Option<MemoryIndexManifest>> {
    let path = storage.memory_index_manifest_path();
    if !fs::try_exists(&path)
        .await
        .with_context(|| format!("checking memory index manifest {}", path.display()))?
    {
        return Ok(None);
    }
    let raw = storage
        .read_json_value(&path)
        .await
        .with_context(|| format!("reading memory index manifest {}", path.display()))?;
    let manifest: MemoryIndexManifest = serde_json::from_value(raw)
        .with_context(|| format!("parsing memory index manifest {}", path.display()))?;
    Ok(Some(manifest))
}

async fn current_app_memory_projection_revision(
    storage: &dyn MemoryStorage,
) -> Result<Option<String>> {
    Ok(load_app_memory_index_projection(storage)
        .await?
        .map(|projection| projection.projection_digest))
}

async fn ensure_app_memory_projection_revision(
    storage: &dyn MemoryStorage,
    expected: &Option<String>,
) -> Result<()> {
    let current = current_app_memory_projection_revision(storage).await?;
    if &current != expected {
        anyhow::bail!("app-memory destination projection changed during index materialization");
    }
    Ok(())
}

async fn write_documents(
    storage: &dyn MemoryStorage,
    path: &Path,
    candidates: &[MemoryCandidateDocument],
) -> Result<()> {
    let mut bytes = Vec::new();
    for candidate in candidates {
        serde_json::to_writer(&mut bytes, candidate)?;
        bytes.push(b'\n');
    }
    storage.write_bytes_atomic(path, &bytes).await?;
    Ok(())
}

async fn load_memory_index_documents(
    storage: &dyn MemoryStorage,
) -> Result<Vec<MemoryCandidateDocument>> {
    let path = storage.memory_index_documents_path();
    let content = fs::read_to_string(&path)
        .await
        .with_context(|| format!("reading memory index documents {}", path.display()))?;
    let mut documents = Vec::new();
    for (index, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        documents.push(
            serde_json::from_str::<MemoryCandidateDocument>(line)
                .with_context(|| format!("parsing memory index document line {}", index + 1))?,
        );
    }
    Ok(documents)
}

async fn write_lancedb_index(
    storage: &dyn MemoryStorage,
    candidates: &[MemoryCandidateDocument],
    allow_replacement: bool,
    app_projection_revision: &Option<String>,
) -> Result<MemoryLanceDbWriteOutcome> {
    ensure_app_memory_projection_revision(storage, app_projection_revision).await?;
    let index_dir = storage.memory_lancedb_index_dir();
    let embedding_config = MemoryEmbeddingConfig::from_env();
    let existing_manifest = load_memory_index_manifest(storage).await;
    let expected_embedding = embedding_config.empty_manifest();
    let manifest_allows_initial_creation = match &existing_manifest {
        Ok(None) => true,
        Ok(Some(manifest)) => {
            manifest.document_count == 0
                && manifest_lancedb_merge_blocker(manifest, &expected_embedding).is_none()
        },
        Err(_) => false,
    };
    let contains_any_lancedb_table = if manifest_allows_initial_creation {
        lancedb_index_contains_any_table(&index_dir).await?
    } else {
        true
    };
    let safe_initial_creation = derived_metadata_allows_initial_creation(
        manifest_allows_initial_creation,
        contains_any_lancedb_table,
    );
    if candidates.is_empty() && (allow_replacement || safe_initial_creation) {
        // Drop idle handles before deleting the live directory. Invalidate
        // again after recreate so a miss-path open during the window cannot
        // recycle a handle onto the empty dir.
        invalidate_lance_table_pool(&index_dir);
        if fs::try_exists(&index_dir)
            .await
            .with_context(|| format!("checking existing LanceDB index {}", index_dir.display()))?
        {
            fs::remove_dir_all(&index_dir).await.with_context(|| {
                format!("removing existing LanceDB index {}", index_dir.display())
            })?;
        }
        fs::create_dir_all(&index_dir)
            .await
            .with_context(|| format!("creating empty LanceDB index dir {}", index_dir.display()))?;
        invalidate_lance_table_pool(&index_dir);
        return Ok(MemoryLanceDbWriteOutcome {
            embedding: embedding_config.empty_manifest(),
            chunk_count: 0,
            report: MemoryLanceDbWriteReport {
                mode: if safe_initial_creation && !allow_replacement {
                    "create_empty".to_string()
                } else {
                    "empty".to_string()
                },
                chunk_count: 0,
                source_rows: Some(0),
                embedded_rows: Some(0),
                reused_rows: Some(0),
                inserted_rows: Some(0),
                updated_rows: Some(0),
                deleted_rows: Some(0),
                replacement_reason: None,
                optimize: MemoryLanceDbOptimizeReport::not_attempted("empty_candidate_set", 0, 0),
            },
        });
    }

    let chunks = build_memory_index_chunks(candidates);
    // Dedup by `chunk_key` — same-key duplicates (observed for e.g.
    // `agent:personal-assistant::entities.entities:name:dashboard#chunk:0000`
    // when the same memory tier emits the same entity from two
    // overlapping consolidation passes) make the LanceDB
    // `merge_insert` reject the batch with "Ambiguous merge inserts
    // are prohibited: multiple source rows match the same target row".
    // Without dedup the warning fires every consolidation cycle, the
    // incremental merge fails, and we fall back to full-index
    // replacement — which works but is wasteful and noisy. Keep the
    // last occurrence (matches the merge's "newer row wins" intent).
    let chunks = dedupe_memory_index_chunks(chunks);
    let mut replacement_reason = match existing_manifest {
        Ok(Some(manifest)) => {
            manifest_lancedb_merge_blocker(&manifest, &expected_embedding).map(|reason| {
                info!(
                    target: "memory_index",
                    index_dir = %index_dir.display(),
                    reason = reason,
                    previous_index_version = %manifest.index_version,
                    previous_schema_version = manifest.schema_version,
                    previous_backend = %manifest.backend,
                    previous_provider = %manifest.embedding_provider,
                    previous_model = %manifest.embedding_model.as_deref().unwrap_or(""),
                    previous_dimensions = manifest.embedding_dimensions,
                    previous_contract_id = %manifest.embedding_contract_id,
                    new_index_version = MEMORY_INDEX_VERSION,
                    new_schema_version = MEMORY_INDEX_SCHEMA_VERSION,
                    new_backend = MEMORY_INDEX_BACKEND,
                    new_provider = %expected_embedding.provider,
                    new_model = %expected_embedding.model.as_deref().unwrap_or(""),
                    new_dimensions = expected_embedding.dimensions,
                    new_contract_id = %expected_embedding.contract_id,
                    "Existing memory index metadata differs; replacing LanceDB table instead of merging rows"
                );
                reason.to_string()
            })
        },
        Ok(None) if safe_initial_creation => {
            info!(
                target: "memory_index",
                index_dir = %index_dir.display(),
                "Memory index has no manifest or existing LanceDB table; creating its initial derived index"
            );
            None
        },
        Ok(None) => {
            warn!(
                target: "memory_index",
                index_dir = %index_dir.display(),
                "Memory index manifest is missing while LanceDB contains table data; refusing runtime replacement"
            );
            Some("missing_manifest".to_string())
        },
        Err(error) => {
            warn!(
                target: "memory_index",
                index_dir = %index_dir.display(),
                error = %format_error_chain(&error),
                "Existing memory index manifest could not be read; replacing LanceDB table instead of merging rows"
            );
            Some("manifest_unreadable".to_string())
        },
    };

    if replacement_reason.is_some() && !allow_replacement {
        anyhow::bail!(
            "runtime memory index reconciliation requires explicit rebuild: {}",
            replacement_reason
                .as_deref()
                .unwrap_or("incompatible_index")
        );
    }

    if replacement_reason.is_none() && !safe_initial_creation {
        match merge_existing_lancedb_index(
            storage,
            &index_dir,
            candidates,
            &chunks,
            &embedding_config,
            &expected_embedding,
            app_projection_revision,
        )
        .await
        {
            Ok(Some(report)) => {
                return Ok(MemoryLanceDbWriteOutcome {
                    embedding: expected_embedding,
                    chunk_count: chunks.len(),
                    report,
                });
            },
            Ok(None) => {},
            Err(error) if allow_replacement => {
                replacement_reason = Some(format_error_chain(&error));
                warn!(
                    target: "memory_index",
                    index_dir = %index_dir.display(),
                    error = %format_error_chain(&error),
                    "Incremental LanceDB memory index merge failed; falling back to full replacement"
                );
            },
            Err(error) => {
                return Err(error).context(
                    "runtime LanceDB memory index merge failed; explicit rebuild required",
                );
            },
        }
    }

    if !allow_replacement && !safe_initial_creation {
        anyhow::bail!(
            "runtime LanceDB memory index is not incrementally compatible; explicit rebuild required"
        );
    }

    let search_texts = chunks
        .iter()
        .map(|chunk| chunk.search_text.clone())
        .collect::<Vec<_>>();
    ensure_app_memory_projection_revision(storage, app_projection_revision).await?;
    let embedding_batch = embedding_config
        .embed_documents_with_cache(storage, &search_texts)
        .await
        .with_context(|| {
            format!(
                "embedding {} memory index chunks for LanceDB index from {} candidates",
                chunks.len(),
                candidates.len()
            )
        })?;
    ensure_app_memory_projection_revision(storage, app_projection_revision).await?;
    let record_batch = lancedb_record_batch(
        candidates,
        &chunks,
        &embedding_batch.vectors,
        embedding_batch.manifest.dimensions,
    )?;

    let replacement_rows = record_batch.num_rows() as u64;
    replace_lancedb_index_dir(&index_dir, record_batch).await?;

    Ok(MemoryLanceDbWriteOutcome {
        embedding: embedding_batch.manifest,
        chunk_count: chunks.len(),
        report: MemoryLanceDbWriteReport {
            mode: if safe_initial_creation {
                "create".to_string()
            } else {
                "replace".to_string()
            },
            chunk_count: chunks.len(),
            source_rows: Some(replacement_rows),
            embedded_rows: Some(replacement_rows),
            reused_rows: Some(0),
            inserted_rows: None,
            updated_rows: None,
            deleted_rows: None,
            replacement_reason,
            optimize: MemoryLanceDbOptimizeReport::not_attempted(
                "full_replacement_builds_fresh_index",
                0,
                replacement_rows,
            ),
        },
    })
}

async fn replace_lancedb_index_dir(index_dir: &Path, record_batch: RecordBatch) -> Result<()> {
    let tmp_dir = temporary_lancedb_index_dir(index_dir)?;
    if fs::try_exists(&tmp_dir)
        .await
        .with_context(|| format!("checking temporary LanceDB index {}", tmp_dir.display()))?
    {
        fs::remove_dir_all(&tmp_dir)
            .await
            .with_context(|| format!("removing temporary LanceDB index {}", tmp_dir.display()))?;
    }
    fs::create_dir_all(&tmp_dir)
        .await
        .with_context(|| format!("creating LanceDB index dir {}", tmp_dir.display()))?;

    let build_result: Result<()> = async {
        let index_uri = lancedb_uri(&tmp_dir);
        let db = connect(&index_uri).execute().await?;
        let table = db
            .create_table(MEMORY_LANCEDB_TABLE, record_batch)
            .execute()
            .await
            .context("creating LanceDB memory candidate table")?;
        ensure_lancedb_memory_indexes(&table, &tmp_dir).await?;
        Ok(())
    }
    .await;
    if let Err(error) = build_result {
        remove_lancedb_tmp_dir_best_effort(&tmp_dir).await;
        return Err(error);
    }
    // Idle search tables still point at this directory. Invalidate before
    // deleting it, then again after promote so a handle opened during the
    // replace window cannot re-enter the pool.
    invalidate_lance_table_pool(index_dir);
    if fs::try_exists(index_dir)
        .await
        .with_context(|| format!("checking existing LanceDB index {}", index_dir.display()))?
    {
        fs::remove_dir_all(index_dir)
            .await
            .with_context(|| format!("removing existing LanceDB index {}", index_dir.display()))?;
    }
    fs::rename(&tmp_dir, index_dir).await.with_context(|| {
        format!(
            "promoting LanceDB memory index {} -> {}",
            tmp_dir.display(),
            index_dir.display()
        )
    })?;
    invalidate_lance_table_pool(index_dir);
    Ok(())
}

/// Invalidate pooled search tables if a Lance write may already have
/// committed, including when the surrounding function returns `Err` after
/// `merge_insert` / `delete`.
struct InvalidateLancePoolOnDrop {
    index_dir: PathBuf,
    armed: bool,
}

impl InvalidateLancePoolOnDrop {
    fn for_dir(index_dir: &Path) -> Self {
        Self {
            index_dir: index_dir.to_path_buf(),
            armed: false,
        }
    }

    fn arm(&mut self) {
        self.armed = true;
    }
}

impl Drop for InvalidateLancePoolOnDrop {
    fn drop(&mut self) {
        if self.armed {
            invalidate_lance_table_pool(&self.index_dir);
        }
    }
}

async fn merge_existing_lancedb_index(
    storage: &dyn MemoryStorage,
    index_dir: &Path,
    candidates: &[MemoryCandidateDocument],
    chunks: &[MemoryIndexChunkDocument],
    embedding_config: &MemoryEmbeddingConfig,
    embedding: &MemoryEmbeddingManifest,
    app_projection_revision: &Option<String>,
) -> Result<Option<MemoryLanceDbWriteReport>> {
    let Some(table) = open_existing_lancedb_memory_table(index_dir).await? else {
        info!(
            target: "memory_index",
            index_dir = %index_dir.display(),
            "Existing LanceDB memory index has no candidate table; full replacement required"
        );
        return Ok(None);
    };
    let schema = table
        .schema()
        .await
        .context("reading existing LanceDB memory candidate table schema")?;
    if !lancedb_schema_supports_incremental_merge(schema.as_ref(), embedding.dimensions) {
        info!(
            target: "memory_index",
            index_dir = %index_dir.display(),
            embedding_dims = embedding.dimensions,
            "Existing LanceDB memory index schema is incompatible with row-level merge; full replacement required"
        );
        return Ok(None);
    }

    let indexes = table
        .list_indices()
        .await
        .context("listing existing LanceDB memory table indexes")?;
    if !has_lancedb_single_column_index(&indexes, MEMORY_LANCEDB_CHUNK_KEY_COLUMN, IndexType::BTree)
        || !has_lancedb_single_column_index(
            &indexes,
            MEMORY_LANCEDB_CANDIDATE_KEY_COLUMN,
            IndexType::BTree,
        )
        || !has_lancedb_single_column_index(&indexes, MEMORY_LANCEDB_FTS_COLUMN, IndexType::FTS)
    {
        info!(
            target: "memory_index",
            index_dir = %index_dir.display(),
            "Existing LanceDB memory index is unhealthy; full replacement required"
        );
        return Ok(None);
    }

    // Read just the metadata needed to decide whether a row is reusable. This
    // deliberately avoids materializing the FixedSizeList embedding column.
    let existing_rows = load_lancedb_chunk_fingerprints(&table).await?;
    let fingerprints = memory_index_chunk_fingerprints(candidates, chunks)?;
    let plan = plan_incremental_lancedb_write(chunks, &fingerprints, &existing_rows)?;
    let changed_chunks = plan
        .changed_chunk_indexes
        .iter()
        .map(|index| chunks[*index].clone())
        .collect::<Vec<_>>();
    let changed_search_texts = changed_chunks
        .iter()
        .map(|chunk| chunk.search_text.clone())
        .collect::<Vec<_>>();
    ensure_app_memory_projection_revision(storage, app_projection_revision).await?;
    let changed_embeddings = embedding_config
        .embed_documents_with_cache(storage, &changed_search_texts)
        .await
        .with_context(|| {
            format!(
                "embedding {} changed memory index chunks for incremental LanceDB write",
                changed_chunks.len()
            )
        })?;
    ensure_app_memory_projection_revision(storage, app_projection_revision).await?;
    if !memory_embedding_manifests_match(&changed_embeddings.manifest, embedding) {
        anyhow::bail!(
            "incremental LanceDB embedding manifest changed during rebuild: expected provider={} model={} dimensions={} contract={}, got provider={} model={} dimensions={} contract={}",
            embedding.provider,
            embedding.model.as_deref().unwrap_or(""),
            embedding.dimensions,
            embedding.contract_id,
            changed_embeddings.manifest.provider,
            changed_embeddings.manifest.model.as_deref().unwrap_or(""),
            changed_embeddings.manifest.dimensions,
            changed_embeddings.manifest.contract_id,
        );
    }

    let mut inserted_rows = 0u64;
    let mut updated_rows = 0u64;
    let mut merge_attempts = 0u32;
    let mut merge_version = None;
    let mut invalidate_pool = InvalidateLancePoolOnDrop::for_dir(index_dir);
    if !changed_chunks.is_empty() {
        let record_batch = lancedb_record_batch(
            candidates,
            &changed_chunks,
            &changed_embeddings.vectors,
            embedding.dimensions,
        )?;
        let mut merge_insert = table.merge_insert(&[MEMORY_LANCEDB_CHUNK_KEY_COLUMN]);
        merge_insert
            .when_matched_update_all(Some(format!(
                "target.{MEMORY_LANCEDB_ROW_HASH_COLUMN} != source.{MEMORY_LANCEDB_ROW_HASH_COLUMN}"
            )))
            .when_not_matched_insert_all();
        invalidate_pool.arm();
        let result = merge_insert
            .execute(record_batch_reader(record_batch))
            .await
            .context("merging changed LanceDB memory index rows")?;
        inserted_rows = result.num_inserted_rows;
        updated_rows = result.num_updated_rows;
        merge_attempts = result.num_attempts;
        merge_version = Some(result.version);
    }

    if !plan.deleted_chunk_keys.is_empty() {
        invalidate_pool.arm();
    }
    let deleted_rows = delete_lancedb_chunks(&table, &plan.deleted_chunk_keys).await?;
    info!(
        target: "memory_index",
        index_dir = %index_dir.display(),
        source_rows = chunks.len(),
        unchanged_rows = plan.unchanged_rows,
        embedded_rows = changed_chunks.len(),
        inserted_rows = inserted_rows,
        updated_rows = updated_rows,
        deleted_rows = deleted_rows,
        attempts = merge_attempts,
        version = ?merge_version,
        "Incremental LanceDB memory index merge completed"
    );
    let mutated_rows = inserted_rows
        .saturating_add(updated_rows)
        .saturating_add(deleted_rows);
    if let Err(error) = ensure_lancedb_memory_indexes(&table, index_dir).await {
        warn!(
            target: "memory_index",
            index_dir = %index_dir.display(),
            error = %error,
            "LanceDB memory index post-merge index maintenance failed"
        );
    }
    let optimize = MemoryLanceDbOptimizeReport::not_attempted(
        "explicit_maintenance_only",
        mutated_rows,
        chunks.len() as u64,
    );
    Ok(Some(MemoryLanceDbWriteReport {
        mode: "merge".to_string(),
        chunk_count: chunks.len(),
        source_rows: Some(chunks.len() as u64),
        embedded_rows: Some(changed_chunks.len() as u64),
        reused_rows: Some(plan.unchanged_rows as u64),
        inserted_rows: Some(inserted_rows),
        updated_rows: Some(updated_rows),
        deleted_rows: Some(deleted_rows),
        replacement_reason: None,
        optimize,
    }))
}

async fn write_lancedb_index_delta(
    storage: &dyn MemoryStorage,
    manifest: &MemoryIndexManifest,
    candidates: &[MemoryCandidateDocument],
    affected_candidate_keys: &BTreeSet<String>,
) -> Result<Option<MemoryLanceDbWriteReport>> {
    let embedding_config = MemoryEmbeddingConfig::from_env();
    let expected_embedding = embedding_config.empty_manifest();
    if manifest_lancedb_merge_blocker(manifest, &expected_embedding).is_some() {
        return Ok(None);
    }

    let index_dir = storage.memory_lancedb_index_dir();
    let Some(table) = open_existing_lancedb_memory_table(&index_dir).await? else {
        return Ok(None);
    };
    let schema = table
        .schema()
        .await
        .context("reading existing LanceDB memory candidate table schema")?;
    if !lancedb_schema_supports_incremental_merge(schema.as_ref(), expected_embedding.dimensions) {
        return Ok(None);
    }
    let indexes = table
        .list_indices()
        .await
        .context("listing existing LanceDB memory table indexes")?;
    if !has_lancedb_single_column_index(&indexes, MEMORY_LANCEDB_CHUNK_KEY_COLUMN, IndexType::BTree)
        || !has_lancedb_single_column_index(
            &indexes,
            MEMORY_LANCEDB_CANDIDATE_KEY_COLUMN,
            IndexType::BTree,
        )
        || !has_lancedb_single_column_index(&indexes, MEMORY_LANCEDB_FTS_COLUMN, IndexType::FTS)
    {
        return Ok(None);
    }

    let chunks = dedupe_memory_index_chunks(build_memory_index_chunks(candidates));
    let existing_rows =
        load_lancedb_chunk_fingerprints_for_candidate_keys(&table, affected_candidate_keys).await?;
    let fingerprints = memory_index_chunk_fingerprints(candidates, &chunks)?;
    let plan = plan_incremental_lancedb_write(&chunks, &fingerprints, &existing_rows)?;
    let changed_chunks = plan
        .changed_chunk_indexes
        .iter()
        .map(|index| chunks[*index].clone())
        .collect::<Vec<_>>();
    let changed_search_texts = changed_chunks
        .iter()
        .map(|chunk| chunk.search_text.clone())
        .collect::<Vec<_>>();
    let embedding_batch = embedding_config
        .embed_documents_with_cache(storage, &changed_search_texts)
        .await
        .context("embedding changed memory chunks for source-addressable index update")?;
    if !memory_embedding_manifests_match(&embedding_batch.manifest, &expected_embedding) {
        anyhow::bail!(
            "incremental memory index embedding manifest changed during source-addressable update"
        );
    }

    let mut inserted_rows = 0u64;
    let mut updated_rows = 0u64;
    let mut invalidate_pool = InvalidateLancePoolOnDrop::for_dir(&index_dir);
    if !changed_chunks.is_empty() {
        let record_batch = lancedb_record_batch(
            candidates,
            &changed_chunks,
            &embedding_batch.vectors,
            expected_embedding.dimensions,
        )?;
        let mut merge_insert = table.merge_insert(&[MEMORY_LANCEDB_CHUNK_KEY_COLUMN]);
        merge_insert
            .when_matched_update_all(Some(format!(
                "target.{MEMORY_LANCEDB_ROW_HASH_COLUMN} != source.{MEMORY_LANCEDB_ROW_HASH_COLUMN}"
            )))
            .when_not_matched_insert_all();
        invalidate_pool.arm();
        let result = merge_insert
            .execute(record_batch_reader(record_batch))
            .await
            .context("merging source-addressable LanceDB memory rows")?;
        inserted_rows = result.num_inserted_rows;
        updated_rows = result.num_updated_rows;
    }
    if !plan.deleted_chunk_keys.is_empty() {
        invalidate_pool.arm();
    }
    let deleted_rows = delete_lancedb_chunks(&table, &plan.deleted_chunk_keys).await?;
    let total_chunk_count = table
        .count_rows(None)
        .await
        .context("counting LanceDB rows after source-addressable memory update")?;
    let mutated_rows = inserted_rows
        .saturating_add(updated_rows)
        .saturating_add(deleted_rows);
    if let Err(error) = ensure_lancedb_memory_indexes(&table, &index_dir).await {
        warn!(
            target: "memory_index",
            index_dir = %index_dir.display(),
            error = %error,
            "LanceDB memory index post-delta index maintenance failed"
        );
    }
    let optimize = MemoryLanceDbOptimizeReport::not_attempted(
        "explicit_maintenance_only",
        mutated_rows,
        total_chunk_count as u64,
    );
    drop(invalidate_pool);
    Ok(Some(MemoryLanceDbWriteReport {
        mode: "source_delta".to_string(),
        chunk_count: total_chunk_count,
        source_rows: Some(chunks.len() as u64),
        embedded_rows: Some(changed_chunks.len() as u64),
        reused_rows: Some(plan.unchanged_rows as u64),
        inserted_rows: Some(inserted_rows),
        updated_rows: Some(updated_rows),
        deleted_rows: Some(deleted_rows),
        replacement_reason: None,
        optimize,
    }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemoryIndexChunkFingerprint {
    row_hash: String,
    embedding_input_hash: String,
}

#[derive(Debug)]
struct IncrementalLanceDbWritePlan {
    changed_chunk_indexes: Vec<usize>,
    deleted_chunk_keys: Vec<String>,
    unchanged_rows: usize,
}

async fn load_lancedb_chunk_fingerprints(
    table: &Table,
) -> Result<BTreeMap<String, MemoryIndexChunkFingerprint>> {
    let batches = table
        .query()
        .select(Select::columns(&[
            MEMORY_LANCEDB_CHUNK_KEY_COLUMN,
            MEMORY_LANCEDB_ROW_HASH_COLUMN,
            MEMORY_LANCEDB_EMBEDDING_INPUT_HASH_COLUMN,
        ]))
        .execute()
        .await
        .context("reading existing LanceDB memory row fingerprints")?
        .try_collect::<Vec<_>>()
        .await
        .context("collecting existing LanceDB memory row fingerprints")?;

    let mut rows = BTreeMap::new();
    for batch in batches {
        let chunk_keys = required_lancedb_string_column(&batch, MEMORY_LANCEDB_CHUNK_KEY_COLUMN)?;
        let row_hashes = required_lancedb_string_column(&batch, MEMORY_LANCEDB_ROW_HASH_COLUMN)?;
        let embedding_input_hashes =
            required_lancedb_string_column(&batch, MEMORY_LANCEDB_EMBEDDING_INPUT_HASH_COLUMN)?;
        for row in 0..batch.num_rows() {
            if chunk_keys.is_null(row)
                || row_hashes.is_null(row)
                || embedding_input_hashes.is_null(row)
            {
                anyhow::bail!("existing LanceDB memory row fingerprint contains a null value");
            }
            let chunk_key = chunk_keys.value(row).to_string();
            let fingerprint = MemoryIndexChunkFingerprint {
                row_hash: row_hashes.value(row).to_string(),
                embedding_input_hash: embedding_input_hashes.value(row).to_string(),
            };
            if rows.insert(chunk_key.clone(), fingerprint).is_some() {
                anyhow::bail!(
                    "existing LanceDB memory index contains duplicate chunk key {chunk_key}"
                );
            }
        }
    }
    Ok(rows)
}

async fn load_lancedb_chunk_fingerprints_for_candidate_keys(
    table: &Table,
    candidate_keys: &BTreeSet<String>,
) -> Result<BTreeMap<String, MemoryIndexChunkFingerprint>> {
    if candidate_keys.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut rows = BTreeMap::new();
    let candidate_keys = candidate_keys.iter().collect::<Vec<_>>();
    for keys in candidate_keys.chunks(MEMORY_LANCEDB_DELETE_KEY_BATCH_SIZE) {
        let predicate = format!(
            "{MEMORY_LANCEDB_CANDIDATE_KEY_COLUMN} IN ({})",
            keys.iter()
                .map(|key| format!("'{}'", key.replace('\'', "''")))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let batches = table
            .query()
            .only_if(predicate)
            .select(Select::columns(&[
                MEMORY_LANCEDB_CHUNK_KEY_COLUMN,
                MEMORY_LANCEDB_ROW_HASH_COLUMN,
                MEMORY_LANCEDB_EMBEDDING_INPUT_HASH_COLUMN,
            ]))
            .execute()
            .await
            .context("reading affected LanceDB memory row fingerprints")?
            .try_collect::<Vec<_>>()
            .await
            .context("collecting affected LanceDB memory row fingerprints")?;

        for batch in batches {
            let chunk_keys =
                required_lancedb_string_column(&batch, MEMORY_LANCEDB_CHUNK_KEY_COLUMN)?;
            let row_hashes =
                required_lancedb_string_column(&batch, MEMORY_LANCEDB_ROW_HASH_COLUMN)?;
            let embedding_input_hashes =
                required_lancedb_string_column(&batch, MEMORY_LANCEDB_EMBEDDING_INPUT_HASH_COLUMN)?;
            for row in 0..batch.num_rows() {
                if chunk_keys.is_null(row)
                    || row_hashes.is_null(row)
                    || embedding_input_hashes.is_null(row)
                {
                    anyhow::bail!("affected LanceDB memory row fingerprint contains a null value");
                }
                let chunk_key = chunk_keys.value(row).to_string();
                let fingerprint = MemoryIndexChunkFingerprint {
                    row_hash: row_hashes.value(row).to_string(),
                    embedding_input_hash: embedding_input_hashes.value(row).to_string(),
                };
                if rows.insert(chunk_key.clone(), fingerprint).is_some() {
                    anyhow::bail!(
                        "existing LanceDB memory index contains duplicate affected chunk key {chunk_key}"
                    );
                }
            }
        }
    }
    Ok(rows)
}

fn required_lancedb_string_column<'a>(
    batch: &'a RecordBatch,
    name: &str,
) -> Result<&'a StringArray> {
    string_column(batch, name)
        .with_context(|| format!("existing LanceDB memory row is missing UTF-8 column {name}"))
}

fn memory_index_chunk_fingerprints(
    candidates: &[MemoryCandidateDocument],
    chunks: &[MemoryIndexChunkDocument],
) -> Result<Vec<MemoryIndexChunkFingerprint>> {
    chunks
        .iter()
        .map(|chunk| {
            let candidate = candidates
                .get(chunk.parent_candidate_index)
                .with_context(|| {
                    format!(
                        "memory index chunk {} references missing candidate index {}",
                        chunk.chunk_key, chunk.parent_candidate_index
                    )
                })?;
            let embedding_input_hash = hash_bytes(chunk.search_text.as_bytes());
            Ok(MemoryIndexChunkFingerprint {
                row_hash: memory_index_chunk_row_hash(candidate, chunk, &embedding_input_hash),
                embedding_input_hash,
            })
        })
        .collect()
}

fn plan_incremental_lancedb_write(
    chunks: &[MemoryIndexChunkDocument],
    fingerprints: &[MemoryIndexChunkFingerprint],
    existing_rows: &BTreeMap<String, MemoryIndexChunkFingerprint>,
) -> Result<IncrementalLanceDbWritePlan> {
    if chunks.len() != fingerprints.len() {
        anyhow::bail!(
            "memory index fingerprint count mismatch: {} fingerprints for {} chunks",
            fingerprints.len(),
            chunks.len()
        );
    }
    let mut current_keys = BTreeSet::new();
    let mut changed_chunk_indexes = Vec::new();
    let mut unchanged_rows = 0usize;

    for (index, (chunk, fingerprint)) in chunks.iter().zip(fingerprints).enumerate() {
        current_keys.insert(chunk.chunk_key.clone());
        let unchanged = existing_rows.get(&chunk.chunk_key).is_some_and(|existing| {
            existing.row_hash == fingerprint.row_hash
                && existing.embedding_input_hash == fingerprint.embedding_input_hash
        });
        if unchanged {
            unchanged_rows += 1;
        } else {
            changed_chunk_indexes.push(index);
        }
    }

    let deleted_chunk_keys = existing_rows
        .keys()
        .filter(|chunk_key| !current_keys.contains(*chunk_key))
        .cloned()
        .collect();
    Ok(IncrementalLanceDbWritePlan {
        changed_chunk_indexes,
        deleted_chunk_keys,
        unchanged_rows,
    })
}

async fn delete_lancedb_chunks(table: &Table, chunk_keys: &[String]) -> Result<u64> {
    let mut deleted_rows = 0u64;
    for keys in chunk_keys.chunks(MEMORY_LANCEDB_DELETE_KEY_BATCH_SIZE) {
        let predicate = format!(
            "{MEMORY_LANCEDB_CHUNK_KEY_COLUMN} IN ({})",
            keys.iter()
                .map(|key| format!("'{}'", key.replace('\'', "''")))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let result = table
            .delete(&predicate)
            .await
            .with_context(|| format!("deleting {} removed LanceDB memory chunks", keys.len()))?;
        deleted_rows = deleted_rows.saturating_add(result.num_deleted_rows);
    }
    Ok(deleted_rows)
}

async fn open_existing_lancedb_memory_table(index_dir: &Path) -> Result<Option<Table>> {
    if !fs::try_exists(index_dir)
        .await
        .with_context(|| format!("checking existing LanceDB index {}", index_dir.display()))?
    {
        return Ok(None);
    }

    let index_uri = lancedb_uri(index_dir);
    let db = connect(&index_uri)
        .execute()
        .await
        .with_context(|| format!("opening existing LanceDB memory index {index_uri}"))?;
    let table_names = db
        .table_names()
        .execute()
        .await
        .context("listing LanceDB memory index tables")?;
    if !table_names.iter().any(|name| name == MEMORY_LANCEDB_TABLE) {
        return Ok(None);
    }

    db.open_table(MEMORY_LANCEDB_TABLE)
        .execute()
        .await
        .map(Some)
        .context("opening existing LanceDB memory candidate table")
}

/// Return whether the LanceDB directory contains any table, including tables
/// unknown to this memory-index schema. This distinction is the runtime safety
/// boundary for a missing manifest: an absent/empty database is safe to
/// initialize, while any table means data already exists and replacement must
/// remain an explicit operator action.
async fn lancedb_index_contains_any_table(index_dir: &Path) -> Result<bool> {
    if !fs::try_exists(index_dir)
        .await
        .with_context(|| format!("checking existing LanceDB index {}", index_dir.display()))?
    {
        return Ok(false);
    }

    let index_uri = lancedb_uri(index_dir);
    let db = connect(&index_uri)
        .execute()
        .await
        .with_context(|| format!("opening existing LanceDB memory index {index_uri}"))?;
    let table_names = db
        .table_names()
        .execute()
        .await
        .context("listing LanceDB tables while deciding whether initial creation is safe")?;
    Ok(!table_names.is_empty())
}

fn derived_metadata_allows_initial_creation(
    metadata_allows_initial_creation: bool,
    contains_any_lancedb_table: bool,
) -> bool {
    metadata_allows_initial_creation && !contains_any_lancedb_table
}

#[derive(Debug, Clone)]
struct MemoryLanceDbWriteOutcome {
    embedding: MemoryEmbeddingManifest,
    chunk_count: usize,
    report: MemoryLanceDbWriteReport,
}

async fn score_lancedb_index(
    index_dir: &Path,
    query_text: &str,
    limit: usize,
    generation: &str,
) -> Result<BTreeMap<String, f32>> {
    let timeout_duration = lancedb_retrieval_timeout();
    let index_dir = index_dir.to_path_buf();
    let query_text = query_text.to_string();
    let search_limit = chunk_search_limit(limit);
    let generation = generation.to_string();
    let hits = run_lancedb_search_at_scheduler_root_with_timeout(
        timeout_duration,
        "LanceDB memory FTS index",
        Box::new(move || {
            Box::pin(async move {
                // Cap concurrent request-path searches so a burst can't crowd the runtime.
                // Held only across the search; a closed semaphore (never here) proceeds
                // without a permit. The permit wait is inside the timeout so a queued
                // request can't block past the retrieval budget (falls back to keyword).
                let _permit = acquire_lance_search_permit().await;
                search_lancedb_fts(&index_dir, &query_text, search_limit, None, &generation).await
            })
        }),
    )
    .await?;
    Ok(aggregate_chunk_hits_by_candidate(hits))
}

async fn score_lancedb_hybrid_index(
    index_dir: &Path,
    query_text: &str,
    limit: usize,
    manifest: &MemoryIndexManifest,
    predicate: Option<&str>,
    generation: &str,
    settings: VectorSearchSettings,
) -> Result<(BTreeMap<String, f32>, bool)> {
    let expanded_query = expand_memory_retrieval_query(query_text);
    let embedding = embed_query_for_manifest(&expanded_query, manifest)
        .await
        .context("embedding memory query for LanceDB hybrid search")?;
    let timeout_duration = lancedb_retrieval_timeout();
    let index_dir = index_dir.to_path_buf();
    let search_limit = chunk_search_limit(limit);
    let predicate = predicate.map(str::to_string);
    let generation = generation.to_string();
    let (hits, served_ann_fallback) = run_lancedb_search_at_scheduler_root_with_timeout(
        timeout_duration,
        "LanceDB memory hybrid index",
        Box::new(move || {
            Box::pin(async move {
                // Cap concurrent request-path searches so a burst can't crowd the runtime.
                // Held only across the lance search; a closed semaphore (never here)
                // proceeds without a permit. The permit wait is inside the timeout so a
                // queued request can't block past the retrieval budget (falls back to
                // keyword). Embedding stays before the timeout (a separate LLM call).
                let _permit = acquire_lance_search_permit().await;
                let search_started = Instant::now();
                search_lancedb_hybrid(
                    &index_dir,
                    &expanded_query,
                    search_limit,
                    embedding.as_slice(),
                    predicate.as_deref(),
                    &generation,
                    settings,
                    search_started,
                    timeout_duration,
                )
                .await
            })
        }),
    )
    .await?;
    Ok((aggregate_chunk_hits_by_candidate(hits), served_ann_fallback))
}

/// Add a deliberately small set of retrieval-only synonyms for intents whose
/// natural phrasing often omits the vocabulary used in stored memory. This is
/// not an LLM rewrite and does not alter the user-visible query or prompt.
pub fn expand_memory_retrieval_query(query: &str) -> String {
    let lower = query.to_ascii_lowercase();
    let mut additions = Vec::new();

    let provenance_intent = (lower.contains("where did") && lower.contains("come from"))
        || lower.contains("how do you know")
        || lower.contains("what is the source")
        || lower.contains("what's the source")
        || lower.contains("show provenance");
    if provenance_intent {
        additions.push("source evidence provenance rationale origin");
    }

    let location_intent = lower.contains("where")
        && [" run", "host", "located", "location", "address", "endpoint"]
            .iter()
            .any(|needle| lower.contains(needle));
    if location_intent {
        additions.push("url uri host endpoint address port location");
    }

    if additions.is_empty() {
        query.to_string()
    } else {
        format!("{} {}", query.trim(), additions.join(" "))
    }
}

/// Bound the number of concurrent request-path hybrid/FTS lance searches.
///
/// Request-path search is brute-force flat-KNN + BM25 (no ANN index), so a
/// burst of concurrent requests can crowd the main runtime's blocking pool.
/// A process-lifetime semaphore caps in-flight searches. Boot calls
/// [`configure_lance_search_concurrency`] with
/// `runtime.scale` `lance_search_cost_units` (default 4) before the first
/// acquire. `MAGICIAN_LANCE_SEARCH_CONCURRENCY` is a restart-bound kill switch:
/// a valid positive integer wins over the configured atomic. This gates only
/// request-path search (`score_lancedb_index` / `score_lancedb_hybrid_index`);
/// index maintenance (rebuild/compaction) runs on the `magician-bg` runtime and
/// never passes through these scorers.
pub const DEFAULT_LANCE_SEARCH_CONCURRENCY: usize = 4;

static CONFIGURED_LANCE_SEARCH_CONCURRENCY: AtomicUsize =
    AtomicUsize::new(DEFAULT_LANCE_SEARCH_CONCURRENCY);

/// Record the boot-plan permit count. Must run before the first search acquire.
/// `0` is ignored so the compiled default of 4 remains.
pub fn configure_lance_search_concurrency(n: usize) {
    if n > 0 {
        CONFIGURED_LANCE_SEARCH_CONCURRENCY.store(n, Ordering::SeqCst);
    }
}

/// Resolve permit count: valid env `> 0` wins, else `configured` if `> 0`,
/// else [`DEFAULT_LANCE_SEARCH_CONCURRENCY`].
pub fn resolve_lance_search_concurrency(env: Option<&str>, configured: usize) -> usize {
    env.and_then(|value| value.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or_else(|| {
            if configured > 0 {
                configured
            } else {
                DEFAULT_LANCE_SEARCH_CONCURRENCY
            }
        })
}

fn lance_search_semaphore() -> &'static tokio::sync::Semaphore {
    static SEM: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    SEM.get_or_init(|| {
        let env = std::env::var("MAGICIAN_LANCE_SEARCH_CONCURRENCY").ok();
        let configured = CONFIGURED_LANCE_SEARCH_CONCURRENCY.load(Ordering::SeqCst);
        let permits = resolve_lance_search_concurrency(env.as_deref(), configured);
        tokio::sync::Semaphore::new(permits)
    })
}

struct LanceSearchPermit {
    _permit: tokio::sync::SemaphorePermit<'static>,
}

impl Drop for LanceSearchPermit {
    fn drop(&mut self) {
        hol_stats::lance_in_flight_end();
    }
}

async fn acquire_lance_search_permit() -> Option<LanceSearchPermit> {
    let wait = hol_stats::OccupancyWait::lance();
    let permit = lance_search_semaphore().acquire().await.ok();
    wait.finish();
    permit.map(|permit| {
        hol_stats::lance_in_flight_begin();
        LanceSearchPermit { _permit: permit }
    })
}

fn lancedb_retrieval_timeout() -> Duration {
    read_positive_u64_env(&["MAGICIAN_MEMORY_LANCEDB_RETRIEVAL_TIMEOUT_MS"])
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(DEFAULT_LANCEDB_RETRIEVAL_TIMEOUT_MS))
}

fn lancedb_health_check_timeout() -> Duration {
    read_positive_u64_env(&["MAGICIAN_MEMORY_LANCEDB_HEALTH_CHECK_TIMEOUT_MS"])
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(DEFAULT_LANCEDB_HEALTH_CHECK_TIMEOUT_MS))
}

fn lancedb_optimize_timeout() -> Duration {
    read_positive_u64_env(&["MAGICIAN_MEMORY_LANCEDB_OPTIMIZE_TIMEOUT_MS"])
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(DEFAULT_LANCEDB_OPTIMIZE_TIMEOUT_MS))
}

fn memory_index_write_lock_timeout() -> Duration {
    read_positive_u64_env(&["MAGICIAN_MEMORY_INDEX_WRITE_LOCK_TIMEOUT_MS"])
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(DEFAULT_MEMORY_INDEX_WRITE_LOCK_TIMEOUT_MS))
}

fn memory_index_lock_path(storage: &dyn MemoryStorage, file_name: &str) -> Result<PathBuf> {
    let manifest_path = storage.memory_index_manifest_path();
    let parent = manifest_path
        .parent()
        .context("memory index manifest path has no parent")?;
    Ok(parent.join(file_name))
}

async fn acquire_memory_index_file_lock(
    storage: &dyn MemoryStorage,
    file_name: &str,
    operation: &'static str,
    timeout_duration: Duration,
) -> Result<MemoryIndexFileLock> {
    let lock_path = memory_index_lock_path(storage, file_name)?;
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("creating memory index lock dir {}", parent.display()))?;
    }

    let started = Instant::now();
    loop {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("opening {operation} lock {}", lock_path.display()))?;
        match file.try_lock_exclusive() {
            Ok(()) => {
                return Ok(MemoryIndexFileLock {
                    file,
                    path: lock_path,
                    operation,
                })
            },
            Err(error)
                if error.kind() == ErrorKind::WouldBlock
                    || error.kind() == ErrorKind::Interrupted =>
            {
                if started.elapsed() >= timeout_duration {
                    return Err(anyhow!(
                        "timed out after {}ms acquiring {operation} lock {}",
                        timeout_duration.as_millis(),
                        lock_path.display()
                    ));
                }
                sleep(Duration::from_millis(
                    DEFAULT_MEMORY_INDEX_WRITE_LOCK_POLL_MS,
                ))
                .await;
            },
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("acquiring {operation} lock {}", lock_path.display())
                });
            },
        }
    }
}

async fn acquire_memory_index_write_lock(
    storage: &dyn MemoryStorage,
) -> Result<MemoryIndexFileLock> {
    acquire_memory_index_file_lock(
        storage,
        MEMORY_INDEX_WRITE_LOCK_FILE,
        "LanceDB memory index write",
        memory_index_write_lock_timeout(),
    )
    .await
}

async fn acquire_memory_index_change_journal_lock(
    storage: &dyn MemoryStorage,
) -> Result<MemoryIndexFileLock> {
    acquire_memory_index_file_lock(
        storage,
        MEMORY_INDEX_CHANGE_JOURNAL_LOCK_FILE,
        "memory index change journal",
        MEMORY_INDEX_CHANGE_JOURNAL_LOCK_TIMEOUT,
    )
    .await
}

fn memory_query_embedding_timeout(fallback: Duration) -> Duration {
    read_positive_u64_env(&["MAGICIAN_MEMORY_QUERY_EMBEDDING_TIMEOUT_MS"])
        .or_else(ollama_keep_alive::default_embedding_query_timeout_ms)
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(DEFAULT_OLLAMA_QUERY_TIMEOUT_MS))
        .min(fallback)
}

fn read_positive_u64_env(names: &[&str]) -> Option<u64> {
    names.iter().find_map(|name| {
        env::var(name)
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .filter(|value| *value > 0)
    })
}

fn read_positive_usize_env(names: &[&str]) -> Option<usize> {
    names.iter().find_map(|name| {
        env::var(name)
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .filter(|value| *value > 0)
    })
}

fn read_usize_env(names: &[&str]) -> Option<usize> {
    names.iter().find_map(|name| {
        env::var(name)
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
    })
}

fn read_bool_env(names: &[&str]) -> Option<bool> {
    names.iter().find_map(|name| {
        let raw = env::var(name).ok()?;
        match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" | "disabled" => Some(false),
            _ => None,
        }
    })
}

fn read_nonempty_string_env(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

#[derive(Debug, Clone)]
struct MemorySearchHit {
    candidate_key: String,
    score: f32,
}

fn chunk_search_limit(limit: usize) -> usize {
    limit
        .max(1)
        .saturating_mul(MEMORY_INDEX_CHUNK_SEARCH_MULTIPLIER)
}

fn aggregate_chunk_hits_by_candidate(hits: Vec<MemorySearchHit>) -> BTreeMap<String, f32> {
    let mut scores = BTreeMap::<String, f32>::new();
    for hit in hits {
        scores
            .entry(hit.candidate_key)
            .and_modify(|score| {
                if hit.score > *score {
                    *score = hit.score;
                }
            })
            .or_insert(hit.score);
    }
    scores
}

#[derive(Debug, Clone)]
struct MemoryIndexChunkDocument {
    parent_candidate_index: usize,
    parent_candidate_key: String,
    chunk_key: String,
    chunk_index: usize,
    char_start: usize,
    char_end: usize,
    chunk_text: String,
    search_text: String,
}

fn build_memory_index_chunks(
    candidates: &[MemoryCandidateDocument],
) -> Vec<MemoryIndexChunkDocument> {
    let mut chunks = Vec::with_capacity(candidates.len());
    for (candidate_index, candidate) in candidates.iter().enumerate() {
        let parent_candidate_key = memory_candidate_index_key(candidate);
        let index_text = memory_candidate_index_text(candidate);
        let ranges = memory_index_chunk_ranges(&index_text);
        for (chunk_index, range) in ranges.into_iter().enumerate() {
            let chunk_text = index_text[range.byte_start..range.byte_end].to_string();
            let search_text = format!(
                "{} {} {} {} {} {}",
                scope_label(&candidate.scope),
                candidate.agent_id.as_deref().unwrap_or(""),
                candidate.semantic_memory_type.as_str(),
                candidate.tier_name,
                candidate.item_key,
                chunk_text
            );
            chunks.push(MemoryIndexChunkDocument {
                parent_candidate_index: candidate_index,
                parent_candidate_key: parent_candidate_key.clone(),
                chunk_key: format!("{parent_candidate_key}#chunk:{chunk_index:04}"),
                chunk_index,
                char_start: range.char_start,
                char_end: range.char_end,
                chunk_text,
                search_text,
            });
        }
    }
    chunks
}

/// Collapse same-`chunk_key` duplicates to the last occurrence.
///
/// `build_memory_index_chunks` can produce two rows with the same
/// `chunk_key` when two source candidates collapse into the same key
/// (overlapping consolidation passes from different rules on the same
/// agent emit the same `name:dashboard` entity, for example). The
/// LanceDB `merge_insert` keyed on `chunk_key` rejects such batches
/// with "Ambiguous merge inserts are prohibited", forcing a full-index
/// replacement. Pre-collapsing here keeps the incremental path
/// working and silences the operator-visible warning storm.
///
/// "Last write wins" matches the merge's `when_matched_update_all`
/// semantics — newer rows would have overwritten the older one
/// anyway, this just does it client-side before the merge can object.
/// Insertion order across keys is not preserved (HashMap iteration
/// order is non-deterministic); the merge doesn't care about row
/// ordering, only key uniqueness.
fn dedupe_memory_index_chunks(
    chunks: Vec<MemoryIndexChunkDocument>,
) -> Vec<MemoryIndexChunkDocument> {
    let mut latest_by_key: std::collections::HashMap<String, MemoryIndexChunkDocument> =
        std::collections::HashMap::with_capacity(chunks.len());
    for chunk in chunks {
        latest_by_key.insert(chunk.chunk_key.clone(), chunk);
    }
    latest_by_key.into_values().collect()
}

fn memory_candidate_index_text(candidate: &MemoryCandidateDocument) -> String {
    if candidate.tier_name == "task_progress.notes"
        && candidate.text.chars().count() > MEMORY_INDEX_TASK_PROGRESS_DIGEST_CHARS
    {
        return compact_task_progress_note_for_index(&candidate.text);
    }
    candidate.text.clone()
}

fn compact_task_progress_note_for_index(text: &str) -> String {
    if text.chars().count() <= MEMORY_INDEX_TASK_PROGRESS_DIGEST_CHARS {
        return text.to_string();
    }

    let mut parts = Vec::new();
    parts.push(format!("start: {}", truncate_chars(text.trim(), 1_200)));

    let lower = text.to_ascii_lowercase();
    for keyword in [
        "goal reached",
        "cannot proceed",
        "execution cancelled",
        "partial progress",
        "failed",
        "error",
        "success",
        "warning",
    ] {
        if let Some(byte_index) = lower.find(keyword) {
            parts.push(format!(
                "{keyword}: {}",
                excerpt_around_byte(text, byte_index, 900)
            ));
        }
    }

    parts.push(format!("end: {}", tail_chars(text.trim(), 1_200)));
    truncate_chars(
        &dedupe_preserve_order(parts)
            .into_iter()
            .collect::<Vec<_>>()
            .join("\n"),
        MEMORY_INDEX_TASK_PROGRESS_DIGEST_CHARS,
    )
}

fn dedupe_preserve_order(values: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for value in values {
        if seen.insert(value.clone()) {
            out.push(value);
        }
    }
    out
}

fn excerpt_around_byte(text: &str, byte_index: usize, max_chars: usize) -> String {
    let half = max_chars / 2;
    let char_index = text[..byte_index.min(text.len())].chars().count();
    let start = char_index.saturating_sub(half);
    let end = (char_index + half).min(text.chars().count());
    slice_chars(text, start, end)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect()
}

fn tail_chars(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    slice_chars(text, count - max_chars, count)
}

fn slice_chars(text: &str, start_char: usize, end_char: usize) -> String {
    let mut offsets = text.char_indices().map(|(idx, _)| idx).collect::<Vec<_>>();
    offsets.push(text.len());
    let start = offsets
        .get(start_char.min(offsets.len().saturating_sub(1)))
        .copied()
        .unwrap_or(text.len());
    let end = offsets
        .get(end_char.min(offsets.len().saturating_sub(1)))
        .copied()
        .unwrap_or(text.len());
    text[start..end].to_string()
}

#[derive(Debug, Clone, Copy)]
struct MemoryIndexChunkRange {
    char_start: usize,
    char_end: usize,
    byte_start: usize,
    byte_end: usize,
}

fn memory_index_chunk_ranges(text: &str) -> Vec<MemoryIndexChunkRange> {
    let char_count = text.chars().count();
    if char_count <= MEMORY_INDEX_SINGLE_CHUNK_CHAR_LIMIT {
        return vec![MemoryIndexChunkRange {
            char_start: 0,
            char_end: char_count,
            byte_start: 0,
            byte_end: text.len(),
        }];
    }

    let mut offsets = text.char_indices().map(|(idx, _)| idx).collect::<Vec<_>>();
    offsets.push(text.len());
    let mut ranges = Vec::new();
    let mut start = 0usize;
    while start < char_count {
        let end = (start + MEMORY_INDEX_CHUNK_TARGET_CHARS).min(char_count);
        ranges.push(MemoryIndexChunkRange {
            char_start: start,
            char_end: end,
            byte_start: offsets[start],
            byte_end: offsets[end],
        });
        if end == char_count {
            break;
        }
        let next_start = end.saturating_sub(MEMORY_INDEX_CHUNK_OVERLAP_CHARS);
        start = next_start.max(start + 1);
    }
    ranges
}

async fn search_lancedb_fts(
    index_dir: &Path,
    query_text: &str,
    limit: usize,
    predicate: Option<&str>,
    generation: &str,
) -> Result<Vec<MemorySearchHit>> {
    let pooled = checkout_search_table(index_dir, generation).await?;
    let table = pooled.clone_table();
    let result = search_lancedb_fts_on_table(&table, query_text, limit, predicate).await;
    drop(table);
    match result {
        Ok(hits) => {
            pooled.recycle();
            Ok(hits)
        },
        Err(error) => Err(error),
    }
}

async fn search_lancedb_fts_on_table(
    table: &Table,
    query_text: &str,
    limit: usize,
    predicate: Option<&str>,
) -> Result<Vec<MemorySearchHit>> {
    // `_score` is requested EXPLICITLY: relying on LanceDB's automatic
    // score-column projection is deprecated (it warns on every query today),
    // and once it's removed the read below would silently fall back to coarse
    // rank-reciprocal scores instead of real FTS relevance.
    let mut query = table
        .query()
        .full_text_search(FullTextSearchQuery::new(query_text.to_string()));
    if let Some(predicate) = predicate {
        query = query.only_if(predicate.to_string());
    }
    let batches = query
        .limit(limit.max(1))
        .select(Select::columns(&[
            "candidate_key",
            MEMORY_LANCEDB_SCORE_COLUMN,
        ]))
        .execute()
        .await
        .context("searching LanceDB memory FTS index")?
        .try_collect::<Vec<_>>()
        .await
        .context("collecting LanceDB memory FTS results")?;

    let mut hits = Vec::new();
    let mut rank = 0usize;
    for batch in batches {
        let Some(keys) = string_column(&batch, "candidate_key") else {
            continue;
        };
        let score_f32 = batch
            .column_by_name(MEMORY_LANCEDB_SCORE_COLUMN)
            .and_then(|column| column.as_any().downcast_ref::<Float32Array>());
        let score_f64 = batch
            .column_by_name(MEMORY_LANCEDB_SCORE_COLUMN)
            .and_then(|column| column.as_any().downcast_ref::<Float64Array>());
        for row in 0..batch.num_rows() {
            if keys.is_null(row) {
                rank += 1;
                continue;
            }
            let score = score_f32
                .filter(|array| !array.is_null(row))
                .map(|array| array.value(row))
                .or_else(|| {
                    score_f64
                        .filter(|array| !array.is_null(row))
                        .map(|array| array.value(row) as f32)
                })
                .unwrap_or_else(|| 1.0 / (rank + 1) as f32);
            hits.push(MemorySearchHit {
                candidate_key: keys.value(row).to_string(),
                score: score.max(0.0),
            });
            rank += 1;
        }
    }
    Ok(hits)
}

/// Vector leg of the hybrid search: candidate keys in distance order (the
/// rank order is all RRF needs). `_distance` is selected EXPLICITLY — same
/// reasoning as the FTS leg's `_score` (deprecated autoprojection).
///
/// Default mode is exhaustive flat (`bypass_vector_index`). `ann_shadow`
/// still serves that flat key list. `ann` uses IVF_PQ plus exact L2 rerank
/// and falls back to flat if the index is missing or the ANN path errors.
struct VectorLegResult {
    keys: Vec<String>,
    /// True when ANN was requested but exhaustive KNN was served.
    ann_fallback: bool,
    /// Shadow path found IVF and should compare after FTS+flat finish.
    run_shadow_observe: bool,
}

#[cfg(test)]
async fn search_lancedb_vector_on_table(
    table: &Table,
    embedding: &[f32],
    limit: usize,
    predicate: Option<&str>,
) -> Result<Vec<String>> {
    Ok(search_lancedb_vector_on_table_with(
        vector_search_settings(),
        table,
        embedding,
        limit,
        predicate,
        Instant::now(),
        Duration::from_secs(30),
        true,
    )
    .await?
    .keys)
}

async fn search_lancedb_vector_on_table_with(
    settings: VectorSearchSettings,
    table: &Table,
    embedding: &[f32],
    limit: usize,
    predicate: Option<&str>,
    search_started: Instant,
    search_budget: Duration,
    observe_shadow: bool,
) -> Result<VectorLegResult> {
    // Flat never consults IVF; skip list_indices so the default path stays
    // one nearest_to + bypass_vector_index.
    let ivf_present = if settings.mode.maintains_ivf() {
        table_has_ivf_pq(table).await.unwrap_or(false)
    } else {
        false
    };
    let plan = plan_vector_search_with_settings(&settings, limit, ivf_present);
    match plan {
        VectorSearchPlan::Flat { limit } => Ok(VectorLegResult {
            keys: execute_flat_vector_search(table, embedding, limit, predicate).await?,
            ann_fallback: false,
            run_shadow_observe: false,
        }),
        VectorSearchPlan::FlatFallback { limit } => {
            hol_stats::record_ann_fallback();
            Ok(VectorLegResult {
                keys: execute_flat_vector_search(table, embedding, limit, predicate).await?,
                ann_fallback: true,
                run_shadow_observe: false,
            })
        },
        VectorSearchPlan::Shadow { limit, run_ann, .. } => {
            if !run_ann {
                hol_stats::record_ann_fallback();
            }
            let keys = execute_flat_vector_search(table, embedding, limit, predicate).await?;
            if observe_shadow && run_ann {
                record_ann_shadow_compare_if_budget(
                    table,
                    embedding,
                    &keys,
                    limit,
                    predicate,
                    settings,
                    search_started,
                    search_budget,
                )
                .await;
            }
            Ok(VectorLegResult {
                keys,
                ann_fallback: false,
                run_shadow_observe: run_ann,
            })
        },
        VectorSearchPlan::Ann { limit, shortlist } => {
            execute_ann_vector_search(
                table,
                embedding,
                limit,
                shortlist,
                predicate,
                settings.nprobes,
                search_started,
                search_budget,
            )
            .await
        },
    }
}

fn plan_vector_search_with_settings(
    settings: &VectorSearchSettings,
    limit: usize,
    ivf_present: bool,
) -> VectorSearchPlan {
    crate::vector_search_mode::plan_vector_search_with(*settings, limit, ivf_present)
}

async fn table_has_ivf_pq(table: &Table) -> Result<bool> {
    let indexes = table
        .list_indices()
        .await
        .context("listing LanceDB memory table indexes for IVF presence")?;
    Ok(has_lancedb_single_column_index(
        &indexes,
        MEMORY_LANCEDB_VECTOR_COLUMN,
        IndexType::IvfPq,
    ))
}

async fn execute_flat_vector_search(
    table: &Table,
    embedding: &[f32],
    limit: usize,
    predicate: Option<&str>,
) -> Result<Vec<String>> {
    let started = Instant::now();
    let keys = execute_vector_knn(table, embedding, limit, predicate, true, 1).await?;
    hol_stats::record_vector_leg_times(Some(elapsed_ms(started)), None);
    Ok(keys)
}

fn remaining_search_budget(started: Instant, budget: Duration) -> Option<Duration> {
    budget
        .checked_sub(started.elapsed())
        .filter(|remaining| *remaining > Duration::from_millis(25))
}

/// Observe-only ANN compare must not delay the already-served flat+FTS
/// hybrid. Cap the extra wait so a 750ms retrieval budget cannot spend
/// hundreds of milliseconds on a score that is never returned.
const MAX_SHADOW_OBSERVE: Duration = Duration::from_millis(50);
/// Leave this much of the outer Lance timeout so a slow IVF query can
/// fail-closed (empty vector keys or a short flat fallback) and still
/// join FTS instead of aborting the whole hybrid into keyword fallback.
const ANN_HYBRID_SLACK: Duration = Duration::from_millis(80);

fn shadow_observe_budget(started: Instant, budget: Duration) -> Option<Duration> {
    remaining_search_budget(started, budget)
        .map(|remaining| remaining.min(MAX_SHADOW_OBSERVE))
        .filter(|remaining| *remaining > Duration::from_millis(25))
}

fn ann_ivf_budget(started: Instant, budget: Duration) -> Option<Duration> {
    remaining_search_budget(started, budget).and_then(|remaining| {
        let ivf = remaining.saturating_sub(ANN_HYBRID_SLACK);
        (ivf > Duration::from_millis(25)).then_some(ivf)
    })
}

async fn record_ann_shadow_compare_if_budget(
    table: &Table,
    embedding: &[f32],
    flat_keys: &[String],
    limit: usize,
    predicate: Option<&str>,
    settings: VectorSearchSettings,
    search_started: Instant,
    search_budget: Duration,
) {
    let Some(remaining) = shadow_observe_budget(search_started, search_budget) else {
        hol_stats::record_ann_fallback();
        return;
    };
    hol_stats::record_ann_query();
    let ann_started = Instant::now();
    let ann_result = tokio::time::timeout(
        remaining,
        execute_vector_knn(
            table,
            embedding,
            settings.ann_shortlist_limit(limit),
            predicate,
            false,
            settings.nprobes,
        ),
    )
    .await;
    let ann_ms = elapsed_ms(ann_started);
    match ann_result {
        Ok(Ok(ann_keys)) => {
            let recall = recall_at_k(flat_keys, &ann_keys, limit.max(1));
            hol_stats::record_ann_shadow_compare(recall.milles, recall.mismatch, 0, ann_ms);
        },
        Ok(Err(error)) => {
            warn!(
                target: "memory_index",
                error = %error,
                "ANN shadow vector search failed; served flat keys"
            );
            hol_stats::record_ann_fallback();
            hol_stats::record_vector_leg_times(None, Some(ann_ms));
        },
        Err(_) => {
            hol_stats::record_ann_fallback();
            hol_stats::record_vector_leg_times(None, Some(ann_ms));
        },
    }
}

async fn execute_ann_vector_search(
    table: &Table,
    embedding: &[f32],
    limit: usize,
    shortlist: usize,
    predicate: Option<&str>,
    nprobes: usize,
    search_started: Instant,
    search_budget: Duration,
) -> Result<VectorLegResult> {
    hol_stats::record_ann_query();
    let Some(ivf_budget) = ann_ivf_budget(search_started, search_budget) else {
        hol_stats::record_ann_fallback();
        return fallback_ann_to_flat(
            table,
            embedding,
            limit,
            predicate,
            search_started,
            search_budget,
        )
        .await;
    };
    let started = Instant::now();
    let ann_result = tokio::time::timeout(
        ivf_budget,
        execute_vector_knn(table, embedding, shortlist, predicate, false, nprobes),
    )
    .await;
    let ann_ms = elapsed_ms(started);
    let shortlisted = match ann_result {
        Ok(Ok(keys)) if !keys.is_empty() => keys,
        Ok(Ok(_)) => {
            hol_stats::record_ann_fallback();
            hol_stats::record_vector_leg_times(None, Some(ann_ms));
            return fallback_ann_to_flat(
                table,
                embedding,
                limit,
                predicate,
                search_started,
                search_budget,
            )
            .await;
        },
        Ok(Err(error)) => {
            warn!(
                target: "memory_index",
                error = %error,
                "ANN vector search failed; falling back to flat KNN"
            );
            hol_stats::record_ann_fallback();
            hol_stats::record_vector_leg_times(None, Some(ann_ms));
            return fallback_ann_to_flat(
                table,
                embedding,
                limit,
                predicate,
                search_started,
                search_budget,
            )
            .await;
        },
        Err(_) => {
            // IVF consumed the inner budget. Returning empty vector keys
            // lets FTS still fuse; a second unbounded KNN would blow the
            // outer hybrid timeout and discard that FTS result.
            hol_stats::record_ann_fallback();
            hol_stats::record_vector_leg_times(None, Some(ann_ms));
            return Ok(VectorLegResult {
                keys: Vec::new(),
                ann_fallback: true,
                run_shadow_observe: false,
            });
        },
    };
    let rerank_budget = remaining_search_budget(search_started, search_budget);
    let reranked = match rerank_budget {
        Some(budget) => {
            tokio::time::timeout(
                budget,
                exact_rerank_ann_candidates(table, embedding, &shortlisted, limit, predicate),
            )
            .await
        },
        None => {
            hol_stats::record_ann_fallback();
            hol_stats::record_vector_leg_times(None, Some(ann_ms));
            return Ok(VectorLegResult {
                keys: Vec::new(),
                ann_fallback: true,
                run_shadow_observe: false,
            });
        },
    };
    match reranked {
        Ok(Ok(keys)) => {
            hol_stats::record_vector_leg_times(None, Some(ann_ms));
            Ok(VectorLegResult {
                keys,
                ann_fallback: false,
                run_shadow_observe: false,
            })
        },
        Ok(Err(error)) => {
            warn!(
                target: "memory_index",
                error = %error,
                "ANN exact rerank failed; falling back to flat KNN"
            );
            hol_stats::record_ann_fallback();
            fallback_ann_to_flat(
                table,
                embedding,
                limit,
                predicate,
                search_started,
                search_budget,
            )
            .await
        },
        Err(_) => {
            hol_stats::record_ann_fallback();
            hol_stats::record_vector_leg_times(None, Some(ann_ms));
            Ok(VectorLegResult {
                keys: Vec::new(),
                ann_fallback: true,
                run_shadow_observe: false,
            })
        },
    }
}

async fn fallback_ann_to_flat(
    table: &Table,
    embedding: &[f32],
    limit: usize,
    predicate: Option<&str>,
    search_started: Instant,
    search_budget: Duration,
) -> Result<VectorLegResult> {
    if remaining_search_budget(search_started, search_budget).is_none() {
        return Ok(VectorLegResult {
            keys: Vec::new(),
            ann_fallback: true,
            run_shadow_observe: false,
        });
    }
    Ok(VectorLegResult {
        keys: execute_flat_vector_search(table, embedding, limit, predicate).await?,
        ann_fallback: true,
        run_shadow_observe: false,
    })
}

async fn execute_vector_knn(
    table: &Table,
    embedding: &[f32],
    limit: usize,
    predicate: Option<&str>,
    bypass_index: bool,
    nprobes: usize,
) -> Result<Vec<String>> {
    let mut query = table
        .query()
        .nearest_to(embedding)
        .context("building LanceDB memory vector query")?
        .column(MEMORY_LANCEDB_VECTOR_COLUMN);
    if let Some(predicate) = predicate {
        query = query.only_if(predicate.to_string());
    }
    query = if bypass_index {
        query.bypass_vector_index()
    } else {
        query.nprobes(nprobes.max(1))
    };
    let batches = query
        .limit(limit.max(1))
        .select(Select::columns(&[
            "candidate_key",
            MEMORY_LANCEDB_DISTANCE_COLUMN,
        ]))
        .execute()
        .await
        .context("searching LanceDB memory vector index")?
        .try_collect::<Vec<_>>()
        .await
        .context("collecting LanceDB memory vector results")?;
    Ok(candidate_keys_from_batches(&batches))
}

fn candidate_keys_from_batches(batches: &[RecordBatch]) -> Vec<String> {
    let mut keys = Vec::new();
    for batch in batches {
        let Some(column) = string_column(batch, "candidate_key") else {
            continue;
        };
        for row in 0..batch.num_rows() {
            if !column.is_null(row) {
                keys.push(column.value(row).to_string());
            }
        }
    }
    keys
}

async fn exact_rerank_ann_candidates(
    table: &Table,
    query: &[f32],
    shortlist: &[String],
    limit: usize,
    predicate: Option<&str>,
) -> Result<Vec<String>> {
    if shortlist.is_empty() {
        return Ok(Vec::new());
    }
    let filter = candidate_key_in_predicate(shortlist, predicate);
    let batches = table
        .query()
        .only_if(filter)
        .select(Select::columns(&[
            "candidate_key",
            MEMORY_LANCEDB_VECTOR_COLUMN,
        ]))
        .execute()
        .await
        .context("fetching embeddings for ANN exact rerank")?
        .try_collect::<Vec<_>>()
        .await
        .context("collecting embeddings for ANN exact rerank")?;

    let mut ranked: Vec<(f32, String)> = Vec::new();
    for batch in batches {
        let Some(keys) = string_column(&batch, "candidate_key") else {
            continue;
        };
        let Some(vectors) = batch
            .column_by_name(MEMORY_LANCEDB_VECTOR_COLUMN)
            .and_then(|column| column.as_any().downcast_ref::<FixedSizeListArray>())
        else {
            continue;
        };
        for row in 0..batch.num_rows() {
            if keys.is_null(row) {
                continue;
            }
            let Some(vector) = fixed_size_f32_row(vectors, row) else {
                continue;
            };
            ranked.push((
                l2_squared_distance(query, &vector),
                keys.value(row).to_string(),
            ));
        }
    }
    if ranked.is_empty() {
        anyhow::bail!("ANN exact rerank fetched no embeddings");
    }
    ranked.sort_by(|left, right| {
        left.0
            .partial_cmp(&right.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.1.cmp(&right.1))
    });
    ranked.truncate(limit.max(1));
    Ok(ranked.into_iter().map(|(_, key)| key).collect())
}

fn candidate_key_in_predicate(keys: &[String], extra: Option<&str>) -> String {
    let mut unique = Vec::new();
    let mut seen = HashSet::new();
    for key in keys {
        if seen.insert(key.as_str()) {
            unique.push(key.as_str());
        }
    }
    let list = unique
        .into_iter()
        .map(|key| format!("'{}'", key.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(", ");
    let in_pred = format!("{MEMORY_LANCEDB_CANDIDATE_KEY_COLUMN} IN ({list})");
    match extra {
        Some(predicate) if !predicate.is_empty() => format!("({predicate}) AND {in_pred}"),
        _ => in_pred,
    }
}

fn fixed_size_f32_row(list: &FixedSizeListArray, row: usize) -> Option<Vec<f32>> {
    if list.is_null(row) {
        return None;
    }
    let values = list.value(row);
    let floats = values.as_any().downcast_ref::<Float32Array>()?;
    Some((0..floats.len()).map(|idx| floats.value(idx)).collect())
}

fn elapsed_ms(started: Instant) -> usize {
    started.elapsed().as_millis().min(usize::MAX as u128) as usize
}

/// Hybrid search = the two legs run CONCURRENTLY + in-house Reciprocal Rank
/// Fusion, replacing lancedb's built-in hybrid path. That path clones ONE
/// shared projection into both sub-queries, and each leg's projectable schema
/// only contains its own scoring pseudo-column — so an explicit projection
/// can never name them both, forcing the DEPRECATED score autoprojection
/// (which warned twice per recall and will eventually be removed). Fusing
/// here keeps the cheap explicit projections, kills the warnings, and
/// reproduces the built-in scores exactly (same RRF formula, same k, same
/// `MEMORY_HYBRID_SCORE_SCALE`).
async fn search_lancedb_hybrid(
    index_dir: &Path,
    query_text: &str,
    limit: usize,
    embedding: &[f32],
    predicate: Option<&str>,
    generation: &str,
    settings: VectorSearchSettings,
    search_started: Instant,
    search_budget: Duration,
) -> Result<(Vec<MemorySearchHit>, bool)> {
    // Concurrent legs get independent pooled handles. Cloning one `Table` for
    // both would share the inner NativeTable/Dataset across two DataFusion
    // plans; aborting one poll could poison the handle the other still holds.
    // Pair checkout is atomic: both idle or both freshly opened against one
    // directory epoch, so RRF cannot fuse a pre-invalidate snapshot with a
    // post-invalidate open. A cold pool still opens twice; a warm pool none.
    let (vector_guard, fts_guard) = checkout_search_table_pair(index_dir, generation).await?;
    let vector_table = vector_guard.clone_table();
    let fts_table = fts_guard.clone_table();
    let joined = tokio::try_join!(
        search_lancedb_vector_on_table_with(
            settings,
            &vector_table,
            embedding,
            limit,
            predicate,
            search_started,
            search_budget,
            false,
        ),
        search_lancedb_fts_on_table(&fts_table, query_text, limit, predicate),
    );
    let (vector_leg, fts_hits) = match joined {
        Ok(legs) => legs,
        Err(error) => return Err(error),
    };
    let vector_keys = vector_leg.keys;
    let served_ann_fallback = vector_leg.ann_fallback;
    let run_shadow_observe = vector_leg.run_shadow_observe;
    drop((vector_table, fts_table));
    vector_guard.recycle();
    fts_guard.recycle();
    if run_shadow_observe {
        // Observe-only: do not delay the already-served flat+FTS hybrid.
        // DataFusion polls are not reliably cancelled by tokio::time::timeout,
        // so awaiting here can still blow the outer 750ms budget.
        let index_dir = index_dir.to_path_buf();
        let generation = generation.to_string();
        let embedding = embedding.to_vec();
        let served_keys = vector_keys.clone();
        let predicate = predicate.map(str::to_string);
        let task = Box::pin(async move {
            let Ok(guard) = checkout_search_table(&index_dir, &generation).await else {
                return;
            };
            let table = guard.clone_table();
            record_ann_shadow_compare_if_budget(
                &table,
                &embedding,
                &served_keys,
                limit,
                predicate.as_deref(),
                settings,
                search_started,
                search_budget,
            )
            .await;
            drop(table);
            guard.recycle();
        });
        let _ = spawn_on_lance_runtime(task);
    }

    // RRF: score(candidate) = Σ over legs of 1 / (rank + k), 0-based rank —
    // identical to lancedb's RRFReranker. BTreeMap keeps tie ordering
    // deterministic across runs.
    let mut fused: BTreeMap<String, f32> = BTreeMap::new();
    for (rank, key) in vector_keys.into_iter().enumerate() {
        *fused.entry(key).or_default() += 1.0 / (rank as f32 + MEMORY_HYBRID_RRF_K);
    }
    for (rank, hit) in fts_hits.into_iter().enumerate() {
        *fused.entry(hit.candidate_key).or_default() += 1.0 / (rank as f32 + MEMORY_HYBRID_RRF_K);
    }

    let mut hits: Vec<MemorySearchHit> = fused
        .into_iter()
        .map(|(candidate_key, score)| MemorySearchHit {
            candidate_key,
            score: score * MEMORY_HYBRID_SCORE_SCALE,
        })
        .collect();
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.candidate_key.cmp(&b.candidate_key))
    });
    hits.truncate(limit.max(1));
    Ok((hits, served_ann_fallback))
}

fn lancedb_record_batch(
    candidates: &[MemoryCandidateDocument],
    chunks: &[MemoryIndexChunkDocument],
    embeddings_by_chunk: &[Vec<f32>],
    embedding_dims: usize,
) -> Result<RecordBatch> {
    if embeddings_by_chunk.len() != chunks.len() {
        anyhow::bail!(
            "memory index embedding count mismatch: {} embeddings for {} chunks",
            embeddings_by_chunk.len(),
            chunks.len()
        );
    }
    let schema = Arc::new(Schema::new(vec![
        Field::new(MEMORY_LANCEDB_CHUNK_KEY_COLUMN, DataType::Utf8, false),
        Field::new(MEMORY_LANCEDB_ROW_HASH_COLUMN, DataType::Utf8, false),
        Field::new(
            MEMORY_LANCEDB_EMBEDDING_INPUT_HASH_COLUMN,
            DataType::Utf8,
            false,
        ),
        Field::new("candidate_key", DataType::Utf8, false),
        Field::new("chunk_index", DataType::UInt32, false),
        Field::new("char_start", DataType::UInt32, false),
        Field::new("char_end", DataType::UInt32, false),
        Field::new("scope", DataType::Utf8, false),
        Field::new("agent_id", DataType::Utf8, false),
        Field::new("goal_id", DataType::Utf8, false),
        Field::new("tier_name", DataType::Utf8, false),
        Field::new("item_key", DataType::Utf8, false),
        Field::new("chunk_text", DataType::Utf8, false),
        Field::new(MEMORY_LANCEDB_FTS_COLUMN, DataType::Utf8, false),
        Field::new(
            MEMORY_LANCEDB_VECTOR_COLUMN,
            DataType::FixedSizeList(
                Arc::new(Field::new("item", DataType::Float32, true)),
                embedding_dims as i32,
            ),
            false,
        ),
    ]));
    let mut chunk_keys = Vec::with_capacity(chunks.len());
    let mut row_hashes = Vec::with_capacity(chunks.len());
    let mut embedding_input_hashes = Vec::with_capacity(chunks.len());
    let mut candidate_keys = Vec::with_capacity(chunks.len());
    let mut chunk_indexes = Vec::with_capacity(chunks.len());
    let mut char_starts = Vec::with_capacity(chunks.len());
    let mut char_ends = Vec::with_capacity(chunks.len());
    let mut scopes = Vec::with_capacity(chunks.len());
    let mut agent_ids = Vec::with_capacity(chunks.len());
    let mut goal_ids = Vec::with_capacity(chunks.len());
    let mut tier_names = Vec::with_capacity(chunks.len());
    let mut item_keys = Vec::with_capacity(chunks.len());
    let mut chunk_texts = Vec::with_capacity(chunks.len());
    let mut stored_search_texts = Vec::with_capacity(chunks.len());
    let mut embeddings = Vec::with_capacity(chunks.len() * embedding_dims);
    let fingerprints = memory_index_chunk_fingerprints(candidates, chunks)?;

    for (idx, (chunk, fingerprint)) in chunks.iter().zip(fingerprints.iter()).enumerate() {
        let candidate = candidates
            .get(chunk.parent_candidate_index)
            .with_context(|| {
                format!(
                    "memory index chunk {} references missing candidate index {}",
                    chunk.chunk_key, chunk.parent_candidate_index
                )
            })?;
        let embedding = &embeddings_by_chunk[idx];
        if embedding.len() != embedding_dims {
            anyhow::bail!(
                "memory index embedding dimension mismatch for chunk {}: expected {}, got {}",
                chunk.chunk_key,
                embedding_dims,
                embedding.len()
            );
        }
        chunk_keys.push(chunk.chunk_key.clone());
        row_hashes.push(fingerprint.row_hash.clone());
        embedding_input_hashes.push(fingerprint.embedding_input_hash.clone());
        candidate_keys.push(chunk.parent_candidate_key.clone());
        chunk_indexes.push(clamp_usize_to_u32(chunk.chunk_index));
        char_starts.push(clamp_usize_to_u32(chunk.char_start));
        char_ends.push(clamp_usize_to_u32(chunk.char_end));
        scopes.push(scope_label(&candidate.scope).to_string());
        agent_ids.push(candidate.agent_id.clone().unwrap_or_default());
        goal_ids.push(candidate.goal_id.clone().unwrap_or_default());
        tier_names.push(candidate.tier_name.clone());
        item_keys.push(candidate.item_key.clone());
        chunk_texts.push(chunk.chunk_text.clone());
        embeddings.extend(embedding.iter().copied());
        stored_search_texts.push(chunk.search_text.clone());
    }

    let embedding_values: ArrayRef = Arc::new(Float32Array::from(embeddings));
    let embedding_array = FixedSizeListArray::try_new(
        Arc::new(Field::new("item", DataType::Float32, true)),
        embedding_dims as i32,
        embedding_values,
        None,
    )
    .context("building LanceDB memory embedding array")?;

    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(chunk_keys)),
            Arc::new(StringArray::from(row_hashes)),
            Arc::new(StringArray::from(embedding_input_hashes)),
            Arc::new(StringArray::from(candidate_keys)),
            Arc::new(UInt32Array::from(chunk_indexes)),
            Arc::new(UInt32Array::from(char_starts)),
            Arc::new(UInt32Array::from(char_ends)),
            Arc::new(StringArray::from(scopes)),
            Arc::new(StringArray::from(agent_ids)),
            Arc::new(StringArray::from(goal_ids)),
            Arc::new(StringArray::from(tier_names)),
            Arc::new(StringArray::from(item_keys)),
            Arc::new(StringArray::from(chunk_texts)),
            Arc::new(StringArray::from(stored_search_texts)),
            Arc::new(embedding_array),
        ],
    )
    .context("building LanceDB memory candidate record batch")
}

fn memory_index_chunk_row_hash(
    candidate: &MemoryCandidateDocument,
    chunk: &MemoryIndexChunkDocument,
    embedding_input_hash: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    let mut update = |part: &str| {
        hasher.update(part.as_bytes());
        hasher.update(b"\0");
    };
    update(&chunk.chunk_key);
    update(&chunk.parent_candidate_key);
    update(&chunk.chunk_index.to_string());
    update(&chunk.char_start.to_string());
    update(&chunk.char_end.to_string());
    update(scope_label(&candidate.scope));
    update(candidate.agent_id.as_deref().unwrap_or(""));
    update(candidate.goal_id.as_deref().unwrap_or(""));
    update(candidate.semantic_memory_type.as_str());
    update(&candidate.tier_name);
    update(&candidate.item_key);
    update(&candidate.content_hash);
    if let Some(path) = candidate.source_path.as_ref() {
        update(&path.display().to_string());
    } else {
        update("");
    }
    update(&chunk.chunk_text);
    update(&chunk.search_text);
    update(embedding_input_hash);
    hasher.finalize().to_hex().to_string()
}

fn record_batch_reader(record_batch: RecordBatch) -> Box<dyn RecordBatchReader + Send> {
    let schema = record_batch.schema();
    Box::new(RecordBatchIterator::new(vec![Ok(record_batch)], schema))
}

async fn ensure_lancedb_memory_indexes(table: &Table, index_dir: &Path) -> Result<()> {
    let existing_indexes = table
        .list_indices()
        .await
        .context("listing LanceDB memory table indexes")?;
    if !has_lancedb_single_column_index(
        &existing_indexes,
        MEMORY_LANCEDB_CHUNK_KEY_COLUMN,
        IndexType::BTree,
    ) {
        table
            .create_index(
                &[MEMORY_LANCEDB_CHUNK_KEY_COLUMN],
                Index::BTree(BTreeIndexBuilder::default()),
            )
            .replace(false)
            .execute()
            .await
            .context("creating LanceDB memory chunk-key BTree index")?;
    }
    if !has_lancedb_single_column_index(
        &existing_indexes,
        MEMORY_LANCEDB_CANDIDATE_KEY_COLUMN,
        IndexType::BTree,
    ) {
        table
            .create_index(
                &[MEMORY_LANCEDB_CANDIDATE_KEY_COLUMN],
                Index::BTree(BTreeIndexBuilder::default()),
            )
            .replace(false)
            .execute()
            .await
            .context("creating LanceDB memory candidate-key BTree index")?;
    }
    if !has_lancedb_single_column_index(
        &existing_indexes,
        MEMORY_LANCEDB_FTS_COLUMN,
        IndexType::FTS,
    ) {
        table
            .create_index(&[MEMORY_LANCEDB_FTS_COLUMN], Index::FTS(Default::default()))
            .replace(false)
            .execute()
            .await
            .context("creating LanceDB memory FTS index")?;
    }
    ensure_lancedb_ivf_pq(table, index_dir, &existing_indexes).await;
    Ok(())
}

async fn ensure_lancedb_ivf_pq(table: &Table, index_dir: &Path, existing_indexes: &[IndexConfig]) {
    let settings = vector_search_settings();
    let already_present = has_lancedb_single_column_index(
        existing_indexes,
        MEMORY_LANCEDB_VECTOR_COLUMN,
        IndexType::IvfPq,
    );
    let mut created = false;
    if settings.mode.maintains_ivf() && !already_present {
        let row_count = match table.count_rows(None).await {
            Ok(count) => count,
            Err(error) => {
                warn!(
                    target: "memory_index",
                    error = %error,
                    "counting LanceDB rows before IVF_PQ create failed"
                );
                0
            },
        };
        if row_count >= settings.min_rows {
            match table
                .create_index(
                    &[MEMORY_LANCEDB_VECTOR_COLUMN],
                    Index::IvfPq(IvfPqIndexBuilder::default()),
                )
                .replace(false)
                .execute()
                .await
            {
                Ok(()) => {
                    created = true;
                    invalidate_lance_table_pool(index_dir);
                    notify_vector_search_ranking_changed();
                    invalidate_hybrid_results_for_lancedb_dir(index_dir);
                },
                Err(error) => {
                    warn!(
                        target: "memory_index",
                        error = %error,
                        row_count,
                        min_rows = settings.min_rows,
                        "creating LanceDB memory IVF_PQ index failed; ANN queries will fall back to flat"
                    );
                },
            }
        }
    }
    let present = already_present || created;
    let disk_bytes = if present {
        cheap_ivf_index_bytes(index_dir)
    } else {
        0
    };
    let last_build = if created { Some(unix_ms_now()) } else { None };
    hol_stats::set_ivf_health(present, disk_bytes, last_build, created);
}

fn cheap_ivf_index_bytes(index_dir: &Path) -> u64 {
    const MAX_VISITS: usize = 512;
    let mut total = 0u64;
    let mut visits = 0usize;
    let mut stack = vec![index_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            visits += 1;
            if visits >= MAX_VISITS {
                return total;
            }
            let path = entry.path();
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                stack.push(path);
            } else if path
                .components()
                .any(|component| component.as_os_str() == "_indices")
            {
                total = total.saturating_add(metadata.len());
            }
        }
    }
    total
}

fn unix_ms_now() -> u64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

async fn lancedb_memory_index_unhealthy_reason(
    index_dir: &Path,
    manifest: &MemoryIndexManifest,
) -> Result<Option<String>> {
    let Some(table) = open_existing_lancedb_memory_table(index_dir).await? else {
        return Ok(Some("missing_lancedb_table".to_string()));
    };
    let schema = table
        .schema()
        .await
        .context("reading LanceDB memory candidate table schema")?;
    if !lancedb_schema_supports_incremental_merge(schema.as_ref(), manifest.embedding_dimensions) {
        return Ok(Some("lancedb_schema_incompatible".to_string()));
    }

    let indexes = table
        .list_indices()
        .await
        .context("listing LanceDB memory table indexes")?;
    if !has_lancedb_single_column_index(&indexes, MEMORY_LANCEDB_CHUNK_KEY_COLUMN, IndexType::BTree)
    {
        return Ok(Some("missing_lancedb_chunk_key_index".to_string()));
    }
    if !has_lancedb_single_column_index(
        &indexes,
        MEMORY_LANCEDB_CANDIDATE_KEY_COLUMN,
        IndexType::BTree,
    ) {
        return Ok(Some("missing_lancedb_candidate_key_index".to_string()));
    }
    if !has_lancedb_single_column_index(&indexes, MEMORY_LANCEDB_FTS_COLUMN, IndexType::FTS) {
        return Ok(Some("missing_lancedb_fts_index".to_string()));
    }

    // Missing IVF_PQ is not a stale-index reason. Flat search bypasses it;
    // ANN mode falls back to flat at query time instead of dropping hybrid.
    Ok(None)
}

fn has_lancedb_single_column_index(
    indexes: &[IndexConfig],
    column: &str,
    index_type: IndexType,
) -> bool {
    indexes.iter().any(|index| {
        index.columns.len() == 1 && index.columns[0] == column && index.index_type == index_type
    })
}

async fn lancedb_memory_index_unhealthy_reason_with_timeout(
    index_dir: &Path,
    manifest: &MemoryIndexManifest,
) -> Result<Option<String>> {
    let timeout_duration = lancedb_health_check_timeout();
    match timeout(
        timeout_duration,
        lancedb_memory_index_unhealthy_reason(index_dir, manifest),
    )
    .await
    {
        Ok(Ok(reason)) => Ok(reason),
        Ok(Err(error)) => {
            warn!(
                target: "memory_index",
                error = %error,
                "LanceDB memory index health check failed"
            );
            Ok(Some(
                MEMORY_INDEX_STALE_REASON_LANCEDB_HEALTH_CHECK_FAILED.to_string(),
            ))
        },
        Err(_) => Ok(Some(
            MEMORY_INDEX_STALE_REASON_LANCEDB_HEALTH_CHECK_TIMED_OUT.to_string(),
        )),
    }
}

fn lancedb_schema_supports_incremental_merge(schema: &Schema, embedding_dims: usize) -> bool {
    lancedb_utf8_field(schema, MEMORY_LANCEDB_CHUNK_KEY_COLUMN)
        && lancedb_utf8_field(schema, MEMORY_LANCEDB_ROW_HASH_COLUMN)
        && lancedb_utf8_field(schema, MEMORY_LANCEDB_EMBEDDING_INPUT_HASH_COLUMN)
        && lancedb_utf8_field(schema, "candidate_key")
        && lancedb_u32_field(schema, "chunk_index")
        && lancedb_u32_field(schema, "char_start")
        && lancedb_u32_field(schema, "char_end")
        && lancedb_utf8_field(schema, "scope")
        && lancedb_utf8_field(schema, "agent_id")
        && lancedb_utf8_field(schema, "goal_id")
        && lancedb_utf8_field(schema, "tier_name")
        && lancedb_utf8_field(schema, "item_key")
        && lancedb_utf8_field(schema, "chunk_text")
        && lancedb_utf8_field(schema, MEMORY_LANCEDB_FTS_COLUMN)
        && lancedb_vector_field(schema, MEMORY_LANCEDB_VECTOR_COLUMN, embedding_dims)
}

fn lancedb_utf8_field(schema: &Schema, name: &str) -> bool {
    schema
        .field_with_name(name)
        .is_ok_and(|field| matches!(field.data_type(), DataType::Utf8))
}

fn lancedb_u32_field(schema: &Schema, name: &str) -> bool {
    schema
        .field_with_name(name)
        .is_ok_and(|field| matches!(field.data_type(), DataType::UInt32))
}

fn lancedb_vector_field(schema: &Schema, name: &str, embedding_dims: usize) -> bool {
    schema.field_with_name(name).is_ok_and(|field| {
        matches!(
            field.data_type(),
            DataType::FixedSizeList(_, dims) if *dims == embedding_dims as i32
        )
    })
}

fn clamp_usize_to_u32(value: usize) -> u32 {
    value.min(u32::MAX as usize) as u32
}

fn string_column<'a>(batch: &'a RecordBatch, name: &str) -> Option<&'a StringArray> {
    batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<StringArray>())
}

#[derive(Debug, Clone)]
struct MemoryEmbeddingManifest {
    provider: String,
    model: Option<String>,
    dimensions: usize,
    contract_id: String,
    fallback_reason: Option<String>,
}

#[derive(Debug, Clone)]
struct MemoryEmbeddingBatch {
    manifest: MemoryEmbeddingManifest,
    vectors: Vec<Vec<f32>>,
}

#[derive(Debug, Clone)]
struct MemoryEmbeddingCacheSpec {
    namespace: String,
    dimensions: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemoryEmbeddingPreference {
    Auto,
    Ollama,
    #[cfg(feature = "test-hash-embeddings")]
    TestHash,
}

/// Clean batches required before a backed-off embedding batch width doubles
/// again.
///
/// Backing off is one failure; recovering should not be one success, or a
/// flapping endpoint oscillates between widths and pays a failed oversized
/// request every few batches. Four is long enough that the earlier failure
/// reads as transient, and short enough that a full-corpus rebuild recovers
/// within the first minutes rather than the last.
const OLLAMA_BATCH_REGROWTH_SUCCESSES: usize = 4;

/// The width to resume at after a clean batch, or `None` to hold.
///
/// Pure so the recovery rule is testable without an Ollama endpoint: the
/// surrounding loop is a network call in an async retry ladder, which is
/// exactly where a subtle off-by-one in backoff recovery hides.
fn widened_ollama_batch_size(
    active: usize,
    configured: usize,
    consecutive_successes: usize,
) -> Option<usize> {
    if active >= configured || consecutive_successes < OLLAMA_BATCH_REGROWTH_SUCCESSES {
        return None;
    }
    Some(active.saturating_mul(2).min(configured))
}

#[derive(Debug, Clone)]
struct MemoryEmbeddingConfig {
    preference: MemoryEmbeddingPreference,
    model: String,
    dimensions: usize,
    base_url: String,
    timeout: Duration,
    warmup_timeout: Duration,
    batch_size: usize,
    max_batch_chars: usize,
    max_retries: usize,
    retry_backoff: Duration,
    warmup: bool,
    keep_alive: Option<String>,
    context_tokens: Option<u32>,
    batch_tokens: Option<u32>,
}

#[derive(Debug)]
struct OllamaLogicalEmbeddingPlan {
    physical_inputs: Vec<String>,
    physical_owner_indexes: Vec<usize>,
    logical_input_count: usize,
}

struct MemoryEmbeddingAdmissionProbe {
    acquired: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
}

impl MemoryEmbeddingConfig {
    fn from_env() -> Self {
        let provider = env::var("MAGICIAN_MEMORY_EMBEDDING_PROVIDER")
            .unwrap_or_else(|_| "auto".to_string())
            .trim()
            .to_ascii_lowercase();
        let preference = match provider.as_str() {
            "ollama" => MemoryEmbeddingPreference::Ollama,
            #[cfg(feature = "test-hash-embeddings")]
            "test_hash" if test_hash_embeddings_enabled() => MemoryEmbeddingPreference::TestHash,
            "test_hash" | "hash" | "hashed" | "local_hash" => {
                warn!(
                    target: "memory_index",
                    provider = %provider,
                    "Ignoring test-only memory embedding provider outside a test-enabled build"
                );
                MemoryEmbeddingPreference::Auto
            },
            _ => MemoryEmbeddingPreference::Auto,
        };
        let model = ollama_keep_alive::default_embedding_model();
        let dimensions = ollama_keep_alive::default_embedding_dimensions();
        let base_url = read_nonempty_string_env(&["MAGICIAN_MEMORY_OLLAMA_URL"])
            .or_else(ollama_keep_alive::default_embedding_base_url)
            .or_else(|| read_nonempty_string_env(&["MAGICIAN_OLLAMA_BASE_URL"]))
            .map(|value| value.trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_OLLAMA_BASE_URL.to_string());
        let timeout = read_positive_u64_env(&[
            "MAGICIAN_OLLAMA_EMBEDDING_TIMEOUT_MS",
            "MAGICIAN_MEMORY_EMBEDDING_TIMEOUT_MS",
        ])
        .or_else(ollama_keep_alive::default_embedding_write_timeout_ms)
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(DEFAULT_OLLAMA_TIMEOUT_MS));
        let warmup_timeout = read_positive_u64_env(&[
            "MAGICIAN_OLLAMA_EMBEDDING_WARMUP_TIMEOUT_MS",
            "MAGICIAN_MEMORY_EMBEDDING_WARMUP_TIMEOUT_MS",
        ])
        .map(Duration::from_millis)
        .unwrap_or_else(|| timeout.max(Duration::from_millis(DEFAULT_OLLAMA_WARMUP_TIMEOUT_MS)));
        let batch_size = ollama_keep_alive::default_embedding_batch_size();
        let max_batch_chars =
            read_positive_usize_env(&["MAGICIAN_MEMORY_EMBEDDING_MAX_BATCH_CHARS"])
                .unwrap_or(DEFAULT_OLLAMA_MAX_BATCH_CHARS)
                .max(1);
        let max_retries = read_usize_env(&["MAGICIAN_MEMORY_EMBEDDING_RETRIES"])
            .map(|value| value.min(8))
            .unwrap_or(DEFAULT_OLLAMA_SINGLE_BATCH_RETRIES);
        let retry_backoff = read_positive_u64_env(&["MAGICIAN_MEMORY_EMBEDDING_RETRY_BACKOFF_MS"])
            .map(Duration::from_millis)
            .unwrap_or_else(|| Duration::from_millis(DEFAULT_OLLAMA_RETRY_BACKOFF_MS));
        let warmup = read_bool_env(&["MAGICIAN_MEMORY_EMBEDDING_WARMUP"]).unwrap_or(true);
        let keep_alive = ollama_keep_alive::default_keep_alive();
        let context_tokens = ollama_keep_alive::default_embedding_context_tokens();
        let batch_tokens = ollama_keep_alive::default_embedding_batch_tokens();
        Self {
            preference,
            model,
            dimensions,
            base_url,
            timeout,
            warmup_timeout,
            batch_size,
            max_batch_chars,
            max_retries,
            retry_backoff,
            warmup,
            keep_alive,
            context_tokens,
            batch_tokens,
        }
    }

    fn empty_manifest(&self) -> MemoryEmbeddingManifest {
        match self.preference {
            #[cfg(feature = "test-hash-embeddings")]
            MemoryEmbeddingPreference::TestHash => test_hash_embedding_manifest(),
            MemoryEmbeddingPreference::Auto | MemoryEmbeddingPreference::Ollama => {
                MemoryEmbeddingManifest {
                    provider: "ollama".to_string(),
                    model: Some(self.model.clone()),
                    dimensions: self.dimensions,
                    contract_id: self.ollama_embedding_contract_id(),
                    fallback_reason: None,
                }
            },
        }
    }

    fn ollama_embedding_contract_id(&self) -> String {
        let mut toolkit_config = OllamaEmbedderConfig::from_env();
        toolkit_config.model = self.model.clone();
        toolkit_config.dims = self.dimensions;
        toolkit_config.context_tokens = self.context_tokens;
        toolkit_config.batch_tokens = self.batch_tokens;

        // The toolkit contract owns provider/model/dimension/context/physical
        // batch identity. Memory-index document embeddings additionally own a
        // logical split + merge preprocessing contract, which must invalidate
        // persisted vectors independently when it changes.
        //
        // The endpoint is deliberately NOT part of this contract: the same
        // model at another URL produces the same vectors, and folding the URL
        // in (as a short-lived v2 of this contract did) marked every personal
        // memory index stale on the day the embedding daemon moved to its
        // own port, with an explicit rebuild demanded for a change that could
        // not alter one vector. Protected app-memory partitions, which must
        // not inherit vectors across an endpoint substitution, carry the URL
        // in `MemoryEmbeddingPhysicalIdentity` instead.
        let toolkit_contract_id = toolkit_config.embedding_contract_id();
        let mut hasher = blake3::Hasher::new();
        for field in [
            "memory-index-embedding-contract-v1",
            toolkit_contract_id.as_str(),
            MEMORY_OLLAMA_EMBEDDING_INPUT_VERSION,
        ] {
            hasher.update(&(field.len() as u64).to_le_bytes());
            hasher.update(field.as_bytes());
        }
        hasher.finalize().to_hex().to_string()
    }

    fn manifest_is_compatible(&self, manifest: &MemoryIndexManifest) -> bool {
        manifest_embedding_matches(manifest, &self.empty_manifest())
    }

    async fn embed_documents(&self, texts: &[String]) -> Result<MemoryEmbeddingBatch> {
        match self.preference {
            #[cfg(feature = "test-hash-embeddings")]
            MemoryEmbeddingPreference::TestHash => Ok(test_hash_embedding_batch(texts)),
            MemoryEmbeddingPreference::Ollama => self.embed_documents_with_ollama(texts).await,
            MemoryEmbeddingPreference::Auto => self.embed_documents_with_ollama(texts).await.context(
                "auto memory embedding provider requires Ollama; direct retrieval remains the runtime fallback until the derived hybrid index rebuilds successfully",
            ),
        }
    }

    async fn embed_documents_with_cache(
        &self,
        storage: &dyn MemoryStorage,
        texts: &[String],
    ) -> Result<MemoryEmbeddingBatch> {
        if texts.is_empty() {
            return Ok(MemoryEmbeddingBatch {
                manifest: self.empty_manifest(),
                vectors: Vec::new(),
            });
        }
        let cache_spec = self.embedding_cache_spec();
        let Some(cache_spec) = cache_spec else {
            return self.embed_documents(texts).await;
        };

        let cache_dir = memory_embedding_cache_dir(storage, &cache_spec);
        let mut vectors = vec![None; texts.len()];
        let mut missed_indices = Vec::new();
        let mut missed_texts = Vec::new();
        let mut cache_hits = 0usize;
        let mut cache_read_errors = 0usize;

        for (index, text) in texts.iter().enumerate() {
            let cache_key = hash_bytes(text.as_bytes());
            let cache_path = memory_embedding_cache_path(&cache_dir, &cache_key);
            match read_memory_embedding_cache_file(&cache_path, cache_spec.dimensions).await {
                Ok(Some(vector)) => {
                    vectors[index] = Some(vector);
                    cache_hits += 1;
                },
                Ok(None) => {
                    missed_indices.push(index);
                    missed_texts.push(text.clone());
                },
                Err(error) => {
                    cache_read_errors += 1;
                    warn!(
                        target: "memory_index",
                        path = %cache_path.display(),
                        error = %format_error_chain(&error),
                        "Memory embedding cache read failed; embedding chunk again"
                    );
                    missed_indices.push(index);
                    missed_texts.push(text.clone());
                },
            }
        }

        info!(
            target: "memory_index",
            namespace = %cache_spec.namespace,
            total_chunks = texts.len(),
            cache_hits = cache_hits,
            cache_misses = missed_texts.len(),
            cache_read_errors = cache_read_errors,
            "memory embedding cache lookup complete"
        );

        let manifest = if missed_texts.is_empty() {
            self.empty_manifest()
        } else {
            let embedded = self.embed_documents(&missed_texts).await?;
            if embedded.manifest.dimensions != cache_spec.dimensions {
                anyhow::bail!(
                    "memory embedding cache dimension mismatch: cache namespace expects {}, embedder returned {}",
                    cache_spec.dimensions,
                    embedded.manifest.dimensions
                );
            }
            for ((index, text), vector) in missed_indices
                .into_iter()
                .zip(missed_texts.into_iter())
                .zip(embedded.vectors.into_iter())
            {
                let cache_key = hash_bytes(text.as_bytes());
                let cache_path = memory_embedding_cache_path(&cache_dir, &cache_key);
                write_memory_embedding_cache_file_best_effort(&cache_path, &vector).await;
                vectors[index] = Some(vector);
            }
            embedded.manifest
        };

        let vectors = vectors
            .into_iter()
            .enumerate()
            .map(|(index, vector)| {
                vector.with_context(|| {
                    format!("missing memory embedding vector for chunk index {index}")
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(MemoryEmbeddingBatch { manifest, vectors })
    }

    fn embedding_cache_spec(&self) -> Option<MemoryEmbeddingCacheSpec> {
        match self.preference {
            #[cfg(feature = "test-hash-embeddings")]
            MemoryEmbeddingPreference::TestHash => Some(MemoryEmbeddingCacheSpec {
                namespace: format!("test_hash-{}", MEMORY_TEST_HASH_EMBEDDING_DIMS),
                dimensions: MEMORY_TEST_HASH_EMBEDDING_DIMS,
            }),
            MemoryEmbeddingPreference::Auto | MemoryEmbeddingPreference::Ollama => {
                Some(MemoryEmbeddingCacheSpec {
                    namespace: format!(
                        "ollama-{}-{}-contract-{}",
                        sanitize_segment(&self.model),
                        self.dimensions,
                        self.ollama_embedding_contract_id(),
                    ),
                    dimensions: self.dimensions,
                })
            },
        }
    }

    async fn embed_documents_with_ollama(&self, texts: &[String]) -> Result<MemoryEmbeddingBatch> {
        self.embed_documents_with_ollama_priority(texts, EmbeddingPriority::Write)
            .await
    }

    async fn embed_documents_with_ollama_priority(
        &self,
        texts: &[String],
        priority: EmbeddingPriority,
    ) -> Result<MemoryEmbeddingBatch> {
        self.embed_documents_with_ollama_priority_and_admission_signal(texts, priority, None)
            .await
    }

    async fn embed_documents_with_ollama_priority_and_admission_signal(
        &self,
        texts: &[String],
        priority: EmbeddingPriority,
        first_admission_probe: Option<MemoryEmbeddingAdmissionProbe>,
    ) -> Result<MemoryEmbeddingBatch> {
        if self.model.trim().is_empty() {
            anyhow::bail!(
                "Ollama embedding model is not configured; set runtime.ollama.embedding_model"
            );
        }
        if self.dimensions == 0 {
            anyhow::bail!(
                "Ollama embedding dimensions are not configured; set runtime.ollama.embedding_dimensions"
            );
        }
        if self.batch_size == 0 {
            anyhow::bail!(
                "Ollama embedding batch size is not configured; set runtime.ollama.embedding_batch_size"
            );
        }
        let context_tokens = self.context_tokens.context(
            "Ollama embedding context is not configured; set runtime.ollama.embedding_context_tokens",
        )?;
        let physical_token_ceiling = self
            .batch_tokens
            .map(|batch_tokens| context_tokens.min(batch_tokens))
            .unwrap_or(context_tokens);
        if physical_token_ceiling <= OLLAMA_EMBEDDING_SPECIAL_TOKEN_RESERVE as u32 {
            anyhow::bail!(
                "Ollama embedding physical token ceiling {physical_token_ceiling} is too small; expected more than {OLLAMA_EMBEDDING_SPECIAL_TOKEN_RESERVE} tokens after combining runtime.ollama embedding context and batch limits"
            );
        }
        if texts.is_empty() {
            return Ok(MemoryEmbeddingBatch {
                manifest: self.empty_manifest(),
                vectors: Vec::new(),
            });
        }
        let client = reqwest::Client::builder()
            .build()
            .context("building Ollama embedding HTTP client")?;
        // Base URL of the embedding daemon; the request path is applied
        // inside the magicllm seam (single chokepoint).
        let endpoint = self.base_url.clone();
        // Ollama's llama.cpp embedding runner rejects an individual input that
        // exceeds either `num_ctx` or its physical `num_batch`. UTF-8 byte
        // length is a conservative tokenizer-independent upper bound for the
        // supported byte-fallback tokenizers, so fragments bounded by the
        // smaller ceiling plus `truncate: false` guarantee complete coverage.
        let physical_input_limit = ollama_physical_input_byte_limit(
            context_tokens as usize,
            self.batch_tokens.map(|tokens| tokens as usize),
        );
        let input_plan = plan_ollama_logical_embedding_inputs(texts, physical_input_limit)?;
        let physical_inputs = input_plan.physical_inputs.as_slice();
        let mut vectors = Vec::with_capacity(physical_inputs.len());
        let mut offset = 0usize;
        let mut active_batch_size = self.batch_size;
        // Halving on a retriable failure used to be permanent: nothing restored
        // `active_batch_size`, so one transient timeout in the first minute of a
        // rebuild ran every remaining batch at the reduced width. On a corpus of
        // tens of thousands of chunks that silently multiplies wall time for the
        // whole job. Count clean batches so the width can climb back.
        let mut consecutive_batch_successes = 0usize;
        let mut single_batch_retries_at_offset = 0usize;
        let started = Instant::now();
        // Foreground retrieval gets one end-to-end budget for its complete
        // logical query, including every context fragment, admission wait,
        // provider call, retry, and backoff. Background writes deliberately
        // retain the existing per-physical-batch budget so a large rebuild can
        // make durable progress while yielding admission between batches.
        let foreground_deadline =
            (priority == EmbeddingPriority::Read).then(|| started + self.timeout);
        let mut first_admission_probe = first_admission_probe;
        let mut last_progress_log = Instant::now();
        if self.warmup {
            if let Err(error) = self
                .warm_ollama_model(&client, &endpoint, first_admission_probe.take())
                .await
            {
                warn!(
                    target: "memory_index",
                    model = %self.model,
                    endpoint = %endpoint,
                    warmup_timeout_ms = self.warmup_timeout.as_millis() as u64,
                    error = %format_error_chain(&error),
                    "Ollama memory embedding warmup failed; continuing with batch embedding"
                );
            }
        }
        info!(
            target: "memory_index",
            model = %self.model,
            endpoint = %endpoint,
            logical_chunks = texts.len(),
            physical_chunks = physical_inputs.len(),
            physical_input_byte_limit = physical_input_limit,
            configured_batch_size = self.batch_size,
            max_batch_chars = self.max_batch_chars,
            timeout_ms = self.timeout.as_millis() as u64,
            max_retries = self.max_retries,
            warmup = self.warmup,
            "embedding memory index chunks with Ollama"
        );
        while offset < physical_inputs.len() {
            let remaining = physical_inputs.len() - offset;
            let batch_size =
                self.ollama_batch_len(physical_inputs, offset, active_batch_size.min(remaining));
            let chunk = &physical_inputs[offset..offset + batch_size];
            let request_timeout = embedding_request_budget(
                foreground_deadline,
                self.timeout,
                "embedding foreground memory query",
            )?;
            match self
                .embed_ollama_chunk_with_timeout(
                    &client,
                    &endpoint,
                    chunk,
                    request_timeout,
                    priority,
                    first_admission_probe.take(),
                )
                .await
            {
                Ok(mut embeddings) => {
                    vectors.append(&mut embeddings);
                    offset += batch_size;
                    single_batch_retries_at_offset = 0;
                    consecutive_batch_successes = consecutive_batch_successes.saturating_add(1);
                    // Recover the way we backed off — by doubling — but only
                    // after enough clean batches that the earlier failure looks
                    // transient rather than a standing limit. Never above the
                    // configured width, and the counter resets on every step so
                    // climbing back from 1 to 32 takes five sustained runs, not
                    // one lucky batch.
                    if let Some(restored) = widened_ollama_batch_size(
                        active_batch_size,
                        self.batch_size,
                        consecutive_batch_successes,
                    ) {
                        debug!(
                            target: "memory_index",
                            model = %self.model,
                            offset = offset,
                            from_batch_size = active_batch_size,
                            to_batch_size = restored,
                            configured_batch_size = self.batch_size,
                            "Ollama memory embedding batch recovered; widening again"
                        );
                        active_batch_size = restored;
                        consecutive_batch_successes = 0;
                    }
                    if offset == physical_inputs.len()
                        || offset % 128 == 0
                        || last_progress_log.elapsed() >= Duration::from_secs(15)
                    {
                        let elapsed_ms = started.elapsed().as_millis() as u64;
                        info!(
                            target: "memory_index",
                            model = %self.model,
                            embedded_chunks = offset,
                            total_chunks = physical_inputs.len(),
                            active_batch_size = active_batch_size,
                            elapsed_ms = elapsed_ms,
                            "memory index Ollama embedding progress"
                        );
                        last_progress_log = Instant::now();
                    }
                },
                Err(error)
                    if batch_size > 1 && ollama_embedding_error_is_batch_retriable(&error) =>
                {
                    let next_batch_size = (batch_size / 2).max(1);
                    warn!(
                        target: "memory_index",
                        model = %self.model,
                        endpoint = %endpoint,
                        offset = offset,
                        failed_batch_size = batch_size,
                        next_batch_size = next_batch_size,
                        error = %format_error_chain(&error),
                        "Ollama memory embedding batch failed; retrying with a smaller batch"
                    );
                    active_batch_size = next_batch_size;
                    consecutive_batch_successes = 0;
                    single_batch_retries_at_offset = 0;
                },
                Err(error)
                    if batch_size == 1
                        && single_batch_retries_at_offset < self.max_retries
                        && ollama_embedding_error_is_batch_retriable(&error) =>
                {
                    single_batch_retries_at_offset += 1;
                    let backoff_ms = self
                        .retry_backoff
                        .as_millis()
                        .saturating_mul(single_batch_retries_at_offset as u128)
                        .min(u64::MAX as u128) as u64;
                    let backoff = Duration::from_millis(backoff_ms);
                    if let Some(deadline) = foreground_deadline {
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        if remaining <= backoff {
                            anyhow::bail!(
                                "embedding foreground memory query exhausted its total {} ms budget before retry backoff",
                                self.timeout.as_millis()
                            );
                        }
                    }
                    warn!(
                        target: "memory_index",
                        model = %self.model,
                        endpoint = %endpoint,
                        offset = offset,
                        retry_attempt = single_batch_retries_at_offset,
                        max_retries = self.max_retries,
                        retry_backoff_ms = backoff_ms,
                        error = %format_error_chain(&error),
                        "Ollama memory embedding single chunk failed; retrying after backoff"
                    );
                    sleep(backoff).await;
                },
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "embedding Ollama memory batch at offset {offset} with batch size {batch_size} (configured batch size {})",
                            self.batch_size
                        )
                    });
                },
            }
        }
        validate_ollama_embedding_vectors(&vectors, physical_inputs.len(), self.dimensions)?;
        let vectors = aggregate_ollama_logical_embeddings(&input_plan, &vectors, self.dimensions)?;
        let dimensions = self.dimensions;
        Ok(MemoryEmbeddingBatch {
            manifest: MemoryEmbeddingManifest {
                provider: "ollama".to_string(),
                model: Some(self.model.clone()),
                dimensions,
                contract_id: self.ollama_embedding_contract_id(),
                fallback_reason: None,
            },
            vectors,
        })
    }

    async fn warm_ollama_model(
        &self,
        client: &reqwest::Client,
        endpoint: &str,
        admission_probe: Option<MemoryEmbeddingAdmissionProbe>,
    ) -> Result<()> {
        let warmup_input = [String::from("memory index warmup")];
        let started = Instant::now();
        let embeddings = self
            .embed_ollama_chunk_with_timeout(
                client,
                endpoint,
                &warmup_input,
                self.warmup_timeout,
                EmbeddingPriority::Write,
                admission_probe,
            )
            .await
            .context("warming Ollama memory embedding model")?;
        let dimensions = embeddings.first().map(Vec::len).unwrap_or(0);
        info!(
            target: "memory_index",
            model = %self.model,
            endpoint = %endpoint,
            elapsed_ms = started.elapsed().as_millis() as u64,
            dimensions = dimensions,
            warmup_timeout_ms = self.warmup_timeout.as_millis() as u64,
            "warmed Ollama memory embedding model"
        );
        Ok(())
    }

    fn ollama_batch_len(&self, texts: &[String], offset: usize, max_items: usize) -> usize {
        let remaining = texts.len().saturating_sub(offset);
        let max_items = max_items.min(remaining).max(1);
        if self.max_batch_chars == usize::MAX {
            return max_items;
        }

        let mut count = 0usize;
        let mut chars = 0usize;
        for text in &texts[offset..offset + max_items] {
            let text_chars = text.chars().count().max(1);
            if count > 0 && chars.saturating_add(text_chars) > self.max_batch_chars {
                break;
            }
            count += 1;
            chars = chars.saturating_add(text_chars);
            if chars >= self.max_batch_chars {
                break;
            }
        }
        count.max(1)
    }

    async fn embed_ollama_chunk_with_timeout(
        &self,
        client: &reqwest::Client,
        base_url: &str,
        chunk: &[String],
        request_timeout: Duration,
        priority: EmbeddingPriority,
        admission_probe: Option<MemoryEmbeddingAdmissionProbe>,
    ) -> Result<Vec<Vec<f32>>> {
        let started = Instant::now();
        let _permit =
            acquire_memory_embedding_provider_permit(priority, request_timeout, admission_probe)
                .await?;
        let remaining = request_timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            anyhow::bail!(
                "{priority:?} memory embedding admission exhausted the {} ms request budget",
                request_timeout.as_millis()
            );
        }
        // Innermost HTTP exchange routes through magicllm when a router is
        // installed; the fallback keeps the previous direct POST byte-for-byte.
        let operation = if priority == EmbeddingPriority::Read {
            crate::embedding_router::EMBED_QUERY_OPERATION
        } else {
            crate::embedding_router::EMBED_DOCUMENTS_OPERATION
        };
        let embeddings = crate::embedding_router::post_ollama_embed(
            operation,
            base_url,
            client,
            &self.model,
            chunk,
            self.keep_alive.as_deref(),
            self.context_tokens,
            self.batch_tokens,
            remaining,
        )
        .await?;
        validate_ollama_embedding_vectors(&embeddings, chunk.len(), self.dimensions)?;
        Ok(embeddings)
    }
}

/// Live-eval-only probe for the real memory-index embedding writer pipeline.
///
/// This intentionally performs no index, manifest, cache, or canonical-memory
/// mutation. It feeds caller-owned synthetic texts through the same Ollama
/// logical fragmentation, retry, batching, and [`EmbeddingPriority::Write`]
/// path used by memory-index construction, making foreground/write contention
/// measurable without touching user data. `first_admission_signal` is emitted
/// only after this probe owns its first provider Write permit, so a live eval
/// Touch the local embedding model so the OS keeps its weights resident.
///
/// `runtime.ollama.embedding_keep_alive: "-1"` pins the model in Ollama's
/// registry, and `/api/ps` duly reports it loaded with an expiry centuries out.
/// That is a registry fact, not a residency one. On a host under memory
/// pressure the OS pages the weights out while the model sits idle, and the
/// next foreground query pays the fault before LanceDB is ever reached.
///
/// Measured on a 32 GB host, 2026-09-08, with the 4B `pplx-embed-v1-4b` Q6_K
/// embedder (4.27 GB resident): a query embedding costs ~0.47 s warm, 3.1 s
/// after eight idle minutes, and 4.93 s with a 10.46 GB local generation model
/// co-resident. The foreground budget is `embedding_query_timeout_ms: 5000`, so
/// the first memory read after a quiet period loses that race, the hybrid path
/// falls back, and `MEMORY_INDEX_HYBRID_SUSPEND_AFTER_EMBEDDING_TIMEOUT` then
/// keeps it in fallback for five minutes.
///
/// One tiny input, at `Write` priority so it queues *behind* any foreground
/// read rather than in front of it, and with `warmup` off because this call is
/// itself the warmup. Returns whether the touch reached the provider; a failure
/// is not an error for the caller — the next one is 30 seconds away.
pub async fn keep_embedding_model_resident() -> Result<bool> {
    let mut config = MemoryEmbeddingConfig::from_env();
    if config.model.trim().is_empty() {
        return Ok(false);
    }
    config.warmup = false;
    let touched = config
        .embed_documents_with_ollama_priority(
            std::slice::from_ref(&String::from("keep resident")),
            EmbeddingPriority::Write,
        )
        .await
        .context("touching the local embedding model to keep it resident")?;
    Ok(!touched.vectors.is_empty())
}

/// can begin foreground work without a timing-based sleep. The probe keeps that
/// permit until `first_admission_release` resolves; dropping its sender also
/// releases the gate cleanly.
#[doc(hidden)]
pub async fn eval_only_run_memory_index_write_embedding_pipeline(
    texts: &[String],
    first_admission_signal: oneshot::Sender<()>,
    first_admission_release: oneshot::Receiver<()>,
) -> Result<usize> {
    if texts.is_empty() {
        anyhow::bail!("memory-index write eval requires at least one synthetic input");
    }
    if texts.iter().any(|text| text.trim().is_empty()) {
        anyhow::bail!("memory-index write eval inputs must be non-empty");
    }
    let mut config = MemoryEmbeddingConfig::from_env();
    // Keep the signal attributable to the caller's synthetic corpus rather
    // than a redundant prewarm request. Production writers retain their normal
    // configured warmup behavior; the batching/retry/Write-admission pipeline
    // below is otherwise identical.
    config.warmup = false;
    let embedded = config
        .embed_documents_with_ollama_priority_and_admission_signal(
            texts,
            EmbeddingPriority::Write,
            Some(MemoryEmbeddingAdmissionProbe {
                acquired: first_admission_signal,
                release: first_admission_release,
            }),
        )
        .await
        .context("running eval-only memory-index write embedding pipeline")?;
    if embedded.vectors.len() != texts.len() {
        anyhow::bail!(
            "memory-index write eval returned {} logical vectors for {} inputs",
            embedded.vectors.len(),
            texts.len()
        );
    }
    Ok(embedded.vectors.len())
}

async fn acquire_memory_embedding_provider_permit(
    priority: EmbeddingPriority,
    request_timeout: Duration,
    admission_probe: Option<MemoryEmbeddingAdmissionProbe>,
) -> Result<crate::embedding_scheduler::EmbeddingPermit> {
    let started = Instant::now();
    let permit = timeout(request_timeout, acquire_embedding_permit(priority))
        .await
        .with_context(|| {
            format!(
                "waiting for {priority:?} memory embedding admission exceeded the {} ms request budget",
                request_timeout.as_millis()
            )
        })?;
    // Probe-only synchronization: emission happens after, never before, this
    // exact provider request owns its Write permit. Hold that permit until the
    // evaluator releases it, allowing a deterministic queued-foreground proof.
    // A dropped acquired/release endpoint disables the gate cleanly.
    if let Some(MemoryEmbeddingAdmissionProbe { acquired, release }) = admission_probe {
        if acquired.send(()).is_ok() {
            let remaining = request_timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                anyhow::bail!(
                    "{priority:?} memory embedding admission probe exhausted the {} ms request budget",
                    request_timeout.as_millis()
                );
            }
            if timeout(remaining, release).await.is_err() {
                anyhow::bail!(
                    "{priority:?} memory embedding admission probe release exceeded the {} ms request budget",
                    request_timeout.as_millis()
                );
            }
        }
    }
    Ok(permit)
}

fn plan_ollama_logical_embedding_inputs(
    texts: &[String],
    max_physical_input_bytes: usize,
) -> Result<OllamaLogicalEmbeddingPlan> {
    if max_physical_input_bytes == 0 {
        anyhow::bail!("Ollama physical embedding input limit must be positive");
    }
    let mut physical_inputs = Vec::new();
    let mut physical_owner_indexes = Vec::new();
    for (logical_index, text) in texts.iter().enumerate() {
        for segment in split_ollama_embedding_input(text, max_physical_input_bytes)? {
            physical_inputs.push(segment);
            physical_owner_indexes.push(logical_index);
        }
    }
    Ok(OllamaLogicalEmbeddingPlan {
        physical_inputs,
        physical_owner_indexes,
        logical_input_count: texts.len(),
    })
}

fn ollama_physical_input_byte_limit(context_tokens: usize, batch_tokens: Option<usize>) -> usize {
    batch_tokens
        .map(|batch_tokens| context_tokens.min(batch_tokens))
        .unwrap_or(context_tokens)
        .saturating_sub(OLLAMA_EMBEDDING_SPECIAL_TOKEN_RESERVE)
        .max(1)
}

fn embedding_request_budget(
    foreground_deadline: Option<Instant>,
    per_batch_timeout: Duration,
    operation: &str,
) -> Result<Duration> {
    let Some(deadline) = foreground_deadline else {
        return Ok(per_batch_timeout);
    };
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        anyhow::bail!(
            "{operation} exhausted its total {} ms budget",
            per_batch_timeout.as_millis()
        );
    }
    Ok(remaining.min(per_batch_timeout))
}

/// Wire-parity contract for the embedding request body. The body now lives in
/// the magicllm seam; this fn remains as the pinned test fixture asserting the
/// field-for-field shape (numeric keep-alive sentinels, fail-closed truncate,
/// conditional options passthrough).
#[cfg(test)]
fn ollama_embedding_request_body(
    model: &str,
    chunk: &[String],
    keep_alive: Option<&str>,
    context_tokens: Option<u32>,
    batch_tokens: Option<u32>,
) -> serde_json::Value {
    // Ollama defaults `truncate` to true. Fail closed instead: logical input
    // splitting owns the complete-coverage contract and a contract mistake must
    // surface as an error rather than silently changing indexed meaning.
    let mut body = json!({
        "model": model,
        "input": chunk,
        "truncate": false,
    });
    if let Some(keep_alive) = keep_alive {
        body["keep_alive"] = magicllm::request_keep_alive_value(keep_alive);
    }
    if context_tokens.is_some() || batch_tokens.is_some() {
        body["options"] = json!({});
    }
    if let Some(context_tokens) = context_tokens {
        body["options"]["num_ctx"] = json!(context_tokens);
    }
    if let Some(batch_tokens) = batch_tokens {
        // The runner also uses `num_batch` as the maximum physical prompt it
        // can evaluate; logical preprocessing has already fragmented each
        // input against this same ceiling.
        body["options"]["num_batch"] = json!(batch_tokens);
    }
    body
}

fn split_ollama_embedding_input(text: &str, max_bytes: usize) -> Result<Vec<String>> {
    if max_bytes == 0 {
        anyhow::bail!("Ollama physical embedding input limit must be positive");
    }
    if text.len() <= max_bytes {
        return Ok(vec![text.to_string()]);
    }

    let mut segments = Vec::new();
    let mut start = 0usize;
    while start < text.len() {
        let mut end = start.saturating_add(max_bytes).min(text.len());
        while end > start && !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            anyhow::bail!(
                "Ollama physical embedding input limit {max_bytes} cannot hold the UTF-8 character at byte {start}"
            );
        }

        // Prefer a nearby word boundary without creating very small segments.
        // The selected whitespace remains in the preceding segment, so joining
        // all segments reconstructs the exact original input.
        if end < text.len() {
            let hard_end = end;
            let slice = &text[start..hard_end];
            if let Some((boundary, ch)) = slice
                .char_indices()
                .rev()
                .find(|(index, ch)| *index >= max_bytes / 2 && ch.is_whitespace())
            {
                end = start + boundary + ch.len_utf8();
            }
        }
        segments.push(text[start..end].to_string());
        start = end;
    }
    Ok(segments)
}

fn validate_ollama_embedding_vectors(
    vectors: &[Vec<f32>],
    expected_count: usize,
    expected_dimensions: usize,
) -> Result<()> {
    if vectors.len() != expected_count {
        anyhow::bail!(
            "Ollama returned {} embeddings for {} inputs",
            vectors.len(),
            expected_count
        );
    }
    for (index, vector) in vectors.iter().enumerate() {
        if vector.len() != expected_dimensions {
            anyhow::bail!(
                "Ollama embedding {index} returned {} dimensions; expected {}",
                vector.len(),
                expected_dimensions
            );
        }
        if vector.iter().any(|value| !value.is_finite()) {
            anyhow::bail!("Ollama embedding {index} contained a non-finite value");
        }
        let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
        if !norm.is_finite() || norm <= f32::EPSILON {
            anyhow::bail!("Ollama embedding {index} had a zero or invalid norm");
        }
    }
    Ok(())
}

fn validate_ollama_query_embedding(vector: &[f32], expected_dimensions: usize) -> Result<()> {
    let vectors = [vector.to_vec()];
    validate_ollama_embedding_vectors(&vectors, 1, expected_dimensions)
        .context("validating memory query embedding")
}

fn aggregate_ollama_logical_embeddings(
    plan: &OllamaLogicalEmbeddingPlan,
    physical_vectors: &[Vec<f32>],
    dimensions: usize,
) -> Result<Vec<Vec<f32>>> {
    validate_ollama_embedding_vectors(physical_vectors, plan.physical_inputs.len(), dimensions)?;
    if plan.physical_owner_indexes.len() != physical_vectors.len() {
        anyhow::bail!(
            "Ollama logical embedding plan has {} owners for {} physical vectors",
            plan.physical_owner_indexes.len(),
            physical_vectors.len()
        );
    }

    let mut logical_vectors = vec![vec![0.0f32; dimensions]; plan.logical_input_count];
    let mut segment_counts = vec![0usize; plan.logical_input_count];
    for owner in plan.physical_owner_indexes.iter().copied() {
        let Some(count) = segment_counts.get_mut(owner) else {
            anyhow::bail!("Ollama logical embedding plan referenced missing input {owner}");
        };
        *count += 1;
    }
    let mut logical_weights = vec![0usize; plan.logical_input_count];
    for ((owner, physical_input), vector) in plan
        .physical_owner_indexes
        .iter()
        .copied()
        .zip(&plan.physical_inputs)
        .zip(physical_vectors)
    {
        let Some(logical_vector) = logical_vectors.get_mut(owner) else {
            anyhow::bail!("Ollama logical embedding plan referenced missing input {owner}");
        };
        // Weight by exact covered UTF-8 bytes so a tiny tail segment cannot
        // influence the aggregate as much as a full physical segment. Empty
        // logical inputs still receive one unit and retain their sole vector.
        let weight = physical_input.len().max(1);
        let physical_norm = if segment_counts[owner] > 1 {
            let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
            if !norm.is_finite() || norm <= f32::EPSILON {
                anyhow::bail!(
                    "Ollama physical segment for logical input {owner} had a zero or invalid norm"
                );
            }
            Some(norm)
        } else {
            None
        };
        for (target, value) in logical_vector.iter_mut().zip(vector) {
            let normalized = physical_norm.map_or(*value, |norm| *value / norm);
            *target += normalized * weight as f32;
        }
        logical_weights[owner] = logical_weights[owner].saturating_add(weight);
    }

    for (index, ((vector, count), total_weight)) in logical_vectors
        .iter_mut()
        .zip(segment_counts)
        .zip(logical_weights)
        .enumerate()
    {
        if count == 0 {
            anyhow::bail!("Ollama logical input {index} had no physical embedding segments");
        }
        let inverse_weight = 1.0 / total_weight as f32;
        for value in vector.iter_mut() {
            *value *= inverse_weight;
        }
        if count == 1 {
            continue;
        }
        let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
        if !norm.is_finite() || norm <= f32::EPSILON {
            anyhow::bail!(
                "Ollama logical input {index} produced a zero or invalid aggregate vector"
            );
        }
        for value in vector.iter_mut() {
            *value /= norm;
        }
    }
    Ok(logical_vectors)
}

fn ollama_embedding_error_is_batch_retriable(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<OllamaHttpStatusError>()
            .is_some_and(|provider_error| {
                provider_error.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
                    || provider_error.status().is_server_error()
            })
            || cause
                .downcast_ref::<reqwest::Error>()
                .is_some_and(|reqwest_error| {
                    reqwest_error.is_timeout()
                        || reqwest_error.status().is_some_and(|status| {
                            status == reqwest::StatusCode::TOO_MANY_REQUESTS
                                || status.is_server_error()
                        })
                })
            // Routed seam: magicllm errors surface either pre-converted to
            // `OllamaHttpStatusError` (ProviderStatus) or as the typed
            // `LLMError` itself. Same retry semantics as the direct path:
            // 429/5xx/timeout retriable, other 4xx terminal.
            || cause
                .downcast_ref::<magicllm::LLMError>()
                .is_some_and(routed_embedding_error_is_retriable)
    })
}

fn routed_embedding_error_is_retriable(error: &magicllm::LLMError) -> bool {
    use magicllm::LLMError;
    match error.root_cause() {
        LLMError::Timeout
        | LLMError::RateLimited { .. }
        | LLMError::ProviderUnavailable
        | LLMError::WorkerWatchdog { .. } => true,
        LLMError::Transport(message) => {
            let lower = message.to_ascii_lowercase();
            lower.contains("timeout") || lower.contains("timed out")
        },
        LLMError::ProviderStatus { status, .. } => *status == 429 || (500..=599).contains(status),
        _ => false,
    }
}

fn format_error_chain(error: &anyhow::Error) -> String {
    error
        .chain()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}

fn memory_embedding_cache_dir(
    storage: &dyn MemoryStorage,
    cache_spec: &MemoryEmbeddingCacheSpec,
) -> PathBuf {
    let index_dir = storage
        .memory_index_manifest_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| storage.root().join("index"));
    index_dir
        .join(MEMORY_EMBEDDING_CACHE_DIR)
        .join(&cache_spec.namespace)
}

fn memory_embedding_cache_path(cache_dir: &Path, cache_key: &str) -> PathBuf {
    let prefix = cache_key.get(..2).unwrap_or("xx");
    cache_dir
        .join(prefix)
        .join(format!("{cache_key}.{MEMORY_EMBEDDING_CACHE_FILE_EXT}"))
}

async fn read_memory_embedding_cache_file(
    path: &Path,
    dimensions: usize,
) -> Result<Option<Vec<f32>>> {
    if !fs::try_exists(path)
        .await
        .with_context(|| format!("checking memory embedding cache file {}", path.display()))?
    {
        return Ok(None);
    }
    let bytes = fs::read(path)
        .await
        .with_context(|| format!("reading memory embedding cache file {}", path.display()))?;
    let expected_len = dimensions
        .checked_mul(std::mem::size_of::<f32>())
        .context("memory embedding cache dimensions overflow")?;
    if bytes.len() != expected_len {
        warn!(
            target: "memory_index",
            path = %path.display(),
            expected_bytes = expected_len,
            actual_bytes = bytes.len(),
            "Ignoring memory embedding cache entry with unexpected size"
        );
        remove_invalid_memory_embedding_cache_file_best_effort(path).await;
        return Ok(None);
    }

    let mut vector = Vec::with_capacity(dimensions);
    for chunk in bytes.chunks_exact(std::mem::size_of::<f32>()) {
        vector.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    if let Err(error) =
        validate_ollama_embedding_vectors(std::slice::from_ref(&vector), 1, dimensions)
    {
        warn!(
            target: "memory_index",
            path = %path.display(),
            error = %format_error_chain(&error),
            "Ignoring memory embedding cache entry with an invalid vector"
        );
        remove_invalid_memory_embedding_cache_file_best_effort(path).await;
        return Ok(None);
    }
    Ok(Some(vector))
}

async fn remove_invalid_memory_embedding_cache_file_best_effort(path: &Path) {
    match fs::remove_file(path).await {
        Ok(()) => {},
        Err(error) if error.kind() == ErrorKind::NotFound => {},
        Err(error) => warn!(
            target: "memory_index",
            path = %path.display(),
            error = %error,
            "Failed to remove invalid memory embedding cache entry"
        ),
    }
}

async fn write_memory_embedding_cache_file_best_effort(path: &Path, vector: &[f32]) {
    if let Err(error) = write_memory_embedding_cache_file(path, vector).await {
        warn!(
            target: "memory_index",
            path = %path.display(),
            error = %format_error_chain(&error),
            "Failed to write memory embedding cache entry; rebuild will continue"
        );
    }
}

async fn write_memory_embedding_cache_file(path: &Path, vector: &[f32]) -> Result<()> {
    let parent = path.parent().with_context(|| {
        format!(
            "memory embedding cache path has no parent: {}",
            path.display()
        )
    })?;
    fs::create_dir_all(parent)
        .await
        .with_context(|| format!("creating memory embedding cache dir {}", parent.display()))?;

    let mut bytes = Vec::with_capacity(std::mem::size_of_val(vector));
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    let sequence = TEMP_INDEX_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let tmp_path = path.with_extension(format!(
        "{MEMORY_EMBEDDING_CACHE_FILE_EXT}.tmp-{}-{sequence}",
        std::process::id()
    ));
    fs::write(&tmp_path, &bytes).await.with_context(|| {
        format!(
            "writing temporary memory embedding cache file {}",
            tmp_path.display()
        )
    })?;
    if let Err(error) = fs::rename(&tmp_path, path).await {
        let _ = fs::remove_file(&tmp_path).await;
        return Err(error).with_context(|| {
            format!(
                "promoting memory embedding cache file {} -> {}",
                tmp_path.display(),
                path.display()
            )
        });
    }
    Ok(())
}

async fn embed_query_for_manifest(
    query_text: &str,
    manifest: &MemoryIndexManifest,
) -> Result<Vec<f32>> {
    let current_embedding = MemoryEmbeddingConfig::from_env();
    if !current_embedding.manifest_is_compatible(manifest) {
        anyhow::bail!(
            "memory embedding contract changed before query execution: persisted={}, current={}",
            manifest.embedding_contract_id,
            current_embedding.empty_manifest().contract_id,
        );
    }
    match manifest.embedding_provider.as_str() {
        "ollama" => {
            let mut config = OllamaEmbedderConfig::from_env();
            if let Some(model) = manifest.embedding_model.as_ref() {
                config.model = model.clone();
            }
            config.dims = manifest.embedding_dimensions;
            config.query_timeout = memory_query_embedding_timeout(config.query_timeout);
            config.context_tokens.context(
                "Ollama embedding context is not configured; set runtime.ollama.embedding_context_tokens",
            )?;
            // All query sizes use the toolkit's one logical-query pipeline.
            // That pipeline owns fragmentation, pooling, one absolute deadline,
            // validation, and the shared coalescer key. A memory retrieval and
            // another retrieval branch can therefore never race two different
            // algorithms under one `ollama-query-v2` identity.
            let vector = OllamaEmbedder::new(config).embed_query(query_text).await?;
            validate_ollama_query_embedding(&vector, manifest.embedding_dimensions)?;
            Ok(vector)
        },
        #[cfg(feature = "test-hash-embeddings")]
        "test_hash" if test_hash_embeddings_enabled() => Ok(embed_memory_text_hashed(
            query_text,
            manifest.embedding_dimensions,
        )),
        "test_hash" => anyhow::bail!(
            "test-only memory embedding provider is unavailable outside test-enabled builds"
        ),
        other => anyhow::bail!("unsupported memory embedding provider `{other}`"),
    }
}

#[cfg(feature = "test-hash-embeddings")]
fn test_hash_embedding_batch(texts: &[String]) -> MemoryEmbeddingBatch {
    MemoryEmbeddingBatch {
        manifest: test_hash_embedding_manifest(),
        vectors: texts
            .iter()
            .map(|text| embed_memory_text_hashed(text, MEMORY_TEST_HASH_EMBEDDING_DIMS))
            .collect(),
    }
}

#[cfg(feature = "test-hash-embeddings")]
fn test_hash_embedding_manifest() -> MemoryEmbeddingManifest {
    MemoryEmbeddingManifest {
        provider: "test_hash".to_string(),
        model: None,
        dimensions: MEMORY_TEST_HASH_EMBEDDING_DIMS,
        contract_id: format!("memory-index-test-hash-v1-dims-{MEMORY_TEST_HASH_EMBEDDING_DIMS}"),
        fallback_reason: None,
    }
}

fn default_embedding_provider() -> String {
    "ollama".to_string()
}

fn default_embedding_dimensions() -> usize {
    0
}

#[cfg(feature = "test-hash-embeddings")]
fn test_hash_embeddings_enabled() -> bool {
    env::var("MAGICIAN_MEMORY_ALLOW_TEST_HASH_EMBEDDINGS")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn temporary_lancedb_index_dir(index_dir: &Path) -> Result<PathBuf> {
    let parent = index_dir
        .parent()
        .context("LanceDB memory index directory has no parent")?;
    let sequence = TEMP_INDEX_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    Ok(parent.join(format!(
        ".lancedb.tmp-{}-{}-{}",
        std::process::id(),
        Utc::now().timestamp_micros(),
        sequence
    )))
}

pub async fn quarantine_scope_memory_lancedb_index(
    storage: &dyn MemoryStorage,
) -> Result<Option<PathBuf>> {
    let _rebuild_guard = MEMORY_INDEX_REBUILD_LOCK.lock().await;
    let _write_lock = acquire_memory_index_write_lock(storage).await?;
    quarantine_scope_memory_lancedb_index_locked(storage).await
}

async fn quarantine_scope_memory_lancedb_index_locked(
    storage: &dyn MemoryStorage,
) -> Result<Option<PathBuf>> {
    let index_dir = storage.memory_lancedb_index_dir();
    if !fs::try_exists(&index_dir)
        .await
        .with_context(|| format!("checking LanceDB memory index {}", index_dir.display()))?
    {
        return Ok(None);
    }

    let parent = index_dir
        .parent()
        .context("LanceDB memory index directory has no parent")?;
    let sequence = TEMP_INDEX_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let quarantine_dir = parent.join(format!(
        "lancedb.corrupt-{}-{}-{}",
        std::process::id(),
        Utc::now().timestamp_micros(),
        sequence
    ));
    invalidate_lance_table_pool(&index_dir);
    fs::rename(&index_dir, &quarantine_dir)
        .await
        .with_context(|| {
            format!(
                "quarantining LanceDB memory index {} -> {}",
                index_dir.display(),
                quarantine_dir.display()
            )
        })?;
    invalidate_lance_table_pool(&index_dir);
    prune_lancedb_quarantine_dirs(parent, Some(&quarantine_dir)).await;
    Ok(Some(quarantine_dir))
}

/// How many quarantined LanceDB index copies to keep, newest first.
const MAX_LANCEDB_QUARANTINE_DIRS: usize = 3;

/// Drop all but the newest few quarantined index copies.
///
/// A quarantine is a forensic snapshot of a *derived, rebuildable* index, so
/// its value decays immediately and only the most recent ones can still explain
/// a live problem. Without a bound they accumulate one per corruption event and
/// never leave: a single machine had 72 of them holding 2.8 GB. Best-effort by
/// design — failing to prune must never fail the rebuild that is trying to
/// restore service.
///
/// `keep` is the quarantine this call just created, which is never a candidate
/// and counts against the bound. Pass `None` for a maintenance sweep, where the
/// whole bound is available to the directories already on disk. The sweep is
/// what reaches debris that predates this pruning: a quarantine-triggered prune
/// alone never runs again on a machine that has stopped corrupting its index,
/// so the pile it was written to bound outlives it.
async fn prune_lancedb_quarantine_dirs(parent: &Path, keep: Option<&Path>) {
    let Ok(mut entries) = fs::read_dir(parent).await else {
        return;
    };
    let mut quarantines = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if keep.is_some_and(|keep| path == keep) {
            continue;
        }
        let is_quarantine = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("lancedb.corrupt-"));
        if !is_quarantine {
            continue;
        }
        let modified = entry
            .metadata()
            .await
            .ok()
            .and_then(|metadata| metadata.modified().ok());
        quarantines.push((modified, path));
    }
    let retain = MAX_LANCEDB_QUARANTINE_DIRS.saturating_sub(usize::from(keep.is_some()));
    if quarantines.len() <= retain {
        return;
    }
    // Newest first; the one just written is kept separately and not in this list.
    quarantines.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| right.1.cmp(&left.1)));
    for (_, path) in quarantines.into_iter().skip(retain) {
        if let Err(error) = fs::remove_dir_all(&path).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                warn!(
                    target: "memory_index",
                    path = %path.display(),
                    error = %error,
                    "Failed to prune quarantined LanceDB memory index directory"
                );
                continue;
            }
        }
        info!(
            target: "memory_index",
            path = %path.display(),
            "Pruned quarantined LanceDB memory index directory"
        );
    }
}

async fn remove_lancedb_tmp_dir_best_effort(path: &Path) {
    if let Err(error) = fs::remove_dir_all(path).await {
        if error.kind() != std::io::ErrorKind::NotFound {
            warn!(
                target: "memory_index",
                path = %path.display(),
                error = %error,
                "Failed to remove temporary LanceDB memory index directory after rebuild failure"
            );
        }
    }
}

#[cfg(feature = "test-hash-embeddings")]
fn embed_memory_text_hashed(text: &str, dimensions: usize) -> Vec<f32> {
    let tokens = embedding_tokens(text);
    let mut vector = vec![0.0_f32; dimensions.max(1)];
    for token in &tokens {
        add_embedding_feature(&mut vector, token, 1.0);
    }
    for window in tokens.windows(2) {
        add_embedding_feature(&mut vector, &format!("{} {}", window[0], window[1]), 1.35);
    }
    for ngram in embedding_char_ngrams(text) {
        add_embedding_feature(&mut vector, &ngram, 0.35);
    }
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut vector {
            *value /= norm;
        }
    }
    vector
}

#[cfg(feature = "test-hash-embeddings")]
fn add_embedding_feature(vector: &mut [f32], feature: &str, weight: f32) {
    let hash = blake3::hash(feature.as_bytes());
    let bytes = hash.as_bytes();
    let bucket =
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize % vector.len().max(1);
    let sign = if bytes[4] & 1 == 0 { 1.0 } else { -1.0 };
    vector[bucket] += weight * sign;
}

#[cfg(feature = "test-hash-embeddings")]
fn embedding_tokens(text: &str) -> Vec<String> {
    text.split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter_map(|raw| {
            let token = raw.trim().to_ascii_lowercase();
            if token.len() < 2 {
                None
            } else {
                Some(token)
            }
        })
        .collect()
}

#[cfg(feature = "test-hash-embeddings")]
fn embedding_char_ngrams(text: &str) -> Vec<String> {
    let compact = text
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect::<Vec<_>>();
    if compact.len() < 4 {
        return Vec::new();
    }
    compact
        .windows(4)
        .map(|window| window.iter().collect::<String>())
        .collect()
}

fn lancedb_uri(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub fn memory_candidate_index_key(candidate: &MemoryCandidateDocument) -> String {
    format!(
        "v2:{}:{}:{}:{}:{}",
        length_prefixed_key_segment(scope_label(&candidate.scope)),
        optional_key_segment(candidate.agent_id.as_deref()),
        optional_key_segment(candidate.goal_id.as_deref()),
        length_prefixed_key_segment(&candidate.tier_name),
        length_prefixed_key_segment(&candidate.item_key),
    )
}

/// Revision-bound lookup key for advisory memory-index scores.
///
/// [`memory_candidate_index_key`] is a stable logical identity and is used for
/// index maintenance. Relevance must never be looked up with that key alone:
/// doing so could transfer a score from old content to a canonical rewrite
/// that lands after scoring. This key adds a compact hash of every field that
/// affects the indexed relevance row, so a renderer loaded after a relevant
/// write automatically rejects the stale boost.
pub fn memory_candidate_index_score_key(candidate: &MemoryCandidateDocument) -> String {
    let candidate_key = memory_candidate_index_key(candidate);
    let revision_hash = memory_candidate_index_revision_hash(candidate);
    memory_candidate_revision_score_key(&candidate_key, &revision_hash)
}

fn memory_candidate_revision_score_key(candidate_key: &str, revision_hash: &str) -> String {
    format!(
        "score-v2:{}:{}",
        length_prefixed_key_segment(candidate_key),
        length_prefixed_key_segment(revision_hash),
    )
}

fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn scope_label(scope: &TierScope) -> &'static str {
    match scope {
        TierScope::User => "user",
        TierScope::Agent => "agent",
        TierScope::AgentGoal => "agent_goal",
    }
}

// Tests that exercise `rebuild_scope_memory_index`,
// `score_fresh_memory_index`, and `score_fresh_memory_hybrid_index` against
// magician's concrete `AgentMemoryService` / `AgentDefinitionStore` live as
// integration tests in `magician/tests/memory_index_vector_index_integration.rs`
// so this crate has no dev-time dependency on magician.
#[cfg(test)]
mod tests {

    /// The bug: halving on a retriable failure was permanent. One transient
    /// timeout early in a full-corpus rebuild ran every remaining batch at the
    /// reduced width, silently multiplying wall time for the whole job.
    #[test]
    fn a_backed_off_batch_width_climbs_back_to_the_configured_size() {
        // Held until enough clean batches accumulate.
        for successes in 0..OLLAMA_BATCH_REGROWTH_SUCCESSES {
            assert_eq!(widened_ollama_batch_size(2, 32, successes), None);
        }
        // Then doubles, one step at a time.
        assert_eq!(
            widened_ollama_batch_size(2, 32, OLLAMA_BATCH_REGROWTH_SUCCESSES),
            Some(4)
        );
        assert_eq!(
            widened_ollama_batch_size(16, 32, OLLAMA_BATCH_REGROWTH_SUCCESSES),
            Some(32)
        );
    }

    /// Recovery must never exceed what the operator configured, including when
    /// doubling would overshoot.
    #[test]
    fn recovery_is_capped_at_the_configured_width() {
        assert_eq!(
            widened_ollama_batch_size(24, 32, OLLAMA_BATCH_REGROWTH_SUCCESSES),
            Some(32)
        );
        // Already at or above the ceiling: nothing to do, however long the run.
        assert_eq!(widened_ollama_batch_size(32, 32, 1_000), None);
        assert_eq!(widened_ollama_batch_size(64, 32, 1_000), None);
    }

    /// A width of 1 is the floor the halving path bottoms out at; it has to be
    /// able to leave, or the slowest possible width is also the stickiest.
    #[test]
    fn the_narrowest_width_can_still_recover() {
        assert_eq!(
            widened_ollama_batch_size(1, 8, OLLAMA_BATCH_REGROWTH_SUCCESSES),
            Some(2)
        );
    }
    use super::*;

    /// Quarantines are forensic copies of a *rebuildable* index, so their value
    /// decays immediately and only the most recent can still explain a live
    /// problem. Unbounded, they accumulate one per corruption event and never
    /// leave — one machine reached 72 of them holding 2.8 GB.
    #[tokio::test]
    async fn quarantine_pruning_keeps_only_the_newest_few() {
        let temp = tempfile::tempdir().expect("tempdir");
        let parent = temp.path();

        // Six older quarantines plus the one just written.
        for index in 0..6 {
            let dir = parent.join(format!("lancedb.corrupt-1-{index}-0"));
            tokio::fs::create_dir_all(dir.join("data"))
                .await
                .expect("create quarantine");
        }
        let keep = parent.join("lancedb.corrupt-1-999-0");
        tokio::fs::create_dir_all(&keep).await.expect("create keep");

        // Things that are not quarantines must be untouched.
        let live_index = parent.join("lancedb");
        tokio::fs::create_dir_all(&live_index).await.expect("live");
        let overlay = parent.join("temperature_overlay.json");
        tokio::fs::write(&overlay, b"{}").await.expect("overlay");

        prune_lancedb_quarantine_dirs(parent, Some(&keep)).await;

        let mut remaining = Vec::new();
        let mut entries = tokio::fs::read_dir(parent).await.expect("read_dir");
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("lancedb.corrupt-") {
                remaining.push(name);
            }
        }
        assert_eq!(
            remaining.len(),
            MAX_LANCEDB_QUARANTINE_DIRS,
            "kept {remaining:?}"
        );
        assert!(
            remaining
                .iter()
                .any(|name| name == "lancedb.corrupt-1-999-0"),
            "the quarantine just written is never a pruning candidate"
        );

        assert!(
            live_index.is_dir(),
            "the live index directory is not a quarantine"
        );
        assert!(
            overlay.is_file(),
            "durable overlay state is not a quarantine"
        );
    }

    /// Below the bound there is nothing to reclaim, and deleting anyway would
    /// throw away the only copies that can still explain a recent failure.
    #[tokio::test]
    async fn quarantine_pruning_is_inert_below_the_bound() {
        let temp = tempfile::tempdir().expect("tempdir");
        let parent = temp.path();
        let older = parent.join("lancedb.corrupt-1-0-0");
        tokio::fs::create_dir_all(&older).await.expect("older");
        let keep = parent.join("lancedb.corrupt-1-1-0");
        tokio::fs::create_dir_all(&keep).await.expect("keep");

        prune_lancedb_quarantine_dirs(parent, Some(&keep)).await;

        assert!(older.is_dir(), "nothing to prune yet");
        assert!(keep.is_dir());
    }

    /// The maintenance sweep is the only thing that reaches a pile built before
    /// the bound existed: the quarantine-time prune stops firing once the index
    /// stops corrupting, so 72 stale copies sat on one machine for two months
    /// after the bound shipped. With no new quarantine to reserve a slot, the
    /// whole bound belongs to what is already on disk.
    #[tokio::test]
    async fn a_maintenance_sweep_bounds_a_pile_no_new_quarantine_would_reach() {
        let temp = tempfile::tempdir().expect("tempdir");
        let parent = temp.path();

        for index in 0..8 {
            let dir = parent.join(format!("lancedb.corrupt-{index}"));
            tokio::fs::create_dir_all(dir.join("data"))
                .await
                .expect("create quarantine");
        }
        let live_index = parent.join("lancedb");
        tokio::fs::create_dir_all(&live_index).await.expect("live");

        prune_lancedb_quarantine_dirs(parent, None).await;

        let mut remaining = 0_usize;
        let mut entries = tokio::fs::read_dir(parent).await.expect("read_dir");
        while let Ok(Some(entry)) = entries.next_entry().await {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("lancedb.corrupt-")
            {
                remaining += 1;
            }
        }
        assert_eq!(
            remaining, MAX_LANCEDB_QUARANTINE_DIRS,
            "a sweep with no new quarantine keeps the full bound"
        );
        assert!(live_index.is_dir(), "the live index is not a quarantine");
    }

    fn test_candidate(item_key: &str, text: &str) -> MemoryCandidateDocument {
        MemoryCandidateDocument {
            principal: Some("anonymous".to_string()),
            workspace: Some("default".to_string()),
            agent_id: None,
            scope: TierScope::User,
            tier_name: "preferences".to_string(),
            semantic_memory_type: crate::memory_candidates::SemanticMemoryType::UserPreference,
            goal_id: None,
            item_key: item_key.to_string(),
            source_path: None,
            json_pointer: format!("/{item_key}"),
            content_hash: format!("hash-{item_key}"),
            last_updated: Utc::now(),
            confidence: None,
            text: text.to_string(),
            metadata_json: serde_json::json!({}),
        }
    }

    fn test_ollama_embedding_config(context_tokens: u32) -> MemoryEmbeddingConfig {
        MemoryEmbeddingConfig {
            preference: MemoryEmbeddingPreference::Ollama,
            model: "qwen3-embedding:4b".to_string(),
            dimensions: 2_560,
            base_url: "http://127.0.0.1:11435".to_string(),
            timeout: Duration::from_secs(30),
            warmup_timeout: Duration::from_secs(30),
            batch_size: 8,
            max_batch_chars: 6_000,
            max_retries: 0,
            retry_backoff: Duration::from_millis(1),
            warmup: false,
            keep_alive: None,
            context_tokens: Some(context_tokens),
            batch_tokens: Some(512),
        }
    }

    /// The embedding endpoint is execution-only: the same model at another
    /// URL yields the same vectors, so moving the daemon must not stale an
    /// index. The pinned id is the one every personal memory index carried
    /// before the endpoint was briefly folded into the contract (2026-08-24);
    /// restoring it made those indexes compatible again without a rebuild.
    #[test]
    fn memory_contract_ignores_the_embedding_endpoint_and_stays_pinned() {
        let mut here = test_ollama_embedding_config(8_192);
        here.model = "hf.co/mykor/pplx-embed-v1-4b-GGUF:Q6_K".to_string();
        let mut elsewhere = here.clone();
        elsewhere.base_url = "http://ollama-embed.internal:11434".to_string();

        let here_id = here.empty_manifest().contract_id;
        assert_eq!(here_id, elsewhere.empty_manifest().contract_id);
        assert_eq!(
            here_id,
            "b054770bfdc0a8c14bcad61058d38a19c2b43231cc63cebc436b806451b089a7"
        );
    }

    fn test_index_manifest(embedding: &MemoryEmbeddingManifest) -> MemoryIndexManifest {
        MemoryIndexManifest {
            index_version: MEMORY_INDEX_VERSION.to_string(),
            schema_version: MEMORY_INDEX_SCHEMA_VERSION,
            backend: MEMORY_INDEX_BACKEND.to_string(),
            backend_status: "ready:test".to_string(),
            embedding_provider: embedding.provider.clone(),
            embedding_model: embedding.model.clone(),
            embedding_dimensions: embedding.dimensions,
            embedding_contract_id: embedding.contract_id.clone(),
            embedding_fallback_reason: embedding.fallback_reason.clone(),
            principal: Some("anonymous".to_string()),
            workspace: Some("default".to_string()),
            rebuilt_at: Utc::now(),
            document_count: 1,
            chunk_count: 1,
            source_count: 1,
            source_hashes: BTreeMap::new(),
            agents: Vec::new(),
        }
    }

    #[test]
    fn lance_search_concurrency_prefers_valid_env_over_configured() {
        assert_eq!(
            resolve_lance_search_concurrency(None, DEFAULT_LANCE_SEARCH_CONCURRENCY),
            DEFAULT_LANCE_SEARCH_CONCURRENCY
        );
        assert_eq!(resolve_lance_search_concurrency(None, 1), 1);
        assert_eq!(resolve_lance_search_concurrency(Some("8"), 1), 8);
        assert_eq!(resolve_lance_search_concurrency(Some("0"), 1), 1);
        assert_eq!(resolve_lance_search_concurrency(Some("nope"), 1), 1);
        assert_eq!(
            resolve_lance_search_concurrency(None, 0),
            DEFAULT_LANCE_SEARCH_CONCURRENCY
        );
        assert_eq!(
            resolve_lance_search_concurrency(Some(""), 4),
            DEFAULT_LANCE_SEARCH_CONCURRENCY
        );
    }

    // Mirrors the acquire/try_acquire semantics used by `lance_search_semaphore`
    // to bound request-path lance searches. Built from a locally-constructed
    // `Semaphore` (not the process-lifetime one) so it stays deterministic and
    // hermetic — no env vars, no shared global state across tests.
    #[tokio::test]
    async fn lance_search_semaphore_caps_in_flight() {
        let sem = tokio::sync::Semaphore::new(2);

        // Two concurrent request-path searches may hold permits at once.
        let first = sem.acquire().await.expect("first permit");
        let second = sem.acquire().await.expect("second permit");

        // A third search must be turned away while both permits are held.
        assert!(
            sem.try_acquire().is_err(),
            "third acquire should fail while the cap is reached"
        );

        // Once one search finishes and its permit drops, capacity frees up.
        drop(first);
        let third = sem
            .try_acquire()
            .expect("acquire should succeed after a permit is released");

        drop(second);
        drop(third);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn filtered_lancedb_fts_runs_on_an_ordinary_stack_scheduler_root() {
        let tmp = tempfile::tempdir().unwrap();
        let index_dir = tmp.path().join("lancedb");
        let candidates = vec![test_candidate(
            "warehouse",
            "Production analytics use the DuckDB warehouse",
        )];
        let chunks = build_memory_index_chunks(&candidates);
        let embeddings = vec![vec![1.0_f32, 0.0_f32]; chunks.len()];
        let record_batch = lancedb_record_batch(&candidates, &chunks, &embeddings, 2).unwrap();
        replace_lancedb_index_dir(&index_dir, record_batch)
            .await
            .unwrap();

        // This is the same filter shape that entered DataFusion's recursive
        // SQL parser in the production crash. The single-worker Tokio runtime
        // deliberately uses its ordinary stack policy.
        let predicate = memory_index_prompt_visibility_predicate("personal-assistant");
        let search_dir = index_dir.clone();
        let hits = run_lancedb_search_at_scheduler_root_with_timeout(
            Duration::from_secs(5),
            "test filtered LanceDB FTS index",
            Box::new(move || {
                Box::pin(async move {
                    search_lancedb_fts(&search_dir, "DuckDB warehouse", 8, Some(&predicate), "")
                        .await
                })
            }),
        )
        .await
        .unwrap();

        assert_eq!(
            hits.first().map(|hit| hit.candidate_key.as_str()),
            Some(memory_candidate_index_key(&candidates[0]).as_str())
        );
        let scores = score_lancedb_index(&index_dir, "DuckDB warehouse", 8, "")
            .await
            .unwrap();
        assert!(scores.contains_key(&memory_candidate_index_key(&candidates[0])));
    }

    #[test]
    fn agent_visibility_filter_includes_user_and_current_agent_memory() {
        assert_eq!(
            memory_index_agent_visibility_predicate("ceo"),
            "(scope = 'user' OR agent_id = 'ceo')"
        );
    }

    #[test]
    fn agent_visibility_filter_escapes_sql_literals() {
        assert_eq!(
            memory_index_agent_visibility_predicate("owner's-agent"),
            "(scope = 'user' OR agent_id = 'owner''s-agent')"
        );
    }

    /// An episodes target must claim exactly this agent's episode documents:
    /// too narrow and stale rows survive the swap, too wide and another
    /// agent's or another tier's rows are dropped from the index.
    #[test]
    fn an_episodes_target_claims_only_that_agents_episode_documents() {
        use crate::episode_candidates::{episode_candidate_document, EpisodeCandidateProjectionV1};

        let target = MemoryIndexIncrementalTarget::Episodes {
            agent_id: "envoy".to_string(),
        };
        let episode_for = |agent: &str, episode_id: &str| {
            episode_candidate_document(
                agent,
                std::path::PathBuf::from("/tmp/ep.json"),
                EpisodeCandidateProjectionV1 {
                    agent_id: agent.to_string(),
                    episode_id: episode_id.to_string(),
                    goal_key: "goal-1".to_string(),
                    outcome_summary: "did a thing".to_string(),
                    ..Default::default()
                },
            )
        };

        assert!(target.matches(&episode_for("envoy", "ep-1")));
        assert!(
            target.matches(&episode_for("envoy", "ep-2")),
            "episodes on different goals are still one surface"
        );
        assert!(
            !target.matches(&episode_for("other-agent", "ep-1")),
            "another agent's episodes are not this target's to replace"
        );

        // A tier candidate for the same agent must survive the swap.
        let mut tier_candidate = test_candidate("preferences", "some tier text");
        tier_candidate.agent_id = Some("envoy".to_string());
        tier_candidate.scope = TierScope::Agent;
        assert!(!target.matches(&tier_candidate));
    }

    /// Score invalidation must reach an episode whatever goal it carries, and
    /// must not reach anything else. Matching too little is the dangerous
    /// direction: it retains a relevance score for content that just changed.
    #[test]
    fn an_episodes_target_matches_scored_keys_across_every_goal() {
        use crate::episode_candidates::{episode_candidate_document, EpisodeCandidateProjectionV1};

        let target = MemoryIndexIncrementalTarget::Episodes {
            agent_id: "envoy".to_string(),
        };
        let key_for = |agent: &str, goal: &str, episode_id: &str| {
            memory_candidate_index_key(&episode_candidate_document(
                agent,
                std::path::PathBuf::from("/tmp/ep.json"),
                EpisodeCandidateProjectionV1 {
                    agent_id: agent.to_string(),
                    episode_id: episode_id.to_string(),
                    goal_key: goal.to_string(),
                    outcome_summary: "did a thing".to_string(),
                    ..Default::default()
                },
            ))
        };

        assert!(target.matches_candidate_key(&key_for("envoy", "task_a", "ep-1")));
        assert!(
            target.matches_candidate_key(&key_for("envoy", "harness:envoy:daily", "ep-2")),
            "a goal containing the separator must still match"
        );
        assert!(!target.matches_candidate_key(&key_for("other", "task_a", "ep-1")));

        // A tier candidate for the same agent keeps its score.
        let mut tier_candidate = test_candidate("preferences", "tier text");
        tier_candidate.agent_id = Some("envoy".to_string());
        tier_candidate.scope = TierScope::Agent;
        tier_candidate.goal_id = Some("task_a".to_string());
        assert!(!target.matches_candidate_key(&memory_candidate_index_key(&tier_candidate)));
    }

    /// The journal key is what coalesces repeat writes into one pending entry.
    /// Two agents must never collapse onto one, and an agent id containing the
    /// separator must not collide with a different pair.
    #[test]
    fn episode_journal_keys_are_per_agent_and_unambiguous() {
        let key_for = |agent: &str| {
            memory_index_change_key(&MemoryIndexChange::Episodes {
                agent_id: agent.to_string(),
            })
        };
        assert_eq!(key_for("envoy"), key_for("envoy"));
        assert_ne!(key_for("envoy"), key_for("cto"));
        assert_ne!(key_for("a:b"), key_for("a"));
        assert_ne!(
            key_for("envoy"),
            memory_index_change_key(&MemoryIndexChange::UserKnowledge)
        );
    }

    /// The journal is persisted, so the new variant has to survive a round
    /// trip in the shape the on-disk file uses.
    #[test]
    fn the_episodes_change_round_trips_through_the_journal_shape() {
        let change = MemoryIndexChange::Episodes {
            agent_id: "envoy".to_string(),
        };
        let encoded = serde_json::to_value(&change).expect("serializes");
        assert_eq!(
            encoded.get("kind").and_then(|kind| kind.as_str()),
            Some("episodes")
        );
        let decoded: MemoryIndexChange = serde_json::from_value(encoded).expect("round trips");
        assert!(matches!(
            decoded,
            MemoryIndexChange::Episodes { agent_id } if agent_id == "envoy"
        ));
    }

    #[test]
    fn prompt_visibility_filter_excludes_search_only_environment_tier() {
        assert_eq!(
            memory_index_prompt_visibility_predicate("personal-assistant"),
            "((scope = 'user' OR agent_id = 'personal-assistant')) AND tier_name NOT LIKE 'environment_knowledge%'"
        );
    }

    #[test]
    fn pending_change_filter_removes_only_stale_affected_scores() {
        let stale_changed_key = memory_candidate_index_key(&test_candidate(
            "database",
            "The database used to be SQLite",
        ));
        let unaffected_key = memory_candidate_index_key(&test_candidate(
            "timezone",
            "The user's timezone is Asia/Kolkata",
        ));
        let scores = BTreeMap::from([
            (stale_changed_key.clone(), 3.5),
            (unaffected_key.clone(), 2.5),
        ]);

        let retained = retain_unaffected_memory_index_scores(
            scores,
            &BTreeSet::from([stale_changed_key.clone()]),
        );

        assert!(!retained.contains_key(&stale_changed_key));
        assert_eq!(retained.get(&unaffected_key), Some(&2.5));
    }

    #[test]
    fn pending_target_identity_filters_only_the_affected_scored_source() {
        let user_target = MemoryIndexIncrementalTarget::UserMemory { tiers: Vec::new() };
        let user_candidate = test_candidate("timezone", "Asia/Kolkata");
        assert!(memory_candidate_index_key(&user_candidate)
            .starts_with(&user_target.candidate_key_prefix()));
        assert!(user_target.matches_candidate_key(&memory_candidate_index_key(&user_candidate)));

        let native_target = MemoryIndexIncrementalTarget::NativeTier {
            agent_id: "assistant:one".to_string(),
            tier: MemoryTierDefinition {
                name: "task:progress".to_string(),
                scope: TierScope::Agent,
                description: String::new(),
                schema: std::collections::BTreeMap::new(),
                render: crate::memory_tiers::RenderConfig {
                    format: String::new(),
                    template: String::new(),
                },
                retention: crate::memory_tiers::RetentionMode::Forever,
            },
            goal_id: None,
        };
        let mut native_candidate = test_candidate("task:42", "active");
        native_candidate.scope = TierScope::Agent;
        native_candidate.agent_id = Some("assistant:one".to_string());
        native_candidate.tier_name = "task:progress".to_string();
        let native_key = memory_candidate_index_key(&native_candidate);
        let native_prefix = native_target.candidate_key_prefix();
        let native_upper = format!("{};", native_prefix.strip_suffix(':').unwrap());

        assert!(native_key.starts_with(&native_prefix));
        assert!(native_key >= native_prefix && native_key < native_upper);
        assert!(native_target.matches_indexed_tier_name("task:progress"));
        assert!(native_target.matches_indexed_tier_name("task:progress.notes"));
        assert!(!native_target.matches_indexed_tier_name("task:progressive"));
        assert!(native_target.matches_candidate_key(&native_key));
        let mut child_tier_candidate = native_candidate.clone();
        child_tier_candidate.tier_name = "task:progress.notes".to_string();
        assert!(
            native_target.matches_candidate_key(&memory_candidate_index_key(&child_tier_candidate))
        );
        let mut similarly_prefixed_tier = native_candidate.clone();
        similarly_prefixed_tier.tier_name = "task:progressive".to_string();
        assert!(!native_target
            .matches_candidate_key(&memory_candidate_index_key(&similarly_prefixed_tier)));
        native_candidate.agent_id = Some("assistant:two".to_string());
        assert!(!memory_candidate_index_key(&native_candidate)
            .starts_with(&native_target.candidate_key_prefix()));
        assert!(
            !native_target.matches_candidate_key(&memory_candidate_index_key(&native_candidate))
        );
    }

    #[test]
    fn pending_journal_is_soft_only_when_the_exact_delta_can_be_overlaid() {
        let status = MemoryIndexStatus {
            manifest: None,
            stale: true,
            reason: "pending_changes".to_string(),
            current_document_count: 2,
            current_source_count: 1,
        };
        let pending = MemoryIndexChangeSnapshot {
            entries: vec![(
                "user_knowledge".to_string(),
                1,
                MemoryIndexChange::UserKnowledge,
            )],
        };
        let empty = MemoryIndexChangeSnapshot {
            entries: Vec::new(),
        };

        assert!(memory_index_status_allows_snapshot_overlay(
            &status, &pending
        ));
        assert!(!memory_index_status_allows_snapshot_overlay(
            &status, &empty
        ));
        let mut hard_status = status;
        hard_status.reason = "schema_version_changed".to_string();
        assert!(!memory_index_status_allows_snapshot_overlay(
            &hard_status,
            &pending
        ));
    }

    #[test]
    fn same_model_and_dimensions_with_new_context_forces_full_vector_rebuild() {
        let old_config = test_ollama_embedding_config(8_192);
        let new_config = test_ollama_embedding_config(16_384);
        let old_embedding = old_config.empty_manifest();
        let new_embedding = new_config.empty_manifest();
        let manifest = test_index_manifest(&old_embedding);

        assert_eq!(old_embedding.model, new_embedding.model);
        assert_eq!(old_embedding.dimensions, new_embedding.dimensions);
        assert_ne!(old_embedding.contract_id, new_embedding.contract_id);
        assert_ne!(
            old_config.embedding_cache_spec().unwrap().namespace,
            new_config.embedding_cache_spec().unwrap().namespace,
            "a rebuild must not refill from vectors cached under the old contract"
        );
        assert_eq!(
            manifest_lancedb_merge_blocker(&manifest, &new_embedding),
            Some("embedding_config_changed")
        );
        assert!(!manifest_embedding_matches(&manifest, &new_embedding));
    }

    #[test]
    fn execution_only_ollama_tuning_keeps_the_persisted_vector_contract() {
        let original = test_ollama_embedding_config(8_192);
        let mut tuned = original.clone();
        tuned.batch_size = 32;
        tuned.timeout = Duration::from_secs(120);
        tuned.keep_alive = Some("30m".to_string());

        assert_eq!(
            original.empty_manifest().contract_id,
            tuned.empty_manifest().contract_id
        );
        assert_eq!(
            original.embedding_cache_spec().unwrap().namespace,
            tuned.embedding_cache_spec().unwrap().namespace
        );
    }

    #[test]
    fn physical_batch_change_forces_full_vector_and_cache_rebuild() {
        let original = test_ollama_embedding_config(8_192);
        let mut changed = original.clone();
        changed.batch_tokens = Some(1_024);
        let original_embedding = original.empty_manifest();
        let changed_embedding = changed.empty_manifest();
        let manifest = test_index_manifest(&original_embedding);

        assert_ne!(
            original_embedding.contract_id,
            changed_embedding.contract_id
        );
        assert_ne!(
            original.embedding_cache_spec().unwrap().namespace,
            changed.embedding_cache_spec().unwrap().namespace
        );
        assert_eq!(
            manifest_lancedb_merge_blocker(&manifest, &changed_embedding),
            Some("embedding_config_changed")
        );
    }

    #[test]
    fn missing_persisted_embedding_contract_fails_closed_to_rebuild() {
        let embedding = test_ollama_embedding_config(8_192).empty_manifest();
        let mut raw = serde_json::to_value(test_index_manifest(&embedding)).unwrap();
        raw.as_object_mut().unwrap().remove("embedding_contract_id");
        let legacy: MemoryIndexManifest = serde_json::from_value(raw).unwrap();

        assert!(legacy.embedding_contract_id.is_empty());
        assert_eq!(
            manifest_lancedb_merge_blocker(&legacy, &embedding),
            Some("embedding_config_changed")
        );
    }

    #[test]
    fn manifest_candidate_source_carries_revision_and_complete_freshness_hash() {
        let candidate = test_candidate("database", "Analytics use DuckDB");
        let encoded = memory_candidate_manifest_source_hash(&candidate).unwrap();
        let encoded_body = encoded
            .strip_prefix("candidate-source-v2:")
            .expect("versioned candidate source prefix");
        let (revision, full_source_hash) =
            parse_length_prefixed_key_segment(encoded_body).expect("encoded source fields");

        assert_eq!(revision, memory_candidate_index_revision_hash(&candidate));
        assert_eq!(
            full_source_hash,
            stable_memory_candidate_source_hash(&candidate).unwrap()
        );
    }

    #[test]
    fn revision_binding_rejects_legacy_unencoded_manifest_source_hash() {
        let candidate = test_candidate("database", "Analytics use DuckDB");
        let candidate_key = memory_candidate_index_key(&candidate);
        let legacy_source_hash = stable_memory_candidate_source_hash(&candidate).unwrap();
        let bound = bind_memory_index_scores_to_revisions(
            BTreeMap::from([(candidate_key.clone(), 2.5)]),
            &BTreeMap::from([(format!("candidate:{candidate_key}"), legacy_source_hash)]),
        )
        .unwrap();

        assert!(bound.is_empty());
    }

    #[test]
    fn index_revision_changes_only_when_index_relevance_inputs_change() {
        let candidate = test_candidate("database", "Analytics use DuckDB");
        let revision = memory_candidate_index_revision_hash(&candidate);

        let mut presentation_only = candidate.clone();
        presentation_only.confidence = Some(0.25);
        presentation_only.metadata_json = serde_json::json!({ "audit": "new" });
        assert_eq!(
            revision,
            memory_candidate_index_revision_hash(&presentation_only)
        );

        let mut changed_content = candidate.clone();
        changed_content.text = "Analytics use PostgreSQL".to_string();
        changed_content.content_hash = hash_bytes(changed_content.text.as_bytes());
        assert_ne!(
            revision,
            memory_candidate_index_revision_hash(&changed_content)
        );

        let mut changed_type = candidate;
        changed_type.semantic_memory_type =
            crate::memory_candidates::SemanticMemoryType::ProjectContext;
        assert_ne!(
            revision,
            memory_candidate_index_revision_hash(&changed_type)
        );
    }

    #[test]
    fn app_index_revision_binds_source_head_and_provider_partition() {
        let mut candidate = test_candidate("memory:candidate:1", "Asha is a mentor");
        candidate.metadata_json = serde_json::json!({
            "app_index_identity": {
                "content_revision": 2,
                "source_head_digest": "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "provider_partition_digest": "blake3:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            }
        });
        let revision = memory_candidate_index_revision_hash(&candidate);

        candidate.metadata_json["app_index_identity"]["source_head_digest"] = serde_json::json!(
            "blake3:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
        );
        assert_ne!(revision, memory_candidate_index_revision_hash(&candidate));

        candidate.metadata_json["app_index_identity"]["source_head_digest"] = serde_json::json!(
            "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        candidate.metadata_json["app_index_identity"]["provider_partition_digest"] = serde_json::json!(
            "blake3:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
        );
        assert_ne!(revision, memory_candidate_index_revision_hash(&candidate));
    }

    #[test]
    fn canonical_write_after_second_snapshot_cannot_inherit_old_revision_score() {
        let before_write =
            test_candidate("database", "Production analytics use the legacy warehouse");
        let candidate_key = memory_candidate_index_key(&before_write);
        let old_source_hash = memory_candidate_manifest_source_hash(&before_write).unwrap();
        let manifest_source_hashes =
            BTreeMap::from([(format!("candidate:{candidate_key}"), old_source_hash)]);
        let bound_scores = bind_memory_index_scores_to_revisions(
            BTreeMap::from([(candidate_key.clone(), 3.5)]),
            &manifest_source_hashes,
        )
        .unwrap();

        let old_score_key = memory_candidate_index_score_key(&before_write);
        assert_eq!(bound_scores.get(&old_score_key), Some(&3.5));

        // Deterministically model a canonical write landing after the scorer's
        // second journal snapshot but before prompt candidates are loaded.
        let mut after_write = before_write;
        after_write.text = "Production analytics now use DuckDB".to_string();
        after_write.content_hash = hash_bytes(after_write.text.as_bytes());
        let new_score_key = memory_candidate_index_score_key(&after_write);

        assert_eq!(candidate_key, memory_candidate_index_key(&after_write));
        assert_ne!(old_score_key, new_score_key);
        assert!(
            bound_scores.get(&new_score_key).is_none(),
            "new canonical content must fall back to direct ranking rather than inherit stale Lance relevance"
        );
    }

    #[test]
    fn large_user_memory_pending_filter_is_bounded_by_returned_score_map() {
        let target = MemoryIndexIncrementalTarget::UserMemory { tiers: Vec::new() };
        let mut scores = BTreeMap::new();
        let bounded_score_count = chunk_search_limit(MEMORY_INDEX_SEARCH_LIMIT);
        for index in 0..bounded_score_count {
            let mut candidate =
                test_candidate(&format!("bounded-{index}"), &format!("candidate {index}"));
            if index % 2 == 1 {
                candidate.scope = TierScope::Agent;
                candidate.agent_id = Some("assistant".to_string());
            }
            scores.insert(memory_candidate_index_key(&candidate), index as f32);
        }

        let affected = affected_scored_candidate_keys(&[target], &scores);
        let score_keys = scores.keys().cloned().collect::<BTreeSet<_>>();

        assert_eq!(scores.len(), bounded_score_count);
        assert_eq!(affected.len(), bounded_score_count / 2);
        assert!(affected.is_subset(&score_keys));
        // The helper accepts only the already bounded result map—there is no
        // storage/table argument through which a large canonical user-memory
        // universe or all Lance rows could be loaded a second time.
    }

    #[test]
    fn identical_pending_snapshots_are_resolved_once_but_new_generation_is_not_lost() {
        let change = MemoryIndexChange::UserKnowledge;
        let before = MemoryIndexChangeSnapshot {
            entries: vec![("user_knowledge".to_string(), 1, change.clone())],
        };
        let same = MemoryIndexChangeSnapshot {
            entries: vec![("user_knowledge".to_string(), 1, change.clone())],
        };
        let rewritten = MemoryIndexChangeSnapshot {
            entries: vec![("user_knowledge".to_string(), 2, change)],
        };

        assert_eq!(
            distinct_memory_index_change_snapshots(&before, &same).len(),
            1
        );
        assert_eq!(
            distinct_memory_index_change_snapshots(&before, &rewritten).len(),
            2
        );
    }

    #[test]
    fn absent_or_compatible_empty_metadata_initializes_only_without_a_table() {
        assert!(derived_metadata_allows_initial_creation(true, false));
        assert!(!derived_metadata_allows_initial_creation(true, true));
        assert!(!derived_metadata_allows_initial_creation(false, false));
        assert!(!derived_metadata_allows_initial_creation(false, true));
    }

    #[tokio::test]
    async fn lancedb_initial_creation_probe_detects_unknown_tables_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let index_dir = tmp.path().join("lancedb");
        assert!(!lancedb_index_contains_any_table(&index_dir).await.unwrap());

        std::fs::create_dir_all(&index_dir).unwrap();
        let db = connect(&lancedb_uri(&index_dir)).execute().await.unwrap();
        let schema = Arc::new(Schema::new(vec![Field::new(
            "value",
            DataType::Utf8,
            false,
        )]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(StringArray::from(vec!["do-not-replace"])) as ArrayRef],
        )
        .unwrap();
        db.create_table("unknown_existing_table", batch)
            .execute()
            .await
            .unwrap();

        assert!(lancedb_index_contains_any_table(&index_dir).await.unwrap());
    }

    #[test]
    fn ollama_input_split_preserves_every_byte_within_physical_limit() {
        let input = "alpha beta gamma delta 🚀 epsilon zeta eta theta";
        let segments = split_ollama_embedding_input(input, 12).unwrap();

        assert_eq!(segments.concat(), input);
        assert!(segments.len() > 1);
        assert!(segments.iter().all(|segment| segment.len() <= 12));
        assert!(segments.iter().all(|segment| segment.is_char_boundary(0)));
    }

    #[test]
    fn ollama_physical_input_limit_uses_smaller_ceiling_and_reserves_special_tokens() {
        assert_eq!(ollama_physical_input_byte_limit(8_192, None), 8_184);
        assert_eq!(ollama_physical_input_byte_limit(8_192, Some(512)), 504);
        assert_eq!(ollama_physical_input_byte_limit(256, Some(512)), 248);
        assert_eq!(ollama_physical_input_byte_limit(8, Some(512)), 1);
        assert_eq!(ollama_physical_input_byte_limit(1, None), 1);
    }

    #[test]
    fn ollama_input_fragmentation_honors_physical_num_batch_ceiling() {
        let text = "x".repeat(2_000);
        let plan = plan_ollama_logical_embedding_inputs(
            &[text.clone()],
            ollama_physical_input_byte_limit(8_192, Some(512)),
        )
        .unwrap();

        assert!(plan.physical_inputs.len() > 1);
        assert_eq!(plan.physical_inputs.concat(), text);
        assert!(plan.physical_inputs.iter().all(|input| input.len() <= 504));
        assert!(plan.physical_owner_indexes.iter().all(|owner| *owner == 0));
    }

    #[test]
    fn ollama_embedding_request_refuses_truncation_and_keeps_num_batch_as_option() {
        let body = ollama_embedding_request_body(
            "embed-model",
            &["complete input".to_string()],
            Some("-1"),
            Some(8_192),
            Some(512),
        );

        assert_eq!(body["model"], json!("embed-model"));
        assert_eq!(body["input"], json!(["complete input"]));
        assert_eq!(body["truncate"], json!(false));
        assert_eq!(body["keep_alive"], json!(-1));
        assert_eq!(body["options"]["num_ctx"], json!(8_192));
        assert_eq!(body["options"]["num_batch"], json!(512));
    }

    #[test]
    fn ollama_status_error_preserves_body_and_retry_classification() {
        let too_large = anyhow::Error::new(OllamaHttpStatusError::new(
            "http://localhost/api/embed".to_string(),
            reqwest::StatusCode::BAD_REQUEST,
            br#"{"error":"input is too large to process; increase physical batch size"}"#,
        ));
        assert!(format_error_chain(&too_large).contains("increase physical batch size"));
        assert!(!ollama_embedding_error_is_batch_retriable(&too_large));

        let busy = anyhow::Error::new(OllamaHttpStatusError::new(
            "http://localhost/api/embed".to_string(),
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            br#"{"error":"runner busy"}"#,
        ));
        assert!(ollama_embedding_error_is_batch_retriable(&busy));
    }

    #[test]
    fn foreground_embedding_budget_is_absolute_while_writes_are_per_batch() {
        let per_batch = Duration::from_secs(5);
        assert_eq!(
            embedding_request_budget(None, per_batch, "write").unwrap(),
            per_batch
        );

        let deadline = Instant::now() + Duration::from_millis(100);
        let remaining =
            embedding_request_budget(Some(deadline), per_batch, "foreground query").unwrap();
        assert!(remaining > Duration::ZERO);
        assert!(remaining <= Duration::from_millis(100));

        let expired = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .unwrap();
        assert!(
            embedding_request_budget(Some(expired), per_batch, "foreground query")
                .unwrap_err()
                .to_string()
                .contains("exhausted its total")
        );
    }

    #[tokio::test]
    async fn memory_index_write_eval_probe_rejects_empty_fixtures_without_provider_work() {
        let (empty_signal, _empty_admitted) = oneshot::channel();
        let (_empty_release, empty_release_wait) = oneshot::channel();
        assert!(eval_only_run_memory_index_write_embedding_pipeline(
            &[],
            empty_signal,
            empty_release_wait,
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("at least one synthetic input"));
        let (blank_signal, _blank_admitted) = oneshot::channel();
        let (_blank_release, blank_release_wait) = oneshot::channel();
        assert!(eval_only_run_memory_index_write_embedding_pipeline(
            &["  ".to_string()],
            blank_signal,
            blank_release_wait,
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("must be non-empty"));
    }

    #[tokio::test]
    async fn write_probe_signal_fires_only_after_its_provider_admission() {
        let _scheduler_guard = crate::embedding_scheduler::EMBEDDING_ADMISSION_TEST_LOCK
            .lock()
            .await;
        crate::embedding_scheduler::install_embedding_admission_capacity(1);
        let held = acquire_embedding_permit(EmbeddingPriority::Write).await;
        let (signal, mut admitted) = oneshot::channel();
        let (release, release_wait) = oneshot::channel();
        let waiter = tokio::spawn(async move {
            acquire_memory_embedding_provider_permit(
                EmbeddingPriority::Write,
                Duration::from_secs(2),
                Some(MemoryEmbeddingAdmissionProbe {
                    acquired: signal,
                    release: release_wait,
                }),
            )
            .await
        });
        tokio::task::yield_now().await;

        assert!(matches!(
            admitted.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        drop(held);

        timeout(Duration::from_secs(2), &mut admitted)
            .await
            .expect("probe admission signal should arrive")
            .expect("probe admission sender should remain live");
        let foreground =
            tokio::spawn(async { acquire_embedding_permit(EmbeddingPriority::Read).await });
        tokio::task::yield_now().await;
        assert!(
            !waiter.is_finished(),
            "probe must retain its provider permit until explicitly released"
        );
        assert!(
            !foreground.is_finished(),
            "foreground must remain queued while the probe gate holds Write admission"
        );
        release
            .send(())
            .expect("probe release receiver should remain live");
        let permit = waiter
            .await
            .expect("probe admission waiter should not panic")
            .expect("probe admission should succeed");
        assert!(
            !foreground.is_finished(),
            "the returned Write permit remains held until its provider request finishes"
        );
        drop(permit);
        let foreground_permit = timeout(Duration::from_secs(2), foreground)
            .await
            .expect("foreground should be admitted after Write releases")
            .expect("foreground waiter should not panic");
        drop(foreground_permit);
    }

    #[tokio::test]
    async fn dropped_write_probe_release_exits_gate_cleanly() {
        let _scheduler_guard = crate::embedding_scheduler::EMBEDDING_ADMISSION_TEST_LOCK
            .lock()
            .await;
        crate::embedding_scheduler::install_embedding_admission_capacity(1);
        let (signal, admitted) = oneshot::channel();
        let (release, release_wait) = oneshot::channel::<()>();
        let waiter = tokio::spawn(async move {
            acquire_memory_embedding_provider_permit(
                EmbeddingPriority::Write,
                Duration::from_secs(2),
                Some(MemoryEmbeddingAdmissionProbe {
                    acquired: signal,
                    release: release_wait,
                }),
            )
            .await
        });

        admitted.await.expect("probe should report admission");
        drop(release);
        let permit = waiter
            .await
            .expect("probe waiter should not panic")
            .expect("a dropped release sender should open the probe gate");
        drop(permit);
    }

    #[test]
    fn ollama_input_split_fails_when_limit_cannot_hold_one_unicode_scalar() {
        let error = split_ollama_embedding_input("🚀", 2).unwrap_err();
        assert!(error
            .to_string()
            .contains("cannot hold the UTF-8 character"));
    }

    #[test]
    fn ollama_logical_plan_tracks_every_segment_owner() {
        let texts = vec!["abcdefghij".to_string(), "xy".to_string()];
        let plan = plan_ollama_logical_embedding_inputs(&texts, 4).unwrap();

        assert_eq!(plan.physical_inputs.concat(), "abcdefghijxy");
        assert_eq!(plan.physical_owner_indexes, vec![0, 0, 0, 1]);
        assert_eq!(plan.logical_input_count, 2);
        assert!(plan.physical_inputs.iter().all(|input| input.len() <= 4));
    }

    #[test]
    fn ollama_logical_embedding_uses_all_segments_and_preserves_single_segment() {
        let plan = OllamaLogicalEmbeddingPlan {
            physical_inputs: vec!["aaaa".into(), "bbbb".into(), "third".into()],
            physical_owner_indexes: vec![0, 0, 1],
            logical_input_count: 2,
        };
        let physical = vec![vec![1.0, 0.0], vec![0.0, 1.0], vec![0.25, 0.75]];

        let logical = aggregate_ollama_logical_embeddings(&plan, &physical, 2).unwrap();

        let expected = 1.0f32 / 2.0f32.sqrt();
        assert!((logical[0][0] - expected).abs() < 1e-6);
        assert!((logical[0][1] - expected).abs() < 1e-6);
        assert_eq!(logical[1], vec![0.25, 0.75]);
    }

    #[test]
    fn ollama_logical_embedding_length_weights_a_tiny_tail() {
        let plan = OllamaLogicalEmbeddingPlan {
            physical_inputs: vec!["abcdefgh".into(), "x".into()],
            physical_owner_indexes: vec![0, 0],
            logical_input_count: 1,
        };
        let physical = vec![vec![1.0, 0.0], vec![0.0, 1.0]];

        let logical = aggregate_ollama_logical_embeddings(&plan, &physical, 2).unwrap();

        assert!(logical[0][0] > logical[0][1] * 7.9);
        assert!((logical[0][0] * logical[0][0] + logical[0][1] * logical[0][1] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn ollama_logical_embedding_normalizes_each_segment_before_pooling() {
        let plan = OllamaLogicalEmbeddingPlan {
            physical_inputs: vec!["aaaa".into(), "bbbb".into()],
            physical_owner_indexes: vec![0, 0],
            logical_input_count: 1,
        };
        let physical = vec![vec![100.0, 0.0], vec![0.0, 1.0]];

        let logical = aggregate_ollama_logical_embeddings(&plan, &physical, 2).unwrap();

        let expected = 1.0f32 / 2.0f32.sqrt();
        assert!((logical[0][0] - expected).abs() < 1e-6);
        assert!((logical[0][1] - expected).abs() < 1e-6);
    }

    #[test]
    fn ollama_embedding_validation_fails_closed_on_count_shape_and_values() {
        assert!(validate_ollama_embedding_vectors(&[vec![1.0, 0.0]], 2, 2).is_err());
        assert!(validate_ollama_embedding_vectors(&[vec![1.0]], 1, 2).is_err());
        assert!(validate_ollama_embedding_vectors(&[vec![f32::NAN, 0.0]], 1, 2).is_err());
        assert!(validate_ollama_embedding_vectors(&[vec![0.0, 0.0]], 1, 2)
            .unwrap_err()
            .to_string()
            .contains("zero or invalid norm"));
        assert!(
            validate_ollama_embedding_vectors(&[vec![f32::MAX, f32::MAX]], 1, 2,)
                .unwrap_err()
                .to_string()
                .contains("zero or invalid norm")
        );
        assert!(validate_ollama_embedding_vectors(&[vec![1.0, 0.0]], 1, 2).is_ok());
    }

    #[test]
    fn single_segment_logical_embedding_rejects_zero_norm_provider_vector() {
        let plan = OllamaLogicalEmbeddingPlan {
            physical_inputs: vec!["one complete input".into()],
            physical_owner_indexes: vec![0],
            logical_input_count: 1,
        };

        assert!(
            aggregate_ollama_logical_embeddings(&plan, &[vec![0.0, 0.0]], 2)
                .unwrap_err()
                .to_string()
                .contains("zero or invalid norm")
        );
    }

    #[test]
    fn memory_query_embedding_rejects_zero_norm_provider_vector() {
        let error = validate_ollama_query_embedding(&[0.0, 0.0], 2).unwrap_err();
        assert!(error
            .chain()
            .any(|cause| cause.to_string().contains("zero or invalid norm")));
        validate_ollama_query_embedding(&[0.6, 0.8], 2).unwrap();
    }

    #[tokio::test]
    async fn legacy_cache_rejects_and_removes_invalid_norm_vectors() {
        let temp = tempfile::tempdir().unwrap();
        for (name, vector) in [
            ("zero", vec![0.0, 0.0]),
            ("non-finite", vec![f32::NAN, 0.0]),
            ("overflow", vec![f32::MAX, f32::MAX]),
        ] {
            let path = temp.path().join(format!("{name}.f32"));
            write_memory_embedding_cache_file(&path, &vector)
                .await
                .unwrap();

            assert!(read_memory_embedding_cache_file(&path, 2)
                .await
                .unwrap()
                .is_none());
            assert!(
                !fs::try_exists(&path).await.unwrap(),
                "invalid legacy {name} cache entry must be removed so it cannot be reused"
            );
        }

        let valid_path = temp.path().join("valid.f32");
        write_memory_embedding_cache_file(&valid_path, &[0.6, 0.8])
            .await
            .unwrap();
        assert_eq!(
            read_memory_embedding_cache_file(&valid_path, 2)
                .await
                .unwrap(),
            Some(vec![0.6, 0.8])
        );
        assert!(fs::try_exists(valid_path).await.unwrap());
    }

    #[test]
    fn retrieval_query_expands_provenance_intent() {
        let expanded = expand_memory_retrieval_query(
            "What have I been working on, and where did that come from?",
        );
        assert!(expanded.contains("source evidence provenance rationale origin"));
    }

    #[test]
    fn retrieval_query_expands_location_intent_without_changing_other_queries() {
        let expanded = expand_memory_retrieval_query(
            "Where do the local SOTA browser test pages usually run?",
        );
        assert!(expanded.contains("url uri host endpoint address port location"));
        assert_eq!(
            expand_memory_retrieval_query("What output format should I use?"),
            "What output format should I use?"
        );
    }

    #[test]
    fn source_hash_changes_are_hard_stale() {
        assert_eq!(
            memory_index_staleness_class("source_hashes_changed"),
            MemoryIndexStalenessClass::Hard
        );
        assert!(!memory_index_stale_reason_is_soft("source_hashes_changed"));
        assert_eq!(
            memory_index_staleness_class("document_count_changed"),
            MemoryIndexStalenessClass::Soft
        );
        assert!(memory_index_stale_reason_is_soft("document_count_changed"));
        assert!(memory_index_stale_reason_is_repairable_lancedb(
            "missing_lancedb_table"
        ));
        assert!(memory_index_stale_reason_is_repairable_lancedb(
            "missing_lancedb_candidate_key_index"
        ));
        assert!(memory_index_stale_reason_is_repairable_lancedb(
            MEMORY_INDEX_STALE_REASON_LANCEDB_HEALTH_CHECK_FAILED
        ));
        assert!(memory_index_stale_reason_is_transient_lancedb(
            MEMORY_INDEX_STALE_REASON_LANCEDB_HEALTH_CHECK_TIMED_OUT
        ));
        assert!(!memory_index_stale_reason_is_soft("missing_lancedb_table"));
    }

    #[test]
    fn journal_keys_distinguish_absent_and_empty_goal_ids() {
        let without_goal = MemoryIndexChange::NativeTier {
            agent_id: "agent:one".to_string(),
            tier_name: "tier:notes".to_string(),
            scope: TierScope::AgentGoal,
            goal_id: None,
        };
        let empty_goal = MemoryIndexChange::NativeTier {
            agent_id: "agent:one".to_string(),
            tier_name: "tier:notes".to_string(),
            scope: TierScope::AgentGoal,
            goal_id: Some(String::new()),
        };

        assert_ne!(
            memory_index_change_key(&without_goal),
            memory_index_change_key(&empty_goal)
        );
    }

    #[test]
    fn candidate_keys_are_unambiguous_for_separator_containing_values() {
        let now = Utc::now();
        let candidate_a = MemoryCandidateDocument {
            principal: Some("anonymous".to_string()),
            workspace: Some("default".to_string()),
            agent_id: None,
            scope: TierScope::User,
            tier_name: "preference:work".to_string(),
            semantic_memory_type: crate::memory_candidates::SemanticMemoryType::UserPreference,
            goal_id: None,
            item_key: "timezone".to_string(),
            source_path: None,
            json_pointer: String::new(),
            content_hash: "a".to_string(),
            last_updated: now,
            confidence: None,
            text: "preference one".to_string(),
            metadata_json: serde_json::json!({}),
        };
        let mut candidate_b = candidate_a.clone();
        candidate_b.tier_name = "preference".to_string();
        candidate_b.item_key = "work:timezone".to_string();

        assert_ne!(
            memory_candidate_index_key(&candidate_a),
            memory_candidate_index_key(&candidate_b)
        );
    }

    #[test]
    fn indexes_custom_user_tiers_once_with_a_deterministic_schema_owner() {
        let custom_for_zulu = MemoryTierDefinition {
            name: "vc_intelligence".to_string(),
            scope: TierScope::User,
            description: "zulu schema".to_string(),
            schema: std::collections::BTreeMap::new(),
            render: crate::memory_tiers::RenderConfig {
                format: "compact_summary".to_string(),
                template: "{profiles}".to_string(),
            },
            retention: crate::memory_tiers::RetentionMode::Forever,
        };
        let mut custom_for_alpha = custom_for_zulu.clone();
        custom_for_alpha.description = "alpha schema".to_string();
        let legacy_contacts = MemoryTierDefinition {
            name: "contacts".to_string(),
            scope: TierScope::User,
            description: "legacy schema".to_string(),
            schema: std::collections::BTreeMap::new(),
            render: crate::memory_tiers::RenderConfig {
                format: "compact_summary".to_string(),
                template: "{entries}".to_string(),
            },
            retention: crate::memory_tiers::RetentionMode::Forever,
        };
        let records = vec![
            MoveableDefinitionRecord {
                agent_id: "zulu".to_string(),
                memory_tiers: vec![custom_for_zulu],
                definition_source_hash: "zulu".to_string(),
            },
            MoveableDefinitionRecord {
                agent_id: "alpha".to_string(),
                memory_tiers: vec![custom_for_alpha, legacy_contacts],
                definition_source_hash: "alpha".to_string(),
            },
        ];

        let user_tiers = user_memory_tiers(&records);
        assert_eq!(user_tiers.len(), 2);
        assert_eq!(user_tiers[1].name, "vc_intelligence");
        assert_eq!(user_tiers[1].description, "alpha schema");

        let indexable = indexable_user_memory_tiers(&records);
        assert_eq!(indexable.len(), 1);
        assert_eq!(indexable[0].name, "vc_intelligence");
    }

    #[test]
    fn incremental_manifest_refresh_replaces_every_candidate_hash() {
        let candidate = MemoryCandidateDocument {
            principal: Some("anonymous".to_string()),
            workspace: Some("default".to_string()),
            agent_id: None,
            scope: TierScope::User,
            tier_name: "preferences".to_string(),
            semantic_memory_type: crate::memory_candidates::SemanticMemoryType::UserPreference,
            goal_id: None,
            item_key: "timezone".to_string(),
            source_path: None,
            json_pointer: "/timezone".to_string(),
            content_hash: "fresh-content".to_string(),
            last_updated: Utc::now(),
            confidence: None,
            text: "timezone: Asia/Kolkata".to_string(),
            metadata_json: serde_json::json!({}),
        };
        let mut hashes = BTreeMap::from([
            (
                "definition:assistant".to_string(),
                "definition-hash".to_string(),
            ),
            (
                "candidate:obsolete".to_string(),
                "obsolete-hash".to_string(),
            ),
        ]);

        replace_manifest_candidate_source_hashes(&mut hashes, &[candidate.clone()]).unwrap();

        assert_eq!(
            hashes.get("definition:assistant"),
            Some(&"definition-hash".to_string())
        );
        assert!(!hashes.contains_key("candidate:obsolete"));
        assert!(hashes.contains_key(&format!(
            "candidate:{}",
            memory_candidate_index_key(&candidate)
        )));
    }

    #[test]
    fn lancedb_index_health_requires_expected_index_type() {
        let indexes = vec![IndexConfig {
            name: "wrong_index".to_string(),
            index_type: IndexType::BTree,
            columns: vec![MEMORY_LANCEDB_FTS_COLUMN.to_string()],
        }];
        assert!(!has_lancedb_single_column_index(
            &indexes,
            MEMORY_LANCEDB_FTS_COLUMN,
            IndexType::FTS
        ));
        assert!(has_lancedb_single_column_index(
            &indexes,
            MEMORY_LANCEDB_FTS_COLUMN,
            IndexType::BTree
        ));
    }

    #[test]
    fn stable_source_hash_ignores_volatile_candidate_timestamps() {
        let now = Utc::now();
        let candidate = MemoryCandidateDocument {
            principal: Some("anonymous".to_string()),
            workspace: Some("default".to_string()),
            agent_id: None,
            scope: TierScope::User,
            tier_name: "knowledge.warehouse".to_string(),
            semantic_memory_type: crate::memory_candidates::SemanticMemoryType::UserPreference,
            goal_id: None,
            item_key: "warehouse".to_string(),
            source_path: Some(std::path::PathBuf::from("user_knowledge.json")),
            json_pointer: "/fields/warehouse".to_string(),
            content_hash: "content-hash".to_string(),
            last_updated: now,
            confidence: None,
            text: "warehouse: Metabase analytics use production DuckDB".to_string(),
            metadata_json: serde_json::json!({
                "candidate_kind": "user_field",
                "field": "warehouse",
                "semantic_memory_type": "user_preference",
            }),
        };
        let mut reloaded = candidate.clone();
        reloaded.last_updated = now + chrono::Duration::seconds(30);

        assert_eq!(
            stable_memory_candidate_source_hash(&candidate).unwrap(),
            stable_memory_candidate_source_hash(&reloaded).unwrap()
        );
    }

    #[test]
    fn incremental_lancedb_plan_embeds_only_changed_or_missing_chunks() {
        let chunks = vec![
            MemoryIndexChunkDocument {
                parent_candidate_index: 0,
                parent_candidate_key: "candidate-a".to_string(),
                chunk_key: "chunk-a".to_string(),
                chunk_index: 0,
                char_start: 0,
                char_end: 1,
                chunk_text: "a".to_string(),
                search_text: "a".to_string(),
            },
            MemoryIndexChunkDocument {
                parent_candidate_index: 1,
                parent_candidate_key: "candidate-b".to_string(),
                chunk_key: "chunk-b".to_string(),
                chunk_index: 0,
                char_start: 0,
                char_end: 1,
                chunk_text: "b".to_string(),
                search_text: "b".to_string(),
            },
            MemoryIndexChunkDocument {
                parent_candidate_index: 2,
                parent_candidate_key: "candidate-c".to_string(),
                chunk_key: "chunk-c".to_string(),
                chunk_index: 0,
                char_start: 0,
                char_end: 1,
                chunk_text: "c".to_string(),
                search_text: "c".to_string(),
            },
        ];
        let fingerprints = vec![
            MemoryIndexChunkFingerprint {
                row_hash: "row-a".to_string(),
                embedding_input_hash: "input-a".to_string(),
            },
            MemoryIndexChunkFingerprint {
                row_hash: "row-b-new".to_string(),
                embedding_input_hash: "input-b".to_string(),
            },
            MemoryIndexChunkFingerprint {
                row_hash: "row-c".to_string(),
                embedding_input_hash: "input-c".to_string(),
            },
        ];
        let existing = BTreeMap::from([
            (
                "chunk-a".to_string(),
                MemoryIndexChunkFingerprint {
                    row_hash: "row-a".to_string(),
                    embedding_input_hash: "input-a".to_string(),
                },
            ),
            (
                "chunk-b".to_string(),
                MemoryIndexChunkFingerprint {
                    row_hash: "row-b-old".to_string(),
                    embedding_input_hash: "input-b".to_string(),
                },
            ),
            (
                "removed-chunk".to_string(),
                MemoryIndexChunkFingerprint {
                    row_hash: "removed-row".to_string(),
                    embedding_input_hash: "removed-input".to_string(),
                },
            ),
        ]);

        let plan = plan_incremental_lancedb_write(&chunks, &fingerprints, &existing).unwrap();

        assert_eq!(plan.unchanged_rows, 1);
        assert_eq!(plan.changed_chunk_indexes, vec![1, 2]);
        assert_eq!(plan.deleted_chunk_keys, vec!["removed-chunk"]);
    }

    #[test]
    fn incremental_lancedb_plan_treats_embedding_input_change_as_changed() {
        let chunks = vec![MemoryIndexChunkDocument {
            parent_candidate_index: 0,
            parent_candidate_key: "candidate".to_string(),
            chunk_key: "chunk".to_string(),
            chunk_index: 0,
            char_start: 0,
            char_end: 1,
            chunk_text: "text".to_string(),
            search_text: "text".to_string(),
        }];
        let fingerprints = vec![MemoryIndexChunkFingerprint {
            row_hash: "same-row-hash".to_string(),
            embedding_input_hash: "new-input-hash".to_string(),
        }];
        let existing = BTreeMap::from([(
            "chunk".to_string(),
            MemoryIndexChunkFingerprint {
                row_hash: "same-row-hash".to_string(),
                embedding_input_hash: "old-input-hash".to_string(),
            },
        )]);

        let plan = plan_incremental_lancedb_write(&chunks, &fingerprints, &existing).unwrap();

        assert_eq!(plan.unchanged_rows, 0);
        assert_eq!(plan.changed_chunk_indexes, vec![0]);
        assert!(plan.deleted_chunk_keys.is_empty());
    }

    struct EmptyDefinitions;

    #[async_trait::async_trait]
    impl DefinitionLookup for EmptyDefinitions {
        async fn list_moveable_definitions(
            &self,
        ) -> Result<Vec<MoveableDefinitionRecord>, anyhow::Error> {
            Ok(Vec::new())
        }
    }

    struct FileMemoryStorage {
        root: PathBuf,
    }

    impl FileMemoryStorage {
        fn new(root: PathBuf) -> Self {
            Self { root }
        }
    }

    #[async_trait::async_trait]
    impl MemoryStorage for FileMemoryStorage {
        fn root(&self) -> &Path {
            &self.root
        }

        fn scope_segments(&self) -> Option<(String, String)> {
            Some(("anonymous".to_string(), "default".to_string()))
        }

        fn agent_tiers_dir(&self, agent_id: &str) -> Result<PathBuf, MemoryStorageError> {
            Ok(self.root.join("agents").join(sanitize_segment(agent_id)))
        }

        fn agent_episodes_dir(&self, agent_id: &str) -> Result<PathBuf, MemoryStorageError> {
            Ok(self
                .root
                .join("agents")
                .join(sanitize_segment(agent_id))
                .join("episodes"))
        }

        fn agent_tier_path(
            &self,
            agent_id: &str,
            tier_name: &str,
            _scope: &TierScope,
            goal_id: Option<&str>,
        ) -> Result<PathBuf, MemoryStorageError> {
            Ok(self.agent_tiers_dir(agent_id)?.join(format!(
                "{}_{}.json",
                tier_name,
                goal_id.unwrap_or("none")
            )))
        }

        fn memory_lancedb_index_dir(&self) -> PathBuf {
            self.root.join("index").join("lancedb")
        }

        fn memory_index_documents_path(&self) -> PathBuf {
            self.root.join("index").join("documents.jsonl")
        }

        fn memory_index_manifest_path(&self) -> PathBuf {
            self.root.join("index").join("manifest.json")
        }

        fn user_knowledge_path(&self) -> PathBuf {
            self.root.join("users").join("knowledge.json")
        }

        fn user_root(&self) -> PathBuf {
            self.root.join("users")
        }

        async fn load_user_knowledge(&self) -> Result<serde_json::Value, MemoryStorageError> {
            Ok(serde_json::json!({}))
        }

        async fn load_native_tier_value(
            &self,
            _agent_id: &str,
            _tier_name: &str,
            _scope: &TierScope,
            _goal_id: Option<&str>,
        ) -> Result<Option<serde_json::Value>, MemoryStorageError> {
            Ok(None)
        }

        async fn read_json_value(
            &self,
            path: &Path,
        ) -> Result<serde_json::Value, MemoryStorageError> {
            let bytes = tokio::fs::read(path).await?;
            Ok(serde_json::from_slice(&bytes)?)
        }

        async fn write_bytes_atomic(
            &self,
            path: &Path,
            bytes: &[u8],
        ) -> Result<(), MemoryStorageError> {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            let tmp = path.with_extension("tmp");
            tokio::fs::write(&tmp, bytes).await?;
            tokio::fs::rename(&tmp, path).await?;
            Ok(())
        }

        async fn write_json_value_atomic(
            &self,
            path: &Path,
            value: &serde_json::Value,
        ) -> Result<(), MemoryStorageError> {
            let bytes = serde_json::to_vec_pretty(value)?;
            self.write_bytes_atomic(path, &bytes).await
        }
    }

    static READ_SNAPSHOT_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[tokio::test]
    async fn request_path_reuses_published_journal_snapshot_without_a_second_lock() {
        let _env = READ_SNAPSHOT_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reset_memory_index_read_snapshots_for_tests();
        let temp = tempfile::tempdir().expect("tempdir");
        let storage = FileMemoryStorage::new(temp.path().to_path_buf());
        record_memory_index_change(&storage, MemoryIndexChange::UserKnowledge)
            .await
            .expect("record");
        let after_write = crate::hol_stats::retrieval_hol_snapshot();
        let first = retrieval_journal_view(&storage)
            .await
            .expect("first retrieval snapshot");
        assert!(!first.pending().is_empty());
        let second = retrieval_journal_view(&storage)
            .await
            .expect("cached retrieval snapshot");
        assert!(!second.pending().is_empty());
        assert_eq!(first.epoch, second.epoch);
        let after_reads = crate::hol_stats::retrieval_hol_snapshot();
        assert!(
            after_reads.read_snapshot_hits > after_write.read_snapshot_hits,
            "second retrieval should hit the in-memory snapshot"
        );
    }

    #[tokio::test]
    async fn pass_through_read_snapshot_still_returns_pending_changes() {
        reset_memory_index_read_snapshots_for_tests();
        let _env = READ_SNAPSHOT_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _guard = EnvVarGuard::set("MAGICIAN_MEMORY_INDEX_READ_SNAPSHOT", "pass_through");
        let temp = tempfile::tempdir().expect("tempdir");
        let storage = FileMemoryStorage::new(temp.path().to_path_buf());
        record_memory_index_change(&storage, MemoryIndexChange::UserKnowledge)
            .await
            .expect("record");
        let view = retrieval_journal_view(&storage)
            .await
            .expect("pass-through snapshot");
        assert!(!view.pending().is_empty());
    }

    #[tokio::test]
    async fn locked_hydrate_pairs_pending_with_file_identity() {
        let _env = READ_SNAPSHOT_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reset_memory_index_read_snapshots_for_tests();
        let temp = tempfile::tempdir().expect("tempdir");
        let storage = FileMemoryStorage::new(temp.path().to_path_buf());
        record_memory_index_change(&storage, MemoryIndexChange::UserKnowledge)
            .await
            .expect("record");
        let published = retrieval_journal_view(&storage)
            .await
            .expect("published snapshot");
        let identity = journal_file_identity(&storage).await;
        assert_eq!(published._journal_identity, identity);
        assert!(!published.pending().is_empty());
    }

    #[tokio::test]
    async fn pending_identity_token_is_stable_until_the_journal_changes() {
        let _env = READ_SNAPSHOT_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        reset_memory_index_read_snapshots_for_tests();
        let temp = tempfile::tempdir().expect("tempdir");
        let storage = FileMemoryStorage::new(temp.path().to_path_buf());
        record_memory_index_change(&storage, MemoryIndexChange::UserKnowledge)
            .await
            .expect("record");
        let first = retrieval_journal_view(&storage)
            .await
            .expect("first snapshot");
        let second = retrieval_journal_view(&storage)
            .await
            .expect("second snapshot");
        assert_ne!(first.pending().identity_token(), "empty");
        assert_eq!(
            first.pending().identity_token(),
            second.pending().identity_token()
        );
        record_memory_index_change(
            &storage,
            MemoryIndexChange::FullScope {
                reason: "external".to_string(),
            },
        )
        .await
        .expect("second record");
        let third = retrieval_journal_view(&storage)
            .await
            .expect("third snapshot");
        assert_ne!(
            first.pending().identity_token(),
            third.pending().identity_token()
        );
    }

    #[test]
    fn lancedb_search_table_name_matches_pool_contract() {
        assert_eq!(
            MEMORY_LANCEDB_TABLE,
            crate::lance_table_pool::SEARCH_TABLE_NAME
        );
    }

    #[test]
    fn shadow_observe_budget_caps_extra_wait() {
        let started = Instant::now();
        let budget = Duration::from_millis(750);
        let observe = shadow_observe_budget(started, budget).expect("fresh budget");
        assert!(observe <= MAX_SHADOW_OBSERVE);
        assert!(observe > Duration::from_millis(25));
    }

    #[test]
    fn ann_ivf_budget_leaves_hybrid_slack() {
        let started = Instant::now();
        let budget = Duration::from_millis(750);
        let ivf = ann_ivf_budget(started, budget).expect("fresh budget");
        assert!(ivf + ANN_HYBRID_SLACK <= budget);
        assert!(ivf > Duration::from_millis(25));
    }

    #[test]
    fn exhausted_search_budget_skips_shadow_and_ivf() {
        let started = Instant::now()
            .checked_sub(Duration::from_secs(2))
            .expect("clock");
        let budget = Duration::from_millis(750);
        assert!(shadow_observe_budget(started, budget).is_none());
        assert!(ann_ivf_budget(started, budget).is_none());
        assert!(remaining_search_budget(started, budget).is_none());
    }

    #[test]
    fn index_generation_token_changes_with_source_hashes_and_rebuild_time() {
        let embedding = MemoryEmbeddingManifest {
            provider: "ollama".to_string(),
            model: Some("model".to_string()),
            dimensions: 8,
            contract_id: "contract".to_string(),
            fallback_reason: None,
        };
        let mut left = test_index_manifest(&embedding);
        left.source_hashes
            .insert("candidate:a".to_string(), "hash-a".to_string());
        let mut right = left.clone();
        assert_eq!(
            memory_index_generation_token(&left),
            memory_index_generation_token(&right)
        );
        right
            .source_hashes
            .insert("candidate:a".to_string(), "hash-b".to_string());
        assert_ne!(
            memory_index_generation_token(&left),
            memory_index_generation_token(&right)
        );
        right = left.clone();
        right.rebuilt_at = left.rebuilt_at + chrono::Duration::seconds(1);
        assert_ne!(
            memory_index_generation_token(&left),
            memory_index_generation_token(&right)
        );
    }

    #[tokio::test]
    async fn hybrid_result_cache_skips_missing_index_and_stays_off_by_default() {
        let _env = READ_SNAPSHOT_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _cache = crate::HYBRID_RESULT_CACHE_TEST_LOCK.lock().await;
        crate::reset_hybrid_result_cache_for_tests();
        reset_memory_index_read_snapshots_for_tests();
        let temp = tempfile::tempdir().expect("tempdir");
        let storage = FileMemoryStorage::new(temp.path().to_path_buf());
        let definitions = EmptyDefinitions;
        let disabled = score_fresh_memory_hybrid_index_with_status(&storage, &definitions, "query")
            .await
            .expect("disabled path");
        assert!(disabled.scores.is_none());
        assert!(!crate::hybrid_result_cache_enabled());

        crate::install_hybrid_result_cache(crate::HybridResultCacheSettings {
            enabled: true,
            ..crate::HybridResultCacheSettings::default()
        });
        let _guard = EnvVarGuard::set("MAGICIAN_MEMORY_HYBRID_RESULT_CACHE", "on");
        let enabled = score_fresh_memory_hybrid_index_with_status(&storage, &definitions, "query")
            .await
            .expect("enabled miss");
        assert!(enabled.scores.is_none());
        assert!(enabled.fallback_reason.is_some());
        assert!(crate::published_hybrid_index_generation(storage.root()).is_none());
        crate::reset_hybrid_result_cache_for_tests();
    }

    async fn vector_search_fixture() -> (tempfile::TempDir, Table, Vec<MemoryCandidateDocument>) {
        let tmp = tempfile::tempdir().unwrap();
        let index_dir = tmp.path().join("lancedb");
        let mut alpha = test_candidate("alpha", "alpha vector");
        alpha.agent_id = Some("keeper".to_string());
        let mut beta = test_candidate("beta", "beta vector");
        beta.agent_id = Some("other".to_string());
        let mut gamma = test_candidate("gamma", "gamma vector");
        gamma.agent_id = Some("keeper".to_string());
        let mut candidates = vec![alpha, beta, gamma];
        // PQ train needs ≥16 rows; extras sit on an unused axis so nearest-3
        // ranking of alpha/beta/gamma stays the same.
        for index in 0..13 {
            let mut extra = test_candidate(&format!("pad{index}"), "pad vector");
            extra.agent_id = Some("pad".to_string());
            candidates.push(extra);
        }
        let chunks = build_memory_index_chunks(&candidates);
        let mut embeddings = vec![
            vec![1.0_f32, 0.0, 0.0, 0.0],
            vec![0.0_f32, 1.0, 0.0, 0.0],
            vec![0.0_f32, 0.0, 1.0, 0.0],
        ];
        embeddings.extend(std::iter::repeat(vec![0.0_f32, 0.0, 0.0, 1.0]).take(13));
        assert_eq!(chunks.len(), embeddings.len());
        let record_batch = lancedb_record_batch(&candidates, &chunks, &embeddings, 4).unwrap();
        replace_lancedb_index_dir(&index_dir, record_batch)
            .await
            .unwrap();
        let table = open_existing_lancedb_memory_table(&index_dir)
            .await
            .unwrap()
            .expect("table");
        (tmp, table, candidates)
    }

    #[tokio::test]
    async fn vector_search_flat_ranks_nearest_candidate() {
        let _lock = crate::VECTOR_SEARCH_TEST_LOCK.lock().await;
        crate::reset_vector_search_for_tests();
        let (_tmp, table, candidates) = vector_search_fixture().await;
        let keys = search_lancedb_vector_on_table(&table, &[1.0, 0.0, 0.0, 0.0], 3, None)
            .await
            .unwrap();
        assert_eq!(
            keys.first().map(String::as_str),
            Some(memory_candidate_index_key(&candidates[0]).as_str())
        );
        crate::reset_vector_search_for_tests();
    }

    #[tokio::test]
    async fn vector_search_shadow_serves_the_same_keys_as_flat() {
        let _lock = crate::VECTOR_SEARCH_TEST_LOCK.lock().await;
        crate::reset_vector_search_for_tests();
        let (_tmp, table, _) = vector_search_fixture().await;
        let query = [1.0_f32, 0.0, 0.0, 0.0];
        let flat = search_lancedb_vector_on_table(&table, &query, 3, None)
            .await
            .unwrap();
        crate::install_vector_search(crate::VectorSearchSettings {
            mode: crate::VectorSearchMode::AnnShadow,
            min_rows: 256,
            candidate_multiplier: 4,
            nprobes: 20,
        });
        let shadow = search_lancedb_vector_on_table(&table, &query, 3, None)
            .await
            .unwrap();
        assert_eq!(shadow, flat);
        crate::reset_vector_search_for_tests();
    }

    #[tokio::test]
    async fn vector_search_ann_falls_back_without_ivf() {
        let _lock = crate::VECTOR_SEARCH_TEST_LOCK.lock().await;
        crate::reset_vector_search_for_tests();
        let (_tmp, table, _) = vector_search_fixture().await;
        let query = [1.0_f32, 0.0, 0.0, 0.0];
        let flat = search_lancedb_vector_on_table(&table, &query, 3, None)
            .await
            .unwrap();
        crate::install_vector_search(crate::VectorSearchSettings {
            mode: crate::VectorSearchMode::Ann,
            min_rows: 256,
            candidate_multiplier: 4,
            nprobes: 20,
        });
        let indexes = table.list_indices().await.unwrap();
        assert!(!has_lancedb_single_column_index(
            &indexes,
            MEMORY_LANCEDB_VECTOR_COLUMN,
            IndexType::IvfPq
        ));
        let ann = search_lancedb_vector_on_table(&table, &query, 3, None)
            .await
            .unwrap();
        assert_eq!(ann, flat);
        crate::reset_vector_search_for_tests();
    }

    #[tokio::test]
    async fn vector_search_predicate_still_applied() {
        let _lock = crate::VECTOR_SEARCH_TEST_LOCK.lock().await;
        crate::reset_vector_search_for_tests();
        let (_tmp, table, candidates) = vector_search_fixture().await;
        let keys = search_lancedb_vector_on_table(
            &table,
            &[0.0, 1.0, 0.0, 0.0],
            8,
            Some("agent_id = 'keeper'"),
        )
        .await
        .unwrap();
        let allowed = [
            memory_candidate_index_key(&candidates[0]),
            memory_candidate_index_key(&candidates[2]),
        ];
        assert!(!keys.is_empty());
        assert!(keys.iter().all(|key| allowed.iter().any(|ok| ok == key)));
        assert!(!keys.contains(&memory_candidate_index_key(&candidates[1])));
        crate::reset_vector_search_for_tests();
    }

    #[tokio::test]
    async fn vector_search_min_rows_skips_ivf_create() {
        let _lock = crate::VECTOR_SEARCH_TEST_LOCK.lock().await;
        crate::reset_vector_search_for_tests();
        crate::install_vector_search(crate::VectorSearchSettings {
            mode: crate::VectorSearchMode::Ann,
            min_rows: 256,
            candidate_multiplier: 4,
            nprobes: 20,
        });
        let tmp = tempfile::tempdir().unwrap();
        let index_dir = tmp.path().join("lancedb");
        let (_keep, table, _) = vector_search_fixture().await;
        let before = table.list_indices().await.unwrap();
        assert!(!has_lancedb_single_column_index(
            &before,
            MEMORY_LANCEDB_VECTOR_COLUMN,
            IndexType::IvfPq
        ));
        ensure_lancedb_memory_indexes(&table, &index_dir)
            .await
            .unwrap();
        let after = table.list_indices().await.unwrap();
        assert!(!has_lancedb_single_column_index(
            &after,
            MEMORY_LANCEDB_VECTOR_COLUMN,
            IndexType::IvfPq
        ));
        crate::reset_vector_search_for_tests();
    }

    async fn create_tiny_ivf_pq(table: &Table) {
        table
            .create_index(
                &[MEMORY_LANCEDB_VECTOR_COLUMN],
                Index::IvfPq(
                    IvfPqIndexBuilder::default()
                        .num_partitions(1)
                        .sample_rate(2)
                        .num_sub_vectors(2)
                        .num_bits(4)
                        .max_iterations(4),
                ),
            )
            .replace(true)
            .execute()
            .await
            .expect("tiny IVF_PQ for vector-search tests");
        let indexes = table.list_indices().await.unwrap();
        assert!(
            has_lancedb_single_column_index(
                &indexes,
                MEMORY_LANCEDB_VECTOR_COLUMN,
                IndexType::IvfPq
            ),
            "IVF_PQ must exist after create_tiny_ivf_pq"
        );
    }

    #[tokio::test]
    async fn vector_search_flat_bypasses_leftover_ivf() {
        let _lock = crate::VECTOR_SEARCH_TEST_LOCK.lock().await;
        crate::reset_vector_search_for_tests();
        let (_tmp, table, candidates) = vector_search_fixture().await;
        create_tiny_ivf_pq(&table).await;
        let keys = search_lancedb_vector_on_table(&table, &[1.0, 0.0, 0.0, 0.0], 3, None)
            .await
            .unwrap();
        assert_eq!(
            keys.first().map(String::as_str),
            Some(memory_candidate_index_key(&candidates[0]).as_str())
        );
        crate::reset_vector_search_for_tests();
    }

    #[tokio::test]
    async fn vector_search_shadow_with_ivf_still_serves_flat_keys() {
        let _lock = crate::VECTOR_SEARCH_TEST_LOCK.lock().await;
        crate::reset_vector_search_for_tests();
        let (_tmp, table, _) = vector_search_fixture().await;
        create_tiny_ivf_pq(&table).await;
        let query = [1.0_f32, 0.0, 0.0, 0.0];
        let flat = search_lancedb_vector_on_table(&table, &query, 3, None)
            .await
            .unwrap();
        crate::install_vector_search(crate::VectorSearchSettings {
            mode: crate::VectorSearchMode::AnnShadow,
            min_rows: 1,
            candidate_multiplier: 4,
            nprobes: 1,
        });
        let before = crate::retrieval_hol_snapshot();
        let shadow = search_lancedb_vector_on_table(&table, &query, 3, None)
            .await
            .unwrap();
        let after = crate::retrieval_hol_snapshot();
        assert_eq!(shadow, flat);
        assert!(
            after.ann_queries > before.ann_queries,
            "shadow with IVF must run the ANN compare path"
        );
        crate::reset_vector_search_for_tests();
    }

    #[tokio::test]
    async fn vector_search_ann_with_ivf_returns_nearest_after_rerank() {
        let _lock = crate::VECTOR_SEARCH_TEST_LOCK.lock().await;
        crate::reset_vector_search_for_tests();
        let (_tmp, table, candidates) = vector_search_fixture().await;
        create_tiny_ivf_pq(&table).await;
        crate::install_vector_search(crate::VectorSearchSettings {
            mode: crate::VectorSearchMode::Ann,
            min_rows: 1,
            candidate_multiplier: 4,
            nprobes: 1,
        });
        let before = crate::retrieval_hol_snapshot();
        let keys = search_lancedb_vector_on_table(&table, &[1.0, 0.0, 0.0, 0.0], 3, None)
            .await
            .unwrap();
        let after = crate::retrieval_hol_snapshot();
        assert_eq!(
            keys.first().map(String::as_str),
            Some(memory_candidate_index_key(&candidates[0]).as_str())
        );
        assert!(
            after.ann_queries > before.ann_queries,
            "ann mode with IVF must use the IVF path"
        );
        assert_eq!(
            after.ann_fallbacks, before.ann_fallbacks,
            "ann mode with IVF must not silently fall back to flat"
        );
        crate::reset_vector_search_for_tests();
    }

    #[tokio::test]
    async fn vector_search_ann_predicate_with_ivf() {
        let _lock = crate::VECTOR_SEARCH_TEST_LOCK.lock().await;
        crate::reset_vector_search_for_tests();
        let (_tmp, table, candidates) = vector_search_fixture().await;
        create_tiny_ivf_pq(&table).await;
        crate::install_vector_search(crate::VectorSearchSettings {
            mode: crate::VectorSearchMode::Ann,
            min_rows: 1,
            candidate_multiplier: 4,
            nprobes: 1,
        });
        let keys = search_lancedb_vector_on_table(
            &table,
            &[0.0, 1.0, 0.0, 0.0],
            8,
            Some("agent_id = 'keeper'"),
        )
        .await
        .unwrap();
        let allowed = [
            memory_candidate_index_key(&candidates[0]),
            memory_candidate_index_key(&candidates[2]),
        ];
        assert!(!keys.is_empty());
        assert!(keys.iter().all(|key| allowed.iter().any(|ok| ok == key)));
        assert!(!keys.contains(&memory_candidate_index_key(&candidates[1])));
        crate::reset_vector_search_for_tests();
    }

    #[test]
    fn ann_fail_closed_scores_are_not_cacheable() {
        let result = MemoryHybridIndexScoreResult {
            scores: Some(BTreeMap::new()),
            fallback_reason: None,
            stale: false,
            current_document_count: 1,
            served_ann_fallback: true,
        };
        assert!(!CachedHybridScore::from(result).cacheable());
        let retained = MemoryHybridIndexScoreResult {
            scores: Some(BTreeMap::new()),
            fallback_reason: None,
            stale: false,
            current_document_count: 1,
            served_ann_fallback: false,
        };
        assert!(CachedHybridScore::from(retained).cacheable());
    }

    #[test]
    fn vector_search_candidate_key_in_predicate_quotes_and_ands() {
        let keys = vec!["a'b".to_string(), "a'b".to_string(), "c".to_string()];
        let pred = candidate_key_in_predicate(&keys, Some("agent_id = 'keeper'"));
        assert!(pred.starts_with("(agent_id = 'keeper') AND "));
        assert!(pred.contains("'a''b'"));
        assert!(pred.contains("'c'"));
        assert!(!pred.contains("IN ()"));
        let only = candidate_key_in_predicate(&["x".to_string()], None);
        assert_eq!(only, "candidate_key IN ('x')");
    }

    #[test]
    fn vector_search_recall_helper_matches_intersection_over_k() {
        let flat = vec!["a".into(), "b".into(), "c".into()];
        let ann = vec!["a".into(), "x".into(), "c".into()];
        let recall = crate::recall_at_k(&flat, &ann, 3);
        assert_eq!(recall.overlap, 2);
        assert_eq!(recall.milles, 666);
        assert!(recall.mismatch);
    }
}
