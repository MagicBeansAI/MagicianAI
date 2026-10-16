use std::collections::HashMap;

use chrono::Utc;
use thiserror::Error;

use crate::magician_v2::agents::AgentStorage;
use crate::magician_v2::json_traversal::{
    discard_json_iteratively, inspect_json_bounded, MAX_RETAINED_JSON_DEPTH,
};
use crate::magician_v2::pipeline::artifact::{AgentArtifact, ArtifactStore};

use super::{service::ScopeRef, workspace::ArtifactV2Workspace};

#[derive(Debug, Error)]
pub enum ExecutionPipelineStoreError {
    #[error("pipeline store not found for execution: {0}")]
    NotFound(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("invalid pipeline store: {0}")]
    InvalidData(String),
}

const MAX_PIPELINE_STORE_BYTES: usize = 64 * 1024 * 1024;
const MAX_PIPELINE_STORE_ARTIFACTS: usize = 100_000;
const MAX_PIPELINE_STORE_CONTENT_NODES: usize = 1_000_000;
const MAX_PIPELINE_STORE_DOCUMENT_NODES: usize = 2_000_000;
const MAX_PIPELINE_STORE_DOCUMENT_DEPTH: usize = MAX_RETAINED_JSON_DEPTH + 4;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ExecutionPipelineStoreDocument {
    execution_id: String,
    artifacts: HashMap<String, AgentArtifact>,
    saved_at: chrono::DateTime<chrono::Utc>,
}

fn cleanup_pipeline_store_document(doc: &mut ExecutionPipelineStoreDocument) {
    for artifact in doc.artifacts.values_mut() {
        discard_json_iteratively(std::mem::replace(
            &mut artifact.content,
            serde_json::Value::Null,
        ));
    }
    doc.artifacts.clear();
}

fn map_workspace_error(
    error: super::service::ArtifactV2Error,
    execution_id: Option<&str>,
) -> ExecutionPipelineStoreError {
    match error {
        super::service::ArtifactV2Error::Io(error)
            if execution_id.is_some() && error.kind() == std::io::ErrorKind::NotFound =>
        {
            ExecutionPipelineStoreError::NotFound(execution_id.unwrap_or_default().to_string())
        },
        super::service::ArtifactV2Error::Io(error) => ExecutionPipelineStoreError::Io(error),
        super::service::ArtifactV2Error::Serde(error) => {
            ExecutionPipelineStoreError::Serialization(error)
        },
        super::service::ArtifactV2Error::InvalidRequest(message) => {
            ExecutionPipelineStoreError::InvalidData(message)
        },
        other => ExecutionPipelineStoreError::Io(std::io::Error::other(other.to_string())),
    }
}

fn admit_pipeline_store(store: &ArtifactStore) -> Result<(), ExecutionPipelineStoreError> {
    admit_pipeline_store_with_limits(
        store,
        MAX_PIPELINE_STORE_ARTIFACTS,
        MAX_PIPELINE_STORE_CONTENT_NODES,
        MAX_RETAINED_JSON_DEPTH,
    )
}

fn admit_pipeline_store_with_limits(
    store: &ArtifactStore,
    max_artifacts: usize,
    max_content_nodes: usize,
    max_content_depth: usize,
) -> Result<(), ExecutionPipelineStoreError> {
    if store.len() > max_artifacts {
        return Err(ExecutionPipelineStoreError::InvalidData(format!(
            "artifact count exceeds {max_artifacts}"
        )));
    }
    let mut remaining_nodes = max_content_nodes;
    for (_, artifact) in store.iter() {
        let metrics =
            inspect_json_bounded(&artifact.content, remaining_nodes).ok_or_else(|| {
                ExecutionPipelineStoreError::InvalidData(format!(
                    "artifact content exceeds {max_content_nodes} total JSON nodes"
                ))
            })?;
        if metrics.max_depth > max_content_depth {
            return Err(ExecutionPipelineStoreError::InvalidData(format!(
                "artifact content exceeds JSON depth {max_content_depth}"
            )));
        }
        remaining_nodes = remaining_nodes.saturating_sub(metrics.nodes);
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct FilesystemExecutionPipelineStore {
    workspace: ArtifactV2Workspace,
}

impl FilesystemExecutionPipelineStore {
    pub fn new(workspace: ArtifactV2Workspace) -> Self {
        Self { workspace }
    }

    fn store_path(
        &self,
        scope: &ScopeRef,
        task_id: Option<&str>,
        execution_id: &str,
    ) -> std::path::PathBuf {
        self.workspace.execution_pipeline_store_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
            execution_id,
        )
    }

    pub async fn save_store(
        &self,
        scope: &ScopeRef,
        task_id: Option<&str>,
        execution_id: &str,
        store: &ArtifactStore,
    ) -> Result<(), ExecutionPipelineStoreError> {
        self.workspace
            .ensure_runtime_execution_workspace(
                &scope.principal(),
                &scope.workspace(),
                task_id,
                execution_id,
            )
            .await
            .map_err(|error| map_workspace_error(error, None))?;

        // Admission precedes both snapshot cloning and generic Serde. The
        // shallow envelope can then use Serde safely while content copying
        // itself remains heap-framed.
        admit_pipeline_store(store)?;

        let path = self.store_path(scope, task_id, execution_id);
        let _store_guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(|error| {
                ExecutionPipelineStoreError::Io(std::io::Error::other(format!(
                    "pipeline-store mutation lock failed: {error}"
                )))
            })?;

        let doc = ExecutionPipelineStoreDocument {
            execution_id: execution_id.to_string(),
            artifacts: store.snapshot(),
            saved_at: Utc::now(),
        };

        self.workspace
            .write_json_pretty_atomic_stream_path_with_cleanup(
                path,
                doc,
                MAX_PIPELINE_STORE_BYTES,
                cleanup_pipeline_store_document,
            )
            .await
            .map_err(|error| map_workspace_error(error, None))
    }

    pub async fn load_store(
        &self,
        scope: &ScopeRef,
        task_id: Option<&str>,
        execution_id: &str,
    ) -> Result<ArtifactStore, ExecutionPipelineStoreError> {
        let path = self.store_path(scope, task_id, execution_id);
        let mut doc = self
            .workspace
            .read_json_bounded_stream_path_with_cleanup_on_error(
                &path,
                MAX_PIPELINE_STORE_BYTES as u64,
                MAX_PIPELINE_STORE_DOCUMENT_DEPTH,
                MAX_PIPELINE_STORE_DOCUMENT_NODES,
                cleanup_pipeline_store_document,
            )
            .await
            .map_err(|error| map_workspace_error(error, Some(execution_id)))?;
        if doc.execution_id != execution_id {
            cleanup_pipeline_store_document(&mut doc);
            return Err(ExecutionPipelineStoreError::InvalidData(
                "embedded execution identity does not match its authoritative path".to_string(),
            ));
        }
        let artifacts = std::mem::take(&mut doc.artifacts);
        let restored = ArtifactStore::restore_from_snapshot(execution_id.to_string(), artifacts);
        // The encoded document guard protects Serde itself. Reapply the
        // durable content contract after decoding so a hand-authored file
        // cannot load a store that the save path would refuse to persist.
        admit_pipeline_store(&restored)?;
        Ok(restored)
    }

    pub async fn recover_corrupt_store(
        &self,
        scope: &ScopeRef,
        task_id: Option<&str>,
        execution_id: &str,
    ) -> Result<ArtifactStore, ExecutionPipelineStoreError> {
        let path = self.store_path(scope, task_id, execution_id);
        let _store_guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(|error| {
                ExecutionPipelineStoreError::Io(std::io::Error::other(format!(
                    "pipeline-store mutation lock failed: {error}"
                )))
            })?;
        // The failed load that requested this recovery happened before this
        // mutation lock was acquired. Another owner may have published a
        // healthy replacement in that interval. Revalidate under the lock so
        // a stale recovery attempt can never rename a newly committed store.
        match self.load_store(scope, task_id, execution_id).await {
            Ok(repaired) => return Ok(repaired),
            Err(ExecutionPipelineStoreError::NotFound(_)) => {
                return Ok(ArtifactStore::new(execution_id.to_string()));
            },
            Err(ExecutionPipelineStoreError::Serialization(_))
            | Err(ExecutionPipelineStoreError::InvalidData(_)) => {},
            Err(error) => return Err(error),
        }

        let corrupt_path =
            path.with_extension(format!("json.corrupt.{}", Utc::now().timestamp_millis()));
        let workspace = self.workspace.clone();
        tokio::task::spawn_blocking(move || workspace.rename_path_sync(path, corrupt_path))
            .await
            .map_err(|error| {
                ExecutionPipelineStoreError::Io(std::io::Error::other(format!(
                    "pipeline-store backup worker failed to join: {error}"
                )))
            })?
            .map_err(|error| map_workspace_error(error, None))?;
        Ok(ArtifactStore::new(execution_id.to_string()))
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::artifact_v2::models::TaskLifecycle;
    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use crate::magician_v2::pipeline::artifact::{AgentArtifact, ArtifactType};
    use serde_json::json;

    fn sample_scope() -> ScopeRef {
        ScopeRef::system_internal_unauthenticated(
            &"principal".to_string(),
            &"workspace".to_string(),
        )
    }

    fn sample_store(chain_id: &str) -> ArtifactStore {
        let mut store = ArtifactStore::new(chain_id.to_string());
        store.put(AgentArtifact {
            artifact_id: "artifact-1".to_string(),
            artifact_type: ArtifactType::Custom("sample".to_string()),
            producer_agent_id: "agent".to_string(),
            producer_cycle_id: "cycle".to_string(),
            content: json!({"value": 1}),
            schema_version: 1,
            produced_at: Utc::now(),
            render_hints: None,
        });
        store
    }

    #[tokio::test]
    async fn round_trip_task_scoped_pipeline_store() {
        let temp_dir = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(temp_dir.path().join("magician_data_v3"));
        let scope = sample_scope();
        workspace
            .ensure_task_workspace_for_lifecycle(
                &scope.principal(),
                &scope.workspace(),
                "task-1",
                TaskLifecycle::Persistent,
            )
            .await
            .unwrap();
        let store = FilesystemExecutionPipelineStore::new(workspace.clone());
        let source = sample_store("exec-1");

        store
            .save_store(&scope, Some("task-1"), "exec-1", &source)
            .await
            .unwrap();
        let persisted_path = store.store_path(&scope, Some("task-1"), "exec-1");
        let persisted = workspace.read_path(&persisted_path).await.unwrap();
        let decoded: ExecutionPipelineStoreDocument = serde_json::from_slice(&persisted).unwrap();
        assert_eq!(persisted, serde_json::to_vec_pretty(&decoded).unwrap());
        let restored = store
            .load_store(&scope, Some("task-1"), "exec-1")
            .await
            .unwrap();

        assert!(restored.get("artifact-1").is_some());
    }

    #[tokio::test]
    async fn round_trip_scope_scoped_pipeline_store_without_task() {
        let temp_dir = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(temp_dir.path().join("magician_data_v3"));
        let store = FilesystemExecutionPipelineStore::new(workspace.clone());
        let scope = sample_scope();
        let source = sample_store("exec-2");

        store
            .save_store(&scope, None, "exec-2", &source)
            .await
            .unwrap();
        let restored = store.load_store(&scope, None, "exec-2").await.unwrap();

        assert!(restored.get("artifact-1").is_some());
        assert!(workspace
            .execution_pipeline_store_path(&scope.principal(), &scope.workspace(), None, "exec-2")
            .exists());
    }

    #[test]
    fn loaded_store_reapplies_the_same_content_admission_as_save() {
        let mut store = sample_store("load-admission");
        store.put(AgentArtifact {
            artifact_id: "artifact-2".to_string(),
            artifact_type: ArtifactType::Custom("sample".to_string()),
            producer_agent_id: "agent".to_string(),
            producer_cycle_id: "cycle".to_string(),
            content: json!([null, null]),
            schema_version: 1,
            produced_at: Utc::now(),
            render_hints: None,
        });

        assert!(admit_pipeline_store_with_limits(&store, 2, 5, 1).is_ok());
        assert!(matches!(
            admit_pipeline_store_with_limits(&store, 1, 5, 1),
            Err(ExecutionPipelineStoreError::InvalidData(_))
        ));
        assert!(matches!(
            admit_pipeline_store_with_limits(&store, 2, 4, 1),
            Err(ExecutionPipelineStoreError::InvalidData(_))
        ));
        assert!(matches!(
            admit_pipeline_store_with_limits(&store, 2, 5, 0),
            Err(ExecutionPipelineStoreError::InvalidData(_))
        ));
    }

    #[tokio::test]
    async fn save_and_corrupt_recovery_share_one_store_mutation_lock() {
        let temp_dir = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(temp_dir.path().join("magician_data_v3"));
        let persistence = FilesystemExecutionPipelineStore::new(workspace.clone());
        let scope = sample_scope();
        let path = persistence.store_path(&scope, None, "locked-exec");

        let external_guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .expect("external store lock");
        let save_persistence = persistence.clone();
        let save_scope = scope.clone();
        let save = tokio::spawn(async move {
            save_persistence
                .save_store(
                    &save_scope,
                    None,
                    "locked-exec",
                    &sample_store("locked-exec"),
                )
                .await
        });
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        assert!(
            !save.is_finished(),
            "save must wait behind the store mutation lock"
        );
        drop(external_guard);
        save.await.expect("save task joins").expect("save succeeds");

        workspace
            .write_path(&path, b"corrupt-after-save")
            .await
            .expect("replace saved store with corrupt bytes");

        let external_guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .expect("external store lock for backup");
        let backup_persistence = persistence.clone();
        let backup_scope = scope.clone();
        let backup = tokio::spawn(async move {
            backup_persistence
                .recover_corrupt_store(&backup_scope, None, "locked-exec")
                .await
        });
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        assert!(
            !backup.is_finished(),
            "backup must wait behind the same store mutation lock"
        );
        drop(external_guard);
        let recovered = backup
            .await
            .expect("backup task joins")
            .expect("backup succeeds");
        assert!(recovered.is_empty());
        assert!(workspace.metadata_path(&path).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn stale_corrupt_recovery_returns_the_concurrently_repaired_store() {
        let temp_dir = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(temp_dir.path().join("magician_data_v3"));
        let persistence = FilesystemExecutionPipelineStore::new(workspace.clone());
        let scope = sample_scope();
        let path = persistence.store_path(&scope, None, "repaired-exec");

        workspace
            .ensure_runtime_execution_workspace(
                &scope.principal(),
                &scope.workspace(),
                None,
                "repaired-exec",
            )
            .await
            .expect("execution workspace");
        workspace
            .write_path(&path, b"corrupt-before-repair")
            .await
            .expect("seed corrupt store");
        persistence
            .save_store(
                &scope,
                None,
                "repaired-exec",
                &sample_store("repaired-exec"),
            )
            .await
            .expect("publish repaired store");

        let recovered = persistence
            .recover_corrupt_store(&scope, None, "repaired-exec")
            .await
            .expect("stale backup request becomes a no-op");

        assert!(recovered.get("artifact-1").is_some());
        let parent = path.parent().unwrap();
        assert!(!std::fs::read_dir(parent)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .any(|candidate| candidate
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("store.json.corrupt."))));
    }

    #[test]
    fn deep_artifact_is_rejected_before_snapshot_serde_on_a_small_stack() {
        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                runtime.block_on(async {
                    let temp_dir = tempfile::tempdir().unwrap();
                    let workspace =
                        ArtifactV2Workspace::new(temp_dir.path().join("magician_data_v3"));
                    let persistence = FilesystemExecutionPipelineStore::new(workspace);
                    let scope = sample_scope();
                    let mut source = ArtifactStore::new("deep-exec");
                    let mut content = serde_json::Value::Null;
                    for _ in 0..10_000 {
                        content = serde_json::Value::Array(vec![content]);
                    }
                    source.put(AgentArtifact {
                        artifact_id: "deep-artifact".to_string(),
                        artifact_type: ArtifactType::Custom("deep".to_string()),
                        producer_agent_id: "agent".to_string(),
                        producer_cycle_id: "cycle".to_string(),
                        content,
                        schema_version: 1,
                        produced_at: Utc::now(),
                        render_hints: None,
                    });

                    let error = persistence
                        .save_store(&scope, None, "deep-exec", &source)
                        .await
                        .expect_err("deep artifact must fail before generic Serde");
                    assert!(matches!(error, ExecutionPipelineStoreError::InvalidData(_)));
                });
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn deep_encoded_pipeline_store_is_rejected_before_typed_decode() {
        std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                runtime.block_on(async {
                    let temp_dir = tempfile::tempdir().unwrap();
                    let workspace =
                        ArtifactV2Workspace::new(temp_dir.path().join("magician_data_v3"));
                    let persistence = FilesystemExecutionPipelineStore::new(workspace.clone());
                    let scope = sample_scope();
                    let path = persistence.store_path(&scope, None, "deep-load-exec");
                    let mut body = String::from(
                        r#"{"execution_id":"deep-load-exec","artifacts":{"a":{"artifact_id":"a","artifact_type":"custom:deep","producer_agent_id":"agent","producer_cycle_id":"cycle","content":"#,
                    );
                    body.extend(std::iter::repeat_n('[', 10_000));
                    body.push_str("null");
                    body.extend(std::iter::repeat_n(']', 10_000));
                    body.push_str(
                        r#","schema_version":1,"produced_at":"2026-08-05T00:00:00Z"}},"saved_at":"2026-08-05T00:00:00Z"}"#,
                    );
                    workspace.write_path(&path, body.as_bytes()).await.unwrap();

                    let error = persistence
                        .load_store(&scope, None, "deep-load-exec")
                        .await
                        .expect_err("encoded depth must fail before typed allocation");
                    assert!(matches!(error, ExecutionPipelineStoreError::InvalidData(_)));
                });
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[tokio::test]
    async fn corrupt_backup_renames_the_exact_file_without_a_read_copy() {
        let temp_dir = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(temp_dir.path().join("magician_data_v3"));
        let persistence = FilesystemExecutionPipelineStore::new(workspace.clone());
        let scope = sample_scope();
        let path = persistence.store_path(&scope, None, "corrupt-exec");
        let original = b"not-json-but-byte-exact";
        workspace
            .ensure_runtime_execution_workspace(
                &scope.principal(),
                &scope.workspace(),
                None,
                "corrupt-exec",
            )
            .await
            .unwrap();
        workspace.write_path(&path, original).await.unwrap();

        let recovered = persistence
            .recover_corrupt_store(&scope, None, "corrupt-exec")
            .await
            .unwrap();
        assert!(recovered.is_empty());

        assert!(workspace.metadata_path(&path).await.unwrap().is_none());
        let parent = path.parent().unwrap();
        let backup = std::fs::read_dir(parent)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|candidate| {
                candidate
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("store.json.corrupt."))
            })
            .expect("renamed corrupt store");
        assert_eq!(workspace.read_path(&backup).await.unwrap(), original);
    }

    #[tokio::test]
    async fn load_rejects_an_embedded_execution_identity_from_another_path() {
        let temp_dir = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(temp_dir.path().join("magician_data_v3"));
        let persistence = FilesystemExecutionPipelineStore::new(workspace.clone());
        let scope = sample_scope();
        let path = persistence.store_path(&scope, None, "path-exec");
        let document = ExecutionPipelineStoreDocument {
            execution_id: "other-exec".to_string(),
            artifacts: sample_store("other-exec").snapshot(),
            saved_at: Utc::now(),
        };
        workspace
            .write_json_atomic_path(&path, &document)
            .await
            .unwrap();

        let error = persistence
            .load_store(&scope, None, "path-exec")
            .await
            .expect_err("path identity is authoritative");
        assert!(matches!(error, ExecutionPipelineStoreError::InvalidData(_)));
    }
}
