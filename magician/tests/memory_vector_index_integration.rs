//! Integration tests for the `magician-vector-index` crate boundary.
//!
//! These tests live in `magician/tests/` because they exercise the
//! cross-crate edge: magician's concrete `AgentStorage` and
//! `AgentDefinitionStore` are passed (as `&dyn MemoryStorage` and
//! `&dyn DefinitionLookup`) to functions that live in
//! `magician_vector_index`. They prove the trait shims and the facade
//! re-exports survive a real call.
//!
//! Note: these are smoke tests, not exhaustive memory-retrieval
//! coverage. The retrieval logic itself is owned by unit tests inside
//! `magician-vector-index` and `magician_data_v3/memory_evals` regression
//! fixtures.

use magician::magician_v2::agents::{storage::AgentStorage, AgentDefinition, AgentDefinitionStore};
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician_vector_index::memory_index::{
    acknowledge_memory_index_changes, apply_memory_index_change_snapshot,
    inspect_scope_memory_index, rebuild_scope_memory_index, reconcile_scope_memory_index,
    record_memory_index_change, score_fresh_memory_hybrid_index,
    score_fresh_memory_hybrid_index_with_status, score_fresh_memory_index,
    snapshot_memory_index_changes, MemoryIndexChange, MemoryIndexIncrementalUpdateResult,
};
use tempfile::TempDir;
use tokio::sync::Mutex;

static TEST_HASH_ENV_LOCK: Mutex<()> = Mutex::const_new(());

struct EnvRestore {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvRestore {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.as_ref() {
            std::env::set_var(self.key, previous);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

/// Build an `AgentStorage` rooted at a scoped V3-style path so
/// `scope_segments()` returns `Some((principal, workspace))`. The
/// trait impl in magician depends on this layout for the analytics
/// + LanceDB sub-directory naming.
fn scoped_storage(tmp: &TempDir, principal: &str, workspace: &str) -> AgentStorage {
    let root = tmp
        .path()
        .join("scopes")
        .join(principal)
        .join(workspace)
        .join("agent_runtime");
    std::fs::create_dir_all(&root).unwrap();
    AgentStorage::with_scoped_memory_root(root)
}

fn provider_backed_scoped_storage(tmp: &TempDir, principal: &str, workspace: &str) -> AgentStorage {
    let workspace_layout = ArtifactV2Workspace::new(tmp.path());
    let root = workspace_layout
        .memory_root(principal, workspace)
        .join("agent_runtime");
    std::fs::create_dir_all(&root).unwrap();
    AgentStorage::with_scoped_memory_root_in_workspace(root, workspace_layout)
}

#[tokio::test]
async fn agent_storage_implements_memory_storage_trait() {
    // Compile-only check that the trait impl is actually wired up:
    // we coerce `&AgentStorage` to `&dyn MemoryStorage` and call one
    // of the trait methods. If the impl block is missing or shape-
    // mismatched, this won't compile.
    let tmp = TempDir::new().unwrap();
    let storage = scoped_storage(&tmp, "p", "w");
    let dyn_storage: &dyn magician_vector_index::storage_trait::MemoryStorage = &storage;
    let (principal, workspace) = dyn_storage.scope_segments().expect("scope segments");
    assert_eq!(principal, "p");
    assert_eq!(workspace, "w");
    assert_eq!(dyn_storage.root(), storage.root());
}

#[tokio::test]
async fn definition_store_implements_definition_lookup_trait() {
    // Same shape check for the second trait.
    let tmp = TempDir::new().unwrap();
    let store = AgentDefinitionStore::with_base_path(tmp.path());
    let dyn_store: &dyn magician_vector_index::definition_trait::DefinitionLookup = &store;
    let records = dyn_store
        .list_moveable_definitions()
        .await
        .expect("list_moveable_definitions on empty store");
    assert!(records.is_empty(), "empty store should return no records");
}

#[tokio::test]
async fn rebuild_index_succeeds_on_empty_scope() {
    // End-to-end: rebuild against a scope with no agent definitions
    // and no persisted memory tiers. Should produce an empty manifest
    // + a zero-document index without error.
    let tmp = TempDir::new().unwrap();
    let storage = scoped_storage(&tmp, "alpha", "default");
    let definition_store = AgentDefinitionStore::with_base_path(tmp.path().join("definitions"));

    let outcome = rebuild_scope_memory_index(&storage, &definition_store)
        .await
        .expect("rebuild should succeed on empty scope");

    assert!(
        outcome.manifest_path.exists(),
        "manifest file should be written: {:?}",
        outcome.manifest_path
    );
    assert!(
        outcome.documents_path.exists(),
        "documents file should be written: {:?}",
        outcome.documents_path
    );
    assert_eq!(
        outcome.manifest.agents.len(),
        0,
        "no definitions registered → no agent summaries"
    );

    record_memory_index_change(&storage, MemoryIndexChange::UserKnowledge)
        .await
        .expect("record pending canonical mutation");
    let score =
        score_fresh_memory_hybrid_index_with_status(&storage, &definition_store, "pending memory")
            .await
            .expect("score pending memory index");
    assert!(score.scores.is_none());
    assert_eq!(
        score.fallback_reason.as_deref(),
        Some("memory_index_pending_initial_content")
    );
}

#[tokio::test]
async fn provider_backed_rebuild_persists_manifest() {
    // Regression guard for provider-backed storage: the manifest is the atomic
    // commit marker for a memory-index rebuild and must be written through the
    // workspace provider, not only returned in-memory.
    let tmp = TempDir::new().unwrap();
    let storage = provider_backed_scoped_storage(&tmp, "alpha", "default");
    let definition_store = AgentDefinitionStore::with_base_path(tmp.path().join("definitions"));

    let outcome = rebuild_scope_memory_index(&storage, &definition_store)
        .await
        .expect("provider-backed rebuild should succeed");
    let status = inspect_scope_memory_index(&storage, &definition_store)
        .await
        .expect("provider-backed inspect after rebuild");

    assert!(
        outcome.manifest_path.exists(),
        "provider-backed manifest file should be written: {:?}",
        outcome.manifest_path
    );
    assert!(
        status.manifest.is_some(),
        "provider-backed manifest should reload"
    );
    assert!(
        !status.stale,
        "provider-backed rebuild should inspect fresh; reason: {}",
        status.reason
    );
}

#[tokio::test]
async fn inspect_reports_no_manifest_before_rebuild() {
    // Inspect against a scope where no rebuild has run yet — should
    // report stale (no manifest on disk).
    let tmp = TempDir::new().unwrap();
    let storage = scoped_storage(&tmp, "alpha", "default");
    let definition_store = AgentDefinitionStore::with_base_path(tmp.path().join("definitions"));

    let status = inspect_scope_memory_index(&storage, &definition_store)
        .await
        .expect("inspect should succeed even with no manifest");

    assert!(status.manifest.is_none(), "no rebuild yet → no manifest");
    assert!(status.stale, "missing manifest counts as stale");
}

#[tokio::test]
async fn runtime_reconcile_bootstraps_a_provably_absent_empty_index() {
    let tmp = TempDir::new().unwrap();
    let storage = scoped_storage(&tmp, "alpha", "default");
    let definition_store = AgentDefinitionStore::with_base_path(tmp.path().join("definitions"));

    let outcome = reconcile_scope_memory_index(&storage, &definition_store)
        .await
        .expect("runtime reconciliation may create the first provably empty derived index");

    assert_eq!(outcome.manifest.document_count, 0);
    assert_eq!(outcome.lancedb_write.mode, "create_empty");
    assert!(storage.memory_index_manifest_path().exists());
}

#[tokio::test]
async fn runtime_reconcile_refuses_existing_lancedb_table_without_a_manifest() {
    let _env_guard = TEST_HASH_ENV_LOCK.lock().await;
    let _allow_test_hash = EnvRestore::set("MAGICIAN_MEMORY_ALLOW_TEST_HASH_EMBEDDINGS", "1");
    let _provider = EnvRestore::set("MAGICIAN_MEMORY_EMBEDDING_PROVIDER", "test_hash");
    let tmp = TempDir::new().unwrap();
    let storage = scoped_storage(&tmp, "alpha", "default");
    let definition_store = AgentDefinitionStore::with_base_path(tmp.path().join("definitions"));
    let user_knowledge_path = storage.user_knowledge_path();
    std::fs::create_dir_all(user_knowledge_path.parent().unwrap()).unwrap();
    std::fs::write(
        &user_knowledge_path,
        r#"{"fields":{"owned":"This row proves that a LanceDB table already exists."}}"#,
    )
    .unwrap();

    let initial = rebuild_scope_memory_index(&storage, &definition_store)
        .await
        .expect("create an owned non-empty LanceDB index");
    assert!(initial.manifest.document_count > 0);
    std::fs::remove_file(storage.memory_index_manifest_path()).expect("remove ownership manifest");

    let error = reconcile_scope_memory_index(&storage, &definition_store)
        .await
        .expect_err("runtime reconciliation must not adopt or replace an unowned table");
    let error_chain = format!("{error:#}");
    assert!(
        error_chain.contains("explicit rebuild") && error_chain.contains("missing_manifest"),
        "runtime failure should require explicit maintenance: {error_chain}"
    );
    assert!(
        !storage.memory_index_manifest_path().exists(),
        "failed runtime reconciliation must not publish a replacement manifest"
    );
}

#[tokio::test]
async fn inspect_reports_fresh_after_rebuild() {
    // Rebuild then inspect — the manifest the inspect call reads
    // should match the one rebuild wrote.
    let tmp = TempDir::new().unwrap();
    let storage = scoped_storage(&tmp, "alpha", "default");
    let definition_store = AgentDefinitionStore::with_base_path(tmp.path().join("definitions"));

    rebuild_scope_memory_index(&storage, &definition_store)
        .await
        .expect("initial rebuild");
    let status = inspect_scope_memory_index(&storage, &definition_store)
        .await
        .expect("inspect after rebuild");

    assert!(status.manifest.is_some(), "rebuild persisted a manifest");
    assert!(
        !status.stale,
        "fresh rebuild with no upstream changes should not be stale; reason: {}",
        status.reason
    );
}

#[tokio::test]
async fn score_fresh_returns_none_when_no_index_exists() {
    // Without a rebuild, `score_fresh_memory_index` should return
    // None (not Err) so the caller can fall back to direct retrieval.
    let tmp = TempDir::new().unwrap();
    let storage = scoped_storage(&tmp, "alpha", "default");
    let definition_store = AgentDefinitionStore::with_base_path(tmp.path().join("definitions"));

    let result = score_fresh_memory_index(&storage, &definition_store, "some query")
        .await
        .expect("score_fresh_memory_index should not error on missing index");

    assert!(
        result.is_none(),
        "missing/stale index → None so caller falls back to direct retrieval"
    );

    let hybrid_result = score_fresh_memory_hybrid_index(&storage, &definition_store, "some query")
        .await
        .expect("score_fresh_memory_hybrid_index should not error on missing index");

    assert!(
        hybrid_result.is_none(),
        "missing/stale hybrid index → None so caller falls back to direct retrieval"
    );
}

#[tokio::test]
async fn hybrid_index_scores_user_memory_after_rebuild() {
    let _env_guard = TEST_HASH_ENV_LOCK.lock().await;
    let _allow_test_hash = EnvRestore::set("MAGICIAN_MEMORY_ALLOW_TEST_HASH_EMBEDDINGS", "1");
    let _provider = EnvRestore::set("MAGICIAN_MEMORY_EMBEDDING_PROVIDER", "test_hash");
    let tmp = TempDir::new().unwrap();
    let storage = scoped_storage(&tmp, "alpha", "default");
    let definition_store = AgentDefinitionStore::with_base_path(tmp.path().join("definitions"));
    let user_knowledge_path = storage.user_knowledge_path();
    std::fs::create_dir_all(user_knowledge_path.parent().unwrap()).unwrap();
    std::fs::write(
        &user_knowledge_path,
        r#"{
          "fields": {
            "warehouse": "Metabase analytics use the production DuckDB warehouse and dashboard SQL cards."
          }
        }"#,
    )
    .unwrap();

    rebuild_scope_memory_index(&storage, &definition_store)
        .await
        .expect("rebuild should index user memory");
    let scores = score_fresh_memory_hybrid_index(
        &storage,
        &definition_store,
        "metabase production warehouse sql",
    )
    .await
    .expect("hybrid scoring should succeed")
    .expect("fresh rebuilt index should return hybrid scores");

    assert!(
        scores.keys().any(|key| key.contains("knowledge.warehouse")),
        "hybrid scores should include the user knowledge candidate, got {scores:?}"
    );
}

#[tokio::test]
async fn rebuild_writes_embedding_cache_entries() {
    let _env_guard = TEST_HASH_ENV_LOCK.lock().await;
    let _allow_test_hash = EnvRestore::set("MAGICIAN_MEMORY_ALLOW_TEST_HASH_EMBEDDINGS", "1");
    let _provider = EnvRestore::set("MAGICIAN_MEMORY_EMBEDDING_PROVIDER", "test_hash");
    let tmp = TempDir::new().unwrap();
    let storage = scoped_storage(&tmp, "alpha", "default");
    let definition_store = AgentDefinitionStore::with_base_path(tmp.path().join("definitions"));
    let user_knowledge_path = storage.user_knowledge_path();
    std::fs::create_dir_all(user_knowledge_path.parent().unwrap()).unwrap();
    std::fs::write(
        &user_knowledge_path,
        r#"{
          "fields": {
            "warehouse": "Metabase analytics use the production DuckDB warehouse and dashboard SQL cards."
          }
        }"#,
    )
    .unwrap();

    rebuild_scope_memory_index(&storage, &definition_store)
        .await
        .expect("rebuild should write embedding cache entries");

    let cache_dir = storage.memory_index_dir().join("embedding_cache");
    let cache_file_count = count_files_with_extension(&cache_dir, "f32");
    assert!(
        cache_file_count > 0,
        "rebuild should cache chunk embeddings under {cache_dir:?}"
    );
}

#[tokio::test]
async fn repeated_rebuild_removes_deleted_lancedb_rows() {
    let _env_guard = TEST_HASH_ENV_LOCK.lock().await;
    let _allow_test_hash = EnvRestore::set("MAGICIAN_MEMORY_ALLOW_TEST_HASH_EMBEDDINGS", "1");
    let _provider = EnvRestore::set("MAGICIAN_MEMORY_EMBEDDING_PROVIDER", "test_hash");
    let tmp = TempDir::new().unwrap();
    let storage = scoped_storage(&tmp, "alpha", "default");
    let definition_store = AgentDefinitionStore::with_base_path(tmp.path().join("definitions"));
    let user_knowledge_path = storage.user_knowledge_path();
    std::fs::create_dir_all(user_knowledge_path.parent().unwrap()).unwrap();
    std::fs::write(
        &user_knowledge_path,
        r#"{
          "fields": {
            "warehouse": "Metabase analytics use the production DuckDB warehouse and dashboard SQL cards.",
            "calendar": "Calendar exports are staged in the analyst spreadsheet."
          }
        }"#,
    )
    .unwrap();

    rebuild_scope_memory_index(&storage, &definition_store)
        .await
        .expect("initial rebuild");
    let initial_scores = score_fresh_memory_hybrid_index(
        &storage,
        &definition_store,
        "calendar analyst spreadsheet",
    )
    .await
    .expect("initial hybrid scoring")
    .expect("initial index should return scores");
    assert!(
        initial_scores
            .keys()
            .any(|key| key.contains("knowledge.calendar")),
        "initial scores should include calendar knowledge, got {initial_scores:?}"
    );

    std::fs::write(
        &user_knowledge_path,
        r#"{
          "fields": {
            "warehouse": "Metabase analytics use the production DuckDB warehouse and dashboard SQL cards.",
            "flights": "Flight-search memories belong to travel planning."
          }
        }"#,
    )
    .unwrap();
    rebuild_scope_memory_index(&storage, &definition_store)
        .await
        .expect("incremental rebuild should delete removed rows");

    let post_delete_scores = score_fresh_memory_hybrid_index(
        &storage,
        &definition_store,
        "calendar analyst spreadsheet",
    )
    .await
    .expect("post-delete hybrid scoring")
    .expect("rebuilt index should return scores");
    assert!(
        !post_delete_scores
            .keys()
            .any(|key| key.contains("knowledge.calendar")),
        "removed calendar knowledge should not remain in LanceDB rows, got {post_delete_scores:?}"
    );

    let inserted_scores =
        score_fresh_memory_hybrid_index(&storage, &definition_store, "flight travel planning")
            .await
            .expect("post-insert hybrid scoring")
            .expect("rebuilt index should return inserted scores");
    assert!(
        inserted_scores
            .keys()
            .any(|key| key.contains("knowledge.flights")),
        "incremental rebuild should insert new flight knowledge, got {inserted_scores:?}"
    );
}

#[tokio::test]
async fn runtime_reconcile_deletes_the_final_row_without_replacing_lancedb() {
    let _env_guard = TEST_HASH_ENV_LOCK.lock().await;
    let _allow_test_hash = EnvRestore::set("MAGICIAN_MEMORY_ALLOW_TEST_HASH_EMBEDDINGS", "1");
    let _provider = EnvRestore::set("MAGICIAN_MEMORY_EMBEDDING_PROVIDER", "test_hash");
    let tmp = TempDir::new().unwrap();
    let storage = scoped_storage(&tmp, "alpha", "default");
    let definition_store = AgentDefinitionStore::with_base_path(tmp.path().join("definitions"));
    let user_knowledge_path = storage.user_knowledge_path();
    std::fs::create_dir_all(user_knowledge_path.parent().unwrap()).unwrap();
    std::fs::write(
        &user_knowledge_path,
        r#"{"fields":{"only":"This is the final indexed memory row."}}"#,
    )
    .unwrap();

    rebuild_scope_memory_index(&storage, &definition_store)
        .await
        .expect("initial explicit rebuild");
    let lancedb_dir = storage.memory_lancedb_index_dir();
    let marker = lancedb_dir.join("runtime-preserved.marker");
    std::fs::write(&marker, "preserve directory").unwrap();

    std::fs::write(&user_knowledge_path, r#"{"fields":{}}"#).unwrap();
    let outcome = reconcile_scope_memory_index(&storage, &definition_store)
        .await
        .expect("merge-only reconciliation should delete the final row");

    assert_eq!(outcome.lancedb_write.mode, "merge");
    assert_eq!(outcome.lancedb_write.deleted_rows, Some(1));
    assert_eq!(outcome.manifest.document_count, 0);
    assert!(
        marker.exists(),
        "runtime reconciliation must preserve the existing LanceDB directory"
    );
}

#[tokio::test]
async fn source_journal_delta_updates_only_changed_user_memory() {
    let _env_guard = TEST_HASH_ENV_LOCK.lock().await;
    let _allow_test_hash = EnvRestore::set("MAGICIAN_MEMORY_ALLOW_TEST_HASH_EMBEDDINGS", "1");
    let _provider = EnvRestore::set("MAGICIAN_MEMORY_EMBEDDING_PROVIDER", "test_hash");
    let tmp = TempDir::new().unwrap();
    let storage = scoped_storage(&tmp, "alpha", "default");
    let definition_store = AgentDefinitionStore::with_base_path(tmp.path().join("definitions"));
    let user_knowledge_path = storage.user_knowledge_path();
    std::fs::create_dir_all(user_knowledge_path.parent().unwrap()).unwrap();
    std::fs::write(
        &user_knowledge_path,
        r#"{
          "fields": {
            "warehouse": "Metabase analytics use the production DuckDB warehouse.",
            "calendar": "Calendar exports are staged in the analyst spreadsheet."
          }
        }"#,
    )
    .unwrap();

    rebuild_scope_memory_index(&storage, &definition_store)
        .await
        .expect("initial rebuild");

    std::fs::write(
        &user_knowledge_path,
        r#"{
          "fields": {
            "warehouse": "Metabase analytics use the production DuckDB warehouse.",
            "flights": "Flight-search memories belong to travel planning."
          }
        }"#,
    )
    .unwrap();
    record_memory_index_change(&storage, MemoryIndexChange::UserKnowledge)
        .await
        .expect("record changed user source");
    let snapshot = snapshot_memory_index_changes(&storage)
        .await
        .expect("snapshot changed user source");

    let update = apply_memory_index_change_snapshot(&storage, &definition_store, &snapshot)
        .await
        .expect("apply source-addressable user-memory delta");
    let MemoryIndexIncrementalUpdateResult::Applied(update) = update else {
        panic!("compatible user source update should not require a full rebuild");
    };
    assert_eq!(update.changed_source_count, 1);
    assert_eq!(update.lancedb_write.mode, "source_delta");
    acknowledge_memory_index_changes(&storage, &snapshot)
        .await
        .expect("acknowledge applied source delta");
    assert!(
        snapshot_memory_index_changes(&storage)
            .await
            .expect("snapshot after acknowledgement")
            .is_empty(),
        "a successfully applied source delta should consume its journal entry"
    );

    let status = inspect_scope_memory_index(&storage, &definition_store)
        .await
        .expect("inspect after source delta");
    assert!(
        !status.stale,
        "source delta should refresh every derived artifact; reason: {}",
        status.reason
    );
}

#[tokio::test]
async fn explicit_rebuild_consumes_the_journal_generation_it_applied() {
    let _env_guard = TEST_HASH_ENV_LOCK.lock().await;
    let _allow_test_hash = EnvRestore::set("MAGICIAN_MEMORY_ALLOW_TEST_HASH_EMBEDDINGS", "1");
    let _provider = EnvRestore::set("MAGICIAN_MEMORY_EMBEDDING_PROVIDER", "test_hash");
    let tmp = TempDir::new().unwrap();
    let storage = scoped_storage(&tmp, "alpha", "default");
    let definition_store = AgentDefinitionStore::with_base_path(tmp.path().join("definitions"));
    let user_knowledge_path = storage.user_knowledge_path();
    std::fs::create_dir_all(user_knowledge_path.parent().unwrap()).unwrap();
    std::fs::write(
        &user_knowledge_path,
        r#"{"fields":{"warehouse":"Initial warehouse memory."}}"#,
    )
    .unwrap();
    rebuild_scope_memory_index(&storage, &definition_store)
        .await
        .expect("initial explicit rebuild");

    std::fs::write(
        &user_knowledge_path,
        r#"{"fields":{"warehouse":"Updated warehouse memory."}}"#,
    )
    .unwrap();
    record_memory_index_change(&storage, MemoryIndexChange::UserKnowledge)
        .await
        .expect("record changed canonical source");
    let pending_status = inspect_scope_memory_index(&storage, &definition_store)
        .await
        .expect("inspect pending source change");
    assert_eq!(pending_status.reason, "pending_changes");

    rebuild_scope_memory_index(&storage, &definition_store)
        .await
        .expect("explicit rebuild with pending journal generation");

    assert!(
        snapshot_memory_index_changes(&storage)
            .await
            .expect("snapshot after explicit rebuild")
            .is_empty(),
        "explicit rebuild must acknowledge only the journal generations it applied"
    );
    let rebuilt_status = inspect_scope_memory_index(&storage, &definition_store)
        .await
        .expect("inspect rebuilt source change");
    assert_eq!(rebuilt_status.reason, "fresh");
}

#[tokio::test]
async fn journal_acknowledgement_preserves_a_rewritten_source() {
    let tmp = TempDir::new().unwrap();
    let storage = scoped_storage(&tmp, "alpha", "default");

    record_memory_index_change(&storage, MemoryIndexChange::UserKnowledge)
        .await
        .expect("record initial source mutation");
    let first_snapshot = snapshot_memory_index_changes(&storage)
        .await
        .expect("snapshot initial source mutation");

    // This is the race an index update must handle: a later canonical write
    // replaces the coalesced journal entry while the first snapshot is being
    // processed. Acknowledging the first snapshot must retain the newer write.
    record_memory_index_change(&storage, MemoryIndexChange::UserKnowledge)
        .await
        .expect("record replacement source mutation");
    acknowledge_memory_index_changes(&storage, &first_snapshot)
        .await
        .expect("acknowledge stale snapshot");

    assert!(
        !snapshot_memory_index_changes(&storage)
            .await
            .expect("snapshot retained source mutation")
            .is_empty(),
        "acknowledging an older generation must not remove the newer source mutation"
    );
}

fn count_files_with_extension(path: &std::path::Path, extension: &str) -> usize {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .map(|path| {
            if path.is_dir() {
                count_files_with_extension(&path, extension)
            } else if path.extension().and_then(|value| value.to_str()) == Some(extension) {
                1
            } else {
                0
            }
        })
        .sum()
}

#[tokio::test]
async fn hybrid_index_is_skipped_when_sources_change() {
    let _env_guard = TEST_HASH_ENV_LOCK.lock().await;
    let _allow_test_hash = EnvRestore::set("MAGICIAN_MEMORY_ALLOW_TEST_HASH_EMBEDDINGS", "1");
    let _provider = EnvRestore::set("MAGICIAN_MEMORY_EMBEDDING_PROVIDER", "test_hash");
    let tmp = TempDir::new().unwrap();
    let storage = scoped_storage(&tmp, "alpha", "default");
    let definition_store = AgentDefinitionStore::with_base_path(tmp.path().join("definitions"));
    let user_knowledge_path = storage.user_knowledge_path();
    std::fs::create_dir_all(user_knowledge_path.parent().unwrap()).unwrap();
    std::fs::write(
        &user_knowledge_path,
        r#"{
          "fields": {
            "warehouse": "Metabase analytics use the production DuckDB warehouse and dashboard SQL cards."
          }
        }"#,
    )
    .unwrap();

    rebuild_scope_memory_index(&storage, &definition_store)
        .await
        .expect("rebuild should index user memory");
    std::fs::write(
        &user_knowledge_path,
        r#"{
            "fields": {
              "warehouse": "Metabase analytics use the production DuckDB warehouse and dashboard SQL cards.",
              "fresh_note": "This source edit should make the manifest hard-stale."
            }
        }"#,
    )
    .unwrap();
    let status = inspect_scope_memory_index(&storage, &definition_store)
        .await
        .expect("inspect after source edit");
    assert!(status.stale, "source edit should make the index stale");
    assert_eq!(status.reason, "source_hashes_changed");

    let scores = score_fresh_memory_hybrid_index(
        &storage,
        &definition_store,
        "metabase production warehouse sql",
    )
    .await
    .expect("hard-stale hybrid scoring should fall back without error");
    assert!(
        scores.is_none(),
        "hard-stale source changes should disable derived hybrid scores, got {scores:?}"
    );
}

/// An episode written to disk is indexed by a rebuild and scored by the hybrid
/// index, end to end, through magician's concrete `AgentStorage`.
///
/// This is the cross-crate edge the rest of this file exists for, applied to
/// the one candidate producer that never used to reach the index: episodes are
/// not a declared tier and live outside the tier walk, so nothing else here
/// would catch it if the loader, the trait shim, or the rebuild wiring stopped
/// agreeing. Deterministic `test_hash` embeddings keep it hermetic — no Ollama,
/// no running magician.
#[tokio::test]
async fn hybrid_index_scores_an_episode_after_rebuild() {
    let _env_guard = TEST_HASH_ENV_LOCK.lock().await;
    let _allow_test_hash = EnvRestore::set("MAGICIAN_MEMORY_ALLOW_TEST_HASH_EMBEDDINGS", "1");
    let _provider = EnvRestore::set("MAGICIAN_MEMORY_EMBEDDING_PROVIDER", "test_hash");
    let tmp = TempDir::new().unwrap();
    let storage = scoped_storage(&tmp, "alpha", "default");
    let definition_store = AgentDefinitionStore::with_base_path(tmp.path().join("definitions"));

    // Episodes are loaded per agent-definition record, so the scope needs one.
    definition_store
        .create_definition(
            AgentDefinition::from_yaml_str(
                "agent_id: envoy\n\
                 name: envoy\n\
                 kind: worker\n\
                 description: Agent under test.\n\
                 persona: Records episodes.\n",
            )
            .expect("definition parses"),
        )
        .await
        .expect("definition is created");

    // Write one episode the way the runtime does: a single JSON file under the
    // agent's episodes directory.
    let episodes_dir = storage.agent_episodes_dir("envoy").expect("episodes dir");
    std::fs::create_dir_all(&episodes_dir).unwrap();
    std::fs::write(
        episodes_dir.join("ep-1.json"),
        r#"{
          "agent_id": "envoy",
          "episode_id": "ep-1",
          "goal_key": "task_indexing_probe",
          "trigger_seq": 1,
          "completed_at": "2026-09-02T10:00:00Z",
          "outcome_kind": "goal_achieved",
          "outcome_summary": "Repaired the parquet compaction job after the duckdb warehouse stalled.",
          "observations": ["the compactor held a write lock for the whole batch"]
        }"#,
    )
    .unwrap();

    rebuild_scope_memory_index(&storage, &definition_store)
        .await
        .expect("rebuild should index the episode");

    let scores = score_fresh_memory_hybrid_index(
        &storage,
        &definition_store,
        "duckdb parquet compaction stalled",
    )
    .await
    .expect("hybrid scoring should succeed")
    .expect("fresh rebuilt index should return hybrid scores");

    assert!(
        scores.keys().any(|key| key.contains("episodes")),
        "the episode should carry a hybrid score, got {scores:?}"
    );
}

/// Appending an episode records a journal entry that resolves to an
/// incremental target, so the episode reaches the index without waiting for the
/// 15-30 minute dirty-scope rebuild.
#[tokio::test]
async fn appending_an_episode_marks_the_index_incrementally() {
    let tmp = TempDir::new().unwrap();
    let storage = scoped_storage(&tmp, "alpha", "default");

    record_memory_index_change(
        &storage,
        MemoryIndexChange::Episodes {
            agent_id: "envoy".to_string(),
        },
    )
    .await
    .expect("recording an episodes change should persist");

    let snapshot = snapshot_memory_index_changes(&storage)
        .await
        .expect("snapshot should load");
    assert!(
        !snapshot.is_empty(),
        "an appended episode should leave a pending index change"
    );
}

/// An episodes-only change must resolve to an incremental target, not fall back
/// to a full rebuild.
///
/// This is the regression test for the bug that made the incremental commit
/// inert and actively harmful: `resolve_incremental_targets` only loaded agent
/// definition records for `UserKnowledge` and `NativeTier` changes, so an
/// episodes-only snapshot saw an empty record list, failed its own agent check,
/// and returned "cannot resolve incrementally". Every episode append then forced
/// a full rebuild and, until that rebuild ran, dropped hybrid scoring for every
/// candidate in the scope. The earlier test only asserted the journal was
/// non-empty, which is why it passed throughout.
#[tokio::test]
async fn an_episode_change_resolves_incrementally_rather_than_forcing_a_rebuild() {
    let _env_guard = TEST_HASH_ENV_LOCK.lock().await;
    let _allow_test_hash = EnvRestore::set("MAGICIAN_MEMORY_ALLOW_TEST_HASH_EMBEDDINGS", "1");
    let _provider = EnvRestore::set("MAGICIAN_MEMORY_EMBEDDING_PROVIDER", "test_hash");
    let tmp = TempDir::new().unwrap();
    let storage = scoped_storage(&tmp, "alpha", "default");
    let definition_store = AgentDefinitionStore::with_base_path(tmp.path().join("definitions"));
    definition_store
        .create_definition(
            AgentDefinition::from_yaml_str(
                "agent_id: envoy\nname: envoy\nkind: worker\n\
                 description: Agent under test.\npersona: Records episodes.\n",
            )
            .expect("definition parses"),
        )
        .await
        .expect("definition created");

    let episodes_dir = storage.agent_episodes_dir("envoy").expect("episodes dir");
    std::fs::create_dir_all(&episodes_dir).unwrap();
    std::fs::write(
        episodes_dir.join("ep-1.json"),
        r#"{
          "agent_id": "envoy",
          "episode_id": "ep-1",
          "goal_key": "task_probe",
          "completed_at": "2026-09-03T10:00:00Z",
          "outcome_kind": "goal_achieved",
          "outcome_summary": "indexed one episode incrementally"
        }"#,
    )
    .unwrap();

    rebuild_scope_memory_index(&storage, &definition_store)
        .await
        .expect("baseline rebuild");

    record_memory_index_change(
        &storage,
        MemoryIndexChange::Episodes {
            agent_id: "envoy".to_string(),
        },
    )
    .await
    .expect("record episodes change");
    let snapshot = snapshot_memory_index_changes(&storage)
        .await
        .expect("snapshot");
    assert!(!snapshot.is_empty());

    let update = apply_memory_index_change_snapshot(&storage, &definition_store, &snapshot)
        .await
        .expect("applying an episodes change should not error");
    let MemoryIndexIncrementalUpdateResult::Applied(update) = update else {
        panic!("an episodes-only change must apply incrementally, got {update:?}");
    };
    assert_eq!(update.changed_source_count, 1);
}

// ---------------------------------------------------------------------------
// Retrieval-quality probe for episodic memory. Ignored by default: it needs the
// real embedding daemon on :11435 and reads the operator's own episodes.
//
//   bash scripts/run-ollama-embedding.sh
//   cargo test -p magician --test memory_vector_index_integration \
//     -- --ignored --nocapture episode_retrieval_quality
//
// It answers one question: does putting episodes in the index actually retrieve
// the right one? Two arms, both real code paths, no reimplementation:
//   lexical = score_fresh_memory_index          (BM25 over the FTS index)
//   hybrid  = score_fresh_memory_hybrid_index   (BM25 + dense, RRF fused)
//
// The lexical arm is a CONSERVATIVE stand-in for the old behaviour. Episodes
// used not to be in the index at all and fell to the chat path's own keyword
// fallback, which is weaker than BM25 over a real FTS index. So this
// understates the improvement rather than flattering it.
// ---------------------------------------------------------------------------
#[tokio::test]
#[ignore = "needs the real embedding daemon on :11435 and the operator's episodes"]
async fn episode_retrieval_quality() {
    // Point the crate at the running embedding daemon the way magician does at
    // boot. Without this the model name is empty and every vector leg is a
    // no-op, which would silently make both arms identical.
    magician_vector_index::ollama_keep_alive::set_default_ollama_embedding_policy(
        Some("http://127.0.0.1:11435".to_string()),
        Some("-1".to_string()),
        Some(1),
        Some(1),
        Some(30_000),
        Some(180_000),
        Some("hf.co/mykor/pplx-embed-v1-4b-GGUF:Q6_K".to_string()),
        Some(8192),
        // batch_tokens / batch_size mirror magician-config.yaml. The rebuild
        // refuses to embed without a batch size rather than guessing one.
        Some(512),
        Some(2),
        Some(2560),
    );

    let live = std::path::PathBuf::from(std::env::var("HOME").unwrap())
        .join("MagicianNotes/scopes/anonymous/default/memory/agents");
    if !live.is_dir() {
        eprintln!("no live episode store at {}; skipping", live.display());
        return;
    }

    let tmp = TempDir::new().unwrap();
    let storage = scoped_storage(&tmp, "alpha", "default");
    let definition_store = AgentDefinitionStore::with_base_path(tmp.path().join("definitions"));

    // Copy one agent's episodes into the temp scope. Read-only on the source.
    let agent =
        std::env::var("EPISODE_EVAL_AGENT").unwrap_or_else(|_| "web-researcher".to_string());
    definition_store
        .create_definition(
            AgentDefinition::from_yaml_str(&format!(
                "agent_id: {agent}\nname: {agent}\nkind: worker\n\
                 description: Agent under evaluation.\npersona: Records episodes.\n"
            ))
            .expect("definition parses"),
        )
        .await
        .expect("definition created");

    let dest = storage.agent_episodes_dir(&agent).expect("episodes dir");
    std::fs::create_dir_all(&dest).unwrap();
    let mut copied = 0usize;
    for entry in std::fs::read_dir(live.join(&agent).join("episodes")).expect("live episodes") {
        let entry = entry.unwrap();
        if entry.path().extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        std::fs::copy(entry.path(), dest.join(entry.file_name())).unwrap();
        copied += 1;
    }
    println!("copied {copied} episodes for {agent}");

    rebuild_scope_memory_index(&storage, &definition_store)
        .await
        .expect("rebuild with real embeddings");

    // Known-item retrieval. Each case names an episode id and a query written
    // to describe it WITHOUT reusing its distinctive wording, which is the case
    // the dense leg exists for. Cases come from EPISODE_EVAL_CASES as
    // `episode_id=query` lines so the fixtures are not baked into the binary.
    let raw = std::env::var("EPISODE_EVAL_CASES").unwrap_or_default();
    let cases: Vec<(String, String)> = raw
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(id, query)| (id.trim().to_string(), query.trim().to_string()))
        .filter(|(id, query)| !id.is_empty() && !query.is_empty())
        .collect();
    if cases.is_empty() {
        println!("no EPISODE_EVAL_CASES supplied; rebuild-only smoke run");
        return;
    }

    // Rank within the SCORED SET is not comparable across the two arms: BM25
    // only returns documents sharing a query term, so a rank of 1 out of three
    // matches would look better than a rank of 17 out of ninety. Report the set
    // size alongside every rank, and also report recall over a fixed universe —
    // whether the target appears in the arm's top k AT ALL — which is the
    // question that survives the difference in set sizes.
    let rank_of = |scores: &std::collections::BTreeMap<String, f32>, episode_id: &str| {
        let mut ranked: Vec<(&String, &f32)> = scores.iter().collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap_or(std::cmp::Ordering::Equal));
        let position = ranked
            .iter()
            .position(|(key, _)| key.contains(episode_id))
            .map(|index| index + 1);
        (position, ranked.len())
    };

    let mut lex_rr = 0.0f64;
    let mut hyb_rr = 0.0f64;
    let mut lex_hits = 0usize;
    let mut hyb_hits = 0usize;
    for (episode_id, query) in &cases {
        let lexical = score_fresh_memory_index(&storage, &definition_store, query)
            .await
            .expect("lexical scoring")
            .unwrap_or_default();
        let hybrid = score_fresh_memory_hybrid_index(&storage, &definition_store, query)
            .await
            .expect("hybrid scoring")
            .unwrap_or_default();
        let (lex, lex_n) = rank_of(&lexical, episode_id);
        let (hyb, hyb_n) = rank_of(&hybrid, episode_id);
        if let Some(rank) = lex {
            lex_rr += 1.0 / rank as f64;
            if rank <= 5 {
                lex_hits += 1;
            }
        }
        if let Some(rank) = hyb {
            hyb_rr += 1.0 / rank as f64;
            if rank <= 5 {
                hyb_hits += 1;
            }
        }
        println!(
            "{episode_id}: lexical={lex:?}/{lex_n} hybrid={hyb:?}/{hyb_n}  found_by_lexical={} found_by_hybrid={}  q={query}",
            lex.is_some(),
            hyb.is_some()
        );
    }
    let n = cases.len() as f64;
    let lex_found = cases.len(); // recomputed below for clarity
    let _ = lex_found;
    println!(
        "\ncases={} | recall@5 lexical={}/{} hybrid={}/{} | MRR lexical={:.3} hybrid={:.3}",
        cases.len(),
        lex_hits,
        cases.len(),
        hyb_hits,
        cases.len(),
        lex_rr / n,
        hyb_rr / n
    );
    println!(
        "NOTE: ranks are within each arm's own scored set; the set sizes above \
         are what make them comparable or not."
    );
}
