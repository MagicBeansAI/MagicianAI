//! File-based memory service for TRUE_AGENTS Phase 2.
//!
//! Phase 2 scope: storage contracts and read/write APIs only.
//! Interpreters (consolidation, prompt rendering, retention sweep) are deferred.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::{sync::Mutex, task};
use tracing::warn;

use magician_vector_index::memory_index::{record_memory_index_change, MemoryIndexChange};

use crate::magician_v2::{
    analytics::{
        memory_index_maintainer::mark_memory_index_dirty_for_scope,
        memory_parquet::{emit_rows_for_storage, json_payload, MemoryAnalyticsRow},
    },
    artifact_v2::{
        memory::{V3EpisodeRecord, V3MemoryTierRecord},
        workspace::ArtifactV2Workspace,
    },
};

use super::{
    memory_tiers::{MemoryTierDefinition, StrategyEffectiveness, StrategyRecord},
    storage::{sanitize_segment, AgentStorage, AgentStorageError},
    types::{EpisodeRetention, UserMemoryIsolation},
};

#[cfg(any(test, feature = "test-fixtures"))]
use super::memory_tiers::TierScope;

#[derive(Debug, Error)]
pub enum AgentMemoryError {
    #[error("storage error: {0}")]
    Storage(#[from] AgentStorageError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("validation error: {0}")]
    Validation(String),
    #[error("exact app-memory replay is older than the retained checkpoint window")]
    HistoryCompacted,
    #[error("timed out acquiring corrections append lock `{lock_path}` after {wait_ms}ms")]
    CorrectionsLockTimeout { lock_path: String, wait_ms: u64 },
}

impl AgentMemoryError {
    /// True when the error is a transient I/O condition that is worth retrying
    /// (a filesystem hiccup or a lock-acquisition timeout under contention).
    /// Permanent errors (validation, serialization, identifier/path violations)
    /// return `false` — retrying them only wastes work. Used by the harness
    /// post-cycle episode-persist retry so a transient blip never costs a cycle
    /// its episode while a genuine bug still fails fast.
    pub fn is_transient(&self) -> bool {
        match self {
            AgentMemoryError::Io(_) | AgentMemoryError::CorrectionsLockTimeout { .. } => true,
            AgentMemoryError::Storage(storage) => matches!(
                storage,
                AgentStorageError::Io(_) | AgentStorageError::FileLockTimeout { .. }
            ),
            AgentMemoryError::Json(_)
            | AgentMemoryError::Validation(_)
            | AgentMemoryError::HistoryCompacted => false,
        }
    }
}

const EPISODE_RECALL_DEFAULT_LIMIT: usize = 10;
const EPISODE_RECALL_MAX_LIMIT: usize = 50;
const APP_RETRIEVAL_PROMPT_MAX_CANDIDATES: usize = 32;

fn conservative_knowledge_stamp(
    storage: &super::storage::AgentStorage,
    fallback_secs: i64,
) -> String {
    if let Ok(modified) =
        std::fs::metadata(storage.user_knowledge_path()).and_then(|meta| meta.modified())
    {
        let stamp = DateTime::<Utc>::from(modified);
        if stamp.timestamp() > 0 {
            return stamp.to_rfc3339();
        }
    }
    DateTime::<Utc>::from_timestamp(fallback_secs.min(1_700_000_000), 0)
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
        .to_rfc3339()
}

const CORRECTIONS_LOCK_MAX_WAIT: Duration = Duration::from_secs(5);
const CORRECTIONS_LOCK_RETRY_DELAY_MIN: Duration = Duration::from_millis(10);
const CORRECTIONS_LOCK_RETRY_DELAY_MAX: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Copy)]
struct CorrectionsAppendLockConfig {
    max_wait: Duration,
    retry_delay_min: Duration,
    retry_delay_max: Duration,
}

impl Default for CorrectionsAppendLockConfig {
    fn default() -> Self {
        Self {
            max_wait: CORRECTIONS_LOCK_MAX_WAIT,
            retry_delay_min: CORRECTIONS_LOCK_RETRY_DELAY_MIN,
            retry_delay_max: CORRECTIONS_LOCK_RETRY_DELAY_MAX,
        }
    }
}

#[derive(Debug)]
struct CorrectionsAppendLockGuard {
    _file: std::fs::File,
    _lock_path: PathBuf,
}

#[derive(Debug, Clone)]
struct ScopedMemoryContext {
    principal: String,
    workspace: String,
}

/// File-based agent memory service for Phase 2.
///
/// Correction appends are serialized within this process (async mutex) and
/// across processes (advisory lock file).
#[derive(Debug, Clone)]
pub struct AgentMemoryService {
    storage: AgentStorage,
    scoped_context: Option<ScopedMemoryContext>,
    // NOTE: Entries grow without bound (one per unique agent_id). Acceptable for
    // current agent cardinality (tens-hundreds). If dynamic agent IDs are introduced,
    // add periodic eviction of entries with Arc::strong_count() == 1.
    correction_append_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    /// Per-agent lock for episode index read-modify-write operations.
    // NOTE: Same unbounded-growth caveat as `correction_append_locks` above.
    episode_index_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    /// Serializes the inert app-contribution receipt/head/projection owner.
    /// Cross-process exclusion is additionally provided by AgentStorage.
    app_contribution_lock: Arc<Mutex<()>>,
}

/// Resolves the correct memory backend for a given scope.
///
/// V3-backed task/execution flows persist agent memory under
/// `magician_data_v3/scopes/<principal>/<workspace>/memory`.
/// This resolver gives runtime readers/writers a single scoped entrypoint into
/// that V3 memory root.
#[derive(Debug, Clone)]
pub struct AgentMemoryResolver {
    workspace_layout: ArtifactV2Workspace,
}

/// Internal tool for on-demand raw episode retrieval during reasoning.
///
/// This is read-only and delegates to [`AgentMemoryService::recall_native_episodes`].
#[derive(Debug, Clone)]
pub struct EpisodeRecallTool {
    memory_service: AgentMemoryService,
}

impl EpisodeRecallTool {
    pub fn new(memory_service: AgentMemoryService) -> Self {
        Self { memory_service }
    }

    pub async fn recall(
        &self,
        agent_id: &str,
        goal_id: &str,
        seq_range: Option<(u64, u64)>,
        limit: Option<usize>,
    ) -> Result<Vec<V3EpisodeRecord>, AgentMemoryError> {
        self.memory_service
            .recall_native_episodes(agent_id, goal_id, seq_range, limit)
            .await
    }
}

impl AgentMemoryResolver {
    pub fn new(base_root: impl AsRef<Path>) -> Self {
        Self {
            workspace_layout: ArtifactV2Workspace::new(base_root),
        }
    }

    pub fn with_workspace_layout(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }

    pub fn resolve_for_scope(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<AgentMemoryService, AgentMemoryError> {
        let principal = principal.trim();
        let workspace = workspace.trim();
        if principal.is_empty() {
            return Err(AgentMemoryError::Validation(
                "principal is required for scoped memory resolution".to_string(),
            ));
        }
        if workspace.is_empty() {
            return Err(AgentMemoryError::Validation(
                "workspace is required for scoped memory resolution".to_string(),
            ));
        }

        Ok(AgentMemoryService::with_scoped_memory_scope_in_workspace(
            self.workspace_layout.clone(),
            principal,
            workspace,
        ))
    }

    /// Tenant scopes only. A reserved sink has no agents and no chat, so
    /// resolving memory in one would materialise an index for a reader that
    /// cannot exist.
    pub fn list_tenant_scopes(&self) -> Vec<(String, String)> {
        self.workspace_layout.list_tenant_scopes()
    }
}

impl AgentMemoryService {
    const MAX_EPISODE_FILENAME_BYTES: usize = 255;
    const EPISODE_FILENAME_PREFIX: &str = "ep~";
    const JSON_EXTENSION: &str = ".json";

    pub fn new(storage: AgentStorage) -> Self {
        Self {
            storage,
            scoped_context: None,
            correction_append_locks: Arc::new(Mutex::new(HashMap::new())),
            episode_index_locks: Arc::new(Mutex::new(HashMap::new())),
            app_contribution_lock: Arc::new(Mutex::new(())),
        }
    }

    pub(crate) fn app_contribution_storage(&self) -> &AgentStorage {
        &self.storage
    }

    pub(crate) fn app_contribution_lock(&self) -> Arc<Mutex<()>> {
        Arc::clone(&self.app_contribution_lock)
    }

    pub fn with_base_path(base: impl AsRef<Path>) -> Self {
        Self::new(AgentStorage::new(base))
    }

    pub fn with_scoped_memory_root(base: impl AsRef<Path>) -> Self {
        Self::new(AgentStorage::with_scoped_memory_root(base))
    }

    pub fn with_scoped_memory_scope(
        base: impl AsRef<Path>,
        principal: impl Into<String>,
        workspace: impl Into<String>,
    ) -> Self {
        let mut service = Self::new(AgentStorage::with_scoped_memory_root(base));
        service.scoped_context = Some(ScopedMemoryContext {
            principal: principal.into(),
            workspace: workspace.into(),
        });
        service
    }

    pub fn with_scoped_memory_scope_in_workspace(
        workspace_layout: ArtifactV2Workspace,
        principal: impl Into<String>,
        workspace: impl Into<String>,
    ) -> Self {
        let principal = principal.into();
        let workspace = workspace.into();
        let base = workspace_layout.memory_root(&principal, &workspace);
        let mut service = Self::new(AgentStorage::with_scoped_memory_root_in_workspace(
            base,
            workspace_layout,
        ));
        service.scoped_context = Some(ScopedMemoryContext {
            principal,
            workspace,
        });
        service
    }

    pub fn storage(&self) -> &AgentStorage {
        &self.storage
    }

    pub fn scoped_memory_scope(&self) -> Option<(&str, &str)> {
        self.scoped_context
            .as_ref()
            .map(|ctx| (ctx.principal.as_str(), ctx.workspace.as_str()))
    }

    /// Live-resolve app-sourced memory envelopes against the scoped app store.
    /// Ordinary documents without an envelope are omitted from the map.
    pub async fn app_memory_prompt_eligibility(
        &self,
        metadatas: impl IntoIterator<Item = serde_json::Value>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> std::collections::HashMap<String, bool> {
        let workspace = self.storage.workspace_layout().cloned();
        let scoped = self
            .scoped_memory_scope()
            .map(|(principal, workspace)| (principal.to_owned(), workspace.to_owned()));
        let metadatas = metadatas.into_iter().collect::<Vec<_>>();
        let metadata_for_contributions = metadatas.clone();
        let mut eligibility = tokio::task::spawn_blocking(move || {
            crate::magician_v2::apps::memory_store::resolve_prompt_app_memory_eligibility_batch(
                workspace.as_ref(),
                scoped.as_ref().map(|(principal, _)| principal.as_str()),
                scoped.as_ref().map(|(_, workspace)| workspace.as_str()),
                metadatas,
                now,
            )
        })
        .await
        .unwrap_or_default();
        let live_contributions = self.live_app_memory_contribution_candidates(now).await;
        let live_retrieval = self.live_personal_agent_retrieval_candidates(now).await;
        for metadata in metadata_for_contributions {
            let Some(Ok(envelope)) =
                crate::magician_v2::apps::memory_bridge::parse_source_eligibility_envelope(
                    &metadata,
                )
            else {
                continue;
            };
            if live_contributions
                .iter()
                .chain(live_retrieval.iter())
                .any(|candidate| {
                    crate::magician_v2::apps::memory_bridge::canonical_source_eligibility_envelope(
                        candidate,
                    ) == envelope
                })
            {
                eligibility.insert(envelope.candidate_id.to_string(), true);
            }
        }
        eligibility
    }

    /// Project canonical accepted app-memory candidates into the prompt
    /// retrieval stream only after their exact source rows remain eligible.
    /// Failure is fail-closed for app memory and does not starve ordinary
    /// file-backed memory candidates.
    pub(crate) async fn eligible_app_memory_candidates(
        &self,
        target: crate::magician_v2::apps::memory_store::AppMemoryPromptTarget,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Vec<crate::magician_v2::apps::memory::AppMemoryCandidate> {
        let workspace = self.storage.workspace_layout().cloned();
        let scoped = self
            .scoped_memory_scope()
            .map(|(principal, workspace)| (principal.to_owned(), workspace.to_owned()));
        let legacy_target = target.clone();
        let mut candidates = tokio::task::spawn_blocking(move || {
            crate::magician_v2::apps::memory_store::load_prompt_eligible_app_memory_candidates(
                workspace.as_ref(),
                scoped.as_ref().map(|(principal, _)| principal.as_str()),
                scoped.as_ref().map(|(_, workspace)| workspace.as_str()),
                &legacy_target,
                now,
            )
            .unwrap_or_default()
        })
        .await
        .unwrap_or_default();
        let mut contribution_candidates = self
            .live_app_memory_contribution_candidates(now)
            .await
            .into_iter()
            .filter(|candidate| contribution_candidate_matches_target(candidate, &target))
            .collect::<Vec<_>>();
        candidates.append(&mut contribution_candidates);
        let mut retrieval_candidates = match &target {
            crate::magician_v2::apps::memory_store::AppMemoryPromptTarget::Agent { agent_id }
                if app_reference_matches_target(agent_id, "agent", "personal-assistant") =>
            {
                self.live_personal_agent_retrieval_candidates(now)
                    .await
                    .into_iter()
                    .filter(|candidate| contribution_candidate_matches_target(candidate, &target))
                    .collect::<Vec<_>>()
            },
            crate::magician_v2::apps::memory_store::AppMemoryPromptTarget::User
            | crate::magician_v2::apps::memory_store::AppMemoryPromptTarget::Agent { .. }
            | crate::magician_v2::apps::memory_store::AppMemoryPromptTarget::AgentGoal { .. } => {
                Vec::new()
            },
        };
        candidates.append(&mut retrieval_candidates);
        candidates.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.candidate_id.cmp(&right.candidate_id))
        });
        candidates.dedup_by(|left, right| left.candidate_id == right.candidate_id);
        candidates.truncate(
            crate::magician_v2::apps::models::AppContractLimits::default().max_collection_items(),
        );
        candidates
    }

    /// Enumerate the canonical Memory-lane candidates for the destination-
    /// owned hybrid-index projection. This is called only after destination
    /// acceptance/recovery or from prompt-time repair; entity mutation paths
    /// merely settle their canonical candidates and never embed content.
    pub(crate) async fn indexable_app_memory_candidates(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Vec<crate::magician_v2::apps::memory::AppMemoryCandidate> {
        let legacy_now = now.clone();
        let workspace = self.storage.workspace_layout().cloned();
        let scoped = self
            .scoped_memory_scope()
            .map(|(principal, workspace)| (principal.to_owned(), workspace.to_owned()));
        let mut candidates = tokio::task::spawn_blocking(move || {
            crate::magician_v2::apps::memory_store::load_indexable_app_memory_candidates(
                workspace.as_ref(),
                scoped.as_ref().map(|(principal, _)| principal.as_str()),
                scoped.as_ref().map(|(_, workspace)| workspace.as_str()),
                legacy_now,
            )
            .unwrap_or_default()
        })
        .await
        .unwrap_or_default();
        candidates.extend(self.live_app_memory_contribution_candidates(now).await);
        candidates.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.candidate_id.cmp(&right.candidate_id))
        });
        candidates.dedup_by(|left, right| left.candidate_id == right.candidate_id);
        candidates.truncate(4_096);
        candidates
    }

    async fn live_app_memory_contribution_candidates(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Vec<crate::magician_v2::apps::memory::AppMemoryCandidate> {
        let Ok(projection) = self.recover_app_memory_destination().await else {
            return Vec::new();
        };
        let Some(workspace_layout) = self.storage.workspace_layout().cloned() else {
            return Vec::new();
        };
        let Some((principal, workspace_name)) = self
            .scoped_memory_scope()
            .map(|(principal, workspace)| (principal.to_owned(), workspace.to_owned()))
        else {
            return Vec::new();
        };
        let entries = projection
            .entries
            .into_values()
            .filter(|entry| {
                entry.state
                    == crate::magician_v2::agents::app_memory_ingress::AppMemoryProjectionStateV1::Accepted
                    && entry.retained_until_ms.is_none_or(|expiry| now.timestamp_millis() < expiry)
            })
            .take(
                crate::magician_v2::apps::models::AppContractLimits::default()
                    .max_collection_items(),
            )
            .collect::<Vec<_>>();
        tokio::task::spawn_blocking(move || {
            let candidates = entries
                .into_iter()
                .filter_map(|entry| {
                    contribution_entry_to_memory_candidate(&principal, &workspace_name, entry).ok()
                })
                .collect::<Vec<_>>();
            let live =
                crate::magician_v2::apps::memory_store::projected_app_memory_candidates_are_live(
                    &workspace_layout,
                    &principal,
                    &workspace_name,
                    &candidates,
                    now,
                )
                .unwrap_or_else(|_| vec![false; candidates.len()]);
            candidates
                .into_iter()
                .zip(live)
                .filter_map(|(candidate, is_live)| is_live.then_some(candidate))
                .collect()
        })
        .await
        .unwrap_or_default()
    }

    /// Read the distinct personal-agent retrieval owner through a fresh,
    /// short-lived system-worker scope derived from this canonical scoped
    /// memory service. Proposal bytes never mint authority: the projection
    /// adapter reopens current package/grant/source authority on both sides of
    /// its private destination snapshot before returning anything here.
    async fn live_personal_agent_retrieval_candidates(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Vec<crate::magician_v2::apps::memory::AppMemoryCandidate> {
        let Some(workspace_layout) = self.storage.workspace_layout().cloned() else {
            return Vec::new();
        };
        let Some((principal, workspace_name)) = self
            .scoped_memory_scope()
            .map(|(principal, workspace)| (principal.to_owned(), workspace.to_owned()))
        else {
            return Vec::new();
        };
        let Ok(authenticated) =
            personal_agent_retrieval_system_scope(&principal, &workspace_name, now.clone())
        else {
            return Vec::new();
        };
        let registry =
            crate::magician_v2::apps::registry::AppRegistryService::new(workspace_layout.clone());
        let Ok(projection) = crate::magician_v2::apps::personal_agent_retrieval_projection::AppPersonalAgentRetrievalProjectionService::from_current_registry(
            registry,
            workspace_layout,
        ) else {
            return Vec::new();
        };
        let Ok(proposals) = projection
            .prompt_projection(
                &authenticated,
                "personal-assistant",
                None,
                crate::magician_v2::apps::models::AppContractLimits::default()
                    .max_collection_items()
                    .min(APP_RETRIEVAL_PROMPT_MAX_CANDIDATES),
                now.clone(),
            )
            .await
        else {
            return Vec::new();
        };
        proposals
            .into_iter()
            .filter_map(|proposal| {
                retrieval_proposal_to_memory_candidate(&principal, &workspace_name, proposal).ok()
            })
            .collect()
    }

    pub fn mark_index_dirty(&self, reason: &'static str) {
        if let Some((principal, workspace)) = self.scoped_memory_scope() {
            mark_memory_index_dirty_for_scope(principal, workspace, reason);
            return;
        }
        if let Some((principal, workspace)) = self.storage.scope_segments() {
            mark_memory_index_dirty_for_scope(principal, workspace, reason);
        }
    }

    async fn record_index_change(&self, change: MemoryIndexChange, reason: &'static str) {
        if let Err(error) = record_memory_index_change(self.storage(), change).await {
            warn!(
                error = %error,
                "failed to persist incremental memory index change; dirty rebuild remains the fallback"
            );
        }
        self.mark_index_dirty(reason);
    }

    pub async fn ensure_base_layout(&self) -> Result<(), AgentMemoryError> {
        self.storage.ensure_base_layout().await?;
        Ok(())
    }

    pub async fn ensure_agent_layout(&self, agent_id: &str) -> Result<(), AgentMemoryError> {
        self.storage.ensure_agent_layout(agent_id).await?;
        Ok(())
    }

    /// Returns the directory used for `user.*` tier storage depending on the
    /// agent's [`UserMemoryIsolation`] mode.
    ///
    /// - `Shared` → `{root}/user/` (shared pool, existing behaviour)
    /// - `FullyIsolated` → `{root}/agents/{agent_id}/memory/user_memory/`
    pub fn user_memory_root(
        &self,
        agent_id: &str,
        isolation: &UserMemoryIsolation,
    ) -> Result<PathBuf, AgentMemoryError> {
        match isolation {
            UserMemoryIsolation::Shared => Ok(self.storage.user_root()),
            UserMemoryIsolation::FullyIsolated => {
                Ok(self.storage.agent_user_memory_dir(agent_id)?)
            },
        }
    }

    /// Save a tier with user-memory-isolation awareness.
    ///
    /// For `User`-scoped tiers the write destination is determined by
    /// `isolation`. Non-user tiers are handled identically to [`save_native_tier`].
    #[cfg(any(test, feature = "test-fixtures"))]
    pub async fn save_tier_isolated(
        &self,
        agent_id: &str,
        tier_definition: &MemoryTierDefinition,
        goal_id: Option<&str>,
        data: &V3MemoryTierRecord,
        isolation: &UserMemoryIsolation,
    ) -> Result<(), AgentMemoryError> {
        if data.tier_name != tier_definition.name {
            return Err(AgentMemoryError::Validation(format!(
                "tier payload mismatch: data.tier_name=`{}` does not match tier definition `{}`",
                data.tier_name, tier_definition.name
            )));
        }

        match tier_definition.scope {
            TierScope::User => {
                let user_root = self.user_memory_root(agent_id, isolation)?;
                self.storage.create_dir_all(&user_root).await?;
                let path = self.storage.agent_tier_path_with_user_root(
                    agent_id,
                    &tier_definition.name,
                    &tier_definition.scope,
                    goal_id,
                    Some(&user_root),
                )?;
                self.write_native_tier_record(&path, data).await?;
            },
            _ => {
                validate_non_empty("agent_id", agent_id)?;
                self.ensure_agent_layout(agent_id).await?;
                let path = self.storage.agent_tier_path(
                    agent_id,
                    &tier_definition.name,
                    &tier_definition.scope,
                    goal_id,
                )?;
                self.write_native_tier_record(&path, data).await?;
            },
        }
        Ok(())
    }

    /// Load a tier with user-memory-isolation awareness.
    ///
    /// For `User`-scoped tiers the read source is determined by `isolation`.
    /// Non-user tiers are handled identically to [`load_native_tier`].
    #[cfg(any(test, feature = "test-fixtures"))]
    pub async fn load_tier_isolated(
        &self,
        agent_id: &str,
        tier_definition: &MemoryTierDefinition,
        goal_id: Option<&str>,
        isolation: &UserMemoryIsolation,
    ) -> Result<Option<V3MemoryTierRecord>, AgentMemoryError> {
        match tier_definition.scope {
            TierScope::User => {
                let user_root = self.user_memory_root(agent_id, isolation)?;
                let path = self.storage.agent_tier_path_with_user_root(
                    agent_id,
                    &tier_definition.name,
                    &tier_definition.scope,
                    goal_id,
                    Some(&user_root),
                )?;
                match self.read_native_tier_record(&path).await {
                    Ok(val) => Ok(Some(val)),
                    Err(AgentMemoryError::Io(err))
                        if err.kind() == std::io::ErrorKind::NotFound =>
                    {
                        Ok(None)
                    },
                    Err(err) => Err(err),
                }
            },
            _ => {
                validate_non_empty("agent_id", agent_id)?;
                self.load_native_tier(agent_id, tier_definition, goal_id)
                    .await
            },
        }
    }

    /// Snapshot-copy all shared user memory files into an agent's isolated
    /// user_memory directory.
    ///
    /// Existing files in the isolated directory are overwritten (full snapshot).
    pub async fn copy_shared_to_isolated(&self, agent_id: &str) -> Result<usize, AgentMemoryError> {
        validate_non_empty("agent_id", agent_id)?;
        let shared_root = self.storage.user_root();
        let isolated_root = self.storage.agent_user_memory_dir(agent_id)?;
        self.storage.create_dir_all(&isolated_root).await?;

        let files = self
            .storage
            .list_files_with_extension(&shared_root, "json")
            .await?;
        let mut copied = 0usize;
        for src_path in &files {
            let Some(file_name) = src_path.file_name() else {
                continue;
            };
            let dst_path = isolated_root.join(file_name);
            let content = self.storage.read_bytes(src_path).await?;
            self.storage.write_bytes_atomic(&dst_path, &content).await?;
            copied += 1;
        }
        Ok(copied)
    }

    /// Merge an agent's isolated user_memory into the shared user pool.
    ///
    /// Strategy: additive merge — for each tier JSON file in the agent's
    /// isolated directory, upsert into the shared pool. On key conflict
    /// the isolated (local) value wins.
    pub async fn merge_isolated_to_shared(
        &self,
        agent_id: &str,
    ) -> Result<usize, AgentMemoryError> {
        validate_non_empty("agent_id", agent_id)?;
        let shared_root = self.storage.user_root();
        let isolated_root = self.storage.agent_user_memory_dir(agent_id)?;

        self.storage.create_dir_all(&shared_root).await?;

        let files = self
            .storage
            .list_files_with_extension(&isolated_root, "json")
            .await?;
        let mut merged = 0usize;
        for src_path in &files {
            let Some(file_name) = src_path.file_name() else {
                continue;
            };
            let dst_path = shared_root.join(file_name);

            // Read the isolated tier data BEFORE acquiring the exclusive lock so
            // we only contend when there is actual data to merge.
            let local_data = match self.read_native_tier_record(src_path).await {
                Ok(data) => data,
                Err(_) => continue,
            };

            // Cross-process exclusive lock covers the read-modify-write cycle on
            // the shared tier file so that no other process can interleave its
            // own R-M-W on the same destination.
            let _flock = AgentStorage::acquire_file_lock_exclusive(&dst_path).await?;

            // Try to read existing shared tier data for merging.
            let merged_data = match self.read_native_tier_record(&dst_path).await {
                Ok(mut shared_data) => {
                    // Additive merge: local wins on conflict (upsert by field name).
                    for (key, value) in local_data.fields.clone() {
                        shared_data.fields.insert(key, value);
                    }
                    shared_data.last_updated =
                        shared_data.last_updated.max(local_data.last_updated);
                    shared_data
                },
                Err(AgentMemoryError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                    // No shared file yet — use the local data as-is.
                    local_data
                },
                Err(err) => return Err(err),
            };

            self.write_native_tier_record(&dst_path, &merged_data)
                .await?;
            self.record_index_change(
                MemoryIndexChange::FullScope {
                    reason: "isolated_user_memory_merge".to_string(),
                },
                "isolated_user_memory_merge",
            )
            .await;
            merged += 1;
        }
        Ok(merged)
    }

    async fn write_native_tier_record(
        &self,
        path: &Path,
        record: &V3MemoryTierRecord,
    ) -> Result<(), AgentMemoryError> {
        let record = self.stamp_scoped_tier_record(record);
        self.storage.write_json_atomic(path, &record).await?;
        Ok(())
    }

    async fn read_native_tier_record(
        &self,
        path: &Path,
    ) -> Result<V3MemoryTierRecord, AgentMemoryError> {
        match self.storage.read_json(path).await {
            Ok(record) => Ok(record),
            Err(AgentStorageError::Io(err)) => Err(AgentMemoryError::Io(err)),
            Err(err) => Err(err.into()),
        }
    }

    async fn write_native_episode_record(
        &self,
        path: &Path,
        episode: &V3EpisodeRecord,
    ) -> Result<(), AgentMemoryError> {
        let episode = self.stamp_scoped_episode_record(episode);
        self.storage.write_json_atomic(path, &episode).await?;
        Ok(())
    }

    async fn read_native_episode_record(
        &self,
        path: &Path,
    ) -> Result<V3EpisodeRecord, AgentMemoryError> {
        match self.storage.read_json(path).await {
            Ok(record) => Ok(record),
            Err(AgentStorageError::Io(err)) => Err(AgentMemoryError::Io(err)),
            Err(err) => Err(err.into()),
        }
    }

    fn native_episode_file_name_for_id_inner(&self, episode_id: &str) -> String {
        let sanitized = sanitize_segment(episode_id);
        if sanitized.len() + Self::JSON_EXTENSION.len() <= Self::MAX_EPISODE_FILENAME_BYTES {
            return format!("{sanitized}{}", Self::JSON_EXTENSION);
        }

        let hash = blake3::hash(episode_id.as_bytes()).to_hex().to_string();
        let keep = Self::MAX_EPISODE_FILENAME_BYTES.saturating_sub(
            Self::EPISODE_FILENAME_PREFIX.len() + 1 + hash.len() + Self::JSON_EXTENSION.len(),
        );
        format!(
            "{}{}_{}{}",
            Self::EPISODE_FILENAME_PREFIX,
            &sanitized[..keep],
            hash,
            Self::JSON_EXTENSION
        )
    }

    fn native_episode_file_name(&self, episode: &V3EpisodeRecord) -> String {
        self.native_episode_file_name_for_id_inner(&episode.episode_id)
    }

    pub fn native_episode_file_name_for_id(&self, episode_id: &str) -> String {
        self.native_episode_file_name_for_id_inner(episode_id)
    }

    pub fn native_episode_file_name_for_record(&self, episode: &V3EpisodeRecord) -> String {
        self.native_episode_file_name(episode)
    }

    /// Work-evidence graph: load the agent's evidence collection (empty when
    /// none has been written yet).
    pub async fn load_native_evidence(
        &self,
        agent_id: &str,
    ) -> Result<Vec<crate::magician_v2::evidence::EvidenceRecord>, AgentMemoryError> {
        validate_non_empty("agent_id", agent_id)?;
        let path = self.storage.agent_evidence_path(agent_id)?;
        match self.storage.read_json(&path).await {
            Ok(records) => Ok(records),
            Err(AgentStorageError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(Vec::new())
            },
            Err(AgentStorageError::Io(err)) => Err(AgentMemoryError::Io(err)),
            Err(err) => Err(err.into()),
        }
    }

    /// Work-evidence graph: upsert an evidence record into the agent's
    /// collection (idempotent by `evidence_id`), apply the retention cap
    /// (newest kept), then atomically rewrite the file.
    pub async fn append_native_evidence(
        &self,
        agent_id: &str,
        record: crate::magician_v2::evidence::EvidenceRecord,
    ) -> Result<(), AgentMemoryError> {
        validate_non_empty("agent_id", agent_id)?;
        let mut records = self.load_native_evidence(agent_id).await?;
        match records
            .iter()
            .position(|existing| existing.evidence_id == record.evidence_id)
        {
            // Never overwrite a human-corrected record (suppressed, deleted, or
            // manually re-faceted): re-distilling its source episode must not
            // resurrect suppressed evidence or wipe a manual re-label.
            Some(idx) if records[idx].is_user_corrected() => {},
            Some(idx) => records[idx] = record,
            None => records.push(record),
        }
        // Daily compaction window: merge same-(entity, kind, day) duplicates
        // before the retention cap, so the cap truncates a deduped set rather
        // than dropping valid older items because same-day duplicates filled it.
        // User-corrected records pass through untouched (see `compact_evidence`).
        let mut records = crate::magician_v2::evidence::compact_evidence(records).records;
        if records.len() > crate::magician_v2::evidence::EVIDENCE_RETENTION_CAP {
            records.sort_by(|a, b| b.last_seen_at.cmp(&a.last_seen_at));
            records.truncate(crate::magician_v2::evidence::EVIDENCE_RETENTION_CAP);
        }
        let path = self.storage.agent_evidence_path(agent_id)?;
        self.storage.write_json_atomic(&path, &records).await?;
        Ok(())
    }

    /// Work-evidence graph: apply an in-place correction to one record (by
    /// `evidence_id`) and atomically rewrite the file. Returns `false` when no
    /// record matches. Backs the inbox suppress / delete / re-facet flows.
    pub async fn update_native_evidence<F>(
        &self,
        agent_id: &str,
        evidence_id: &str,
        mutate: F,
    ) -> Result<bool, AgentMemoryError>
    where
        F: FnOnce(&mut crate::magician_v2::evidence::EvidenceRecord),
    {
        validate_non_empty("agent_id", agent_id)?;
        let mut records = self.load_native_evidence(agent_id).await?;
        let Some(idx) = records
            .iter()
            .position(|existing| existing.evidence_id == evidence_id)
        else {
            return Ok(false);
        };
        mutate(&mut records[idx]);
        let path = self.storage.agent_evidence_path(agent_id)?;
        self.storage.write_json_atomic(&path, &records).await?;
        Ok(true)
    }

    /// Work-evidence graph (Slice 2): load the agent's entity anchors (empty
    /// when none resolved yet).
    pub async fn load_native_entities(
        &self,
        agent_id: &str,
    ) -> Result<Vec<crate::magician_v2::evidence::EntityRecord>, AgentMemoryError> {
        validate_non_empty("agent_id", agent_id)?;
        let path = self.storage.agent_entities_path(agent_id)?;
        match self.storage.read_json(&path).await {
            Ok(records) => Ok(records),
            Err(AgentStorageError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(Vec::new())
            },
            Err(AgentStorageError::Io(err)) => Err(AgentMemoryError::Io(err)),
            Err(err) => Err(err.into()),
        }
    }

    /// Work-evidence graph (Slice 2): conservatively resolve candidate anchors
    /// into the stored set (exact key/alias merge only), apply the retention
    /// cap, then atomically rewrite the file. Backs the consolidation hook.
    pub async fn resolve_native_entities(
        &self,
        agent_id: &str,
        candidates: Vec<crate::magician_v2::evidence::EntityRecord>,
    ) -> Result<(), AgentMemoryError> {
        validate_non_empty("agent_id", agent_id)?;
        if candidates.is_empty() {
            return Ok(());
        }
        let mut records = self.load_native_entities(agent_id).await?;
        crate::magician_v2::evidence::resolve_entities(&mut records, candidates);
        if records.len() > crate::magician_v2::evidence::ENTITY_RETENTION_CAP {
            records.sort_by(|a, b| b.last_seen_at.cmp(&a.last_seen_at));
            records.truncate(crate::magician_v2::evidence::ENTITY_RETENTION_CAP);
        }
        let path = self.storage.agent_entities_path(agent_id)?;
        self.storage.write_json_atomic(&path, &records).await?;
        Ok(())
    }

    /// Work-evidence graph (Slice 2): apply an in-place mutation across the whole
    /// entity set (the closure sees the full slice so merge/split can touch two
    /// anchors at once) and atomically rewrite. Returns the closure's `bool`
    /// (e.g. whether the correction applied). Backs the inbox merge/split/rename
    /// flows.
    pub async fn mutate_native_entities<F>(
        &self,
        agent_id: &str,
        mutate: F,
    ) -> Result<bool, AgentMemoryError>
    where
        F: FnOnce(&mut Vec<crate::magician_v2::evidence::EntityRecord>) -> bool,
    {
        validate_non_empty("agent_id", agent_id)?;
        let mut records = self.load_native_entities(agent_id).await?;
        let applied = mutate(&mut records);
        if applied {
            let path = self.storage.agent_entities_path(agent_id)?;
            self.storage.write_json_atomic(&path, &records).await?;
        }
        Ok(applied)
    }

    // ─── User-owned (ambient) evidence lane + unified read (WEG Phase 2) ─────
    //
    // Passive/ambient browsing evidence is the USER's, shared across their
    // agents — stored under the user-memory root, a sibling of the agent-scoped
    // `evidence.json`. The `load_scoped_*` resolvers merge both lanes so
    // consumers (reviews / dashboard) discover every producer without knowing
    // the storage layout.

    pub async fn load_user_work_evidence(
        &self,
    ) -> Result<Vec<crate::magician_v2::evidence::EvidenceRecord>, AgentMemoryError> {
        let path = self.storage.user_work_evidence_path();
        match self.storage.read_json(&path).await {
            Ok(records) => Ok(records),
            Err(AgentStorageError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(Vec::new())
            },
            Err(err) => Err(err.into()),
        }
    }

    pub async fn append_user_work_evidence(
        &self,
        record: crate::magician_v2::evidence::EvidenceRecord,
    ) -> Result<(), AgentMemoryError> {
        let mut records = self.load_user_work_evidence().await?;
        match records
            .iter()
            .position(|existing| existing.evidence_id == record.evidence_id)
        {
            Some(idx) if records[idx].is_user_corrected() => {},
            Some(idx) => records[idx] = record,
            None => records.push(record),
        }
        // Daily compaction window: merge same-(entity, kind, day) duplicates
        // before the retention cap (user-corrected records pass through untouched).
        let mut records = crate::magician_v2::evidence::compact_evidence(records).records;
        if records.len() > crate::magician_v2::evidence::EVIDENCE_RETENTION_CAP {
            records.sort_by(|a, b| b.last_seen_at.cmp(&a.last_seen_at));
            records.truncate(crate::magician_v2::evidence::EVIDENCE_RETENTION_CAP);
        }
        self.ensure_base_layout().await?;
        let path = self.storage.user_work_evidence_path();
        self.storage.write_json_atomic(&path, &records).await?;
        Ok(())
    }

    pub async fn load_user_work_entities(
        &self,
    ) -> Result<Vec<crate::magician_v2::evidence::EntityRecord>, AgentMemoryError> {
        let path = self.storage.user_work_entities_path();
        match self.storage.read_json(&path).await {
            Ok(records) => Ok(records),
            Err(AgentStorageError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(Vec::new())
            },
            Err(err) => Err(err.into()),
        }
    }

    pub async fn resolve_user_work_entities(
        &self,
        candidates: Vec<crate::magician_v2::evidence::EntityRecord>,
    ) -> Result<(), AgentMemoryError> {
        if candidates.is_empty() {
            return Ok(());
        }
        let mut records = self.load_user_work_entities().await?;
        crate::magician_v2::evidence::resolve_entities(&mut records, candidates);
        if records.len() > crate::magician_v2::evidence::ENTITY_RETENTION_CAP {
            records.sort_by(|a, b| b.last_seen_at.cmp(&a.last_seen_at));
            records.truncate(crate::magician_v2::evidence::ENTITY_RETENTION_CAP);
        }
        self.ensure_base_layout().await?;
        let path = self.storage.user_work_entities_path();
        self.storage.write_json_atomic(&path, &records).await?;
        Ok(())
    }

    /// Unified evidence read: the agent's task-linked evidence ∪ the user's
    /// ambient evidence. The discovery seam consumers call instead of a single
    /// lane — new producers just add to a lane, consumers stay unchanged.
    pub async fn load_scoped_evidence(
        &self,
        agent_id: &str,
    ) -> Result<Vec<crate::magician_v2::evidence::EvidenceRecord>, AgentMemoryError> {
        let mut records = self.load_native_evidence(agent_id).await?;
        records.extend(self.load_user_work_evidence().await?);
        // Privacy: default-suppress affirmatively-sensitive evidence (financial,
        // health, credentials, private comms, …) from the unified read model that
        // reviews / dashboard / staleness consume. `work`/`unknown` flow normally;
        // agent task evidence is `unknown` so it is unaffected.
        records.retain(|r| !crate::magician_v2::evidence::is_sensitive(&r.sensitivity));
        Ok(records)
    }

    /// Unified entity read: agent-scoped ∪ user-owned anchors, deduped by
    /// `entity_key` (agent-scoped wins on collision).
    pub async fn load_scoped_entities(
        &self,
        agent_id: &str,
    ) -> Result<Vec<crate::magician_v2::evidence::EntityRecord>, AgentMemoryError> {
        let mut records = self.load_native_entities(agent_id).await?;
        let mut seen: std::collections::HashSet<String> =
            records.iter().map(|e| e.entity_key.clone()).collect();
        for entity in self.load_user_work_entities().await? {
            if seen.insert(entity.entity_key.clone()) {
                records.push(entity);
            }
        }
        Ok(records)
    }

    fn stamp_scoped_tier_record(&self, record: &V3MemoryTierRecord) -> V3MemoryTierRecord {
        let mut stamped = record.clone();
        if let Some((principal, workspace)) = self.scoped_memory_scope() {
            stamped.principal = Some(principal.to_string());
            stamped.workspace = Some(workspace.to_string());
        }
        stamped
    }

    fn stamp_scoped_episode_record(&self, episode: &V3EpisodeRecord) -> V3EpisodeRecord {
        let mut stamped = episode.clone();
        if let Some((principal, workspace)) = self.scoped_memory_scope() {
            stamped.principal = Some(principal.to_string());
            stamped.workspace = Some(workspace.to_string());
        }
        stamped
    }

    pub async fn load_profile(&self) -> Result<UserProfile, AgentMemoryError> {
        let path = self.storage.user_profile_path();
        match self.storage.read_json(&path).await {
            Ok(val) => Ok(val),
            Err(AgentStorageError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(UserProfile::default())
            },
            Err(err) => Err(err.into()),
        }
    }

    pub async fn save_profile(&self, profile: &UserProfile) -> Result<(), AgentMemoryError> {
        self.ensure_base_layout().await?;
        self.storage
            .write_json_atomic(self.storage.user_profile_path(), profile)
            .await?;
        Ok(())
    }

    pub async fn load_user_knowledge(&self) -> Result<Value, AgentMemoryError> {
        self.storage.load_user_knowledge().await.map_err(Into::into)
    }

    /// Persist the user-knowledge store, refusing a tree deeper than the
    /// process-wide retained-JSON contract.
    ///
    /// This store is the one place model-proposed structure is retained
    /// verbatim: the learning bridge, the consolidator's promotions and the
    /// memory tools all land here, and an entry's `value` is whatever the model
    /// produced. Every later reader — the resurfacing corpus stringifying an
    /// entry, `Value`'s own recursive `Drop` — then walks it.
    ///
    /// The guard is on the write and not the read deliberately. Bounding reads
    /// would turn one over-deep entry into an unreadable store, trading a
    /// bounded risk for total loss of the file; bounding writes keeps it out in
    /// the first place and leaves every existing file readable. `serde_json`'s
    /// own 128-level parse limit is a backstop, not this contract: it is twice
    /// the depth this codebase promises its consumers, and it is not a promise
    /// this code makes.
    ///
    /// The trade-off is that a store already holding an over-deep entry would
    /// refuse every later write until repaired, because each write re-saves the
    /// whole document. Measured across 76,662 files in a live scope root the
    /// deepest was 20, so nothing existing is near the limit — but a rejection
    /// names the depth it saw so the offending entry can be found rather than
    /// guessed at. Cost is one iterative pass per save, against a full atomic
    /// rewrite of the same document.
    /// Load-modify-save `knowledge.json` under the exclusive file lock.
    /// `mutate` returns whether the document changed.
    pub async fn update_user_knowledge<F>(&self, mutate: F) -> Result<(), AgentMemoryError>
    where
        F: FnOnce(&mut Value) -> Result<bool, AgentMemoryError>,
    {
        let path = self.storage.user_knowledge_path();
        let _flock = AgentStorage::acquire_file_lock_exclusive(&path).await?;
        let mut knowledge = self.load_user_knowledge().await?;
        if mutate(&mut knowledge)? {
            self.persist_user_knowledge(&knowledge).await?;
        }
        Ok(())
    }

    /// Write `knowledge.json`. Takes the exclusive lock so a bare save cannot
    /// race a confirm. Callers that already hold the lock must use
    /// [`Self::persist_user_knowledge`].
    pub async fn save_user_knowledge(&self, value: &Value) -> Result<(), AgentMemoryError> {
        let path = self.storage.user_knowledge_path();
        let _flock = AgentStorage::acquire_file_lock_exclusive(&path).await?;
        self.persist_user_knowledge(value).await
    }

    /// Write `knowledge.json` with no lock. Only for a caller that already
    /// holds `acquire_file_lock_exclusive` on this path.
    pub async fn persist_user_knowledge(&self, value: &Value) -> Result<(), AgentMemoryError> {
        // Iterative, so measuring depth cannot itself exhaust the stack.
        let metrics = crate::magician_v2::json_traversal::inspect_json(value);
        if metrics.max_depth > crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH {
            return Err(AgentMemoryError::Validation(format!(
                "user knowledge nests {} levels, above the retained-JSON limit of {}",
                metrics.max_depth,
                crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH
            )));
        }
        self.ensure_base_layout().await?;
        self.storage
            .write_json_atomic(self.storage.user_knowledge_path(), value)
            .await?;
        self.record_index_change(MemoryIndexChange::UserKnowledge, "user_knowledge_write")
            .await;
        Ok(())
    }

    /// Stamp `updated_at` only on entries that have none. Existing stamps are
    /// real age evidence and must not be rewritten (that would reset decay).
    pub async fn backfill_entry_timestamps(
        &self,
        now_secs: i64,
    ) -> Result<usize, AgentMemoryError> {
        let stamp = conservative_knowledge_stamp(&self.storage, now_secs);
        let mut stamped = 0_usize;
        self.update_user_knowledge(|knowledge| {
            let Some(tiers) = knowledge.as_object_mut() else {
                return Ok(false);
            };
            for (_tier, value) in tiers.iter_mut() {
                let Some(entries) = value.as_array_mut() else {
                    continue;
                };
                for entry in entries.iter_mut() {
                    let Some(object) = entry.as_object_mut() else {
                        continue;
                    };
                    if object
                        .get("updated_at")
                        .and_then(Value::as_str)
                        .is_some_and(|raw| !raw.trim().is_empty())
                    {
                        continue;
                    }
                    object.insert("updated_at".to_string(), Value::String(stamp.clone()));
                    stamped += 1;
                }
            }
            Ok(stamped > 0)
        })
        .await?;
        Ok(stamped)
    }

    /// Owner confirms an inferred entry. Writes `owner_confirmed` and keeps
    /// the prior `source_type` so "confirmed an insight" stays distinct from
    /// "the owner wrote this". Untrusted provenance cannot be promoted.
    pub async fn confirm_user_memory_entry(
        &self,
        tier: &str,
        key: &str,
        confirmed_at: i64,
    ) -> Result<(), AgentMemoryError> {
        let tier = tier.trim();
        let key = key.trim();
        if tier.is_empty() || key.is_empty() {
            return Err(AgentMemoryError::Validation(
                "tier and key are required to confirm a memory entry".to_string(),
            ));
        }
        if !super::memory_scope::is_owner_confirmable_memory_tier(tier) {
            return Err(AgentMemoryError::Validation(format!(
                "memory tier `{tier}` cannot be confirmed; only preferences and research_findings can become stated owner rules"
            )));
        }
        let kind = super::memory_provenance::MemoryKind::for_tier(tier);
        self.update_user_knowledge(|knowledge| {
            let Some(entries) = knowledge
                .as_object_mut()
                .and_then(|tiers| tiers.get_mut(tier))
                .and_then(Value::as_array_mut)
            else {
                return Err(AgentMemoryError::Validation(format!(
                    "memory tier `{tier}` is missing or not an entry list"
                )));
            };
            let Some(entry) = entries.iter_mut().find(|entry| {
                entry
                    .get("key")
                    .and_then(Value::as_str)
                    .is_some_and(|value| value == key)
            }) else {
                return Err(AgentMemoryError::Validation(format!(
                    "memory entry `{tier}/{key}` was not found"
                )));
            };
            let current_source = entry
                .get("source_type")
                .and_then(Value::as_str)
                .unwrap_or("insight")
                .to_string();
            let trust = super::memory_provenance::MemoryTrust::from_source_type(&current_source);
            if matches!(trust, super::memory_provenance::MemoryTrust::Stated) {
                return Ok(false);
            }
            if matches!(trust, super::memory_provenance::MemoryTrust::Untrusted) {
                return Err(AgentMemoryError::Validation(format!(
                    "untrusted memory `{tier}/{key}` cannot be promoted to a stated rule"
                )));
            }
            let Some(object) = entry.as_object_mut() else {
                return Err(AgentMemoryError::Validation(format!(
                    "memory entry `{tier}/{key}` is not an object"
                )));
            };
            object.insert("confirmed_from".to_string(), Value::String(current_source));
            object.insert(
                "source_type".to_string(),
                Value::String("owner_confirmed".to_string()),
            );
            object.insert(
                "confirmed_at".to_string(),
                Value::Number(confirmed_at.into()),
            );
            object.insert(
                "trust".to_string(),
                Value::String(
                    super::memory_provenance::MemoryTrust::Stated
                        .as_str()
                        .to_string(),
                ),
            );
            object.insert("kind".to_string(), Value::String(kind.as_str().to_string()));
            // Scope stays owner-edited. Auto-extract would attach on generic
            // verbs the moment the row becomes stated.
            Ok(true)
        })
        .await
    }

    /// Replace the extracted/editable scope on one entry. Empty or malformed
    /// scope is stored as omitted so it cannot later read as "applies to all".
    pub async fn set_user_memory_entry_scope(
        &self,
        tier: &str,
        key: &str,
        scope: Option<super::memory_scope::MemoryScope>,
    ) -> Result<(), AgentMemoryError> {
        let tier = tier.trim();
        let key = key.trim();
        if tier.is_empty() || key.is_empty() {
            return Err(AgentMemoryError::Validation(
                "tier and key are required to edit memory scope".to_string(),
            ));
        }
        if !super::memory_scope::is_owner_confirmable_memory_tier(tier) {
            return Err(AgentMemoryError::Validation(format!(
                "memory tier `{tier}` cannot carry an attachable scope"
            )));
        }
        if scope.as_ref().is_some_and(|value| {
            value.is_empty() || (value.topics.is_empty() && value.entities.is_empty())
        }) {
            return Err(AgentMemoryError::Validation(
                "empty or kind-only scope is refused; topics or entities are required to attach"
                    .to_string(),
            ));
        }
        self.update_user_knowledge(|knowledge| {
            let Some(entries) = knowledge
                .as_object_mut()
                .and_then(|tiers| tiers.get_mut(tier))
                .and_then(Value::as_array_mut)
            else {
                return Err(AgentMemoryError::Validation(format!(
                    "memory tier `{tier}` is missing or not an entry list"
                )));
            };
            let Some(entry) = entries.iter_mut().find_map(|entry| {
                let object = entry.as_object_mut()?;
                let matches = object.get("key").and_then(Value::as_str) == Some(key);
                matches.then_some(object)
            }) else {
                return Err(AgentMemoryError::Validation(format!(
                    "memory entry `{tier}/{key}` was not found"
                )));
            };
            match scope {
                Some(scope) => {
                    entry.insert(
                        "scope".to_string(),
                        serde_json::to_value(scope).unwrap_or(Value::Null),
                    );
                    entry.insert(
                        "scope_extracted_at".to_string(),
                        Value::Number(Utc::now().timestamp().into()),
                    );
                },
                None => {
                    entry.remove("scope");
                },
            }
            Ok(true)
        })
        .await
    }

    pub async fn list_user_memory_entries(&self) -> Result<Vec<Value>, AgentMemoryError> {
        Ok(self
            .list_user_memory_entries_page(
                0,
                200,
                super::memory_scope::OWNER_CONFIRMABLE_MEMORY_TIERS,
            )
            .await?
            .0)
    }

    pub async fn list_user_memory_entries_page(
        &self,
        offset: usize,
        limit: usize,
        tiers: &[&str],
    ) -> Result<(Vec<Value>, usize), AgentMemoryError> {
        let all = self.list_user_memory_entries_all(tiers).await?;
        let total = all.len();
        let limit = limit.clamp(1, 200);
        Ok((all.into_iter().skip(offset).take(limit).collect(), total))
    }

    async fn list_user_memory_entries_all(
        &self,
        allowed_tiers: &[&str],
    ) -> Result<Vec<Value>, AgentMemoryError> {
        let knowledge = self.load_user_knowledge().await?;
        let Some(tiers) = knowledge.as_object() else {
            return Ok(Vec::new());
        };
        if allowed_tiers.is_empty() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for (tier, entries) in tiers {
            if !allowed_tiers.iter().any(|name| *name == tier) {
                continue;
            }
            let Some(entries) = entries.as_array() else {
                continue;
            };
            for entry in entries {
                let Some(key) = entry.get("key").and_then(Value::as_str) else {
                    continue;
                };
                let source_type = entry.get("source_type").and_then(Value::as_str);
                let (trust, kind, may_suppress, may_condition, may_explain) =
                    super::memory_scope::entry_permissions(tier, source_type);
                let mut card = serde_json::json!({
                    "tier": tier,
                    "key": key,
                    "source_type": source_type.unwrap_or("insight"),
                    "trust": trust.as_str(),
                    "kind": kind.as_str(),
                    "may_suppress": may_suppress,
                    "may_condition_salience": may_condition,
                    "may_explain": may_explain,
                    "updated_at": entry.get("updated_at"),
                    "value": entry.get("value"),
                    "scope": entry.get("scope"),
                });
                if let Some(object) = card.as_object_mut() {
                    if let Some(confirmed_from) = entry.get("confirmed_from") {
                        object.insert("confirmed_from".to_string(), confirmed_from.clone());
                    }
                }
                out.push(card);
            }
        }
        Ok(out)
    }

    pub async fn build_context_profile(
        &self,
        agent_id: &str,
    ) -> Result<AgentContextProfile, AgentMemoryError> {
        validate_non_empty("agent_id", agent_id)?;
        let base = self.load_profile().await?;
        let corrections = self.load_corrections(agent_id).await?;

        let mut effective_preferences = HashMap::new();
        if let Some(Value::Object(preferences)) = base.sections.get("preferences") {
            for (key, value) in preferences {
                effective_preferences.insert(key.clone(), value.clone());
            }
        }

        for correction in &corrections {
            apply_correction_to_preferences(correction, &mut effective_preferences);
        }

        Ok(AgentContextProfile {
            base,
            corrections,
            effective_preferences,
        })
    }

    /// Resolve the filesystem path for a tier data file.
    ///
    /// Useful for callers that need to acquire a cross-process advisory lock
    /// (via [`AgentStorage::acquire_file_lock_exclusive`]) before performing
    /// read-modify-write operations on the tier.
    pub fn tier_data_path(
        &self,
        agent_id: &str,
        tier_definition: &MemoryTierDefinition,
        goal_id: Option<&str>,
    ) -> Result<std::path::PathBuf, AgentMemoryError> {
        Ok(self.storage.agent_tier_path(
            agent_id,
            &tier_definition.name,
            &tier_definition.scope,
            goal_id,
        )?)
    }

    pub async fn save_native_tier(
        &self,
        agent_id: &str,
        tier_definition: &MemoryTierDefinition,
        goal_id: Option<&str>,
        data: &V3MemoryTierRecord,
    ) -> Result<(), AgentMemoryError> {
        if data.tier_name != tier_definition.name {
            return Err(AgentMemoryError::Validation(format!(
                "tier payload mismatch: data.tier_name=`{}` does not match tier definition `{}`",
                data.tier_name, tier_definition.name
            )));
        }

        match tier_definition.scope {
            super::memory_tiers::TierScope::User => self.ensure_base_layout().await?,
            _ => {
                validate_non_empty("agent_id", agent_id)?;
                self.ensure_agent_layout(agent_id).await?;
            },
        }

        let path = self.storage.agent_tier_path(
            agent_id,
            &tier_definition.name,
            &tier_definition.scope,
            goal_id,
        )?;
        self.write_native_tier_record(&path, data).await?;
        self.record_index_change(
            MemoryIndexChange::NativeTier {
                agent_id: agent_id.to_string(),
                tier_name: tier_definition.name.clone(),
                scope: tier_definition.scope.clone(),
                goal_id: goal_id.map(ToOwned::to_owned),
            },
            "native_tier_write",
        )
        .await;
        Ok(())
    }

    pub async fn load_native_tier(
        &self,
        agent_id: &str,
        tier_definition: &MemoryTierDefinition,
        goal_id: Option<&str>,
    ) -> Result<Option<V3MemoryTierRecord>, AgentMemoryError> {
        if !matches!(tier_definition.scope, super::memory_tiers::TierScope::User) {
            validate_non_empty("agent_id", agent_id)?;
        }
        self.storage
            .load_native_tier_record::<V3MemoryTierRecord>(
                agent_id,
                &tier_definition.name,
                &tier_definition.scope,
                goal_id,
            )
            .await
            .map_err(Into::into)
    }

    pub async fn append_native_episode(
        &self,
        agent_id: &str,
        episode: &V3EpisodeRecord,
    ) -> Result<(), AgentMemoryError> {
        validate_non_empty("agent_id", agent_id)?;
        if episode.agent_id != agent_id {
            return Err(AgentMemoryError::Validation(format!(
                "episode.agent_id `{}` does not match path agent_id `{}`",
                episode.agent_id, agent_id
            )));
        }
        validate_non_empty("episode_id", &episode.episode_id)?;
        validate_non_empty("goal_key", episode.goal_id())?;

        self.ensure_agent_layout(agent_id).await?;

        let file_name = self.native_episode_file_name(episode);
        let path = self.storage.agent_episodes_dir(agent_id)?.join(&file_name);
        self.write_native_episode_record(&path, episode).await?;

        let index_lock = self.episode_index_lock_for_agent(agent_id).await;
        let _guard = index_lock.lock().await;
        let mut index = self.load_episode_index(agent_id).await.unwrap_or_default();
        if !index.entries.iter().any(|e| e.filename == file_name) {
            index.entries.push(EpisodeIndexEntry {
                filename: file_name,
                goal_id: episode.goal_id().to_string(),
                completed_at: Some(episode.completed_at.clone()),
            });
        }
        // Appending proves only that this one file is indexed. It must not turn
        // an already-invalidated index back into an authoritative complete
        // index; an out-of-band deletion or older missing write may still
        // require the next reader to rebuild it.
        if index.last_rebuilt.is_some() {
            index.last_rebuilt = Some(Utc::now());
        }
        if let Err(e) = self.save_episode_index(agent_id, &index).await {
            tracing::warn!(agent_id = %agent_id, error = %e, "failed to persist episode index after native append");
        }
        self.emit_episode_recorded_row(agent_id, episode);

        // Episodes are indexed like every other candidate, so a new one has to
        // reach the derived index. Without this the episode would rank by
        // keyword alone until the next dirty-scope rebuild, which waits 15-30
        // minutes by design. `record_index_change` also marks the scope dirty,
        // so a journal write that fails still degrades to that rebuild rather
        // than losing the episode from the index entirely.
        self.record_index_change(
            MemoryIndexChange::Episodes {
                agent_id: agent_id.to_string(),
            },
            "episode_appended",
        )
        .await;

        Ok(())
    }

    fn emit_episode_recorded_row(&self, agent_id: &str, episode: &V3EpisodeRecord) {
        let mut row = MemoryAnalyticsRow::now("memory_episode_recorded", "agent_memory_service");
        row.agent_id = Some(agent_id.to_string());
        row.goal_id = Some(episode.goal_key.clone());
        row.item_key = Some(episode.episode_id.clone());
        row.candidate_count = Some(episode.memory_candidates.len().min(u32::MAX as usize) as u32);
        row.status = "recorded".to_string();
        row.payload_json = json_payload(&serde_json::json!({
            "episode_id": episode.episode_id,
            "record_type": episode.record_type,
            "trigger_type": episode.trigger_type,
            "outcome_kind": episode.outcome_kind,
            "task_id": episode.task_id,
            "execution_id": episode.execution_id,
            "root_execution_id": episode.root_execution_id,
            "ui_thread_id": episode.ui_thread_id,
            "memory_candidate_types": episode
                .memory_candidates
                .iter()
                .map(|candidate| candidate.candidate_type.as_str())
                .collect::<Vec<_>>(),
        }));

        let mut rows = vec![row];
        if episode.memory_candidates.is_empty() {
            let mut skipped =
                MemoryAnalyticsRow::now("memory_capture_skipped", "agent_memory_service");
            skipped.agent_id = Some(agent_id.to_string());
            skipped.goal_id = Some(episode.goal_key.clone());
            skipped.item_key = Some(episode.episode_id.clone());
            skipped.candidate_count = Some(0);
            skipped.status = "no_memory_candidates".to_string();
            skipped.payload_json = json_payload(&serde_json::json!({
                "episode_id": episode.episode_id,
                "record_type": episode.record_type,
                "trigger_type": episode.trigger_type,
                "reason": "episode contained no structured memory_candidates"
            }));
            rows.push(skipped);
        }

        emit_rows_for_storage(&self.storage, rows);
    }

    pub async fn load_native_episodes(
        &self,
        agent_id: &str,
    ) -> Result<Vec<V3EpisodeRecord>, AgentMemoryError> {
        self.load_native_episodes_filtered(agent_id, None, None)
            .await
    }

    /// Read one immutable replay source without scanning an agent's entire
    /// episode history. The caller still checks its scoped identity and full
    /// revision against the captured decision input.
    pub(crate) async fn load_native_episode_by_id(
        &self,
        agent_id: &str,
        episode_id: &str,
    ) -> Result<Option<V3EpisodeRecord>, AgentMemoryError> {
        validate_non_empty("agent_id", agent_id)?;
        validate_non_empty("episode_id", episode_id)?;
        let path = self
            .storage
            .agent_episodes_dir(agent_id)?
            .join(self.native_episode_file_name_for_id_inner(episode_id));
        match tokio::fs::metadata(&path).await {
            Ok(metadata) if metadata.len() > 256 * 1024 => return Ok(None),
            Ok(_) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(AgentMemoryError::Io(error)),
        }
        match self.read_native_episode_record(&path).await {
            Ok(record)
                if record.agent_id == agent_id
                    && record.episode_id == episode_id
                    && validate_loaded_native_episode(agent_id, &record).is_ok() =>
            {
                Ok(Some(record))
            },
            Ok(_) => Ok(None),
            Err(AgentMemoryError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(None)
            },
            Err(error) => Err(error),
        }
    }

    /// Read only the newest bounded episode records. The index carries the
    /// ordering key so a caller asking for three summaries does not deserialize
    /// an agent's entire history. An old index is rebuilt once to populate that
    /// recoverable metadata; subsequent reads touch at most `limit * 4` files
    /// so stale/corrupt entries cannot turn a bounded projection into a scan.
    pub async fn load_recent_native_episodes(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<V3EpisodeRecord>, AgentMemoryError> {
        validate_non_empty("agent_id", agent_id)?;
        if limit == 0 {
            return Ok(Vec::new());
        }
        let dir = self.storage.agent_episodes_dir(agent_id)?;
        let mut index = self.load_episode_index(agent_id).await?;
        let index_is_orderable = index.last_rebuilt.is_some()
            && index.entries.iter().all(|entry| {
                entry
                    .completed_at
                    .as_deref()
                    .is_some_and(|value| DateTime::parse_from_rfc3339(value).is_ok())
            });
        if !index_is_orderable {
            index = self.rebuild_episode_index(agent_id).await?;
        }

        let mut entries = index
            .entries
            .into_iter()
            .filter(|entry| {
                !entry.filename.contains('/')
                    && !entry.filename.contains('\\')
                    && !entry.filename.contains("..")
                    && entry.filename.ends_with(".json")
                    && entry.completed_at.is_some()
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| {
            let left_completed = left
                .completed_at
                .as_deref()
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok());
            let right_completed = right
                .completed_at
                .as_deref()
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok());
            right_completed
                .cmp(&left_completed)
                .then_with(|| right.filename.cmp(&left.filename))
        });

        let candidate_cap = limit.saturating_mul(4).max(limit).min(64);
        let mut episodes = Vec::with_capacity(limit.min(candidate_cap));
        for entry in entries.into_iter().take(candidate_cap) {
            let path = dir.join(entry.filename);
            let Ok(episode) = self.read_native_episode_record(&path).await else {
                continue;
            };
            if validate_loaded_native_episode(agent_id, &episode).is_err()
                || path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_none_or(|name| name != self.native_episode_file_name(&episode))
            {
                continue;
            }
            episodes.push(episode);
            if episodes.len() == limit {
                break;
            }
        }
        Ok(episodes)
    }

    pub async fn load_native_episodes_for_goal(
        &self,
        agent_id: &str,
        goal_id: &str,
    ) -> Result<Vec<V3EpisodeRecord>, AgentMemoryError> {
        validate_non_empty("goal_id", goal_id)?;
        self.load_native_episodes_filtered(agent_id, Some(goal_id), None)
            .await
    }

    pub async fn recall_native_episodes(
        &self,
        agent_id: &str,
        goal_id: &str,
        seq_range: Option<(u64, u64)>,
        limit: Option<usize>,
    ) -> Result<Vec<V3EpisodeRecord>, AgentMemoryError> {
        validate_non_empty("goal_id", goal_id)?;
        if let Some((start, end)) = seq_range {
            if start > end {
                return Err(AgentMemoryError::Validation(
                    "seq_range start must be <= end".to_string(),
                ));
            }
        }
        let effective_limit = normalize_episode_recall_limit(limit)?;
        let mut episodes = self
            .load_native_episodes_filtered(agent_id, Some(goal_id), seq_range)
            .await?;
        episodes.sort_by(|left, right| {
            compare_native_episode_order(left, right)
                .then_with(|| left.episode_id.cmp(&right.episode_id))
        });
        if episodes.len() > effective_limit {
            let offset = episodes.len().saturating_sub(effective_limit);
            episodes = episodes.split_off(offset);
        }
        Ok(episodes)
    }

    /// Select the most effective strategy for a goal from candidate strategy names.
    ///
    /// Selection uses `StrategyEffectiveness` from `memory_tiers.rs`:
    /// 1. Highest `success_rate()` (returns 0.5 neutral prior when <5 data points)
    /// 2. Highest attempt count (tie-break)
    /// 3. Most recent completion timestamp (tie-break)
    /// 4. Lexicographic (tie-break for determinism)
    ///
    /// Returns `None` when no candidate has any matching episode history.
    pub async fn select_effective_strategy_for_goal(
        &self,
        agent_id: &str,
        goal_id: &str,
        candidates: &[&str],
    ) -> Result<Option<String>, AgentMemoryError> {
        validate_non_empty("agent_id", agent_id)?;
        validate_non_empty("goal_id", goal_id)?;

        let mut candidates_by_key: HashMap<String, String> = HashMap::new();
        for candidate in candidates {
            let Some(normalized) = normalize_strategy_key(candidate) else {
                continue;
            };
            candidates_by_key
                .entry(normalized)
                .or_insert_with(|| candidate.trim().to_string());
        }
        if candidates_by_key.is_empty() {
            return Ok(None);
        }

        let episodes = self
            .load_native_episodes_filtered(agent_id, Some(goal_id), None)
            .await?;
        if episodes.is_empty() {
            return Ok(None);
        }

        // Build StrategyEffectiveness from episode records.
        let mut eff = StrategyEffectiveness::default();
        // Track per-candidate attempt counts and recency for tie-breaking.
        let mut attempt_counts: HashMap<String, usize> = HashMap::new();
        let mut last_completed: HashMap<String, chrono::DateTime<chrono::Utc>> = HashMap::new();
        let mut any_match = false;

        for episode in episodes {
            let Some(summary) = episode.strategy_summary.as_deref() else {
                continue;
            };
            let Some(key) = normalize_strategy_key(summary) else {
                continue;
            };
            let Some(candidate_name) = candidates_by_key.get(&key) else {
                continue;
            };

            any_match = true;
            eff.record(StrategyRecord {
                goal_id: goal_id.to_string(),
                strategy_type: candidate_name.clone(),
                succeeded: episode.outcome_is_succeeded(),
                execution_time_ms: 0,
                actions_count: 0,
                timestamp: episode.completed_at_dt().map_err(|err| {
                    AgentMemoryError::Validation(format!(
                        "invalid completed_at on episode `{}`: {err}",
                        episode.episode_id
                    ))
                })?,
            });

            *attempt_counts.entry(candidate_name.clone()).or_default() += 1;
            last_completed
                .entry(candidate_name.clone())
                .and_modify(|existing| {
                    if let Ok(completed_at) = episode.completed_at_dt() {
                        *existing = (*existing).max(completed_at);
                    }
                })
                .or_insert(episode.completed_at_dt().map_err(|err| {
                    AgentMemoryError::Validation(format!(
                        "invalid completed_at on episode `{}`: {err}",
                        episode.episode_id
                    ))
                })?);
        }

        if !any_match {
            return Ok(None);
        }

        // Rank candidates by success_rate (primary), attempts, recency, lexicographic.
        let mut ranked: Vec<String> = attempt_counts.keys().cloned().collect();
        ranked.sort_by(|left, right| {
            let left_rate = eff.success_rate(goal_id, left);
            let right_rate = eff.success_rate(goal_id, right);
            right_rate
                .partial_cmp(&left_rate)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    attempt_counts
                        .get(right)
                        .unwrap_or(&0)
                        .cmp(attempt_counts.get(left).unwrap_or(&0))
                })
                .then_with(|| last_completed.get(right).cmp(&last_completed.get(left)))
                .then_with(|| left.cmp(right))
        });

        Ok(ranked.into_iter().next())
    }

    pub async fn load_native_tier_by_name(
        &self,
        agent_id: &str,
        tier_name: &str,
        tier_definitions: &[MemoryTierDefinition],
        goal_id: Option<&str>,
    ) -> Result<Option<V3MemoryTierRecord>, AgentMemoryError> {
        let tier_def = tier_definitions.iter().find(|t| t.name == tier_name);
        let Some(tier_def) = tier_def else {
            return Ok(None);
        };
        self.load_native_tier(agent_id, tier_def, goal_id).await
    }

    pub async fn save_native_tier_by_name(
        &self,
        agent_id: &str,
        tier_name: &str,
        tier_definitions: &[MemoryTierDefinition],
        goal_id: Option<&str>,
        data: &V3MemoryTierRecord,
    ) -> Result<(), AgentMemoryError> {
        let tier_def = tier_definitions.iter().find(|t| t.name == tier_name);
        let Some(tier_def) = tier_def else {
            return Err(AgentMemoryError::Validation(format!(
                "tier definition not found for name `{}`",
                tier_name
            )));
        };
        self.save_native_tier(agent_id, tier_def, goal_id, data)
            .await
    }

    /// Load the episode index from disk, or return empty if missing.
    pub async fn load_episode_index(
        &self,
        agent_id: &str,
    ) -> Result<EpisodeIndex, AgentMemoryError> {
        validate_non_empty("agent_id", agent_id)?;
        let path = self.storage.agent_episode_index_path(agent_id)?;
        match self.storage.read_json(&path).await {
            Ok(index) => Ok(index),
            Err(AgentStorageError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(EpisodeIndex::default())
            },
            Err(err) => Err(err.into()),
        }
    }

    /// Mark the cached episode index stale (`last_rebuilt = None`) so
    /// the next `load_native_episodes_filtered` falls back to scanning
    /// the directory. Use after deleting episode files outside the
    /// canonical `delete_native_episodes` flow (e.g. chat-side
    /// `forget_memory` which removes by ranked path rather than
    /// passing whole `V3EpisodeRecord`s back through the storage
    /// layer). Idempotent — repeated calls just rewrite the same
    /// `last_rebuilt = None` value.
    pub async fn invalidate_episode_index(&self, agent_id: &str) -> Result<(), AgentMemoryError> {
        validate_non_empty("agent_id", agent_id)?;
        let index_lock = self.episode_index_lock_for_agent(agent_id).await;
        let _guard = index_lock.lock().await;
        let mut index = self.load_episode_index(agent_id).await.unwrap_or_default();
        index.last_rebuilt = None;
        self.save_episode_index(agent_id, &index).await
    }

    /// Persist the episode index to disk.
    pub async fn save_episode_index(
        &self,
        agent_id: &str,
        index: &EpisodeIndex,
    ) -> Result<(), AgentMemoryError> {
        validate_non_empty("agent_id", agent_id)?;
        self.ensure_agent_layout(agent_id).await?;
        let path = self.storage.agent_episode_index_path(agent_id)?;
        self.storage.write_json_atomic(path, index).await?;
        Ok(())
    }

    /// Full rebuild: scan episode directory, read each file's metadata,
    /// build fresh index. Called on first access or when index is stale.
    pub async fn rebuild_episode_index(
        &self,
        agent_id: &str,
    ) -> Result<EpisodeIndex, AgentMemoryError> {
        validate_non_empty("agent_id", agent_id)?;
        let index_lock = self.episode_index_lock_for_agent(agent_id).await;
        let _guard = index_lock.lock().await;
        self.rebuild_episode_index_locked(agent_id).await
    }

    async fn rebuild_episode_index_locked(
        &self,
        agent_id: &str,
    ) -> Result<EpisodeIndex, AgentMemoryError> {
        let dir = self.storage.agent_episodes_dir(agent_id)?;
        let files = self.storage.list_files_with_extension(&dir, "json").await?;
        let mut entries = Vec::new();
        let mut seen_episode_ids: HashMap<String, usize> = HashMap::new();

        for file in &files {
            match self.read_native_episode_record(file).await {
                Ok(episode) => {
                    if validate_loaded_native_episode(agent_id, &episode).is_err() {
                        continue;
                    }
                    let filename = file
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_string();
                    if filename != self.native_episode_file_name(&episode) {
                        continue;
                    }
                    if let Some(existing_index) = seen_episode_ids.get(&episode.episode_id).copied()
                    {
                        entries[existing_index] = EpisodeIndexEntry {
                            filename,
                            goal_id: episode.goal_id().to_string(),
                            completed_at: Some(episode.completed_at.clone()),
                        };
                    } else {
                        seen_episode_ids.insert(episode.episode_id.clone(), entries.len());
                        entries.push(EpisodeIndexEntry {
                            filename,
                            goal_id: episode.goal_id().to_string(),
                            completed_at: Some(episode.completed_at.clone()),
                        });
                    }
                },
                Err(_) => continue,
            }
        }

        let index = EpisodeIndex {
            entries,
            last_rebuilt: Some(Utc::now()),
        };
        self.save_episode_index(agent_id, &index).await?;
        Ok(index)
    }

    async fn load_native_episodes_filtered(
        &self,
        agent_id: &str,
        goal_id_filter: Option<&str>,
        seq_range: Option<(u64, u64)>,
    ) -> Result<Vec<V3EpisodeRecord>, AgentMemoryError> {
        validate_non_empty("agent_id", agent_id)?;
        let dir = self.storage.agent_episodes_dir(agent_id)?;

        let index = self.load_episode_index(agent_id).await?;
        let index_usable = index.last_rebuilt.is_some_and(|rebuilt| {
            Utc::now().signed_duration_since(rebuilt) < chrono::Duration::hours(1)
        });

        let files: Vec<std::path::PathBuf> = if index_usable && !index.entries.is_empty() {
            index
                .entries
                .iter()
                .filter(|entry| {
                    if entry.filename.contains('/')
                        || entry.filename.contains('\\')
                        || entry.filename.contains("..")
                        || !entry.filename.ends_with(".json")
                    {
                        return false;
                    }
                    if let Some(goal_id) = goal_id_filter {
                        entry.goal_id == goal_id
                    } else {
                        true
                    }
                })
                .map(|entry| dir.join(entry.filename.as_str()))
                .collect()
        } else {
            self.storage.list_files_with_extension(&dir, "json").await?
        };

        let mut episodes = Vec::new();
        let mut seen_episode_ids: HashMap<String, usize> = HashMap::new();
        let mut saw_missing_index_entry = false;

        for file in files {
            match self.read_native_episode_record(&file).await {
                Ok(episode) => {
                    if let Err(reason) = validate_loaded_native_episode(agent_id, &episode) {
                        tracing::warn!(
                            path = %file.display(),
                            reason = %reason,
                            "skipping invalid native episode file"
                        );
                        continue;
                    }
                    let is_canonical = file
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name == self.native_episode_file_name(&episode));
                    if !is_canonical {
                        continue;
                    }
                    if goal_id_filter.is_some_and(|goal_id| episode.goal_id() != goal_id) {
                        continue;
                    }
                    if let Some((start_seq, end_seq)) = seq_range {
                        if episode.trigger_seq < start_seq || episode.trigger_seq > end_seq {
                            continue;
                        }
                    }
                    if let Some(existing_index) = seen_episode_ids.get(&episode.episode_id).copied()
                    {
                        episodes[existing_index] = episode;
                    } else {
                        seen_episode_ids.insert(episode.episode_id.clone(), episodes.len());
                        episodes.push(episode);
                    }
                },
                Err(AgentMemoryError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                    saw_missing_index_entry = true;
                    tracing::debug!(
                        path = %file.display(),
                        error = %err,
                        "skipping missing native episode file from stale index"
                    );
                },
                Err(err) => {
                    tracing::warn!(
                        path = %file.display(),
                        error = %err,
                        "skipping unreadable native episode file"
                    );
                },
            }
        }

        if saw_missing_index_entry && index_usable {
            if let Err(err) = self.rebuild_episode_index(agent_id).await {
                tracing::warn!(
                    agent_id = %agent_id,
                    error = %err,
                    "failed to rebuild native episode index after missing entries"
                );
            }
        }

        episodes.sort_by(compare_native_episode_order);
        Ok(episodes)
    }

    pub async fn find_expiring_native_episodes(
        &self,
        agent_id: &str,
        retention: &EpisodeRetention,
        now: DateTime<Utc>,
    ) -> Result<Vec<V3EpisodeRecord>, AgentMemoryError> {
        let episodes = self.load_native_episodes(agent_id).await?;
        Ok(episodes
            .into_iter()
            .filter(|episode| native_episode_is_expired(episode, retention, now))
            .collect())
    }

    pub async fn delete_native_episodes(
        &self,
        agent_id: &str,
        episodes: &[V3EpisodeRecord],
    ) -> Result<usize, AgentMemoryError> {
        validate_non_empty("agent_id", agent_id)?;
        if episodes.is_empty() {
            return Ok(0);
        }

        let episodes_dir = self.storage.agent_episodes_dir(agent_id)?;
        let mut deleted = 0usize;
        for episode in episodes {
            if episode.agent_id != agent_id {
                return Err(AgentMemoryError::Validation(format!(
                    "episode.agent_id `{}` does not match delete path agent_id `{}`",
                    episode.agent_id, agent_id
                )));
            }
            let path = episodes_dir.join(self.native_episode_file_name(episode));
            if self.storage.exists(&path).await? {
                self.storage.remove_file(&path).await?;
                deleted += 1;
            }
        }
        if deleted > 0 {
            let index_lock = self.episode_index_lock_for_agent(agent_id).await;
            let _guard = index_lock.lock().await;
            let mut index = self.load_episode_index(agent_id).await.unwrap_or_default();
            index.last_rebuilt = None;
            if let Err(e) = self.save_episode_index(agent_id, &index).await {
                tracing::warn!(agent_id = %agent_id, error = %e, "failed to invalidate native episode index after delete");
            }
        }
        if deleted > 0 {
            // Episodes are indexed now, so a delete has to reach the derived
            // index the same way an append does. Without this a deleted or
            // retention-expired episode keeps its LanceDB row and its cached
            // relevance, so `search_memory` would go on ranking and returning
            // the text of a file that is gone, with a `source_path` pointing at
            // nothing for a follow-up `forget_memory` to act on.
            self.record_index_change(
                MemoryIndexChange::Episodes {
                    agent_id: agent_id.to_string(),
                },
                "episodes_deleted",
            )
            .await;
        }
        Ok(deleted)
    }

    /// Check whether an episode with the given trigger sequence already exists
    /// for the goal in native V3 storage.
    pub async fn has_episode_for_trigger(
        &self,
        agent_id: &str,
        goal_id: &str,
        trigger_seq: u64,
    ) -> Result<bool, AgentMemoryError> {
        validate_non_empty("agent_id", agent_id)?;
        validate_non_empty("goal_id", goal_id)?;
        Ok(!self
            .load_native_episodes_filtered(
                agent_id,
                Some(goal_id),
                Some((trigger_seq, trigger_seq)),
            )
            .await?
            .is_empty())
    }

    pub async fn append_correction(
        &self,
        agent_id: &str,
        correction: &Correction,
    ) -> Result<(), AgentMemoryError> {
        validate_non_empty("agent_id", agent_id)?;
        validate_non_empty("correction_id", &correction.correction_id)?;
        if correction.agent_id != agent_id {
            return Err(AgentMemoryError::Validation(format!(
                "correction.agent_id `{}` does not match path agent_id `{}`",
                correction.agent_id, agent_id
            )));
        }

        self.ensure_agent_layout(agent_id).await?;
        let path = self.storage.agent_corrections_path(agent_id)?;

        let append_lock = self.correction_append_lock_for_agent(agent_id).await;
        let _guard = append_lock.lock().await;
        let _file_lock = acquire_corrections_append_lock(&self.storage, agent_id).await?;
        self.storage.append_jsonl(&path, correction).await?;
        Ok(())
    }

    pub async fn load_corrections(
        &self,
        agent_id: &str,
    ) -> Result<Vec<Correction>, AgentMemoryError> {
        validate_non_empty("agent_id", agent_id)?;
        let path = self.storage.agent_corrections_path(agent_id)?;
        let content = match self.storage.read_to_string(&path).await {
            Ok(c) => c,
            Err(AgentStorageError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Vec::new());
            },
            Err(err) => return Err(AgentMemoryError::Storage(err)),
        };
        let mut corrections = Vec::new();

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            match serde_json::from_str::<Correction>(trimmed) {
                Ok(correction) => corrections.push(correction),
                Err(err) => {
                    tracing::warn!(error = %err, "skipping invalid correction line");
                },
            }
        }

        corrections.sort_by_key(|c| c.timestamp);
        Ok(corrections)
    }

    async fn correction_append_lock_for_agent(&self, agent_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.correction_append_locks.lock().await;
        locks
            .entry(agent_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub async fn episode_index_lock_for_agent(&self, agent_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.episode_index_locks.lock().await;
        locks
            .entry(agent_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

fn normalize_episode_recall_limit(limit: Option<usize>) -> Result<usize, AgentMemoryError> {
    let limit = limit.unwrap_or(EPISODE_RECALL_DEFAULT_LIMIT);
    if limit == 0 {
        return Err(AgentMemoryError::Validation(
            "episode recall limit must be > 0".to_string(),
        ));
    }
    Ok(limit.min(EPISODE_RECALL_MAX_LIMIT))
}

fn validate_loaded_native_episode(agent_id: &str, episode: &V3EpisodeRecord) -> Result<(), String> {
    if episode.agent_id != agent_id {
        return Err(format!(
            "episode.agent_id `{}` does not match path agent_id `{}`",
            episode.agent_id, agent_id
        ));
    }
    if episode.episode_id.trim().is_empty() {
        return Err("episode.episode_id must not be empty".to_string());
    }
    if episode.goal_id().trim().is_empty() {
        return Err("episode.goal_key must not be empty".to_string());
    }
    Ok(())
}

fn compare_native_episode_order(
    left: &V3EpisodeRecord,
    right: &V3EpisodeRecord,
) -> std::cmp::Ordering {
    let left_completed = left.completed_at_dt().ok();
    let right_completed = right.completed_at_dt().ok();
    left_completed
        .cmp(&right_completed)
        .then_with(|| left.goal_id().cmp(right.goal_id()))
        .then_with(|| left.trigger_seq.cmp(&right.trigger_seq))
        .then_with(|| left.episode_id.cmp(&right.episode_id))
}

fn native_episode_is_expired(
    episode: &V3EpisodeRecord,
    retention: &EpisodeRetention,
    now: DateTime<Utc>,
) -> bool {
    let retention_days = if let Some(days) = retention.per_goal_override.get(episode.goal_id()) {
        *days
    } else if episode.outcome_is_failed() {
        retention.on_failure.unwrap_or(retention.default_days)
    } else {
        retention.default_days
    }
    .max(1);

    match episode.completed_at_dt() {
        Ok(completed_at) => completed_at + chrono::Duration::days(i64::from(retention_days)) <= now,
        Err(_) => false,
    }
}

async fn acquire_corrections_append_lock(
    storage: &AgentStorage,
    agent_id: &str,
) -> Result<CorrectionsAppendLockGuard, AgentMemoryError> {
    acquire_corrections_append_lock_with_config(
        storage,
        agent_id,
        CorrectionsAppendLockConfig::default(),
    )
    .await
}

async fn acquire_corrections_append_lock_with_config(
    storage: &AgentStorage,
    agent_id: &str,
    config: CorrectionsAppendLockConfig,
) -> Result<CorrectionsAppendLockGuard, AgentMemoryError> {
    let lock_path = storage.agent_corrections_lock_path(agent_id)?;
    let lock_path_for_open = lock_path.clone();
    let mut file = task::spawn_blocking(move || {
        std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path_for_open)
    })
    .await
    .map_err(|join_err| {
        std::io::Error::other(format!(
            "failed to join corrections lock-open task: {join_err}"
        ))
    })??;

    let started = Instant::now();
    let mut retry_delay = config.retry_delay_min;

    loop {
        let (next_file, lock_result) = task::spawn_blocking(move || {
            let lock_result = file.try_lock();
            (file, lock_result)
        })
        .await
        .map_err(|join_err| {
            std::io::Error::other(format!(
                "failed to join corrections lock-attempt task: {join_err}"
            ))
        })?;
        file = next_file;

        match lock_result {
            Ok(()) => {
                return Ok(CorrectionsAppendLockGuard {
                    _file: file,
                    _lock_path: lock_path,
                });
            },
            Err(std::fs::TryLockError::WouldBlock) => {
                if started.elapsed() >= config.max_wait {
                    return Err(AgentMemoryError::CorrectionsLockTimeout {
                        lock_path: lock_path.display().to_string(),
                        wait_ms: u64::try_from(config.max_wait.as_millis()).unwrap_or(u64::MAX),
                    });
                }
                tokio::time::sleep(retry_delay).await;
                retry_delay = retry_delay.saturating_mul(2).min(config.retry_delay_max);
            },
            Err(std::fs::TryLockError::Error(err)) => return Err(err.into()),
        }
    }
}

fn validate_non_empty(field: &str, value: &str) -> Result<(), AgentMemoryError> {
    if value.trim().is_empty() {
        return Err(AgentMemoryError::Validation(format!(
            "{field} must not be empty"
        )));
    }
    Ok(())
}

fn parse_scalar_rule_value(raw: &str) -> Value {
    let trimmed = raw.trim();
    if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
        return parsed;
    }
    Value::String(trimmed.to_string())
}

fn normalize_strategy_key(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let normalized = trimmed
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    (!normalized.is_empty()).then_some(normalized)
}

fn apply_correction_to_preferences(
    correction: &Correction,
    effective_preferences: &mut HashMap<String, Value>,
) {
    // Preferred structured form: generalized_rule is a JSON object of preference keys.
    if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&correction.generalized_rule) {
        for (key, value) in map {
            if !key.trim().is_empty() {
                effective_preferences.insert(key, value);
            }
        }
        return;
    }

    // Secondary structured form: "key=value" or "key: value".
    // Use `": "` (colon-space) for the colon form so we don't mis-parse URLs like
    // "https://example.com" as key="https", value="//example.com".
    let parse_pair = correction
        .generalized_rule
        .split_once('=')
        .or_else(|| correction.generalized_rule.split_once(": "));
    if let Some((key, value)) = parse_pair {
        let key = key.trim();
        if !key.is_empty() && !key.contains(' ') {
            effective_preferences.insert(key.to_string(), parse_scalar_rule_value(value));
            return;
        }
    }

    // Fallback for unstructured rules: keep a stable correction-derived entry.
    effective_preferences.insert(
        format!("correction.{}", correction.correction_id),
        Value::String(correction.generalized_rule.clone()),
    );
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeIndexEntry {
    pub filename: String,
    pub goal_id: String,
    #[serde(default)]
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EpisodeIndex {
    pub entries: Vec<EpisodeIndexEntry>,
    pub last_rebuilt: Option<DateTime<Utc>>,
}

/// Outcome of an agent execution episode.
///
/// Serializes with serde's default enum representation (externally tagged,
/// PascalCase variant names). This is deliberate — episode JSON is internal
/// storage, and PascalCase makes the variant name visually distinct from
/// the field keys inside each variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EpisodeOutcome {
    GoalAchieved {
        summary: String,
    },
    PartialProgress {
        summary: String,
        remaining: String,
    },
    Failed {
        error: String,
    },
    UserIntervened {
        reason: String,
    },
    BudgetExhausted,
    Paused {
        pending_actions: Vec<String>,
    },
    CircuitOpen {
        failure_count: usize,
        last_error: String,
    },
}

impl EpisodeOutcome {
    pub fn is_succeeded(&self) -> bool {
        matches!(self, Self::GoalAchieved { .. })
    }

    pub fn is_failed(&self) -> bool {
        matches!(
            self,
            Self::Failed { .. } | Self::BudgetExhausted | Self::CircuitOpen { .. }
        )
    }

    pub fn is_paused(&self) -> bool {
        matches!(self, Self::Paused { .. })
    }

    pub fn error_summary(&self) -> String {
        match self {
            Self::Failed { error } => error.clone(),
            Self::CircuitOpen { last_error, .. } => last_error.clone(),
            Self::BudgetExhausted => "Budget exhausted".to_string(),
            _ => String::new(),
        }
    }

    pub fn summary(&self) -> &str {
        match self {
            Self::GoalAchieved { summary } => summary,
            Self::PartialProgress { summary, .. } => summary,
            Self::Failed { error } => error,
            Self::Paused { .. } => "Paused (awaiting approval)",
            Self::CircuitOpen { .. } => "Circuit open (escalated)",
            Self::UserIntervened { reason } => reason,
            Self::BudgetExhausted => "Budget exhausted",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserProfile {
    pub profile_id: String,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub sections: HashMap<String, Value>,
}

impl Default for UserProfile {
    fn default() -> Self {
        Self {
            profile_id: "default-profile".to_string(),
            updated_at: Utc::now(),
            sections: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentContextProfile {
    pub base: UserProfile,
    pub corrections: Vec<Correction>,
    pub effective_preferences: HashMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Correction {
    pub correction_id: String,
    pub agent_id: String,
    pub timestamp: DateTime<Utc>,
    pub trigger: String,
    pub correction: String,
    pub generalized_rule: String,
    #[serde(default)]
    pub affected_episodes: Vec<String>,
    pub category: CorrectionCategory,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionCategory {
    Targeting,
    Timing,
    Action,
    Preference,
    Safety,
    #[serde(other)]
    Unknown,
}

fn contribution_candidate_matches_target(
    candidate: &crate::magician_v2::apps::memory::AppMemoryCandidate,
    target: &crate::magician_v2::apps::memory_store::AppMemoryPromptTarget,
) -> bool {
    use crate::magician_v2::apps::memory::AppMemoryTierScope;
    match (&candidate.intended_tier_scope, target) {
        (
            AppMemoryTierScope::User,
            crate::magician_v2::apps::memory_store::AppMemoryPromptTarget::User,
        ) => true,
        (
            AppMemoryTierScope::Agent { agent_id },
            crate::magician_v2::apps::memory_store::AppMemoryPromptTarget::Agent {
                agent_id: expected,
            },
        ) => app_reference_matches_target(agent_id.as_str(), "agent", expected),
        (
            AppMemoryTierScope::AgentGoal { agent_id, goal_id },
            crate::magician_v2::apps::memory_store::AppMemoryPromptTarget::AgentGoal {
                agent_id: expected_agent,
                goal_id: expected_goal,
            },
        ) => {
            app_reference_matches_target(agent_id.as_str(), "agent", expected_agent)
                && app_reference_matches_target(goal_id.as_str(), "goal", expected_goal)
        },
        _ => false,
    }
}

fn app_reference_matches_target(reference: &str, kind: &str, expected: &str) -> bool {
    reference == expected
        || reference
            .strip_prefix(&format!("{kind}:"))
            .is_some_and(|value| value == expected)
}

fn personal_agent_retrieval_system_scope(
    principal: &str,
    workspace: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<crate::magician_v2::apps::authority::AuthenticatedAppScope, AgentMemoryError> {
    use crate::magician_v2::apps::{
        authority::AuthenticatedAppScope,
        models::{AppDigest, AppReference, AppScopeBindingRef},
        records::AppScope,
    };

    let scope = AppScope {
        principal: AppReference::parse(principal.to_owned())
            .map_err(|error| AgentMemoryError::Validation(error.to_string()))?,
        workspace: AppReference::parse(workspace.to_owned())
            .map_err(|error| AgentMemoryError::Validation(error.to_string()))?,
    };
    let scope_digest = AppDigest::blake3(format!("{principal}\0{workspace}").as_bytes());
    let scope_binding_ref = AppScopeBindingRef::parse(format!(
        "scope_{}",
        scope_digest.as_str().trim_start_matches("blake3:")
    ))
    .map_err(|error| AgentMemoryError::Validation(error.to_string()))?;
    let run_digest = AppDigest::blake3(
        format!(
            "{principal}\0{workspace}\0{}",
            now.timestamp_nanos_opt()
                .unwrap_or_else(|| now.timestamp_micros().saturating_mul(1_000))
        )
        .as_bytes(),
    );
    AuthenticatedAppScope::from_system_worker(
        scope,
        scope_binding_ref,
        AppReference::parse("worker:personal-agent-retrieval-prompt")
            .map_err(|error| AgentMemoryError::Validation(error.to_string()))?,
        AppReference::parse(format!(
            "run:personal-agent-retrieval-prompt:{}",
            run_digest.as_str().trim_start_matches("blake3:")
        ))
        .map_err(|error| AgentMemoryError::Validation(error.to_string()))?,
        now.clone(),
        now + chrono::Duration::minutes(2),
    )
    .map_err(|error| AgentMemoryError::Validation(error.to_string()))
}

fn retrieval_proposal_to_memory_candidate(
    principal: &str,
    workspace: &str,
    proposal: magician_app_contract::contribution::AppPersonalAgentRetrievalProjectionProposalV1,
) -> Result<
    crate::magician_v2::apps::memory::AppMemoryCandidate,
    Box<dyn std::error::Error + Send + Sync>,
> {
    use crate::magician_v2::apps::memory::{
        AppMemoryCandidate, AppMemoryCandidateStatus, AppMemorySemanticDestination,
        AppMemorySourceRef, AppMemoryTierScope,
    };
    use crate::magician_v2::apps::models::{
        AppDataClassification, AppDigest, AppFieldPath, AppHandlingLabels, AppInstallationId,
        AppModelProcessing, AppName, AppProtocolVersion, AppRecordId, AppReference, AppRevision,
        AppSourceRef, AppSourceRefKind,
    };
    use crate::magician_v2::apps::records::AppScope;
    use magician_app_contract::contribution::{
        AppContributionClassification, AppContributionModelProcessing,
    };

    proposal.validate()?;
    let labels = |value: &magician_app_contract::contribution::AppContributionHandlingLabelsV1| {
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(AppHandlingLabels {
            classification: match value.classification {
                AppContributionClassification::Public => AppDataClassification::Public,
                AppContributionClassification::Internal => AppDataClassification::Ordinary,
                AppContributionClassification::Personal => AppDataClassification::Personal,
                AppContributionClassification::Sensitive => AppDataClassification::Sensitive,
                AppContributionClassification::Secret => AppDataClassification::Secret,
            },
            model_processing: match value.model_processing {
                AppContributionModelProcessing::None => AppModelProcessing::None,
                AppContributionModelProcessing::LocalOnly => AppModelProcessing::LocalOnly,
                AppContributionModelProcessing::RemoteAllowed => AppModelProcessing::RemoteAllowed,
            },
            policy_digest: AppDigest::parse(value.policy_digest.clone())?,
            provenance_digest: AppDigest::parse(value.provenance_digest.clone())?,
        })
    };
    let mut source_refs = Vec::with_capacity(proposal.header.sources.len());
    for source in &proposal.header.sources {
        let fields = source
            .selected_fields
            .iter()
            .cloned()
            .map(AppFieldPath::parse)
            .collect::<Result<Vec<_>, _>>()?;
        source_refs.push(AppMemorySourceRef {
            installation_id: AppInstallationId::parse(source.installation_id.clone())?,
            package_revision_ref: AppReference::parse(
                proposal.header.package_revision_ref.clone(),
            )?,
            grant_revision: AppRevision::new(proposal.header.grant_revision)?,
            schema_revision: AppRevision::new(proposal.header.schema_revision)?,
            entity_name: AppName::parse(source.entity_name.clone())?,
            record_id: AppRecordId::parse(source.record_id.clone())?,
            record_revision: AppRevision::new(source.record_revision)?,
            selected_fields: fields.clone(),
            canonical_source_ref: AppSourceRef {
                kind: AppSourceRefKind::EntityRecord,
                reference: AppReference::parse(source.canonical_source_ref.clone())?,
                revision: Some(AppRevision::new(source.record_revision)?),
                fields,
            },
            handling_labels: labels(&source.handling_labels)?,
        });
    }
    let evidence_and_provenance_refs = source_refs
        .iter()
        .map(|source| source.canonical_source_ref.clone())
        .collect::<Vec<_>>();
    let proposed_at =
        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(proposal.header.issued_at_ms)
            .ok_or_else(|| {
                AgentMemoryError::Validation(
                    "personal-agent retrieval proposal time is invalid".to_owned(),
                )
            })?;
    let mut candidate = AppMemoryCandidate {
        protocol_version: AppProtocolVersion::V1,
        candidate_id: AppReference::parse(proposal.header.proposal_id.clone())?,
        candidate_revision: AppRevision::new(proposal.header.proposal_revision)?,
        candidate_fingerprint: AppDigest::blake3(b"unsealed-personal-agent-retrieval"),
        scope: AppScope {
            principal: AppReference::parse(principal.to_owned())?,
            workspace: AppReference::parse(workspace.to_owned())?,
        },
        intended_tier_scope: AppMemoryTierScope::Agent {
            agent_id: AppReference::parse(proposal.target_agent_id)?,
        },
        semantic_destination: AppMemorySemanticDestination::Knowledge,
        source_refs,
        derived_claim_or_summary: proposal.projection_text,
        claim_content_digest: AppDigest::parse(proposal.projection_digest)?,
        handling_labels: labels(&proposal.header.handling_labels)?,
        evidence_and_provenance_refs,
        status: AppMemoryCandidateStatus::Accepted,
        proposed_at,
        updated_at: proposed_at,
    };
    candidate.seal_fingerprint()?;
    Ok(candidate)
}

fn contribution_entry_to_memory_candidate(
    principal: &str,
    workspace: &str,
    entry: crate::magician_v2::agents::app_memory_ingress::AppMemoryProjectionEntryV1,
) -> Result<
    crate::magician_v2::apps::memory::AppMemoryCandidate,
    Box<dyn std::error::Error + Send + Sync>,
> {
    use crate::magician_v2::apps::memory::{
        AppMemoryCandidate, AppMemoryCandidateStatus, AppMemorySemanticDestination,
        AppMemorySourceRef, AppMemoryTierScope,
    };
    use crate::magician_v2::apps::models::{
        AppDataClassification, AppDigest, AppFieldPath, AppHandlingLabels, AppInstallationId,
        AppModelProcessing, AppName, AppProtocolVersion, AppRecordId, AppReference, AppRevision,
        AppSourceRef, AppSourceRefKind,
    };
    use crate::magician_v2::apps::records::AppScope;
    use magician_app_contract::contribution::{
        AppContributionClassification, AppContributionModelProcessing,
        AppMemorySemanticDestinationV1, AppMemoryTierScopeV1,
    };

    let proposal = entry.proposal;
    proposal.validate()?;
    let labels = |value: &magician_app_contract::contribution::AppContributionHandlingLabelsV1| {
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(AppHandlingLabels {
            classification: match value.classification {
                AppContributionClassification::Public => AppDataClassification::Public,
                AppContributionClassification::Internal => AppDataClassification::Ordinary,
                AppContributionClassification::Personal => AppDataClassification::Personal,
                AppContributionClassification::Sensitive => AppDataClassification::Sensitive,
                AppContributionClassification::Secret => AppDataClassification::Secret,
            },
            model_processing: match value.model_processing {
                AppContributionModelProcessing::None => AppModelProcessing::None,
                AppContributionModelProcessing::LocalOnly => AppModelProcessing::LocalOnly,
                AppContributionModelProcessing::RemoteAllowed => AppModelProcessing::RemoteAllowed,
            },
            policy_digest: AppDigest::parse(value.policy_digest.clone())?,
            provenance_digest: AppDigest::parse(value.provenance_digest.clone())?,
        })
    };
    let mut source_refs = Vec::with_capacity(proposal.header.sources.len());
    for source in &proposal.header.sources {
        let fields = source
            .selected_fields
            .iter()
            .cloned()
            .map(AppFieldPath::parse)
            .collect::<Result<Vec<_>, _>>()?;
        source_refs.push(AppMemorySourceRef {
            installation_id: AppInstallationId::parse(source.installation_id.clone())?,
            package_revision_ref: AppReference::parse(
                proposal.header.package_revision_ref.clone(),
            )?,
            grant_revision: AppRevision::new(proposal.header.grant_revision)?,
            schema_revision: AppRevision::new(proposal.header.schema_revision)?,
            entity_name: AppName::parse(source.entity_name.clone())?,
            record_id: AppRecordId::parse(source.record_id.clone())?,
            record_revision: AppRevision::new(source.record_revision)?,
            selected_fields: fields.clone(),
            canonical_source_ref: AppSourceRef {
                kind: AppSourceRefKind::EntityRecord,
                reference: AppReference::parse(source.canonical_source_ref.clone())?,
                revision: Some(AppRevision::new(source.record_revision)?),
                fields,
            },
            handling_labels: labels(&source.handling_labels)?,
        });
    }
    let evidence_and_provenance_refs = source_refs
        .iter()
        .filter(|source| {
            proposal
                .evidence_refs
                .iter()
                .any(|evidence| evidence == source.canonical_source_ref.reference.as_str())
        })
        .map(|source| source.canonical_source_ref.clone())
        .collect::<Vec<_>>();
    let intended_tier_scope = match proposal.intended_tier_scope {
        AppMemoryTierScopeV1::User => AppMemoryTierScope::User,
        AppMemoryTierScopeV1::Agent { agent_id } => AppMemoryTierScope::Agent {
            agent_id: AppReference::parse(agent_id)?,
        },
        AppMemoryTierScopeV1::AgentGoal { agent_id, goal_id } => AppMemoryTierScope::AgentGoal {
            agent_id: AppReference::parse(agent_id)?,
            goal_id: AppReference::parse(goal_id)?,
        },
    };
    let semantic_destination = match proposal.semantic_destination {
        AppMemorySemanticDestinationV1::TaskProgress => AppMemorySemanticDestination::TaskProgress,
        AppMemorySemanticDestinationV1::Entities => AppMemorySemanticDestination::Entities,
        AppMemorySemanticDestinationV1::Knowledge => AppMemorySemanticDestination::Knowledge,
        AppMemorySemanticDestinationV1::Archive => AppMemorySemanticDestination::Archive,
    };
    let proposed_at =
        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(proposal.header.issued_at_ms)
            .ok_or_else(|| {
                AgentMemoryError::Validation("app-memory proposal time is invalid".to_owned())
            })?;
    let mut candidate = AppMemoryCandidate {
        protocol_version: AppProtocolVersion::V1,
        candidate_id: AppReference::parse(proposal.header.proposal_id.clone())?,
        candidate_revision: AppRevision::new(proposal.header.proposal_revision)?,
        candidate_fingerprint: AppDigest::blake3(b"unsealed-app-memory-contribution"),
        scope: AppScope {
            principal: AppReference::parse(principal.to_owned())?,
            workspace: AppReference::parse(workspace.to_owned())?,
        },
        intended_tier_scope,
        semantic_destination,
        source_refs,
        derived_claim_or_summary: proposal.claim_or_summary,
        claim_content_digest: AppDigest::parse(proposal.claim_digest)?,
        handling_labels: labels(&proposal.header.handling_labels)?,
        evidence_and_provenance_refs,
        status: AppMemoryCandidateStatus::Accepted,
        proposed_at,
        updated_at: proposed_at,
    };
    candidate.seal_fingerprint()?;
    Ok(candidate)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::agents::memory_scope;
    use crate::magician_v2::agents::memory_tiers::{
        RenderConfig, RetentionMode, TierFieldSchema, TierScope,
    };
    use crate::magician_v2::agents::types::EpisodeRetention;
    use crate::magician_v2::artifact_v2::memory::V3_MEMORY_EPISODE_SCHEMA_ORIGIN_STAMPED;
    use chrono::{Duration as ChronoDuration, TimeZone};
    use std::{collections::BTreeMap, time::Duration};

    fn sample_tier() -> MemoryTierDefinition {
        MemoryTierDefinition {
            name: "task_progress".to_string(),
            scope: TierScope::AgentGoal,
            description: "progress".to_string(),
            schema: BTreeMap::from([(String::from("context_summary"), TierFieldSchema::Text {})]),
            render: RenderConfig {
                format: "compact_summary".to_string(),
                template: "{context_summary}".to_string(),
            },
            retention: RetentionMode::GoalLifetime,
        }
    }

    fn sample_episode(
        agent_id: &str,
        goal_id: &str,
        trigger_seq: u64,
        episode_id: &str,
    ) -> V3EpisodeRecord {
        let now = Utc::now();
        V3EpisodeRecord::new_memory_episode(
            None,
            agent_id.to_string(),
            episode_id.to_string(),
            goal_id.to_string(),
            "cron".to_string(),
            trigger_seq,
            now,
            None,
            now,
            now,
            &EpisodeOutcome::GoalAchieved {
                summary: "ok".to_string(),
            },
            Vec::new(),
            Vec::new(),
            Vec::new(),
            None,
            None,
            None,
        )
    }

    fn sample_correction(agent_id: &str, correction_id: &str) -> Correction {
        Correction {
            correction_id: correction_id.to_string(),
            agent_id: agent_id.to_string(),
            timestamp: Utc::now(),
            trigger: "manual".to_string(),
            correction: "do not apply to crypto".to_string(),
            generalized_rule: "avoid crypto companies".to_string(),
            affected_episodes: Vec::new(),
            category: CorrectionCategory::Preference,
        }
    }

    fn sample_episode_with_strategy(
        agent_id: &str,
        goal_id: &str,
        trigger_seq: u64,
        episode_id: &str,
        strategy_summary: Option<&str>,
        succeeded: bool,
        started_at: DateTime<Utc>,
    ) -> V3EpisodeRecord {
        let mut episode = sample_episode(agent_id, goal_id, trigger_seq, episode_id);
        episode.started_at = started_at.to_rfc3339();
        episode.completed_at = (started_at + ChronoDuration::seconds(1)).to_rfc3339();
        episode.strategy_summary = strategy_summary.map(str::to_string);
        let outcome = if succeeded {
            EpisodeOutcome::GoalAchieved {
                summary: "ok".to_string(),
            }
        } else {
            EpisodeOutcome::Failed {
                error: "failed".to_string(),
            }
        };
        let (
            outcome_kind,
            outcome_summary,
            outcome_remaining,
            pending_actions,
            failure_count,
            last_error,
        ) = match &outcome {
            EpisodeOutcome::GoalAchieved { summary } => (
                "goal_achieved".to_string(),
                summary.clone(),
                None,
                Vec::new(),
                None,
                None,
            ),
            EpisodeOutcome::Failed { error } => (
                "failed".to_string(),
                error.clone(),
                None,
                Vec::new(),
                None,
                Some(error.clone()),
            ),
            _ => unreachable!(),
        };
        episode.outcome_kind = outcome_kind;
        episode.outcome_summary = outcome_summary;
        episode.outcome_remaining = outcome_remaining;
        episode.pending_actions = pending_actions;
        episode.failure_count = failure_count;
        episode.last_error = last_error;
        episode
    }

    fn native_episode_record(
        _service: &AgentMemoryService,
        episode: &V3EpisodeRecord,
    ) -> V3EpisodeRecord {
        episode.clone()
    }

    fn set_episode_window(
        episode: &mut V3EpisodeRecord,
        started_at: DateTime<Utc>,
        completed_at: DateTime<Utc>,
    ) {
        episode.started_at = started_at.to_rfc3339();
        episode.completed_at = completed_at.to_rfc3339();
    }

    fn set_episode_outcome(episode: &mut V3EpisodeRecord, outcome: EpisodeOutcome) {
        let (
            outcome_kind,
            outcome_summary,
            outcome_remaining,
            pending_actions,
            failure_count,
            last_error,
        ) = match outcome {
            EpisodeOutcome::GoalAchieved { summary } => (
                "goal_achieved".to_string(),
                summary,
                None,
                Vec::new(),
                None,
                None,
            ),
            EpisodeOutcome::Failed { error } => (
                "failed".to_string(),
                error.clone(),
                None,
                Vec::new(),
                None,
                Some(error),
            ),
            EpisodeOutcome::PartialProgress { summary, remaining } => (
                "partial_progress".to_string(),
                summary,
                Some(remaining),
                Vec::new(),
                None,
                None,
            ),
            EpisodeOutcome::UserIntervened { reason } => (
                "user_intervened".to_string(),
                reason,
                None,
                Vec::new(),
                None,
                None,
            ),
            EpisodeOutcome::BudgetExhausted => (
                "budget_exhausted".to_string(),
                "Budget exhausted".to_string(),
                None,
                Vec::new(),
                None,
                None,
            ),
            EpisodeOutcome::Paused { pending_actions } => (
                "paused".to_string(),
                pending_actions.join(", "),
                None,
                pending_actions,
                None,
                None,
            ),
            EpisodeOutcome::CircuitOpen {
                failure_count,
                last_error,
            } => (
                "circuit_open".to_string(),
                last_error.clone(),
                None,
                Vec::new(),
                Some(failure_count),
                Some(last_error),
            ),
        };
        episode.outcome_kind = outcome_kind;
        episode.outcome_summary = outcome_summary;
        episode.outcome_remaining = outcome_remaining;
        episode.pending_actions = pending_actions;
        episode.failure_count = failure_count;
        episode.last_error = last_error;
    }

    async fn append_test_episode(
        service: &AgentMemoryService,
        agent_id: &str,
        episode: &V3EpisodeRecord,
    ) {
        service
            .append_native_episode(agent_id, episode)
            .await
            .unwrap();
    }

    async fn load_test_episodes(
        service: &AgentMemoryService,
        agent_id: &str,
    ) -> Vec<V3EpisodeRecord> {
        service.load_native_episodes(agent_id).await.unwrap()
    }

    async fn recall_test_episodes(
        service: &AgentMemoryService,
        agent_id: &str,
        goal_id: &str,
        seq_range: Option<(u64, u64)>,
        limit: Option<usize>,
    ) -> Result<Vec<V3EpisodeRecord>, AgentMemoryError> {
        service
            .recall_native_episodes(agent_id, goal_id, seq_range, limit)
            .await
    }

    async fn save_test_tier(
        service: &AgentMemoryService,
        agent_id: &str,
        tier_definition: &MemoryTierDefinition,
        goal_id: Option<&str>,
        data: &V3MemoryTierRecord,
    ) {
        service
            .save_native_tier(agent_id, tier_definition, goal_id, data)
            .await
            .unwrap();
    }

    async fn load_test_tier(
        service: &AgentMemoryService,
        agent_id: &str,
        tier_definition: &MemoryTierDefinition,
        goal_id: Option<&str>,
    ) -> Option<V3MemoryTierRecord> {
        service
            .load_native_tier(agent_id, tier_definition, goal_id)
            .await
            .unwrap()
    }

    async fn save_test_tier_by_name(
        service: &AgentMemoryService,
        agent_id: &str,
        tier_name: &str,
        tier_definitions: &[MemoryTierDefinition],
        goal_id: Option<&str>,
        data: &V3MemoryTierRecord,
    ) -> Result<(), AgentMemoryError> {
        service
            .save_native_tier_by_name(agent_id, tier_name, tier_definitions, goal_id, data)
            .await
    }

    async fn load_test_tier_by_name(
        service: &AgentMemoryService,
        agent_id: &str,
        tier_name: &str,
        tier_definitions: &[MemoryTierDefinition],
        goal_id: Option<&str>,
    ) -> Result<Option<V3MemoryTierRecord>, AgentMemoryError> {
        service
            .load_native_tier_by_name(agent_id, tier_name, tier_definitions, goal_id)
            .await
    }

    #[tokio::test]
    async fn episode_roundtrip_and_trigger_dedup() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        let episode = sample_episode("agent-1", "g1", 7, "ep-1");

        append_test_episode(&service, "agent-1", &episode).await;
        let loaded = load_test_episodes(&service, "agent-1").await;
        assert_eq!(loaded.len(), 1);
        assert!(service
            .has_episode_for_trigger("agent-1", "g1", 7)
            .await
            .unwrap());

        let episodes_dir = service.storage.agent_episodes_dir("agent-1").unwrap();
        let files = service
            .storage
            .list_files_with_extension(episodes_dir, "json")
            .await
            .unwrap();
        let file_name = files[0].file_name().and_then(|n| n.to_str()).unwrap();
        assert!(
            file_name == "ep-1.json",
            "unexpected native episode filename: {file_name}"
        );
    }

    fn sample_user_evidence(
        evidence_id: &str,
        sensitivity: &str,
    ) -> crate::magician_v2::evidence::EvidenceRecord {
        crate::magician_v2::evidence::EvidenceRecord {
            evidence_id: evidence_id.to_string(),
            summary: "visited a page".to_string(),
            evidence_kind: "activity".to_string(),
            observed_actions: Vec::new(),
            entity_keys: Vec::new(),
            people_keys: Vec::new(),
            artifact_refs: Vec::new(),
            source_refs: vec!["episode:test".to_string()],
            facets: Vec::new(),
            importance: 0.5,
            confidence: 0.5,
            sensitivity: sensitivity.to_string(),
            first_seen_at: "2026-06-13T00:00:00Z".to_string(),
            last_seen_at: "2026-06-13T00:00:00Z".to_string(),
            status: crate::magician_v2::evidence::EvidenceStatus::Active,
            last_corrected_at: None,
            producer: "ambient_browser".to_string(),
            metadata: serde_json::Value::Null,
        }
    }

    /// The unified consumer read (`load_scoped_evidence`) suppresses
    /// affirmatively-sensitive ambient evidence while non-sensitive records flow.
    #[tokio::test]
    async fn scoped_evidence_read_suppresses_sensitive_records() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        service
            .append_user_work_evidence(sample_user_evidence("evd:work-1", "work"))
            .await
            .unwrap();
        service
            .append_user_work_evidence(sample_user_evidence("evd:fin-1", "financial"))
            .await
            .unwrap();

        // Both persist in the raw user-owned lane …
        assert_eq!(service.load_user_work_evidence().await.unwrap().len(), 2);

        // … but the unified read that reviews/dashboard consume suppresses the
        // affirmatively-sensitive one.
        let visible = service.load_scoped_evidence("agent-x").await.unwrap();
        assert_eq!(
            visible.len(),
            1,
            "only the non-sensitive record should survive the unified read"
        );
        assert_eq!(visible[0].evidence_id, "evd:work-1");
        assert!(visible.iter().all(|r| r.evidence_id != "evd:fin-1"));
    }

    #[test]
    fn sensitivity_predicate_covers_every_sensitive_category() {
        use crate::magician_v2::evidence::{
            is_sensitive, normalize_sensitivity, SENSITIVE_CATEGORIES,
        };
        for cat in SENSITIVE_CATEGORIES {
            assert!(is_sensitive(cat), "{cat} must be sensitive");
            assert!(
                is_sensitive(&format!("  {} ", cat.to_uppercase())),
                "{cat} must be sensitive case/space-insensitively"
            );
        }
        for ok in ["work", "unknown", "", "  ", "research"] {
            assert!(!is_sensitive(ok), "{ok:?} must not be sensitive");
        }
        assert_eq!(normalize_sensitivity(None), "unknown");
        assert_eq!(normalize_sensitivity(Some("  Financial ")), "financial");
    }

    #[tokio::test]
    async fn has_episode_for_trigger_does_not_match_seq_prefixes() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        let episode = sample_episode("agent-1", "g1", 70, "ep-70");
        append_test_episode(&service, "agent-1", &episode).await;

        assert!(!service
            .has_episode_for_trigger("agent-1", "g1", 7)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn auto_select_strategy_prefers_higher_success_rate() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        let now = Utc::now();

        // StrategyEffectiveness::success_rate() returns a 0.5 neutral prior
        // when fewer than 5 data points exist per strategy.  We need >=5
        // episodes per candidate so the real success rate is used.
        //
        // guided_search: 5 episodes, all succeed → rate = 1.0
        // atomic_composition: 5 episodes, 1 succeed + 4 fail → rate = 0.2
        let episodes = vec![
            // --- guided_search: 5 successes ---
            sample_episode_with_strategy(
                "agent-1",
                "g1",
                1,
                "ep-1",
                Some("GuidedSearch"),
                true,
                now,
            ),
            sample_episode_with_strategy(
                "agent-1",
                "g1",
                2,
                "ep-2",
                Some("guided_search"),
                true,
                now + ChronoDuration::seconds(1),
            ),
            sample_episode_with_strategy(
                "agent-1",
                "g1",
                3,
                "ep-3",
                Some("guided_search"),
                true,
                now + ChronoDuration::seconds(2),
            ),
            sample_episode_with_strategy(
                "agent-1",
                "g1",
                4,
                "ep-4",
                Some("guided_search"),
                true,
                now + ChronoDuration::seconds(3),
            ),
            sample_episode_with_strategy(
                "agent-1",
                "g1",
                5,
                "ep-5",
                Some("guided_search"),
                true,
                now + ChronoDuration::seconds(4),
            ),
            // --- atomic_composition: 1 success + 4 failures ---
            sample_episode_with_strategy(
                "agent-1",
                "g1",
                6,
                "ep-6",
                Some("atomic_composition"),
                true,
                now + ChronoDuration::seconds(5),
            ),
            sample_episode_with_strategy(
                "agent-1",
                "g1",
                7,
                "ep-7",
                Some("atomic_composition"),
                false,
                now + ChronoDuration::seconds(6),
            ),
            sample_episode_with_strategy(
                "agent-1",
                "g1",
                8,
                "ep-8",
                Some("atomic_composition"),
                false,
                now + ChronoDuration::seconds(7),
            ),
            sample_episode_with_strategy(
                "agent-1",
                "g1",
                9,
                "ep-9",
                Some("atomic_composition"),
                false,
                now + ChronoDuration::seconds(8),
            ),
            sample_episode_with_strategy(
                "agent-1",
                "g1",
                10,
                "ep-10",
                Some("atomic_composition"),
                false,
                now + ChronoDuration::seconds(9),
            ),
        ];

        for episode in episodes {
            append_test_episode(&service, "agent-1", &episode).await;
        }

        let selected = service
            .select_effective_strategy_for_goal(
                "agent-1",
                "g1",
                &["atomic_composition", "guided_search"],
            )
            .await
            .unwrap();
        assert_eq!(selected.as_deref(), Some("guided_search"));
    }

    #[tokio::test]
    async fn select_effective_strategy_uses_neutral_prior_under_5_points() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        let now = Utc::now();

        // Two candidates each with fewer than 5 data points — both get the 0.5
        // neutral prior from StrategyEffectiveness.success_rate(), so the
        // tie-break should pick by attempt count then lexicographic order.
        // guided_search: 2 episodes (both succeed → rate=0.5 neutral prior)
        // atomic_composition: 1 episode (succeeds → rate=0.5 neutral prior)
        let episodes = vec![
            sample_episode_with_strategy(
                "agent-1",
                "g1",
                1,
                "ep-1",
                Some("guided_search"),
                true,
                now,
            ),
            sample_episode_with_strategy(
                "agent-1",
                "g1",
                2,
                "ep-2",
                Some("guided_search"),
                true,
                now + ChronoDuration::seconds(1),
            ),
            sample_episode_with_strategy(
                "agent-1",
                "g1",
                3,
                "ep-3",
                Some("atomic_composition"),
                true,
                now + ChronoDuration::seconds(2),
            ),
        ];

        for episode in episodes {
            append_test_episode(&service, "agent-1", &episode).await;
        }

        let selected = service
            .select_effective_strategy_for_goal(
                "agent-1",
                "g1",
                &["atomic_composition", "guided_search"],
            )
            .await
            .unwrap();

        // Both have 0.5 neutral rate (< 5 points). guided_search has more attempts (2 vs 1),
        // so it wins the tie-break.
        assert_eq!(
            selected.as_deref(),
            Some("guided_search"),
            "neutral prior (< 5 data points) should tie-break by attempts"
        );
    }

    #[tokio::test]
    async fn auto_select_strategy_returns_none_without_matching_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        let now = Utc::now();
        let episode = sample_episode_with_strategy(
            "agent-1",
            "g1",
            1,
            "ep-1",
            Some("experimental_planner"),
            true,
            now,
        );
        append_test_episode(&service, "agent-1", &episode).await;

        let selected = service
            .select_effective_strategy_for_goal(
                "agent-1",
                "g1",
                &["atomic_composition", "guided_search"],
            )
            .await
            .unwrap();
        assert!(selected.is_none());
    }

    #[tokio::test]
    async fn has_episode_for_trigger_does_not_alias_delimiter_like_goal_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        // Old filename encoding could alias this queried pair against a different
        // stored pair due delimiter tokens embedded in goal/episode IDs.
        let stored = sample_episode("agent-1", "x", 1, "y__seq_2_z");
        append_test_episode(&service, "agent-1", &stored).await;

        assert!(!service
            .has_episode_for_trigger("agent-1", "x__seq_1__ep_y", 2)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn append_episode_with_max_length_goal_id_uses_bounded_filename() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        let long_goal = "g".repeat(255);
        let long_episode_id = "e".repeat(255);
        let episode = sample_episode("agent-1", &long_goal, 42, &long_episode_id);
        append_test_episode(&service, "agent-1", &episode).await;

        let episodes_dir = service.storage.agent_episodes_dir("agent-1").unwrap();
        let files = service
            .storage
            .list_files_with_extension(episodes_dir, "json")
            .await
            .unwrap();
        assert_eq!(files.len(), 1);
        let file_name = files[0].file_name().and_then(|n| n.to_str()).unwrap();
        assert!(
            file_name.len() <= 255,
            "filename too long: {}",
            file_name.len()
        );

        assert!(service
            .has_episode_for_trigger("agent-1", &long_goal, 42)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn one_episode_replay_source_detects_change_and_missing_record() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        let episode = sample_episode("agent-1", "goal", 7, "episode-7");
        append_test_episode(&service, "agent-1", &episode).await;
        let path = service
            .storage
            .agent_episodes_dir("agent-1")
            .unwrap()
            .join(service.native_episode_file_name_for_id("episode-7"));
        let before = service
            .load_native_episode_by_id("agent-1", "episode-7")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(before.episode_id, "episode-7");

        let mut changed = before;
        changed.outcome_summary = "source changed after decision".into();
        service
            .storage
            .write_json_atomic(&path, &changed)
            .await
            .unwrap();
        let current = service
            .load_native_episode_by_id("agent-1", "episode-7")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(current.outcome_summary, "source changed after decision");

        std::fs::write(&path, vec![b' '; 256 * 1024 + 1]).unwrap();
        assert!(service
            .load_native_episode_by_id("agent-1", "episode-7")
            .await
            .unwrap()
            .is_none());

        std::fs::remove_file(path).unwrap();
        assert!(service
            .load_native_episode_by_id("agent-1", "episode-7")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn has_episode_for_trigger_ignores_legacy_filename_records() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        let episode = sample_episode("agent-1", "g1", 7, "legacy-ep");
        service.ensure_agent_layout("agent-1").await.unwrap();

        let legacy_path = service
            .storage
            .agent_episodes_dir("agent-1")
            .unwrap()
            .join("legacy-ep.v1.json");
        service
            .storage
            .write_json_atomic(&legacy_path, &episode)
            .await
            .unwrap();

        assert!(!service
            .has_episode_for_trigger("agent-1", "g1", 7)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn append_episode_rejects_agent_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        let episode = sample_episode("agent-2", "g1", 7, "ep-1");

        let err = service
            .append_native_episode("agent-1", &episode)
            .await
            .unwrap_err();
        assert!(matches!(err, AgentMemoryError::Validation(_)));
    }

    #[tokio::test]
    async fn append_native_episode_rejects_empty_goal_key() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        let mut episode =
            native_episode_record(&service, &sample_episode("agent-1", "g1", 7, "ep-1"));
        episode.goal_key.clear();

        let err = service
            .append_native_episode("agent-1", &episode)
            .await
            .unwrap_err();
        assert!(matches!(err, AgentMemoryError::Validation(_)));
    }

    #[tokio::test]
    async fn load_episodes_skips_records_with_broken_trigger_invariants() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        let valid = sample_episode("agent-1", "g1", 1, "ep-valid");
        append_test_episode(&service, "agent-1", &valid).await;

        service.ensure_agent_layout("agent-1").await.unwrap();
        let episodes_dir = service.storage.agent_episodes_dir("agent-1").unwrap();

        let mut goal_mismatch = sample_episode("agent-1", "g1", 2, "ep-goal-mismatch");
        goal_mismatch.goal_key = "other-goal".to_string();
        service
            .storage
            .write_json_atomic(episodes_dir.join("bad-goal.json"), &goal_mismatch)
            .await
            .unwrap();

        let mut seq_mismatch = sample_episode("agent-1", "g1", 3, "ep-seq-mismatch");
        seq_mismatch.trigger_seq = 99;
        service
            .storage
            .write_json_atomic(episodes_dir.join("bad-seq.json"), &seq_mismatch)
            .await
            .unwrap();

        let loaded = load_test_episodes(&service, "agent-1").await;
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].episode_id, "ep-valid");
    }

    #[tokio::test]
    async fn corrections_jsonl_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        let correction = sample_correction("agent-1", "c1");

        service
            .append_correction("agent-1", &correction)
            .await
            .unwrap();

        let loaded = service.load_corrections("agent-1").await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].correction_id, "c1");
    }

    #[tokio::test]
    async fn concurrent_correction_appends_preserve_jsonl_framing() {
        let tmp = tempfile::tempdir().unwrap();
        let service_a = AgentMemoryService::with_base_path(tmp.path());
        let service_b = AgentMemoryService::with_base_path(tmp.path());

        let mut tasks = tokio::task::JoinSet::new();
        for i in 0..64 {
            let svc = if i % 2 == 0 {
                service_a.clone()
            } else {
                service_b.clone()
            };
            tasks.spawn(async move {
                let correction = sample_correction("agent-1", &format!("c{i}"));
                svc.append_correction("agent-1", &correction).await
            });
        }

        while let Some(joined) = tasks.join_next().await {
            joined.unwrap().unwrap();
        }

        let lock_path = service_a
            .storage
            .agent_corrections_lock_path("agent-1")
            .unwrap();
        assert!(tokio::fs::try_exists(lock_path).await.unwrap());

        let corrections_path = service_a.storage.agent_corrections_path("agent-1").unwrap();
        let content = tokio::fs::read_to_string(corrections_path).await.unwrap();
        let mut line_count = 0usize;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let parsed: Correction = serde_json::from_str(trimmed).unwrap();
            assert_eq!(parsed.agent_id, "agent-1");
            line_count += 1;
        }
        assert_eq!(line_count, 64);
    }

    #[tokio::test]
    async fn correction_append_lock_is_scoped_per_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        let agent_a_first = service.correction_append_lock_for_agent("agent-a").await;
        let agent_a_second = service.correction_append_lock_for_agent("agent-a").await;
        let agent_b = service.correction_append_lock_for_agent("agent-b").await;

        assert!(Arc::ptr_eq(&agent_a_first, &agent_a_second));
        assert!(!Arc::ptr_eq(&agent_a_first, &agent_b));
    }

    #[tokio::test]
    async fn corrections_lock_acquisition_times_out_when_held() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        service.ensure_agent_layout("agent-1").await.unwrap();

        let lock_path = service
            .storage
            .agent_corrections_lock_path("agent-1")
            .unwrap();
        let holder = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        holder.lock().unwrap();

        let err = acquire_corrections_append_lock_with_config(
            service.storage(),
            "agent-1",
            CorrectionsAppendLockConfig {
                max_wait: Duration::from_millis(30),
                retry_delay_min: Duration::from_millis(5),
                retry_delay_max: Duration::from_millis(10),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            AgentMemoryError::CorrectionsLockTimeout { .. }
        ));
    }

    #[tokio::test]
    async fn append_correction_rejects_agent_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        let correction = Correction {
            correction_id: "c1".to_string(),
            agent_id: "agent-2".to_string(),
            timestamp: Utc::now(),
            trigger: "manual".to_string(),
            correction: "prefer remote".to_string(),
            generalized_rule: "preferences.remote_only=true".to_string(),
            affected_episodes: Vec::new(),
            category: CorrectionCategory::Preference,
        };

        let err = service
            .append_correction("agent-1", &correction)
            .await
            .unwrap_err();
        assert!(matches!(err, AgentMemoryError::Validation(_)));
    }

    #[test]
    fn correction_lock_file_supports_exclusive_locking() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        let lock_path = service
            .storage
            .agent_corrections_lock_path("agent-1")
            .unwrap();
        std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();

        let first = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        first.lock().unwrap();

        let second = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        let second_try = second.try_lock();
        assert!(second_try.is_err());
    }

    #[tokio::test]
    async fn tier_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        let tier = sample_tier();
        let data = sample_tier_record(
            "task_progress",
            TierScope::AgentGoal,
            "context_summary",
            "hello",
        );

        save_test_tier(&service, "agent-1", &tier, Some("goal-a"), &data).await;

        let loaded = load_test_tier(&service, "agent-1", &tier, Some("goal-a"))
            .await
            .unwrap();
        assert_eq!(loaded.tier_name, "task_progress");
    }

    #[tokio::test]
    async fn user_scope_tier_does_not_require_agent_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        let tier = MemoryTierDefinition {
            name: "knowledge".to_string(),
            scope: TierScope::User,
            description: "user knowledge".to_string(),
            schema: BTreeMap::from([(String::from("facts"), TierFieldSchema::Document {})]),
            render: RenderConfig {
                format: "compact_summary".to_string(),
                template: "{facts}".to_string(),
            },
            retention: RetentionMode::Forever,
        };

        let mut data =
            V3MemoryTierRecord::new("knowledge", TierScope::User, None, None, None, None);
        data.fields.insert(
            "facts".to_string(),
            serde_json::json!({"remote_only": true}),
        );

        save_test_tier(&service, "", &tier, None, &data).await;
        let loaded = load_test_tier(&service, "", &tier, None).await.unwrap();
        assert_eq!(loaded.tier_name, "knowledge");
    }

    #[tokio::test]
    async fn build_context_profile_applies_structured_correction_overlay() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        let mut base = UserProfile::default();
        base.sections.insert(
            "preferences".to_string(),
            serde_json::json!({
                "remote_only": false,
                "salary_min": 150000
            }),
        );
        service.save_profile(&base).await.unwrap();

        let correction = Correction {
            correction_id: "c1".to_string(),
            agent_id: "agent-1".to_string(),
            timestamp: Utc::now(),
            trigger: "manual".to_string(),
            correction: "remote only".to_string(),
            generalized_rule: r#"{"remote_only": true}"#.to_string(),
            affected_episodes: Vec::new(),
            category: CorrectionCategory::Preference,
        };
        service
            .append_correction("agent-1", &correction)
            .await
            .unwrap();

        let context = service.build_context_profile("agent-1").await.unwrap();
        assert_eq!(
            context.effective_preferences.get("remote_only"),
            Some(&Value::Bool(true))
        );
        assert_eq!(
            context.effective_preferences.get("salary_min"),
            Some(&serde_json::json!(150000))
        );
    }

    #[tokio::test]
    async fn save_profile_creates_base_layout_on_fresh_storage() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        // No ensure_base_layout called beforehand — save_profile should do it
        let profile = UserProfile::default();
        service.save_profile(&profile).await.unwrap();

        let loaded = service.load_profile().await.unwrap();
        assert_eq!(loaded.sections.len(), 0);
    }

    /// The user-knowledge store is where model-proposed structure is retained
    /// verbatim, so it is where the retained-JSON depth contract has to hold.
    /// Refusing on write keeps an over-deep tree out; refusing on read would
    /// have turned one bad entry into an unreadable store.
    #[tokio::test]
    async fn save_user_knowledge_refuses_a_tree_deeper_than_the_retained_json_contract() {
        use crate::magician_v2::json_traversal::MAX_RETAINED_JSON_DEPTH;

        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        // Build iteratively; a recursive builder would be the very hazard here.
        let nest = |depth: usize| {
            let mut value = serde_json::json!("leaf");
            for _ in 0..depth {
                value = serde_json::json!({ "next": value });
            }
            serde_json::json!({ "knowledge": [{ "key": "k", "value": value }] })
        };

        // At the limit the store saves and reads back unchanged.
        let at_limit = nest(MAX_RETAINED_JSON_DEPTH - 4);
        service.save_user_knowledge(&at_limit).await.unwrap();
        assert_eq!(service.load_user_knowledge().await.unwrap(), at_limit);

        // Past it the write is refused, and the refusal names the depth so the
        // offending entry can be found rather than guessed at.
        let error = service
            .save_user_knowledge(&nest(MAX_RETAINED_JSON_DEPTH + 5))
            .await
            .expect_err("an over-deep tree must not be persisted");
        assert!(matches!(error, AgentMemoryError::Validation(_)));
        assert!(error
            .to_string()
            .contains(&MAX_RETAINED_JSON_DEPTH.to_string()));
        // Permanent, not a blip: retrying cannot make it shallower.
        assert!(!error.is_transient());

        // The rejection left the previously saved store intact.
        assert_eq!(service.load_user_knowledge().await.unwrap(), at_limit);
    }

    #[tokio::test]
    async fn save_user_knowledge_creates_base_layout_on_fresh_storage() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        // No ensure_base_layout called beforehand — save_user_knowledge should do it
        let knowledge = serde_json::json!({"favorite_color": "blue"});
        service.save_user_knowledge(&knowledge).await.unwrap();

        let loaded = service.load_user_knowledge().await.unwrap();
        assert_eq!(loaded["favorite_color"], "blue");
    }

    #[tokio::test]
    async fn backfill_stamps_only_entries_missing_updated_at() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        service
            .save_user_knowledge(&serde_json::json!({
                "preferences": [
                    {"key": "a", "value": "x", "updated_at": "2026-01-01T00:00:00Z"},
                    {"key": "b", "value": "y"}
                ]
            }))
            .await
            .unwrap();

        let stamped = service
            .backfill_entry_timestamps(1_700_000_000)
            .await
            .unwrap();
        assert_eq!(stamped, 1, "only the entry missing a stamp is touched");
        let value = service.load_user_knowledge().await.unwrap();
        let items = value["preferences"].as_array().unwrap();
        assert_eq!(items[0]["updated_at"], "2026-01-01T00:00:00Z");
        assert!(items[1]["updated_at"].is_string());
    }

    #[tokio::test]
    async fn backfill_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        service
            .save_user_knowledge(&serde_json::json!({"preferences": [{"key": "b"}]}))
            .await
            .unwrap();
        assert_eq!(
            service
                .backfill_entry_timestamps(1_700_000_000)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            service
                .backfill_entry_timestamps(1_700_000_001)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn confirming_an_entry_makes_it_stated_and_records_the_prior() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        service
            .save_user_knowledge(&serde_json::json!({
                "preferences": [{"key": "avoid_vendor_calls", "value": "...", "source_type": "insight"}]
            }))
            .await
            .unwrap();

        service
            .confirm_user_memory_entry("preferences", "avoid_vendor_calls", 1_700_000_000)
            .await
            .unwrap();

        let v = service.load_user_knowledge().await.unwrap();
        let entry = &v["preferences"][0];
        assert_eq!(entry["source_type"], "owner_confirmed");
        assert_eq!(entry["confirmed_from"], "insight");
        assert_eq!(entry["confirmed_at"], 1_700_000_000);
        assert!(
            entry.get("scope").is_none(),
            "confirm must not invent a scope; the owner edits attachability"
        );
    }

    #[tokio::test]
    async fn confirming_an_agent_tier_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        service
            .save_user_knowledge(&serde_json::json!({
                "workflows": [{"key": "w1", "value": "do the thing", "source_type": "insight"}]
            }))
            .await
            .unwrap();
        let err = service
            .confirm_user_memory_entry("workflows", "w1", 1)
            .await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn confirming_an_already_stated_entry_is_a_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        service
            .save_user_knowledge(&serde_json::json!({
                "preferences": [{
                    "key": "k",
                    "value": "v",
                    "source_type": "owner_confirmed",
                    "confirmed_from": "insight"
                }]
            }))
            .await
            .unwrap();
        service
            .confirm_user_memory_entry("preferences", "k", 9)
            .await
            .unwrap();
        let v = service.load_user_knowledge().await.unwrap();
        assert_eq!(v["preferences"][0]["confirmed_from"], "insight");
        assert!(v["preferences"][0].get("confirmed_at").is_none());
    }

    #[tokio::test]
    async fn setting_scope_stores_topics_and_refuses_kind_only_or_agent_tiers() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        service
            .save_user_knowledge(&serde_json::json!({
                "preferences": [{"key": "p1", "value": "v", "source_type": "insight"}],
                "workflows": [{"key": "w1", "value": "do it", "source_type": "insight"}]
            }))
            .await
            .unwrap();
        service
            .set_user_memory_entry_scope(
                "preferences",
                "p1",
                Some(memory_scope::MemoryScope {
                    topics: vec!["vendor".to_string()],
                    entities: Vec::new(),
                    applies_to: Vec::new(),
                }),
            )
            .await
            .unwrap();
        let stored = service.load_user_knowledge().await.unwrap();
        assert_eq!(stored["preferences"][0]["scope"]["topics"][0], "vendor");

        let kind_only = service
            .set_user_memory_entry_scope(
                "preferences",
                "p1",
                Some(memory_scope::MemoryScope {
                    topics: Vec::new(),
                    entities: Vec::new(),
                    applies_to: vec!["comm".to_string()],
                }),
            )
            .await;
        assert!(kind_only.is_err());

        let agent = service
            .set_user_memory_entry_scope(
                "workflows",
                "w1",
                Some(memory_scope::MemoryScope {
                    topics: vec!["vendor".to_string()],
                    entities: Vec::new(),
                    applies_to: Vec::new(),
                }),
            )
            .await;
        assert!(agent.is_err());

        service
            .set_user_memory_entry_scope("preferences", "p1", None)
            .await
            .unwrap();
        let cleared = service.load_user_knowledge().await.unwrap();
        assert!(cleared["preferences"][0].get("scope").is_none());
    }

    #[tokio::test]
    async fn listing_default_tiers_skips_accounts() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        service
            .save_user_knowledge(&serde_json::json!({
                "accounts": [{"key": "acme", "value": "acct", "source_type": "entity"}],
                "preferences": [{"key": "p1", "value": "pref", "source_type": "insight"}]
            }))
            .await
            .unwrap();
        let (page, total) = service
            .list_user_memory_entries_page(0, 20, memory_scope::OWNER_CONFIRMABLE_MEMORY_TIERS)
            .await
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(page[0]["key"], "p1");
        assert_eq!(page[0]["tier"], "preferences");
    }

    #[tokio::test]
    async fn update_user_knowledge_skips_write_when_mutate_returns_false() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        service
            .save_user_knowledge(&serde_json::json!({"preferences": []}))
            .await
            .unwrap();
        service
            .update_user_knowledge(|knowledge| {
                knowledge
                    .as_object_mut()
                    .unwrap()
                    .insert("touched".to_string(), serde_json::json!(true));
                Ok(false)
            })
            .await
            .unwrap();
        let loaded = service.load_user_knowledge().await.unwrap();
        assert!(loaded.get("touched").is_none());
        service
            .update_user_knowledge(|knowledge| {
                knowledge
                    .as_object_mut()
                    .unwrap()
                    .insert("touched".to_string(), serde_json::json!(true));
                Ok(true)
            })
            .await
            .unwrap();
        let loaded = service.load_user_knowledge().await.unwrap();
        assert_eq!(loaded["touched"], true);
    }

    #[tokio::test]
    async fn listing_memory_entries_pages_and_reports_total() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        service
            .save_user_knowledge(&serde_json::json!({
                "preferences": [
                    {"key": "a", "value": "alpha", "source_type": "insight"},
                    {"key": "b", "value": "bravo", "source_type": "owner_confirmed"}
                ]
            }))
            .await
            .unwrap();
        let (page, total) = service
            .list_user_memory_entries_page(0, 1, &["preferences"])
            .await
            .unwrap();
        assert_eq!(total, 2);
        assert_eq!(page.len(), 1);
        let (rest, _) = service
            .list_user_memory_entries_page(1, 1, &["preferences"])
            .await
            .unwrap();
        assert_eq!(rest.len(), 1);
        assert_ne!(page[0]["key"], rest[0]["key"]);
    }

    #[tokio::test]
    async fn untrusted_entries_cannot_be_promoted_by_confirmation() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        service
            .save_user_knowledge(&serde_json::json!({
                "screen_observations": [{"key": "s1", "source_type": "screen_capture"}]
            }))
            .await
            .unwrap();

        let err = service
            .confirm_user_memory_entry("screen_observations", "s1", 1)
            .await;
        assert!(err.is_err());
    }

    #[test]
    fn correction_colon_parsing_does_not_mismatch_urls() {
        let mut prefs = HashMap::new();

        // A correction with a URL-like rule should go to fallback, not split on the colon
        let url_correction = Correction {
            correction_id: "c-url".to_string(),
            agent_id: "agent-1".to_string(),
            timestamp: Utc::now(),
            trigger: "manual".to_string(),
            correction: "check this site".to_string(),
            generalized_rule: "always check https://example.com for listings".to_string(),
            affected_episodes: Vec::new(),
            category: CorrectionCategory::Preference,
        };
        apply_correction_to_preferences(&url_correction, &mut prefs);

        // Should NOT have "always check https" as a key — should fall through to correction.{id}
        assert!(!prefs.contains_key("always check https"));
        assert!(prefs.contains_key("correction.c-url"));
    }

    #[test]
    fn correction_key_value_parsing_works_for_valid_pairs() {
        let mut prefs = HashMap::new();

        let kv_correction = Correction {
            correction_id: "c-kv".to_string(),
            agent_id: "agent-1".to_string(),
            timestamp: Utc::now(),
            trigger: "manual".to_string(),
            correction: "prefer remote".to_string(),
            generalized_rule: "remote_only=true".to_string(),
            affected_episodes: Vec::new(),
            category: CorrectionCategory::Preference,
        };
        apply_correction_to_preferences(&kv_correction, &mut prefs);
        assert_eq!(prefs.get("remote_only"), Some(&Value::Bool(true)));

        let mut prefs2 = HashMap::new();
        let kv_correction2 = Correction {
            correction_id: "c-kv2".to_string(),
            agent_id: "agent-1".to_string(),
            timestamp: Utc::now(),
            trigger: "manual".to_string(),
            correction: "set salary".to_string(),
            generalized_rule: "salary_min: 150000".to_string(),
            affected_episodes: Vec::new(),
            category: CorrectionCategory::Preference,
        };
        apply_correction_to_preferences(&kv_correction2, &mut prefs2);
        assert_eq!(prefs2.get("salary_min"), Some(&serde_json::json!(150000)));
    }

    #[test]
    fn episode_outcome_summary_is_human_readable() {
        let paused = EpisodeOutcome::Paused {
            pending_actions: vec!["browser.click".to_string()],
        };
        assert_eq!(paused.summary(), "Paused (awaiting approval)");

        let failed = EpisodeOutcome::Failed {
            error: "network error".to_string(),
        };
        assert_eq!(failed.summary(), "network error");

        let budget_exhausted = EpisodeOutcome::BudgetExhausted;
        assert_eq!(budget_exhausted.error_summary(), "Budget exhausted");
    }

    #[tokio::test]
    async fn load_corrections_skips_corrupt_jsonl_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        // Write a valid correction first to set up the agent layout
        let valid = Correction {
            correction_id: "c1".to_string(),
            agent_id: "agent-1".to_string(),
            timestamp: Utc::now(),
            trigger: "manual".to_string(),
            correction: "use dark mode".to_string(),
            generalized_rule: "prefer dark themes".to_string(),
            affected_episodes: Vec::new(),
            category: CorrectionCategory::Preference,
        };
        service.append_correction("agent-1", &valid).await.unwrap();

        // Append a corrupt line directly to the JSONL file
        let path = service.storage.agent_corrections_path("agent-1").unwrap();
        tokio::fs::write(
            &path,
            format!(
                "{}\n{{CORRUPT}}\n{}\n",
                serde_json::to_string(&valid).unwrap(),
                serde_json::to_string(&Correction {
                    correction_id: "c2".to_string(),
                    ..valid.clone()
                })
                .unwrap()
            ),
        )
        .await
        .unwrap();

        let loaded = service.load_corrections("agent-1").await.unwrap();
        assert_eq!(
            loaded.len(),
            2,
            "should skip the corrupt line and load 2 valid ones"
        );
        assert_eq!(loaded[0].correction_id, "c1");
        assert_eq!(loaded[1].correction_id, "c2");
    }

    #[tokio::test]
    async fn load_corrections_preserves_unknown_category_as_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        service.ensure_agent_layout("agent-1").await.unwrap();
        let path = service.storage.agent_corrections_path("agent-1").unwrap();
        tokio::fs::write(
            &path,
            r#"{"correction_id":"c1","agent_id":"agent-1","timestamp":"2026-01-01T00:00:00Z","trigger":"manual","correction":"x","generalized_rule":"y","affected_episodes":[],"category":"new_category"}"#,
        )
        .await
        .unwrap();

        let loaded = service.load_corrections("agent-1").await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert!(matches!(loaded[0].category, CorrectionCategory::Unknown));
    }

    #[tokio::test]
    async fn find_expiring_episodes_applies_failure_and_goal_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        let mut old_success = sample_episode("agent-1", "g1", 1, "ep-success-old");
        set_episode_outcome(
            &mut old_success,
            EpisodeOutcome::GoalAchieved {
                summary: "done".to_string(),
            },
        );
        let success_completed_at = Utc::now() - ChronoDuration::days(11);
        set_episode_window(
            &mut old_success,
            success_completed_at - ChronoDuration::minutes(1),
            success_completed_at,
        );
        service
            .append_native_episode("agent-1", &native_episode_record(&service, &old_success))
            .await
            .unwrap();

        let mut old_failure = sample_episode("agent-1", "g1", 2, "ep-failure-old");
        set_episode_outcome(
            &mut old_failure,
            EpisodeOutcome::Failed {
                error: "failed".to_string(),
            },
        );
        let failure_completed_at = Utc::now() - ChronoDuration::days(11);
        set_episode_window(
            &mut old_failure,
            failure_completed_at - ChronoDuration::minutes(1),
            failure_completed_at,
        );
        service
            .append_native_episode("agent-1", &native_episode_record(&service, &old_failure))
            .await
            .unwrap();

        let mut old_override = sample_episode("agent-1", "g2", 1, "ep-override-old");
        let override_completed_at = Utc::now() - ChronoDuration::days(20);
        set_episode_window(
            &mut old_override,
            override_completed_at - ChronoDuration::minutes(1),
            override_completed_at,
        );
        service
            .append_native_episode("agent-1", &native_episode_record(&service, &old_override))
            .await
            .unwrap();

        let retention = EpisodeRetention {
            default_days: 10,
            on_failure: Some(30),
            per_goal_override: HashMap::from([(String::from("g2"), 15)]),
            consolidate_before_delete: false,
        };

        let expiring = service
            .find_expiring_native_episodes("agent-1", &retention, Utc::now())
            .await
            .unwrap();
        let ids = expiring
            .into_iter()
            .map(|episode| episode.episode_id)
            .collect::<Vec<_>>();
        let mut ids = ids;
        ids.sort();
        assert_eq!(
            ids,
            vec!["ep-override-old".to_string(), "ep-success-old".to_string()]
        );
    }

    #[tokio::test]
    async fn delete_episodes_removes_requested_records() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        let ep1 = sample_episode("agent-1", "g1", 1, "ep-1");
        let ep2 = sample_episode("agent-1", "g1", 2, "ep-2");
        append_test_episode(&service, "agent-1", &ep1).await;
        append_test_episode(&service, "agent-1", &ep2).await;

        let deleted = service
            .delete_native_episodes("agent-1", &[native_episode_record(&service, &ep1)])
            .await
            .unwrap();
        assert_eq!(deleted, 1);

        let remaining = load_test_episodes(&service, "agent-1").await;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].episode_id, "ep-2");
    }

    #[tokio::test]
    async fn recall_episodes_filters_by_goal_and_sequence_range() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        let mut g1_seq1 = sample_episode("agent-1", "goal-1", 1, "g1-seq1");
        let g1_seq1_completed_at = Utc::now() - ChronoDuration::minutes(4);
        set_episode_window(
            &mut g1_seq1,
            g1_seq1_completed_at - ChronoDuration::seconds(1),
            g1_seq1_completed_at,
        );
        append_test_episode(&service, "agent-1", &g1_seq1).await;

        let mut g1_seq2 = sample_episode("agent-1", "goal-1", 2, "g1-seq2");
        let g1_seq2_completed_at = Utc::now() - ChronoDuration::minutes(3);
        set_episode_window(
            &mut g1_seq2,
            g1_seq2_completed_at - ChronoDuration::seconds(1),
            g1_seq2_completed_at,
        );
        append_test_episode(&service, "agent-1", &g1_seq2).await;

        let mut g1_seq3 = sample_episode("agent-1", "goal-1", 3, "g1-seq3");
        let g1_seq3_completed_at = Utc::now() - ChronoDuration::minutes(2);
        set_episode_window(
            &mut g1_seq3,
            g1_seq3_completed_at - ChronoDuration::seconds(1),
            g1_seq3_completed_at,
        );
        append_test_episode(&service, "agent-1", &g1_seq3).await;

        let mut g2_seq2 = sample_episode("agent-1", "goal-2", 2, "g2-seq2");
        let g2_seq2_completed_at = Utc::now() - ChronoDuration::minutes(1);
        set_episode_window(
            &mut g2_seq2,
            g2_seq2_completed_at - ChronoDuration::seconds(1),
            g2_seq2_completed_at,
        );
        append_test_episode(&service, "agent-1", &g2_seq2).await;

        let recalled = recall_test_episodes(&service, "agent-1", "goal-1", Some((2, 3)), Some(10))
            .await
            .unwrap();

        let recalled_ids = recalled
            .iter()
            .map(|episode| episode.episode_id.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            recalled_ids,
            vec!["g1-seq2".to_string(), "g1-seq3".to_string()]
        );
    }

    #[tokio::test]
    async fn recall_episodes_default_limit_and_max_limit_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        let base_time = Utc::now() - ChronoDuration::hours(2);

        for seq in 1..=60u64 {
            let mut episode = sample_episode("agent-1", "goal-1", seq, &format!("ep-{seq}"));
            let started_at = base_time + ChronoDuration::seconds(seq as i64);
            set_episode_window(
                &mut episode,
                started_at,
                started_at + ChronoDuration::milliseconds(100),
            );
            append_test_episode(&service, "agent-1", &episode).await;
        }

        let default_recall = recall_test_episodes(&service, "agent-1", "goal-1", None, None)
            .await
            .unwrap();
        assert_eq!(default_recall.len(), 10);
        assert_eq!(default_recall.first().unwrap().trigger_seq, 51);
        assert_eq!(default_recall.last().unwrap().trigger_seq, 60);

        let capped_recall = recall_test_episodes(&service, "agent-1", "goal-1", None, Some(200))
            .await
            .unwrap();
        assert_eq!(capped_recall.len(), 50);
        assert_eq!(capped_recall.first().unwrap().trigger_seq, 11);
        assert_eq!(capped_recall.last().unwrap().trigger_seq, 60);

        let err = recall_test_episodes(&service, "agent-1", "goal-1", None, Some(0))
            .await
            .expect_err("limit=0 must fail validation");
        assert!(matches!(err, AgentMemoryError::Validation(_)));
    }

    #[tokio::test]
    async fn recall_episodes_is_read_only() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        for seq in 1..=3u64 {
            let episode = sample_episode("agent-1", "goal-1", seq, &format!("ep-{seq}"));
            append_test_episode(&service, "agent-1", &episode).await;
        }

        let episodes_dir = service.storage.agent_episodes_dir("agent-1").unwrap();
        let before_files = service
            .storage
            .list_files_with_extension(&episodes_dir, "json")
            .await
            .unwrap();
        let mut before_names = before_files
            .iter()
            .filter_map(|path| path.file_name().and_then(|name| name.to_str()))
            .map(|name| name.to_string())
            .collect::<Vec<_>>();
        before_names.sort();

        let _ = recall_test_episodes(&service, "agent-1", "goal-1", Some((1, 3)), Some(2))
            .await
            .unwrap();

        let after_files = service
            .storage
            .list_files_with_extension(&episodes_dir, "json")
            .await
            .unwrap();
        let mut after_names = after_files
            .iter()
            .filter_map(|path| path.file_name().and_then(|name| name.to_str()))
            .map(|name| name.to_string())
            .collect::<Vec<_>>();
        after_names.sort();

        assert_eq!(before_names, after_names);
        let after_loaded = load_test_episodes(&service, "agent-1").await;
        assert_eq!(after_loaded.len(), 3);
    }

    // --- Tests for load_tier_by_name / save_tier_by_name ---

    #[tokio::test]
    async fn load_tier_by_name_returns_none_for_unknown_tier() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        let tier_defs = vec![sample_tier()];

        let result = load_test_tier_by_name(
            &service,
            "agent-1",
            "nonexistent_tier",
            &tier_defs,
            Some("g1"),
        )
        .await
        .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn save_tier_by_name_rejects_unknown_tier() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        let tier_defs = vec![sample_tier()];
        let data = V3MemoryTierRecord::new(
            "nonexistent_tier",
            TierScope::AgentGoal,
            None,
            None,
            None,
            None,
        );

        let err = save_test_tier_by_name(
            &service,
            "agent-1",
            "nonexistent_tier",
            &tier_defs,
            Some("g1"),
            &data,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("tier definition not found"));
    }

    #[tokio::test]
    async fn tier_by_name_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        let tier_defs = vec![sample_tier()];

        let data = sample_tier_record(
            "task_progress",
            TierScope::AgentGoal,
            "context_summary",
            "hello world",
        );

        save_test_tier_by_name(
            &service,
            "agent-1",
            "task_progress",
            &tier_defs,
            Some("g1"),
            &data,
        )
        .await
        .unwrap();

        let loaded =
            load_test_tier_by_name(&service, "agent-1", "task_progress", &tier_defs, Some("g1"))
                .await
                .unwrap()
                .expect("tier should exist after save");

        assert_eq!(loaded.tier_name, "task_progress");
        assert_eq!(
            loaded
                .fields
                .get("context_summary")
                .and_then(|v| v.as_str()),
            Some("hello world")
        );
    }

    // --- Tests for EpisodeIndex ---

    #[test]
    fn episode_index_serialization_roundtrip() {
        let index = EpisodeIndex {
            entries: vec![EpisodeIndexEntry {
                filename: "goalh_abc__seq_1__eph_xyz.json".to_string(),
                goal_id: "goal-1".to_string(),
                completed_at: None,
            }],
            last_rebuilt: Some(Utc::now()),
        };

        let json = serde_json::to_string(&index).unwrap();
        let deserialized: EpisodeIndex = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.entries.len(), 1);
        assert_eq!(deserialized.entries[0].goal_id, "goal-1");
        assert!(deserialized.last_rebuilt.is_some());
    }

    #[test]
    fn episode_index_default_is_empty() {
        let index = EpisodeIndex::default();
        assert!(index.entries.is_empty());
        assert!(index.last_rebuilt.is_none());
    }

    // --- Tests for index load/save/rebuild ---

    #[tokio::test]
    async fn episode_index_load_returns_empty_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        let index = service.load_episode_index("agent-1").await.unwrap();
        assert!(index.entries.is_empty());
        assert!(index.last_rebuilt.is_none());
    }

    #[tokio::test]
    async fn episode_index_save_and_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        let index = EpisodeIndex {
            entries: vec![EpisodeIndexEntry {
                filename: "test.json".to_string(),
                goal_id: "g1".to_string(),
                completed_at: None,
            }],
            last_rebuilt: Some(Utc::now()),
        };

        service.save_episode_index("agent-1", &index).await.unwrap();
        let loaded = service.load_episode_index("agent-1").await.unwrap();

        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].filename, "test.json");
        assert_eq!(loaded.entries[0].goal_id, "g1");
        assert!(loaded.last_rebuilt.is_some());
    }

    #[tokio::test]
    async fn rebuild_episode_index_scans_episode_files() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        let ep1 = sample_episode("agent-1", "g1", 1, "ep-1");
        let ep2 = sample_episode("agent-1", "g1", 2, "ep-2");
        let ep3 = sample_episode("agent-1", "g2", 3, "ep-3");

        append_test_episode(&service, "agent-1", &ep1).await;
        append_test_episode(&service, "agent-1", &ep2).await;
        append_test_episode(&service, "agent-1", &ep3).await;

        let index = service.rebuild_episode_index("agent-1").await.unwrap();

        assert_eq!(index.entries.len(), 3);
        assert!(index.last_rebuilt.is_some());

        let g1_entries: Vec<_> = index.entries.iter().filter(|e| e.goal_id == "g1").collect();
        let g2_entries: Vec<_> = index.entries.iter().filter(|e| e.goal_id == "g2").collect();
        assert_eq!(g1_entries.len(), 2);
        assert_eq!(g2_entries.len(), 1);

        // Verify the index was persisted to disk
        let persisted = service.load_episode_index("agent-1").await.unwrap();
        assert_eq!(persisted.entries.len(), 3);
    }

    #[tokio::test]
    async fn recent_episode_projection_rebuilds_legacy_metadata_and_returns_only_newest() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        let base = Utc.with_ymd_and_hms(2026, 8, 16, 0, 0, 0).single().unwrap();
        for sequence in 0..6u64 {
            let mut episode = sample_episode("agent-1", "g1", sequence, &format!("ep-{sequence}"));
            set_episode_window(
                &mut episode,
                base + ChronoDuration::minutes(sequence as i64),
                base + ChronoDuration::minutes(sequence as i64) + ChronoDuration::seconds(1),
            );
            append_test_episode(&service, "agent-1", &episode).await;
        }

        let mut legacy_index = service.load_episode_index("agent-1").await.unwrap();
        for entry in &mut legacy_index.entries {
            entry.completed_at = None;
        }
        service
            .save_episode_index("agent-1", &legacy_index)
            .await
            .unwrap();

        let recent = service
            .load_recent_native_episodes("agent-1", 2)
            .await
            .unwrap();
        assert_eq!(
            recent
                .iter()
                .map(|episode| episode.episode_id.as_str())
                .collect::<Vec<_>>(),
            vec!["ep-5", "ep-4"]
        );
        assert!(service
            .load_episode_index("agent-1")
            .await
            .unwrap()
            .entries
            .iter()
            .all(|entry| entry.completed_at.is_some()));
    }

    // --- Tests for append_episode index maintenance ---

    #[tokio::test]
    async fn append_episode_incrementally_updates_index() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        let ep1 = sample_episode("agent-1", "g1", 1, "ep-1");
        append_test_episode(&service, "agent-1", &ep1).await;

        let index = service.load_episode_index("agent-1").await.unwrap();
        assert_eq!(index.entries.len(), 1);
        assert_eq!(index.entries[0].goal_id, "g1");

        // Append second episode
        let ep2 = sample_episode("agent-1", "g2", 2, "ep-2");
        append_test_episode(&service, "agent-1", &ep2).await;

        let index = service.load_episode_index("agent-1").await.unwrap();
        assert_eq!(index.entries.len(), 2);
    }

    #[tokio::test]
    async fn concurrent_episode_append_and_rebuild_cannot_lose_the_new_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        let first = sample_episode("agent-1", "g1", 1, "ep-1");
        append_test_episode(&service, "agent-1", &first).await;

        let second = sample_episode("agent-1", "g1", 2, "ep-2");
        let (append_result, rebuild_result) = tokio::join!(
            service.append_native_episode("agent-1", &second),
            service.rebuild_episode_index("agent-1")
        );
        append_result.expect("concurrent append should complete");
        rebuild_result.expect("concurrent rebuild should complete");

        let filenames = service
            .load_episode_index("agent-1")
            .await
            .unwrap()
            .entries
            .into_iter()
            .map(|entry| entry.filename)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(filenames.len(), 2);
        assert!(filenames.contains(&service.native_episode_file_name(&first)));
        assert!(filenames.contains(&service.native_episode_file_name(&second)));
    }

    #[tokio::test]
    async fn append_cannot_mark_an_invalidated_episode_index_complete() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        let first = sample_episode("agent-1", "g1", 1, "ep-1");
        append_test_episode(&service, "agent-1", &first).await;
        service.rebuild_episode_index("agent-1").await.unwrap();
        service.invalidate_episode_index("agent-1").await.unwrap();

        let second = sample_episode("agent-1", "g1", 2, "ep-2");
        append_test_episode(&service, "agent-1", &second).await;
        assert!(service
            .load_episode_index("agent-1")
            .await
            .unwrap()
            .last_rebuilt
            .is_none());

        let recent = service
            .load_recent_native_episodes("agent-1", 2)
            .await
            .unwrap();
        assert_eq!(recent.len(), 2);
        assert!(service
            .load_episode_index("agent-1")
            .await
            .unwrap()
            .last_rebuilt
            .is_some());
    }

    // --- Tests for index-aware load_episodes_filtered ---

    #[tokio::test]
    async fn load_episodes_filtered_uses_index_when_fresh() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        // Create episodes for two different goals
        for seq in 1..=3u64 {
            let ep = sample_episode("agent-1", "g1", seq, &format!("ep-g1-{seq}"));
            append_test_episode(&service, "agent-1", &ep).await;
        }
        for seq in 4..=6u64 {
            let ep = sample_episode("agent-1", "g2", seq, &format!("ep-g2-{seq}"));
            append_test_episode(&service, "agent-1", &ep).await;
        }

        // Rebuild index to set last_rebuilt
        service.rebuild_episode_index("agent-1").await.unwrap();

        // Recall episodes for g1 only — should filter via index
        let g1_episodes = recall_test_episodes(&service, "agent-1", "g1", None, None)
            .await
            .unwrap();
        assert_eq!(g1_episodes.len(), 3);
        for ep in &g1_episodes {
            assert_eq!(ep.goal_id(), "g1");
        }
    }

    #[tokio::test]
    async fn load_episodes_filtered_falls_back_when_no_index() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());

        let ep = sample_episode("agent-1", "g1", 1, "ep-1");
        append_test_episode(&service, "agent-1", &ep).await;

        // Remove the index file so it falls back to full scan
        let index_path = service.storage.agent_episode_index_path("agent-1").unwrap();
        let _ = tokio::fs::remove_file(&index_path).await;

        let episodes = recall_test_episodes(&service, "agent-1", "g1", None, None)
            .await
            .unwrap();
        assert_eq!(episodes.len(), 1);

        // Index is NOT rebuilt as a side-effect of reading (P2-7 fix).
        // The index file was deleted, so it should remain empty.
        let index = service.load_episode_index("agent-1").await.unwrap();
        assert_eq!(index.entries.len(), 0);
    }

    // ================================================================
    // User memory isolation tests
    // ================================================================

    fn user_tier_definition(name: &str) -> MemoryTierDefinition {
        MemoryTierDefinition {
            name: name.to_string(),
            scope: TierScope::User,
            description: "user-scoped tier".to_string(),
            schema: BTreeMap::new(),
            render: RenderConfig {
                format: "compact_summary".to_string(),
                template: "{items}".to_string(),
            },
            retention: RetentionMode::Forever,
        }
    }

    fn sample_tier_record(
        tier_name: &str,
        tier_scope: TierScope,
        key: &str,
        value: &str,
    ) -> V3MemoryTierRecord {
        let mut data = V3MemoryTierRecord::new(tier_name, tier_scope, None, None, None, None);
        data.fields.insert(
            key.to_string(),
            serde_json::Value::String(value.to_string()),
        );
        data
    }

    #[tokio::test]
    async fn test_shared_mode_writes_to_shared_pool() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        service.ensure_base_layout().await.unwrap();
        service.ensure_agent_layout("agent-a").await.unwrap();

        let tier_def = user_tier_definition("prefs");
        let data = sample_tier_record("prefs", TierScope::User, "theme", "dark");

        // Write with Shared isolation
        service
            .save_tier_isolated(
                "agent-a",
                &tier_def,
                None,
                &data,
                &UserMemoryIsolation::Shared,
            )
            .await
            .unwrap();

        // Verify file landed in shared user root
        let shared_path = service.storage.user_root().join("prefs.json");
        assert!(
            shared_path.exists(),
            "tier should be written to shared user root"
        );

        // Verify agent's isolated dir does NOT contain the file
        let isolated_path = service
            .storage
            .agent_user_memory_dir("agent-a")
            .unwrap()
            .join("prefs.json");
        assert!(
            !isolated_path.exists(),
            "shared mode should not write to isolated dir"
        );

        // Load it back via Shared
        let loaded = service
            .load_tier_isolated("agent-a", &tier_def, None, &UserMemoryIsolation::Shared)
            .await
            .unwrap();
        assert!(loaded.is_some());
        assert_eq!(
            loaded.unwrap().fields.get("theme"),
            Some(&serde_json::Value::String("dark".to_string()))
        );
    }

    #[tokio::test]
    async fn test_fully_isolated_writes_to_local() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        service.ensure_base_layout().await.unwrap();
        service.ensure_agent_layout("agent-b").await.unwrap();

        let tier_def = user_tier_definition("prefs");
        let data = sample_tier_record("prefs", TierScope::User, "lang", "rust");

        // Write with FullyIsolated
        service
            .save_tier_isolated(
                "agent-b",
                &tier_def,
                None,
                &data,
                &UserMemoryIsolation::FullyIsolated,
            )
            .await
            .unwrap();

        // Verify file landed in agent's isolated user_memory dir
        let isolated_path = service
            .storage
            .agent_user_memory_dir("agent-b")
            .unwrap()
            .join("prefs.json");
        assert!(
            isolated_path.exists(),
            "FullyIsolated should write to agent-scoped user_memory dir"
        );

        // Verify shared pool does NOT contain the file
        let shared_path = service.storage.user_root().join("prefs.json");
        assert!(
            !shared_path.exists(),
            "FullyIsolated should not write to shared user root"
        );
    }

    #[tokio::test]
    async fn test_fully_isolated_reads_only_local() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        service.ensure_base_layout().await.unwrap();
        service.ensure_agent_layout("agent-c").await.unwrap();

        let tier_def = user_tier_definition("prefs");

        // Write data into SHARED pool
        let shared_data = sample_tier_record("prefs", TierScope::User, "color", "blue");
        service
            .save_tier_isolated(
                "agent-c",
                &tier_def,
                None,
                &shared_data,
                &UserMemoryIsolation::Shared,
            )
            .await
            .unwrap();

        // Reading with FullyIsolated should NOT see the shared data
        let loaded = service
            .load_tier_isolated(
                "agent-c",
                &tier_def,
                None,
                &UserMemoryIsolation::FullyIsolated,
            )
            .await
            .unwrap();
        assert!(
            loaded.is_none(),
            "FullyIsolated read should not see shared pool data"
        );

        // Write data into isolated dir, then it should be visible
        let local_data = sample_tier_record("prefs", TierScope::User, "color", "red");
        service
            .save_tier_isolated(
                "agent-c",
                &tier_def,
                None,
                &local_data,
                &UserMemoryIsolation::FullyIsolated,
            )
            .await
            .unwrap();

        let loaded = service
            .load_tier_isolated(
                "agent-c",
                &tier_def,
                None,
                &UserMemoryIsolation::FullyIsolated,
            )
            .await
            .unwrap();
        assert!(loaded.is_some());
        assert_eq!(
            loaded.unwrap().fields.get("color"),
            Some(&serde_json::Value::String("red".to_string()))
        );
    }

    #[tokio::test]
    async fn test_worker_isolation_field_ignored() {
        // Worker agents always use the default (Shared) isolation.
        // The `user_memory_isolation` field on the definition struct defaults to
        // Shared. Even if a worker definition somehow got FullyIsolated, the
        // isolation routing still functions correctly — but the important
        // behavioural point is that the default Shared value means Worker
        // agents continue to read/write the shared pool.
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        service.ensure_base_layout().await.unwrap();
        service.ensure_agent_layout("worker-1").await.unwrap();

        let tier_def = user_tier_definition("prefs");
        let data = sample_tier_record("prefs", TierScope::User, "mode", "worker");

        // Worker uses default Shared isolation
        let isolation = UserMemoryIsolation::default();
        assert_eq!(isolation, UserMemoryIsolation::Shared);

        service
            .save_tier_isolated("worker-1", &tier_def, None, &data, &isolation)
            .await
            .unwrap();

        // Data is in shared pool
        let shared_path = service.storage.user_root().join("prefs.json");
        assert!(
            shared_path.exists(),
            "Worker with default isolation should write to shared pool"
        );

        // Verify load also reads from shared pool
        let loaded = service
            .load_tier_isolated("worker-1", &tier_def, None, &isolation)
            .await
            .unwrap();
        assert!(loaded.is_some());
        assert_eq!(
            loaded.unwrap().fields.get("mode"),
            Some(&serde_json::Value::String("worker".to_string()))
        );
    }

    #[tokio::test]
    async fn copy_shared_to_isolated_snapshots_all_tiers() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        service.ensure_base_layout().await.unwrap();
        service.ensure_agent_layout("agent-d").await.unwrap();

        // Write two tiers to shared pool
        let tier_a = user_tier_definition("pref-a");
        let data_a = sample_tier_record("pref-a", TierScope::User, "k", "v1");
        service
            .save_tier_isolated(
                "agent-d",
                &tier_a,
                None,
                &data_a,
                &UserMemoryIsolation::Shared,
            )
            .await
            .unwrap();

        let tier_b = user_tier_definition("pref-b");
        let data_b = sample_tier_record("pref-b", TierScope::User, "k", "v2");
        service
            .save_tier_isolated(
                "agent-d",
                &tier_b,
                None,
                &data_b,
                &UserMemoryIsolation::Shared,
            )
            .await
            .unwrap();

        let copied = service.copy_shared_to_isolated("agent-d").await.unwrap();
        assert_eq!(copied, 2);

        // Isolated dir now has both files
        let isolated_root = service.storage.agent_user_memory_dir("agent-d").unwrap();
        assert!(isolated_root.join("pref-a.json").exists());
        assert!(isolated_root.join("pref-b.json").exists());
    }

    #[tokio::test]
    async fn merge_isolated_to_shared_additive_local_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_base_path(tmp.path());
        service.ensure_base_layout().await.unwrap();
        service.ensure_agent_layout("agent-e").await.unwrap();

        let tier_def = user_tier_definition("prefs");

        // Write to shared: {theme: "light", font: "mono"}
        let mut shared_data =
            V3MemoryTierRecord::new("prefs", TierScope::User, None, None, None, None);
        shared_data.fields.insert(
            "theme".to_string(),
            serde_json::Value::String("light".to_string()),
        );
        shared_data.fields.insert(
            "font".to_string(),
            serde_json::Value::String("mono".to_string()),
        );
        service
            .save_tier_isolated(
                "agent-e",
                &tier_def,
                None,
                &shared_data,
                &UserMemoryIsolation::Shared,
            )
            .await
            .unwrap();

        // Write to isolated: {theme: "dark", lang: "rust"}
        let mut local_data =
            V3MemoryTierRecord::new("prefs", TierScope::User, None, None, None, None);
        local_data.fields.insert(
            "theme".to_string(),
            serde_json::Value::String("dark".to_string()),
        );
        local_data.fields.insert(
            "lang".to_string(),
            serde_json::Value::String("rust".to_string()),
        );
        service
            .save_tier_isolated(
                "agent-e",
                &tier_def,
                None,
                &local_data,
                &UserMemoryIsolation::FullyIsolated,
            )
            .await
            .unwrap();

        let merged_count = service.merge_isolated_to_shared("agent-e").await.unwrap();
        assert_eq!(merged_count, 1);

        // Verify shared pool now has merged data: local wins on conflict
        let result = service
            .load_tier_isolated("agent-e", &tier_def, None, &UserMemoryIsolation::Shared)
            .await
            .unwrap()
            .unwrap();

        // "theme" should be "dark" (local wins)
        assert_eq!(
            result.fields.get("theme"),
            Some(&serde_json::Value::String("dark".to_string()))
        );
        // "font" should be preserved from shared
        assert_eq!(
            result.fields.get("font"),
            Some(&serde_json::Value::String("mono".to_string()))
        );
        // "lang" should be added from local
        assert_eq!(
            result.fields.get("lang"),
            Some(&serde_json::Value::String("rust".to_string()))
        );
    }

    #[tokio::test]
    async fn scoped_memory_stores_native_v3_episode_records() {
        let tmp = tempfile::tempdir().unwrap();
        let service =
            AgentMemoryService::with_scoped_memory_scope(tmp.path(), "principal-a", "workspace-a");
        service.ensure_base_layout().await.unwrap();
        service.ensure_agent_layout("agent-1").await.unwrap();

        let episode = sample_episode("agent-1", "goal-a", 42, "ep-native");
        service
            .append_native_episode("agent-1", &native_episode_record(&service, &episode))
            .await
            .unwrap();

        let stored_path = service
            .storage
            .agent_episodes_dir("agent-1")
            .unwrap()
            .join("ep-native.json");
        let stored: V3EpisodeRecord = service.storage.read_json(&stored_path).await.unwrap();

        assert_eq!(stored.record_type, "memory_episode");
        assert_eq!(
            stored.schema_version,
            V3_MEMORY_EPISODE_SCHEMA_ORIGIN_STAMPED
        );
        assert_eq!(stored.principal.as_deref(), Some("principal-a"));
        assert_eq!(stored.workspace.as_deref(), Some("workspace-a"));
        assert_eq!(stored.agent_id, "agent-1");
        assert_eq!(stored.goal_key, "goal-a");
        assert_eq!(stored.trigger_seq, 42);
        assert_eq!(stored.outcome_kind, "goal_achieved");

        let loaded = service
            .load_native_episodes_for_goal("agent-1", "goal-a")
            .await
            .unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].episode_id, "ep-native");
    }

    #[tokio::test]
    async fn scoped_memory_stores_native_v3_tier_records() {
        let tmp = tempfile::tempdir().unwrap();
        let service =
            AgentMemoryService::with_scoped_memory_scope(tmp.path(), "principal-b", "workspace-b");
        service.ensure_base_layout().await.unwrap();
        service.ensure_agent_layout("agent-2").await.unwrap();

        let tier = sample_tier();
        let mut data =
            sample_tier_record("task_progress", TierScope::AgentGoal, "status", "running");
        data.principal = Some("principal-b".to_string());
        data.workspace = Some("workspace-b".to_string());
        data.agent_id = Some("agent-2".to_string());
        data.goal_id = Some("goal-b".to_string());
        service
            .save_native_tier("agent-2", &tier, Some("goal-b"), &data)
            .await
            .unwrap();

        let stored_path = service
            .storage
            .agent_tier_path("agent-2", "task_progress", &tier.scope, Some("goal-b"))
            .unwrap();
        let stored: V3MemoryTierRecord = service.storage.read_json(&stored_path).await.unwrap();

        assert_eq!(stored.record_type, "memory_tier");
        assert_eq!(stored.schema_version, "v3_memory_tier/v1");
        assert_eq!(stored.principal.as_deref(), Some("principal-b"));
        assert_eq!(stored.workspace.as_deref(), Some("workspace-b"));
        assert_eq!(stored.agent_id.as_deref(), Some("agent-2"));
        assert_eq!(stored.goal_id.as_deref(), Some("goal-b"));
        assert!(matches!(stored.tier_scope, TierScope::AgentGoal));

        let loaded = service
            .load_native_tier("agent-2", &tier, Some("goal-b"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            loaded.fields.get("status"),
            Some(&serde_json::Value::String("running".to_string()))
        );
    }
}
