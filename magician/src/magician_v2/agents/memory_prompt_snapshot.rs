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
        load_memory_candidate_documents, MemoryCandidateDocument, MemoryCandidateRequest,
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
    tier_definition_hash: [u8; 32],
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
        }
    }
}

#[derive(Debug)]
struct SnapshotCacheEntry {
    fingerprint: SourceFingerprint,
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
        )
        .await;
    }

    let key = snapshot_cache_key(
        memory_service.storage(),
        agent_id,
        tier_definitions,
        request,
    )?;
    let spec = SnapshotBuildSpec {
        storage: memory_service.storage().clone(),
        agent_id: agent_id.to_string(),
        tier_definitions: tier_definitions.to_vec(),
        scope: request.scope.clone(),
        goal_id: request.goal_id.map(ToString::to_string),
        include_environment_knowledge: request.include_environment_knowledge,
    };
    let fingerprint = source_fingerprint(&spec).await?;
    if let Some(snapshot) = cached_snapshot(&key, &fingerprint, &settings) {
        CACHE_HITS.fetch_add(1, Ordering::Relaxed);
        return Ok(snapshot);
    }
    CACHE_MISSES.fetch_add(1, Ordering::Relaxed);

    let build_lock = snapshot_build_locks()
        .lock()
        .map(|mut locks| {
            Arc::clone(
                locks
                    .entry(key.clone())
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
            )
        })
        .unwrap_or_else(|_| Arc::new(tokio::sync::Mutex::new(())));
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
        let (fingerprint, snapshot) = build_consistent_snapshot(&spec, fingerprint).await?;
        CACHE_BUILDS.fetch_add(1, Ordering::Relaxed);
        insert_snapshot(key.clone(), fingerprint, snapshot.clone(), spec, &settings);
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
) -> Result<SnapshotCacheKey, MemoryStorageError> {
    let tier_json = serde_json::to_vec(tier_definitions).map_err(MemoryStorageError::Json)?;
    Ok(SnapshotCacheKey {
        root: storage.root().to_path_buf(),
        agent_id: agent_id.to_string(),
        scope: scope_label(&request.scope),
        goal_id: request.goal_id.map(ToString::to_string),
        include_environment_knowledge: request.include_environment_knowledge,
        tier_definition_hash: *blake3::hash(&tier_json).as_bytes(),
    })
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
) -> Result<Arc<MemoryPromptCandidateSnapshot>, MemoryStorageError> {
    let documents =
        load_memory_candidate_documents(storage, agent_id, tier_definitions, request).await?;
    Ok(Arc::new(build_snapshot(documents, source_generation)))
}

async fn build_consistent_snapshot(
    spec: &SnapshotBuildSpec,
    mut before: SourceFingerprint,
) -> Result<(SourceFingerprint, Arc<MemoryPromptCandidateSnapshot>), MemoryStorageError> {
    for _ in 0..3 {
        let documents = load_memory_candidate_documents(
            &spec.storage,
            &spec.agent_id,
            &spec.tier_definitions,
            &spec.request(),
        )
        .await?;
        let after = source_fingerprint(spec).await?;
        if before == after {
            let generation = fingerprint_generation(&after);
            return Ok((after, Arc::new(build_snapshot(documents, &generation))));
        }
        before = after;
        tokio::task::yield_now().await;
    }
    Err(MemoryStorageError::Other(
        "canonical memory kept changing while building prompt snapshot".to_string(),
    ))
}

fn build_snapshot(
    documents: Vec<MemoryCandidateDocument>,
    source_generation: &str,
) -> MemoryPromptCandidateSnapshot {
    let profiles = build_memory_prompt_candidate_profiles(&documents);
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
    let mut profiles = documents
        .iter()
        .map(|document| {
            let dedupe_text = document.text.trim();
            MemoryPromptCandidateProfile {
                source_text: Arc::<str>::from(document.text.clone()),
                text_lower: document.text.to_lowercase(),
                text_tokens: normalized_token_set(&document.text),
                fuzzy_ngrams: packed_char_ngrams(&document.text),
                exact_overlap_ngrams: Arc::from(exact_overlap_ngrams(dedupe_text)),
                text_char_count: dedupe_text.chars().count(),
                exact_overlap_indices: Arc::from(Vec::<usize>::new()),
            }
        })
        .collect::<Vec<_>>();
    populate_exact_overlap_graph(&mut profiles);
    profiles
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
    paths.sort();
    paths.dedup();
    let mut files = Vec::with_capacity(paths.len());
    for path in paths {
        let stamp = match tokio::fs::metadata(&path).await {
            Ok(metadata) => {
                let modified_nanos = metadata
                    .modified()
                    .ok()
                    .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_nanos());
                Some(SourceFileStamp {
                    len: metadata.len(),
                    modified_nanos,
                })
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(MemoryStorageError::Io(error)),
        };
        files.push((path, stamp));
    }
    Ok(SourceFingerprint { files })
}

fn source_paths(spec: &SnapshotBuildSpec) -> Result<Vec<PathBuf>, MemoryStorageError> {
    let mut paths = Vec::new();
    if matches!(spec.scope, TierScope::User) {
        paths.push(spec.storage.user_knowledge_path());
        for file_name in ["contacts.json", "routines.json", "research_findings.json"] {
            paths.push(spec.storage.user_root().join(file_name));
        }
    }
    for tier in spec
        .tier_definitions
        .iter()
        .filter(|tier| same_scope(&tier.scope, &spec.scope))
    {
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
    snapshot: Arc<MemoryPromptCandidateSnapshot>,
    spec: SnapshotBuildSpec,
    settings: &MagicianMemoryPromptSnapshotSettings,
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
            snapshot,
            spec,
            last_used: now,
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
        match build_consistent_snapshot(&spec, before).await {
            Ok((fingerprint, snapshot)) => {
                BACKGROUND_REFRESHES.fetch_add(1, Ordering::Relaxed);
                insert_snapshot(key, fingerprint, snapshot, spec, &settings);
            },
            Err(error) => warn!(
                root = %root.display(),
                error = %error,
                "failed to rebuild memory prompt snapshot in background"
            ),
        }
    }
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
                tier_definition_hash: [0; 32],
            };
            cache.estimated_bytes += snapshot.estimated_bytes();
            cache.entries.insert(
                key,
                SnapshotCacheEntry {
                    fingerprint: SourceFingerprint { files: Vec::new() },
                    snapshot: Arc::clone(&snapshot),
                    spec: SnapshotBuildSpec {
                        storage: AgentStorage::new(PathBuf::from("/tmp/test")),
                        agent_id: agent_id.to_string(),
                        tier_definitions: Vec::new(),
                        scope: TierScope::Agent,
                        goal_id: None,
                        include_environment_knowledge: false,
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
        };
        let key = snapshot_cache_key(&storage, "presto", &[], &request).expect("snapshot key");
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
}
