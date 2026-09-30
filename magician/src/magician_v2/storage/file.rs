//! File-based execution storage over the scoped V3 workspace layout.

use std::{
    collections::HashSet,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use dashmap::DashMap;
use fs2::FileExt;
use tokio::{
    fs,
    sync::{Mutex, OwnedMutexGuard},
};
use uuid::Uuid;

use super::{
    models::{
        ClarificationHistoryEntry, CreateExecutionParams, ExecutionEntryMode, ExecutionIndex,
        ExecutionRun, ExecutionRunDocument, ExecutionSummary, PaginatedResult, PaginationInfo,
        PaginationParams, StrategyAttempt, TurnDirection, V2Slot, V2SlotStatus, V2Turn,
        WaitingState,
    },
    r#trait::V2StorageError,
};
use crate::magician_v2::artifact_v2::io::write_bytes_durably;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::ask_loop::session::ClarificationSession;
use crate::magician_v2::orchestrator::v2_orchestrator::{
    PendingClarification, RecommendedQuestion,
};
use crate::magician_v2::work_context::{WorkAuthorityRef, WorkContextKind};
use crate::magician_v2::{AnalysisMetadata, ProcessingMetadata, StateBundle, UnifiedQueryAnalysis};
use runtime_core::{V2ConversationStore as CoreV2ConversationStore, WorkAuthorityGrant};

pub struct FileV2Store {
    artifact_workspace: ArtifactV2Workspace,
    execution_locks: DashMap<String, Arc<Mutex<()>>>,
    execution_paths: DashMap<String, PathBuf>,
}

struct CrossProcessExecutionGuard {
    file: File,
}

impl FileV2Store {
    pub fn new<P: AsRef<Path>>(base_path: P) -> Self {
        let storage_root = base_path.as_ref().to_path_buf();
        Self::with_workspace_layout(ArtifactV2Workspace::new(Self::default_v3_root_for(
            &storage_root,
        )))
    }

    pub fn with_workspace_layout(artifact_workspace: ArtifactV2Workspace) -> Self {
        Self {
            artifact_workspace,
            execution_locks: DashMap::new(),
            execution_paths: DashMap::new(),
        }
    }

    fn default_v3_root_for(base_path: &Path) -> PathBuf {
        ArtifactV2Workspace::resolve_scoped_root(base_path)
    }

    async fn ensure_execution_generation(&self) -> Result<(), V2StorageError> {
        self.artifact_workspace
            .ensure_root()
            .await
            .map_err(|err| V2StorageError::Storage(err.to_string()))?;
        Ok(())
    }

    async fn path_exists(path: &Path) -> bool {
        fs::metadata(path).await.is_ok()
    }

    fn v3_execution_dir_from_execution(&self, execution: &ExecutionRun) -> PathBuf {
        self.artifact_workspace.runtime_execution_dir(
            &execution.principal,
            &execution.workspace,
            execution.task_id.as_deref(),
            &execution.id,
        )
    }

    async fn locate_v3_execution_dir(
        &self,
        execution_id: &str,
    ) -> Result<Option<PathBuf>, V2StorageError> {
        if let Some(cached) = self.execution_paths.get(execution_id) {
            let path = cached.value().clone();
            drop(cached);
            if Self::path_exists(&path.join("execution.json")).await {
                return Ok(Some(path));
            }
            self.execution_paths.remove(execution_id);
        }

        let scopes = self
            .artifact_workspace
            .list_scope_segments()
            .await
            .map_err(|err| V2StorageError::Storage(err.to_string()))?;

        for (principal, workspace) in scopes {
            let scoped_dir =
                self.artifact_workspace
                    .scoped_execution_dir(&principal, &workspace, execution_id);
            if Self::path_exists(&scoped_dir.join("execution.json")).await {
                self.execution_paths
                    .insert(execution_id.to_string(), scoped_dir.clone());
                return Ok(Some(scoped_dir));
            }

            // Task-bound executions share one document contract across both
            // user-visible and internal lifecycle roots. Internal tasks used to
            // be omitted here, which made their execution documents unreachable
            // after a process restart once the in-memory path cache was gone.
            for tasks_root in [
                self.artifact_workspace.tasks_root(&principal, &workspace),
                self.artifact_workspace
                    .internal_tasks_root(&principal, &workspace),
            ] {
                let mut task_entries = match fs::read_dir(&tasks_root).await {
                    Ok(entries) => entries,
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(err) => return Err(V2StorageError::Io(err)),
                };

                while let Some(task_entry) = task_entries.next_entry().await? {
                    if !task_entry.file_type().await?.is_dir() {
                        continue;
                    }
                    let candidate = task_entry.path().join("executions").join(execution_id);
                    if Self::path_exists(&candidate.join("execution.json")).await {
                        self.execution_paths
                            .insert(execution_id.to_string(), candidate.clone());
                        return Ok(Some(candidate));
                    }
                }
            }
        }

        Ok(None)
    }

    async fn execution_dir_for_read(&self, execution_id: &str) -> Result<PathBuf, V2StorageError> {
        if let Some(path) = self.locate_v3_execution_dir(execution_id).await? {
            return Ok(path);
        }

        Err(V2StorageError::ExecutionNotFound(execution_id.to_string()))
    }

    async fn execution_path_for_read(&self, execution_id: &str) -> Result<PathBuf, V2StorageError> {
        Ok(self
            .execution_dir_for_read(execution_id)
            .await?
            .join("execution.json"))
    }

    async fn execution_dir_for_write(
        &self,
        execution: &ExecutionRun,
    ) -> Result<PathBuf, V2StorageError> {
        self.artifact_workspace
            .ensure_runtime_execution_workspace(
                &execution.principal,
                &execution.workspace,
                execution.task_id.as_deref(),
                &execution.id,
            )
            .await
            .map_err(|err| V2StorageError::Storage(err.to_string()))?;
        let path = self.v3_execution_dir_from_execution(execution);
        self.execution_paths
            .insert(execution.id.clone(), path.clone());
        Ok(path)
    }

    fn lock_path_for_execution(&self, execution: &ExecutionRun) -> PathBuf {
        match execution.task_id.as_deref() {
            Some(task_id) => self.artifact_workspace.task_lock_path(
                &execution.principal,
                &execution.workspace,
                task_id,
            ),
            None => self
                .v3_execution_dir_from_execution(execution)
                .join(".file_v2.lock"),
        }
    }

    fn lock_path_for_execution_dir(&self, execution_dir: &Path) -> PathBuf {
        let is_task_execution_dir = execution_dir
            .parent()
            .and_then(|parent| parent.file_name())
            .map(|segment| segment == "executions")
            .unwrap_or(false);
        if is_task_execution_dir {
            execution_dir
                .parent()
                .and_then(|parent| parent.parent())
                .and_then(|task_dir| {
                    let task_id = task_dir.file_name()?.to_str()?;
                    let scope_root = task_dir.parent()?.parent()?;
                    Some(
                        scope_root
                            .join(".task_lifecycle")
                            .join("locks")
                            .join(format!("{task_id}.lock")),
                    )
                })
                .unwrap_or_else(|| execution_dir.join(".file_v2.lock"))
        } else {
            execution_dir.join(".file_v2.lock")
        }
    }

    async fn acquire_cross_process_guard(
        &self,
        lock_path: PathBuf,
    ) -> Result<CrossProcessExecutionGuard, V2StorageError> {
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let file = magician_core::blocking_admission::spawn_blocking_admitted(
            move || -> Result<File, std::io::Error> {
                let file = OpenOptions::new()
                    .create(true)
                    .read(true)
                    .write(true)
                    .open(&lock_path)?;
                file.lock_exclusive()?;
                Ok(file)
            },
        )
        .await
        .map_err(|err| V2StorageError::Storage(format!("execution lock task panicked: {err}")))??;
        Ok(CrossProcessExecutionGuard { file })
    }

    async fn acquire_execution_write_guard_for_doc(
        &self,
        execution: &ExecutionRun,
    ) -> Result<CrossProcessExecutionGuard, V2StorageError> {
        let guard = self
            .acquire_cross_process_guard(self.lock_path_for_execution(execution))
            .await?;
        if let Some(task_id) = execution.task_id.as_deref() {
            let marker = self.artifact_workspace.task_deletion_marker_path(
                &execution.principal,
                &execution.workspace,
                task_id,
            );
            if fs::try_exists(marker).await? {
                return Err(V2StorageError::ExecutionNotFound(execution.id.clone()));
            }
        }
        Ok(guard)
    }

    async fn acquire_execution_write_guard(
        &self,
        execution_id: &str,
    ) -> Result<CrossProcessExecutionGuard, V2StorageError> {
        let dir = self.execution_dir_for_read(execution_id).await?;
        let guard = self
            .acquire_cross_process_guard(self.lock_path_for_execution_dir(&dir))
            .await?;
        if let Some(marker) = Self::task_deletion_marker_for_execution_dir(&dir) {
            if fs::try_exists(marker).await? {
                return Err(V2StorageError::ExecutionNotFound(execution_id.to_string()));
            }
        }
        Ok(guard)
    }

    /// The task-deletion refusal that [`Self::acquire_execution_write_guard`]
    /// performs while taking the lock, for callers that already hold it.
    async fn ensure_execution_not_task_deleted(
        &self,
        execution_id: &str,
    ) -> Result<(), V2StorageError> {
        let dir = self.execution_dir_for_read(execution_id).await?;
        if let Some(marker) = Self::task_deletion_marker_for_execution_dir(&dir) {
            if fs::try_exists(marker).await? {
                return Err(V2StorageError::ExecutionNotFound(execution_id.to_string()));
            }
        }
        Ok(())
    }

    fn task_deletion_marker_for_execution_dir(execution_dir: &Path) -> Option<PathBuf> {
        let executions_dir = execution_dir.parent()?;
        if executions_dir.file_name()?.to_str()? != "executions" {
            return None;
        }
        let task_dir = executions_dir.parent()?;
        let task_id = task_dir.file_name()?.to_str()?;
        let scope_root = task_dir.parent()?.parent()?;
        Some(
            scope_root
                .join(".task_lifecycle")
                .join("deleted")
                .join(format!("{task_id}.deleted")),
        )
    }

    /// Publish `content` at `path` through the shared durable writer: unique
    /// temp name, `sync_all`, rename, then a parent-directory sync.
    ///
    /// The hand-rolled version this replaces already fsynced the file and the
    /// parent directory, so durability is unchanged. What it lacked was a
    /// unique temp name, removal of the staging file on a failed publish, and
    /// the transient-fd-exhaustion retry. Its fixed `<file>.tmp` was not a
    /// live hazard here — every writer of `execution.json` reaches this
    /// through a per-execution mutex plus an exclusive cross-process flock —
    /// but the next caller to copy the shape would not inherit those locks.
    async fn write_bytes_atomic(&self, path: &Path, content: &[u8]) -> Result<(), V2StorageError> {
        if crate::magician_v2::work_owners::store_for_any_owner(&self.artifact_workspace, path)
            .is_some()
        {
            crate::magician_v2::work_owners::persist_work_file(
                &self.artifact_workspace,
                path,
                content,
            )
            .await
            .map_err(|err| V2StorageError::Storage(err.to_string()))?;
            return Ok(());
        }
        write_bytes_durably(path, content).await?;
        Ok(())
    }

    async fn collect_execution_documents(
        &self,
    ) -> Result<Vec<ExecutionRunDocument>, V2StorageError> {
        self.ensure_execution_generation().await?;
        let mut docs = Vec::new();
        let mut seen = HashSet::new();

        let scopes = self
            .artifact_workspace
            .list_scope_segments()
            .await
            .map_err(|err| V2StorageError::Storage(err.to_string()))?;

        for (principal, workspace) in scopes {
            let scoped_root = self
                .artifact_workspace
                .scoped_executions_root(&principal, &workspace);
            self.collect_execution_documents_under(&scoped_root, &mut docs, &mut seen)
                .await?;

            for tasks_root in [
                self.artifact_workspace.tasks_root(&principal, &workspace),
                self.artifact_workspace
                    .internal_tasks_root(&principal, &workspace),
            ] {
                let mut task_entries = match fs::read_dir(&tasks_root).await {
                    Ok(entries) => entries,
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(err) => return Err(V2StorageError::Io(err)),
                };
                while let Some(task_entry) = task_entries.next_entry().await? {
                    if !task_entry.file_type().await?.is_dir() {
                        continue;
                    }
                    let executions_root = task_entry.path().join("executions");
                    self.collect_execution_documents_under(&executions_root, &mut docs, &mut seen)
                        .await?;
                }
            }
        }

        Ok(docs)
    }

    async fn collect_execution_documents_under(
        &self,
        root: &Path,
        docs: &mut Vec<ExecutionRunDocument>,
        seen: &mut HashSet<String>,
    ) -> Result<(), V2StorageError> {
        let mut entries = match fs::read_dir(root).await {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(V2StorageError::Io(err)),
        };

        while let Some(entry) = entries.next_entry().await? {
            if !entry.file_type().await?.is_dir() {
                continue;
            }
            let dir = entry.path();
            let execution_path = dir.join("execution.json");
            if !Self::path_exists(&execution_path).await {
                continue;
            }
            let content = fs::read_to_string(&execution_path).await?;
            let doc: ExecutionRunDocument = serde_json::from_str(&content)?;
            if seen.insert(doc.execution.id.clone()) {
                self.execution_paths
                    .insert(doc.execution.id.clone(), dir.clone());
                docs.push(doc);
            }
        }
        Ok(())
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    async fn load_execution_index(&self) -> Result<ExecutionIndex, V2StorageError> {
        self.rebuild_execution_index().await
    }

    async fn sync_execution_index_entry(
        &self,
        _execution_id: &str,
        _present: bool,
    ) -> Result<(), V2StorageError> {
        Ok(())
    }

    pub async fn rebuild_execution_index(&self) -> Result<ExecutionIndex, V2StorageError> {
        let mut index = ExecutionIndex::default();
        let docs = self.collect_execution_documents().await?;
        for doc in docs {
            // Index rebuild is discovery, not lifecycle authority. Another
            // process may still own this Executing row through a live loop
            // lease/pin; recovery performs its exact segment-binding owner
            // check under the execution admission fence before any transition.
            index.insert(
                &doc.execution.id,
                doc.execution.waiting_state.clone(),
                doc.execution.active_owner_agent_id.clone(),
            );
        }
        Ok(index)
    }

    pub async fn create_execution_with_params(
        &self,
        params: CreateExecutionParams,
    ) -> Result<ExecutionRun, V2StorageError> {
        self.ensure_execution_generation().await?;
        let now = chrono::Utc::now().timestamp_millis();
        let execution = ExecutionRun {
            id: params
                .execution_id
                .unwrap_or_else(|| Uuid::new_v4().to_string()),
            principal: params.principal,
            workspace: params.workspace,
            task_id: params.task_id,
            root_execution_id: params.root_execution_id,
            title: params.title,
            waiting_state: params.waiting_state,
            created_at: now,
            updated_at: now,
            processing_correlation_id: None,
            current_stage: None,
            current_provider: None,
            escalation_trigger: None,
            parent_execution_id: params.parent_execution_id,
            child_execution_ids: Vec::new(),
            active_owner_agent_id: params.active_owner_agent_id,
            owner_stack: Vec::new(),
            active_delegation_group: Vec::new(),
            timeout_secs: params.timeout_secs,
            delegation_chain: params.delegation_chain,
            work_authority: params.work_authority,
            paused_from_state: None,
            entry_mode: params.entry_mode,
        };

        let doc = ExecutionRunDocument {
            execution: execution.clone(),
            status_revision: 0,
            turns: Vec::new(),
            slots: Vec::new(),
            states: Vec::new(),
            clarification_session: None,
            clarification_history: Vec::new(),
        };

        self.save_execution_document(&doc).await?;
        self.sync_execution_index_entry(&execution.id, true).await?;
        Ok(execution)
    }

    async fn load_execution_document(
        &self,
        execution_id: &str,
    ) -> Result<ExecutionRunDocument, V2StorageError> {
        self.ensure_execution_generation().await?;
        let path = self.execution_path_for_read(execution_id).await?;
        let content = fs::read_to_string(&path)
            .await
            .map_err(|_| V2StorageError::ExecutionNotFound(execution_id.to_string()))?;
        let doc = serde_json::from_str(&content)?;
        Ok(doc)
    }

    async fn save_execution_document_unlocked(
        &self,
        doc: &ExecutionRunDocument,
    ) -> Result<(), V2StorageError> {
        self.ensure_execution_generation().await?;
        let path = self
            .execution_dir_for_write(&doc.execution)
            .await?
            .join("execution.json");
        let content = serde_json::to_vec_pretty(&doc)?;
        self.write_bytes_atomic(&path, &content).await
    }

    pub async fn save_execution_document(
        &self,
        doc: &ExecutionRunDocument,
    ) -> Result<(), V2StorageError> {
        let _guard = self
            .acquire_execution_write_guard_for_doc(&doc.execution)
            .await?;
        self.save_execution_document_unlocked(doc).await
    }

    fn acquire_lock(&self, execution_id: &str) -> Arc<Mutex<()>> {
        self.execution_locks
            .entry(execution_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    async fn lock_execution(&self, execution_id: &str) -> (Arc<Mutex<()>>, OwnedMutexGuard<()>) {
        let lock = self.acquire_lock(execution_id);
        let guard = lock.clone().lock_owned().await;
        (lock, guard)
    }

    fn maybe_cleanup_execution_lock(&self, execution_id: &str, lock: &Arc<Mutex<()>>) {
        if Arc::strong_count(lock) <= 2 {
            self.execution_locks.remove(execution_id);
        }
    }

    /// The status compare-and-swap body shared by the ordinary CAS and the
    /// variant that runs under a caller-held task fence.
    fn apply_status_compare_exchange(
        doc: &mut ExecutionRunDocument,
        expected: &WaitingState,
        status: WaitingState,
    ) -> bool {
        if doc.execution.waiting_state != *expected {
            return false;
        }
        if status == WaitingState::Paused {
            if doc.execution.waiting_state != WaitingState::Paused
                && !doc.execution.waiting_state.is_terminal()
            {
                doc.execution.paused_from_state = Some(doc.execution.waiting_state.clone());
            }
        } else {
            doc.execution.paused_from_state = None;
        }
        doc.execution.waiting_state = status;
        doc.status_revision = doc.status_revision.saturating_add(1);
        doc.execution.updated_at = chrono::Utc::now()
            .timestamp_millis()
            .max(doc.execution.updated_at.saturating_add(1));
        true
    }

    async fn with_execution_document<R, F>(
        &self,
        execution_id: &str,
        mutate: F,
    ) -> Result<R, V2StorageError>
    where
        R: Send,
        F: FnOnce(&mut ExecutionRunDocument) -> Result<R, V2StorageError> + Send,
    {
        let (lock, guard) = self.lock_execution(execution_id).await;
        let _cross_process_guard = self.acquire_execution_write_guard(execution_id).await?;
        let mut doc = self.load_execution_document(execution_id).await?;
        let result = mutate(&mut doc)?;
        self.save_execution_document_unlocked(&doc).await?;
        self.sync_execution_index_entry(execution_id, true).await?;
        drop(guard);
        self.maybe_cleanup_execution_lock(execution_id, &lock);
        Ok(result)
    }

    /// [`Self::with_execution_document`] for a caller that already holds this
    /// execution's task-scoped flock as an outer transaction fence. `flock` is
    /// per open-file-description, so re-opening the same lock file here would
    /// block against the caller's own descriptor forever. Skipping it does not
    /// weaken exclusion — the caller holds the exact same lock — but the
    /// task-deletion refusal that normally rides along with the guard still
    /// has to run.
    ///
    /// Deliberately a sibling rather than a shared inner helper: every store
    /// write goes through `with_execution_document`, and wrapping it in one
    /// more `async fn` frame grows the state machine of the whole runtime call
    /// graph, which this crate's stack-depth contract tests hold to a budget.
    async fn with_execution_document_under_held_task_lock<R, F>(
        &self,
        execution_id: &str,
        mutate: F,
    ) -> Result<R, V2StorageError>
    where
        R: Send,
        F: FnOnce(&mut ExecutionRunDocument) -> Result<R, V2StorageError> + Send,
    {
        let (lock, guard) = self.lock_execution(execution_id).await;
        self.ensure_execution_not_task_deleted(execution_id).await?;
        let mut doc = self.load_execution_document(execution_id).await?;
        let result = mutate(&mut doc)?;
        self.save_execution_document_unlocked(&doc).await?;
        self.sync_execution_index_entry(execution_id, true).await?;
        drop(guard);
        self.maybe_cleanup_execution_lock(execution_id, &lock);
        Ok(result)
    }

    /// Update an execution's metadata via the per-execution mutex.
    pub async fn update_execution_metadata(
        &self,
        execution_id: &str,
        f: impl FnOnce(&mut ExecutionRun) + Send,
    ) -> Result<(), V2StorageError> {
        self.with_execution_document(execution_id, |doc| {
            f(&mut doc.execution);
            doc.execution.updated_at = chrono::Utc::now().timestamp_millis();
            Ok(())
        })
        .await
    }

    pub async fn add_child_execution_link(
        &self,
        parent_execution_id: &str,
        child_execution_id: &str,
    ) -> Result<(), V2StorageError> {
        let child_id = child_execution_id.to_string();
        self.update_execution_metadata(parent_execution_id, move |execution| {
            if !execution.child_execution_ids.contains(&child_id) {
                execution.child_execution_ids.push(child_id.clone());
            }
        })
        .await
    }
}

#[async_trait]
impl CoreV2ConversationStore for FileV2Store {
    type Error = V2StorageError;
    type Execution = ExecutionRun;
    type ExecutionSummary = ExecutionSummary;
    type ExecutionStatus = WaitingState;
    type Turn = V2Turn;
    type TurnDirection = TurnDirection;
    type Slot = V2Slot;
    type SlotStatus = V2SlotStatus;
    type StrategyAttempt = StrategyAttempt;
    type ProcessingMetadata = ProcessingMetadata;
    type UnifiedAnalysis = UnifiedQueryAnalysis;
    type AnalysisMetadata = AnalysisMetadata;
    type StateBundle = StateBundle;
    type RecommendedQuestion = RecommendedQuestion;
    type PendingClarification = PendingClarification;
    type ClarificationSession = ClarificationSession;
    type ClarificationHistoryEntry = ClarificationHistoryEntry;

    async fn create_execution_with_options(
        &self,
        principal: &str,
        workspace: &str,
        title: Option<String>,
        active_owner_agent_id: &str,
        task_id: Option<String>,
        root_execution_id: Option<String>,
        execution_id: Option<String>,
        parent_execution_id: Option<String>,
        timeout_secs: Option<u64>,
        delegation_chain: Vec<String>,
        waiting_state: WaitingState,
    ) -> Result<ExecutionRun, V2StorageError> {
        self.create_execution_with_work_authority(
            principal,
            workspace,
            title,
            active_owner_agent_id,
            task_id,
            root_execution_id,
            execution_id,
            parent_execution_id,
            timeout_secs,
            delegation_chain,
            waiting_state,
            None,
        )
        .await
    }

    async fn create_execution_with_work_authority(
        &self,
        principal: &str,
        workspace: &str,
        title: Option<String>,
        active_owner_agent_id: &str,
        task_id: Option<String>,
        root_execution_id: Option<String>,
        execution_id: Option<String>,
        parent_execution_id: Option<String>,
        timeout_secs: Option<u64>,
        delegation_chain: Vec<String>,
        waiting_state: WaitingState,
        work_authority: Option<WorkAuthorityGrant>,
    ) -> Result<ExecutionRun, V2StorageError> {
        if parent_execution_id.is_none()
            && (task_id.is_none()
                || root_execution_id.is_none()
                || execution_id.is_none()
                || root_execution_id != execution_id)
        {
            return Err(V2StorageError::Storage(
                "root executions must be created with explicit execution_id, task_id, and matching root_execution_id".to_string(),
            ));
        }

        // The wire grant becomes the typed carrier here, and an unreadable one
        // is an error rather than an absent authority: a kind token this build
        // does not know, or an id carrying the U+001F separator derived ids are
        // joined with, would otherwise be dropped into `None` and the run would
        // start unconfined while the caller believed it was confined.
        let work_authority = match work_authority {
            None => None,
            Some(grant) => {
                let kind = WorkContextKind::from_token(&grant.work_kind, &grant.work_id)
                    .map_err(V2StorageError::Storage)?;
                Some(
                    WorkAuthorityRef::new(kind, grant.authority_revision)
                        .map_err(V2StorageError::Storage)?,
                )
            },
        };

        self.create_execution_with_params(CreateExecutionParams {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            task_id,
            root_execution_id,
            title,
            active_owner_agent_id: active_owner_agent_id.to_string(),
            execution_id,
            parent_execution_id,
            timeout_secs,
            delegation_chain,
            work_authority,
            waiting_state,
            entry_mode: ExecutionEntryMode::PlanningBacked,
        })
        .await
    }

    async fn get_execution(&self, execution_id: &str) -> Result<ExecutionRun, V2StorageError> {
        let doc = self.load_execution_document(execution_id).await?;
        Ok(doc.execution)
    }

    async fn update_execution_entry_mode(
        &self,
        execution_id: &str,
        entry_mode: String,
    ) -> Result<(), V2StorageError> {
        let next_mode = ExecutionEntryMode::from_token(&entry_mode);
        self.with_execution_document(execution_id, |doc| {
            if doc.execution.entry_mode != next_mode {
                doc.execution.entry_mode = next_mode.clone();
                doc.execution.updated_at = chrono::Utc::now().timestamp_millis();
            }
            Ok(())
        })
        .await
    }

    async fn add_child_execution_id(
        &self,
        parent_execution_id: &str,
        child_execution_id: &str,
    ) -> Result<(), V2StorageError> {
        self.add_child_execution_link(parent_execution_id, child_execution_id)
            .await
    }

    async fn add_turn(
        &self,
        execution_id: &str,
        direction: TurnDirection,
        text: String,
        in_reply_to_slot_id: Option<String>,
    ) -> Result<V2Turn, V2StorageError> {
        self.with_execution_document(execution_id, |doc| {
            let now = chrono::Utc::now().timestamp_millis();
            let turn = V2Turn {
                id: Uuid::new_v4().to_string(),
                execution_id: execution_id.to_string(),
                direction,
                text,
                in_reply_to_slot_id,
                created_at: now,
                query_analysis: None,
                analysis_metadata: None,
                strategy_attempts: Vec::new(),
                processing_metadata: None,
                recommended_questions: None,
                enriched_query: None,
            };

            doc.turns.push(turn.clone());
            doc.execution.updated_at = now;
            Ok(turn)
        })
        .await
    }

    async fn add_turn_with_id(
        &self,
        execution_id: &str,
        turn_id: &str,
        direction: TurnDirection,
        text: String,
        in_reply_to_slot_id: Option<String>,
    ) -> Option<Result<V2Turn, V2StorageError>> {
        if turn_id.trim().is_empty() || turn_id.len() > 512 {
            return Some(Err(V2StorageError::Storage(
                "deterministic turn id is invalid".to_owned(),
            )));
        }
        Some(
            self.with_execution_document(execution_id, |doc| {
                if let Some(existing) = doc.turns.iter().find(|turn| turn.id == turn_id) {
                    if existing.execution_id == execution_id
                        && existing.direction == direction
                        && existing.text == text
                        && existing.in_reply_to_slot_id == in_reply_to_slot_id
                    {
                        return Ok(existing.clone());
                    }
                    return Err(V2StorageError::Storage(
                        "deterministic turn id is already bound to different content".to_owned(),
                    ));
                }
                let now = chrono::Utc::now().timestamp_millis();
                let turn = V2Turn {
                    id: turn_id.to_owned(),
                    execution_id: execution_id.to_string(),
                    direction,
                    text,
                    in_reply_to_slot_id,
                    created_at: now,
                    query_analysis: None,
                    analysis_metadata: None,
                    strategy_attempts: Vec::new(),
                    processing_metadata: None,
                    recommended_questions: None,
                    enriched_query: None,
                };
                doc.turns.push(turn.clone());
                doc.execution.updated_at = now;
                Ok(turn)
            })
            .await,
        )
    }

    async fn store_analysis(
        &self,
        execution_id: &str,
        turn_id: &str,
        analysis: UnifiedQueryAnalysis,
        metadata: AnalysisMetadata,
    ) -> Result<(), V2StorageError> {
        self.with_execution_document(execution_id, |doc| {
            let turn = doc
                .turns
                .iter_mut()
                .find(|t| t.id == turn_id)
                .ok_or_else(|| V2StorageError::TurnNotFound(turn_id.to_string()))?;

            turn.query_analysis = Some(analysis.clone());
            turn.analysis_metadata = Some(metadata.clone());
            doc.execution.updated_at = chrono::Utc::now().timestamp_millis();
            Ok(())
        })
        .await
    }

    async fn store_strategy_attempts(
        &self,
        execution_id: &str,
        turn_id: &str,
        attempts: Vec<StrategyAttempt>,
        processing_metadata: crate::magician_v2::orchestrator::v2_orchestrator::ProcessingMetadata,
        recommended_questions: Option<Vec<RecommendedQuestion>>,
    ) -> Result<(), V2StorageError> {
        self.with_execution_document(execution_id, |doc| {
            let turn = doc
                .turns
                .iter_mut()
                .find(|t| t.id == turn_id)
                .ok_or_else(|| V2StorageError::TurnNotFound(turn_id.to_string()))?;

            turn.strategy_attempts = attempts.clone();
            turn.processing_metadata = Some(processing_metadata.clone());
            turn.recommended_questions = recommended_questions.clone();
            // Phase 5: pending_clarification removed - now tracked in ClarificationSession
            doc.execution.updated_at = chrono::Utc::now().timestamp_millis();
            Ok(())
        })
        .await
    }

    async fn get_turn(&self, execution_id: &str, turn_id: &str) -> Result<V2Turn, V2StorageError> {
        let doc = self.load_execution_document(execution_id).await?;
        doc.turns
            .into_iter()
            .find(|t| t.id == turn_id)
            .ok_or_else(|| V2StorageError::TurnNotFound(turn_id.to_string()))
    }

    async fn get_turns(&self, execution_id: &str) -> Result<Vec<V2Turn>, V2StorageError> {
        let doc = self.load_execution_document(execution_id).await?;
        Ok(doc.turns)
    }

    async fn get_latest_turn_with_analysis(
        &self,
        execution_id: &str,
    ) -> Result<Option<V2Turn>, V2StorageError> {
        let doc = self.load_execution_document(execution_id).await?;

        // Find latest turn that has analysis
        Ok(doc
            .turns
            .into_iter()
            .rev()
            .find(|t| t.query_analysis.is_some()))
    }

    async fn list_executions(
        &self,
        principal: &str,
        workspace: &str,
        pagination: PaginationParams,
    ) -> Result<PaginatedResult<ExecutionSummary>, V2StorageError> {
        let mut summaries = Vec::new();
        for doc in self.collect_execution_documents().await? {
            if doc.execution.principal == principal && doc.execution.workspace == workspace {
                summaries.push(ExecutionSummary::from(&doc));
            }
        }

        // Sort by updated_at descending (newest first)
        summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

        // Apply pagination
        let total = summaries.len();
        let items = summaries
            .into_iter()
            .skip(pagination.offset)
            .take(pagination.limit)
            .collect::<Vec<_>>();

        let has_more = pagination.offset + items.len() < total;

        Ok(PaginatedResult {
            items,
            pagination: PaginationInfo {
                total,
                limit: pagination.limit,
                offset: pagination.offset,
                has_more,
            },
        })
    }

    async fn delete_execution(&self, execution_id: &str) -> Result<(), V2StorageError> {
        self.ensure_execution_generation().await?;
        let (_lock, _guard) = self.lock_execution(execution_id).await;
        let _cross_process_guard = self.acquire_execution_write_guard(execution_id).await?;
        let execution_dir = self.execution_dir_for_read(execution_id).await?;

        if execution_dir.exists() {
            fs::remove_dir_all(&execution_dir).await?;
        }

        // Remove from in-memory locks
        self.execution_locks.remove(execution_id);
        self.execution_paths.remove(execution_id);
        self.sync_execution_index_entry(execution_id, false).await?;

        tracing::info!(
            "[FILE-STORE] Execution {} deleted (entire directory)",
            execution_id
        );
        Ok(())
    }

    async fn get_turns_paginated(
        &self,
        execution_id: &str,
        pagination: PaginationParams,
        direction_filter: Option<TurnDirection>,
    ) -> Result<PaginatedResult<V2Turn>, V2StorageError> {
        let doc = self.load_execution_document(execution_id).await?;

        // Filter by direction if specified
        let turns: Vec<V2Turn> = if let Some(ref direction) = direction_filter {
            doc.turns
                .into_iter()
                .filter(|t| &t.direction == direction)
                .collect()
        } else {
            doc.turns
        };

        // Apply pagination
        let total = turns.len();
        let items = turns
            .into_iter()
            .skip(pagination.offset)
            .take(pagination.limit)
            .collect::<Vec<_>>();

        let has_more = pagination.offset + items.len() < total;

        Ok(PaginatedResult {
            items,
            pagination: PaginationInfo {
                total,
                limit: pagination.limit,
                offset: pagination.offset,
                has_more,
            },
        })
    }

    // ========================================================================
    // Execution Status Management
    // ========================================================================

    async fn update_execution_status(
        &self,
        execution_id: &str,
        status: WaitingState,
    ) -> Result<(), V2StorageError> {
        self.with_execution_document(execution_id, |doc| {
            if status == WaitingState::Paused {
                if doc.execution.waiting_state != WaitingState::Paused
                    && !doc.execution.waiting_state.is_terminal()
                {
                    doc.execution.paused_from_state = Some(doc.execution.waiting_state.clone());
                }
            } else {
                doc.execution.paused_from_state = None;
            }
            doc.execution.waiting_state = status;
            doc.status_revision = doc.status_revision.saturating_add(1);
            doc.execution.updated_at = chrono::Utc::now()
                .timestamp_millis()
                .max(doc.execution.updated_at.saturating_add(1));
            Ok(())
        })
        .await
    }

    async fn compare_exchange_execution_status(
        &self,
        execution_id: &str,
        expected: WaitingState,
        status: WaitingState,
    ) -> Result<bool, V2StorageError> {
        self.with_execution_document(execution_id, |doc| {
            Ok(Self::apply_status_compare_exchange(doc, &expected, status))
        })
        .await
    }

    /// This store's execution write guard is the task-scoped flock for any
    /// task-bound execution, which is the same file a caller holding the task
    /// transaction fence already owns. Re-acquiring it here would self-deadlock
    /// on a second descriptor, so reuse the caller's fence.
    async fn compare_exchange_execution_status_holding_task_lock(
        &self,
        execution_id: &str,
        expected: WaitingState,
        status: WaitingState,
    ) -> Result<bool, V2StorageError> {
        self.with_execution_document_under_held_task_lock(execution_id, |doc| {
            Ok(Self::apply_status_compare_exchange(doc, &expected, status))
        })
        .await
    }

    async fn compare_exchange_execution_status_at(
        &self,
        execution_id: &str,
        expected: WaitingState,
        expected_status_revision: u64,
        status: WaitingState,
    ) -> Result<bool, V2StorageError> {
        self.with_execution_document(execution_id, |doc| {
            if doc.execution.waiting_state != expected
                || doc.status_revision != expected_status_revision
            {
                return Ok(false);
            }
            if status == WaitingState::Paused {
                if doc.execution.waiting_state != WaitingState::Paused
                    && !doc.execution.waiting_state.is_terminal()
                {
                    doc.execution.paused_from_state = Some(doc.execution.waiting_state.clone());
                }
            } else {
                doc.execution.paused_from_state = None;
            }
            doc.execution.waiting_state = status;
            doc.status_revision = doc.status_revision.saturating_add(1);
            doc.execution.updated_at = chrono::Utc::now()
                .timestamp_millis()
                .max(doc.execution.updated_at.saturating_add(1));
            Ok(true)
        })
        .await
    }

    async fn get_execution_status_revision(
        &self,
        execution_id: &str,
    ) -> Result<u64, V2StorageError> {
        Ok(self
            .load_execution_document(execution_id)
            .await?
            .status_revision)
    }

    async fn bind_execution_scope(
        &self,
        execution_id: &str,
        task_id: Option<String>,
        root_execution_id: Option<String>,
    ) -> Result<(), V2StorageError> {
        let execution_id_for_lookup = execution_id.to_string();
        let execution_id_for_error = execution_id_for_lookup.clone();
        self.with_execution_document(&execution_id_for_lookup, move |doc| {
            if doc.execution.task_id.as_deref().is_some()
                && doc.execution.task_id.as_deref() != task_id.as_deref()
            {
                return Err(V2StorageError::Storage(format!(
                    "execution {} already belongs to task {:?} and cannot be rebound to {:?}",
                    execution_id_for_error, doc.execution.task_id, task_id
                )));
            }
            if doc.execution.root_execution_id.as_deref().is_some()
                && doc.execution.root_execution_id.as_deref() != root_execution_id.as_deref()
            {
                return Err(V2StorageError::Storage(format!(
                    "execution {} already belongs to root execution {:?} and cannot be rebound to {:?}",
                    execution_id_for_error, doc.execution.root_execution_id, root_execution_id
                )));
            }
            doc.execution.task_id = task_id.clone();
            doc.execution.root_execution_id = root_execution_id.clone();
            doc.execution.updated_at = chrono::Utc::now().timestamp_millis();
            Ok(())
        })
        .await
    }

    async fn update_execution_owner_snapshot(
        &self,
        execution_id: &str,
        active_owner_agent_id: &str,
        owner_stack: &[String],
    ) -> Result<(), V2StorageError> {
        let active_owner_agent_id = active_owner_agent_id.to_string();
        let owner_stack = owner_stack.to_vec();
        self.with_execution_document(execution_id, move |doc| {
            doc.execution.active_owner_agent_id = active_owner_agent_id.clone();
            doc.execution.owner_stack = owner_stack.clone();
            doc.execution.updated_at = chrono::Utc::now().timestamp_millis();
            Ok(())
        })
        .await
    }

    async fn replace_active_delegation_group(
        &self,
        execution_id: &str,
        active_delegation_group: &[String],
    ) -> Result<(), V2StorageError> {
        let active_delegation_group = active_delegation_group.to_vec();
        self.with_execution_document(execution_id, move |doc| {
            doc.execution.active_delegation_group = active_delegation_group.clone();
            doc.execution.updated_at = chrono::Utc::now().timestamp_millis();
            Ok(())
        })
        .await
    }

    async fn settle_delegation_parent(
        &self,
        execution_id: &str,
        status: WaitingState,
    ) -> Result<(), V2StorageError> {
        self.with_execution_document(execution_id, move |doc| {
            doc.execution.waiting_state = status.clone();
            doc.status_revision = doc.status_revision.saturating_add(1);
            doc.execution.paused_from_state = None;
            doc.execution.active_delegation_group.clear();
            doc.execution.updated_at = chrono::Utc::now().timestamp_millis();
            Ok(())
        })
        .await
    }

    async fn get_execution_status(
        &self,
        execution_id: &str,
    ) -> Result<WaitingState, V2StorageError> {
        let doc = self.load_execution_document(execution_id).await?;
        Ok(doc.execution.waiting_state)
    }

    async fn update_processing_correlation_id(
        &self,
        execution_id: &str,
        correlation_id: Option<String>,
    ) -> Result<(), V2StorageError> {
        self.with_execution_document(execution_id, |doc| {
            doc.execution.processing_correlation_id = correlation_id.clone();
            doc.execution.updated_at = chrono::Utc::now().timestamp_millis();
            Ok(())
        })
        .await
    }

    // ========================================================================
    // Slot Management
    // ========================================================================

    async fn create_slot(
        &self,
        execution_id: &str,
        name: String,
        schema_json: serde_json::Value,
        required: bool,
        asked_turn_id: Option<String>,
    ) -> Result<V2Slot, V2StorageError> {
        self.with_execution_document(execution_id, |doc| {
            let now = chrono::Utc::now().timestamp_millis();
            let slot = V2Slot {
                id: Uuid::new_v4().to_string(),
                execution_id: execution_id.to_string(),
                name,
                schema_json,
                required,
                status: V2SlotStatus::Pending,
                asked_turn_id,
                answer: None,
                created_at: now,
                updated_at: now,
            };

            doc.slots.push(slot.clone());
            doc.execution.updated_at = now;
            Ok(slot)
        })
        .await
    }

    async fn update_slot_answer(
        &self,
        execution_id: &str,
        slot_id: &str,
        answer: serde_json::Value,
    ) -> Result<(), V2StorageError> {
        self.with_execution_document(execution_id, |doc| {
            let slot = doc
                .slots
                .iter_mut()
                .find(|s| s.id == slot_id)
                .ok_or_else(|| V2StorageError::SlotNotFound(slot_id.to_string()))?;

            slot.answer = Some(answer.clone());
            slot.status = V2SlotStatus::Answered;
            slot.updated_at = chrono::Utc::now().timestamp_millis();
            doc.execution.updated_at = slot.updated_at;
            Ok(())
        })
        .await
    }

    async fn update_slot_status(
        &self,
        execution_id: &str,
        slot_id: &str,
        status: V2SlotStatus,
    ) -> Result<(), V2StorageError> {
        self.with_execution_document(execution_id, |doc| {
            let slot = doc
                .slots
                .iter_mut()
                .find(|s| s.id == slot_id)
                .ok_or_else(|| V2StorageError::SlotNotFound(slot_id.to_string()))?;

            slot.status = status;
            slot.updated_at = chrono::Utc::now().timestamp_millis();
            doc.execution.updated_at = slot.updated_at;
            Ok(())
        })
        .await
    }

    async fn get_slots(&self, execution_id: &str) -> Result<Vec<V2Slot>, V2StorageError> {
        let doc = self.load_execution_document(execution_id).await?;
        Ok(doc.slots)
    }

    async fn get_pending_slots(&self, execution_id: &str) -> Result<Vec<V2Slot>, V2StorageError> {
        let doc = self.load_execution_document(execution_id).await?;
        Ok(doc
            .slots
            .into_iter()
            .filter(|s| s.status == V2SlotStatus::Pending)
            .collect())
    }

    async fn get_slot(&self, execution_id: &str, slot_id: &str) -> Result<V2Slot, V2StorageError> {
        let doc = self.load_execution_document(execution_id).await?;
        doc.slots
            .into_iter()
            .find(|s| s.id == slot_id)
            .ok_or_else(|| V2StorageError::SlotNotFound(slot_id.to_string()))
    }

    async fn append_state(
        &self,
        execution_id: &str,
        state: crate::magician_v2::state_tracker::StateBundle,
    ) -> Result<(), V2StorageError> {
        self.with_execution_document(execution_id, |doc| {
            doc.states.push(state.clone());
            doc.execution.updated_at = chrono::Utc::now().timestamp_millis();
            Ok(())
        })
        .await
    }

    async fn get_states(
        &self,
        execution_id: &str,
    ) -> Result<Vec<crate::magician_v2::state_tracker::StateBundle>, V2StorageError> {
        let doc = self.load_execution_document(execution_id).await?;
        let mut states = doc.states;
        states.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(states)
    }

    async fn get_latest_state(
        &self,
        execution_id: &str,
    ) -> Result<Option<crate::magician_v2::state_tracker::StateBundle>, V2StorageError> {
        let doc = self.load_execution_document(execution_id).await?;
        Ok(doc
            .states
            .into_iter()
            .max_by(|a, b| a.created_at.cmp(&b.created_at)))
    }

    async fn update_execution_processing_stage(
        &self,
        execution_id: &str,
        stage: Option<String>,
        provider: Option<String>,
    ) -> Result<(), V2StorageError> {
        self.with_execution_document(execution_id, |doc| {
            doc.execution.current_stage = stage.clone();
            doc.execution.current_provider = provider.clone();
            doc.execution.updated_at = chrono::Utc::now().timestamp_millis();
            Ok(())
        })
        .await
    }

    /// Update the enriched_query field for a specific turn (Gap #5 fix)
    ///
    /// This is called after batch completion and query rewriting to store
    /// the consolidated query that incorporates all user answers.
    async fn update_turn_enriched_query(
        &self,
        execution_id: &str,
        turn_id: &str,
        enriched_query: Option<String>,
    ) -> Result<(), V2StorageError> {
        self.with_execution_document(execution_id, |doc| {
            // Find the turn and update its enriched_query field
            if let Some(turn) = doc.turns.iter_mut().find(|t| t.id == turn_id) {
                turn.enriched_query = enriched_query.clone();
                doc.execution.updated_at = chrono::Utc::now().timestamp_millis();
                Ok(())
            } else {
                Err(V2StorageError::TurnNotFound(format!(
                    "Turn {} not found in execution {}",
                    turn_id, execution_id
                )))
            }
        })
        .await
    }

    async fn load_clarification_session(
        &self,
        execution_id: &str,
    ) -> Result<Option<Self::ClarificationSession>, V2StorageError> {
        let doc = self.load_execution_document(execution_id).await?;
        Ok(doc.clarification_session)
    }

    async fn store_clarification_session(
        &self,
        execution_id: &str,
        session: Self::ClarificationSession,
    ) -> Result<(), V2StorageError> {
        self.with_execution_document(execution_id, |doc| {
            doc.clarification_session = Some(session.clone());
            doc.execution.updated_at = chrono::Utc::now().timestamp_millis();
            Ok(())
        })
        .await
    }

    async fn delete_clarification_session(&self, execution_id: &str) -> Result<(), V2StorageError> {
        self.with_execution_document(execution_id, |doc| {
            if doc.clarification_session.is_some() {
                doc.clarification_session = None;
                doc.execution.updated_at = chrono::Utc::now().timestamp_millis();
            }
            Ok(())
        })
        .await
    }

    async fn append_clarification_history(
        &self,
        execution_id: &str,
        entries: Vec<ClarificationHistoryEntry>,
    ) -> Result<(), V2StorageError> {
        if entries.is_empty() {
            return Ok(());
        }

        self.with_execution_document(execution_id, |doc| {
            doc.clarification_history.extend(entries.clone());
            doc.execution.updated_at = chrono::Utc::now().timestamp_millis();
            Ok(())
        })
        .await
    }

    async fn get_clarification_history(
        &self,
        execution_id: &str,
    ) -> Result<Vec<ClarificationHistoryEntry>, V2StorageError> {
        let doc = self.load_execution_document(execution_id).await?;
        Ok(doc.clarification_history)
    }
}

impl Drop for CrossProcessExecutionGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn execution_index_tracks_status_and_owner_updates() {
        let temp = tempdir().unwrap();
        let store = FileV2Store::new(temp.path());

        let execution = store
            .create_execution_with_params(CreateExecutionParams::root(
                "principal",
                "workspace",
                Some("Execution".to_string()),
                "personal-assistant",
            ))
            .await
            .unwrap();

        store
            .update_execution_status(&execution.id, WaitingState::WaitingUser)
            .await
            .unwrap();
        store
            .update_execution_metadata(&execution.id, |execution| {
                execution.active_owner_agent_id = "keka-clocker".to_string();
            })
            .await
            .unwrap();

        let index = store.load_execution_index().await.unwrap();
        let entry = index.entries.get(&execution.id).unwrap();
        assert_eq!(entry.waiting_state, WaitingState::WaitingUser);
        assert_eq!(entry.active_owner_agent_id, "keka-clocker");
    }

    #[tokio::test]
    async fn execution_status_compare_exchange_never_overwrites_terminal_winner() {
        let temp = tempdir().unwrap();
        let store = FileV2Store::new(temp.path());
        let execution = store
            .create_execution_with_params(CreateExecutionParams::root(
                "principal",
                "workspace",
                Some("Execution".to_string()),
                "personal-assistant",
            ))
            .await
            .unwrap();
        store
            .update_execution_status(&execution.id, WaitingState::Cancelled)
            .await
            .unwrap();

        assert!(!store
            .compare_exchange_execution_status(
                &execution.id,
                WaitingState::Paused,
                WaitingState::Executing,
            )
            .await
            .unwrap());
        assert_eq!(
            store
                .get_execution(&execution.id)
                .await
                .unwrap()
                .waiting_state,
            WaitingState::Cancelled
        );
    }

    #[tokio::test]
    async fn execution_status_revision_compare_exchange_rejects_paused_aba() {
        let temp = tempdir().unwrap();
        let store = FileV2Store::new(temp.path());
        let execution = store
            .create_execution_with_params(CreateExecutionParams::root(
                "principal",
                "workspace",
                Some("Execution".to_string()),
                "personal-assistant",
            ))
            .await
            .unwrap();
        store
            .update_execution_status(&execution.id, WaitingState::Paused)
            .await
            .unwrap();
        let first_pause_revision = store
            .get_execution_status_revision(&execution.id)
            .await
            .unwrap();
        store
            .update_execution_status(&execution.id, WaitingState::WaitingUser)
            .await
            .unwrap();
        store
            .update_execution_status(&execution.id, WaitingState::Paused)
            .await
            .unwrap();

        assert!(!store
            .compare_exchange_execution_status_at(
                &execution.id,
                WaitingState::Paused,
                first_pause_revision,
                WaitingState::WaitingChildren,
            )
            .await
            .unwrap());
        assert_eq!(
            store
                .get_execution(&execution.id)
                .await
                .unwrap()
                .waiting_state,
            WaitingState::Paused
        );
    }

    #[tokio::test]
    async fn execution_status_revision_ignores_incidental_metadata_updates() {
        let temp = tempdir().unwrap();
        let store = FileV2Store::new(temp.path());
        let execution = store
            .create_execution_with_params(CreateExecutionParams::root(
                "principal",
                "workspace",
                Some("Execution".to_string()),
                "personal-assistant",
            ))
            .await
            .unwrap();
        store
            .update_execution_status(&execution.id, WaitingState::Paused)
            .await
            .unwrap();
        let paused_revision = store
            .get_execution_status_revision(&execution.id)
            .await
            .unwrap();
        store
            .update_processing_correlation_id(&execution.id, Some("correlation".to_string()))
            .await
            .unwrap();

        assert_eq!(
            store
                .get_execution_status_revision(&execution.id)
                .await
                .unwrap(),
            paused_revision
        );
        assert!(store
            .compare_exchange_execution_status_at(
                &execution.id,
                WaitingState::Paused,
                paused_revision,
                WaitingState::WaitingChildren,
            )
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn rebuild_execution_index_preserves_executing_lifecycle_authority() {
        let temp = tempdir().unwrap();
        let store = FileV2Store::new(temp.path());

        let execution = store
            .create_execution_with_params(CreateExecutionParams::root(
                "principal",
                "workspace",
                Some("Execution".to_string()),
                "personal-assistant",
            ))
            .await
            .unwrap();

        store
            .update_execution_status(&execution.id, WaitingState::Executing)
            .await
            .unwrap();

        let restarted_store = FileV2Store::new(temp.path());
        let index = restarted_store.rebuild_execution_index().await.unwrap();
        let entry = index.entries.get(&execution.id).unwrap();
        assert_eq!(entry.waiting_state, WaitingState::Executing);

        let recovered_execution = restarted_store.get_execution(&execution.id).await.unwrap();
        assert_eq!(recovered_execution.waiting_state, WaitingState::Executing);
    }

    #[tokio::test]
    async fn rebuild_execution_index_recovers_internal_task_executions() {
        let temp = tempdir().unwrap();
        let store = FileV2Store::new(temp.path());
        let task_id = "task-internal";
        let execution_id = "exec-internal";

        store
            .artifact_workspace
            .ensure_task_workspace_for_lifecycle(
                "principal",
                "workspace",
                task_id,
                crate::magician_v2::artifact_v2::models::TaskLifecycle::Internal,
            )
            .await
            .unwrap();

        let execution = store
            .create_execution_with_params(
                CreateExecutionParams::root(
                    "principal",
                    "workspace",
                    Some("Internal execution".to_string()),
                    "web-researcher",
                )
                .with_execution_id(execution_id)
                .with_execution_scope(Some(task_id.to_string()), Some(execution_id.to_string())),
            )
            .await
            .unwrap();
        store
            .update_execution_status(&execution.id, WaitingState::Executing)
            .await
            .unwrap();

        // A restarted store has no cached path. Both index rebuilding and the
        // subsequent direct lookup must rediscover the execution below
        // `internal_tasks/` without claiming lifecycle authority during index
        // construction.
        let restarted_store = FileV2Store::new(temp.path());
        let index = restarted_store.rebuild_execution_index().await.unwrap();
        let entry = index.entries.get(execution_id).unwrap();
        assert_eq!(entry.waiting_state, WaitingState::Executing);
        assert_eq!(entry.active_owner_agent_id, "web-researcher");

        let recovered_execution = restarted_store.get_execution(execution_id).await.unwrap();
        assert_eq!(recovered_execution.task_id.as_deref(), Some(task_id));
        assert_eq!(recovered_execution.waiting_state, WaitingState::Executing);

        let page = restarted_store
            .list_executions(
                "principal",
                "workspace",
                PaginationParams::new(Some(200), Some(0)),
            )
            .await
            .unwrap();
        assert!(page.items.iter().any(|item| item.id == execution_id));
    }

    #[tokio::test]
    async fn bind_execution_scope_rejects_conflicting_rebinds() {
        let temp = tempdir().unwrap();
        let store = FileV2Store::new(temp.path());

        let execution = store
            .create_execution_with_params(CreateExecutionParams::root(
                "principal",
                "workspace",
                Some("Execution".to_string()),
                "personal-assistant",
            ))
            .await
            .unwrap();

        store
            .artifact_workspace
            .ensure_task_workspace_for_lifecycle(
                "principal",
                "workspace",
                "task-a",
                crate::magician_v2::artifact_v2::models::TaskLifecycle::Persistent,
            )
            .await
            .unwrap();

        store
            .bind_execution_scope(
                &execution.id,
                Some("task-a".to_string()),
                Some("exec-root-a".to_string()),
            )
            .await
            .expect("initial bind should succeed");

        let task_err = store
            .bind_execution_scope(
                &execution.id,
                Some("task-b".to_string()),
                Some("exec-root-a".to_string()),
            )
            .await
            .expect_err("rebinding to a different task should fail");
        assert!(task_err.to_string().contains("already belongs to task"));

        let root_err = store
            .bind_execution_scope(
                &execution.id,
                Some("task-a".to_string()),
                Some("exec-root-b".to_string()),
            )
            .await
            .expect_err("rebinding to a different root execution should fail");
        assert!(root_err
            .to_string()
            .contains("already belongs to root execution"));
    }

    /// A reader that lands between two writes must see a whole document and no
    /// staging file.
    ///
    /// Deliberately sequential: every writer of `execution.json` goes through
    /// `with_execution_document`, which holds a per-execution mutex *and* an
    /// exclusive cross-process flock, so two concurrent writers would exercise
    /// the lock rather than the staging-file name. What this pins is that the
    /// durable writer's temp never survives a successful publish.
    #[tokio::test]
    async fn execution_document_writes_leave_no_staging_file() {
        let temp = tempdir().unwrap();
        let store = FileV2Store::new(temp.path());

        let execution = store
            .create_execution_with_params(CreateExecutionParams::root(
                "principal",
                "workspace",
                Some("Execution".to_string()),
                "personal-assistant",
            ))
            .await
            .unwrap();

        for state in [
            WaitingState::WaitingUser,
            WaitingState::Runnable,
            WaitingState::Executing,
        ] {
            store
                .update_execution_status(&execution.id, state)
                .await
                .unwrap();
        }

        let execution_dir = store
            .locate_v3_execution_dir(&execution.id)
            .await
            .unwrap()
            .expect("execution directory should exist");

        let staging: Vec<String> = std::fs::read_dir(&execution_dir)
            .expect("execution directory listing")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(
            staging.is_empty(),
            "durable writes must leave no staging file, found {staging:?}"
        );

        let published = std::fs::read_to_string(execution_dir.join("execution.json"))
            .expect("execution document should be readable");
        let doc: ExecutionRunDocument =
            serde_json::from_str(&published).expect("published document should parse");
        assert_eq!(doc.execution.id, execution.id);
        assert_eq!(doc.execution.waiting_state, WaitingState::Executing);
    }
}
