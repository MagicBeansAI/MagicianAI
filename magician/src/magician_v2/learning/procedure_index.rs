//! Durable derived index maintenance for active reusable procedures.
//!
//! Canonical procedure YAML remains the source of truth. This module keeps a
//! scope-local LanceDB index current outside prompt rendering, embedding only
//! procedure text whose fingerprint changed and deleting rows that are no
//! longer active.

use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::OnceLock,
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use magician_vector_index::{OllamaEmbedder, OllamaEmbedderConfig, VectorItem, VectorTable};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::magician_v2::artifact_v2::ArtifactV2Error;

use super::{
    procedure_prompt_blocks::procedure_hybrid_text, LearningProcedure, LearningProcedureFilters,
    LearningProcedureStatus, LearningScope, LearningStore,
};

// v2 binds every persisted procedure vector to the vector toolkit's complete
// semantic embedding contract (model, dimensions, logical context, physical
// batch ceiling, and input preprocessing version). This deliberately
// invalidates v1 manifests created before no-truncation fragmentation/pooling
// became part of that contract.
const PROCEDURE_INDEX_SCHEMA_VERSION: u32 = 2;
const PROCEDURE_INDEX_TEXT_VERSION: &str = "procedure-hybrid-text-v1";
const PROCEDURE_INDEX_QUEUE_CAPACITY: usize = 128;

static PROCEDURE_INDEX_QUEUE: OnceLock<mpsc::Sender<ProcedureIndexRefreshRequest>> =
    OnceLock::new();

#[derive(Debug, Clone)]
struct ProcedureIndexRefreshRequest {
    store: LearningStore,
    scope: LearningScope,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ProcedureIndexManifest {
    schema_version: u32,
    text_version: String,
    embedding_model: String,
    embedding_dimensions: usize,
    embedding_contract_id: String,
    entries: BTreeMap<String, String>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProcedureIndexDirtyMarker {
    generation: String,
    changed_at: DateTime<Utc>,
}

#[derive(Debug)]
enum ManifestState {
    Missing,
    Valid(ProcedureIndexManifest),
    Invalid(String),
}

#[derive(Debug)]
struct ProcedureIndexWritePlan {
    changed: Vec<LearningProcedure>,
    deleted_ids: Vec<String>,
    next_manifest: ProcedureIndexManifest,
    reset_table: bool,
}

/// Start the single process-wide procedure-index maintainer. Calling this more
/// than once is harmless; only the first caller installs a worker.
pub fn start_procedure_index_maintainer() {
    let (sender, mut receiver) = mpsc::channel(PROCEDURE_INDEX_QUEUE_CAPACITY);
    if PROCEDURE_INDEX_QUEUE.set(sender).is_err() {
        return;
    }

    tokio::spawn(async move {
        while let Some(first) = receiver.recv().await {
            let mut pending = HashMap::new();
            pending.insert(refresh_key(&first), first);
            while let Ok(next) = receiver.try_recv() {
                pending.insert(refresh_key(&next), next);
            }

            for request in pending.into_values() {
                if let Err(error) = refresh_procedure_index(&request.store, &request.scope).await {
                    warn!(
                        principal = %request.scope.principal,
                        workspace = %request.scope.workspace,
                        error = %error,
                        "Failed to refresh durable procedure index; canonical lexical retrieval remains available"
                    );
                }
            }
        }
    });
}

/// Mark the derived index stale after a canonical active-procedure mutation and
/// notify the background worker when the runtime has installed one.
pub(super) fn mark_procedure_index_dirty(store: &LearningStore, scope: &LearningScope) {
    let marker = ProcedureIndexDirtyMarker {
        generation: Uuid::new_v4().simple().to_string(),
        changed_at: Utc::now(),
    };
    let workspace = store.workspace_layout();
    let path = workspace.learning_procedure_index_dirty_path(&scope.principal, &scope.workspace);
    match serde_json::to_vec_pretty(&marker)
        .context("serializing procedure index dirty marker")
        .and_then(|body| {
            workspace
                .write_atomic_path_sync(&path, &body)
                .with_context(|| format!("writing procedure index dirty marker {}", path.display()))
        }) {
        Ok(()) => queue_procedure_index_refresh(store, scope),
        Err(error) => warn!(
            principal = %scope.principal,
            workspace = %scope.workspace,
            error = %error,
            "Failed to mark durable procedure index dirty; retrieval-time fingerprint checks will retry"
        ),
    }
}

pub(super) fn procedure_index_content_changed(
    before: &LearningProcedure,
    after: &LearningProcedure,
) -> bool {
    before.id != after.id || procedure_hybrid_text(before) != procedure_hybrid_text(after)
}

/// Queue maintenance when the persisted manifest is absent or no longer
/// matches the current active procedure corpus. This check is local and does
/// not contact the embedding provider.
pub(super) fn queue_procedure_index_refresh_if_stale(
    store: &LearningStore,
    scope: &LearningScope,
    active_procedures: &[LearningProcedure],
) -> bool {
    let config = OllamaEmbedderConfig::from_env();
    let stale = procedure_index_is_stale(store, scope, active_procedures, &config);
    if stale {
        queue_procedure_index_refresh(store, scope);
    }
    stale
}

pub(super) fn procedure_vector_table(store: &LearningStore, scope: &LearningScope) -> VectorTable {
    let config = OllamaEmbedderConfig::from_env();
    VectorTable::at(
        store
            .workspace_layout()
            .learning_procedure_lancedb_dir(&scope.principal, &scope.workspace),
        config.dims,
    )
}

pub(super) fn queue_procedure_index_refresh(store: &LearningStore, scope: &LearningScope) {
    let Some(sender) = PROCEDURE_INDEX_QUEUE.get() else {
        return;
    };
    let request = ProcedureIndexRefreshRequest {
        store: store.clone(),
        scope: scope.clone(),
    };
    if let Err(error) = sender.try_send(request) {
        debug!(
            principal = %scope.principal,
            workspace = %scope.workspace,
            error = %error,
            "Procedure index refresh queue is unavailable or full; dirty marker remains for retry"
        );
    }
}

fn refresh_key(request: &ProcedureIndexRefreshRequest) -> PathBuf {
    request
        .store
        .workspace_layout()
        .learning_procedure_index_dir(&request.scope.principal, &request.scope.workspace)
}

fn procedure_index_is_stale(
    store: &LearningStore,
    scope: &LearningScope,
    active_procedures: &[LearningProcedure],
    config: &OllamaEmbedderConfig,
) -> bool {
    let workspace = store.workspace_layout();
    let dirty_path =
        workspace.learning_procedure_index_dirty_path(&scope.principal, &scope.workspace);
    if workspace
        .metadata_path_sync(&dirty_path)
        .ok()
        .flatten()
        .is_some()
    {
        return true;
    }
    let table_dir = workspace.learning_procedure_lancedb_dir(&scope.principal, &scope.workspace);
    if !table_dir.exists() {
        return !active_procedures.is_empty();
    }
    let ManifestState::Valid(manifest) = read_manifest(store, scope) else {
        return true;
    };
    !manifest_contract_matches(&manifest, config)
        || manifest.entries != expected_fingerprints(active_procedures)
}

async fn refresh_procedure_index(store: &LearningStore, scope: &LearningScope) -> Result<()> {
    let load_store = store.clone();
    let load_scope = scope.clone();
    let active_procedures = tokio::task::spawn_blocking(move || {
        load_store.list_procedures(
            &load_scope,
            LearningProcedureFilters {
                status: Some(LearningProcedureStatus::Active.as_str().to_string()),
                owner_agent: None,
                limit: None,
            },
        )
    })
    .await
    .context("joining active procedure load for index refresh")??;

    let config = OllamaEmbedderConfig::from_env();
    let workspace = store.workspace_layout();
    let table_dir = workspace.learning_procedure_lancedb_dir(&scope.principal, &scope.workspace);
    let table = VectorTable::at(&table_dir, config.dims);
    let table_exists = match table.exists().await {
        Ok(exists) => exists,
        Err(error) => {
            warn!(
                principal = %scope.principal,
                workspace = %scope.workspace,
                path = %table_dir.display(),
                error = %error,
                "Procedure index table is unreadable; rebuilding the derived index"
            );
            remove_table_dir_if_present(&table_dir).await?;
            false
        },
    };
    let manifest_state = read_manifest(store, scope);
    let dirty_marker_body = read_dirty_marker_body(store, scope).ok().flatten();
    let plan = build_write_plan(&active_procedures, manifest_state, &config, table_exists);

    if plan.reset_table {
        remove_table_dir_if_present(&table_dir).await?;
    }
    let table = VectorTable::at(&table_dir, config.dims);
    if !plan.changed.is_empty() {
        let items = plan
            .changed
            .iter()
            .map(procedure_vector_item)
            .collect::<Vec<_>>();
        let embedder = OllamaEmbedder::new(config.clone());
        // Record this embed batch into the separate `llm_embeddings` dataset.
        // Best-effort/non-blocking: recording never affects the index write.
        let (principal, workspace, model, operation, batch_size, input_tokens) =
            procedure_index_embed_record_args(scope, &config, &plan.changed);
        let started = std::time::Instant::now();
        let index_result = table.index(&embedder, &items).await;
        let latency_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        crate::magician_v2::analytics::llm_embeddings_sink::record_embedding_batch(
            principal,
            workspace,
            model,
            operation,
            batch_size,
            input_tokens,
            latency_ms,
            index_result.is_ok(),
        );
        index_result.with_context(|| format!("indexing {} changed procedures", items.len()))?;
    }
    if !plan.deleted_ids.is_empty() {
        table
            .delete_ids(&plan.deleted_ids)
            .await
            .with_context(|| format!("deleting {} inactive procedures", plan.deleted_ids.len()))?;
    }

    write_manifest(store, scope, &plan.next_manifest)?;
    clear_dirty_marker_if_unchanged(store, scope, dirty_marker_body.as_deref())?;
    info!(
        principal = %scope.principal,
        workspace = %scope.workspace,
        indexed = plan.changed.len(),
        deleted = plan.deleted_ids.len(),
        active = plan.next_manifest.entries.len(),
        "Durable procedure index refresh completed"
    );
    Ok(())
}

fn build_write_plan(
    procedures: &[LearningProcedure],
    manifest_state: ManifestState,
    config: &OllamaEmbedderConfig,
    table_exists: bool,
) -> ProcedureIndexWritePlan {
    let entries = expected_fingerprints(procedures);
    let existing = match manifest_state {
        ManifestState::Valid(manifest) => Some(manifest),
        ManifestState::Missing => None,
        ManifestState::Invalid(error) => {
            warn!(error = %error, "Procedure index manifest is invalid; rebuilding derived index");
            None
        },
    };
    let reset_table = table_exists
        && existing
            .as_ref()
            .map(|manifest| !manifest_contract_matches(manifest, config))
            .unwrap_or(true);
    let reusable_manifest = existing.as_ref().filter(|_| table_exists && !reset_table);

    let changed = procedures
        .iter()
        .filter(|procedure| {
            reusable_manifest.and_then(|manifest| manifest.entries.get(&procedure.id))
                != entries.get(&procedure.id)
        })
        .cloned()
        .collect::<Vec<_>>();
    let deleted_ids = reusable_manifest
        .map(|manifest| {
            manifest
                .entries
                .keys()
                .filter(|id| !entries.contains_key(*id))
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    ProcedureIndexWritePlan {
        changed,
        deleted_ids,
        next_manifest: ProcedureIndexManifest {
            schema_version: PROCEDURE_INDEX_SCHEMA_VERSION,
            text_version: PROCEDURE_INDEX_TEXT_VERSION.to_string(),
            embedding_model: config.model.clone(),
            embedding_dimensions: config.dims,
            embedding_contract_id: config.embedding_contract_id(),
            entries,
            updated_at: Utc::now(),
        },
        reset_table,
    }
}

fn expected_fingerprints(procedures: &[LearningProcedure]) -> BTreeMap<String, String> {
    procedures
        .iter()
        .map(|procedure| {
            let mut hasher = Sha256::new();
            hasher.update(PROCEDURE_INDEX_TEXT_VERSION.as_bytes());
            hasher.update([0]);
            hasher.update(procedure_hybrid_text(procedure).as_bytes());
            (procedure.id.clone(), format!("{:x}", hasher.finalize()))
        })
        .collect()
}

fn manifest_contract_matches(
    manifest: &ProcedureIndexManifest,
    config: &OllamaEmbedderConfig,
) -> bool {
    manifest.schema_version == PROCEDURE_INDEX_SCHEMA_VERSION
        && manifest.text_version == PROCEDURE_INDEX_TEXT_VERSION
        && manifest.embedding_model == config.model
        && manifest.embedding_dimensions == config.dims
        && manifest.embedding_contract_id == config.embedding_contract_id()
}

/// Compute the embeddings-telemetry record args for a procedure-index write.
///
/// Kept as a small pure function so the mapping (operation tag, batch size =
/// count of changed procedures actually embedded, length-based token estimate)
/// is unit-testable without a live LanceDB/Ollama round trip. The caller pairs
/// this with a measured latency + success flag and forwards to
/// [`record_embedding_batch`](crate::magician_v2::analytics::llm_embeddings_sink::record_embedding_batch).
fn procedure_index_embed_record_args<'a>(
    scope: &'a LearningScope,
    config: &'a OllamaEmbedderConfig,
    changed: &[LearningProcedure],
) -> (&'a str, &'a str, &'a str, &'static str, usize, i64) {
    let texts = changed
        .iter()
        .map(procedure_hybrid_text)
        .collect::<Vec<_>>();
    let input_tokens =
        crate::magician_v2::analytics::llm_embeddings_sink::estimate_input_tokens(&texts);
    (
        scope.principal.as_str(),
        scope.workspace.as_str(),
        config.model.as_str(),
        "procedure_index",
        texts.len(),
        input_tokens,
    )
}

fn procedure_vector_item(procedure: &LearningProcedure) -> VectorItem {
    VectorItem {
        id: procedure.id.clone(),
        text: procedure_hybrid_text(procedure),
        metadata: json!({
            "title": procedure.title.clone(),
            "owner_agent": procedure.owner_agent.clone(),
        }),
    }
}

fn read_manifest(store: &LearningStore, scope: &LearningScope) -> ManifestState {
    let workspace = store.workspace_layout();
    let path = workspace.learning_procedure_index_manifest_path(&scope.principal, &scope.workspace);
    match workspace.read_to_string_path_sync(&path) {
        Ok(body) => match serde_json::from_str(&body) {
            Ok(manifest) => ManifestState::Valid(manifest),
            Err(error) => ManifestState::Invalid(error.to_string()),
        },
        Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            ManifestState::Missing
        },
        Err(error) => ManifestState::Invalid(error.to_string()),
    }
}

fn write_manifest(
    store: &LearningStore,
    scope: &LearningScope,
    manifest: &ProcedureIndexManifest,
) -> Result<()> {
    let workspace = store.workspace_layout();
    let path = workspace.learning_procedure_index_manifest_path(&scope.principal, &scope.workspace);
    let body =
        serde_json::to_vec_pretty(manifest).context("serializing procedure index manifest")?;
    workspace
        .write_atomic_path_sync(&path, &body)
        .with_context(|| format!("writing procedure index manifest {}", path.display()))
}

fn read_dirty_marker_body(store: &LearningStore, scope: &LearningScope) -> Result<Option<String>> {
    let workspace = store.workspace_layout();
    let path = workspace.learning_procedure_index_dirty_path(&scope.principal, &scope.workspace);
    match workspace.read_to_string_path_sync(&path) {
        Ok(body) => Ok(Some(body)),
        Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error)
            .with_context(|| format!("reading procedure index dirty marker {}", path.display())),
    }
}

fn clear_dirty_marker_if_unchanged(
    store: &LearningStore,
    scope: &LearningScope,
    refreshed_marker_body: Option<&str>,
) -> Result<()> {
    let Some(refreshed_marker_body) = refreshed_marker_body else {
        return Ok(());
    };
    let Some(current_marker_body) = read_dirty_marker_body(store, scope)? else {
        return Ok(());
    };
    if current_marker_body != refreshed_marker_body {
        return Ok(());
    }
    let workspace = store.workspace_layout();
    let path = workspace.learning_procedure_index_dirty_path(&scope.principal, &scope.workspace);
    match workspace.remove_file_path_sync(&path) {
        Ok(()) => Ok(()),
        Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("removing procedure index dirty marker {}", path.display())),
    }
}

async fn remove_table_dir_if_present(path: &PathBuf) -> Result<()> {
    match tokio::fs::remove_dir_all(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("removing derived procedure index {}", path.display())),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use serde_json::Value;
    use tempfile::TempDir;

    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

    use super::*;
    use crate::magician_v2::learning::LearningProcedureActivation;

    fn procedure(id: &str, workflow: &str) -> LearningProcedure {
        let now = Utc::now();
        LearningProcedure {
            id: id.to_string(),
            scope: LearningScope::new("anonymous", "default"),
            status: LearningProcedureStatus::Active,
            title: format!("Procedure {id}"),
            summary: format!("Reusable guidance for {id}"),
            owner_agent: None,
            activation: LearningProcedureActivation {
                use_when: vec![format!("when handling {id}")],
                avoid_when: Vec::new(),
                example_goals: Vec::new(),
            },
            workflow: vec![workflow.to_string()],
            decision_points: Vec::new(),
            verification: vec!["Verify the result".to_string()],
            failure_modes: Vec::new(),
            evidence_refs: Vec::new(),
            source_candidate_id: None,
            source_task_ids: Vec::new(),
            source_chat_session_ids: Vec::new(),
            success_count: 0,
            failure_count: 0,
            payload: Value::Null,
            created_at: now,
            updated_at: now,
            last_used_at: None,
            version: 1,
        }
    }

    fn embedding_config() -> OllamaEmbedderConfig {
        let mut config = OllamaEmbedderConfig::default();
        config.model = "test-embedder".to_string();
        config.dims = 2;
        config
    }

    #[test]
    fn write_plan_embeds_only_changed_text_and_deletes_inactive_rows() {
        let config = embedding_config();
        let first = procedure("first", "Inspect the source");
        let second = procedure("second", "Draft the response");
        let initial = build_write_plan(
            &[first.clone(), second.clone()],
            ManifestState::Missing,
            &config,
            false,
        );
        assert_eq!(initial.changed.len(), 2);
        assert!(initial.deleted_ids.is_empty());
        assert!(!initial.reset_table);

        let baseline = initial.next_manifest;
        let unchanged = build_write_plan(
            &[first.clone(), second.clone()],
            ManifestState::Valid(baseline.clone()),
            &config,
            true,
        );
        assert!(unchanged.changed.is_empty());
        assert!(unchanged.deleted_ids.is_empty());

        let mut counters_only = first.clone();
        counters_only.success_count = 9;
        let counters_plan = build_write_plan(
            &[counters_only, second.clone()],
            ManifestState::Valid(baseline.clone()),
            &config,
            true,
        );
        assert!(counters_plan.changed.is_empty());

        let mut changed_text = first.clone();
        changed_text
            .workflow
            .push("Check the final output".to_string());
        let changed_plan = build_write_plan(
            &[changed_text, second.clone()],
            ManifestState::Valid(baseline.clone()),
            &config,
            true,
        );
        assert_eq!(
            changed_plan
                .changed
                .iter()
                .map(|procedure| procedure.id.as_str())
                .collect::<Vec<_>>(),
            vec!["first"]
        );

        let deleted_plan =
            build_write_plan(&[first], ManifestState::Valid(baseline), &config, true);
        assert_eq!(deleted_plan.deleted_ids, vec!["second"]);
    }

    #[test]
    fn write_plan_rebuilds_for_embedding_contract_changes() {
        let original_config = embedding_config();
        let procedures = vec![procedure("first", "Inspect the source")];
        let initial =
            build_write_plan(&procedures, ManifestState::Missing, &original_config, false);
        let mut changed_config = original_config;
        changed_config.model = "replacement-embedder".to_string();

        let plan = build_write_plan(
            &procedures,
            ManifestState::Valid(initial.next_manifest),
            &changed_config,
            true,
        );
        assert!(plan.reset_table);
        assert_eq!(plan.changed.len(), 1);
        assert!(plan.deleted_ids.is_empty());
    }

    #[test]
    fn write_plan_rebuilds_when_context_changes_with_same_model_and_dimensions() {
        let original_config = embedding_config();
        let procedures = vec![procedure("first", "Inspect the source")];
        let initial =
            build_write_plan(&procedures, ManifestState::Missing, &original_config, false);
        let original_manifest = initial.next_manifest;
        let original_contract = original_manifest.embedding_contract_id.clone();

        let mut changed_config = original_config;
        changed_config.context_tokens = Some(
            changed_config
                .context_tokens
                .unwrap_or(8_192)
                .saturating_add(1_024),
        );
        assert_eq!(changed_config.model, original_manifest.embedding_model);
        assert_eq!(changed_config.dims, original_manifest.embedding_dimensions);
        assert_ne!(changed_config.embedding_contract_id(), original_contract);

        let plan = build_write_plan(
            &procedures,
            ManifestState::Valid(original_manifest),
            &changed_config,
            true,
        );
        assert!(plan.reset_table);
        assert_eq!(plan.changed.len(), 1);
        assert!(plan.deleted_ids.is_empty());
    }

    #[test]
    fn write_plan_rebuilds_when_physical_batch_ceiling_changes() {
        let original_config = embedding_config();
        let procedures = vec![procedure("first", "Inspect the source")];
        let initial =
            build_write_plan(&procedures, ManifestState::Missing, &original_config, false);
        let original_manifest = initial.next_manifest;
        let original_contract = original_manifest.embedding_contract_id.clone();

        let mut changed_config = original_config;
        changed_config.batch_tokens = Some(
            changed_config
                .batch_tokens
                .unwrap_or(512)
                .saturating_add(512),
        );
        assert_ne!(changed_config.embedding_contract_id(), original_contract);

        let plan = build_write_plan(
            &procedures,
            ManifestState::Valid(original_manifest),
            &changed_config,
            true,
        );
        assert!(plan.reset_table);
        assert_eq!(plan.changed.len(), 1);
        assert!(plan.deleted_ids.is_empty());
    }

    #[test]
    fn legacy_manifest_without_embedding_contract_fails_closed_to_rebuild() {
        let config = embedding_config();
        let procedures = vec![procedure("first", "Inspect the source")];
        let manifest =
            build_write_plan(&procedures, ManifestState::Missing, &config, false).next_manifest;
        let mut legacy = serde_json::to_value(manifest).expect("serialize current manifest");
        legacy
            .as_object_mut()
            .expect("manifest is an object")
            .remove("embedding_contract_id");

        assert!(
            serde_json::from_value::<ProcedureIndexManifest>(legacy).is_err(),
            "a v1 manifest cannot be treated as compatible without an exact embedding contract"
        );
    }

    #[test]
    fn embed_record_args_map_changed_procedures_to_a_procedure_index_batch() {
        let mut config = embedding_config();
        config.model = "changed-procedure-embedder".to_string();
        let scope = LearningScope::new("owner", "default");
        let changed = vec![
            procedure("first", "Inspect the source"),
            procedure("second", "Draft the response"),
        ];

        let (principal, workspace, model, operation, batch_size, input_tokens) =
            procedure_index_embed_record_args(&scope, &config, &changed);

        assert_eq!(principal, "owner");
        assert_eq!(workspace, "default");
        assert_eq!(model, "changed-procedure-embedder");
        assert_eq!(operation, "procedure_index");
        // One record per index write, batch size = number of changed procedures.
        assert_eq!(batch_size, 2);
        // Length-based estimate is derived from the same hybrid text that is
        // embedded, so it is strictly positive for non-empty procedures.
        let expected_tokens =
            crate::magician_v2::analytics::llm_embeddings_sink::estimate_input_tokens(
                &changed
                    .iter()
                    .map(procedure_hybrid_text)
                    .collect::<Vec<_>>(),
            );
        assert_eq!(input_tokens, expected_tokens);
        assert!(input_tokens > 0);
    }

    #[test]
    fn embed_record_args_report_an_empty_batch_when_nothing_changed() {
        let config = embedding_config();
        let scope = LearningScope::new("owner", "default");
        let (_, _, _, operation, batch_size, input_tokens) =
            procedure_index_embed_record_args(&scope, &config, &[]);
        assert_eq!(operation, "procedure_index");
        assert_eq!(batch_size, 0);
        assert_eq!(input_tokens, 0);
    }

    #[test]
    fn procedure_index_content_ignores_feedback_bookkeeping() {
        let before = procedure("first", "Inspect the source");
        let mut feedback_only = before.clone();
        feedback_only.success_count = 4;
        feedback_only.failure_count = 2;
        feedback_only.last_used_at = Some(Utc::now());
        feedback_only.version += 1;
        assert!(!procedure_index_content_changed(&before, &feedback_only));

        let mut changed_workflow = before.clone();
        changed_workflow
            .workflow
            .push("Verify the output".to_string());
        assert!(procedure_index_content_changed(&before, &changed_workflow));
    }

    #[test]
    fn dirty_marker_is_durable_without_a_running_maintainer() {
        let dir = TempDir::new().unwrap();
        let store = LearningStore::new(ArtifactV2Workspace::new(dir.path()));
        let scope = LearningScope::new("anonymous", "default");

        mark_procedure_index_dirty(&store, &scope);

        let marker = read_dirty_marker_body(&store, &scope).unwrap();
        assert!(marker.is_some());
        assert!(procedure_index_is_stale(
            &store,
            &scope,
            &[procedure("first", "Inspect the source")],
            &embedding_config()
        ));
    }

    #[test]
    fn successful_refresh_can_clear_an_unchanged_malformed_marker() {
        let dir = TempDir::new().unwrap();
        let store = LearningStore::new(ArtifactV2Workspace::new(dir.path()));
        let scope = LearningScope::new("anonymous", "default");
        let path = store
            .workspace_layout()
            .learning_procedure_index_dirty_path(&scope.principal, &scope.workspace);
        store
            .workspace_layout()
            .write_atomic_path_sync(&path, b"not-json")
            .unwrap();
        let marker_body = read_dirty_marker_body(&store, &scope).unwrap().unwrap();

        clear_dirty_marker_if_unchanged(&store, &scope, Some(&marker_body)).unwrap();

        assert!(read_dirty_marker_body(&store, &scope).unwrap().is_none());
    }
}
