use std::{path::Path, sync::Arc, time::Duration};

use runtime_core::{RuntimeConfig, ToolCatalog, ToolMatching};

use crate::magician_v2::{
    artifact_v2::ArtifactV2Service,
    ask_loop::ClarificationSessionManager,
    execution::{ExecutionConfig, MagicutorClient},
    gaui::MuijStorage,
    orchestrator::MagicianV2Orchestrator,
    prompts::JsonPromptStorage,
    query_analysis::operation_llm_router::{MockQueryAnalysisLLM, QueryAnalysisLLM},
    services::MagicianV2Services,
    storage::FileV2Store,
};

#[derive(Debug)]
struct TestRuntimeConfig {
    storage_path: String,
}

impl RuntimeConfig for TestRuntimeConfig {
    fn realtime_events_enabled(&self) -> bool {
        false
    }

    fn storage_path(&self) -> &str {
        &self.storage_path
    }

    fn max_conversations(&self) -> usize {
        32
    }

    fn conversation_timeout(&self) -> Duration {
        Duration::from_secs(60)
    }
}

struct NullToolDiscovery;

#[runtime_core::prelude::async_trait]
impl crate::ToolDiscovery for NullToolDiscovery {
    async fn find_best_match_with_context(
        &self,
        _task_description: &str,
        _context: &crate::ExecutionContext,
    ) -> crate::ToolMatchResult {
        crate::ToolMatchResult::default()
    }

    async fn find_multiple_matches_with_context(
        &self,
        _task_description: &str,
        _context: &crate::ExecutionContext,
    ) -> crate::MultipleToolMatchResult {
        crate::MultipleToolMatchResult::default()
    }

    async fn is_tool_available(
        &self,
        _tool_name: &str,
        _context: &crate::ExecutionContext,
    ) -> bool {
        false
    }

    async fn get_tool_metadata(
        &self,
        _tool_name: &str,
        _context: &crate::ExecutionContext,
    ) -> Option<std::collections::HashMap<String, serde_json::Value>> {
        None
    }

    async fn get_available_tools(&self, _context: &crate::ExecutionContext) -> Vec<String> {
        Vec::new()
    }
}

pub fn build_test_orchestrator(base_root: &Path) -> Arc<MagicianV2Orchestrator> {
    build_test_orchestrator_inner(base_root, &base_root.join("conversation_store"), false)
}

fn build_test_orchestrator_inner(
    base_root: &Path,
    conversation_store_root: &Path,
    with_operation_router: bool,
) -> Arc<MagicianV2Orchestrator> {
    build_test_orchestrator_inner_at(
        base_root,
        base_root,
        conversation_store_root,
        with_operation_router,
    )
}

/// Same, with the RUNTIME root named separately from the storage base.
///
/// Production sets the orchestrator's workspace layout EXPLICITLY --
/// `magician_core/builder.rs` calls `.with_workspace_layout(storage_workspace)`
/// with the resolved runtime root -- so its fallback never fires. The fallback
/// resolves from `runtime_config.storage_path()`, which is the SEED root
/// (`magician_data_v3` by default), not the runtime store; `$HOME/MagicianNotes`
/// or an env override is where durable state actually lives.
///
/// The harness supplied no layout, took that fallback, and landed on
/// `base_root` while handing `ArtifactV2Service` `base_root/magician_data_v3`.
/// The two disagreed by one segment, so a scope sealed through the service's
/// store was invisible to the orchestrator's and every stateless-driver
/// precondition read as unmet. This wires the layout the way production does
/// rather than redefining what `storage_path` means.
fn build_test_orchestrator_inner_at(
    base_root: &Path,
    runtime_root: &Path,
    conversation_store_root: &Path,
    with_operation_router: bool,
) -> Arc<MagicianV2Orchestrator> {
    build_test_orchestrator_inner_at_with_llm(
        base_root,
        runtime_root,
        conversation_store_root,
        with_operation_router,
        Arc::new(MockQueryAnalysisLLM),
    )
}

fn build_test_orchestrator_inner_at_with_llm(
    base_root: &Path,
    runtime_root: &Path,
    conversation_store_root: &Path,
    with_operation_router: bool,
    llm_service: Arc<dyn QueryAnalysisLLM>,
) -> Arc<MagicianV2Orchestrator> {
    std::fs::create_dir_all(base_root).expect("test support should create storage root");

    let broadcaster =
        Arc::new(crate::magician_v2::realtime_events::RuntimeTransportBroadcaster::new(16));
    let conversation_store: Arc<dyn crate::magician_v2::storage::V2ConversationStore> =
        Arc::new(FileV2Store::new(conversation_store_root));
    let session_manager: Arc<dyn crate::magician_v2::ask_loop::SessionManager> = Arc::new(
        ClarificationSessionManager::new(Arc::clone(&conversation_store), Arc::clone(&broadcaster)),
    );
    let prompt_store =
        JsonPromptStorage::with_default_config().expect("default prompt storage should load");
    let prompt_manager = Arc::new(crate::magician_v2::prompts::PromptManager::new(Arc::new(
        prompt_store,
    )));
    let tool_discovery: Arc<dyn crate::ToolDiscovery> = Arc::new(NullToolDiscovery);
    let tool_adapter = Arc::new(crate::magician_v2::tooling::ToolDiscoveryAdapter::new(
        tool_discovery,
    ));
    let tool_catalog: Arc<dyn ToolCatalog> = tool_adapter.clone();
    let tool_matching: Arc<dyn ToolMatching> = tool_adapter.clone();
    let runtime_config: Arc<dyn RuntimeConfig> = Arc::new(TestRuntimeConfig {
        storage_path: base_root.to_string_lossy().to_string(),
    });
    let magicutor_client = Arc::new(
        MagicutorClient::new(ExecutionConfig::default())
            .expect("magicutor client should construct for tests"),
    );
    let services = MagicianV2Services::new(
        runtime_config,
        conversation_store,
        prompt_manager,
        tool_catalog,
        tool_matching,
        None,
        session_manager,
        magicutor_client,
        Vec::new(),
        std::path::PathBuf::from("."),
        None,
    )
    // Exactly what `magician_core/builder.rs` does in production: hand the
    // orchestrator the SAME workspace the service gets, so a scope sealed
    // through one store is visible to the other.
    .with_workspace_layout(
        crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::new(runtime_root),
    );

    let orchestrator = MagicianV2Orchestrator::new_with_secret_runtime(
        services,
        llm_service,
        crate::magician_v2::secrets::in_memory_secret_runtime_bootstrap(),
    )
    .with_event_broadcaster(broadcaster);
    let orchestrator = if with_operation_router {
        orchestrator.with_operation_llm_router(Arc::new(
            crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter::new(None),
        ))
    } else {
        orchestrator
    };
    orchestrator.initialize()
}

/// Mark one scope's legacy-writer cutover complete, the way an operator does
/// once per scope with `magician seal-stateless-loop-cutover`.
///
/// The stateless driver — the DEFAULT, and the only one selected implicitly —
/// refuses to run in a scope that has not completed the cutover. That is a
/// deployment precondition, not a behaviour under test, so a fixture that
/// wants a working service has to establish it exactly as a real deployment
/// would.
///
/// Do NOT reach for `MAGICIAN_EXECUTION_DRIVER=inprocess` instead. That driver
/// is documented as never selected implicitly, has no durable outbox drain,
/// and carries restart guarantees that deliberately differ. Switching to it
/// turns these failures green by no longer exercising the driver production
/// runs.
pub async fn activate_stateless_scope(runtime_root: &Path, principal: &str, workspace: &str) {
    crate::magician_v2::execution::agentic::run_loop::store::fs::FsLoopStateStore::new(
        runtime_root.to_path_buf(),
    )
    .seal_legacy_writer_cutover(principal, workspace, "test-harness", 1)
    .await
    .expect("test scope should activate");
}

/// Scopes the suite conventionally uses. A harness-built service is a
/// cutover-complete deployment, which is the ordinary state; a test that wants
/// the migration state builds its own store and seals nothing.
const CONVENTIONAL_TEST_SCOPES: &[(&str, &str)] = &[
    ("anonymous", "default"),
    ("principal", "workspace"),
    ("principal-a", "workspace-a"),
    ("user", "workspace"),
    ("test-principal", "test-workspace"),
    ("owner", "workspace"),
    // `magician/tests/magician_v2_api_integration_test.rs` and the secure-HITL
    // qualification lane beside it.
    ("test-user", "test-workspace"),
    ("integration-user", "integration-workspace"),
];

pub fn activate_conventional_test_scopes_sync(runtime_root: &Path) {
    let store = crate::magician_v2::execution::agentic::run_loop::store::fs::FsLoopStateStore::new(
        runtime_root.to_path_buf(),
    );
    for (principal, workspace) in CONVENTIONAL_TEST_SCOPES {
        // Best effort: a scope already sealed by the same deployment id is a
        // no-op, and a fixture that never touches a scope does not care.
        let _ = store.seal_legacy_writer_cutover_sync(principal, workspace, "test-harness", 1);
    }
}

pub fn build_test_artifact_v2_harness(
    base_root: &Path,
) -> (Arc<ArtifactV2Service>, Arc<MagicianV2Orchestrator>) {
    build_test_artifact_v2_harness_with_llm(base_root, Arc::new(MockQueryAnalysisLLM))
}

/// Exercise the canonical task owner with an explicit model implementation.
/// An unavailable counting implementation proves a mechanical path neither
/// depends on a provider nor silently falls back to the query-analysis mock.
pub fn build_test_artifact_v2_harness_with_llm(
    base_root: &Path,
    llm_service: Arc<dyn QueryAnalysisLLM>,
) -> (Arc<ArtifactV2Service>, Arc<MagicianV2Orchestrator>) {
    let runtime_root = base_root.join("magician_data_v3");
    activate_conventional_test_scopes_sync(&runtime_root);
    let orchestrator = build_test_orchestrator_inner_at_with_llm(
        base_root,
        &runtime_root,
        &runtime_root,
        false,
        llm_service,
    );
    orchestrator.set_full_pause_store(Arc::new(
        crate::magician_v2::execution::agentic::FullPauseStore::with_scoped_v3_persistence_root(
            runtime_root.clone(),
        ),
    ));
    let service = Arc::new(ArtifactV2Service::with_orchestrator(
        runtime_root,
        Arc::clone(&orchestrator),
        MuijStorage::new(base_root.join("muij")),
    ));
    orchestrator.set_artifact_v2_service(Arc::clone(&service));
    (service, orchestrator)
}

pub fn build_test_artifact_v2_service(base_root: &Path) -> Arc<ArtifactV2Service> {
    build_test_artifact_v2_harness(base_root).0
}

/// Wire a real list index into a test service, ready to answer.
///
/// **This exists because the index's read path had no coverage at all.** It
/// reaches a handler through a `OnceLock` only `bin/magician.rs` ever fills,
/// so every test that builds a service directly takes the fallback walk.
/// That cuts both ways: the existing suites are a complete proof the WALK
/// still answers every case, and simultaneously the reason nothing ever
/// exercised a handler choosing the index. This is the seam that closes it.
///
/// Called BEFORE any records exist, so `TaskWriteReconciler` is what keeps the
/// index current from here on — which is how the backend actually runs, and
/// which means a test that creates a task through the service is also
/// exercising the reconciliation. The rebuild over an empty tree is what marks
/// the index ready; without it `is_ready()` is false and every handler would
/// decline.
///
/// **Not `persist_task_record_unlocked`, whatever this said before
/// `e3326b3c3`.** That it is *not* the chokepoint is the entire reason
/// `artifact_v2/task_writes.rs` exists: the claim that it was got made twice
/// and was wrong twice, and both times a task an agent finished kept its old
/// list row and its stale Today projection until a restart.
///
/// Returns the index so a test can assert on it directly, or mark it
/// unready again to prove a handler falls back.
pub fn wire_test_list_index(
    service: &Arc<ArtifactV2Service>,
) -> crate::magician_v2::storage::ListIndex {
    let workspace = service.workspace();
    let index = crate::magician_v2::storage::ListIndex::open(workspace.base_root())
        .expect("a test list index should open");
    index
        .rebuild_from_disk(&workspace.scopes_root())
        .expect("a test list index should rebuild from disk");
    service.set_list_index(index.clone());
    index
}

/// Wire an index that is present but **not ready** — the state every reader
/// meets for the whole of a rebuild, and still meets after a crash partway
/// through one, because the marker is on disk.
///
/// A freshly created index is born unready and stays that way until a
/// rebuild finishes, so this is that state exactly, not a simulation of it.
/// Writes still reconcile it; nothing may read it.
///
/// This is the harder half of the coverage. An unready index that a handler
/// wrongly consulted would not error and would not look empty — it would
/// look like a complete index holding fewer records, which is the one
/// failure mode here a reader could never detect.
pub fn wire_unready_test_list_index(
    service: &Arc<ArtifactV2Service>,
) -> crate::magician_v2::storage::ListIndex {
    let index = crate::magician_v2::storage::ListIndex::open(service.workspace().base_root())
        .expect("a test list index should open");
    assert!(
        !index.is_ready().expect("readiness"),
        "a never-rebuilt index must report itself unready, or this helper wires nothing"
    );
    service.set_list_index(index.clone());
    index
}

pub fn build_test_artifact_v2_service_with_router(base_root: &Path) -> Arc<ArtifactV2Service> {
    let runtime_root = base_root.join("magician_data_v3");
    activate_conventional_test_scopes_sync(&runtime_root);
    let orchestrator =
        build_test_orchestrator_inner_at(base_root, &runtime_root, &runtime_root, true);
    let service = Arc::new(ArtifactV2Service::with_orchestrator(
        runtime_root,
        Arc::clone(&orchestrator),
        MuijStorage::new(base_root.join("muij")),
    ));
    orchestrator.set_artifact_v2_service(Arc::clone(&service));
    service
}
