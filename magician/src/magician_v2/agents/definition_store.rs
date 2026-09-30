//! Definition repository for TRUE_AGENTS Phase 3(a)-02.
//!
//! Provides CRUD operations by stable `agent_id`, version archives, and
//! optimistic concurrency via expected-version checks.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, RwLock as StdRwLock},
    time::SystemTime,
};

use chrono::{DateTime, Utc};
use fs2::FileExt;
use magician_vector_index::memory_index::{record_memory_index_change, MemoryIndexChange};
use thiserror::Error;
use tokio::{
    fs, task,
    time::{Duration, Instant},
};
use tracing::warn;

use super::{
    storage::{AgentStorage, AgentStorageError},
    types::{disabled_agent_hierarchy, AgentDefinition, AgentDefinitionError, AgentKind},
};
use crate::magician_v2::{
    analytics::memory_index_maintainer::mark_memory_index_dirty_for_scope,
    artifact_v2::workspace::ArtifactV2Workspace,
};

fn build_extra_template_storages() -> Vec<AgentStorage> {
    crate::magician_v2::config_extras::extra_agent_template_dirs()
        .into_iter()
        .map(AgentStorage::new)
        .collect()
}

const STORE_WRITE_LOCK_FILE_NAME: &str = ".definition_store.write.lock";
const STORE_WRITE_LOCK_RETRY_DELAY_MIN: Duration = Duration::from_millis(10);
const STORE_WRITE_LOCK_RETRY_DELAY_MAX: Duration = Duration::from_millis(250);
const STORE_WRITE_LOCK_MAX_WAIT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionRecord {
    pub definition: AgentDefinition,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl DefinitionRecord {
    pub fn version(&self) -> u32 {
        self.definition.version
    }

    pub fn etag(&self) -> String {
        format!("v{}", self.version())
    }
}

#[derive(Debug, Error)]
pub enum DefinitionStoreError {
    #[error("storage error: {0}")]
    Storage(#[from] AgentStorageError),
    #[error("agent definition validation error: {0}")]
    Validation(String),
    #[error("definition error: {0}")]
    Definition(#[from] AgentDefinitionError),
    #[error("agent definition `{0}` not found")]
    NotFound(String),
    #[error("agent definition `{0}` already exists")]
    AlreadyExists(String),
    #[error(
        "version conflict for agent `{agent_id}`: expected v{expected_version}, current v{current_version}"
    )]
    VersionConflict {
        agent_id: String,
        expected_version: u32,
        current_version: u32,
    },
    #[error(
        "agent_id mismatch: path agent `{path_agent_id}` does not match payload agent `{payload_agent_id}`"
    )]
    AgentIdMismatch {
        path_agent_id: String,
        payload_agent_id: String,
    },
    #[error("timed out acquiring definition store write lock `{lock_path}` after {wait_ms}ms")]
    LockTimeout { lock_path: String, wait_ms: u64 },
}

#[derive(Debug, Clone, Copy)]
struct StoreWriteLockConfig {
    retry_delay_min: Duration,
    retry_delay_max: Duration,
    max_wait: Duration,
}

impl Default for StoreWriteLockConfig {
    fn default() -> Self {
        Self {
            retry_delay_min: STORE_WRITE_LOCK_RETRY_DELAY_MIN,
            retry_delay_max: STORE_WRITE_LOCK_RETRY_DELAY_MAX,
            max_wait: STORE_WRITE_LOCK_MAX_WAIT,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentDefinitionStore {
    storage: AgentStorage,
    /// Primary template storage (writeable; built-in templates live here).
    template_storage: Option<AgentStorage>,
    /// Read-only overlay template storages discovered via
    /// [`set_extra_template_roots`]. Layered after `template_storage`:
    /// later entries override earlier on `agent_id` collision; any of these
    /// overrides the primary. Writes always go to the primary.
    extra_template_storages: Vec<AgentStorage>,
    workspace_layout: Option<ArtifactV2Workspace>,
    current_scope: Option<(String, String)>,
    /// Shared in-memory cache: per-scope definition lists + the set of scopes whose
    /// builtin templates have been materialized. Shared across `for_scope` clones via
    /// `Arc`, so a write's invalidation is seen by the scheduler's per-scope reads.
    /// Eliminates the 5-second scheduler tick's disk re-scan + re-materialization.
    cache: Arc<DefinitionCache>,
}

/// Process-lifetime cache behind [`AgentDefinitionStore`]. The scheduler tick reads
/// definitions across every scope every few seconds; without this it re-read + re-parsed
/// every definition YAML (and re-materialized templates) from disk each tick.
#[derive(Default)]
struct DefinitionCache {
    /// (principal, workspace) -> the last-read definition list for that scope.
    lists: StdRwLock<HashMap<(String, String), Arc<Vec<DefinitionRecord>>>>,
    /// Scopes whose builtin templates have already been materialized this process
    /// (boot-once — makes the per-tick `ensure_builtin_templates_materialized` a no-op).
    materialized: StdRwLock<HashSet<(String, String)>>,
}

impl std::fmt::Debug for DefinitionCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefinitionCache").finish_non_exhaustive()
    }
}

impl AgentDefinitionStore {
    pub fn new(storage: AgentStorage) -> Self {
        Self {
            storage,
            template_storage: None,
            extra_template_storages: Vec::new(),
            workspace_layout: None,
            current_scope: None,
            cache: Arc::new(DefinitionCache::default()),
        }
    }

    pub fn with_base_path(base: impl AsRef<Path>) -> Self {
        Self::new(AgentStorage::new(base))
    }

    pub fn with_workspace_root(base_root: impl AsRef<Path>) -> Self {
        let workspace_layout =
            ArtifactV2Workspace::new(ArtifactV2Workspace::resolve_scoped_root(base_root.as_ref()));
        Self::with_workspace_layout(workspace_layout)
    }

    pub fn with_workspace_layout(workspace_layout: ArtifactV2Workspace) -> Self {
        Self {
            // Templates are read-only SEED files (under the seed root, which may be
            // outside the runtime workspace provider's root) — read them via
            // std::fs (AgentStorage::new), NOT the runtime provider, matching
            // build_extra_template_storages.
            storage: AgentStorage::new(workspace_layout.system_agent_template_root()),
            template_storage: Some(AgentStorage::new(
                workspace_layout.system_agent_template_root(),
            )),
            extra_template_storages: build_extra_template_storages(),
            workspace_layout: Some(workspace_layout),
            current_scope: None,
            cache: Arc::new(DefinitionCache::default()),
        }
    }

    pub fn for_scope(&self, principal: &str, workspace: &str) -> Self {
        let Some(layout) = self.workspace_layout.clone() else {
            return self.clone();
        };
        Self {
            storage: AgentStorage::with_scoped_memory_root_in_workspace(
                layout.scoped_agent_runtime_root(principal, workspace),
                layout.clone(),
            ),
            // Seed templates → std::fs (see with_workspace_layout above).
            template_storage: Some(AgentStorage::new(layout.system_agent_template_root())),
            extra_template_storages: build_extra_template_storages(),
            workspace_layout: Some(layout),
            current_scope: Some((principal.to_string(), workspace.to_string())),
            // Share the root store's cache so a write in one scope invalidates the
            // scheduler's per-scope reads (and boot-once materialization is shared).
            cache: self.cache.clone(),
        }
    }

    pub fn storage(&self) -> &AgentStorage {
        &self.storage
    }

    pub fn current_scope(&self) -> Option<(String, String)> {
        self.current_scope.clone()
    }

    async fn mark_memory_index_dirty(&self, reason: &'static str) {
        if let Some((principal, workspace)) = self.current_scope.as_ref() {
            if let Err(error) = record_memory_index_change(
                self.storage(),
                MemoryIndexChange::FullScope {
                    reason: reason.to_string(),
                },
            )
            .await
            {
                warn!(
                    error = %error,
                    "failed to persist definition-change memory index marker; dirty rebuild remains the fallback"
                );
            }
            mark_memory_index_dirty_for_scope(principal.as_str(), workspace.as_str(), reason);
        }
    }

    pub fn template_storage(&self) -> Option<&AgentStorage> {
        self.template_storage.as_ref()
    }

    pub fn workspace_layout(&self) -> Option<&ArtifactV2Workspace> {
        self.workspace_layout.as_ref()
    }

    fn stamp_owned_scope(&self, definition: &mut AgentDefinition) {
        let Some(layout) = self.workspace_layout.as_ref() else {
            return;
        };
        let storage_root = self.storage.root();
        let scopes_root_path = layout.base_root().join("scopes");
        let Some(scopes_root) = scopes_root_path.to_str() else {
            return;
        };
        let storage_root_str = storage_root.to_string_lossy();
        if !storage_root_str.starts_with(scopes_root) {
            return;
        }
        let Ok(relative) = storage_root.strip_prefix(&scopes_root_path) else {
            return;
        };
        let mut segments = relative.iter();
        let Some(principal) = segments.next().and_then(|value| value.to_str()) else {
            return;
        };
        let Some(workspace) = segments.next().and_then(|value| value.to_str()) else {
            return;
        };
        definition.principal = Some(principal.to_string());
        definition.workspace = Some(workspace.to_string());
    }

    fn has_scoped_storage(&self) -> bool {
        self.current_scope.is_some()
    }

    fn builtin_template_definitions() -> Vec<AgentDefinition> {
        Vec::new()
    }

    async fn ensure_template_storage_seeded(&self) -> Result<(), DefinitionStoreError> {
        let Some(template_storage) = self.template_storage.as_ref() else {
            return Ok(());
        };
        template_storage
            .create_dir_all(template_storage.agents_root())
            .await?;
        for definition in Self::builtin_template_definitions() {
            let path = template_storage.agent_definition_path(&definition.agent_id)?;
            if !template_storage.exists(&path).await? {
                template_storage
                    .ensure_agent_layout(&definition.agent_id)
                    .await?;
                template_storage
                    .write_yaml_atomic(&path, &definition)
                    .await?;
            }
        }
        Ok(())
    }

    /// Resolve a template by `agent_id` across the extras-declared
    /// roots and the primary template root. Extras override the
    /// primary, and earlier-listed extras override later-listed —
    /// matches the SkillLoader / boot-snapshot / dispatcher / browser
    /// resolver convention (first-wins, à la PATH / XDG_DATA_DIRS).
    async fn read_template_record_layered(
        &self,
        agent_id: &str,
    ) -> Result<Option<DefinitionRecord>, DefinitionStoreError> {
        for storage in &self.extra_template_storages {
            if let Some(record) = self.read_definition_from_storage(storage, agent_id).await? {
                return Ok(Some(record));
            }
        }
        let Some(primary) = self.template_storage.as_ref() else {
            return Ok(None);
        };
        self.read_definition_from_storage(primary, agent_id).await
    }

    async fn materialize_template_into_scope(
        &self,
        agent_id: &str,
    ) -> Result<Option<DefinitionRecord>, DefinitionStoreError> {
        if !self.has_scoped_storage() {
            return Ok(None);
        }
        if self.template_storage.is_none() {
            return Ok(None);
        }
        self.ensure_template_storage_seeded().await?;
        let Some(mut record) = self.read_template_record_layered(agent_id).await? else {
            return Ok(None);
        };
        self.stamp_owned_scope(&mut record.definition);
        self.storage.ensure_base_layout().await?;
        self.storage.ensure_agent_layout(agent_id).await?;
        let path = self.storage.agent_definition_path(agent_id)?;
        if !self.storage.exists(&path).await? {
            self.storage
                .write_yaml_atomic(&path, &record.definition)
                .await?;
        }
        self.read_definition_from_storage(&self.storage, agent_id)
            .await
    }

    async fn ensure_builtin_templates_materialized(&self) -> Result<(), DefinitionStoreError> {
        if !self.has_scoped_storage() {
            return Ok(());
        }
        // The `system/system` bucket exists only as a fall-through for
        // unscoped/diagnostic transport events (see
        // `transport_log.rs:SYSTEM_PRINCIPAL`). It is NOT a user scope, so
        // materializing every system template into it just litters
        // `magician_data_v3/scopes/system/system/agent_runtime/agents/`
        // with redundant copies that the scheduler-tick scope enumerator
        // then has to read on every tick. Skip it.
        if let Some((principal, workspace)) = self.current_scope.as_ref() {
            if principal == "system" && workspace == "system" {
                return Ok(());
            }
            // Boot-once: after templates are materialized for a scope this is a no-op
            // (the 5-second scheduler tick would otherwise re-run it every tick).
            if self
                .cache
                .materialized
                .read()
                .expect("definition cache lock poisoned")
                .contains(&(principal.clone(), workspace.clone()))
            {
                return Ok(());
            }
        }
        self.ensure_template_storage_seeded().await?;
        let Some(template_storage) = self.template_storage.as_ref() else {
            return Ok(());
        };
        // Collect agent_ids across primary + extras, deduped (extras only
        // add ids not present in primary — extras already shadow primary
        // in `read_template_record_layered`, so just one materialize call
        // per id is enough).
        let mut seen_ids: HashSet<String> = HashSet::new();
        let mut ordered_ids: Vec<String> = Vec::new();
        for record in self.list_definitions_from_storage(template_storage).await? {
            if seen_ids.insert(record.definition.agent_id.clone()) {
                ordered_ids.push(record.definition.agent_id);
            }
        }
        for extra_storage in &self.extra_template_storages {
            for record in self.list_definitions_from_storage(extra_storage).await? {
                if seen_ids.insert(record.definition.agent_id.clone()) {
                    ordered_ids.push(record.definition.agent_id);
                }
            }
        }
        for agent_id in ordered_ids {
            let _ = self.materialize_template_into_scope(&agent_id).await?;
        }
        if let Some(scope) = self.current_scope.as_ref() {
            self.cache
                .materialized
                .write()
                .expect("definition cache lock poisoned")
                .insert(scope.clone());
        }
        Ok(())
    }

    pub async fn list_all_definitions_across_scopes(
        &self,
    ) -> Result<Vec<DefinitionRecord>, DefinitionStoreError> {
        let Some(layout) = self.workspace_layout.as_ref() else {
            return self.list_definitions().await;
        };

        let mut records = Vec::new();
        for (principal, workspace) in layout.list_scope_segments().await.map_err(|err| {
            DefinitionStoreError::Storage(AgentStorageError::Io(std::io::Error::other(
                err.to_string(),
            )))
        })? {
            let scoped_store = self.for_scope(&principal, &workspace);
            records.extend(scoped_store.list_definitions().await?);
        }

        records.sort_by(|left, right| {
            let left_scope = (
                left.definition.principal.as_deref().unwrap_or(""),
                left.definition.workspace.as_deref().unwrap_or(""),
                left.definition.agent_id.as_str(),
            );
            let right_scope = (
                right.definition.principal.as_deref().unwrap_or(""),
                right.definition.workspace.as_deref().unwrap_or(""),
                right.definition.agent_id.as_str(),
            );
            left_scope.cmp(&right_scope)
        });
        Ok(records)
    }

    /// The distinct `(principal, workspace)` scope stamps carried by the
    /// definitions [`Self::list_all_definitions_across_scopes`] would return,
    /// sorted, without materializing a single [`DefinitionRecord`] clone.
    ///
    /// Same source of truth and same filter as deriving the tuples from that
    /// method's result (a definition contributes only when it stamps *both*
    /// `principal` and `workspace`), but it borrows each scope's cached
    /// `Arc<Vec<DefinitionRecord>>` instead of deep-cloning it. The 5s agent
    /// scheduler tick only ever wanted these tuples; cloning ~130 parsed
    /// definitions every tick to throw all of them away was pure allocator
    /// churn on the supervisor runtime.
    pub async fn list_definition_scope_keys(
        &self,
    ) -> Result<Vec<(String, String)>, DefinitionStoreError> {
        let mut keys = Vec::new();

        match self.workspace_layout.as_ref() {
            None => push_definition_scope_keys(&self.list_definitions_shared().await?, &mut keys),
            Some(layout) => {
                for (principal, workspace) in layout.list_scope_segments().await.map_err(|err| {
                    DefinitionStoreError::Storage(AgentStorageError::Io(std::io::Error::other(
                        err.to_string(),
                    )))
                })? {
                    let scoped_store = self.for_scope(&principal, &workspace);
                    push_definition_scope_keys(
                        &scoped_store.list_definitions_shared().await?,
                        &mut keys,
                    );
                }
            },
        }

        keys.sort();
        Ok(keys)
    }

    pub async fn create_definition(
        &self,
        mut definition: AgentDefinition,
    ) -> Result<DefinitionRecord, DefinitionStoreError> {
        self.storage.ensure_base_layout().await?;
        let _lock = acquire_store_write_lock(&self.storage).await?;

        if definition.version != 1 {
            return Err(DefinitionStoreError::Validation(format!(
                "create payload version must be v1, got v{}",
                definition.version
            )));
        }
        // Creation always starts at version 1; updates are versioned separately.
        definition.version = 1;
        self.stamp_owned_scope(&mut definition);

        // Inject sane defaults (e.g. memory tiers for Personal agents) when the
        // caller didn't provide them.  This mirrors the `from_yaml_str()` path
        // so that UI-created agents get the same baseline as YAML-defined ones.
        definition.apply_defaults();

        let path = match self.storage.agent_definition_path(&definition.agent_id) {
            Ok(path) => path,
            Err(path_err) => {
                // Preserve validator-style error semantics for invalid identifiers while
                // still allowing duplicate-existence precedence for valid agent IDs.
                validate_definition(&definition)?;
                return Err(path_err.into());
            },
        };
        if self.storage.exists(&path).await? {
            return Err(DefinitionStoreError::AlreadyExists(definition.agent_id));
        }
        if self.has_scoped_storage() {
            self.ensure_template_storage_seeded().await?;
            // Reject the create if the id already exists in the primary
            // template root OR any extra template root — otherwise the
            // scope-overlay layering would silently shadow the external
            // template, making the create look successful while the
            // template still surfaces from disk on a fresh process.
            let primary_iter = self.template_storage.as_ref().into_iter();
            for storage in primary_iter.chain(self.extra_template_storages.iter()) {
                let template_path = storage.agent_definition_path(&definition.agent_id)?;
                if storage.exists(&template_path).await? {
                    return Err(DefinitionStoreError::AlreadyExists(definition.agent_id));
                }
            }
        }
        validate_definition(&definition)?;
        if let Err(write_err) = self.storage.write_yaml_atomic(&path, &definition).await {
            if let Err(cleanup_err) =
                rollback_failed_create_definition(&self.storage, &definition.agent_id).await
            {
                warn!(
                    agent_id = %definition.agent_id,
                    error = %cleanup_err,
                    "failed to rollback create_definition after write failure"
                );
            }
            return Err(write_err.into());
        }

        if let Err(reset_err) =
            reset_definition_versions_dir(&self.storage, &definition.agent_id).await
        {
            if let Err(cleanup_err) =
                rollback_failed_create_definition(&self.storage, &definition.agent_id).await
            {
                warn!(
                    agent_id = %definition.agent_id,
                    error = %cleanup_err,
                    "failed to rollback create_definition after versions reset failure"
                );
            }
            return Err(reset_err);
        }

        let (created_at, updated_at) = read_definition_timestamps_after_write(
            &self.storage,
            &definition.agent_id,
            &path,
            None,
        )
        .await;
        self.mark_memory_index_dirty("agent_definition_created")
            .await;
        self.invalidate_scope_cache();

        Ok(DefinitionRecord {
            definition,
            created_at,
            updated_at,
        })
    }

    pub async fn get_definition(
        &self,
        agent_id: &str,
    ) -> Result<Option<DefinitionRecord>, DefinitionStoreError> {
        if let Some(record) = self
            .read_definition_from_storage(&self.storage, agent_id)
            .await?
        {
            return Ok(Some(record));
        }
        self.materialize_template_into_scope(agent_id).await
    }

    pub async fn get_definition_any_scope(
        &self,
        agent_id: &str,
    ) -> Result<Option<DefinitionRecord>, DefinitionStoreError> {
        if let Some(record) = self
            .read_definition_from_storage(&self.storage, agent_id)
            .await?
        {
            return Ok(Some(record));
        }

        let Some(layout) = self.workspace_layout.as_ref() else {
            return Ok(None);
        };

        for (principal, workspace) in layout.list_scope_segments().await.map_err(|err| {
            DefinitionStoreError::Storage(AgentStorageError::Io(std::io::Error::other(
                err.to_string(),
            )))
        })? {
            let scoped_store = self.for_scope(&principal, &workspace);
            if let Some(record) = scoped_store.get_definition(agent_id).await? {
                return Ok(Some(record));
            }
        }

        Ok(None)
    }

    pub async fn list_definitions(&self) -> Result<Vec<DefinitionRecord>, DefinitionStoreError> {
        let shared = self.list_definitions_shared().await?;
        // Cache hit -> the `Arc` is shared, so an owning caller pays one deep copy
        // (unchanged). Cache miss with no scope -> nothing else holds the `Arc`, so
        // `try_unwrap` hands the freshly read `Vec` over without copying it at all.
        Ok(Arc::try_unwrap(shared).unwrap_or_else(|shared| (*shared).clone()))
    }

    /// [`Self::list_definitions`] without the deep copy: hands back the cached
    /// `Arc` so read-only callers that only project a few fields off each record
    /// don't allocate a whole second copy of the scope's definitions.
    ///
    /// Identical semantics otherwise — same template materialization, same
    /// per-scope cache read/fill, same storage read on a miss.
    pub(crate) async fn list_definitions_shared(
        &self,
    ) -> Result<Arc<Vec<DefinitionRecord>>, DefinitionStoreError> {
        if self.has_scoped_storage() {
            self.ensure_builtin_templates_materialized().await?;
        }
        // Serve from the per-scope cache when warm (invalidated on every write). This is
        // what stops the 5-second scheduler tick from re-reading every definition YAML.
        if let Some(scope) = self.current_scope.as_ref() {
            let cached = self
                .cache
                .lists
                .read()
                .expect("definition cache lock poisoned")
                .get(scope)
                .cloned();
            if let Some(list) = cached {
                return Ok(list);
            }
        }
        let records = Arc::new(self.list_definitions_from_storage(&self.storage).await?);
        if let Some(scope) = self.current_scope.as_ref() {
            self.cache
                .lists
                .write()
                .expect("definition cache lock poisoned")
                .insert(scope.clone(), Arc::clone(&records));
        }
        Ok(records)
    }

    /// The `Arc` currently cached for this scope, if any. Lets a test prove by
    /// pointer identity that a read path borrowed the cached list rather than
    /// deep-copying every record out of it.
    #[cfg(any(test, feature = "test-fixtures"))]
    fn cached_definitions_for_tests(&self) -> Option<Arc<Vec<DefinitionRecord>>> {
        let scope = self.current_scope.as_ref()?;
        self.cache
            .lists
            .read()
            .expect("definition cache lock poisoned")
            .get(scope)
            .cloned()
    }

    /// Drop this scope's cached definition list so the next read re-reads from disk.
    /// Called after every write that mutates definitions in this scope.
    fn invalidate_scope_cache(&self) {
        if let Some(scope) = self.current_scope.as_ref() {
            self.cache
                .lists
                .write()
                .expect("definition cache lock poisoned")
                .remove(scope);
        }
    }

    /// Clear the entire definition cache and the materialized-templates set, so the next
    /// reads re-read from disk and re-materialize. Backs the manual "refresh definitions"
    /// control (API / CLI / UI) for when definition files change out-of-band.
    pub fn clear_cache(&self) {
        self.cache
            .lists
            .write()
            .expect("definition cache lock poisoned")
            .clear();
        self.cache
            .materialized
            .write()
            .expect("definition cache lock poisoned")
            .clear();
    }

    /// Clear the cache and eagerly re-materialize + reload every scope, so the next
    /// scheduler tick is warm. Returns the number of definitions reloaded.
    pub async fn refresh_all(&self) -> Result<usize, DefinitionStoreError> {
        self.clear_cache();
        Ok(self.list_all_definitions_across_scopes().await?.len())
    }

    pub async fn disabled_hierarchy_agent_ids(
        &self,
    ) -> Result<HashSet<String>, DefinitionStoreError> {
        let records = self.list_definitions().await?;
        Ok(disabled_agent_hierarchy(
            records.iter().map(|record| &record.definition),
        ))
    }

    pub async fn list_enabled_definitions(
        &self,
    ) -> Result<Vec<DefinitionRecord>, DefinitionStoreError> {
        let records = self.list_definitions().await?;
        let disabled_agent_ids =
            disabled_agent_hierarchy(records.iter().map(|record| &record.definition));
        Ok(records
            .into_iter()
            .filter(|record| !disabled_agent_ids.contains(&record.definition.agent_id))
            .collect())
    }

    pub async fn get_enabled_definition(
        &self,
        agent_id: &str,
    ) -> Result<Option<DefinitionRecord>, DefinitionStoreError> {
        let record = match self.get_definition(agent_id).await? {
            Some(record) => record,
            None => return Ok(None),
        };
        let disabled_agent_ids = self.disabled_hierarchy_agent_ids().await?;
        if disabled_agent_ids.contains(agent_id) {
            return Ok(None);
        }
        Ok(Some(record))
    }

    pub async fn get_primary_enabled_agent(
        &self,
    ) -> Result<Option<DefinitionRecord>, DefinitionStoreError> {
        let records = self.list_enabled_definitions().await?;
        Ok(records.into_iter().find(|d| d.definition.is_primary))
    }

    pub async fn update_definition(
        &self,
        agent_id: &str,
        mut definition: AgentDefinition,
        expected_version: u32,
    ) -> Result<DefinitionRecord, DefinitionStoreError> {
        self.storage.ensure_base_layout().await?;
        let _lock = acquire_store_write_lock(&self.storage).await?;

        if definition.agent_id != agent_id {
            return Err(DefinitionStoreError::AgentIdMismatch {
                path_agent_id: agent_id.to_string(),
                payload_agent_id: definition.agent_id,
            });
        }

        let current_path = self.storage.agent_definition_path(agent_id)?;
        let Some(current) =
            read_optional_definition(&self.storage, &current_path, agent_id).await?
        else {
            return Err(DefinitionStoreError::NotFound(agent_id.to_string()));
        };

        if current.version != expected_version {
            return Err(DefinitionStoreError::VersionConflict {
                agent_id: agent_id.to_string(),
                expected_version,
                current_version: current.version,
            });
        }
        if definition.version != expected_version {
            return Err(DefinitionStoreError::Validation(format!(
                "update payload version must match If-Match version v{expected_version}, got v{}",
                definition.version
            )));
        }

        let next_version = current.version.checked_add(1).ok_or_else(|| {
            DefinitionStoreError::Validation(format!(
                "agent `{agent_id}` version overflow at {}",
                current.version
            ))
        })?;
        definition.version = next_version;
        self.stamp_owned_scope(&mut definition);
        validate_definition(&definition)?;

        self.storage.ensure_agent_layout(agent_id).await?;

        let archive_path = self
            .storage
            .agent_definition_version_path(agent_id, current.version)?;
        match read_optional_definition(&self.storage, &archive_path, agent_id).await {
            Ok(Some(existing_archive)) => {
                if existing_archive != current {
                    return Err(DefinitionStoreError::Validation(format!(
                        "archive mismatch for agent `{agent_id}` at version {}",
                        current.version
                    )));
                }
            },
            Ok(None) => {
                self.storage
                    .write_yaml_atomic(&archive_path, &current)
                    .await?;
            },
            Err(err) => {
                // Corrupted archive should not permanently block updates.
                // Log the error and overwrite the corrupted file.
                warn!(
                    agent_id = %agent_id,
                    version = current.version,
                    error = %err,
                    "Corrupted archive detected during update; overwriting"
                );
                self.storage
                    .write_yaml_atomic(&archive_path, &current)
                    .await?;
            },
        }

        // Preserve original creation time across updates. If we only derive
        // `created_at` after replacing the definition file, mtime advances and
        // first-update records lose the true creation timestamp.
        let fallback_timestamps =
            read_definition_timestamps(&self.storage, agent_id, &current_path)
                .await
                .ok();
        let fallback_created_at = fallback_timestamps.map(|(created_at, _)| created_at);
        let fallback_updated_at = fallback_timestamps.map(|(_, updated_at)| updated_at);

        self.storage
            .write_yaml_atomic(&current_path, &definition)
            .await?;
        let updated_at =
            read_updated_timestamp_after_write(agent_id, &current_path, fallback_updated_at).await;
        let created_at = fallback_created_at.unwrap_or(updated_at);
        self.mark_memory_index_dirty("agent_definition_updated")
            .await;
        self.invalidate_scope_cache();

        Ok(DefinitionRecord {
            definition,
            created_at,
            updated_at,
        })
    }

    pub async fn delete_definition(&self, agent_id: &str) -> Result<bool, DefinitionStoreError> {
        self.storage.ensure_base_layout().await?;
        let _lock = acquire_store_write_lock(&self.storage).await?;

        let agent_dir = self.storage.agent_dir(agent_id)?;
        let state_dir = self.storage.agent_state_dir(agent_id)?;
        let tiers_dir = self.storage.agent_tiers_dir(agent_id)?;
        let episodes_dir = self.storage.agent_episodes_dir(agent_id)?;
        let consolidations_dir = self.storage.agent_consolidations_dir(agent_id)?;
        let memory_dir = self.storage.agent_memory_dir(agent_id)?;
        let current_path = self.storage.agent_definition_path(agent_id)?;
        let versions_dir = self.storage.agent_definition_versions_dir(agent_id)?;
        let mut removed_definition = false;

        if self.storage.exists(&current_path).await? {
            self.storage.remove_file(&current_path).await?;
            removed_definition = true;
        }

        if self.storage.exists(&versions_dir).await? {
            self.storage.remove_dir_all(&versions_dir).await?;
        }
        remove_dir_if_empty(&tiers_dir).await?;
        remove_dir_if_empty(&episodes_dir).await?;
        if self.storage.exists(&consolidations_dir).await? {
            self.storage.remove_dir_all(&consolidations_dir).await?;
        }
        // Remove corrections.jsonl before attempting to remove memory_dir, since
        // a non-empty directory would leave ghost agent directories on disk.
        let corrections_path = self.storage.agent_corrections_path(agent_id)?;
        if self.storage.exists(&corrections_path).await? {
            self.storage.remove_file(&corrections_path).await?;
        }
        let corrections_lock_path = self.storage.agent_corrections_lock_path(agent_id)?;
        if self.storage.exists(&corrections_lock_path).await? {
            self.storage.remove_file(&corrections_lock_path).await?;
        }
        remove_dir_if_empty(&memory_dir).await?;
        if self.storage.exists(&state_dir).await? {
            self.storage.remove_dir_all(&state_dir).await?;
        }
        remove_dir_if_empty(&agent_dir).await?;

        if removed_definition {
            self.mark_memory_index_dirty("agent_definition_deleted")
                .await;
        }

        self.invalidate_scope_cache();
        Ok(removed_definition)
    }

    /// Returns the definition with `is_primary == true`.
    /// Returns `None` if no primary agent exists.
    pub async fn get_primary_agent(
        &self,
    ) -> Result<Option<DefinitionRecord>, DefinitionStoreError> {
        let defs = self.list_definitions().await?;
        Ok(defs.into_iter().find(|d| d.definition.is_primary))
    }

    /// Atomically sets `agent_id` as primary, clearing the flag on the current primary.
    /// Returns error if the target agent is not `Personal` kind.
    pub async fn set_primary(
        &self,
        agent_id: &str,
    ) -> Result<DefinitionRecord, DefinitionStoreError> {
        self.storage.ensure_base_layout().await?;
        let _lock = acquire_store_write_lock(&self.storage).await?;

        // 1. Load target definition, verify it exists
        let target_path = self.storage.agent_definition_path(agent_id)?;
        let Some(mut target) =
            read_optional_definition(&self.storage, &target_path, agent_id).await?
        else {
            return Err(DefinitionStoreError::NotFound(agent_id.to_string()));
        };

        // 2. Verify it's Personal
        if target.kind != AgentKind::Personal {
            return Err(DefinitionStoreError::Validation(format!(
                "only Personal agents can be primary, but `{agent_id}` is {:?}",
                target.kind
            )));
        }

        // 3. If already primary, return early
        if target.is_primary {
            let (created_at, updated_at) =
                read_definition_timestamps(&self.storage, agent_id, &target_path).await?;
            return Ok(DefinitionRecord {
                definition: target,
                created_at,
                updated_at,
            });
        }

        // 4. Find and clear the current primary (if any)
        let all_defs = self.list_definitions_raw_unlocked().await?;
        for record in &all_defs {
            if record.definition.is_primary && record.definition.agent_id != agent_id {
                let mut cleared = record.definition.clone();
                cleared.is_primary = false;
                let cleared_path = self.storage.agent_definition_path(&cleared.agent_id)?;
                self.storage
                    .write_yaml_atomic(&cleared_path, &cleared)
                    .await?;
            }
        }

        // 5. Set is_primary on target and save
        target.is_primary = true;
        self.storage
            .write_yaml_atomic(&target_path, &target)
            .await?;
        self.invalidate_scope_cache();

        let (created_at, updated_at) =
            read_definition_timestamps(&self.storage, agent_id, &target_path).await?;
        Ok(DefinitionRecord {
            definition: target,
            created_at,
            updated_at,
        })
    }

    async fn read_definition_from_storage(
        &self,
        storage: &AgentStorage,
        agent_id: &str,
    ) -> Result<Option<DefinitionRecord>, DefinitionStoreError> {
        let path = storage.agent_definition_path(agent_id)?;
        let Some(mut definition) = read_optional_definition(storage, &path, agent_id).await? else {
            return Ok(None);
        };
        self.stamp_owned_scope(&mut definition);
        let (created_at, updated_at) = read_definition_timestamps(storage, agent_id, &path).await?;
        Ok(Some(DefinitionRecord {
            definition,
            created_at,
            updated_at,
        }))
    }

    async fn list_definitions_from_storage(
        &self,
        storage: &AgentStorage,
    ) -> Result<Vec<DefinitionRecord>, DefinitionStoreError> {
        let root = storage.agents_root();
        let mut out = Vec::new();
        for agent_dir in storage.list_child_dirs(root).await? {
            let Some(agent_id) = agent_dir
                .file_name()
                .and_then(|value| value.to_str())
                .map(ToOwned::to_owned)
            else {
                continue;
            };

            let path = match storage.agent_definition_path(&agent_id) {
                Ok(path) => path,
                Err(_) => continue,
            };

            let mut definition = match read_optional_definition(storage, &path, &agent_id).await {
                Ok(Some(definition)) => definition,
                Ok(None) => continue,
                Err(err) if is_skippable_list_read_error(&err) => continue,
                Err(err) => return Err(err),
            };
            self.stamp_owned_scope(&mut definition);

            let (created_at, updated_at) = match read_definition_timestamps(
                storage, &agent_id, &path,
            )
            .await
            {
                Ok(timestamps) => timestamps,
                Err(err) => {
                    warn!(
                        agent_id = %agent_id,
                        error = %err,
                        "skipping agent in list_definitions_from_storage because timestamp read failed"
                    );
                    continue;
                },
            };

            out.push(DefinitionRecord {
                definition,
                created_at,
                updated_at,
            });
        }

        out.sort_by(|a, b| a.definition.agent_id.cmp(&b.definition.agent_id));
        Ok(out)
    }

    /// Internal helper: list definitions without acquiring the write lock.
    /// Must only be called from contexts that already hold the lock.
    async fn list_definitions_raw_unlocked(
        &self,
    ) -> Result<Vec<DefinitionRecord>, DefinitionStoreError> {
        self.list_definitions_from_storage(&self.storage).await
    }
}

#[derive(Debug)]
struct StoreWriteLockGuard {
    _file: std::fs::File,
    _lock_path: PathBuf,
}

async fn acquire_store_write_lock(
    storage: &AgentStorage,
) -> Result<StoreWriteLockGuard, DefinitionStoreError> {
    acquire_store_write_lock_with_config(storage, StoreWriteLockConfig::default()).await
}

async fn acquire_store_write_lock_with_config(
    storage: &AgentStorage,
    config: StoreWriteLockConfig,
) -> Result<StoreWriteLockGuard, DefinitionStoreError> {
    let lock_path = storage.agents_root().join(STORE_WRITE_LOCK_FILE_NAME);
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
        DefinitionStoreError::Storage(AgentStorageError::from(std::io::Error::other(format!(
            "failed to join lock-open task: {join_err}"
        ))))
    })?
    .map_err(AgentStorageError::from)?;

    let started = Instant::now();
    let mut retry_delay = config.retry_delay_min;

    loop {
        let (next_file, lock_result) = task::spawn_blocking(move || {
            let lock_result = file.try_lock_exclusive();
            (file, lock_result)
        })
        .await
        .map_err(|join_err| {
            DefinitionStoreError::Storage(AgentStorageError::from(std::io::Error::other(format!(
                "failed to join lock-attempt task: {join_err}"
            ))))
        })?;

        file = next_file;
        match lock_result {
            Ok(()) => {
                return Ok(StoreWriteLockGuard {
                    _file: file,
                    _lock_path: lock_path,
                });
            },
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                if started.elapsed() >= config.max_wait {
                    return Err(DefinitionStoreError::LockTimeout {
                        lock_path: lock_path.display().to_string(),
                        wait_ms: u64::try_from(config.max_wait.as_millis()).unwrap_or(u64::MAX),
                    });
                }

                tokio::time::sleep(retry_delay).await;
                retry_delay = retry_delay.saturating_mul(2).min(config.retry_delay_max);
            },
            Err(err) => return Err(AgentStorageError::from(err).into()),
        }
    }
}

fn validate_definition(definition: &AgentDefinition) -> Result<(), DefinitionStoreError> {
    match definition.validate() {
        Ok(()) => Ok(()),
        Err(AgentDefinitionError::Validation(msg)) => Err(DefinitionStoreError::Validation(msg)),
        Err(other) => Err(DefinitionStoreError::Definition(other)),
    }
}

async fn remove_dir_if_empty(path: &Path) -> Result<(), DefinitionStoreError> {
    if !fs::try_exists(path)
        .await
        .map_err(AgentStorageError::from)?
    {
        return Ok(());
    }
    match fs::remove_dir(path).await {
        Ok(()) => Ok(()),
        Err(err)
            if err.kind() == std::io::ErrorKind::NotFound
                || err.kind() == std::io::ErrorKind::DirectoryNotEmpty =>
        {
            Ok(())
        },
        Err(err) => Err(AgentStorageError::from(err).into()),
    }
}

/// Append the `(principal, workspace)` stamps carried by `records` to `out`,
/// skipping any already present. Borrows the records; allocates only the tuples
/// it actually keeps.
///
/// `out` holds at most one entry per scope and a deployment has a handful of
/// scopes, so the linear membership scan is cheaper than hashing — and it lets
/// a repeat stamp cost zero allocations instead of building a throwaway tuple
/// just to probe a set.
fn push_definition_scope_keys(records: &[DefinitionRecord], out: &mut Vec<(String, String)>) {
    for record in records {
        let (Some(principal), Some(workspace)) = (
            record.definition.principal.as_deref(),
            record.definition.workspace.as_deref(),
        ) else {
            continue;
        };
        if out.iter().any(|(known_principal, known_workspace)| {
            known_principal == principal && known_workspace == workspace
        }) {
            continue;
        }
        out.push((principal.to_owned(), workspace.to_owned()));
    }
}

fn is_skippable_list_read_error(error: &DefinitionStoreError) -> bool {
    match error {
        DefinitionStoreError::Validation(_)
        | DefinitionStoreError::Definition(_)
        | DefinitionStoreError::AgentIdMismatch { .. } => true,
        DefinitionStoreError::Storage(AgentStorageError::Yaml(_)) => true,
        DefinitionStoreError::Storage(AgentStorageError::Io(io_err))
            if matches!(
                io_err.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            true
        },
        _ => false,
    }
}

async fn reset_definition_versions_dir(
    storage: &AgentStorage,
    agent_id: &str,
) -> Result<(), DefinitionStoreError> {
    let versions_dir = storage.agent_definition_versions_dir(agent_id)?;
    if storage.exists(&versions_dir).await? {
        storage.remove_dir_all(&versions_dir).await?;
    }
    Ok(())
}

async fn rollback_failed_create_definition(
    storage: &AgentStorage,
    agent_id: &str,
) -> Result<(), DefinitionStoreError> {
    let definition_path = storage.agent_definition_path(agent_id)?;
    if storage.exists(&definition_path).await? {
        let metadata = fs::metadata(&definition_path)
            .await
            .map_err(AgentStorageError::from)?;
        if metadata.is_dir() {
            storage.remove_dir_all(&definition_path).await?;
        } else {
            storage.remove_file(&definition_path).await?;
        }
    }

    let versions_dir = storage.agent_definition_versions_dir(agent_id)?;
    if storage.exists(&versions_dir).await? {
        let metadata = fs::metadata(&versions_dir)
            .await
            .map_err(AgentStorageError::from)?;
        if metadata.is_dir() {
            storage.remove_dir_all(&versions_dir).await?;
        } else {
            storage.remove_file(&versions_dir).await?;
        }
    }

    let agent_dir = storage.agent_dir(agent_id)?;
    remove_dir_if_empty(&agent_dir).await?;
    Ok(())
}

async fn read_definition_timestamps_after_write(
    storage: &AgentStorage,
    agent_id: &str,
    definition_path: &Path,
    fallback_created_at: Option<DateTime<Utc>>,
) -> (DateTime<Utc>, DateTime<Utc>) {
    match read_definition_timestamps(storage, agent_id, definition_path).await {
        Ok((created_at, updated_at)) => (fallback_created_at.unwrap_or(created_at), updated_at),
        Err(err) => {
            let now = Utc::now();
            warn!(
                agent_id = agent_id,
                error = %err,
                "failed to read definition timestamps after write; returning fallback timestamps"
            );
            (fallback_created_at.unwrap_or(now), now)
        },
    }
}

async fn read_updated_timestamp_after_write(
    agent_id: &str,
    definition_path: &Path,
    fallback_updated_at: Option<DateTime<Utc>>,
) -> DateTime<Utc> {
    match read_modified_timestamp(definition_path).await {
        Ok(updated_at) => updated_at,
        Err(err) => {
            let fallback = fallback_updated_at.unwrap_or_else(Utc::now);
            warn!(
                agent_id = agent_id,
                error = %err,
                "failed to read definition updated_at after write; returning fallback timestamp"
            );
            fallback
        },
    }
}

async fn read_optional_definition(
    storage: &AgentStorage,
    path: impl AsRef<Path>,
    expected_agent_id: &str,
) -> Result<Option<AgentDefinition>, DefinitionStoreError> {
    let path = path.as_ref();
    let content = match storage.read_to_string(path).await {
        Ok(content) => content,
        Err(AgentStorageError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        },
        Err(err) => return Err(err.into()),
    };

    let definition = parse_definition_on_its_own_stack(content, path.to_path_buf()).await?;

    if definition.agent_id != expected_agent_id {
        return Err(DefinitionStoreError::AgentIdMismatch {
            path_agent_id: expected_agent_id.to_string(),
            payload_agent_id: definition.agent_id,
        });
    }

    Ok(Some(definition))
}

/// Stack a definition parse needs, well above what the deepest seed
/// definition recurses to through serde's untagged buffering.
const DEFINITION_PARSE_STACK_BYTES: usize = 32 * 1024 * 1024;

/// Parse and validate a definition on a thread with its own generous stack.
///
/// Deserializing a definition recurses deeply: nested memory-tier field
/// schemas go through serde's untagged content buffering, several frames per
/// YAML level. On an execution worker that is already deep in an agent run
/// (goal trigger → orchestrator → tool resolution) that recursion has
/// overflowed the worker's 2 MiB stack and aborted the process at boot.
/// The worker stack stays as it is; the parse simply does not run on it.
async fn parse_definition_on_its_own_stack(
    content: String,
    path: PathBuf,
) -> Result<AgentDefinition, DefinitionStoreError> {
    let parse = move || -> Result<AgentDefinition, DefinitionStoreError> {
        let raw: serde_yaml::Value =
            serde_yaml::from_str(&content).map_err(AgentStorageError::from)?;
        ensure_explicit_version(&raw, &path)?;
        let definition: AgentDefinition =
            serde_yaml::from_value(raw).map_err(AgentStorageError::from)?;
        validate_definition(&definition)?;
        Ok(definition)
    };
    let joined = tokio::task::spawn_blocking(move || {
        std::thread::Builder::new()
            .name("agent-definition-parse".to_string())
            .stack_size(DEFINITION_PARSE_STACK_BYTES)
            .spawn(parse)
            .map_err(|err| {
                DefinitionStoreError::Validation(format!(
                    "could not start the definition parse thread: {err}"
                ))
            })?
            .join()
            .map_err(|_| {
                DefinitionStoreError::Validation("the definition parse thread panicked".to_string())
            })?
    })
    .await;
    match joined {
        Ok(result) => result,
        Err(err) => Err(DefinitionStoreError::Validation(format!(
            "the definition parse task was lost: {err}"
        ))),
    }
}

fn ensure_explicit_version(
    raw: &serde_yaml::Value,
    path: &Path,
) -> Result<(), DefinitionStoreError> {
    let Some(mapping) = raw.as_mapping() else {
        return Err(DefinitionStoreError::Validation(format!(
            "definition file `{}` must deserialize to a mapping",
            path.display()
        )));
    };

    let version_key = serde_yaml::Value::String("version".to_string());
    if !mapping.contains_key(&version_key) {
        return Err(DefinitionStoreError::Validation(format!(
            "definition file `{}` must include explicit `version`",
            path.display()
        )));
    }

    Ok(())
}

async fn read_definition_timestamps(
    storage: &AgentStorage,
    agent_id: &str,
    definition_path: &Path,
) -> Result<(DateTime<Utc>, DateTime<Utc>), DefinitionStoreError> {
    let mut created_at = read_modified_timestamp(definition_path).await?;
    let mut updated_at = created_at;

    let versions_dir = storage.agent_definition_versions_dir(agent_id)?;
    if storage.exists(&versions_dir).await? {
        for path in storage
            .list_files_with_extension(&versions_dir, "yaml")
            .await?
        {
            let ts = read_modified_timestamp(path).await?;
            if ts < created_at {
                created_at = ts;
            }
            if ts > updated_at {
                updated_at = ts;
            }
        }
    }

    Ok((created_at, updated_at))
}

async fn read_modified_timestamp(
    path: impl AsRef<Path>,
) -> Result<DateTime<Utc>, DefinitionStoreError> {
    let metadata = fs::metadata(path.as_ref())
        .await
        .map_err(AgentStorageError::from)?;
    let system_time = metadata
        .modified()
        .or_else(|_| metadata.created())
        .unwrap_or(SystemTime::UNIX_EPOCH);
    Ok(DateTime::<Utc>::from(system_time))
}

// ── magician-vector-index trait impl ───────────────────────────────────────
//
// `magician-vector-index::memory_index` lists agent definitions to compute
// source-hashes / iterate over tiers. This adapter projects each
// `DefinitionRecord` into the smaller `MoveableDefinitionRecord` surface
// the moved code consumes.

#[async_trait::async_trait]
impl magician_vector_index::definition_trait::DefinitionLookup for AgentDefinitionStore {
    async fn list_moveable_definitions(
        &self,
    ) -> Result<Vec<magician_vector_index::definition_trait::MoveableDefinitionRecord>, anyhow::Error>
    {
        // The memory index covers ONLY this scope's own agents — read them
        // directly from `self.storage`. We deliberately bypass `list_definitions`
        // here because it has a builtin-template materialization side-effect
        // (`ensure_builtin_templates_materialized`); templates carry no memory
        // and must not be read or materialized during an index rebuild.
        let records = self
            .list_definitions_from_storage(&self.storage)
            .await
            .map_err(anyhow::Error::from)?;
        let mut out = Vec::with_capacity(records.len());
        for record in records {
            let path = self
                .storage
                .agent_definition_path(&record.definition.agent_id)
                .map_err(anyhow::Error::from)?;
            let source_bytes = self.storage.read_bytes(&path).await.map_err(|err| {
                anyhow::Error::msg(format!(
                    "reading agent definition `{}` source for memory index at {}: {err}",
                    record.definition.agent_id,
                    path.display()
                ))
            })?;
            out.push(
                magician_vector_index::definition_trait::MoveableDefinitionRecord {
                    agent_id: record.definition.agent_id.clone(),
                    memory_tiers: record.definition.memory_tiers.clone(),
                    definition_source_hash: blake3::hash(&source_bytes).to_hex().to_string(),
                },
            );
        }
        // Phase B — inject the synthetic project-knowledge lane so its `code_knowledge` facts
        // (written by `contribute_to_project`) get embedded into the hybrid memory index for
        // SEMANTIC recall, exactly like a roster agent's lane. It is NOT a roster agent (no
        // definition file), so a fixed synthetic source hash stands in for the def-staleness key.
        //
        // Inject ONLY when the lane has persisted facts in this scope: `write_recall_fact` saves
        // the tier record only after appending a fact, so "record exists" ⟺ "lane has ≥1 fact".
        // Gating here keeps empty scopes empty (the "no definitions → no summaries" invariant) and
        // avoids embedding an empty lane.
        //
        // Facts live under the scope MEMORY root (`scope/memory`), NOT this store's definition root
        // (`self.storage` → `scope/agent_runtime`). The index rebuild loads facts from
        // `memory_service.storage()` (the memory root) keyed by agent id, and `contribute_to_project`
        // writes via that same memory root — so the existence check must read the memory root too,
        // reconstructed from the `(scope, layout)` this store already carries. The tier path is a
        // pure function of (root, ScopedV3 layout, agent_id, tier name/scope), so a fresh storage
        // rooted there reads the exact file the rebuild will.
        let pk_tier = super::project_knowledge::code_knowledge_tier_def();
        let pk_has_facts = match (self.workspace_layout.as_ref(), self.current_scope.as_ref()) {
            (Some(layout), Some((principal, workspace))) => {
                let memory_storage =
                    AgentStorage::with_scoped_memory_root(layout.memory_root(principal, workspace));
                match memory_storage
                    .load_native_tier_record::<serde_json::Value>(
                        super::project_knowledge::PROJECT_KNOWLEDGE_AGENT,
                        &pk_tier.name,
                        &pk_tier.scope,
                        None,
                    )
                    .await
                {
                    Ok(record) => record.is_some(),
                    Err(error) => {
                        warn!(
                            %error,
                            "project-knowledge lane existence check failed; skipping its memory-index injection this cycle"
                        );
                        false
                    },
                }
            },
            // Unscoped / template-only store (no scope context) → no scoped facts to index.
            _ => false,
        };
        if pk_has_facts {
            out.push(
                magician_vector_index::definition_trait::MoveableDefinitionRecord {
                    agent_id: super::project_knowledge::PROJECT_KNOWLEDGE_AGENT.to_string(),
                    memory_tiers: vec![pk_tier],
                    definition_source_hash: super::project_knowledge::PROJECT_KNOWLEDGE_SOURCE_HASH
                        .to_string(),
                },
            );
        }
        Ok(out)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn sample_definition(agent_id: &str, name: &str) -> AgentDefinition {
        let yaml = format!(
            r#"
agent_id: "{agent_id}"
name: "{name}"
persona: "Test persona"
tools: []
"#
        );
        AgentDefinition::from_yaml_str(&yaml).unwrap()
    }

    async fn write_raw_yaml(path: impl AsRef<Path>, content: &str) {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await.unwrap();
        }
        fs::write(path, content).await.unwrap();
    }

    /// The seed definition whose memory-tier schemas overflowed a worker
    /// stack at boot parses on the dedicated thread, and a parse failure
    /// comes back as an error rather than a lost task.
    #[tokio::test]
    async fn the_deepest_seed_definition_parses_off_the_worker_stack() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
            "../magician_data_v3/system/agent_templates/agents/ambassador/definition.agent.yaml",
        );
        let content = std::fs::read_to_string(&path).unwrap();
        let definition = parse_definition_on_its_own_stack(content, path.clone())
            .await
            .unwrap();
        assert_eq!(definition.agent_id, "ambassador");

        let err = parse_definition_on_its_own_stack("not: [valid".to_string(), path)
            .await
            .unwrap_err();
        assert!(matches!(err, DefinitionStoreError::Storage(_)), "{err}");
    }

    #[tokio::test]
    async fn create_definition_sets_version_1_and_timestamps() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        let definition = sample_definition("agent-a", "Alpha");

        let created = store.create_definition(definition).await.unwrap();
        assert_eq!(created.version(), 1);
        assert_eq!(created.etag(), "v1");
        assert!(created.created_at <= created.updated_at);

        let loaded = store.get_definition("agent-a").await.unwrap().unwrap();
        assert_eq!(loaded.definition.version, 1);
        assert_eq!(loaded.definition.agent_id, "agent-a");
        assert!(loaded.created_at <= loaded.updated_at);
        assert_eq!(created.created_at, loaded.created_at);
        assert_eq!(created.updated_at, loaded.updated_at);
    }

    #[tokio::test]
    async fn create_rejects_non_initial_payload_version() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        let mut definition = sample_definition("agent-a", "Alpha");
        definition.version = 2;

        let err = store.create_definition(definition).await.unwrap_err();
        match err {
            DefinitionStoreError::Validation(msg) => {
                assert!(msg.contains("create payload version must be v1"));
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_validation_failure_does_not_create_agent_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        let mut definition = sample_definition("agent-a", "Alpha");
        definition.name = "".to_string();

        let err = store.create_definition(definition).await.unwrap_err();
        match err {
            DefinitionStoreError::Validation(msg) => {
                assert!(msg.contains("name must not be empty"));
            },
            other => panic!("unexpected error: {other:?}"),
        }

        let agent_dir = store.storage().agent_dir("agent-a").unwrap();
        assert!(!agent_dir.exists());
    }

    #[tokio::test]
    async fn create_rejects_duplicate_agent_id() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        store
            .create_definition(sample_definition("agent-a", "Alpha"))
            .await
            .unwrap();

        let err = store
            .create_definition(sample_definition("agent-a", "Alpha again"))
            .await
            .unwrap_err();
        match err {
            DefinitionStoreError::AlreadyExists(agent_id) => assert_eq!(agent_id, "agent-a"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_prefers_already_exists_over_payload_validation() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        store
            .create_definition(sample_definition("agent-a", "Alpha"))
            .await
            .unwrap();

        let mut invalid_duplicate = sample_definition("agent-a", "Alpha again");
        invalid_duplicate.name = "".to_string();

        let err = store
            .create_definition(invalid_duplicate)
            .await
            .unwrap_err();
        match err {
            DefinitionStoreError::AlreadyExists(agent_id) => assert_eq!(agent_id, "agent-a"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_rejects_reserved_and_whitespace_agent_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());

        let mut reserved = sample_definition("agent-a", "Alpha");
        reserved.agent_id = "approvals".to_string();
        let err = store.create_definition(reserved).await.unwrap_err();
        match err {
            DefinitionStoreError::Validation(msg) => {
                assert!(msg.contains("reserved infrastructure names"));
            },
            other => panic!("unexpected error: {other:?}"),
        }

        let mut lock_reserved = sample_definition("agent-a", "Alpha");
        lock_reserved.agent_id = ".definition_store.write.lock".to_string();
        let err = store.create_definition(lock_reserved).await.unwrap_err();
        match err {
            DefinitionStoreError::Validation(msg) => {
                assert!(msg.contains("reserved infrastructure names"));
            },
            other => panic!("unexpected error: {other:?}"),
        }

        let mut spaced = sample_definition("agent-a", "Alpha");
        spaced.agent_id = " agent-a ".to_string();
        let err = store.create_definition(spaced).await.unwrap_err();
        match err {
            DefinitionStoreError::Validation(msg) => {
                assert!(msg.contains("trimmed"));
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_with_expected_version_archives_previous_version() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        let initial = sample_definition("agent-a", "Alpha");
        store.create_definition(initial).await.unwrap();

        let updated = sample_definition("agent-a", "Alpha v2");
        let updated = store
            .update_definition("agent-a", updated, 1)
            .await
            .unwrap();
        assert_eq!(updated.version(), 2);
        assert_eq!(updated.definition.name, "Alpha v2");
        assert_eq!(updated.etag(), "v2");

        let archived_path = store
            .storage()
            .agent_definition_version_path("agent-a", 1)
            .unwrap();
        let archived: AgentDefinition = store.storage().read_yaml(&archived_path).await.unwrap();
        assert_eq!(archived.version, 1);
        assert_eq!(archived.name, "Alpha");
    }

    #[tokio::test]
    async fn create_definition_does_not_create_empty_versions_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        store
            .create_definition(sample_definition("agent-a", "Alpha"))
            .await
            .unwrap();

        let versions_dir = store
            .storage()
            .agent_definition_versions_dir("agent-a")
            .unwrap();
        assert!(!versions_dir.exists());
    }

    #[tokio::test]
    async fn update_preserves_original_created_at_timestamp() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        let created = store
            .create_definition(sample_definition("agent-a", "Alpha"))
            .await
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let updated = store
            .update_definition("agent-a", sample_definition("agent-a", "Alpha v2"), 1)
            .await
            .unwrap();
        assert_eq!(updated.created_at, created.created_at);
        assert!(updated.updated_at >= created.updated_at);
    }

    #[tokio::test]
    async fn update_with_stale_version_fails_conflict_with_precise_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        store
            .create_definition(sample_definition("agent-a", "Alpha"))
            .await
            .unwrap();
        store
            .update_definition("agent-a", sample_definition("agent-a", "Alpha v2"), 1)
            .await
            .unwrap();

        let err = store
            .update_definition("agent-a", sample_definition("agent-a", "Alpha v3"), 1)
            .await
            .unwrap_err();
        match err {
            DefinitionStoreError::VersionConflict {
                agent_id,
                expected_version,
                current_version,
            } => {
                assert_eq!(agent_id, "agent-a");
                assert_eq!(expected_version, 1);
                assert_eq!(current_version, 2);
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_rejects_payload_version_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        store
            .create_definition(sample_definition("agent-a", "Alpha"))
            .await
            .unwrap();
        store
            .update_definition("agent-a", sample_definition("agent-a", "Alpha v2"), 1)
            .await
            .unwrap();

        let err = store
            .update_definition("agent-a", sample_definition("agent-a", "Alpha v3"), 2)
            .await
            .unwrap_err();
        match err {
            DefinitionStoreError::Validation(msg) => {
                assert!(msg.contains("update payload version must match If-Match version v2"));
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn rename_preserves_agent_id_continuity_and_has_no_mismatch_side_effects() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        store
            .create_definition(sample_definition("agent-a", "Alpha"))
            .await
            .unwrap();

        let renamed = store
            .update_definition("agent-a", sample_definition("agent-a", "Renamed"), 1)
            .await
            .unwrap();
        assert_eq!(renamed.definition.agent_id, "agent-a");
        assert_eq!(renamed.definition.name, "Renamed");

        let mismatch = store
            .update_definition("ghost-agent", sample_definition("agent-b", "Wrong id"), 2)
            .await
            .unwrap_err();
        match mismatch {
            DefinitionStoreError::AgentIdMismatch {
                path_agent_id,
                payload_agent_id,
            } => {
                assert_eq!(path_agent_id, "ghost-agent");
                assert_eq!(payload_agent_id, "agent-b");
            },
            other => panic!("unexpected error: {other:?}"),
        }

        let ghost_dir = store.storage().agent_dir("ghost-agent").unwrap();
        assert!(!ghost_dir.exists());
    }

    #[tokio::test]
    async fn update_nonexistent_returns_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());

        let err = store
            .update_definition("agent-a", sample_definition("agent-a", "Alpha"), 1)
            .await
            .unwrap_err();
        match err {
            DefinitionStoreError::NotFound(agent_id) => assert_eq!(agent_id, "agent-a"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_version_overflow_fails_validation() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        let path = store.storage().agent_definition_path("agent-a").unwrap();
        write_raw_yaml(
            &path,
            r#"
agent_id: "agent-a"
version: 4294967295
name: "Alpha"
persona: "Test persona"
tools: []
"#,
        )
        .await;

        let mut payload = sample_definition("agent-a", "Alpha v2");
        payload.version = u32::MAX;
        let err = store
            .update_definition("agent-a", payload, u32::MAX)
            .await
            .unwrap_err();
        match err {
            DefinitionStoreError::Validation(msg) => assert!(msg.contains("version overflow")),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_validation_failure_does_not_write_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        store
            .create_definition(sample_definition("agent-a", "Alpha"))
            .await
            .unwrap();

        let mut invalid = sample_definition("agent-a", "Alpha v2");
        invalid.persona = "".to_string();
        let err = store
            .update_definition("agent-a", invalid, 1)
            .await
            .unwrap_err();
        match err {
            DefinitionStoreError::Validation(msg) => {
                assert!(msg.contains("persona must not be empty"));
            },
            other => panic!("unexpected error: {other:?}"),
        }

        let archive_path = store
            .storage()
            .agent_definition_version_path("agent-a", 1)
            .unwrap();
        assert!(!archive_path.exists());
    }

    #[tokio::test]
    async fn get_definition_returns_none_for_missing_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        assert!(store.get_definition("missing").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn list_empty_store_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        let listed = store.list_definitions().await.unwrap();
        assert!(listed.is_empty());
    }

    #[tokio::test]
    async fn list_definitions_returns_sorted_records() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        store
            .create_definition(sample_definition("agent-z", "Zulu"))
            .await
            .unwrap();
        store
            .create_definition(sample_definition("agent-a", "Alpha"))
            .await
            .unwrap();

        let listed = store.list_definitions().await.unwrap();
        let ids = listed
            .into_iter()
            .map(|record| record.definition.agent_id)
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["agent-a".to_string(), "agent-z".to_string()]);
    }

    #[tokio::test]
    async fn list_definitions_skips_corrupted_files() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        store
            .create_definition(sample_definition("agent-a", "Alpha"))
            .await
            .unwrap();
        store
            .create_definition(sample_definition("agent-b", "Bravo"))
            .await
            .unwrap();

        let broken_path = store.storage().agent_definition_path("agent-b").unwrap();
        write_raw_yaml(&broken_path, "not: [valid").await;

        let listed = store.list_definitions().await.unwrap();
        let ids = listed
            .into_iter()
            .map(|record| record.definition.agent_id)
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["agent-a".to_string()]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn list_definitions_skips_permission_denied_definition_files() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        store
            .create_definition(sample_definition("agent-a", "Alpha"))
            .await
            .unwrap();
        store
            .create_definition(sample_definition("agent-b", "Bravo"))
            .await
            .unwrap();

        let blocked_path = store.storage().agent_definition_path("agent-b").unwrap();
        let original_mode = fs::metadata(&blocked_path)
            .await
            .unwrap()
            .permissions()
            .mode();
        let mut blocked_permissions = fs::metadata(&blocked_path).await.unwrap().permissions();
        blocked_permissions.set_mode(0o000);
        fs::set_permissions(&blocked_path, blocked_permissions)
            .await
            .unwrap();

        let listed = store.list_definitions().await.unwrap();

        let mut restore_permissions = fs::metadata(&blocked_path).await.unwrap().permissions();
        restore_permissions.set_mode(original_mode);
        fs::set_permissions(&blocked_path, restore_permissions)
            .await
            .unwrap();

        let ids = listed
            .into_iter()
            .map(|record| record.definition.agent_id)
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["agent-a".to_string()]);
    }

    #[tokio::test]
    async fn list_definitions_skips_timestamp_read_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        store
            .create_definition(sample_definition("agent-a", "Alpha"))
            .await
            .unwrap();
        store
            .update_definition("agent-a", sample_definition("agent-a", "Alpha v2"), 1)
            .await
            .unwrap();
        store
            .create_definition(sample_definition("agent-b", "Bravo"))
            .await
            .unwrap();

        let versions_dir = store
            .storage()
            .agent_definition_versions_dir("agent-a")
            .unwrap();
        fs::remove_dir_all(&versions_dir).await.unwrap();
        fs::write(&versions_dir, "not a directory").await.unwrap();

        let listed = store.list_definitions().await.unwrap();
        let ids = listed
            .into_iter()
            .map(|record| record.definition.agent_id)
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["agent-b".to_string()]);
    }

    #[tokio::test]
    async fn get_definition_surfaces_corrupted_yaml_error() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        let broken_path = store.storage().agent_definition_path("agent-a").unwrap();
        write_raw_yaml(&broken_path, "not: [valid").await;

        let err = store.get_definition("agent-a").await.unwrap_err();
        match err {
            DefinitionStoreError::Storage(AgentStorageError::Yaml(_)) => {},
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_definition_requires_explicit_version_field() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        let path = store.storage().agent_definition_path("agent-a").unwrap();
        write_raw_yaml(
            &path,
            r#"
agent_id: "agent-a"
name: "Alpha"
persona: "Test persona"
tools: []
"#,
        )
        .await;

        let err = store.get_definition("agent-a").await.unwrap_err();
        match err {
            DefinitionStoreError::Validation(msg) => {
                assert!(msg.contains("must include explicit `version`"));
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn ensure_explicit_version_rejects_non_mapping_payload() {
        let raw = serde_yaml::from_str::<serde_yaml::Value>("- just\n- a\n- list").unwrap();
        let err = ensure_explicit_version(&raw, Path::new("definition.agent.yaml")).unwrap_err();
        match err {
            DefinitionStoreError::Validation(msg) => {
                assert!(msg.contains("must deserialize to a mapping"));
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn is_skippable_list_read_error_covers_expected_branches() {
        assert!(is_skippable_list_read_error(
            &DefinitionStoreError::Validation("invalid".to_string())
        ));
        assert!(is_skippable_list_read_error(
            &DefinitionStoreError::Definition(AgentDefinitionError::Validation(
                "bad definition".to_string()
            ))
        ));
        assert!(is_skippable_list_read_error(
            &DefinitionStoreError::AgentIdMismatch {
                path_agent_id: "a".to_string(),
                payload_agent_id: "b".to_string(),
            }
        ));

        let yaml_err = serde_yaml::from_str::<serde_yaml::Value>("not: [valid").unwrap_err();
        assert!(is_skippable_list_read_error(
            &DefinitionStoreError::Storage(AgentStorageError::Yaml(yaml_err))
        ));
        assert!(is_skippable_list_read_error(
            &DefinitionStoreError::Storage(AgentStorageError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "missing"
            )))
        ));
        assert!(is_skippable_list_read_error(
            &DefinitionStoreError::Storage(AgentStorageError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "denied"
            )))
        ));

        assert!(!is_skippable_list_read_error(
            &DefinitionStoreError::AlreadyExists("agent-a".to_string())
        ));
        assert!(!is_skippable_list_read_error(
            &DefinitionStoreError::Storage(AgentStorageError::Io(std::io::Error::other("other")))
        ));
    }

    #[tokio::test]
    async fn read_definition_timestamps_after_write_uses_fallback_on_read_error() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = AgentStorage::new(tmp.path());
        let missing = tmp.path().join("missing").join("definition.agent.yaml");
        let fallback = Utc::now();
        let before = Utc::now();
        let (created_at, updated_at) =
            read_definition_timestamps_after_write(&storage, "agent-a", &missing, Some(fallback))
                .await;
        let after = Utc::now();

        assert_eq!(created_at, fallback);
        assert!(updated_at >= before);
        assert!(updated_at <= after);
    }

    #[tokio::test]
    async fn get_definition_rejects_identity_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        let path = store.storage().agent_definition_path("agent-a").unwrap();
        write_raw_yaml(
            &path,
            r#"
agent_id: "agent-b"
version: 1
name: "Alpha"
persona: "Test persona"
tools: []
"#,
        )
        .await;

        let err = store.get_definition("agent-a").await.unwrap_err();
        match err {
            DefinitionStoreError::AgentIdMismatch {
                path_agent_id,
                payload_agent_id,
            } => {
                assert_eq!(path_agent_id, "agent-a");
                assert_eq!(payload_agent_id, "agent-b");
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn delete_definition_removes_only_definition_files() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        store
            .create_definition(sample_definition("agent-a", "Alpha"))
            .await
            .unwrap();

        let memory_path = store
            .storage()
            .agent_tiers_dir("agent-a")
            .unwrap()
            .join("persist.json");
        write_raw_yaml(&memory_path, "{\"ok\": true}").await;
        assert!(memory_path.exists());

        assert!(store.delete_definition("agent-a").await.unwrap());
        assert!(store.get_definition("agent-a").await.unwrap().is_none());
        assert!(memory_path.exists());
        assert!(!store.delete_definition("agent-a").await.unwrap());
    }

    #[tokio::test]
    async fn delete_definition_removes_empty_agent_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        store
            .create_definition(sample_definition("agent-a", "Alpha"))
            .await
            .unwrap();

        let agent_dir = store.storage().agent_dir("agent-a").unwrap();
        assert!(agent_dir.exists());

        assert!(store.delete_definition("agent-a").await.unwrap());
        assert!(!agent_dir.exists());
    }

    #[tokio::test]
    async fn delete_definition_removes_corrections_lock_file() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        store
            .create_definition(sample_definition("agent-a", "Alpha"))
            .await
            .unwrap();

        let lock_path = store
            .storage()
            .agent_corrections_lock_path("agent-a")
            .unwrap();
        write_raw_yaml(&lock_path, "").await;
        assert!(lock_path.exists());

        assert!(store.delete_definition("agent-a").await.unwrap());
        assert!(!lock_path.exists());
        assert!(!store.storage().agent_dir("agent-a").unwrap().exists());
    }

    #[tokio::test]
    async fn delete_definition_removes_state_files() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        store
            .create_definition(sample_definition("agent-a", "Alpha"))
            .await
            .unwrap();

        let consolidations_dir = store.storage().agent_consolidations_dir("agent-a").unwrap();
        let run_state_file = consolidations_dir.join("memory_consolidation_runs.json");
        write_raw_yaml(
            &run_state_file,
            "{\"rules\": {\"rule-a\": \"2026-02-22T00:00:00Z\"}}",
        )
        .await;
        assert!(run_state_file.exists());

        assert!(store.delete_definition("agent-a").await.unwrap());
        assert!(!consolidations_dir.exists());
        assert!(!store.storage().agent_dir("agent-a").unwrap().exists());
    }

    #[tokio::test]
    async fn delete_definition_cleans_orphaned_versions_when_current_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        store
            .create_definition(sample_definition("agent-a", "Alpha"))
            .await
            .unwrap();
        store
            .update_definition("agent-a", sample_definition("agent-a", "Alpha v2"), 1)
            .await
            .unwrap();

        let current_path = store.storage().agent_definition_path("agent-a").unwrap();
        fs::remove_file(&current_path).await.unwrap();
        let versions_dir = store
            .storage()
            .agent_definition_versions_dir("agent-a")
            .unwrap();
        assert!(versions_dir.exists());

        assert!(!store.delete_definition("agent-a").await.unwrap());
        assert!(!versions_dir.exists());
    }

    #[tokio::test]
    async fn concurrent_create_from_two_store_instances_has_single_winner() {
        let tmp = tempfile::tempdir().unwrap();
        let store_a = AgentDefinitionStore::with_base_path(tmp.path());
        let store_b = AgentDefinitionStore::with_base_path(tmp.path());
        let d1 = sample_definition("agent-a", "Alpha");
        let d2 = sample_definition("agent-a", "Alpha-2");

        let (r1, r2) = tokio::join!(store_a.create_definition(d1), store_b.create_definition(d2));

        let success_count = usize::from(r1.is_ok()) + usize::from(r2.is_ok());
        let exists_count = usize::from(matches!(r1, Err(DefinitionStoreError::AlreadyExists(_))))
            + usize::from(matches!(r2, Err(DefinitionStoreError::AlreadyExists(_))));
        assert_eq!(success_count, 1);
        assert_eq!(exists_count, 1);
    }

    #[tokio::test]
    async fn update_reuses_existing_archive_instead_of_overwriting() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        store
            .create_definition(sample_definition("agent-a", "Alpha"))
            .await
            .unwrap();

        // Read the actual stored v1 definition and write it as the archive so that
        // the archive content matches what `update_definition` expects.
        let current_path = store.storage().agent_definition_path("agent-a").unwrap();
        let current_yaml = fs::read_to_string(&current_path).await.unwrap();

        let archived_path = store
            .storage()
            .agent_definition_version_path("agent-a", 1)
            .unwrap();
        write_raw_yaml(&archived_path, &current_yaml).await;
        let archive_bytes_before = fs::read(&archived_path).await.unwrap();

        let updated = store
            .update_definition("agent-a", sample_definition("agent-a", "Alpha v2"), 1)
            .await
            .unwrap();
        assert_eq!(updated.version(), 2);

        let archive_bytes_after = fs::read(&archived_path).await.unwrap();
        assert_eq!(archive_bytes_after, archive_bytes_before);
    }

    #[tokio::test]
    async fn update_rejects_existing_archive_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        store
            .create_definition(sample_definition("agent-a", "Alpha"))
            .await
            .unwrap();

        let mismatched_archive = store
            .storage()
            .agent_definition_version_path("agent-a", 1)
            .unwrap();
        write_raw_yaml(
            &mismatched_archive,
            r#"
agent_id: "agent-a"
version: 1
name: "Different"
persona: "Test persona"
tools: []
"#,
        )
        .await;

        let err = store
            .update_definition("agent-a", sample_definition("agent-a", "Alpha v2"), 1)
            .await
            .unwrap_err();
        match err {
            DefinitionStoreError::Validation(msg) => {
                assert!(msg.contains("archive mismatch"));
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recreate_definition_clears_stale_archives_before_first_update() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        store
            .create_definition(sample_definition("agent-a", "Alpha"))
            .await
            .unwrap();
        store
            .update_definition("agent-a", sample_definition("agent-a", "Alpha v2"), 1)
            .await
            .unwrap();

        let current_path = store.storage().agent_definition_path("agent-a").unwrap();
        fs::remove_file(&current_path).await.unwrap();

        store
            .create_definition(sample_definition("agent-a", "Recreated"))
            .await
            .unwrap();
        let updated = store
            .update_definition("agent-a", sample_definition("agent-a", "Recreated v2"), 1)
            .await
            .unwrap();
        assert_eq!(updated.version(), 2);
    }

    #[tokio::test]
    async fn create_definition_rolls_back_when_versions_reset_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        store.storage().ensure_base_layout().await.unwrap();

        let agent_dir = store.storage().agent_dir("agent-a").unwrap();
        fs::create_dir_all(&agent_dir).await.unwrap();
        let versions_path = store
            .storage()
            .agent_definition_versions_dir("agent-a")
            .unwrap();
        fs::write(&versions_path, "not-a-directory").await.unwrap();

        let err = store
            .create_definition(sample_definition("agent-a", "Alpha"))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            DefinitionStoreError::Storage(AgentStorageError::Io(_))
        ));

        let definition_path = store.storage().agent_definition_path("agent-a").unwrap();
        assert!(!definition_path.exists());
        assert!(!versions_path.exists());
        assert!(!agent_dir.exists());
    }

    #[tokio::test]
    async fn acquire_lock_releases_on_drop_and_keeps_lockfile() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = AgentStorage::new(tmp.path());
        storage.ensure_base_layout().await.unwrap();
        let lock_path = storage.agents_root().join(STORE_WRITE_LOCK_FILE_NAME);

        let first_guard = acquire_store_write_lock_with_config(
            &storage,
            StoreWriteLockConfig {
                retry_delay_min: Duration::from_millis(1),
                retry_delay_max: Duration::from_millis(2),
                max_wait: Duration::from_millis(50),
            },
        )
        .await
        .unwrap();
        assert!(lock_path.exists());

        let timeout = acquire_store_write_lock_with_config(
            &storage,
            StoreWriteLockConfig {
                retry_delay_min: Duration::from_millis(1),
                retry_delay_max: Duration::from_millis(2),
                max_wait: Duration::from_millis(10),
            },
        )
        .await;
        assert!(matches!(
            timeout,
            Err(DefinitionStoreError::LockTimeout { .. })
        ));

        drop(first_guard);

        let second_guard = acquire_store_write_lock_with_config(
            &storage,
            StoreWriteLockConfig {
                retry_delay_min: Duration::from_millis(1),
                retry_delay_max: Duration::from_millis(2),
                max_wait: Duration::from_millis(50),
            },
        )
        .await
        .unwrap();
        drop(second_guard);

        assert!(lock_path.exists());
    }

    #[tokio::test]
    async fn acquire_lock_returns_lock_timeout_error() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = AgentStorage::new(tmp.path());
        storage.ensure_base_layout().await.unwrap();
        let guard = acquire_store_write_lock_with_config(
            &storage,
            StoreWriteLockConfig {
                retry_delay_min: Duration::from_millis(1),
                retry_delay_max: Duration::from_millis(2),
                max_wait: Duration::from_millis(50),
            },
        )
        .await
        .unwrap();

        let err = acquire_store_write_lock_with_config(
            &storage,
            StoreWriteLockConfig {
                retry_delay_min: Duration::from_millis(1),
                retry_delay_max: Duration::from_millis(2),
                max_wait: Duration::from_millis(10),
            },
        )
        .await
        .unwrap_err();
        drop(guard);

        match err {
            DefinitionStoreError::LockTimeout {
                lock_path: path,
                wait_ms,
            } => {
                assert!(path.ends_with(STORE_WRITE_LOCK_FILE_NAME));
                assert_eq!(wait_ms, 10);
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn complex_definition_roundtrip_preserves_optional_surfaces() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());
        let definition = AgentDefinition::from_yaml_str(
            r#"
agent_id: "agent-complex"
name: "Complex Agent"
persona: "Planner"
tools: ["browser", "files"]
constraints:
  max_iterations: 10
  max_tokens_per_cycle: 2000
  max_consecutive_failures: 2
  approval_ttl_secs: 300
  coordination:
    delegation_timeout_secs: 120
memory_tiers:
  - name: "task_progress"
    scope: "agent"
    description: "Track progress"
    schema:
      summary: { type: text }
    render:
      format: "compact_summary"
      template: "{summary}"
    retention: "goal_lifetime"
memory_consolidation:
  - name: "consolidate_progress"
    trigger: "cycle_completed"
    source: "episodes(g1, limit=5)"
    target: "task_progress"
    transform:
      type: "structured"
      builtin: "map_episode_to_task"
prompt_pipeline:
  sections:
    - name: "context"
      source: "tiers(task_progress)"
  output_rules:
    max_context_tokens: 1500
    truncation_priority: ["context"]
circuit_breaker:
  thresholds:
    - failures: 1
      action: "inject_failure_context"
  recovery:
    trigger: "user_reset"
feedback_loops:
  - name: "failure_adaptation"
    trigger: "episode.outcome.is_failed"
    extract:
      source: "episodes(g1, limit=5)"
      fields: ["error"]
    transform: "failure_context"
    inject_into: "prompt_pipeline.context"
notification_rules:
  - match: "episode.outcome.is_failed"
    severity: "high"
    channels: ["webhook"]
retention:
  episodes:
    default_days: 30
  corrections:
    resolved_days: 7
  definition_versions:
    keep_last: 5
llm_routing:
  planning:
    provider: "openai"
    model: "gpt-4o-mini"
strategy: "auto_select"
state_machines:
  workflow:
    state: "idle"
"#,
        )
        .unwrap();

        store.create_definition(definition.clone()).await.unwrap();
        let loaded = store
            .get_definition("agent-complex")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.definition, definition);
        assert!(loaded.definition.prompt_pipeline.is_some());
        assert!(loaded.definition.circuit_breaker.is_some());
        assert!(loaded.definition.retention.is_some());
    }

    fn sample_personal_definition(agent_id: &str, name: &str, is_primary: bool) -> AgentDefinition {
        let yaml = format!(
            r#"
agent_id: "{agent_id}"
name: "{name}"
persona: "Test persona"
kind: personal
is_primary: {is_primary}
tools: []
"#
        );
        AgentDefinition::from_yaml_str(&yaml).unwrap()
    }

    fn sample_worker_definition(agent_id: &str, name: &str) -> AgentDefinition {
        let yaml = format!(
            r#"
agent_id: "{agent_id}"
name: "{name}"
persona: "Test worker persona"
kind: worker
tools: []
"#
        );
        AgentDefinition::from_yaml_str(&yaml).unwrap()
    }

    #[tokio::test]
    async fn test_get_primary_agent_returns_primary() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());

        store
            .create_definition(sample_personal_definition("agent-a", "Alpha", false))
            .await
            .unwrap();
        store
            .create_definition(sample_personal_definition("agent-b", "Bravo", true))
            .await
            .unwrap();

        let primary = store.get_primary_agent().await.unwrap();
        assert!(primary.is_some());
        let primary = primary.unwrap();
        assert_eq!(primary.definition.agent_id, "agent-b");
        assert!(primary.definition.is_primary);
    }

    #[tokio::test]
    async fn test_get_primary_agent_returns_none_when_no_primary() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());

        store
            .create_definition(sample_personal_definition("agent-a", "Alpha", false))
            .await
            .unwrap();

        let primary = store.get_primary_agent().await.unwrap();
        assert!(primary.is_none());
    }

    #[tokio::test]
    async fn test_set_primary_transfers_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());

        store
            .create_definition(sample_personal_definition("agent-a", "Alpha", true))
            .await
            .unwrap();
        store
            .create_definition(sample_personal_definition("agent-b", "Bravo", false))
            .await
            .unwrap();

        // Transfer primary from A to B
        let result = store.set_primary("agent-b").await.unwrap();
        assert_eq!(result.definition.agent_id, "agent-b");
        assert!(result.definition.is_primary);

        // Verify A is no longer primary
        let agent_a = store.get_definition("agent-a").await.unwrap().unwrap();
        assert!(!agent_a.definition.is_primary);

        // Verify B is primary
        let agent_b = store.get_definition("agent-b").await.unwrap().unwrap();
        assert!(agent_b.definition.is_primary);

        // get_primary_agent should return B
        let primary = store.get_primary_agent().await.unwrap().unwrap();
        assert_eq!(primary.definition.agent_id, "agent-b");
    }

    #[tokio::test]
    async fn test_set_primary_rejects_worker() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());

        store
            .create_definition(sample_worker_definition("worker-a", "Worker A"))
            .await
            .unwrap();

        let err = store.set_primary("worker-a").await.unwrap_err();
        match err {
            DefinitionStoreError::Validation(msg) => {
                assert!(msg.contains("only Personal agents can be primary"));
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_set_primary_noop_when_already_primary() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());

        store
            .create_definition(sample_personal_definition("agent-a", "Alpha", true))
            .await
            .unwrap();

        // Setting primary on the already-primary agent should succeed
        let result = store.set_primary("agent-a").await.unwrap();
        assert_eq!(result.definition.agent_id, "agent-a");
        assert!(result.definition.is_primary);
    }

    #[tokio::test]
    async fn test_set_primary_rejects_nonexistent() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_base_path(tmp.path());

        let err = store.set_primary("ghost").await.unwrap_err();
        match err {
            DefinitionStoreError::NotFound(agent_id) => assert_eq!(agent_id, "ghost"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn scoped_store_isolates_mutable_agents_without_builtin_system_templates() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_workspace_root(tmp.path());
        let scope_a = store.for_scope("principal-a", "workspace-a");
        let scope_b = store.for_scope("principal-b", "workspace-b");

        scope_a
            .create_definition(sample_personal_definition("agent-a", "Alpha", true))
            .await
            .unwrap();

        let scoped_a = scope_a
            .get_definition("agent-a")
            .await
            .unwrap()
            .expect("scope a definition should exist");
        assert_eq!(
            scoped_a.definition.principal.as_deref(),
            Some("principal-a")
        );
        assert_eq!(
            scoped_a.definition.workspace.as_deref(),
            Some("workspace-a")
        );

        assert!(
            scope_b.get_definition("agent-a").await.unwrap().is_none(),
            "mutable agents must not leak across scopes"
        );

        let all_defs = store
            .list_all_definitions_across_scopes()
            .await
            .expect("listing across scopes should succeed");
        assert!(all_defs.iter().any(|record| {
            record.definition.agent_id == "agent-a"
                && record.definition.principal.as_deref() == Some("principal-a")
                && record.definition.workspace.as_deref() == Some("workspace-a")
        }));
    }

    #[tokio::test]
    async fn scoped_store_stamps_scope_on_legacy_scoped_definition_reads() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_workspace_root(tmp.path());
        let scope_a = store.for_scope("principal-a", "workspace-a");
        let definition_path = scope_a.storage().agent_definition_path("agent-a").unwrap();

        write_raw_yaml(
            &definition_path,
            r#"
agent_id: "agent-a"
version: 1
name: "Alpha"
persona: "Test persona"
kind: worker
tools: []
"#,
        )
        .await;

        let loaded = scope_a
            .get_definition("agent-a")
            .await
            .unwrap()
            .expect("legacy scoped definition should load");
        assert_eq!(loaded.definition.principal.as_deref(), Some("principal-a"));
        assert_eq!(loaded.definition.workspace.as_deref(), Some("workspace-a"));

        let listed = scope_a.list_definitions().await.unwrap();
        let listed_agent = listed
            .iter()
            .find(|record| record.definition.agent_id == "agent-a")
            .expect("legacy scoped definition should appear in scoped listings");
        assert_eq!(
            listed_agent.definition.principal.as_deref(),
            Some("principal-a")
        );
        assert_eq!(
            listed_agent.definition.workspace.as_deref(),
            Some("workspace-a")
        );
    }

    #[tokio::test]
    async fn scoped_store_allows_duplicate_agent_ids_across_scopes() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_workspace_root(tmp.path());
        let scope_a = store.for_scope("principal-a", "workspace-a");
        let scope_b = store.for_scope("principal-b", "workspace-b");

        scope_a
            .create_definition(sample_personal_definition("agent-a", "Alpha", true))
            .await
            .unwrap();

        let created = scope_b
            .create_definition(sample_personal_definition("agent-a", "Bravo", true))
            .await
            .expect("duplicate agent ids should be isolated by scope");
        assert_eq!(created.definition.agent_id, "agent-a");
        assert_eq!(created.definition.principal.as_deref(), Some("principal-b"));
        assert_eq!(created.definition.workspace.as_deref(), Some("workspace-b"));
    }

    /// Regression guard: a write in a scope must invalidate that scope's cached
    /// definition list, so the very next `list_definitions()` reflects the change
    /// (rather than serving a stale cached list to the scheduler tick).
    #[tokio::test]
    async fn write_invalidates_scope_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_workspace_root(tmp.path());
        let scope = store.for_scope("principal-a", "workspace-a");
        let scope_key = ("principal-a".to_string(), "workspace-a".to_string());

        // Warm the cache with an initial read.
        let before = scope.list_definitions().await.unwrap();
        assert!(
            before
                .iter()
                .all(|record| record.definition.agent_id != "agent-a"),
            "agent-a should not exist before it is created"
        );
        assert!(
            scope.cache.lists.read().unwrap().contains_key(&scope_key),
            "list_definitions() should have populated the scope cache"
        );

        // A write must drop this scope's cached list.
        scope
            .create_definition(sample_personal_definition("agent-a", "Alpha", true))
            .await
            .unwrap();
        assert!(
            !scope.cache.lists.read().unwrap().contains_key(&scope_key),
            "create_definition must invalidate the scope's cached list"
        );

        // The next read reflects the write (and re-warms the cache).
        let after = scope.list_definitions().await.unwrap();
        assert!(
            after
                .iter()
                .any(|record| record.definition.agent_id == "agent-a"),
            "list_definitions() after a write must reflect the newly created definition"
        );
        assert!(
            scope.cache.lists.read().unwrap().contains_key(&scope_key),
            "the post-write read should have re-warmed the scope cache"
        );
    }

    /// Regression guard: builtin-template materialization is boot-once per scope.
    /// After the first `list_definitions()` pass materializes a scope, the shared
    /// `materialized` set records it so the per-tick scheduler read early-returns
    /// instead of re-materializing every template from disk on each tick.
    #[tokio::test]
    async fn materialization_is_boot_once() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_workspace_root(tmp.path());
        let scope = store.for_scope("principal-a", "workspace-a");
        let scope_key = ("principal-a".to_string(), "workspace-a".to_string());

        // Not yet materialized.
        assert!(
            !scope
                .cache
                .materialized
                .read()
                .unwrap()
                .contains(&scope_key),
            "scope must not be marked materialized before any read"
        );

        // First pass runs materialization and records the scope as boot-once done.
        scope.ensure_builtin_templates_materialized().await.unwrap();
        assert!(
            scope
                .cache
                .materialized
                .read()
                .unwrap()
                .contains(&scope_key),
            "first materialization pass must mark the scope as materialized"
        );
        assert_eq!(
            scope.cache.materialized.read().unwrap().len(),
            1,
            "exactly one scope should be recorded after the first pass"
        );

        // Second pass must early-return (no re-materialization): the scope stays
        // recorded exactly once, proving the boot-once guard was hit.
        scope.ensure_builtin_templates_materialized().await.unwrap();
        assert!(
            scope
                .cache
                .materialized
                .read()
                .unwrap()
                .contains(&scope_key),
            "scope must remain marked materialized after the second pass"
        );
        assert_eq!(
            scope.cache.materialized.read().unwrap().len(),
            1,
            "the boot-once guard must not re-insert or duplicate the scope entry"
        );

        // The cache is shared across for_scope clones: a fresh clone of the same
        // scope also sees it as already materialized, so its tick read is a no-op.
        let scope_clone = store.for_scope("principal-a", "workspace-a");
        assert!(
            scope_clone
                .cache
                .materialized
                .read()
                .unwrap()
                .contains(&scope_key),
            "for_scope clones share the cache, so the scope stays boot-once materialized"
        );
    }

    #[tokio::test]
    async fn scope_keys_borrow_the_cached_definitions_instead_of_cloning_them() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_workspace_root(tmp.path());
        let scope_a = store.for_scope("principal-a", "workspace-a");
        let scope_b = store.for_scope("principal-b", "workspace-b");

        scope_a
            .create_definition(sample_personal_definition("agent-a", "Alpha", true))
            .await
            .unwrap();
        scope_b
            .create_definition(sample_personal_definition("agent-b", "Beta", true))
            .await
            .unwrap();

        // Warm every scope's cache the way a running process would.
        let _ = store.list_all_definitions_across_scopes().await.unwrap();

        // Pointer identity is the whole point: the scope-key path hands back the
        // very allocation the cache holds, so no `DefinitionRecord` was copied.
        let cached = scope_a
            .cached_definitions_for_tests()
            .expect("scope a should be cached after the warm read");
        let borrowed = scope_a.list_definitions_shared().await.unwrap();
        assert!(
            Arc::ptr_eq(&cached, &borrowed),
            "list_definitions_shared must return the cached Arc, not a copy of it"
        );
        assert!(
            !borrowed.is_empty(),
            "the borrowed list must actually carry the scope's definitions"
        );

        // ...and it derives exactly the tuples the scheduler tick used to build by
        // cloning every record and throwing all but two fields away.
        let mut expected = store
            .list_all_definitions_across_scopes()
            .await
            .unwrap()
            .into_iter()
            .filter_map(|record| Some((record.definition.principal?, record.definition.workspace?)))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        expected.sort();

        let keys = store.list_definition_scope_keys().await.unwrap();
        assert_eq!(keys, expected);
        assert!(keys.contains(&("principal-a".to_string(), "workspace-a".to_string())));
        assert!(keys.contains(&("principal-b".to_string(), "workspace-b".to_string())));
    }

    #[tokio::test]
    async fn scope_keys_deduplicate_and_sort() {
        let tmp = tempfile::tempdir().unwrap();
        let store = AgentDefinitionStore::with_workspace_root(tmp.path());
        let scope = store.for_scope("principal-z", "workspace-z");

        // Two definitions in one scope must collapse to one key.
        scope
            .create_definition(sample_personal_definition("agent-one", "One", true))
            .await
            .unwrap();
        scope
            .create_definition(sample_personal_definition("agent-two", "Two", false))
            .await
            .unwrap();
        store
            .for_scope("principal-a", "workspace-a")
            .create_definition(sample_personal_definition("agent-a", "Alpha", true))
            .await
            .unwrap();

        let keys = store.list_definition_scope_keys().await.unwrap();
        let z_count = keys
            .iter()
            .filter(|(principal, _)| principal == "principal-z")
            .count();
        assert_eq!(z_count, 1, "a scope contributes exactly one key");

        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "the round-robin cursor needs a stable order");
    }
}
