use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use tokio::sync::RwLock;

use crate::magician_v2::{
    artifact_v2::{ArtifactV2Service, ScopeRef, V3ReadApi},
    orchestrator::v2_orchestrator::MagicianV2Orchestrator,
};

use crate::magician_v2::progress_channel_seam::{
    storage::ProgressChannelStorage,
    types::{ExecutionLineage, ProgressMessage},
};

/// Execution-id → lineage cache, persisted as one rebuildable index per scope.
///
/// Writes are coalesced. An `upsert` only marks the index dirty; the router's
/// flush loop turns however many upserts happened into a single write. Before
/// that, every message carrying an execution id rewrote the whole map: an
/// `O(N)` clone plus an `O(N)` serialize plus two disk barriers, with `N`
/// growing for the life of the scope — 1,186 entries and 405 KB on the live
/// store, per progress event.
///
/// What a crash between the last upsert and the next flush loses is entries
/// for executions that ran inside that window. That is acceptable because this
/// is a cache, not a record: `resolve_or_load` rebuilds a missing entry from
/// the orchestrator and the V3 service on first use, which is the same path
/// that populated it originally.
#[derive(Clone)]
pub struct ExecutionLineageIndex {
    storage: ProgressChannelStorage,
    entries: Arc<RwLock<HashMap<String, ExecutionLineage>>>,
    dirty: Arc<AtomicBool>,
}

impl ExecutionLineageIndex {
    pub async fn load(storage: ProgressChannelStorage) -> anyhow::Result<Self> {
        let entries = storage.load_lineage().await?;
        Ok(Self {
            storage,
            entries: Arc::new(RwLock::new(entries)),
            dirty: Arc::new(AtomicBool::new(false)),
        })
    }

    pub async fn get(&self, execution_id: &str) -> Option<ExecutionLineage> {
        self.entries.read().await.get(execution_id).cloned()
    }

    /// Record a lineage entry and mark the index for the next flush.
    ///
    /// The in-memory map is authoritative for every reader in this process, so
    /// deferring the write changes nothing a caller can observe while the
    /// process lives.
    pub async fn upsert(
        &self,
        execution_id: impl Into<String>,
        lineage: ExecutionLineage,
    ) -> anyhow::Result<()> {
        {
            self.entries
                .write()
                .await
                .insert(execution_id.into(), lineage);
        }
        self.dirty.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Write the index if anything changed since the last flush.
    ///
    /// A failed write leaves the index dirty so the next tick retries it
    /// rather than dropping the accumulated entries.
    pub async fn flush_if_dirty(&self) -> anyhow::Result<()> {
        if !self.dirty.swap(false, Ordering::SeqCst) {
            return Ok(());
        }
        if let Err(error) = self.persist().await {
            self.dirty.store(true, Ordering::SeqCst);
            return Err(error);
        }
        Ok(())
    }

    pub async fn persist(&self) -> anyhow::Result<()> {
        let snapshot = self.entries.read().await.clone();
        self.storage.save_lineage(&snapshot).await
    }

    pub async fn resolve_or_load(
        &self,
        execution_id: &str,
        orchestrator: &Arc<MagicianV2Orchestrator>,
        v3_service: &Arc<ArtifactV2Service>,
    ) -> Option<ExecutionLineage> {
        if let Some(lineage) = self.get(execution_id).await {
            return Some(lineage);
        }

        let execution = orchestrator.get_execution(execution_id).await.ok()?;
        let mut task_id = execution.task_id.clone();
        let mut ui_thread_id = None;
        let mut principal = execution.principal.clone();
        let mut workspace = execution.workspace.clone();

        if let Some(task_id) = execution.task_id.as_deref() {
            let scope = ScopeRef::system_internal_unauthenticated(
                &execution.principal.clone(),
                &execution.workspace.clone(),
            );
            if let Ok(task) = v3_service.get_task(&scope, task_id).await {
                ui_thread_id = Some(task.manifest.ui_thread_id);
            }
        }
        if ui_thread_id.is_none() {
            if let Some(root_execution_id) = execution.root_execution_id.as_deref() {
                if let Ok(Some((scope, resolved_task_id, _))) =
                    v3_service.find_execution_scope(root_execution_id).await
                {
                    if let Ok(task) = v3_service.get_task(&scope, &resolved_task_id).await {
                        task_id = Some(resolved_task_id);
                        ui_thread_id = Some(task.manifest.ui_thread_id);
                        principal = scope.principal().to_string();
                        workspace = scope.workspace().to_string();
                    }
                }
            }
        }

        let lineage = ExecutionLineage {
            task_id,
            root_execution_id: execution
                .root_execution_id
                .clone()
                .or_else(|| Some(execution.id.clone())),
            parent_execution_id: execution.parent_execution_id.clone(),
            agent_id: Some(execution.active_owner_agent_id.clone()),
            ui_thread_id,
            step_id: None,
            principal,
            workspace,
            updated_at: chrono::Utc::now().timestamp_millis(),
        };

        let _ = self.upsert(execution_id.to_string(), lineage.clone()).await;
        Some(lineage)
    }

    pub async fn update_from_message(&self, message: &ProgressMessage) -> anyhow::Result<()> {
        let Some(execution_id) = message.execution_id.as_ref() else {
            return Ok(());
        };
        let lineage = ExecutionLineage {
            task_id: message
                .root_task_id
                .clone()
                .or_else(|| message.task_id.clone()),
            root_execution_id: message.root_execution_id.clone(),
            parent_execution_id: message.parent_execution_id.clone(),
            agent_id: message.agent_id.clone(),
            ui_thread_id: message.ui_thread_id.clone(),
            step_id: message.step_id.clone(),
            principal: message.principal.clone(),
            workspace: message.workspace.clone(),
            updated_at: chrono::Utc::now().timestamp_millis(),
        };
        self.upsert(execution_id.clone(), lineage).await
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use crate::magician_v2::progress_channel_seam::lineage::ExecutionLineageIndex;
    use crate::magician_v2::progress_channel_seam::storage::ProgressChannelStorage;
    use crate::magician_v2::progress_channel_seam::types::ExecutionLineage;
    use std::collections::HashMap;

    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

    fn lineage_for(task_id: &str) -> ExecutionLineage {
        ExecutionLineage {
            task_id: Some(task_id.to_string()),
            root_execution_id: None,
            parent_execution_id: None,
            agent_id: None,
            ui_thread_id: None,
            step_id: None,
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
            updated_at: 0,
        }
    }

    async fn index(root: &std::path::Path) -> (ExecutionLineageIndex, std::path::PathBuf) {
        let workspace_layout = ArtifactV2Workspace::new(root);
        let storage = ProgressChannelStorage::with_workspace_layout(workspace_layout.clone())
            .await
            .expect("progress storage");
        let path = workspace_layout.progress_lineage_path("anonymous", "default");
        let index = ExecutionLineageIndex::load(storage)
            .await
            .expect("lineage index");
        (index, path)
    }

    fn read_index(path: &std::path::Path) -> HashMap<String, ExecutionLineage> {
        serde_json::from_slice(&std::fs::read(path).expect("read lineage index"))
            .expect("parse lineage index")
    }

    #[tokio::test]
    async fn many_upserts_produce_one_write_at_the_flush_not_one_each() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (index, path) = index(tmp.path()).await;

        for entry in 0..64_u32 {
            index
                .upsert(
                    format!("exec-{entry}"),
                    lineage_for(&format!("task-{entry}")),
                )
                .await
                .expect("upsert");
        }
        assert!(
            !path.exists(),
            "an upsert must only mark the index dirty; the flush does the write"
        );

        index.flush_if_dirty().await.expect("flush");

        let written = read_index(&path);
        assert_eq!(written.len(), 64);
        assert_eq!(
            written
                .get("exec-63")
                .and_then(|entry| entry.task_id.as_deref()),
            Some("task-63")
        );
    }

    #[tokio::test]
    async fn the_flush_writes_what_the_last_upsert_left() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (index, path) = index(tmp.path()).await;

        index
            .upsert("exec-1", lineage_for("first"))
            .await
            .expect("first upsert");
        index
            .upsert("exec-1", lineage_for("last"))
            .await
            .expect("second upsert");
        index.flush_if_dirty().await.expect("flush");

        assert_eq!(
            read_index(&path)
                .get("exec-1")
                .and_then(|entry| entry.task_id.as_deref()),
            Some("last")
        );
    }

    #[tokio::test]
    async fn a_clean_index_does_not_write_again() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (index, path) = index(tmp.path()).await;

        index
            .upsert("exec-1", lineage_for("only"))
            .await
            .expect("upsert");
        index.flush_if_dirty().await.expect("first flush");
        std::fs::remove_file(&path).expect("remove written index");

        index.flush_if_dirty().await.expect("second flush");

        assert!(
            !path.exists(),
            "a flush with nothing dirty must not touch the disk"
        );
    }

    #[tokio::test]
    async fn the_in_memory_view_is_authoritative_before_any_flush() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (index, path) = index(tmp.path()).await;

        index
            .upsert("exec-1", lineage_for("pending"))
            .await
            .expect("upsert");

        assert!(!path.exists());
        assert_eq!(
            index.get("exec-1").await.and_then(|entry| entry.task_id),
            Some("pending".to_string()),
            "deferring the write must not change what a reader in this process sees"
        );
    }
}
