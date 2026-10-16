//! Periodic maintenance for the derived memory retrieval index.
//!
//! Memory tier JSON remains the source of truth. This worker only inspects
//! scoped derived indexes and incrementally reconciles compatible LanceDB/JSONL
//! state. Full replacement and compaction are explicit maintenance operations.

use std::{
    collections::{BTreeSet, HashMap},
    sync::{Mutex, OnceLock, RwLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    sync::mpsc,
    task::JoinHandle,
    time::{interval, MissedTickBehavior},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::magician_v2::{
    agents::{
        acknowledge_memory_index_changes, apply_memory_index_change_snapshot,
        inspect_scope_memory_index, memory_index_stale_reason_is_transient_lancedb,
        reconcile_scope_memory_index, snapshot_memory_index_changes, AgentDefinitionStore,
        AgentMemoryResolver, AgentStorage, AgentStorageError, MemoryIndexIncrementalUpdateOutcome,
        MemoryIndexIncrementalUpdateResult, MemoryIndexRebuildOutcome, MemoryLanceDbWriteReport,
    },
    analytics::memory_parquet::{emit_rows_for_storage, json_payload, MemoryAnalyticsRow},
    artifact_v2::workspace::ArtifactV2Workspace,
};

const MEMORY_INDEX_HYBRID_SUSPEND_AFTER_REPAIRABLE_ERROR: Duration = Duration::from_secs(300);
const MEMORY_INDEX_HYBRID_SUSPEND_AFTER_EMBEDDING_TIMEOUT: Duration = Duration::from_secs(300);
/// How often to touch the local embedding model so the OS keeps its weights
/// resident. Pinning it in Ollama (`embedding_keep_alive: "-1"`) stops Ollama
/// evicting it; it does not stop the kernel paging it out on a host under
/// memory pressure, and the fault is then paid by a foreground memory read that
/// has a five-second budget. Thirty seconds is well inside the window a 4 GB
/// resident set survives untouched, and one tiny embed at `Write` priority is
/// cheap enough that being generous costs nothing.
///
/// Measured on a 32 GB host, 2026-09-08, GPU otherwise idle: first query embed
/// after eight idle minutes was 3.108 s without this and 0.337 s with it, a
/// 9.2x difference against a 5 s foreground budget. Override the period with
/// `MAGICIAN_MEMORY_EMBEDDING_KEEP_RESIDENT_SECS`; a zero or unparseable value
/// falls back to this default rather than disabling the touch.
const MEMORY_EMBEDDING_KEEP_RESIDENT_INTERVAL: Duration = Duration::from_secs(30);
const MEMORY_INDEX_REBUILD_RETRY_INITIAL: Duration = Duration::from_secs(15 * 60);
const MEMORY_INDEX_REBUILD_RETRY_MAX: Duration = Duration::from_secs(60 * 60);
const MEMORY_INDEX_REBUILD_COOLDOWN_FILE: &str = "rebuild-cooldown.json";
/// A quiet-period debounce coalesces bursts, but continuous writes must not
/// postpone convergence indefinitely. The first-dirty ceiling is derived from
/// the configured debounce and clamped to this conservative range: ordinary
/// 15-second deployments do not rebuild a large scope every minute, while an
/// accidentally huge debounce still cannot starve maintenance beyond 30m.
const MEMORY_INDEX_DIRTY_MAX_WAIT_MIN: Duration = Duration::from_secs(15 * 60);
const MEMORY_INDEX_DIRTY_MAX_WAIT_MAX: Duration = Duration::from_secs(30 * 60);
const RETRIEVAL_FALLBACK_SUMMARY_INTERVAL: Duration = Duration::from_secs(5 * 60);
const RETRIEVAL_FALLBACK_EPISODE_CAP: usize = 1_024;

#[derive(Debug)]
pub struct MemoryIndexMaintainer {
    handle: JoinHandle<()>,
    cancel: CancellationToken,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DirtyMemoryIndexScope {
    principal: String,
    workspace: String,
}

#[derive(Debug, Clone)]
struct DirtyMemoryIndexEvent {
    scope: DirtyMemoryIndexScope,
    reason: String,
}

#[derive(Debug, Clone)]
struct DirtyMemoryIndexState {
    first_dirty_at: Instant,
    last_dirty_at: Instant,
    reasons: BTreeSet<String>,
    next_retry_at_unix_ms: Option<u64>,
}

#[derive(Debug, Clone)]
struct MemoryIndexHybridSuspension {
    until: Instant,
    reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RetrievalFallbackKey {
    component: &'static str,
    principal: String,
    workspace: String,
    actor: String,
}

#[derive(Debug, Clone)]
struct RetrievalFallbackEpisode {
    started_at: Instant,
    last_seen_at: Instant,
    last_logged_at: Instant,
    total_count: u64,
    suppressed_since_log: u64,
    last_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RetrievalFallbackObservation {
    Started,
    Suppressed,
    Summary {
        total_count: u64,
        occurrences_since_last_log: u64,
        duration_ms: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RetrievalFallbackRecovery {
    total_count: u64,
    duration_ms: u64,
    last_reason: String,
}

#[derive(Debug, Default)]
struct RetrievalFallbackTracker {
    episodes: HashMap<RetrievalFallbackKey, RetrievalFallbackEpisode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MemoryIndexRebuildCooldown {
    failure_count: u32,
    next_retry_at_unix_ms: u64,
    last_error: String,
}

impl MemoryIndexRebuildCooldown {
    fn is_active(&self, now_unix_ms: u64) -> bool {
        self.next_retry_at_unix_ms > now_unix_ms
    }

    fn backoff(&self) -> Duration {
        memory_index_rebuild_backoff(self.failure_count)
    }

    fn remaining_ms(&self, now_unix_ms: u64) -> u64 {
        self.next_retry_at_unix_ms.saturating_sub(now_unix_ms)
    }
}

#[derive(Debug, Clone, Copy)]
enum DirtyScopeRebuildOutcome {
    Complete,
    RetryAt(u64),
}

enum DirtyMemoryIndexUpdate {
    Reconciled(MemoryIndexRebuildOutcome),
    Incremental(MemoryIndexIncrementalUpdateOutcome),
}

#[derive(Debug, Clone)]
pub struct MemoryIndexMaintainerConfig {
    pub enabled: bool,
    pub interval: std::time::Duration,
    pub startup_delay: std::time::Duration,
    pub dirty_debounce: std::time::Duration,
}

impl Default for MemoryIndexMaintainerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval: std::time::Duration::from_secs(120),
            startup_delay: std::time::Duration::from_secs(15),
            dirty_debounce: std::time::Duration::from_secs(15),
        }
    }
}

impl MemoryIndexMaintainerConfig {
    pub fn from_env() -> Self {
        let mut config = Self::default();
        if let Ok(raw) = std::env::var("MAGICIAN_MEMORY_INDEX_MAINTAINER") {
            let raw = raw.trim().to_ascii_lowercase();
            config.enabled = !matches!(raw.as_str(), "0" | "false" | "off" | "disabled");
        }
        if let Some(interval) = read_positive_duration_env("MAGICIAN_MEMORY_INDEX_INTERVAL_SECS") {
            config.interval = interval;
        }
        if let Some(delay) = read_duration_env("MAGICIAN_MEMORY_INDEX_STARTUP_DELAY_SECS") {
            config.startup_delay = delay;
        }
        if let Some(debounce) = read_positive_duration_env("MAGICIAN_MEMORY_INDEX_DEBOUNCE_SECS") {
            config.dirty_debounce = debounce;
        }
        config
    }
}

fn format_error_chain(error: &anyhow::Error) -> String {
    format!("{error:#}")
}

static DIRTY_MEMORY_INDEX_SENDER: OnceLock<
    RwLock<Option<mpsc::UnboundedSender<DirtyMemoryIndexEvent>>>,
> = OnceLock::new();
static MEMORY_INDEX_HYBRID_SUSPENSIONS: OnceLock<
    RwLock<HashMap<DirtyMemoryIndexScope, MemoryIndexHybridSuspension>>,
> = OnceLock::new();
static RETRIEVAL_FALLBACK_TRACKER: OnceLock<Mutex<RetrievalFallbackTracker>> = OnceLock::new();

impl RetrievalFallbackTracker {
    fn observe_fallback(
        &mut self,
        key: RetrievalFallbackKey,
        reason: &str,
        now: Instant,
        summary_interval: Duration,
    ) -> RetrievalFallbackObservation {
        let reason = truncate_retrieval_fallback_reason(reason);
        if let Some(episode) = self.episodes.get_mut(&key) {
            episode.last_seen_at = now;
            episode.total_count = episode.total_count.saturating_add(1);
            episode.last_reason = reason;
            if now
                .checked_duration_since(episode.last_logged_at)
                .is_some_and(|elapsed| elapsed >= summary_interval)
            {
                let occurrences_since_last_log = episode.suppressed_since_log.saturating_add(1);
                episode.suppressed_since_log = 0;
                episode.last_logged_at = now;
                return RetrievalFallbackObservation::Summary {
                    total_count: episode.total_count,
                    occurrences_since_last_log,
                    duration_ms: duration_millis(now, episode.started_at),
                };
            }
            episode.suppressed_since_log = episode.suppressed_since_log.saturating_add(1);
            return RetrievalFallbackObservation::Suppressed;
        }

        // A permanently failing, dynamically named actor must not make the
        // process-wide observability registry itself grow without bound.
        if self.episodes.len() >= RETRIEVAL_FALLBACK_EPISODE_CAP {
            if let Some(oldest) = self
                .episodes
                .iter()
                .min_by_key(|(_, episode)| episode.last_seen_at)
                .map(|(key, _)| key.clone())
            {
                self.episodes.remove(&oldest);
            }
        }
        self.episodes.insert(
            key,
            RetrievalFallbackEpisode {
                started_at: now,
                last_seen_at: now,
                last_logged_at: now,
                total_count: 1,
                suppressed_since_log: 0,
                last_reason: reason,
            },
        );
        RetrievalFallbackObservation::Started
    }

    fn observe_recovery(
        &mut self,
        key: &RetrievalFallbackKey,
        now: Instant,
    ) -> Option<RetrievalFallbackRecovery> {
        self.episodes
            .remove(key)
            .map(|episode| RetrievalFallbackRecovery {
                total_count: episode.total_count,
                duration_ms: duration_millis(now, episode.started_at),
                last_reason: episode.last_reason,
            })
    }
}

/// Record a direct-fallback occurrence without emitting the same warning for
/// every prompt. Analytics rows remain per-request; logs expose the start of an
/// episode and a bounded periodic summary with suppressed occurrence counts.
pub fn note_retrieval_fallback_episode(
    component: &'static str,
    principal: &str,
    workspace: &str,
    actor: &str,
    reason: &str,
) {
    let key = retrieval_fallback_key(component, principal, workspace, actor);
    let observation = retrieval_fallback_tracker()
        .lock()
        .map(|mut tracker| {
            tracker.observe_fallback(
                key,
                reason,
                Instant::now(),
                RETRIEVAL_FALLBACK_SUMMARY_INTERVAL,
            )
        })
        .unwrap_or(RetrievalFallbackObservation::Started);
    match observation {
        RetrievalFallbackObservation::Started => warn!(
            target: "analytics::retrieval_fallback",
            component,
            principal,
            workspace,
            actor,
            reason = %truncate_retrieval_fallback_reason(reason),
            "derived retrieval entered direct-fallback mode"
        ),
        RetrievalFallbackObservation::Summary {
            total_count,
            occurrences_since_last_log,
            duration_ms,
        } => warn!(
            target: "analytics::retrieval_fallback",
            component,
            principal,
            workspace,
            actor,
            reason = %truncate_retrieval_fallback_reason(reason),
            total_count,
            occurrences_since_last_log,
            duration_ms,
            "derived retrieval remains in direct-fallback mode"
        ),
        RetrievalFallbackObservation::Suppressed => {},
    }
}

/// Close a prior fallback episode only after the same component/scope/actor
/// completes a real derived retrieval again.
pub fn note_retrieval_recovery_episode(
    component: &'static str,
    principal: &str,
    workspace: &str,
    actor: &str,
) {
    let key = retrieval_fallback_key(component, principal, workspace, actor);
    let recovery = retrieval_fallback_tracker()
        .lock()
        .ok()
        .and_then(|mut tracker| tracker.observe_recovery(&key, Instant::now()));
    if let Some(recovery) = recovery {
        info!(
            target: "analytics::retrieval_fallback",
            component,
            principal,
            workspace,
            actor,
            fallback_count = recovery.total_count,
            fallback_duration_ms = recovery.duration_ms,
            last_fallback_reason = %recovery.last_reason,
            "derived retrieval recovered from direct-fallback mode"
        );
    }
}

fn retrieval_fallback_key(
    component: &'static str,
    principal: &str,
    workspace: &str,
    actor: &str,
) -> RetrievalFallbackKey {
    RetrievalFallbackKey {
        component,
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        actor: actor.to_string(),
    }
}

fn retrieval_fallback_tracker() -> &'static Mutex<RetrievalFallbackTracker> {
    RETRIEVAL_FALLBACK_TRACKER.get_or_init(|| Mutex::new(RetrievalFallbackTracker::default()))
}

fn truncate_retrieval_fallback_reason(reason: &str) -> String {
    reason.chars().take(500).collect()
}

fn duration_millis(later: Instant, earlier: Instant) -> u64 {
    later
        .checked_duration_since(earlier)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

/// Best-effort dirty notification for scoped memory-index maintenance.
///
/// The canonical memory files remain authoritative. This only asks the
/// in-process maintainer to rebuild sooner than the periodic repair loop.
pub fn mark_memory_index_dirty_for_scope(
    principal: impl Into<String>,
    workspace: impl Into<String>,
    reason: impl Into<String>,
) {
    let principal = principal.into();
    let workspace = workspace.into();
    if principal.trim().is_empty() || workspace.trim().is_empty() {
        return;
    }
    let event = DirtyMemoryIndexEvent {
        scope: DirtyMemoryIndexScope {
            principal,
            workspace,
        },
        reason: normalize_dirty_reason(reason.into()),
    };
    let Some(sender) = current_dirty_sender() else {
        return;
    };
    let _ = sender.send(event);
}

pub fn mark_memory_index_dirty_for_storage(storage: &AgentStorage, reason: impl Into<String>) {
    let Some((principal, workspace)) = storage.scope_segments() else {
        return;
    };
    mark_memory_index_dirty_for_scope(principal, workspace, reason);
}

pub fn memory_index_hybrid_suspension_reason_for_storage(storage: &AgentStorage) -> Option<String> {
    let scope = scope_for_storage(storage)?;
    let now = Instant::now();
    let mut guard = memory_index_hybrid_suspensions().write().ok()?;
    let suspension = guard.get(&scope).cloned()?;
    if suspension.until <= now {
        guard.remove(&scope);
        return None;
    }
    let remaining_secs = suspension.until.duration_since(now).as_secs().max(1);
    Some(format!("{}:{remaining_secs}s_remaining", suspension.reason))
}

pub fn note_memory_index_retrieval_error_for_storage(
    storage: &AgentStorage,
    error_text: &str,
) -> bool {
    if memory_index_retrieval_error_is_transient(error_text) {
        let Some(scope) = scope_for_storage(storage) else {
            return true;
        };
        suspend_memory_index_hybrid_for_scope(
            &scope,
            "hybrid_index_temporarily_disabled_after_lancedb_timeout",
            MEMORY_INDEX_HYBRID_SUSPEND_AFTER_REPAIRABLE_ERROR,
        );
        return true;
    }
    if !memory_index_retrieval_error_is_repairable(error_text) {
        return false;
    }
    let Some(scope) = scope_for_storage(storage) else {
        return true;
    };
    mark_memory_index_dirty_for_scope(
        scope.principal.clone(),
        scope.workspace.clone(),
        "lancedb_retrieval_error",
    );
    suspend_memory_index_hybrid_for_scope(
        &scope,
        "hybrid_index_temporarily_disabled_after_repairable_lancedb_error",
        MEMORY_INDEX_HYBRID_SUSPEND_AFTER_REPAIRABLE_ERROR,
    );
    true
}

pub fn note_memory_index_embedding_unavailable_for_storage(
    storage: &AgentStorage,
    error_text: &str,
) -> bool {
    if !memory_index_embedding_error_is_transient(error_text) {
        return false;
    }
    let Some(scope) = scope_for_storage(storage) else {
        return true;
    };
    suspend_memory_index_hybrid_for_scope(
        &scope,
        "hybrid_index_temporarily_disabled_after_ollama_embedding_timeout",
        MEMORY_INDEX_HYBRID_SUSPEND_AFTER_EMBEDDING_TIMEOUT,
    );
    true
}

fn suspend_memory_index_hybrid_for_scope(
    scope: &DirtyMemoryIndexScope,
    reason: &str,
    duration: Duration,
) {
    suspend_memory_index_hybrid_until(scope, reason, Instant::now() + duration);
}

fn suspend_memory_index_hybrid_until(
    scope: &DirtyMemoryIndexScope,
    reason: &str,
    suspend_until: Instant,
) {
    if let Ok(mut guard) = memory_index_hybrid_suspensions().write() {
        let previous = guard.insert(
            scope.clone(),
            MemoryIndexHybridSuspension {
                until: suspend_until,
                reason: reason.to_string(),
            },
        );
        if previous
            .map(|suspension| suspension.until <= Instant::now())
            .unwrap_or(true)
        {
            info!(
                target: "analytics::memory_index_maintainer",
                principal = %scope.principal,
                workspace = %scope.workspace,
                suspend_secs = suspend_until
                    .checked_duration_since(Instant::now())
                    .map(|duration| duration.as_secs().max(1))
                    .unwrap_or(0),
                reason = %reason,
                "temporarily disabled LanceDB memory retrieval"
            );
        }
    }
}

pub fn memory_index_retrieval_error_is_repairable(error_text: &str) -> bool {
    let lower = error_text.to_ascii_lowercase();
    if memory_index_retrieval_error_is_transient(&lower) {
        return false;
    }
    if lower.contains("embedding memory query for lancedb hybrid search")
        && lower.contains("ollama")
    {
        return false;
    }
    lower.contains("failed to fill whole buffer")
        || lower.contains("lanceerror(io)")
        || lower.contains("generic localfilesystem error")
        || lower.contains("lancedb memory index unhealthy")
        || lower.contains("no such table")
        || (lower.contains("memory candidate table") && lower.contains("not found"))
        || (lower.contains("lancedb memory") && lower.contains("not found"))
}

fn memory_index_retrieval_error_is_transient(error_text: &str) -> bool {
    let lower = error_text.to_ascii_lowercase();
    lower.contains("lancedb_health_check_timed_out")
        || (lower.contains("timed out") && lower.contains("querying lancedb memory"))
        || (lower.contains("timeout") && lower.contains("querying lancedb memory"))
}

fn memory_index_embedding_error_is_transient(error_text: &str) -> bool {
    let lower = error_text.to_ascii_lowercase();
    if !lower.contains("embedding memory query for lancedb hybrid search")
        || !lower.contains("ollama")
    {
        return false;
    }
    if lower.contains("401")
        || lower.contains("403")
        || lower.contains("404")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("not found")
        || lower.contains("model not found")
    {
        return false;
    }
    lower.contains("timed out")
        || lower.contains("timeout")
        || contains_status_code(&lower, "429")
        || lower.contains("too many requests")
        || lower.contains("rate limit")
        || contains_status_code(&lower, "500")
        || contains_status_code(&lower, "502")
        || contains_status_code(&lower, "503")
        || contains_status_code(&lower, "504")
        || lower.contains("5xx")
        || lower.contains("error sending request")
        || lower.contains("connection refused")
        || lower.contains("connect error")
        || lower.contains("dns")
        || lower.contains("connection reset")
        || lower.contains("connection closed")
        || lower.contains("connection aborted")
        || lower.contains("broken pipe")
        || lower.contains("internal server error")
        || lower.contains("bad gateway")
        || lower.contains("service unavailable")
        || lower.contains("gateway timeout")
        || lower.contains("server error")
}

fn contains_status_code(text: &str, code: &str) -> bool {
    text.match_indices(code).any(|(idx, _)| {
        let before = text[..idx]
            .chars()
            .next_back()
            .is_none_or(|ch| !ch.is_ascii_digit());
        let after = text[idx + code.len()..]
            .chars()
            .next()
            .is_none_or(|ch| !ch.is_ascii_digit());
        before && after
    })
}

fn scope_for_storage(storage: &AgentStorage) -> Option<DirtyMemoryIndexScope> {
    let (principal, workspace) = storage.scope_segments()?;
    Some(DirtyMemoryIndexScope {
        principal,
        workspace,
    })
}

fn clear_memory_index_hybrid_suspension(scope: &DirtyMemoryIndexScope) {
    if let Ok(mut guard) = memory_index_hybrid_suspensions().write() {
        guard.remove(scope);
    }
}

fn memory_index_rebuild_cooldown_path(storage: &AgentStorage) -> std::path::PathBuf {
    storage
        .memory_index_dir()
        .join(MEMORY_INDEX_REBUILD_COOLDOWN_FILE)
}

async fn active_memory_index_rebuild_cooldown(
    storage: &AgentStorage,
    scope: &DirtyMemoryIndexScope,
) -> Option<MemoryIndexRebuildCooldown> {
    let cooldown = load_memory_index_rebuild_cooldown(storage, scope).await?;
    if cooldown.is_active(unix_epoch_millis_now()) {
        Some(cooldown)
    } else {
        // An expired record leaves the circuit half-open. Keeping its failure
        // count means a failed retry backs off further; success deletes it.
        None
    }
}

async fn load_memory_index_rebuild_cooldown(
    storage: &AgentStorage,
    scope: &DirtyMemoryIndexScope,
) -> Option<MemoryIndexRebuildCooldown> {
    let path = memory_index_rebuild_cooldown_path(storage);
    let bytes = match storage.read_bytes(&path).await {
        Ok(bytes) => bytes,
        Err(AgentStorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return None;
        },
        Err(error) => {
            warn!(
                target: "analytics::memory_index_maintainer",
                principal = %scope.principal,
                workspace = %scope.workspace,
                cooldown_path = %path.display(),
                error = %error,
                "failed to load persisted memory index rebuild cooldown"
            );
            return None;
        },
    };
    match serde_json::from_slice::<MemoryIndexRebuildCooldown>(&bytes) {
        Ok(cooldown) if cooldown.failure_count > 0 && cooldown.next_retry_at_unix_ms > 0 => {
            Some(cooldown)
        },
        Ok(_) => {
            warn!(
                target: "analytics::memory_index_maintainer",
                principal = %scope.principal,
                workspace = %scope.workspace,
                cooldown_path = %path.display(),
                "discarding invalid persisted memory index rebuild cooldown"
            );
            let _ = storage.remove_file(&path).await;
            None
        },
        Err(error) => {
            warn!(
                target: "analytics::memory_index_maintainer",
                principal = %scope.principal,
                workspace = %scope.workspace,
                cooldown_path = %path.display(),
                error = %error,
                "discarding unreadable persisted memory index rebuild cooldown"
            );
            let _ = storage.remove_file(&path).await;
            None
        },
    }
}

async fn record_memory_index_rebuild_failure(
    storage: &AgentStorage,
    scope: &DirtyMemoryIndexScope,
    error: &str,
) -> Option<MemoryIndexRebuildCooldown> {
    let failure_count = load_memory_index_rebuild_cooldown(storage, scope)
        .await
        .map(|cooldown| cooldown.failure_count.saturating_add(1))
        .unwrap_or(1);
    let backoff = memory_index_rebuild_backoff(failure_count);
    let cooldown = MemoryIndexRebuildCooldown {
        failure_count,
        next_retry_at_unix_ms: unix_epoch_millis_after(backoff),
        last_error: truncate_memory_index_rebuild_error(error),
    };
    // Keep the in-process retry even when persistence is unavailable. Dropping
    // the dirty scope here would leave a known failed source update stranded
    // until a later unrelated write or the periodic audit.
    let _persisted = persist_memory_index_rebuild_cooldown(storage, scope, &cooldown).await;
    Some(cooldown)
}

async fn persist_memory_index_rebuild_cooldown(
    storage: &AgentStorage,
    scope: &DirtyMemoryIndexScope,
    cooldown: &MemoryIndexRebuildCooldown,
) -> bool {
    let path = memory_index_rebuild_cooldown_path(storage);
    if let Err(error) = storage.write_json_atomic(&path, cooldown).await {
        warn!(
            target: "analytics::memory_index_maintainer",
            principal = %scope.principal,
            workspace = %scope.workspace,
            cooldown_path = %path.display(),
            error = %error,
            "failed to atomically persist memory index rebuild cooldown"
        );
        return false;
    }
    true
}

async fn clear_memory_index_rebuild_cooldown(
    storage: &AgentStorage,
    scope: &DirtyMemoryIndexScope,
) {
    let path = memory_index_rebuild_cooldown_path(storage);
    if let Err(error) = storage.remove_file(&path).await {
        warn!(
            target: "analytics::memory_index_maintainer",
            principal = %scope.principal,
            workspace = %scope.workspace,
            cooldown_path = %path.display(),
            error = %error,
            "failed to clear persisted memory index rebuild cooldown after success"
        );
    }
}

fn memory_index_rebuild_backoff(failure_count: u32) -> Duration {
    let exponent = failure_count.saturating_sub(1).min(63);
    let multiplier = 1_u64.checked_shl(exponent).unwrap_or(u64::MAX);
    Duration::from_secs(
        MEMORY_INDEX_REBUILD_RETRY_INITIAL
            .as_secs()
            .saturating_mul(multiplier)
            .min(MEMORY_INDEX_REBUILD_RETRY_MAX.as_secs()),
    )
}

fn unix_epoch_millis_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn unix_epoch_millis_after(duration: Duration) -> u64 {
    unix_epoch_millis_now().saturating_add(duration.as_millis().min(u64::MAX as u128) as u64)
}

fn truncate_memory_index_rebuild_error(error: &str) -> String {
    error.chars().take(1_000).collect()
}

fn suspend_hybrid_for_memory_index_rebuild_cooldown(
    scope: &DirtyMemoryIndexScope,
    cooldown: &MemoryIndexRebuildCooldown,
    error: &str,
) {
    let lower = error.to_ascii_lowercase();
    let reason =
        if lower.contains("ollama") && (lower.contains("timed out") || lower.contains("timeout")) {
            "hybrid_index_temporarily_disabled_after_ollama_embedding_timeout"
        } else {
            "hybrid_index_temporarily_disabled_after_rebuild_failure"
        };
    suspend_memory_index_hybrid_for_scope(
        scope,
        reason,
        Duration::from_millis(cooldown.remaining_ms(unix_epoch_millis_now())),
    );
}

fn memory_index_hybrid_suspensions(
) -> &'static RwLock<HashMap<DirtyMemoryIndexScope, MemoryIndexHybridSuspension>> {
    MEMORY_INDEX_HYBRID_SUSPENSIONS.get_or_init(|| RwLock::new(HashMap::new()))
}

impl MemoryIndexMaintainer {
    pub fn spawn(
        workspace_layout: ArtifactV2Workspace,
        definition_store: AgentDefinitionStore,
        memory_resolver: AgentMemoryResolver,
        config: MemoryIndexMaintainerConfig,
        runtime_handle: tokio::runtime::Handle,
    ) -> Self {
        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();
        let (dirty_tx, dirty_rx) = mpsc::unbounded_channel();
        register_dirty_sender(Some(dirty_tx));
        let handle = runtime_handle.spawn(async move {
            if !config.enabled {
                info!(target: "analytics::memory_index_maintainer", "memory index maintainer disabled");
                register_dirty_sender(None);
                return;
            }
            run_periodic(
                workspace_layout,
                definition_store,
                memory_resolver,
                config,
                cancel_for_task,
                dirty_rx,
            )
            .await;
            register_dirty_sender(None);
        });
        Self { handle, cancel }
    }

    pub async fn shutdown(self) {
        self.cancel.cancel();
        let _ = self.handle.await;
    }
}

async fn run_periodic(
    workspace_layout: ArtifactV2Workspace,
    definition_store: AgentDefinitionStore,
    memory_resolver: AgentMemoryResolver,
    config: MemoryIndexMaintainerConfig,
    cancel: CancellationToken,
    mut dirty_rx: mpsc::UnboundedReceiver<DirtyMemoryIndexEvent>,
) {
    if !crate::magician_v2::runtime::startup::wait_for_http_or_cancel(&cancel).await {
        return;
    }

    if !config.startup_delay.is_zero() {
        tokio::select! {
            _ = tokio::time::sleep(config.startup_delay) => {},
            _ = cancel.cancelled() => return,
        }
    }

    {
        let Some(_startup_permit) =
            crate::magician_v2::runtime::startup::admit_backfill_or_cancel(&cancel).await
        else {
            return;
        };
        run_once_with_logging(&workspace_layout, &definition_store, &memory_resolver).await;
    }

    let mut pending_dirty = HashMap::<DirtyMemoryIndexScope, DirtyMemoryIndexState>::new();
    let periodic_interval = non_zero_or_default(config.interval, Duration::from_secs(120));
    let mut periodic = interval(periodic_interval);
    periodic.set_missed_tick_behavior(MissedTickBehavior::Delay);
    periodic.tick().await;
    let mut dirty_check = interval(Duration::from_secs(1));
    dirty_check.set_missed_tick_behavior(MissedTickBehavior::Delay);
    dirty_check.tick().await;
    let keep_resident_interval =
        read_positive_duration_env("MAGICIAN_MEMORY_EMBEDDING_KEEP_RESIDENT_SECS")
            .unwrap_or(MEMORY_EMBEDDING_KEEP_RESIDENT_INTERVAL);
    let mut keep_resident = interval(non_zero_or_default(
        keep_resident_interval,
        MEMORY_EMBEDDING_KEEP_RESIDENT_INTERVAL,
    ));
    keep_resident.set_missed_tick_behavior(MissedTickBehavior::Delay);
    keep_resident.tick().await;
    let mut dirty_channel_open = true;

    loop {
        tokio::select! {
            maybe_event = dirty_rx.recv(), if dirty_channel_open => {
                match maybe_event {
                    Some(event) => record_dirty_event(&mut pending_dirty, event),
                    None => dirty_channel_open = false,
                }
            }
            _ = dirty_check.tick() => {
                rebuild_due_dirty_scopes(
                    &workspace_layout,
                    &definition_store,
                    &memory_resolver,
                    &mut pending_dirty,
                    config.dirty_debounce,
                ).await;
            }
            _ = periodic.tick() => {
                run_once_with_logging(&workspace_layout, &definition_store, &memory_resolver).await;
            }
            _ = keep_resident.tick() => {
                touch_embedding_model_for_residency().await;
            }
            _ = cancel.cancelled() => break,
        }
    }
}

/// Keep the local embedding model's weights in physical memory.
///
/// Deliberately quiet: this runs every thirty seconds forever, so a success is
/// not worth a line and a failure is not worth a warning — the provider may
/// simply be a remote one, or down, and the next attempt is thirty seconds
/// away. The signal that matters is the foreground one already logged when a
/// query embedding misses its deadline.
async fn touch_embedding_model_for_residency() {
    match magician_vector_index::keep_embedding_model_resident().await {
        Ok(touched) => debug!(
            target: "memory_index",
            touched,
            "touched the local embedding model to keep it resident"
        ),
        Err(error) => debug!(
            target: "memory_index",
            error = %error,
            "embedding residency touch did not reach the provider"
        ),
    }
}

async fn run_once_with_logging(
    workspace_layout: &ArtifactV2Workspace,
    definition_store: &AgentDefinitionStore,
    memory_resolver: &AgentMemoryResolver,
) {
    match run_once(workspace_layout, definition_store, memory_resolver).await {
        Ok(outcome) => {
            // The maintainer runs on a timer and, in steady state, finds nothing
            // stale (rebuilt == 0, failed == 0). Logging that at INFO every tick
            // is pure noise, so only surface a tick that actually did something:
            // WARN only when work failed or was deferred. A successful rebuild
            // is operationally useful but healthy, so report it at INFO.
            if outcome.scopes_failed > 0 || outcome.scopes_skipped_cooldown > 0 {
                warn!(
                    target: "analytics::memory_index_maintainer",
                    scopes_checked = outcome.scopes_checked,
                    scopes_rebuilt = outcome.scopes_rebuilt,
                    scopes_failed = outcome.scopes_failed,
                    scopes_skipped_cooldown = outcome.scopes_skipped_cooldown,
                    scopes_quiesced_empty = outcome.scopes_quiesced_empty,
                    scopes_ignored_ephemeral_eval = outcome.scopes_ignored_ephemeral_eval,
                    "memory index maintainer completed"
                );
            } else if outcome.scopes_rebuilt > 0 {
                info!(
                    target: "analytics::memory_index_maintainer",
                    scopes_checked = outcome.scopes_checked,
                    scopes_rebuilt = outcome.scopes_rebuilt,
                    scopes_quiesced_empty = outcome.scopes_quiesced_empty,
                    scopes_ignored_ephemeral_eval = outcome.scopes_ignored_ephemeral_eval,
                    "memory index maintainer completed"
                );
            } else {
                debug!(
                    target: "analytics::memory_index_maintainer",
                    scopes_checked = outcome.scopes_checked,
                    scopes_rebuilt = outcome.scopes_rebuilt,
                    scopes_failed = outcome.scopes_failed,
                    scopes_skipped_cooldown = outcome.scopes_skipped_cooldown,
                    scopes_quiesced_empty = outcome.scopes_quiesced_empty,
                    scopes_ignored_ephemeral_eval = outcome.scopes_ignored_ephemeral_eval,
                    "memory index maintainer completed (no changes)"
                );
            }
        },
        Err(error) => {
            warn!(
                target: "analytics::memory_index_maintainer",
                error = %error,
                "memory index maintainer failed"
            );
        },
    }
}

#[derive(Debug, Default)]
struct MemoryIndexMaintenanceOutcome {
    scopes_checked: usize,
    scopes_rebuilt: usize,
    scopes_failed: usize,
    scopes_skipped_cooldown: usize,
    scopes_quiesced_empty: usize,
    scopes_ignored_ephemeral_eval: usize,
}

#[derive(Debug)]
struct EmptyMemoryIndexSettlement {
    stale_reason: String,
    cleared_cooldown: bool,
}

async fn run_once(
    workspace_layout: &ArtifactV2Workspace,
    definition_store: &AgentDefinitionStore,
    memory_resolver: &AgentMemoryResolver,
) -> Result<MemoryIndexMaintenanceOutcome> {
    let scopes = workspace_layout
        .list_scope_segments()
        .await
        .context("listing scoped workspaces for memory index maintenance")?;
    let mut outcome = MemoryIndexMaintenanceOutcome::default();
    for (principal, workspace) in scopes {
        if !scope_has_memory_root(workspace_layout, &principal, &workspace).await {
            continue;
        }
        if scope_is_ephemeral_eval(&principal) {
            // Live-eval scripts intentionally own these isolated scopes. The
            // long-lived production maintainer must not adopt their residual
            // journals/cooldowns after a script exits, nor race an active eval.
            outcome.scopes_ignored_ephemeral_eval += 1;
            continue;
        }
        outcome.scopes_checked += 1;
        let scope = DirtyMemoryIndexScope {
            principal: principal.clone(),
            workspace: workspace.clone(),
        };
        let scoped_store = definition_store.for_scope(&principal, &workspace);
        let memory_service = match memory_resolver.resolve_for_scope(&principal, &workspace) {
            Ok(service) => service,
            Err(error) => {
                outcome.scopes_failed += 1;
                warn!(
                    target: "analytics::memory_index_maintainer",
                    principal = %principal,
                    workspace = %workspace,
                    error = %error,
                    "failed to resolve scoped memory service"
                );
                continue;
            },
        };
        // Empty canonical scopes may retain a cooldown from an older failed
        // reconciliation. Once full canonical inspection proves the scope is
        // still empty, clear only that stale circuit state. Do not consume its
        // journal or leave the maintenance path: normal reconciliation below
        // must create stable empty derived metadata, then acknowledge the exact
        // journal snapshot only after that write succeeds.
        match settle_empty_memory_index_scope(memory_service.storage(), &scoped_store, &scope).await
        {
            Ok(Some(settlement)) => {
                outcome.scopes_quiesced_empty += 1;
                if settlement.cleared_cooldown {
                    info!(
                        target: "analytics::memory_index_maintainer",
                        principal = %principal,
                        workspace = %workspace,
                        stale_reason = %settlement.stale_reason,
                        cleared_cooldown = settlement.cleared_cooldown,
                        "cleared stale memory-index circuit state for a canonically empty scope; continuing normal reconciliation"
                    );
                }
            },
            Ok(None) => {},
            Err(error) => debug!(
                target: "analytics::memory_index_maintainer",
                principal = %principal,
                workspace = %workspace,
                error = %format_error_chain(&error),
                "empty-scope preflight was inconclusive; continuing normal index maintenance"
            ),
        }
        if let Some(cooldown) =
            active_memory_index_rebuild_cooldown(memory_service.storage(), &scope).await
        {
            suspend_hybrid_for_memory_index_rebuild_cooldown(
                &scope,
                &cooldown,
                &cooldown.last_error,
            );
            outcome.scopes_skipped_cooldown += 1;
            emit_maintenance_row(
                memory_service.storage(),
                "memory_index_rebuild_skipped",
                "cooldown",
                None,
                Some("rebuild_cooldown".to_string()),
                None,
                None,
                None,
                None,
                None,
                Some(&cooldown),
            );
            warn!(
                target: "analytics::memory_index_maintainer",
                principal = %principal,
                workspace = %workspace,
                failure_count = cooldown.failure_count,
                backoff_ms = cooldown.backoff().as_millis() as u64,
                next_retry_at_unix_ms = cooldown.next_retry_at_unix_ms,
                next_retry_in_ms = cooldown.remaining_ms(unix_epoch_millis_now()),
                "skipping memory index rebuild during per-scope cooldown"
            );
            continue;
        }
        let mut journal_snapshot_for_full = None;
        let mut journal_full_reason = None;
        match snapshot_memory_index_changes(memory_service.storage()).await {
            Ok(snapshot) if !snapshot.is_empty() => {
                emit_maintenance_row(
                    memory_service.storage(),
                    "memory_index_incremental_update_started",
                    "started",
                    None,
                    Some("startup_pending_source_journal".to_string()),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                );
                let started = Instant::now();
                match apply_memory_index_change_snapshot(
                    memory_service.storage(),
                    &scoped_store,
                    &snapshot,
                )
                .await
                {
                    Ok(MemoryIndexIncrementalUpdateResult::Applied(update)) => {
                        let duration_ms = started.elapsed().as_millis() as u64;
                        if let Err(error) =
                            acknowledge_memory_index_changes(memory_service.storage(), &snapshot)
                                .await
                        {
                            warn!(
                                target: "analytics::memory_index_maintainer",
                                principal = %principal,
                                workspace = %workspace,
                                error = %format_error_chain(&error),
                                "startup incremental memory index update succeeded but journal acknowledgement failed; it will be safely retried"
                            );
                        }
                        clear_memory_index_hybrid_suspension(&scope);
                        clear_memory_index_rebuild_cooldown(memory_service.storage(), &scope).await;
                        outcome.scopes_rebuilt += 1;
                        emit_maintenance_row(
                            memory_service.storage(),
                            "memory_index_incremental_update_completed",
                            "ok",
                            Some(update.manifest.document_count),
                            Some("startup_pending_source_journal".to_string()),
                            Some(&update.manifest),
                            Some(duration_ms),
                            None,
                            Some(&update.lancedb_write),
                            None,
                            None,
                        );
                        emit_memory_index_lancedb_write_rows(
                            memory_service.storage(),
                            &update.lancedb_write,
                            Some(duration_ms),
                            Some("startup_pending_source_journal"),
                        );
                        record_memory_index_embedding(
                            &principal,
                            &workspace,
                            &update.manifest,
                            &update.lancedb_write,
                            duration_ms,
                        );
                        info!(
                            target: "analytics::memory_index_maintainer",
                            principal = %principal,
                            workspace = %workspace,
                            document_count = update.manifest.document_count,
                            source_count = update.manifest.source_count,
                            changed_source_count = update.changed_source_count,
                            duration_ms = duration_ms,
                            "applied pending incremental memory index update at startup"
                        );
                        continue;
                    },
                    Ok(MemoryIndexIncrementalUpdateResult::FullRebuildRequired { reason }) => {
                        journal_snapshot_for_full = Some(snapshot);
                        journal_full_reason =
                            Some(format!("startup_journal_reconcile_fallback:{reason}"));
                    },
                    Ok(MemoryIndexIncrementalUpdateResult::NoChanges) => {
                        if let Err(error) =
                            acknowledge_memory_index_changes(memory_service.storage(), &snapshot)
                                .await
                        {
                            warn!(
                                target: "analytics::memory_index_maintainer",
                                principal = %principal,
                                workspace = %workspace,
                                error = %format_error_chain(&error),
                                "startup no-op memory index journal acknowledgement failed; it will be safely retried"
                            );
                        }
                        continue;
                    },
                    Err(error) => {
                        let duration_ms = started.elapsed().as_millis() as u64;
                        let error_chain = format_error_chain(&error);
                        let cooldown = record_memory_index_rebuild_failure(
                            memory_service.storage(),
                            &scope,
                            &error_chain,
                        )
                        .await;
                        if let Some(cooldown) = cooldown.as_ref() {
                            suspend_hybrid_for_memory_index_rebuild_cooldown(
                                &scope,
                                cooldown,
                                &error_chain,
                            );
                        }
                        outcome.scopes_failed += 1;
                        emit_maintenance_row(
                            memory_service.storage(),
                            "memory_index_incremental_update_failed",
                            "failed",
                            None,
                            Some("startup_pending_source_journal".to_string()),
                            None,
                            Some(duration_ms),
                            None,
                            None,
                            Some(error_chain.clone()),
                            cooldown.as_ref(),
                        );
                        warn!(
                            target: "analytics::memory_index_maintainer",
                            principal = %principal,
                            workspace = %workspace,
                            error = %error_chain,
                            "failed to apply pending incremental memory index update at startup"
                        );
                        schedule_memory_index_retry(&scope, "incremental_update_retry");
                        continue;
                    },
                }
            },
            Ok(_) => {},
            Err(error) => {
                warn!(
                    target: "analytics::memory_index_maintainer",
                    principal = %principal,
                    workspace = %workspace,
                    error = %format_error_chain(&error),
                    "failed to read pending memory index journal; continuing with periodic audit"
                );
            },
        }

        // A structural journal entry already established that a whole-source
        // reconciliation is required, so avoid an extra inspection first.
        // Scopes without a journal still receive the periodic
        // audit needed to detect out-of-band file edits.
        let status = if journal_full_reason.is_some() {
            None
        } else {
            match inspect_scope_memory_index(memory_service.storage(), &scoped_store).await {
                Ok(status) => Some(status),
                Err(error) => {
                    outcome.scopes_failed += 1;
                    emit_maintenance_row(
                        memory_service.storage(),
                        "memory_index_reconcile_failed",
                        "failed",
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        Some(error.to_string()),
                        None,
                    );
                    warn!(
                        target: "analytics::memory_index_maintainer",
                        principal = %principal,
                        workspace = %workspace,
                        error = %error,
                        "failed to inspect memory index"
                    );
                    schedule_memory_index_retry(&scope, "memory_index_inspect_retry");
                    continue;
                },
            }
        };
        if status.as_ref().is_some_and(|status| !status.stale) {
            continue;
        }
        if journal_full_reason.is_none()
            && status.as_ref().is_some_and(|status| {
                memory_index_stale_reason_is_transient_lancedb(&status.reason)
            })
        {
            let status = status.as_ref().expect("checked above");
            suspend_memory_index_hybrid_for_scope(
                &scope,
                "hybrid_index_temporarily_disabled_after_lancedb_health_timeout",
                MEMORY_INDEX_HYBRID_SUSPEND_AFTER_REPAIRABLE_ERROR,
            );
            emit_maintenance_row(
                memory_service.storage(),
                "memory_index_rebuild_skipped",
                "skipped",
                Some(status.current_document_count),
                Some(status.reason.clone()),
                status.manifest.as_ref(),
                None,
                None,
                None,
                None,
                None,
            );
            warn!(
                target: "analytics::memory_index_maintainer",
                principal = %principal,
                workspace = %workspace,
                reason = %status.reason,
                "skipping memory index rebuild for transient LanceDB unavailability"
            );
            continue;
        }

        let rebuild_reason = journal_full_reason
            .clone()
            .or_else(|| status.as_ref().map(|status| status.reason.clone()))
            .unwrap_or_else(|| "startup_full_rebuild".to_string());
        let current_document_count = status.as_ref().map(|status| status.current_document_count);
        let existing_manifest = status.as_ref().and_then(|status| status.manifest.as_ref());

        emit_maintenance_row(
            memory_service.storage(),
            "memory_index_reconcile_started",
            "started",
            current_document_count,
            Some(rebuild_reason.clone()),
            existing_manifest,
            None,
            None,
            None,
            None,
            None,
        );
        let started = std::time::Instant::now();
        match reconcile_scope_memory_index(memory_service.storage(), &scoped_store).await {
            Ok(rebuild) => {
                let duration_ms = started.elapsed().as_millis() as u64;
                if let Some(snapshot) = journal_snapshot_for_full.as_ref() {
                    if let Err(error) =
                        acknowledge_memory_index_changes(memory_service.storage(), snapshot).await
                    {
                        warn!(
                            target: "analytics::memory_index_maintainer",
                            principal = %principal,
                            workspace = %workspace,
                            error = %format_error_chain(&error),
                            "startup full memory index rebuild succeeded but journal acknowledgement failed; it will be safely retried"
                        );
                    }
                }
                clear_memory_index_hybrid_suspension(&scope);
                clear_memory_index_rebuild_cooldown(memory_service.storage(), &scope).await;
                outcome.scopes_rebuilt += 1;
                emit_maintenance_row(
                    memory_service.storage(),
                    "memory_index_reconcile_completed",
                    "ok",
                    Some(rebuild.manifest.document_count),
                    Some(rebuild_reason.clone()),
                    Some(&rebuild.manifest),
                    Some(duration_ms),
                    None,
                    Some(&rebuild.lancedb_write),
                    None,
                    None,
                );
                emit_memory_index_lancedb_write_rows(
                    memory_service.storage(),
                    &rebuild.lancedb_write,
                    Some(duration_ms),
                    Some(rebuild_reason.as_str()),
                );
                record_memory_index_embedding(
                    &principal,
                    &workspace,
                    &rebuild.manifest,
                    &rebuild.lancedb_write,
                    duration_ms,
                );
                info!(
                    target: "analytics::memory_index_maintainer",
                    principal = %principal,
                    workspace = %workspace,
                    document_count = rebuild.manifest.document_count,
                    source_count = rebuild.manifest.source_count,
                    embedding_provider = %rebuild.manifest.embedding_provider,
                    embedding_model = ?rebuild.manifest.embedding_model,
                    duration_ms = duration_ms,
                    "reconciled stale memory index without replacement"
                );
            },
            Err(error) => {
                let duration_ms = started.elapsed().as_millis() as u64;
                let error_chain = format_error_chain(&error);
                let cooldown = record_memory_index_rebuild_failure(
                    memory_service.storage(),
                    &scope,
                    &error_chain,
                )
                .await;
                if let Some(cooldown) = cooldown.as_ref() {
                    suspend_hybrid_for_memory_index_rebuild_cooldown(
                        &scope,
                        cooldown,
                        &error_chain,
                    );
                }
                outcome.scopes_failed += 1;
                emit_maintenance_row(
                    memory_service.storage(),
                    "memory_index_reconcile_failed",
                    "failed",
                    current_document_count,
                    Some(rebuild_reason),
                    existing_manifest,
                    Some(duration_ms),
                    None,
                    None,
                    Some(error_chain.clone()),
                    cooldown.as_ref(),
                );
                warn!(
                    target: "analytics::memory_index_maintainer",
                    principal = %principal,
                    workspace = %workspace,
                    error = %error_chain,
                    failure_count = cooldown.as_ref().map(|value| value.failure_count),
                    backoff_ms = cooldown.as_ref().map(|value| value.backoff().as_millis() as u64),
                    next_retry_at_unix_ms = cooldown.as_ref().map(|value| value.next_retry_at_unix_ms),
                    "failed to reconcile stale memory index without replacement"
                );
                schedule_memory_index_retry(&scope, "memory_index_rebuild_retry");
            },
        }
    }
    Ok(outcome)
}

async fn settle_empty_memory_index_scope(
    storage: &AgentStorage,
    definition_store: &AgentDefinitionStore,
    scope: &DirtyMemoryIndexScope,
) -> Result<Option<EmptyMemoryIndexSettlement>> {
    if !derived_memory_index_may_be_empty_or_uninitialized(storage).await? {
        return Ok(None);
    }

    let status = inspect_scope_memory_index(storage, definition_store).await?;
    if status.current_document_count != 0 {
        return Ok(None);
    }

    let had_cooldown = load_memory_index_rebuild_cooldown(storage, scope)
        .await
        .is_some();
    clear_memory_index_rebuild_cooldown(storage, scope).await;
    clear_memory_index_hybrid_suspension(scope);
    Ok(Some(EmptyMemoryIndexSettlement {
        stale_reason: status.reason,
        cleared_cooldown: had_cooldown,
    }))
}

async fn derived_memory_index_may_be_empty_or_uninitialized(
    storage: &AgentStorage,
) -> Result<bool> {
    let manifest_path = storage.memory_index_manifest_path();
    let bytes = match storage.read_bytes(&manifest_path).await {
        Ok(bytes) => bytes,
        Err(AgentStorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(true);
        },
        Err(error) => return Err(error.into()),
    };
    let Ok(manifest) =
        serde_json::from_slice::<magician_vector_index::memory_index::MemoryIndexManifest>(&bytes)
    else {
        // A malformed manifest is not an empty-scope signal. The normal audit
        // path must surface it rather than silently discarding repair state.
        return Ok(false);
    };
    Ok(manifest.document_count == 0)
}

async fn rebuild_due_dirty_scopes(
    workspace_layout: &ArtifactV2Workspace,
    definition_store: &AgentDefinitionStore,
    memory_resolver: &AgentMemoryResolver,
    pending_dirty: &mut HashMap<DirtyMemoryIndexScope, DirtyMemoryIndexState>,
    debounce: Duration,
) {
    if pending_dirty.is_empty() {
        return;
    }
    let now = Instant::now();
    let now_unix_ms = unix_epoch_millis_now();
    let debounce = non_zero_or_default(debounce, Duration::from_secs(15));
    let max_wait = bounded_dirty_memory_index_max_wait(debounce);
    let due = pending_dirty
        .iter()
        .filter_map(|(scope, state)| {
            if dirty_memory_index_state_is_due(state, now, now_unix_ms, debounce, max_wait) {
                Some(scope.clone())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    for scope in due {
        let Some(state) = pending_dirty.get(&scope).cloned() else {
            continue;
        };
        match rebuild_dirty_scope(
            workspace_layout,
            definition_store,
            memory_resolver,
            &scope,
            state,
        )
        .await
        {
            DirtyScopeRebuildOutcome::Complete => {
                pending_dirty.remove(&scope);
            },
            DirtyScopeRebuildOutcome::RetryAt(next_retry_at_unix_ms) => {
                if let Some(state) = pending_dirty.get_mut(&scope) {
                    state.next_retry_at_unix_ms = Some(next_retry_at_unix_ms);
                }
            },
        }
    }
}

fn bounded_dirty_memory_index_max_wait(quiet_debounce: Duration) -> Duration {
    quiet_debounce
        .max(MEMORY_INDEX_DIRTY_MAX_WAIT_MIN)
        .min(MEMORY_INDEX_DIRTY_MAX_WAIT_MAX)
}

fn dirty_memory_index_state_is_due(
    state: &DirtyMemoryIndexState,
    now: Instant,
    now_unix_ms: u64,
    quiet_debounce: Duration,
    max_wait: Duration,
) -> bool {
    if state
        .next_retry_at_unix_ms
        .is_some_and(|next_retry_at_unix_ms| next_retry_at_unix_ms > now_unix_ms)
    {
        return false;
    }
    let quiet_for = now
        .checked_duration_since(state.last_dirty_at)
        .unwrap_or_default();
    let dirty_for = now
        .checked_duration_since(state.first_dirty_at)
        .unwrap_or_default();
    quiet_for >= quiet_debounce || dirty_for >= max_wait
}

async fn rebuild_dirty_scope(
    workspace_layout: &ArtifactV2Workspace,
    definition_store: &AgentDefinitionStore,
    memory_resolver: &AgentMemoryResolver,
    scope: &DirtyMemoryIndexScope,
    state: DirtyMemoryIndexState,
) -> DirtyScopeRebuildOutcome {
    if !scope_has_memory_root(workspace_layout, &scope.principal, &scope.workspace).await {
        return DirtyScopeRebuildOutcome::Complete;
    }
    if scope_is_ephemeral_eval(&scope.principal) {
        return DirtyScopeRebuildOutcome::Complete;
    }
    let scoped_store = definition_store.for_scope(&scope.principal, &scope.workspace);
    let memory_service = match memory_resolver.resolve_for_scope(&scope.principal, &scope.workspace)
    {
        Ok(service) => service,
        Err(error) => {
            warn!(
                target: "analytics::memory_index_maintainer",
                principal = %scope.principal,
                workspace = %scope.workspace,
                error = %error,
                "failed to resolve dirty scoped memory service"
            );
            return DirtyScopeRebuildOutcome::Complete;
        },
    };
    if let Some(cooldown) =
        active_memory_index_rebuild_cooldown(memory_service.storage(), scope).await
    {
        suspend_hybrid_for_memory_index_rebuild_cooldown(scope, &cooldown, &cooldown.last_error);
        emit_maintenance_row(
            memory_service.storage(),
            "memory_index_rebuild_skipped",
            "cooldown",
            None,
            Some("rebuild_cooldown".to_string()),
            None,
            None,
            Some(&state),
            None,
            None,
            Some(&cooldown),
        );
        warn!(
            target: "analytics::memory_index_maintainer",
            principal = %scope.principal,
            workspace = %scope.workspace,
            dirty_reasons = ?state.reasons,
            failure_count = cooldown.failure_count,
            backoff_ms = cooldown.backoff().as_millis() as u64,
            next_retry_at_unix_ms = cooldown.next_retry_at_unix_ms,
            next_retry_in_ms = cooldown.remaining_ms(unix_epoch_millis_now()),
            "skipping dirty memory index rebuild during per-scope cooldown"
        );
        return DirtyScopeRebuildOutcome::RetryAt(cooldown.next_retry_at_unix_ms);
    }
    let mut stale_reason = format!(
        "dirty:{}",
        state.reasons.iter().cloned().collect::<Vec<_>>().join(",")
    );
    let started = Instant::now();
    emit_maintenance_row(
        memory_service.storage(),
        "memory_index_reconcile_started",
        "started",
        None,
        Some(stale_reason.clone()),
        None,
        None,
        Some(&state),
        None,
        None,
        None,
    );
    let journal_snapshot = match snapshot_memory_index_changes(memory_service.storage()).await {
        Ok(snapshot) => Some(snapshot),
        Err(error) => {
            warn!(
                target: "analytics::memory_index_maintainer",
                principal = %scope.principal,
                workspace = %scope.workspace,
                error = %format_error_chain(&error),
                "failed to read incremental memory index journal; using merge-only source reconciliation"
            );
            None
        },
    };
    let update_result = if let Some(snapshot) = journal_snapshot
        .as_ref()
        .filter(|snapshot| !snapshot.is_empty())
    {
        match apply_memory_index_change_snapshot(memory_service.storage(), &scoped_store, snapshot)
            .await
        {
            Ok(MemoryIndexIncrementalUpdateResult::Applied(update)) => {
                stale_reason.push_str(":incremental");
                Ok(DirtyMemoryIndexUpdate::Incremental(update))
            },
            Ok(MemoryIndexIncrementalUpdateResult::FullRebuildRequired { reason }) => {
                stale_reason.push_str(&format!(":reconcile_fallback:{reason}"));
                reconcile_scope_memory_index(memory_service.storage(), &scoped_store)
                    .await
                    .map(DirtyMemoryIndexUpdate::Reconciled)
            },
            Ok(MemoryIndexIncrementalUpdateResult::NoChanges) => {
                stale_reason.push_str(":reconcile_audit");
                reconcile_scope_memory_index(memory_service.storage(), &scoped_store)
                    .await
                    .map(DirtyMemoryIndexUpdate::Reconciled)
            },
            Err(error) => Err(error),
        }
    } else {
        // Legacy callers and external writers may still emit only a dirty
        // notification. Audit all canonical sources, but permit row-level merge
        // only; incompatible state stays disabled until explicit maintenance.
        stale_reason.push_str(":reconcile_audit");
        reconcile_scope_memory_index(memory_service.storage(), &scoped_store)
            .await
            .map(DirtyMemoryIndexUpdate::Reconciled)
    };
    match update_result {
        Ok(update) => {
            let duration_ms = started.elapsed().as_millis() as u64;
            let completed_reason = stale_reason.clone();
            let (manifest, lancedb_write, changed_source_count, event_kind, update_kind) =
                match update {
                    DirtyMemoryIndexUpdate::Reconciled(rebuild) => (
                        rebuild.manifest,
                        rebuild.lancedb_write,
                        None,
                        "memory_index_reconcile_completed",
                        "reconciled dirty memory index without replacement",
                    ),
                    DirtyMemoryIndexUpdate::Incremental(update) => (
                        update.manifest,
                        update.lancedb_write,
                        Some(update.changed_source_count),
                        "memory_index_incremental_update_completed",
                        "applied incremental dirty memory index update",
                    ),
                };
            if let Some(snapshot) = journal_snapshot.as_ref() {
                if let Err(error) =
                    acknowledge_memory_index_changes(memory_service.storage(), snapshot).await
                {
                    warn!(
                        target: "analytics::memory_index_maintainer",
                        principal = %scope.principal,
                        workspace = %scope.workspace,
                        error = %format_error_chain(&error),
                        "memory index update succeeded but journal acknowledgement failed; it will be safely retried"
                    );
                }
            }
            clear_memory_index_hybrid_suspension(scope);
            clear_memory_index_rebuild_cooldown(memory_service.storage(), scope).await;
            emit_maintenance_row(
                memory_service.storage(),
                event_kind,
                "ok",
                Some(manifest.document_count),
                Some(completed_reason.clone()),
                Some(&manifest),
                Some(duration_ms),
                Some(&state),
                Some(&lancedb_write),
                None,
                None,
            );
            emit_memory_index_lancedb_write_rows(
                memory_service.storage(),
                &lancedb_write,
                Some(duration_ms),
                Some(completed_reason.as_str()),
            );
            record_memory_index_embedding(
                &scope.principal,
                &scope.workspace,
                &manifest,
                &lancedb_write,
                duration_ms,
            );
            info!(
                target: "analytics::memory_index_maintainer",
                principal = %scope.principal,
                workspace = %scope.workspace,
                document_count = manifest.document_count,
                source_count = manifest.source_count,
                changed_source_count,
                embedding_provider = %manifest.embedding_provider,
                embedding_model = ?manifest.embedding_model,
                duration_ms = duration_ms,
                dirty_reasons = ?state.reasons,
                update_kind = update_kind,
                "memory index dirty update completed"
            );
            DirtyScopeRebuildOutcome::Complete
        },
        Err(error) => {
            let duration_ms = started.elapsed().as_millis() as u64;
            let error_chain = format_error_chain(&error);
            let cooldown =
                record_memory_index_rebuild_failure(memory_service.storage(), scope, &error_chain)
                    .await;
            if let Some(cooldown) = cooldown.as_ref() {
                suspend_hybrid_for_memory_index_rebuild_cooldown(scope, cooldown, &error_chain);
            }
            emit_maintenance_row(
                memory_service.storage(),
                "memory_index_reconcile_failed",
                "failed",
                None,
                Some(stale_reason),
                None,
                Some(duration_ms),
                Some(&state),
                None,
                Some(error_chain.clone()),
                cooldown.as_ref(),
            );
            warn!(
                target: "analytics::memory_index_maintainer",
                principal = %scope.principal,
                workspace = %scope.workspace,
                error = %error_chain,
                dirty_reasons = ?state.reasons,
                failure_count = cooldown.as_ref().map(|value| value.failure_count),
                backoff_ms = cooldown.as_ref().map(|value| value.backoff().as_millis() as u64),
                next_retry_at_unix_ms = cooldown.as_ref().map(|value| value.next_retry_at_unix_ms),
                "failed to reconcile dirty memory index without replacement"
            );
            cooldown
                .map(|cooldown| DirtyScopeRebuildOutcome::RetryAt(cooldown.next_retry_at_unix_ms))
                .unwrap_or(DirtyScopeRebuildOutcome::Complete)
        },
    }
}

fn scope_is_ephemeral_eval(principal: &str) -> bool {
    // These are explicit defaults owned by the checked-in live-eval scripts,
    // not a naming convention. Avoid suffix matching: a legitimate tenant may
    // choose a principal that happens to end in `-live-eval`.
    matches!(principal, "live-eval" | "storage-live-eval")
}

async fn scope_has_memory_root(
    workspace_layout: &ArtifactV2Workspace,
    principal: &str,
    workspace: &str,
) -> bool {
    fs::try_exists(workspace_layout.memory_root(principal, workspace))
        .await
        .unwrap_or(false)
}

/// Number of items actually embedded for a completed memory-index write.
///
/// Prefers the LanceDB write report's `embedded_rows` (chunks whose vectors
/// were obtained for this write, including cache hits) and falls back to the
/// report's total `chunk_count` when the finer count is unavailable. This is the
/// coarse "one record per write" batch size fed to the embeddings telemetry.
fn memory_index_embedded_batch_size(lancedb_write: &MemoryLanceDbWriteReport) -> usize {
    lancedb_write
        .embedded_rows
        .map(|rows| rows.min(usize::MAX as u64) as usize)
        .unwrap_or(lancedb_write.chunk_count)
}

/// Embedding model name for a completed memory-index write, or `"unknown"` when
/// the manifest did not record one (older manifests / fallback embedders).
fn memory_index_embedding_model(
    manifest: &magician_vector_index::memory_index::MemoryIndexManifest,
) -> &str {
    manifest.embedding_model.as_deref().unwrap_or("unknown")
}

/// Best-effort, non-blocking record of a completed memory-index embed batch into
/// the separate `llm_embeddings` telemetry dataset. A no-op when nothing was
/// embedded (`batch_size == 0`); never affects control flow or the reconcile.
fn record_memory_index_embedding(
    principal: &str,
    workspace: &str,
    manifest: &magician_vector_index::memory_index::MemoryIndexManifest,
    lancedb_write: &MemoryLanceDbWriteReport,
    duration_ms: u64,
) {
    record_memory_index_embedding_to(
        None,
        principal,
        workspace,
        manifest,
        lancedb_write,
        duration_ms,
    );
}

fn record_memory_index_embedding_to(
    sink: Option<&crate::magician_v2::analytics::llm_embeddings_sink::LlmEmbeddingsSink>,
    principal: &str,
    workspace: &str,
    manifest: &magician_vector_index::memory_index::MemoryIndexManifest,
    lancedb_write: &MemoryLanceDbWriteReport,
    duration_ms: u64,
) {
    let batch_size = memory_index_embedded_batch_size(lancedb_write);
    if batch_size == 0 {
        return;
    }
    let model = memory_index_embedding_model(manifest);
    // The maintainer holds counts but not the embedded texts (they are produced
    // inside the vector-index crate), so no length estimate is available here:
    // record 0 rather than fabricating a token count.
    if let Some(sink) = sink {
        crate::magician_v2::analytics::llm_embeddings_sink::record_embedding_batch_to(
            sink,
            principal,
            workspace,
            model,
            "memory_index",
            batch_size,
            0,
            duration_ms,
            true,
        );
    } else {
        crate::magician_v2::analytics::llm_embeddings_sink::record_embedding_batch(
            principal,
            workspace,
            model,
            "memory_index",
            batch_size,
            0,
            duration_ms,
            true,
        );
    }
}

fn emit_maintenance_row(
    storage: &crate::magician_v2::agents::storage::AgentStorage,
    event_kind: &str,
    status: &str,
    document_count: Option<usize>,
    stale_reason: Option<String>,
    manifest: Option<&magician_vector_index::memory_index::MemoryIndexManifest>,
    duration_ms: Option<u64>,
    dirty_state: Option<&DirtyMemoryIndexState>,
    lancedb_write: Option<&MemoryLanceDbWriteReport>,
    error: Option<String>,
    rebuild_cooldown: Option<&MemoryIndexRebuildCooldown>,
) {
    let mut row = MemoryAnalyticsRow::now(event_kind, "memory_index_maintainer");
    row.status = status.to_string();
    row.candidate_count = document_count.map(|count| count.min(u32::MAX as usize) as u32);
    row.retrieval_backend = Some("lancedb_hybrid".to_string());
    row.payload_json = json_payload(&serde_json::json!({
        "stale_reason": stale_reason,
        "provider": manifest.map(|manifest| manifest.embedding_provider.as_str()),
        "model": manifest.and_then(|manifest| manifest.embedding_model.as_deref()),
        "dims": manifest.map(|manifest| manifest.embedding_dimensions),
        "embedding_fallback_reason": manifest
            .and_then(|manifest| manifest.embedding_fallback_reason.as_deref()),
        "doc_count": document_count,
        "duration_ms": duration_ms,
        "dirty": dirty_state.map(|state| serde_json::json!({
            "reason_count": state.reasons.len(),
            "reasons": state.reasons.iter().cloned().collect::<Vec<_>>(),
            "debounced_ms": state.last_dirty_at
                .checked_duration_since(state.first_dirty_at)
                .map(|duration| duration.as_millis() as u64)
                .unwrap_or(0),
        })),
        "lancedb_write": lancedb_write,
        "error": error,
        "rebuild_cooldown": rebuild_cooldown.map(|cooldown| serde_json::json!({
            "failure_count": cooldown.failure_count,
            "backoff_ms": cooldown.backoff().as_millis().min(u64::MAX as u128) as u64,
            "next_retry_at_unix_ms": cooldown.next_retry_at_unix_ms,
            "next_retry_in_ms": cooldown.remaining_ms(unix_epoch_millis_now()),
            "last_error": cooldown.last_error.as_str(),
        })),
    }));
    emit_rows_for_storage(storage, vec![row]);
}

pub fn emit_memory_index_lancedb_write_rows(
    storage: &crate::magician_v2::agents::storage::AgentStorage,
    report: &MemoryLanceDbWriteReport,
    duration_ms: Option<u64>,
    stale_reason: Option<&str>,
) {
    let mut rows = Vec::new();
    let mut write_row = MemoryAnalyticsRow::now(
        match report.mode.as_str() {
            "merge" => "memory_index_lancedb_rows_updated",
            "source_delta" => "memory_index_lancedb_source_delta_updated",
            "replace" => "memory_index_lancedb_replaced",
            "empty" => "memory_index_lancedb_emptied",
            _ => "memory_index_lancedb_written",
        },
        "memory_index_maintainer",
    );
    write_row.status = "ok".to_string();
    write_row.retrieval_backend = Some("lancedb_hybrid".to_string());
    write_row.source_kind = Some(report.mode.clone());
    let changed_rows = report
        .inserted_rows
        .unwrap_or(0)
        .saturating_add(report.updated_rows.unwrap_or(0))
        .saturating_add(report.deleted_rows.unwrap_or(0));
    let write_input_rows = match report.mode.as_str() {
        "merge" | "source_delta" => changed_rows,
        _ => report.chunk_count as u64,
    };
    write_row.input_count = Some(write_input_rows.min(u32::MAX as u64) as u32);
    write_row.output_count = Some(report.chunk_count.min(u32::MAX as usize) as u32);
    write_row.candidate_count = Some(report.chunk_count.min(u32::MAX as usize) as u32);
    write_row.payload_json = json_payload(&serde_json::json!({
        "stale_reason": stale_reason,
        "duration_ms": duration_ms,
        "mode": report.mode.as_str(),
        "chunk_count": report.chunk_count,
        "source_rows": report.source_rows,
        "embedded_rows": report.embedded_rows,
        "reused_rows": report.reused_rows,
        "inserted_rows": report.inserted_rows,
        "updated_rows": report.updated_rows,
        "deleted_rows": report.deleted_rows,
        "replacement_reason": report.replacement_reason.as_deref(),
        "optimize": &report.optimize,
    }));
    rows.push(write_row);

    if report.optimize.attempted {
        let mut optimize_row = MemoryAnalyticsRow::now(
            if report.optimize.completed {
                "memory_index_lancedb_optimize_completed"
            } else {
                "memory_index_lancedb_optimize_failed"
            },
            "memory_index_maintainer",
        );
        optimize_row.status = if report.optimize.completed {
            "ok".to_string()
        } else {
            "failed".to_string()
        };
        optimize_row.retrieval_backend = Some("lancedb_hybrid".to_string());
        optimize_row.source_kind = Some(report.mode.clone());
        optimize_row.input_count = Some(report.optimize.mutated_rows.min(u32::MAX as u64) as u32);
        optimize_row.output_count = Some(report.optimize.current_rows.min(u32::MAX as u64) as u32);
        optimize_row.payload_json = json_payload(&serde_json::json!({
            "stale_reason": stale_reason,
            "duration_ms": duration_ms,
            "mode": report.mode.as_str(),
            "optimize": &report.optimize,
        }));
        rows.push(optimize_row);
    }

    emit_rows_for_storage(storage, rows);
}

fn record_dirty_event(
    pending_dirty: &mut HashMap<DirtyMemoryIndexScope, DirtyMemoryIndexState>,
    event: DirtyMemoryIndexEvent,
) {
    let now = Instant::now();
    pending_dirty
        .entry(event.scope)
        .and_modify(|state| {
            state.last_dirty_at = now;
            state.reasons.insert(event.reason.clone());
        })
        .or_insert_with(|| DirtyMemoryIndexState {
            first_dirty_at: now,
            last_dirty_at: now,
            reasons: BTreeSet::from([event.reason]),
            next_retry_at_unix_ms: None,
        });
}

/// `run_once` follows the long audit interval, while a failed source update
/// needs a prompt retry once its per-scope cooldown expires. Enqueue it through
/// the dirty loop instead of waiting for the next audit; the persisted cooldown
/// remains the authority that prevents retry storms.
fn schedule_memory_index_retry(scope: &DirtyMemoryIndexScope, reason: &'static str) {
    mark_memory_index_dirty_for_scope(scope.principal.clone(), scope.workspace.clone(), reason);
}

fn current_dirty_sender() -> Option<mpsc::UnboundedSender<DirtyMemoryIndexEvent>> {
    DIRTY_MEMORY_INDEX_SENDER
        .get()
        .and_then(|slot| slot.read().ok().and_then(|guard| guard.clone()))
}

fn register_dirty_sender(sender: Option<mpsc::UnboundedSender<DirtyMemoryIndexEvent>>) {
    let slot = DIRTY_MEMORY_INDEX_SENDER.get_or_init(|| RwLock::new(None));
    if let Ok(mut guard) = slot.write() {
        *guard = sender;
    }
}

fn normalize_dirty_reason(reason: String) -> String {
    let reason = reason.trim();
    if reason.is_empty() {
        "memory_changed".to_string()
    } else {
        reason.chars().take(120).collect()
    }
}

fn non_zero_or_default(value: Duration, fallback: Duration) -> Duration {
    if value.is_zero() {
        fallback
    } else {
        value
    }
}

fn read_duration_env(key: &str) -> Option<std::time::Duration> {
    std::env::var(key)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .map(std::time::Duration::from_secs)
}

fn read_positive_duration_env(key: &str) -> Option<std::time::Duration> {
    read_duration_env(key).filter(|duration| !duration.is_zero())
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use magician_vector_index::memory_index::{
        MemoryIndexManifest, MemoryLanceDbOptimizeReport, MemoryLanceDbWriteReport,
    };

    fn optimize_report() -> MemoryLanceDbOptimizeReport {
        MemoryLanceDbOptimizeReport {
            attempted: false,
            completed: false,
            reason: None,
            error: None,
            mutated_rows: 0,
            current_rows: 0,
            compaction_ran: false,
            prune_ran: false,
        }
    }

    fn write_report(chunk_count: usize, embedded_rows: Option<u64>) -> MemoryLanceDbWriteReport {
        MemoryLanceDbWriteReport {
            mode: "merge".to_string(),
            chunk_count,
            source_rows: None,
            embedded_rows,
            reused_rows: None,
            inserted_rows: None,
            updated_rows: None,
            deleted_rows: None,
            replacement_reason: None,
            optimize: optimize_report(),
        }
    }

    fn manifest_with_model(model: Option<&str>) -> MemoryIndexManifest {
        MemoryIndexManifest {
            index_version: "v1".to_string(),
            schema_version: 1,
            backend: "lancedb".to_string(),
            backend_status: "ok".to_string(),
            embedding_provider: "ollama".to_string(),
            embedding_model: model.map(|value| value.to_string()),
            embedding_dimensions: 768,
            embedding_contract_id: format!("test-contract:{}:768", model.unwrap_or("default")),
            embedding_fallback_reason: None,
            principal: Some("owner".to_string()),
            workspace: Some("default".to_string()),
            rebuilt_at: chrono::Utc::now(),
            document_count: 0,
            chunk_count: 0,
            source_count: 0,
            source_hashes: Default::default(),
            agents: Vec::new(),
        }
    }

    #[test]
    fn embedded_batch_size_prefers_embedded_rows_then_falls_back_to_chunk_count() {
        // Fine-grained embedded_rows wins when present.
        assert_eq!(
            memory_index_embedded_batch_size(&write_report(20, Some(7))),
            7
        );
        // Absent embedded_rows falls back to the total chunk_count.
        assert_eq!(
            memory_index_embedded_batch_size(&write_report(11, None)),
            11
        );
        // Nothing embedded and no chunks → an empty batch (record is suppressed).
        assert_eq!(
            memory_index_embedded_batch_size(&write_report(0, Some(0))),
            0
        );
        assert_eq!(memory_index_embedded_batch_size(&write_report(0, None)), 0);
    }

    #[test]
    fn embedding_model_falls_back_to_unknown_when_manifest_omits_it() {
        assert_eq!(
            memory_index_embedding_model(&manifest_with_model(Some("nomic-embed-text"))),
            "nomic-embed-text"
        );
        assert_eq!(
            memory_index_embedding_model(&manifest_with_model(None)),
            "unknown"
        );
    }

    #[tokio::test]
    async fn record_memory_index_embedding_writes_one_tagged_memory_index_row() {
        use crate::magician_v2::analytics::llm_embeddings_sink::LlmEmbeddingsSink;

        let temp = tempfile::tempdir().expect("tempdir");
        let ws = ArtifactV2Workspace::new(temp.path());
        let sink = LlmEmbeddingsSink::spawn(ws.clone());

        record_memory_index_embedding_to(
            Some(&sink),
            "owner",
            "default",
            &manifest_with_model(Some("nomic-embed-text")),
            &write_report(20, Some(5)),
            42,
        );
        // A zero-batch write must be suppressed (no vectors were obtained).
        record_memory_index_embedding_to(
            Some(&sink),
            "owner",
            "default",
            &manifest_with_model(Some("nomic-embed-text")),
            &write_report(0, Some(0)),
            7,
        );
        sink.shutdown().await;

        let root = ws.analytics_llm_embeddings_root("owner", "default");
        let glob = crate::magician_v2::dataset_owners::family_read_glob(
            &root,
            crate::magician_v2::dataset_owners::DatasetFamily::LlmEmbeddings,
        );
        let conn = duckdb::Connection::open_in_memory().unwrap();
        let mut stmt = conn
            .prepare(&format!(
                "SELECT operation, provider, model, batch_size, cost_usd, success \
                 FROM read_parquet('{glob}', union_by_name = true)"
            ))
            .expect("prepare");
        let out: Vec<(String, String, String, i32, f64, bool)> = stmt
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(out.len(), 1, "exactly one non-empty batch is recorded");
        assert_eq!(out[0].0, "memory_index");
        assert_eq!(out[0].1, "ollama", "provider tagged by the sink");
        assert_eq!(out[0].2, "nomic-embed-text");
        assert_eq!(out[0].3, 5, "batch_size = embedded_rows");
        assert_eq!(out[0].4, 0.0, "local embeddings cost nothing");
        assert!(out[0].5);
    }

    #[test]
    fn memory_index_rebuild_backoff_is_exponential_and_capped() {
        assert_eq!(
            memory_index_rebuild_backoff(1),
            MEMORY_INDEX_REBUILD_RETRY_INITIAL
        );
        assert_eq!(
            memory_index_rebuild_backoff(2),
            Duration::from_secs(30 * 60)
        );
        assert_eq!(
            memory_index_rebuild_backoff(3),
            MEMORY_INDEX_REBUILD_RETRY_MAX
        );
        assert_eq!(
            memory_index_rebuild_backoff(u32::MAX),
            MEMORY_INDEX_REBUILD_RETRY_MAX
        );
    }

    #[test]
    fn dirty_scope_due_policy_keeps_quiet_debounce_but_caps_continuous_write_starvation() {
        let now = Instant::now();
        let debounce = Duration::from_secs(20 * 60);
        let max_wait = bounded_dirty_memory_index_max_wait(debounce);
        let cases = [
            (
                "new write is still coalesced",
                Duration::from_secs(10),
                Duration::from_secs(2),
                None,
                false,
            ),
            (
                "quiet period makes scope due",
                Duration::from_secs(30),
                debounce,
                None,
                true,
            ),
            (
                "continuous writes cannot reset first-dirty max wait",
                max_wait,
                Duration::from_millis(1),
                None,
                true,
            ),
            (
                "retry cooldown remains authoritative after max wait",
                max_wait.saturating_mul(10),
                debounce,
                Some(unix_epoch_millis_now().saturating_add(30_000)),
                false,
            ),
        ];
        for (name, dirty_for, quiet_for, next_retry_at_unix_ms, expected) in cases {
            let state = DirtyMemoryIndexState {
                first_dirty_at: now - dirty_for,
                last_dirty_at: now - quiet_for,
                reasons: BTreeSet::from(["test".to_string()]),
                next_retry_at_unix_ms,
            };
            assert_eq!(
                dirty_memory_index_state_is_due(
                    &state,
                    now,
                    unix_epoch_millis_now(),
                    debounce,
                    max_wait,
                ),
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn dirty_scope_max_wait_is_conservative_and_bounded_from_configured_debounce() {
        let cases = [
            (
                "default short debounce avoids rebuild thrash",
                Duration::from_secs(15),
                Duration::from_secs(15 * 60),
            ),
            (
                "production debounce remains its own first-dirty ceiling",
                Duration::from_secs(20 * 60),
                Duration::from_secs(20 * 60),
            ),
            (
                "oversized debounce cannot starve forever",
                Duration::from_secs(6 * 60 * 60),
                Duration::from_secs(30 * 60),
            ),
        ];
        for (name, debounce, expected) in cases {
            assert_eq!(
                bounded_dirty_memory_index_max_wait(debounce),
                expected,
                "{name}"
            );
        }
    }

    #[test]
    fn only_explicit_live_eval_principal_contracts_are_ignored_by_runtime_maintenance() {
        let cases = [
            ("live-eval", true),
            ("storage-live-eval", true),
            ("memory-live-eval", false),
            ("anonymous", false),
            ("local", false),
            ("live-evaluation", false),
            ("customer-live-eval-data", false),
        ];
        for (principal, expected) in cases {
            assert_eq!(
                scope_is_ephemeral_eval(principal),
                expected,
                "principal={principal}"
            );
        }
    }

    #[test]
    fn retrieval_fallback_tracker_rate_limits_summarizes_and_recovers_by_episode() {
        let mut tracker = RetrievalFallbackTracker::default();
        let key = retrieval_fallback_key(
            "memory_prompt_hybrid",
            "anonymous",
            "default",
            "personal-assistant",
        );
        let started = Instant::now();
        let summary_interval = Duration::from_secs(300);

        assert_eq!(
            tracker.observe_fallback(
                key.clone(),
                "memory_index_pending_changes",
                started,
                summary_interval,
            ),
            RetrievalFallbackObservation::Started
        );
        assert_eq!(
            tracker.observe_fallback(
                key.clone(),
                "memory_index_pending_changes",
                started + Duration::from_secs(1),
                summary_interval,
            ),
            RetrievalFallbackObservation::Suppressed
        );
        assert_eq!(
            tracker.observe_fallback(
                key.clone(),
                "Ollama embedding request timed out",
                started + summary_interval,
                summary_interval,
            ),
            RetrievalFallbackObservation::Summary {
                total_count: 3,
                occurrences_since_last_log: 2,
                duration_ms: summary_interval.as_millis() as u64,
            }
        );

        let recovery = tracker
            .observe_recovery(&key, started + summary_interval + Duration::from_secs(1))
            .expect("active episode recovers once");
        assert_eq!(recovery.total_count, 3);
        assert_eq!(recovery.last_reason, "Ollama embedding request timed out");
        assert!(tracker
            .observe_recovery(&key, started + summary_interval + Duration::from_secs(2))
            .is_none());
        assert_eq!(
            tracker.observe_fallback(
                key,
                "new episode",
                started + summary_interval + Duration::from_secs(3),
                summary_interval,
            ),
            RetrievalFallbackObservation::Started,
            "a recovered key starts a fresh episode"
        );
    }

    #[test]
    fn retrieval_fallback_tracker_evicts_the_oldest_episode_at_its_hard_cap() {
        let mut tracker = RetrievalFallbackTracker::default();
        let started = Instant::now();
        for index in 0..RETRIEVAL_FALLBACK_EPISODE_CAP {
            let key = retrieval_fallback_key(
                "memory_prompt_hybrid",
                "anonymous",
                "default",
                &format!("actor-{index}"),
            );
            assert_eq!(
                tracker.observe_fallback(
                    key,
                    "test fallback",
                    started + Duration::from_millis(index as u64),
                    Duration::from_secs(300),
                ),
                RetrievalFallbackObservation::Started,
            );
        }
        let oldest =
            retrieval_fallback_key("memory_prompt_hybrid", "anonymous", "default", "actor-0");
        let newest = retrieval_fallback_key(
            "memory_prompt_hybrid",
            "anonymous",
            "default",
            "actor-over-cap",
        );
        assert_eq!(
            tracker.observe_fallback(
                newest.clone(),
                "test fallback",
                started + Duration::from_secs(10),
                Duration::from_secs(300),
            ),
            RetrievalFallbackObservation::Started,
        );
        assert_eq!(tracker.episodes.len(), RETRIEVAL_FALLBACK_EPISODE_CAP);
        assert!(!tracker.episodes.contains_key(&oldest));
        assert!(tracker.episodes.contains_key(&newest));
    }

    #[tokio::test]
    async fn empty_uninitialized_scope_clears_circuit_but_preserves_journal_for_reconciliation() {
        use magician_vector_index::memory_index::{record_memory_index_change, MemoryIndexChange};

        let temp = tempfile::tempdir().expect("tempdir");
        let workspace_layout = ArtifactV2Workspace::new(temp.path());
        let principal = "anonymous";
        let workspace = "empty";
        let memory_root = workspace_layout.memory_root(principal, workspace);
        fs::create_dir_all(&memory_root)
            .await
            .expect("create empty canonical memory root");
        let storage = AgentStorage::with_scoped_memory_root_in_workspace(
            &memory_root,
            workspace_layout.clone(),
        );
        let definition_store = AgentDefinitionStore::with_workspace_layout(workspace_layout)
            .for_scope(principal, workspace);
        let scope = DirtyMemoryIndexScope {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
        };
        let cooldown = MemoryIndexRebuildCooldown {
            failure_count: 42,
            next_retry_at_unix_ms: unix_epoch_millis_now().saturating_add(60_000),
            last_error:
                "runtime memory index reconciliation requires explicit rebuild: missing_manifest"
                    .to_string(),
        };
        assert!(persist_memory_index_rebuild_cooldown(&storage, &scope, &cooldown).await);
        record_memory_index_change(&storage, MemoryIndexChange::UserKnowledge)
            .await
            .expect("record pending no-op source change");

        let settled = settle_empty_memory_index_scope(&storage, &definition_store, &scope)
            .await
            .expect("empty-scope preflight")
            .expect("scope is canonically empty");
        assert_eq!(settled.stale_reason, "missing_manifest");
        assert!(settled.cleared_cooldown);
        assert!(load_memory_index_rebuild_cooldown(&storage, &scope)
            .await
            .is_none());
        let snapshot = snapshot_memory_index_changes(&storage)
            .await
            .expect("read journal after circuit reset");
        assert!(
            !snapshot.is_empty(),
            "canonical proof must not acknowledge pending derived-index work"
        );
        assert!(
            !fs::try_exists(storage.memory_index_manifest_path())
                .await
                .expect("manifest existence"),
            "preflight must leave stable empty-index creation to normal reconciliation"
        );

        assert!(matches!(
            apply_memory_index_change_snapshot(&storage, &definition_store, &snapshot)
                .await
                .expect("incremental preflight"),
            MemoryIndexIncrementalUpdateResult::FullRebuildRequired { reason }
                if reason == "missing_manifest"
        ));
        let rebuilt = reconcile_scope_memory_index(&storage, &definition_store)
            .await
            .expect("safe initial empty-index reconciliation");
        assert_eq!(rebuilt.manifest.document_count, 0);
        assert_eq!(rebuilt.lancedb_write.mode, "create_empty");
        assert!(fs::try_exists(storage.memory_index_manifest_path())
            .await
            .expect("manifest existence after reconciliation"));
        acknowledge_memory_index_changes(&storage, &snapshot)
            .await
            .expect("acknowledge after successful reconciliation");
        assert!(snapshot_memory_index_changes(&storage)
            .await
            .expect("read journal after reconciliation")
            .is_empty());
    }

    #[tokio::test]
    async fn empty_scope_preflight_gate_never_hides_nonempty_or_malformed_manifests() {
        let temp = tempfile::tempdir().expect("tempdir");
        let storage = AgentStorage::new(temp.path());
        assert!(derived_memory_index_may_be_empty_or_uninitialized(&storage)
            .await
            .expect("missing manifest gate"));

        let mut manifest = manifest_with_model(Some("nomic-embed-text"));
        storage
            .write_json_atomic(storage.memory_index_manifest_path(), &manifest)
            .await
            .expect("write empty manifest");
        assert!(derived_memory_index_may_be_empty_or_uninitialized(&storage)
            .await
            .expect("empty manifest gate"));

        manifest.document_count = 1;
        storage
            .write_json_atomic(storage.memory_index_manifest_path(), &manifest)
            .await
            .expect("write nonempty manifest");
        assert!(
            !derived_memory_index_may_be_empty_or_uninitialized(&storage)
                .await
                .expect("nonempty manifest gate")
        );

        storage
            .write_bytes_atomic(storage.memory_index_manifest_path(), b"{not-json")
            .await
            .expect("write malformed manifest");
        assert!(
            !derived_memory_index_may_be_empty_or_uninitialized(&storage)
                .await
                .expect("malformed manifest gate")
        );
    }

    #[tokio::test]
    async fn persisted_memory_index_rebuild_cooldown_round_trips_and_expires() {
        let temp = tempfile::tempdir().expect("tempdir");
        let storage = AgentStorage::new(temp.path());
        let scope = DirtyMemoryIndexScope {
            principal: "principal".to_string(),
            workspace: "workspace".to_string(),
        };
        let now_unix_ms = unix_epoch_millis_now();
        let cooldown = MemoryIndexRebuildCooldown {
            failure_count: 2,
            next_retry_at_unix_ms: now_unix_ms.saturating_add(10_000),
            last_error: "Ollama embedding request timed out".to_string(),
        };

        assert!(
            persist_memory_index_rebuild_cooldown(&storage, &scope, &cooldown).await,
            "persist cooldown"
        );
        let restored = load_memory_index_rebuild_cooldown(&storage, &scope)
            .await
            .expect("load persisted cooldown");
        assert_eq!(restored.failure_count, 2);
        assert_eq!(restored.backoff(), Duration::from_secs(30 * 60));
        assert!(restored.is_active(now_unix_ms));
        assert!(!restored.is_active(restored.next_retry_at_unix_ms));

        clear_memory_index_rebuild_cooldown(&storage, &scope).await;
        assert!(
            load_memory_index_rebuild_cooldown(&storage, &scope)
                .await
                .is_none(),
            "a successful rebuild removes the persisted cooldown"
        );
    }

    #[test]
    fn repairable_memory_index_errors_include_lancedb_io_but_not_query_timeout() {
        assert!(memory_index_retrieval_error_is_repairable(
            "LanceError(IO): Generic LocalFileSystem error: failed to fill whole buffer"
        ));
        assert!(memory_index_retrieval_error_is_transient(
            "timed out after 5000ms querying LanceDB memory hybrid index"
        ));
        assert!(!memory_index_retrieval_error_is_repairable(
            "timed out after 5000ms querying LanceDB memory hybrid index"
        ));
        assert!(memory_index_retrieval_error_is_repairable(
            "LanceDB memory index unhealthy: missing_lancedb_table"
        ));
        assert!(memory_index_retrieval_error_is_repairable(
            "LanceDB memory index unhealthy: lancedb_health_check_failed"
        ));
        assert!(memory_index_retrieval_error_is_transient(
            "LanceDB memory index transiently unavailable: lancedb_health_check_timed_out"
        ));
        assert!(!memory_index_retrieval_error_is_repairable(
            "LanceDB memory index transiently unavailable: lancedb_health_check_timed_out"
        ));
        assert!(!memory_index_retrieval_error_is_repairable(
            "embedding endpoint returned 401 unauthorized"
        ));
        assert!(!memory_index_retrieval_error_is_repairable(
            "embedding memory query for LanceDB hybrid search: calling Ollama embedding endpoint http://127.0.0.1:11434/api/embed: Ollama embedding endpoint returned 404 Not Found"
        ));
    }

    #[test]
    fn transient_ollama_query_embedding_errors_include_busy_server_responses() {
        assert!(memory_index_embedding_error_is_transient(
            "embedding memory query for LanceDB hybrid search: calling Ollama embedding endpoint http://127.0.0.1:11434/api/embed: HTTP status client error (429 Too Many Requests)"
        ));
        assert!(memory_index_embedding_error_is_transient(
            "embedding memory query for LanceDB hybrid search: calling Ollama embedding endpoint http://127.0.0.1:11434/api/embed: HTTP status server error (503 Service Unavailable)"
        ));
        assert!(memory_index_embedding_error_is_transient(
            "embedding memory query for LanceDB hybrid search: calling Ollama embedding endpoint http://127.0.0.1:11434/api/embed: error sending request for url: connection refused"
        ));
        assert!(!memory_index_embedding_error_is_transient(
            "embedding memory query for LanceDB hybrid search: calling Ollama embedding endpoint http://127.0.0.1:11434/api/embed: HTTP status client error (401 Unauthorized)"
        ));
        assert!(!memory_index_embedding_error_is_transient(
            "embedding memory query for LanceDB hybrid search: calling Ollama embedding endpoint http://127.0.0.1:11434/api/embed: HTTP status client error (404 Not Found)"
        ));
        assert!(!memory_index_embedding_error_is_transient(
            "embedding memory query for LanceDB hybrid search: 1500 cached rows were considered"
        ));
    }

    #[tokio::test]
    async fn lancedb_quarantine_moves_existing_index_dir() {
        use crate::magician_v2::agents::quarantine_scope_memory_lancedb_index;

        let temp = tempfile::tempdir().expect("tempdir");
        let storage = AgentStorage::new(temp.path());
        let index_dir = storage.memory_lancedb_index_dir();
        fs::create_dir_all(&index_dir)
            .await
            .expect("create index dir");
        fs::write(index_dir.join("sentinel"), b"broken")
            .await
            .expect("write sentinel");

        quarantine_scope_memory_lancedb_index(&storage)
            .await
            .expect("quarantine index");

        assert!(
            !fs::try_exists(&index_dir)
                .await
                .expect("check original dir"),
            "original corrupt LanceDB dir should be moved aside"
        );
        let mut entries = fs::read_dir(index_dir.parent().expect("index parent"))
            .await
            .expect("read parent");
        let mut found_quarantine = false;
        while let Some(entry) = entries.next_entry().await.expect("next entry") {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("lancedb.corrupt-")
            {
                found_quarantine = true;
                assert!(
                    fs::try_exists(entry.path().join("sentinel"))
                        .await
                        .expect("check sentinel"),
                    "quarantined dir should keep prior files for debugging"
                );
            }
        }
        assert!(found_quarantine, "expected a quarantined LanceDB dir");
    }
}
