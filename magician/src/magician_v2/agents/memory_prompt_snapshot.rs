//! Bounded immutable prompt-retrieval snapshots.
//!
//! Canonical memory files and the durable memory-index mutation journal remain
//! authoritative. This module caches prompt-ready candidate documents plus the
//! exact lexical and duplicate-detection features otherwise rebuilt on every
//! request. File stamps validate cross-process freshness; in-process journal
//! notifications eagerly refresh warm entries after canonical commits.

use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex as StdMutex, OnceLock, RwLock,
    },
    time::{Duration, Instant},
};

use magician_vector_index::{
    memory_candidates::{
        load_memory_candidate_documents, load_memory_candidate_documents_with_prepared,
        MemoryCandidateDocument, MemoryCandidateRequest, PreparedCandidateSources,
    },
    memory_index::{subscribe_memory_index_changes, MemoryIndexChange},
    memory_tiers::{MemoryTierDefinition, TierScope},
    retrieval_scope::RetrievalScope,
    storage_trait::MemoryStorageError,
};
use tracing::warn;

use crate::config::MagicianMemoryPromptSnapshotSettings;
use crate::magician_v2::json_traversal::json_encoded_len;

use super::{memory::AgentMemoryService, storage::AgentStorage};

const ENVIRONMENT_KNOWLEDGE_TIER: &str = "environment_knowledge";
const EXACT_OVERLAP_NGRAM_CHARS: usize = 8;

#[derive(Debug, Clone)]
pub struct MemoryPromptCandidateProfile {
    pub source_text: Arc<str>,
    pub text_lower: String,
    pub text_tokens: BTreeSet<String>,
    pub fuzzy_ngrams: Vec<u32>,
    pub exact_overlap_ngrams: Arc<[u64]>,
    pub text_char_count: usize,
    pub exact_overlap_indices: Arc<[usize]>,
}

#[derive(Debug)]
pub struct MemoryPromptCandidateSnapshot {
    pub documents: Arc<[MemoryCandidateDocument]>,
    pub profiles: Arc<[MemoryPromptCandidateProfile]>,
    pub source_generation: String,
    estimated_bytes: usize,
}

impl MemoryPromptCandidateSnapshot {
    pub fn estimated_bytes(&self) -> usize {
        self.estimated_bytes
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemoryPromptSnapshotCacheStats {
    pub entries: usize,
    pub estimated_bytes: usize,
    pub hits: u64,
    pub misses: u64,
    pub builds: u64,
    pub background_refreshes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SnapshotCacheKey {
    root: PathBuf,
    agent_id: String,
    scope: &'static str,
    goal_id: Option<String>,
    include_environment_knowledge: bool,
    isolated_user_memory: bool,
    tier_definition_hash: [u8; 32],
    include_prompt_profiles: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceFileStamp {
    len: u64,
    modified_nanos: Option<u128>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceFingerprint {
    files: Vec<(PathBuf, Option<SourceFileStamp>)>,
}

#[derive(Debug, Clone)]
struct SnapshotBuildSpec {
    storage: AgentStorage,
    agent_id: String,
    tier_definitions: Vec<MemoryTierDefinition>,
    scope: TierScope,
    goal_id: Option<String>,
    include_environment_knowledge: bool,
    isolated_user_root: Option<PathBuf>,
    include_prompt_profiles: bool,
}

impl SnapshotBuildSpec {
    fn request(&self) -> MemoryCandidateRequest<'_> {
        MemoryCandidateRequest {
            scope: self.scope.clone(),
            goal_id: self.goal_id.as_deref(),
            recency_cutoff: None,
            include_environment_knowledge: self.include_environment_knowledge,
            // Unbound, and `SnapshotCacheKey` deliberately has no engagement
            // field to match. This cache is shared across every execution that
            // asks for the same root/agent/scope/goal/tiers, so a snapshot
            // built under one engagement would be served to the next — the
            // §5A.2 leak arriving through the cache. The snapshot stays
            // complete; `memory_prompt_blocks` applies containment per render.
            retrieval_scope: RetrievalScope::Unbound,
            isolated_user_root: self.isolated_user_root.clone(),
        }
    }
}

#[derive(Debug)]
struct SnapshotCacheEntry {
    fingerprint: SourceFingerprint,
    /// Content of the stamped source files the snapshot was built from.
    content_hashes: Arc<HashMap<PathBuf, [u8; 32]>>,
    snapshot: Arc<MemoryPromptCandidateSnapshot>,
    spec: SnapshotBuildSpec,
    last_used: Instant,
}

struct SnapshotBuildLockCleanup<'a> {
    key: &'a SnapshotCacheKey,
    build_lock: &'a Arc<tokio::sync::Mutex<()>>,
}

impl Drop for SnapshotBuildLockCleanup<'_> {
    fn drop(&mut self) {
        release_unused_build_lock(self.key, self.build_lock);
    }
}

#[derive(Debug, Default)]
struct SnapshotCache {
    entries: HashMap<SnapshotCacheKey, SnapshotCacheEntry>,
    estimated_bytes: usize,
}

/// One episode file's candidate, kept between builds so a rebuild reads only
/// episode files whose content changed. A document is a function of the file
/// bytes, its path and the storage's scope, all fixed by the cache key.
#[derive(Debug)]
struct EpisodeFileState {
    stamp: SourceFileStamp,
    content_hash: [u8; 32],
    document: Option<MemoryCandidateDocument>,
}

type EpisodeFileCache = HashMap<(PathBuf, String), HashMap<PathBuf, Arc<EpisodeFileState>>>;

fn episode_file_cache() -> &'static StdMutex<EpisodeFileCache> {
    static CACHE: OnceLock<StdMutex<EpisodeFileCache>> = OnceLock::new();
    CACHE.get_or_init(|| StdMutex::new(HashMap::new()))
}

/// What one tier file's candidates depend on besides its bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TierFileKey {
    root: PathBuf,
    agent_id: String,
    tier_definition_hash: [u8; 32],
    scope: &'static str,
    goal_id: Option<String>,
}

/// One tier file's candidates, kept between builds like [`EpisodeFileState`].
#[derive(Debug)]
struct TierFileState {
    stamp: SourceFileStamp,
    content_hash: [u8; 32],
    documents: Arc<[MemoryCandidateDocument]>,
}

fn tier_file_cache() -> &'static StdMutex<HashMap<TierFileKey, Arc<TierFileState>>> {
    static CACHE: OnceLock<StdMutex<HashMap<TierFileKey, Arc<TierFileState>>>> = OnceLock::new();
    CACHE.get_or_init(|| StdMutex::new(HashMap::new()))
}

fn snapshot_settings() -> &'static RwLock<MagicianMemoryPromptSnapshotSettings> {
    static SETTINGS: OnceLock<RwLock<MagicianMemoryPromptSnapshotSettings>> = OnceLock::new();
    SETTINGS.get_or_init(|| RwLock::new(MagicianMemoryPromptSnapshotSettings::default()))
}

fn snapshot_cache() -> &'static StdMutex<SnapshotCache> {
    static CACHE: OnceLock<StdMutex<SnapshotCache>> = OnceLock::new();
    CACHE.get_or_init(|| StdMutex::new(SnapshotCache::default()))
}

fn snapshot_build_locks(
) -> &'static StdMutex<HashMap<SnapshotCacheKey, Arc<tokio::sync::Mutex<()>>>> {
    static LOCKS: OnceLock<StdMutex<HashMap<SnapshotCacheKey, Arc<tokio::sync::Mutex<()>>>>> =
        OnceLock::new();
    LOCKS.get_or_init(|| StdMutex::new(HashMap::new()))
}

fn snapshot_refresh_worker() -> &'static StdMutex<Option<tokio::task::JoinHandle<()>>> {
    static WORKER: OnceLock<StdMutex<Option<tokio::task::JoinHandle<()>>>> = OnceLock::new();
    WORKER.get_or_init(|| StdMutex::new(None))
}

static CACHE_HITS: AtomicU64 = AtomicU64::new(0);
static CACHE_MISSES: AtomicU64 = AtomicU64::new(0);
static CACHE_BUILDS: AtomicU64 = AtomicU64::new(0);
static BACKGROUND_REFRESHES: AtomicU64 = AtomicU64::new(0);

pub fn configure_memory_prompt_snapshot(settings: &MagicianMemoryPromptSnapshotSettings) {
    if let Ok(mut current) = snapshot_settings().write() {
        *current = settings.clone();
    }
    if let Ok(mut cache) = snapshot_cache().lock() {
        prune_cache(&mut cache, settings, Instant::now());
    }
}

pub fn memory_prompt_snapshot_cache_stats() -> MemoryPromptSnapshotCacheStats {
    let (entries, estimated_bytes) = snapshot_cache()
        .lock()
        .map(|cache| (cache.entries.len(), cache.estimated_bytes))
        .unwrap_or_default();
    MemoryPromptSnapshotCacheStats {
        entries,
        estimated_bytes,
        hits: CACHE_HITS.load(Ordering::Relaxed),
        misses: CACHE_MISSES.load(Ordering::Relaxed),
        builds: CACHE_BUILDS.load(Ordering::Relaxed),
        background_refreshes: BACKGROUND_REFRESHES.load(Ordering::Relaxed),
    }
}

pub async fn load_memory_prompt_candidate_snapshot(
    memory_service: &AgentMemoryService,
    agent_id: &str,
    tier_definitions: &[MemoryTierDefinition],
    request: &MemoryCandidateRequest<'_>,
) -> Result<Arc<MemoryPromptCandidateSnapshot>, MemoryStorageError> {
    load_memory_candidate_snapshot(memory_service, agent_id, tier_definitions, request, true).await
}

/// Recall needs canonical documents, not the prompt renderer's lexical and
/// duplicate-comparison graph. Keep the same bounded, source-validated cache
/// while separating these entries from fully profiled prompt snapshots.
/// Engagement containment is applied by the recall caller before ranking.
pub async fn load_memory_recall_candidate_documents(
    memory_service: &AgentMemoryService,
    agent_id: &str,
    tier_definitions: &[MemoryTierDefinition],
    request: &MemoryCandidateRequest<'_>,
) -> Result<Arc<[MemoryCandidateDocument]>, MemoryStorageError> {
    let snapshot =
        load_memory_candidate_snapshot(memory_service, agent_id, tier_definitions, request, false)
            .await?;
    Ok(Arc::clone(&snapshot.documents))
}

async fn load_memory_candidate_snapshot(
    memory_service: &AgentMemoryService,
    agent_id: &str,
    tier_definitions: &[MemoryTierDefinition],
    request: &MemoryCandidateRequest<'_>,
    include_prompt_profiles: bool,
) -> Result<Arc<MemoryPromptCandidateSnapshot>, MemoryStorageError> {
    let started = Instant::now();
    ensure_snapshot_refresh_worker();
    let settings = current_settings();
    if !settings.enabled
        || settings.max_bytes == 0
        || settings.max_entries == 0
        || request.recency_cutoff.is_some()
    {
        return build_uncached_snapshot(
            memory_service.storage(),
            agent_id,
            tier_definitions,
            request,
            "uncached",
            include_prompt_profiles,
        )
        .await;
    }

    let key = snapshot_cache_key(
        memory_service.storage(),
        agent_id,
        tier_definitions,
        request,
        include_prompt_profiles,
    )?;
    let spec = SnapshotBuildSpec {
        storage: memory_service.storage().clone(),
        agent_id: agent_id.to_string(),
        tier_definitions: tier_definitions.to_vec(),
        scope: request.scope.clone(),
        goal_id: request.goal_id.map(ToString::to_string),
        include_environment_knowledge: request.include_environment_knowledge,
        isolated_user_root: request.isolated_user_root.clone(),
        include_prompt_profiles,
    };
    let fingerprint = source_fingerprint(&spec).await?;
    if let Some(snapshot) = cached_snapshot(&key, &fingerprint, &settings) {
        CACHE_HITS.fetch_add(1, Ordering::Relaxed);
        tracing::info!(target: "memory_index", include_prompt_profiles,
            elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
            candidates = snapshot.documents.len(), estimated_bytes = snapshot.estimated_bytes(),
            "Memory candidate snapshot cache hit");
        return Ok(snapshot);
    }
    CACHE_MISSES.fetch_add(1, Ordering::Relaxed);

    // The build runs as its own task. A retrieval that reaches its deadline
    // drops this future; a build polled in place was aborted with it, so a
    // multi-second rebuild never reached the cache and every later lookup
    // started it again. Detached, it finishes and serves the next caller.
    tokio::spawn(build_snapshot_under_lock(key, spec, settings, started))
        .await
        .map_err(|error| {
            MemoryStorageError::Other(format!("memory prompt snapshot build: {error}"))
        })?
}

fn snapshot_build_lock(key: &SnapshotCacheKey) -> Arc<tokio::sync::Mutex<()>> {
    snapshot_build_locks()
        .lock()
        .map(|mut locks| {
            Arc::clone(
                locks
                    .entry(key.clone())
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
            )
        })
        .unwrap_or_else(|_| Arc::new(tokio::sync::Mutex::new(())))
}

async fn build_snapshot_under_lock(
    key: SnapshotCacheKey,
    spec: SnapshotBuildSpec,
    settings: MagicianMemoryPromptSnapshotSettings,
    started: Instant,
) -> Result<Arc<MemoryPromptCandidateSnapshot>, MemoryStorageError> {
    let build_lock = snapshot_build_lock(&key);
    let _cleanup = SnapshotBuildLockCleanup {
        key: &key,
        build_lock: &build_lock,
    };
    let guard = build_lock.lock().await;
    let result = async {
        let fingerprint = source_fingerprint(&spec).await?;
        if let Some(snapshot) = cached_snapshot(&key, &fingerprint, &settings) {
            CACHE_HITS.fetch_add(1, Ordering::Relaxed);
            return Ok(snapshot);
        }
        // New stamps on unchanged bytes (a touch, an identical rewrite) keep
        // the snapshot rather than rebuilding it.
        if restamp_if_content_unchanged(&key, &fingerprint).await? {
            if let Some(snapshot) = cached_snapshot(&key, &fingerprint, &settings) {
                CACHE_HITS.fetch_add(1, Ordering::Relaxed);
                return Ok(snapshot);
            }
        }
        let previous = cached_entry_snapshot(&key);
        let (fingerprint, snapshot, content_hashes) =
            build_consistent_snapshot(&spec, fingerprint, previous).await?;
        CACHE_BUILDS.fetch_add(1, Ordering::Relaxed);
        tracing::info!(target: "memory_index",
            include_prompt_profiles = spec.include_prompt_profiles,
            elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
            candidates = snapshot.documents.len(), estimated_bytes = snapshot.estimated_bytes(),
            cacheable = snapshot.estimated_bytes() <= settings.max_bytes,
            "Memory candidate snapshot cache build");
        insert_snapshot(
            key.clone(),
            fingerprint,
            content_hashes,
            snapshot.clone(),
            spec,
            &settings,
            None,
        );
        Ok(snapshot)
    }
    .await;
    drop(guard);
    result
}

fn current_settings() -> MagicianMemoryPromptSnapshotSettings {
    snapshot_settings()
        .read()
        .map(|settings| settings.clone())
        .unwrap_or_default()
}

fn snapshot_cache_key(
    storage: &AgentStorage,
    agent_id: &str,
    tier_definitions: &[MemoryTierDefinition],
    request: &MemoryCandidateRequest<'_>,
    include_prompt_profiles: bool,
) -> Result<SnapshotCacheKey, MemoryStorageError> {
    let tier_json = serde_json::to_vec(tier_definitions).map_err(MemoryStorageError::Json)?;
    Ok(SnapshotCacheKey {
        root: storage.root().to_path_buf(),
        agent_id: agent_id.to_string(),
        scope: scope_label(&request.scope),
        goal_id: request.goal_id.map(ToString::to_string),
        include_environment_knowledge: request.include_environment_knowledge,
        isolated_user_memory: request.isolated_user_root.is_some(),
        tier_definition_hash: *blake3::hash(&tier_json).as_bytes(),
        include_prompt_profiles,
    })
}

/// The entry's snapshot whatever its freshness: the base a rebuild of the
/// same key reuses unchanged profiles from.
fn cached_entry_snapshot(key: &SnapshotCacheKey) -> Option<Arc<MemoryPromptCandidateSnapshot>> {
    let cache = snapshot_cache().lock().ok()?;
    cache
        .entries
        .get(key)
        .map(|entry| Arc::clone(&entry.snapshot))
}

fn cached_snapshot(
    key: &SnapshotCacheKey,
    fingerprint: &SourceFingerprint,
    settings: &MagicianMemoryPromptSnapshotSettings,
) -> Option<Arc<MemoryPromptCandidateSnapshot>> {
    let now = Instant::now();
    let mut cache = snapshot_cache().lock().ok()?;
    prune_cache(&mut cache, settings, now);
    let entry = cache.entries.get_mut(key)?;
    if &entry.fingerprint != fingerprint {
        return None;
    }
    entry.last_used = now;
    Some(Arc::clone(&entry.snapshot))
}

async fn build_uncached_snapshot(
    storage: &AgentStorage,
    agent_id: &str,
    tier_definitions: &[MemoryTierDefinition],
    request: &MemoryCandidateRequest<'_>,
    source_generation: &str,
    include_prompt_profiles: bool,
) -> Result<Arc<MemoryPromptCandidateSnapshot>, MemoryStorageError> {
    let documents = load_snapshot_documents(
        storage,
        agent_id,
        tier_definitions,
        request,
        include_prompt_profiles,
        false,
    )
    .await?;
    Ok(Arc::new(build_snapshot(
        documents,
        source_generation,
        include_prompt_profiles,
        None,
    )))
}

type BuiltSnapshot = (
    SourceFingerprint,
    Arc<MemoryPromptCandidateSnapshot>,
    Arc<HashMap<PathBuf, [u8; 32]>>,
);

async fn build_consistent_snapshot(
    spec: &SnapshotBuildSpec,
    mut before: SourceFingerprint,
    previous: Option<Arc<MemoryPromptCandidateSnapshot>>,
) -> Result<BuiltSnapshot, MemoryStorageError> {
    for _ in 0..3 {
        let load_started = Instant::now();
        let documents = load_snapshot_documents(
            &spec.storage,
            &spec.agent_id,
            &spec.tier_definitions,
            &spec.request(),
            spec.include_prompt_profiles,
            true,
        )
        .await?;
        tracing::info!(target: "memory_index",
            candidates = documents.len(),
            load_ms = load_started.elapsed().as_secs_f64() * 1_000.0,
            "Memory candidate snapshot source load");
        let after = source_fingerprint(spec).await?;
        if before == after {
            // Hashed at the stamps the documents were loaded under, so a later
            // touch can be told apart from a real change.
            let content = source_content(stamped_files(&after)).await?;
            if content.unstable {
                before = source_fingerprint(spec).await?;
                continue;
            }
            let generation = fingerprint_generation(&after);
            let include_prompt_profiles = spec.include_prompt_profiles;
            let previous = previous.clone();
            // Profiling ~2k documents is seconds of CPU; on an async worker it
            // also delayed the retrieval coordinator's deadline check.
            let snapshot = magician_core::blocking_admission::spawn_blocking_admitted(move || {
                build_snapshot(
                    documents,
                    &generation,
                    include_prompt_profiles,
                    previous.as_deref(),
                )
            })
            .await
            .map_err(|error| {
                MemoryStorageError::Other(format!("memory prompt snapshot profile: {error}"))
            })?;
            return Ok((after, Arc::new(snapshot), Arc::new(content.hashes)));
        }
        before = after;
        tokio::task::yield_now().await;
    }
    Err(MemoryStorageError::Other(
        "canonical memory kept changing while building prompt snapshot".to_string(),
    ))
}

async fn load_snapshot_documents(
    storage: &AgentStorage,
    agent_id: &str,
    tiers: &[MemoryTierDefinition],
    request: &MemoryCandidateRequest<'_>,
    include_prompt_profiles: bool,
    reuse_unchanged_files: bool,
) -> Result<Vec<MemoryCandidateDocument>, MemoryStorageError> {
    if include_prompt_profiles
        || !matches!(request.scope, TierScope::AgentGoal)
        || request.goal_id.is_some()
    {
        let mut prepared = PreparedCandidateSources::default();
        if reuse_unchanged_files {
            if matches!(request.scope, TierScope::Agent) {
                prepared.episodes =
                    Some(load_episode_documents_reusing_unchanged_files(storage, agent_id).await?);
            }
            prepared.tiers =
                load_tier_documents_reusing_unchanged_files(storage, agent_id, tiers, request)
                    .await?;
        }
        return load_memory_candidate_documents_with_prepared(
            storage, agent_id, tiers, request, prepared,
        )
        .await;
    }
    let mut documents = Vec::new();
    for tier in tiers
        .iter()
        .filter(|tier| matches!(tier.scope, TierScope::AgentGoal))
    {
        let goal_ids = magician_vector_index::memory_index::discover_goal_ids_for_tier(
            storage, agent_id, tier,
        )
        .await
        .map_err(|error| MemoryStorageError::Other(error.to_string()))?;
        for goal_id in goal_ids {
            let goal_request = MemoryCandidateRequest {
                goal_id: Some(&goal_id),
                ..request.clone()
            };
            documents.extend(
                load_memory_candidate_documents(
                    storage,
                    agent_id,
                    std::slice::from_ref(tier),
                    &goal_request,
                )
                .await?,
            );
        }
    }
    Ok(documents)
}

/// The agent's episode candidates, as `load_episode_candidate_documents`
/// returns them, reading only files whose stamp moved and whose bytes then
/// differ from the cached version.
async fn load_episode_documents_reusing_unchanged_files(
    storage: &AgentStorage,
    agent_id: &str,
) -> Result<Vec<MemoryCandidateDocument>, MemoryStorageError> {
    use magician_vector_index::episode_candidates::{
        episode_candidate_document_from_path, episode_candidate_paths,
    };
    let paths = episode_candidate_paths(storage, agent_id).await?;
    let cache_key = (storage.root().to_path_buf(), agent_id.to_string());
    let previous = episode_file_cache()
        .lock()
        .ok()
        .and_then(|cache| cache.get(&cache_key).cloned())
        .unwrap_or_default();
    let stamps = stat_paths(paths.clone()).await?;
    let to_hash = paths
        .iter()
        .zip(&stamps)
        .filter_map(|(path, stamp)| {
            let stamp = (*stamp)?;
            previous
                .get(path)
                .is_none_or(|state| state.stamp != stamp)
                .then(|| (path.clone(), stamp))
        })
        .collect::<Vec<_>>();
    let content = source_content(to_hash).await?;

    let mut documents = Vec::with_capacity(paths.len());
    let mut current = HashMap::with_capacity(paths.len());
    for (path, stamp) in paths.into_iter().zip(stamps) {
        let known = previous.get(&path);
        let hash = content.hashes.get(&path).copied();
        let reused = match (stamp, known) {
            (Some(stamp), Some(state)) if state.stamp == stamp => Some(Arc::clone(state)),
            (Some(stamp), Some(state)) if hash == Some(state.content_hash) => {
                Some(Arc::new(EpisodeFileState {
                    stamp,
                    content_hash: state.content_hash,
                    document: state.document.clone(),
                }))
            },
            _ => None,
        };
        let state = match reused {
            Some(state) => state,
            None => {
                let document =
                    episode_candidate_document_from_path(storage, agent_id, path.clone()).await?;
                let (Some(stamp), Some(content_hash)) = (stamp, hash) else {
                    // Changed while read: use it now, cache nothing.
                    documents.extend(document);
                    continue;
                };
                Arc::new(EpisodeFileState {
                    stamp,
                    content_hash,
                    document,
                })
            },
        };
        documents.extend(state.document.clone());
        current.insert(path, state);
    }
    if let Ok(mut cache) = episode_file_cache().lock() {
        cache.insert(cache_key, current);
    }
    Ok(documents)
}

/// Each in-scope tier's candidates, as `tier_candidate_documents` returns
/// them, reading a tier file only when its stamp moved and its bytes then
/// differ from the cached version. A tier left out (missing, or changed while
/// read) is loaded by the caller as usual.
async fn load_tier_documents_reusing_unchanged_files(
    storage: &AgentStorage,
    agent_id: &str,
    tiers: &[MemoryTierDefinition],
    request: &MemoryCandidateRequest<'_>,
) -> Result<HashMap<String, Vec<MemoryCandidateDocument>>, MemoryStorageError> {
    use magician_vector_index::memory_candidates::tier_candidate_documents;
    let mut prepared = HashMap::new();
    if request.recency_cutoff.is_some() {
        return Ok(prepared);
    }
    let mut sources = Vec::new();
    for tier in tiers
        .iter()
        .filter(|tier| same_scope(&tier.scope, &request.scope))
    {
        let goal_id = matches!(tier.scope, TierScope::AgentGoal)
            .then_some(request.goal_id)
            .flatten();
        let path = storage
            .agent_tier_path(agent_id, &tier.name, &tier.scope, goal_id)
            .map_err(MemoryStorageError::from)?;
        let tier_json = serde_json::to_vec(tier).map_err(MemoryStorageError::Json)?;
        let key = TierFileKey {
            root: storage.root().to_path_buf(),
            agent_id: agent_id.to_string(),
            tier_definition_hash: *blake3::hash(&tier_json).as_bytes(),
            scope: scope_label(&request.scope),
            goal_id: goal_id.map(ToString::to_string),
        };
        sources.push((tier, key, path));
    }
    let stamps = stat_paths(sources.iter().map(|(_, _, path)| path.clone()).collect()).await?;
    let previous = tier_file_cache()
        .lock()
        .map(|cache| {
            sources
                .iter()
                .map(|(_, key, _)| cache.get(key).cloned())
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|_| vec![None; sources.len()]);
    let to_hash = sources
        .iter()
        .zip(&stamps)
        .zip(&previous)
        .filter_map(|(((_, _, path), stamp), previous)| {
            let stamp = (*stamp)?;
            previous
                .as_ref()
                .is_none_or(|state| state.stamp != stamp)
                .then(|| (path.clone(), stamp))
        })
        .collect::<Vec<_>>();
    let content = source_content(to_hash).await?;

    let mut current = Vec::new();
    for (((tier, key, path), stamp), previous) in sources.into_iter().zip(stamps).zip(previous) {
        let Some(stamp) = stamp else {
            continue;
        };
        let hash = content.hashes.get(&path).copied();
        let state = match previous {
            Some(state) if state.stamp == stamp => state,
            Some(state) if hash == Some(state.content_hash) => Arc::new(TierFileState {
                stamp,
                content_hash: state.content_hash,
                documents: Arc::clone(&state.documents),
            }),
            _ => {
                let Some(content_hash) = hash else {
                    continue;
                };
                let documents = tier_candidate_documents(storage, agent_id, tier, request).await?;
                Arc::new(TierFileState {
                    stamp,
                    content_hash,
                    documents: Arc::from(documents),
                })
            },
        };
        prepared.insert(tier.name.clone(), state.documents.to_vec());
        current.push((key, state));
    }
    if let Ok(mut cache) = tier_file_cache().lock() {
        cache.extend(current);
    }
    Ok(prepared)
}

fn build_snapshot(
    documents: Vec<MemoryCandidateDocument>,
    source_generation: &str,
    include_prompt_profiles: bool,
    previous: Option<&MemoryPromptCandidateSnapshot>,
) -> MemoryPromptCandidateSnapshot {
    let profile_started = Instant::now();
    let (mut profiles, reused_profiles) = if include_prompt_profiles {
        candidate_profiles_reusing_previous(&documents, previous)
    } else {
        (Vec::new(), 0)
    };
    let graph_started = Instant::now();
    let new_texts = profiles.len().saturating_sub(reused_profiles);
    let incremental_graph = previous
        .filter(|previous| {
            previous.profiles.len() == previous.documents.len() && !previous.profiles.is_empty()
        })
        .filter(|_| new_texts <= INCREMENTAL_OVERLAP_GRAPH_MAX_NEW.max(profiles.len() / 4));
    match incremental_graph {
        Some(previous) => {
            populate_exact_overlap_graph_from_previous(&mut profiles, &previous.profiles)
        },
        None => populate_exact_overlap_graph(&mut profiles),
    }
    tracing::info!(target: "memory_index",
        candidates = documents.len(),
        reused_profiles,
        incremental_overlap_graph = incremental_graph.is_some(),
        profile_ms = graph_started.duration_since(profile_started).as_secs_f64() * 1_000.0,
        overlap_graph_ms = graph_started.elapsed().as_secs_f64() * 1_000.0,
        "Memory candidate snapshot build phases");
    let estimated_bytes = estimate_snapshot_bytes(&documents, &profiles);
    MemoryPromptCandidateSnapshot {
        documents: Arc::from(documents),
        profiles: Arc::from(profiles),
        source_generation: source_generation.to_string(),
        estimated_bytes,
    }
}

/// Build the same lexical/deduplication profile used by cached file-backed
/// candidates for a small set of live app-memory projections. Kept here so a
/// second retrieval producer cannot silently drift to different ranking or
/// exact-overlap semantics.
pub(crate) fn build_memory_prompt_candidate_profiles(
    documents: &[MemoryCandidateDocument],
) -> Vec<MemoryPromptCandidateProfile> {
    let mut profiles = candidate_profiles_without_overlap_graph(documents);
    populate_exact_overlap_graph(&mut profiles);
    profiles
}

fn candidate_profiles_without_overlap_graph(
    documents: &[MemoryCandidateDocument],
) -> Vec<MemoryPromptCandidateProfile> {
    documents
        .iter()
        .map(|document| candidate_profile(&document.text))
        .collect()
}

/// A candidate's profile, without overlap links. Every field derives from
/// the document's text alone, which is what lets a rebuild reuse it.
fn candidate_profile(text: &str) -> MemoryPromptCandidateProfile {
    let dedupe_text = text.trim();
    MemoryPromptCandidateProfile {
        source_text: Arc::<str>::from(text),
        text_lower: text.to_lowercase(),
        text_tokens: normalized_token_set(text),
        fuzzy_ngrams: packed_char_ngrams(text),
        exact_overlap_ngrams: Arc::from(exact_overlap_ngrams(dedupe_text)),
        text_char_count: dedupe_text.chars().count(),
        exact_overlap_indices: Arc::from(Vec::<usize>::new()),
    }
}

/// Profiles for `documents`, taking each unchanged text's profile from the
/// previous snapshot of the same key and computing only new texts. Overlap
/// links are cleared: they depend on the whole set and are rebuilt after.
fn candidate_profiles_reusing_previous(
    documents: &[MemoryCandidateDocument],
    previous: Option<&MemoryPromptCandidateSnapshot>,
) -> (Vec<MemoryPromptCandidateProfile>, usize) {
    let known = previous
        .filter(|previous| previous.profiles.len() == previous.documents.len())
        .map(|previous| {
            previous
                .documents
                .iter()
                .zip(previous.profiles.iter())
                .map(|(document, profile)| (document.text.as_str(), profile))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let mut reused = 0;
    let profiles = documents
        .iter()
        .map(|document| match known.get(document.text.as_str()) {
            Some(profile) => {
                reused += 1;
                MemoryPromptCandidateProfile {
                    exact_overlap_indices: Arc::from(Vec::<usize>::new()),
                    ..(*profile).clone()
                }
            },
            None => candidate_profile(&document.text),
        })
        .collect();
    (profiles, reused)
}

fn estimate_snapshot_bytes(
    documents: &[MemoryCandidateDocument],
    profiles: &[MemoryPromptCandidateProfile],
) -> usize {
    let document_bytes = documents
        .iter()
        .map(|document| {
            document
                .text
                .len()
                .saturating_add(document.tier_name.len())
                .saturating_add(document.item_key.len())
                .saturating_add(document.json_pointer.len())
                .saturating_add(document.content_hash.len())
                .saturating_add(json_encoded_len(&document.metadata_json).unwrap_or(usize::MAX))
                .saturating_add(384)
        })
        .fold(0usize, usize::saturating_add);
    let profile_bytes = profiles
        .iter()
        .map(|profile| {
            profile
                .source_text
                .len()
                .saturating_add(profile.text_lower.len())
                .saturating_add(
                    profile
                        .fuzzy_ngrams
                        .len()
                        .saturating_mul(std::mem::size_of::<u32>()),
                )
                .saturating_add(
                    profile
                        .exact_overlap_ngrams
                        .len()
                        .saturating_mul(std::mem::size_of::<u64>()),
                )
                .saturating_add(
                    profile
                        .exact_overlap_indices
                        .len()
                        .saturating_mul(std::mem::size_of::<usize>()),
                )
                .saturating_add(
                    profile
                        .text_tokens
                        .iter()
                        .map(|token| token.len().saturating_add(32))
                        .fold(0usize, usize::saturating_add),
                )
                .saturating_add(128)
        })
        .fold(0usize, usize::saturating_add);
    document_bytes.saturating_add(profile_bytes)
}

async fn source_fingerprint(
    spec: &SnapshotBuildSpec,
) -> Result<SourceFingerprint, MemoryStorageError> {
    let mut paths = source_paths(spec)?;
    if !spec.include_prompt_profiles
        && matches!(spec.scope, TierScope::AgentGoal)
        && spec.goal_id.is_none()
    {
        for tier in spec
            .tier_definitions
            .iter()
            .filter(|tier| matches!(tier.scope, TierScope::AgentGoal))
        {
            let goal_ids = magician_vector_index::memory_index::discover_goal_ids_for_tier(
                &spec.storage,
                &spec.agent_id,
                tier,
            )
            .await
            .map_err(|error| MemoryStorageError::Other(error.to_string()))?;
            for goal_id in goal_ids {
                paths.push(
                    spec.storage
                        .agent_tier_path(&spec.agent_id, &tier.name, &tier.scope, Some(&goal_id))
                        .map_err(MemoryStorageError::from)?,
                );
            }
        }
    }
    if matches!(spec.scope, TierScope::Agent) {
        let episodes = spec
            .storage
            .agent_episodes_dir(&spec.agent_id)
            .map_err(MemoryStorageError::from)?;
        // The directory stamp detects additions/removals; individual stamps
        // detect repair or source revision without waiting for a TTL.
        paths.extend(
            spec.storage
                .list_files_with_extension(&episodes, "json")
                .await
                .map_err(MemoryStorageError::from)?,
        );
        paths.push(episodes);
    }
    paths.sort();
    paths.dedup();
    let stamps = stat_paths(paths.clone()).await?;
    Ok(SourceFingerprint {
        files: paths.into_iter().zip(stamps).collect(),
    })
}

fn file_stamp(path: &Path) -> Result<Option<SourceFileStamp>, std::io::Error> {
    match std::fs::metadata(path) {
        Ok(metadata) => {
            let modified_nanos = metadata
                .modified()
                .ok()
                .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos());
            Ok(Some(SourceFileStamp {
                len: metadata.len(),
                modified_nanos,
            }))
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Stat a source set in one admitted job. One Tokio filesystem job per
/// episode otherwise dominates cache hits for large histories.
async fn stat_paths(
    paths: Vec<PathBuf>,
) -> Result<Vec<Option<SourceFileStamp>>, MemoryStorageError> {
    magician_core::blocking_admission::spawn_blocking_admitted(move || {
        paths
            .iter()
            .map(|path| file_stamp(path).map_err(MemoryStorageError::Io))
            .collect::<Result<Vec<_>, _>>()
    })
    .await
    .map_err(|error| MemoryStorageError::Other(format!("memory source fingerprint: {error}")))?
}

/// Content of a set of source files, for telling a real change from a new
/// stamp on the same bytes (a `touch`, or a writer rewriting identical JSON).
#[derive(Debug, Default)]
struct SourceContent {
    hashes: HashMap<PathBuf, [u8; 32]>,
    directories: BTreeSet<PathBuf>,
    /// A file changed while it was read; its hash is not known.
    unstable: bool,
}

fn content_hash_memo() -> &'static StdMutex<HashMap<PathBuf, (SourceFileStamp, [u8; 32])>> {
    static MEMO: OnceLock<StdMutex<HashMap<PathBuf, (SourceFileStamp, [u8; 32])>>> =
        OnceLock::new();
    MEMO.get_or_init(|| StdMutex::new(HashMap::new()))
}

/// Hash each file at its given stamp. A file is read only when the memo has
/// no hash for that exact stamp — the same stamp-means-same-bytes assumption
/// the fingerprint already makes.
async fn source_content(
    files: Vec<(PathBuf, SourceFileStamp)>,
) -> Result<SourceContent, MemoryStorageError> {
    let mut content = SourceContent::default();
    let mut to_read = Vec::new();
    if let Ok(memo) = content_hash_memo().lock() {
        for (path, stamp) in files {
            match memo.get(&path) {
                Some((known, hash)) if *known == stamp => {
                    content.hashes.insert(path, *hash);
                },
                _ => to_read.push((path, stamp)),
            }
        }
    } else {
        to_read = files;
    }
    if to_read.is_empty() {
        return Ok(content);
    }
    let read = magician_core::blocking_admission::spawn_blocking_admitted(move || {
        let mut read = Vec::with_capacity(to_read.len());
        for (path, stamp) in to_read {
            if std::fs::metadata(&path).is_ok_and(|metadata| metadata.is_dir()) {
                read.push((path, stamp, None, true));
                continue;
            }
            let hash = match std::fs::read(&path) {
                Ok(bytes) => Some(*blake3::hash(&bytes).as_bytes()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(MemoryStorageError::Io(error)),
            };
            // Bytes read under a different stamp belong to neither version.
            let still = file_stamp(&path).map_err(MemoryStorageError::Io)?;
            read.push((path, stamp, hash.filter(|_| still == Some(stamp)), false));
        }
        Ok(read)
    })
    .await
    .map_err(|error| MemoryStorageError::Other(format!("memory source content: {error}")))??;
    let mut memo = content_hash_memo().lock().ok();
    for (path, stamp, hash, is_directory) in read {
        if is_directory {
            content.directories.insert(path);
            continue;
        }
        let Some(hash) = hash else {
            content.unstable = true;
            continue;
        };
        if let Some(memo) = memo.as_mut() {
            memo.insert(path.clone(), (stamp, hash));
        }
        content.hashes.insert(path, hash);
    }
    Ok(content)
}

fn stamped_files(fingerprint: &SourceFingerprint) -> Vec<(PathBuf, SourceFileStamp)> {
    fingerprint
        .files
        .iter()
        .filter_map(|(path, stamp)| stamp.map(|stamp| (path.clone(), stamp)))
        .collect()
}

/// Accept `fingerprint` for a cached entry whose sources changed stamp but not
/// content: the entry is re-stamped and keeps its snapshot. False when any
/// source was added, removed, or really changed.
async fn restamp_if_content_unchanged(
    key: &SnapshotCacheKey,
    fingerprint: &SourceFingerprint,
) -> Result<bool, MemoryStorageError> {
    let Some((previous, hashes)) = snapshot_cache().lock().ok().and_then(|cache| {
        cache
            .entries
            .get(key)
            .map(|entry| (entry.fingerprint.clone(), Arc::clone(&entry.content_hashes)))
    }) else {
        return Ok(false);
    };
    if previous.files.len() != fingerprint.files.len() {
        return Ok(false);
    }
    let mut changed = Vec::new();
    for ((path, before), (now_path, now)) in previous.files.iter().zip(&fingerprint.files) {
        if path != now_path {
            return Ok(false);
        }
        if before == now {
            continue;
        }
        let Some(now) = now else {
            return Ok(false);
        };
        if before.is_none() {
            return Ok(false);
        }
        changed.push((path.clone(), *now));
    }
    let content = source_content(changed.clone()).await?;
    if content.unstable {
        return Ok(false);
    }
    for (path, _) in &changed {
        // A directory's stamp moves when a writer renames a temp file into
        // it; additions and removals already show in the path list above.
        if content.directories.contains(path) {
            continue;
        }
        if hashes.get(path).is_none() || hashes.get(path) != content.hashes.get(path) {
            return Ok(false);
        }
    }
    let Ok(mut cache) = snapshot_cache().lock() else {
        return Ok(false);
    };
    match cache.entries.get_mut(key) {
        Some(entry) if entry.fingerprint == previous => {
            entry.fingerprint = fingerprint.clone();
            Ok(true)
        },
        _ => Ok(false),
    }
}

fn source_paths(spec: &SnapshotBuildSpec) -> Result<Vec<PathBuf>, MemoryStorageError> {
    let mut paths = Vec::new();
    if matches!(spec.scope, TierScope::User) {
        if let Some(root) = &spec.isolated_user_root {
            paths.push(root.join("knowledge.json"));
        } else {
            paths.push(spec.storage.user_knowledge_path());
            for file_name in ["contacts.json", "routines.json", "research_findings.json"] {
                paths.push(spec.storage.user_root().join(file_name));
            }
        }
    }
    for tier in spec
        .tier_definitions
        .iter()
        .filter(|tier| same_scope(&tier.scope, &spec.scope))
    {
        if !spec.include_prompt_profiles
            && matches!(tier.scope, TierScope::AgentGoal)
            && spec.goal_id.is_none()
        {
            continue;
        }
        if matches!(spec.scope, TierScope::Agent)
            && tier.name == ENVIRONMENT_KNOWLEDGE_TIER
            && !spec.include_environment_knowledge
        {
            continue;
        }
        let goal_id = if matches!(tier.scope, TierScope::AgentGoal) {
            spec.goal_id.as_deref()
        } else {
            None
        };
        paths.push(
            spec.storage
                .agent_tier_path(&spec.agent_id, &tier.name, &tier.scope, goal_id)
                .map_err(MemoryStorageError::from)?,
        );
    }
    Ok(paths)
}

fn fingerprint_generation(fingerprint: &SourceFingerprint) -> String {
    let mut hasher = blake3::Hasher::new();
    for (path, stamp) in &fingerprint.files {
        hasher.update(path.to_string_lossy().as_bytes());
        match stamp {
            Some(stamp) => {
                hasher.update(&stamp.len.to_le_bytes());
                hasher.update(&stamp.modified_nanos.unwrap_or_default().to_le_bytes());
            },
            None => {
                hasher.update(b"missing");
            },
        };
    }
    hasher.finalize().to_hex().to_string()
}

fn insert_snapshot(
    key: SnapshotCacheKey,
    fingerprint: SourceFingerprint,
    content_hashes: Arc<HashMap<PathBuf, [u8; 32]>>,
    snapshot: Arc<MemoryPromptCandidateSnapshot>,
    spec: SnapshotBuildSpec,
    settings: &MagicianMemoryPromptSnapshotSettings,
    // A background refresh keeps the entry's own last use: a write is not a
    // read, and resetting it kept entries alive while memory changed.
    last_used: Option<Instant>,
) {
    if snapshot.estimated_bytes() > settings.max_bytes {
        return;
    }
    let Ok(mut cache) = snapshot_cache().lock() else {
        return;
    };
    let now = Instant::now();
    prune_cache(&mut cache, settings, now);
    if let Some(previous) = cache.entries.remove(&key) {
        cache.estimated_bytes = cache
            .estimated_bytes
            .saturating_sub(previous.snapshot.estimated_bytes());
    }
    while !cache.entries.is_empty()
        && (cache.entries.len() >= settings.max_entries
            || cache
                .estimated_bytes
                .saturating_add(snapshot.estimated_bytes())
                > settings.max_bytes)
    {
        evict_oldest(&mut cache);
    }
    cache.estimated_bytes = cache
        .estimated_bytes
        .saturating_add(snapshot.estimated_bytes());
    cache.entries.insert(
        key,
        SnapshotCacheEntry {
            fingerprint,
            content_hashes,
            snapshot,
            spec,
            last_used: last_used.unwrap_or(now),
        },
    );
}

fn prune_cache(
    cache: &mut SnapshotCache,
    settings: &MagicianMemoryPromptSnapshotSettings,
    now: Instant,
) {
    if !settings.enabled || settings.max_bytes == 0 || settings.max_entries == 0 {
        cache.entries.clear();
        cache.estimated_bytes = 0;
        return;
    }
    let idle_ttl = Duration::from_secs(settings.idle_ttl_secs.max(1));
    let expired = cache
        .entries
        .iter()
        .filter_map(|(key, entry)| {
            (now.saturating_duration_since(entry.last_used) > idle_ttl).then_some(key.clone())
        })
        .collect::<Vec<_>>();
    for key in expired {
        if let Some(entry) = cache.entries.remove(&key) {
            cache.estimated_bytes = cache
                .estimated_bytes
                .saturating_sub(entry.snapshot.estimated_bytes());
        }
    }
    while cache.entries.len() > settings.max_entries || cache.estimated_bytes > settings.max_bytes {
        evict_oldest(cache);
    }
}

fn evict_oldest(cache: &mut SnapshotCache) {
    let oldest_key = cache
        .entries
        .iter()
        .min_by_key(|(_, entry)| entry.last_used)
        .map(|(key, _)| key.clone());
    if let Some(key) = oldest_key {
        if let Some(entry) = cache.entries.remove(&key) {
            cache.estimated_bytes = cache
                .estimated_bytes
                .saturating_sub(entry.snapshot.estimated_bytes());
        }
    }
}

fn release_unused_build_lock(key: &SnapshotCacheKey, build_lock: &Arc<tokio::sync::Mutex<()>>) {
    if Arc::strong_count(build_lock) != 2 {
        return;
    }
    if let Ok(mut locks) = snapshot_build_locks().lock() {
        if locks
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, build_lock))
        {
            locks.remove(key);
        }
    }
}

fn ensure_snapshot_refresh_worker() {
    let Ok(mut worker) = snapshot_refresh_worker().lock() else {
        return;
    };
    if worker.as_ref().is_some_and(|handle| !handle.is_finished()) {
        return;
    }
    let mut receiver = subscribe_memory_index_changes();
    *worker = Some(tokio::spawn(async move {
        loop {
            let first = match receiver.recv().await {
                Ok(notification) => notification,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    evict_all_snapshots();
                    continue;
                },
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            let settings = current_settings();
            let mut roots = HashMap::<PathBuf, bool>::new();
            roots.insert(
                first.root,
                matches!(first.change, MemoryIndexChange::FullScope { .. }),
            );
            let debounce = Duration::from_millis(settings.refresh_debounce_ms.max(1));
            let deadline = tokio::time::Instant::now() + debounce;
            loop {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    break;
                }
                match tokio::time::timeout(remaining, receiver.recv()).await {
                    Ok(Ok(notification)) => {
                        let full_scope =
                            matches!(notification.change, MemoryIndexChange::FullScope { .. });
                        roots
                            .entry(notification.root)
                            .and_modify(|current| *current |= full_scope)
                            .or_insert(full_scope);
                    },
                    Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                        evict_all_snapshots();
                    },
                    Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => return,
                    Err(_) => break,
                }
            }
            for (root, full_scope) in roots {
                if full_scope {
                    evict_snapshots_for_root(&root);
                } else {
                    refresh_snapshots_for_root(&root).await;
                }
                tokio::task::yield_now().await;
            }
        }
    }));
}

fn evict_all_snapshots() {
    if let Ok(mut cache) = snapshot_cache().lock() {
        cache.entries.clear();
        cache.estimated_bytes = 0;
    }
}

fn evict_snapshots_for_root(root: &Path) {
    if let Ok(mut cache) = snapshot_cache().lock() {
        let keys = cache
            .entries
            .keys()
            .filter(|key| key.root == root)
            .cloned()
            .collect::<Vec<_>>();
        for key in keys {
            if let Some(entry) = cache.entries.remove(&key) {
                cache.estimated_bytes = cache
                    .estimated_bytes
                    .saturating_sub(entry.snapshot.estimated_bytes());
            }
        }
    }
}

async fn refresh_snapshots_for_root(root: &Path) {
    let settings = current_settings();
    if !settings.enabled {
        return;
    }
    let specs = snapshot_cache()
        .lock()
        .map(|cache| {
            cache
                .entries
                .iter()
                .filter(|(key, _)| key.root == root)
                .map(|(key, entry)| (key.clone(), entry.spec.clone()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for (key, spec) in specs {
        // Same lock as a foreground build, so the two never build one key
        // at once; a foreground build that just finished is then seen below.
        let build_lock = snapshot_build_lock(&key);
        let _cleanup = SnapshotBuildLockCleanup {
            key: &key,
            build_lock: &build_lock,
        };
        let guard = build_lock.lock().await;
        let before = match source_fingerprint(&spec).await {
            Ok(fingerprint) => fingerprint,
            Err(error) => {
                warn!(
                    root = %root.display(),
                    error = %error,
                    "failed to fingerprint memory prompt snapshot during background refresh"
                );
                continue;
            },
        };
        // A change notification names a root, not this entry's sources: an
        // entry whose files are unchanged is still current, and one evicted
        // since the listing is not rebuilt.
        let Some(last_used) = stale_entry_last_used(&key, &before) else {
            continue;
        };
        match restamp_if_content_unchanged(&key, &before).await {
            Ok(true) => continue,
            Ok(false) => {},
            Err(error) => warn!(
                root = %root.display(),
                error = %error,
                "failed to compare memory prompt snapshot sources during background refresh"
            ),
        }
        let previous = cached_entry_snapshot(&key);
        match build_consistent_snapshot(&spec, before, previous).await {
            Ok((fingerprint, snapshot, content_hashes)) => {
                BACKGROUND_REFRESHES.fetch_add(1, Ordering::Relaxed);
                insert_snapshot(
                    key.clone(),
                    fingerprint,
                    content_hashes,
                    snapshot,
                    spec,
                    &settings,
                    Some(last_used),
                );
            },
            Err(error) => warn!(
                root = %root.display(),
                error = %error,
                "failed to rebuild memory prompt snapshot in background"
            ),
        }
        drop(guard);
    }
}

/// The last use of a cached entry whose sources no longer match
/// `fingerprint`; `None` when the entry is current or no longer cached.
fn stale_entry_last_used(
    key: &SnapshotCacheKey,
    fingerprint: &SourceFingerprint,
) -> Option<Instant> {
    let cache = snapshot_cache().lock().ok()?;
    let entry = cache.entries.get(key)?;
    (&entry.fingerprint != fingerprint).then_some(entry.last_used)
}

pub fn normalized_token_set(text: &str) -> BTreeSet<String> {
    text.split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter_map(normalize_token)
        .collect()
}

fn normalize_token(raw: &str) -> Option<String> {
    let mut token = raw.trim().to_ascii_lowercase();
    if token.len() < 3 {
        return None;
    }
    for suffix in ["ing", "ers", "ies", "ied", "ed", "es", "s"] {
        if token.len() > suffix.len() + 3 && token.ends_with(suffix) {
            token.truncate(token.len() - suffix.len());
            break;
        }
    }
    (token.len() >= 3).then_some(token)
}

pub fn packed_char_ngrams(text: &str) -> Vec<u32> {
    let compact = text
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| match ch.to_ascii_lowercase() {
            '0'..='9' => ch.to_ascii_lowercase() as u8 - b'0',
            'a'..='z' => ch.to_ascii_lowercase() as u8 - b'a' + 10,
            _ => unreachable!("ASCII alphanumeric filter"),
        })
        .collect::<Vec<_>>();
    if compact.len() < 5 {
        return Vec::new();
    }
    let mut ngrams = compact
        .windows(5)
        .map(|window| {
            window
                .iter()
                .fold(0_u32, |encoded, value| (encoded << 6) | u32::from(*value))
        })
        .collect::<Vec<_>>();
    ngrams.sort_unstable();
    ngrams.dedup();
    ngrams
}

pub fn exact_overlap_ngrams(text: &str) -> Vec<u64> {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.len() < EXACT_OVERLAP_NGRAM_CHARS {
        return Vec::new();
    }
    let mut ngrams = chars
        .windows(EXACT_OVERLAP_NGRAM_CHARS)
        .map(|window| {
            window.iter().fold(0xcbf29ce484222325_u64, |hash, ch| {
                (hash ^ (*ch as u32 as u64)).wrapping_mul(0x100000001b3)
            })
        })
        .collect::<Vec<_>>();
    ngrams.sort_unstable();
    ngrams.dedup();
    ngrams
}

/// New texts up to which a rebuild updates the overlap graph from the
/// previous snapshot instead of recomputing it (or a quarter of the set).
const INCREMENTAL_OVERLAP_GRAPH_MAX_NEW: usize = 64;

/// The overlap graph [`populate_exact_overlap_graph`] builds, computed from
/// the previous snapshot's graph. Whether two candidates overlap depends only
/// on their two texts, and the full build finds every overlapping pair (see
/// `precomputed_overlap_graph_matches_pairwise_reference`). So links between
/// texts that were both present before are carried over, identical texts link
/// to each other, and only texts new to this build are compared, against all
/// candidates.
fn populate_exact_overlap_graph_from_previous(
    profiles: &mut [MemoryPromptCandidateProfile],
    previous: &[MemoryPromptCandidateProfile],
) {
    let overlaps = {
        let mut previous_pairs = std::collections::HashSet::<(&str, &str)>::new();
        for profile in previous {
            let left = &*profile.source_text;
            for &index in profile.exact_overlap_indices.iter() {
                let right = &*previous[index].source_text;
                if left < right {
                    previous_pairs.insert((left, right));
                } else if right < left {
                    previous_pairs.insert((right, left));
                }
            }
        }
        let previous_texts = previous
            .iter()
            .map(|profile| &*profile.source_text)
            .collect::<std::collections::HashSet<_>>();
        let mut by_text = HashMap::<&str, Vec<usize>>::new();
        for (index, profile) in profiles.iter().enumerate() {
            by_text
                .entry(&*profile.source_text)
                .or_default()
                .push(index);
        }

        let mut overlaps = vec![Vec::<usize>::new(); profiles.len()];
        let link = |left: usize, right: usize, overlaps: &mut [Vec<usize>]| {
            overlaps[left].push(right);
            overlaps[right].push(left);
        };
        for indices in by_text.values() {
            let Some(&first) = indices.first() else {
                continue;
            };
            let profile = &profiles[first];
            let text = profile.source_text.trim();
            let count = profile.text_char_count;
            if indices.len() < 2 || !exact_high_overlap(text, text, count, count) {
                continue;
            }
            for (position, &left) in indices.iter().enumerate() {
                for &right in &indices[position + 1..] {
                    link(left, right, &mut overlaps);
                }
            }
        }
        for (left_text, right_text) in previous_pairs {
            let (Some(lefts), Some(rights)) = (by_text.get(left_text), by_text.get(right_text))
            else {
                continue;
            };
            for &left in lefts {
                for &right in rights {
                    link(left, right, &mut overlaps);
                }
            }
        }
        for (current_index, current) in profiles.iter().enumerate() {
            if previous_texts.contains(&*current.source_text) {
                continue;
            }
            for (other_index, other) in profiles.iter().enumerate() {
                if other.source_text == current.source_text {
                    continue;
                }
                if exact_high_overlap(
                    current.source_text.trim(),
                    other.source_text.trim(),
                    current.text_char_count,
                    other.text_char_count,
                ) {
                    link(current_index, other_index, &mut overlaps);
                }
            }
        }
        overlaps
    };
    for (profile, mut indices) in profiles.iter_mut().zip(overlaps) {
        indices.sort_unstable();
        indices.dedup();
        profile.exact_overlap_indices = Arc::from(indices);
    }
}

pub fn populate_exact_overlap_graph(profiles: &mut [MemoryPromptCandidateProfile]) {
    let mut overlaps = vec![Vec::<usize>::new(); profiles.len()];
    let mut short_text_indices = Vec::<usize>::new();
    let mut all_ngram_index = HashMap::<u64, Vec<usize>>::new();
    let mut anchor_ngram_index = HashMap::<u64, Vec<usize>>::new();

    for current_index in 0..profiles.len() {
        let current = &profiles[current_index];
        let mut candidate_indices = if current.text_char_count < EXACT_OVERLAP_NGRAM_CHARS {
            (0..current_index).collect::<Vec<_>>()
        } else {
            let mut candidates = BTreeSet::new();
            // Keep this hot snapshot-building loop iterative. The equivalent
            // `min_by_key` + nested `filter` adapter chain was the top native
            // frame in a measured debug-build stack overflow, even after the
            // caller had crossed to the execution runtime.
            let mut rarest_ngram = None::<(u64, usize)>;
            for ngram in current.exact_overlap_ngrams.iter().copied() {
                let frequency = match all_ngram_index.get(&ngram) {
                    Some(indices) => indices.len(),
                    None => 0,
                };
                let should_replace = match rarest_ngram {
                    Some((_, rarest_frequency)) => frequency < rarest_frequency,
                    None => true,
                };
                if should_replace {
                    rarest_ngram = Some((ngram, frequency));
                }
            }
            if let Some((rarest_ngram, _)) = rarest_ngram {
                if let Some(indices) = all_ngram_index.get(&rarest_ngram) {
                    for index in indices.iter().copied() {
                        if profiles[index].text_char_count >= current.text_char_count {
                            candidates.insert(index);
                        }
                    }
                }
            }
            for ngram in current.exact_overlap_ngrams.iter() {
                if let Some(indices) = anchor_ngram_index.get(ngram) {
                    for index in indices.iter().copied() {
                        if profiles[index].text_char_count <= current.text_char_count {
                            candidates.insert(index);
                        }
                    }
                }
            }
            for index in short_text_indices.iter().copied() {
                candidates.insert(index);
            }
            candidates.into_iter().collect::<Vec<_>>()
        };
        candidate_indices.sort_unstable();
        candidate_indices.dedup();

        for previous_index in candidate_indices {
            let previous = &profiles[previous_index];
            if exact_high_overlap(
                current.source_text.trim(),
                previous.source_text.trim(),
                current.text_char_count,
                previous.text_char_count,
            ) {
                overlaps[current_index].push(previous_index);
                overlaps[previous_index].push(current_index);
            }
        }

        if current.text_char_count < EXACT_OVERLAP_NGRAM_CHARS {
            short_text_indices.push(current_index);
        } else {
            if let Some(anchor) = current.exact_overlap_ngrams.first().copied() {
                anchor_ngram_index
                    .entry(anchor)
                    .or_default()
                    .push(current_index);
            }
            for ngram in current.exact_overlap_ngrams.iter().copied() {
                all_ngram_index
                    .entry(ngram)
                    .or_default()
                    .push(current_index);
            }
        }
    }

    for (profile, mut indices) in profiles.iter_mut().zip(overlaps) {
        indices.sort_unstable();
        indices.dedup();
        profile.exact_overlap_indices = Arc::from(indices);
    }
}

fn exact_high_overlap(
    left: &str,
    right: &str,
    left_char_count: usize,
    right_char_count: usize,
) -> bool {
    if left.is_empty() || right.is_empty() {
        return false;
    }
    let min_len = left_char_count.min(right_char_count);
    let max_len = left_char_count.max(right_char_count);
    min_len * 100 >= max_len * 80 && (left.contains(right) || right.contains(left))
}

fn scope_label(scope: &TierScope) -> &'static str {
    match scope {
        TierScope::User => "user",
        TierScope::Agent => "agent",
        TierScope::AgentGoal => "agent_goal",
    }
}

fn same_scope(left: &TierScope, right: &TierScope) -> bool {
    matches!(
        (left, right),
        (TierScope::User, TierScope::User)
            | (TierScope::Agent, TierScope::Agent)
            | (TierScope::AgentGoal, TierScope::AgentGoal)
    )
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn packed_features_match_reference_sets() {
        let text = "Credit Card 1234 due tomorrow; credit card follow-up";
        let packed = packed_char_ngrams(text);
        assert!(packed.windows(2).all(|pair| pair[0] < pair[1]));
        let exact = exact_overlap_ngrams(text);
        assert!(exact.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(normalized_token_set("Planning planned plans"), {
            let mut expected = BTreeSet::new();
            expected.insert("plann".to_string());
            expected.insert("plan".to_string());
            expected
        });
    }

    #[test]
    fn precomputed_overlap_graph_matches_pairwise_reference() {
        let texts = [
            "Schedule the quarterly planning review tomorrow",
            "Schedule the quarterly planning review tomorrow morning",
            " quarterly planning review tomorrow ",
            "A different durable finance fact",
            "abcd",
            "abcde",
            "Unicode caf\u{e9} planning reminder",
            "Unicode caf\u{e9} planning reminder with notes",
        ];
        let mut profiles = texts
            .iter()
            .map(|text| {
                let dedupe_text = text.trim();
                MemoryPromptCandidateProfile {
                    source_text: Arc::<str>::from(*text),
                    text_lower: text.to_lowercase(),
                    text_tokens: normalized_token_set(text),
                    fuzzy_ngrams: packed_char_ngrams(text),
                    exact_overlap_ngrams: Arc::from(exact_overlap_ngrams(dedupe_text)),
                    text_char_count: dedupe_text.chars().count(),
                    exact_overlap_indices: Arc::from(Vec::<usize>::new()),
                }
            })
            .collect::<Vec<_>>();
        populate_exact_overlap_graph(&mut profiles);

        for left in 0..profiles.len() {
            for right in 0..profiles.len() {
                if left == right {
                    continue;
                }
                let expected = exact_high_overlap(
                    profiles[left].source_text.trim(),
                    profiles[right].source_text.trim(),
                    profiles[left].text_char_count,
                    profiles[right].text_char_count,
                );
                assert_eq!(
                    profiles[left].exact_overlap_indices.contains(&right),
                    expected,
                    "overlap mismatch for {left} and {right}",
                );
            }
        }
    }

    #[test]
    fn bounded_cache_evicts_oldest_entry() {
        let mut cache = SnapshotCache::default();
        let settings = MagicianMemoryPromptSnapshotSettings {
            max_entries: 1,
            max_bytes: 1024,
            idle_ttl_secs: 600,
            ..MagicianMemoryPromptSnapshotSettings::default()
        };
        let snapshot = Arc::new(MemoryPromptCandidateSnapshot {
            documents: Arc::from(Vec::<MemoryCandidateDocument>::new()),
            profiles: Arc::from(Vec::<MemoryPromptCandidateProfile>::new()),
            source_generation: "test".to_string(),
            estimated_bytes: 128,
        });
        for (offset, agent_id) in ["one", "two"].into_iter().enumerate() {
            let key = SnapshotCacheKey {
                root: PathBuf::from("/tmp/test"),
                agent_id: agent_id.to_string(),
                scope: "agent",
                goal_id: None,
                include_environment_knowledge: false,
                isolated_user_memory: false,
                tier_definition_hash: [0; 32],
                include_prompt_profiles: true,
            };
            cache.estimated_bytes += snapshot.estimated_bytes();
            cache.entries.insert(
                key,
                SnapshotCacheEntry {
                    fingerprint: SourceFingerprint { files: Vec::new() },
                    content_hashes: Arc::default(),
                    snapshot: Arc::clone(&snapshot),
                    spec: SnapshotBuildSpec {
                        storage: AgentStorage::new(PathBuf::from("/tmp/test")),
                        agent_id: agent_id.to_string(),
                        tier_definitions: Vec::new(),
                        scope: TierScope::Agent,
                        goal_id: None,
                        include_environment_knowledge: false,
                        isolated_user_root: None,
                        include_prompt_profiles: true,
                    },
                    last_used: Instant::now() + Duration::from_millis(offset as u64),
                },
            );
            prune_cache(&mut cache, &settings, Instant::now());
        }
        assert_eq!(cache.entries.len(), 1);
        assert!(cache.entries.keys().any(|key| key.agent_id == "two"));
    }

    #[tokio::test]
    async fn memory_recall_prompt_snapshot_discovers_episode_and_tracks_source_repair() {
        let temp = tempfile::tempdir().unwrap();
        let storage = AgentStorage::new(temp.path());
        let spec = SnapshotBuildSpec {
            storage: storage.clone(),
            agent_id: "presto".to_string(),
            tier_definitions: Vec::new(),
            scope: TierScope::Agent,
            goal_id: None,
            include_environment_knowledge: false,
            isolated_user_root: None,
            include_prompt_profiles: true,
        };
        let before = source_fingerprint(&spec).await.unwrap();
        let dir = storage.agent_episodes_dir("presto").unwrap();
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let path = dir.join("conversation.json");
        let mut episode = serde_json::json!({"agent_id": "presto", "episode_id": "conversation",
            "goal_key": "chat:session", "completed_at": "2026-10-06T00:00:00Z",
            "observations": ["user_message_excerpt: I attended The Glass Menagerie."]});
        storage.write_json_atomic(&path, &episode).await.unwrap();
        let added = source_fingerprint(&spec).await.unwrap();
        assert_ne!(before, added);
        let service = AgentMemoryService::new(storage.clone());
        let request = MemoryCandidateRequest {
            scope: TierScope::Agent,
            goal_id: None,
            recency_cutoff: None,
            include_environment_knowledge: false,
            retrieval_scope: RetrievalScope::Unbound,
            isolated_user_root: None,
        };
        let snapshot = load_memory_prompt_candidate_snapshot(&service, "presto", &[], &request)
            .await
            .unwrap();
        assert_eq!(snapshot.documents.len(), 1);
        assert!(snapshot.documents[0].text.contains("The Glass Menagerie"));
        episode["observations"] =
            serde_json::json!(["user_message_excerpt: A corrected longer conversation."]);
        storage.write_json_atomic(&path, &episode).await.unwrap();
        assert_ne!(added, source_fingerprint(&spec).await.unwrap());
        let snapshot = load_memory_prompt_candidate_snapshot(&service, "presto", &[], &request)
            .await
            .unwrap();
        assert!(snapshot.documents[0].text.contains("corrected longer"));
    }

    #[tokio::test]
    async fn source_fingerprint_changes_when_canonical_file_changes() {
        let temp = tempfile::tempdir().expect("temp root");
        let storage = AgentStorage::new(temp.path());
        tokio::fs::create_dir_all(storage.user_root())
            .await
            .expect("create user root");
        tokio::fs::write(storage.user_knowledge_path(), b"{\"knowledge\":[]}")
            .await
            .expect("write first source");
        let spec = SnapshotBuildSpec {
            storage: storage.clone(),
            agent_id: "presto".to_string(),
            tier_definitions: Vec::new(),
            scope: TierScope::User,
            goal_id: None,
            include_environment_knowledge: false,
            isolated_user_root: None,
            include_prompt_profiles: true,
        };
        let before = source_fingerprint(&spec).await.expect("first fingerprint");
        tokio::fs::write(
            storage.user_knowledge_path(),
            b"{\"knowledge\":[{\"fact\":\"changed\"}]}",
        )
        .await
        .expect("replace source");
        let after = source_fingerprint(&spec).await.expect("second fingerprint");
        assert_ne!(before, after);
    }

    #[tokio::test]
    async fn prompt_snapshot_reuses_generation_and_invalidates_after_source_write() {
        let temp = tempfile::tempdir().expect("temp root");
        let storage = AgentStorage::new(temp.path());
        tokio::fs::create_dir_all(storage.user_root())
            .await
            .expect("create user root");
        tokio::fs::write(
            storage.user_knowledge_path(),
            b"{\"fields\":{\"facts\":[\"first fact\"]}}",
        )
        .await
        .expect("write first source");
        let service = AgentMemoryService::new(storage.clone());
        let request = MemoryCandidateRequest {
            scope: TierScope::User,
            goal_id: None,
            recency_cutoff: None,
            include_environment_knowledge: false,
            retrieval_scope: RetrievalScope::Unbound,
            isolated_user_root: None,
        };

        let first = load_memory_prompt_candidate_snapshot(&service, "presto", &[], &request)
            .await
            .expect("first snapshot");
        let reused = load_memory_prompt_candidate_snapshot(&service, "presto", &[], &request)
            .await
            .expect("reused snapshot");
        assert!(Arc::ptr_eq(&first, &reused));

        tokio::fs::write(
            storage.user_knowledge_path(),
            b"{\"fields\":{\"facts\":[\"second, longer fact\"]}}",
        )
        .await
        .expect("replace source");
        let refreshed = load_memory_prompt_candidate_snapshot(&service, "presto", &[], &request)
            .await
            .expect("refreshed snapshot");
        assert!(!Arc::ptr_eq(&first, &refreshed));
        assert!(refreshed
            .documents
            .iter()
            .any(|candidate| candidate.text.contains("second, longer fact")));
        evict_snapshots_for_root(temp.path());
    }

    #[tokio::test]
    async fn durable_change_notification_eagerly_replaces_warm_snapshot() {
        let temp = tempfile::tempdir().expect("temp root");
        let storage = AgentStorage::new(temp.path());
        tokio::fs::create_dir_all(storage.user_root())
            .await
            .expect("create user root");
        tokio::fs::write(
            storage.user_knowledge_path(),
            b"{\"fields\":{\"facts\":[\"first fact\"]}}",
        )
        .await
        .expect("write first source");
        let service = AgentMemoryService::new(storage.clone());
        let request = MemoryCandidateRequest {
            scope: TierScope::User,
            goal_id: None,
            recency_cutoff: None,
            include_environment_knowledge: false,
            retrieval_scope: RetrievalScope::Unbound,
            isolated_user_root: None,
        };
        let key =
            snapshot_cache_key(&storage, "presto", &[], &request, true).expect("snapshot key");
        let first = load_memory_prompt_candidate_snapshot(&service, "presto", &[], &request)
            .await
            .expect("first snapshot");

        // The refresh worker is a process singleton and `#[tokio::test]` gives
        // every test its own runtime, so `load_..` above may have found one
        // already running on a concurrent test's runtime -- which dies with
        // that test. A subscriber added afterwards cannot see a notification
        // already published, so claim the worker on this runtime while nothing
        // has been recorded yet.
        if let Ok(mut worker) = snapshot_refresh_worker().lock() {
            if let Some(handle) = worker.take() {
                handle.abort();
            }
        }
        ensure_snapshot_refresh_worker();

        tokio::fs::write(
            storage.user_knowledge_path(),
            b"{\"fields\":{\"facts\":[\"replacement fact from journal\"]}}",
        )
        .await
        .expect("replace source");
        magician_vector_index::memory_index::record_memory_index_change(
            &storage,
            MemoryIndexChange::UserKnowledge,
        )
        .await
        .expect("record durable source change");

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let replacement = snapshot_cache().lock().ok().and_then(|cache| {
                cache
                    .entries
                    .get(&key)
                    .map(|entry| Arc::clone(&entry.snapshot))
            });
            if replacement.as_ref().is_some_and(|snapshot| {
                !Arc::ptr_eq(&first, snapshot)
                    && snapshot
                        .documents
                        .iter()
                        .any(|candidate| candidate.text.contains("replacement fact from journal"))
            }) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "journal notification did not replace the warm prompt snapshot"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        evict_snapshots_for_root(temp.path());
    }

    async fn user_snapshot_fixture(
        root: &Path,
    ) -> (
        AgentMemoryService,
        MemoryCandidateRequest<'static>,
        SnapshotCacheKey,
    ) {
        let storage = AgentStorage::new(root);
        tokio::fs::create_dir_all(storage.user_root())
            .await
            .expect("create user root");
        tokio::fs::write(
            storage.user_knowledge_path(),
            b"{\"fields\":{\"facts\":[\"first fact\"]}}",
        )
        .await
        .expect("write source");
        let request = MemoryCandidateRequest {
            scope: TierScope::User,
            goal_id: None,
            recency_cutoff: None,
            include_environment_knowledge: false,
            retrieval_scope: RetrievalScope::Unbound,
            isolated_user_root: None,
        };
        let key =
            snapshot_cache_key(&storage, "presto", &[], &request, true).expect("snapshot key");
        (AgentMemoryService::new(storage), request, key)
    }

    /// A retrieval that gives up at its deadline drops the snapshot load. The
    /// build must still reach the cache, or the next lookup pays it again.
    /// Holding the build lock keeps the build waiting until its caller is gone.
    #[tokio::test]
    async fn a_snapshot_build_outlives_the_lookup_that_started_it() {
        let temp = tempfile::tempdir().expect("temp root");
        let (service, request, key) = user_snapshot_fixture(temp.path()).await;
        let build_lock = snapshot_build_lock(&key);
        let held = build_lock.lock().await;

        let caller = tokio::spawn(async move {
            load_memory_prompt_candidate_snapshot(&service, "presto", &[], &request).await
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        // The map and this test hold two references; the build holds the third.
        while Arc::strong_count(&build_lock) < 3 {
            assert!(
                Instant::now() < deadline,
                "the build never reached its lock"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        caller.abort();
        assert!(caller.await.is_err_and(|error| error.is_cancelled()));
        drop(held);

        while !snapshot_cache()
            .lock()
            .is_ok_and(|cache| cache.entries.contains_key(&key))
        {
            assert!(
                Instant::now() < deadline,
                "an abandoned lookup took its snapshot build with it"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        evict_snapshots_for_root(temp.path());
    }

    /// A change notification names a root, not an entry. An entry whose files
    /// did not change keeps its snapshot; a changed one is rebuilt without
    /// counting the rebuild as a use, so idle entries still expire.
    #[tokio::test]
    async fn background_refresh_rebuilds_only_changed_entries_and_keeps_their_last_use() {
        let temp = tempfile::tempdir().expect("temp root");
        let (service, request, key) = user_snapshot_fixture(temp.path()).await;
        let first = load_memory_prompt_candidate_snapshot(&service, "presto", &[], &request)
            .await
            .expect("first snapshot");
        let cached = || {
            snapshot_cache().lock().ok().and_then(|cache| {
                cache
                    .entries
                    .get(&key)
                    .map(|entry| (Arc::clone(&entry.snapshot), entry.last_used))
            })
        };
        let (_, last_used) = cached().expect("first snapshot is cached");

        refresh_snapshots_for_root(temp.path()).await;
        let (unchanged, _) = cached().expect("still cached");
        assert!(
            Arc::ptr_eq(&first, &unchanged),
            "an entry with unchanged sources was rebuilt"
        );

        tokio::fs::write(
            service.storage().user_knowledge_path(),
            b"{\"fields\":{\"facts\":[\"a changed fact\"]}}",
        )
        .await
        .expect("change source");
        refresh_snapshots_for_root(temp.path()).await;
        let (rebuilt, rebuilt_last_used) = cached().expect("rebuilt entry is cached");
        assert!(rebuilt
            .documents
            .iter()
            .any(|candidate| candidate.text.contains("a changed fact")));
        assert_eq!(rebuilt_last_used, last_used);
        evict_snapshots_for_root(temp.path());
    }

    fn agent_request() -> MemoryCandidateRequest<'static> {
        MemoryCandidateRequest {
            scope: TierScope::Agent,
            goal_id: None,
            recency_cutoff: None,
            include_environment_knowledge: false,
            retrieval_scope: RetrievalScope::Unbound,
            isolated_user_root: None,
        }
    }

    fn episode_json(id: &str, observations: &[&str]) -> serde_json::Value {
        serde_json::json!({"agent_id": "presto", "episode_id": id,
            "goal_key": "chat:session", "completed_at": "2026-10-06T00:00:00Z",
            "observations": observations})
    }

    /// New stamps on unchanged bytes are not a change: the snapshot object is
    /// kept, for a tier file and for an episode written through a temp file
    /// (which also moves the episodes directory's stamp).
    #[tokio::test]
    async fn rewriting_identical_bytes_keeps_the_snapshot() {
        let temp = tempfile::tempdir().expect("temp root");
        let (service, request, key) = user_snapshot_fixture(temp.path()).await;
        let first = load_memory_prompt_candidate_snapshot(&service, "presto", &[], &request)
            .await
            .expect("first snapshot");
        let stamped = snapshot_cache()
            .lock()
            .ok()
            .and_then(|cache| {
                cache
                    .entries
                    .get(&key)
                    .map(|entry| entry.fingerprint.clone())
            })
            .expect("cached");
        tokio::time::sleep(Duration::from_millis(20)).await;
        tokio::fs::write(
            service.storage().user_knowledge_path(),
            b"{\"fields\":{\"facts\":[\"first fact\"]}}",
        )
        .await
        .expect("rewrite identical bytes");
        let again = load_memory_prompt_candidate_snapshot(&service, "presto", &[], &request)
            .await
            .expect("second snapshot");
        assert!(
            Arc::ptr_eq(&first, &again),
            "a touch rebuilt the user snapshot"
        );
        let restamped = snapshot_cache()
            .lock()
            .ok()
            .and_then(|cache| {
                cache
                    .entries
                    .get(&key)
                    .map(|entry| entry.fingerprint.clone())
            })
            .expect("cached");
        assert_ne!(stamped, restamped, "the entry kept a stale stamp");

        let storage = service.storage().clone();
        let dir = storage.agent_episodes_dir("presto").expect("episodes dir");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create episodes dir");
        let episode = episode_json("one", &["user_message_excerpt: a walk by the river"]);
        storage
            .write_json_atomic(&dir.join("one.json"), &episode)
            .await
            .expect("write episode");
        let request = agent_request();
        let first = load_memory_prompt_candidate_snapshot(&service, "presto", &[], &request)
            .await
            .expect("agent snapshot");
        tokio::time::sleep(Duration::from_millis(20)).await;
        storage
            .write_json_atomic(&dir.join("one.json"), &episode)
            .await
            .expect("rewrite episode");
        let again = load_memory_prompt_candidate_snapshot(&service, "presto", &[], &request)
            .await
            .expect("agent snapshot again");
        assert!(
            Arc::ptr_eq(&first, &again),
            "a rewritten episode rebuilt the snapshot"
        );
        evict_snapshots_for_root(temp.path());
    }

    /// Reused episode and tier files, reused profiles and the incrementally
    /// updated overlap graph must give exactly what a full build gives: same
    /// documents, order, profiles and overlap links, after every add, change,
    /// delete and identical rewrite in a seeded random sequence. Shared
    /// sentences across episodes and the tier make the graph non-trivial.
    #[tokio::test]
    async fn incremental_rebuilds_match_a_full_build() {
        const SENTENCES: [&str; 6] = [
            "user_message_excerpt: we planned the quarterly offsite agenda together",
            "user_message_excerpt: the invoice for the design work is overdue",
            "user_message_excerpt: remember that Asha prefers morning meetings",
            "user_message_excerpt: we planned the quarterly offsite agenda together and booked rooms",
            "user_message_excerpt: a short note",
            "user_message_excerpt: the invoice for the design work is overdue since last week",
        ];
        let temp = tempfile::tempdir().expect("temp root");
        let storage = AgentStorage::new(temp.path());
        let service = AgentMemoryService::new(storage.clone());
        let dir = storage.agent_episodes_dir("presto").expect("episodes dir");
        tokio::fs::create_dir_all(&dir)
            .await
            .expect("create episodes dir");
        let request = agent_request();
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move |bound: u64| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) % bound
        };
        let tiers = crate::magician_v2::agents::default_memory_config_for_personal_agent()
            .0
            .into_iter()
            .filter(|tier| tier.name == "insights")
            .collect::<Vec<_>>();
        let tier_path = storage
            .agent_tier_path("presto", "insights", &TierScope::Agent, None)
            .expect("tier path");
        let mut tier_record = None::<serde_json::Value>;
        let mut present: Vec<(String, serde_json::Value)> = Vec::new();
        for step in 0..80 {
            match next(6) {
                4 => {
                    let patterns = (0..=next(3))
                        .map(|_| {
                            serde_json::json!({"pattern": SENTENCES[next(6) as usize],
                            "type": "habit", "confidence": "0.8"})
                        })
                        .collect::<Vec<_>>();
                    let record = serde_json::json!({"tier_name": "insights",
                        "tier_scope": "agent", "last_updated": "2026-10-06T00:00:00Z",
                        "fields": {"insights": patterns}});
                    storage
                        .write_json_atomic(&tier_path, &record)
                        .await
                        .expect("write tier");
                    tier_record = Some(record);
                },
                5 if tier_record.is_some() => {
                    storage
                        .write_json_atomic(&tier_path, tier_record.as_ref().expect("record"))
                        .await
                        .expect("rewrite identical tier");
                },
                0 | 1 if present.len() < 8 || present.is_empty() => {
                    let id = format!("episode-{step}");
                    let first = SENTENCES[next(6) as usize];
                    let second = SENTENCES[next(6) as usize];
                    let episode = episode_json(&id, &[first, second]);
                    storage
                        .write_json_atomic(&dir.join(format!("{id}.json")), &episode)
                        .await
                        .expect("add episode");
                    present.push((id, episode));
                },
                0 | 1 => {
                    let index = next(present.len() as u64) as usize;
                    let (id, episode) = &mut present[index];
                    *episode = episode_json(id, &[SENTENCES[next(6) as usize]]);
                    storage
                        .write_json_atomic(&dir.join(format!("{id}.json")), &*episode)
                        .await
                        .expect("change episode");
                },
                2 if !present.is_empty() => {
                    let index = next(present.len() as u64) as usize;
                    let (id, _) = present.remove(index);
                    tokio::fs::remove_file(dir.join(format!("{id}.json")))
                        .await
                        .expect("delete episode");
                },
                _ if !present.is_empty() => {
                    let index = next(present.len() as u64) as usize;
                    let (id, episode) = &present[index];
                    storage
                        .write_json_atomic(&dir.join(format!("{id}.json")), episode)
                        .await
                        .expect("rewrite identical episode");
                },
                _ => continue,
            }
            let cached =
                load_memory_prompt_candidate_snapshot(&service, "presto", &tiers, &request)
                    .await
                    .expect("cached snapshot");
            let full = build_uncached_snapshot(&storage, "presto", &tiers, &request, "full", true)
                .await
                .expect("full build");
            assert_eq!(
                cached.documents, full.documents,
                "documents differ at step {step}"
            );
            assert_eq!(
                format!("{:?}", cached.profiles),
                format!("{:?}", full.profiles),
                "profiles or overlap links differ at step {step}"
            );
        }
        evict_snapshots_for_root(temp.path());
    }

    /// The incremental overlap graph equals a full rebuild for random old and
    /// new text sets: kept, reordered, duplicated and new texts; containment
    /// near the 80% length bound; padding; short and empty texts.
    #[test]
    fn incremental_overlap_graph_matches_a_full_rebuild() {
        const BASES: [&str; 5] = [
            "alpha beta gamma delta epsilon",
            "the invoice for the design work is overdue",
            "remember Asha prefers morning meetings",
            "beta gamma delta",
            "quarterly offsite agenda",
        ];
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = move |bound: u64| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) % bound.max(1)
        };
        let text = |next: &mut dyn FnMut(u64) -> u64| -> String {
            let base = BASES[next(BASES.len() as u64) as usize];
            match next(8) {
                0 => base.to_string(),
                1 => format!("{base}!"),
                2 => format!("  {base} "),
                3 => base[..base.len() - 4].to_string(),
                4 => format!("{base} {}", BASES[next(BASES.len() as u64) as usize]),
                5 => ["", "ab", "abcdefg", "abcdefgh", " "][next(5) as usize].to_string(),
                6 => format!("{base} and more"),
                _ => format!("x{base}"),
            }
        };
        let profiles_for = |texts: &[String]| -> Vec<MemoryPromptCandidateProfile> {
            texts.iter().map(|text| candidate_profile(text)).collect()
        };
        let links = |profiles: &[MemoryPromptCandidateProfile]| -> Vec<Vec<usize>> {
            profiles
                .iter()
                .map(|profile| profile.exact_overlap_indices.to_vec())
                .collect()
        };
        for round in 0..400 {
            let previous_texts = (0..next(14)).map(|_| text(&mut next)).collect::<Vec<_>>();
            let mut previous = profiles_for(&previous_texts);
            populate_exact_overlap_graph(&mut previous);
            let mut current_texts = Vec::new();
            for _ in 0..next(16) {
                if !previous_texts.is_empty() && next(3) > 0 {
                    current_texts
                        .push(previous_texts[next(previous_texts.len() as u64) as usize].clone());
                } else {
                    current_texts.push(text(&mut next));
                }
            }
            let mut incremental = profiles_for(&current_texts);
            populate_exact_overlap_graph_from_previous(&mut incremental, &previous);
            let mut full = profiles_for(&current_texts);
            populate_exact_overlap_graph(&mut full);
            assert_eq!(
                links(&incremental),
                links(&full),
                "round {round}: {previous_texts:?} -> {current_texts:?}"
            );
        }
    }
}
