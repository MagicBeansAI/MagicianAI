//! Exercise the execution seam without compiling the monolith's fixture graph.
//! The production module is compiled here with its private behavioral tests;
//! runtime dependencies still come from the ordinary production library.
pub use magician::magician_v2;

mod config {
    pub use magician::config::*;
    // The production default helpers are crate-private. Keep this test seam
    // tied to their public snapshot instead of duplicating numeric defaults.
    pub fn default_harness_turn_max_seconds() -> u64 {
        magician_v2::execution::plane::HarnessEngineSnapshot::default().turn_max_seconds
    }
    pub fn default_harness_turn_max_tool_calls() -> u32 {
        magician_v2::execution::plane::HarnessEngineSnapshot::default().turn_max_tool_calls
    }
    use crate::magician_v2;
}

// turn_engine reads the chat harness snapshot through `super::chat_turn`.
mod chat_turn {
    pub use magician::magician_v2::execution::plane::chat_turn::*;
}

// Only part of this production file is exercised here; the rest is used by
// the runtime, not dead.
#[allow(dead_code)]
#[path = "../../magician/src/magician_v2/execution/plane/turn_engine.rs"]
mod turn_engine;

#[path = "../../magician/src/magician_v2/execution/plane/usage.rs"]
mod usage;

#[path = "../../magician/src/magician_v2/execution/coding_engine/codex_usage.rs"]
mod codex_usage;

use magician_v2::execution::agentic::{ActionExecutors, AgenticContext};
use magician_v2::execution::plane::{plane_tools_call, PlaneGrant, PlaneTurnStopReason};
use magician_v2::slot_graph::extraction::{
    LlmFunctionCallRequest, LlmFunctionCallResponse, LlmService,
};
use std::sync::{Arc, OnceLock, RwLock};

struct NoLlm;

#[test]
fn long_run_native_metadata_does_not_become_action_approval() {
    use magician_v2::execution::plane::engine::HarnessPauseContinuation;
    let metadata = HarnessPauseContinuation {
        native_session_id: Some("native-session".into()),
        prior_result_note: Some("completed host receipts".into()),
        session_fingerprint: Some("profile".into()),
        turn_tool_calls_spent: 2,
        turn_wall_clock_spent_ms: 500,
        ..Default::default()
    };
    assert!(metadata.is_metadata_only());
    for marker in 0..3 {
        let mut action = metadata.clone();
        match marker {
            0 => action.action_json = Some(String::new()),
            1 => action.action_summary = Some(String::new()),
            _ => action.action_type = Some(String::new()),
        }
        assert!(
            !action.is_metadata_only(),
            "even an empty action marker requires its existing approval path"
        );
    }
}

#[test]
fn long_run_root_owner_authority_binds_native_session_metadata_and_counters() {
    use magician_v2::execution::agentic::{pause_authority_hash, FullPauseData};
    use magician_v2::execution::agentic::{AgenticPauseState, EnvironmentState, UserInputType};
    use magician_v2::execution::plane::engine::HarnessPauseContinuation;
    let dir = tempfile::tempdir().unwrap();
    let grant = run_grant(dir.path(), false);
    let mut state = AgenticPauseState::new(
        4,
        "local goal",
        "finish",
        EnvironmentState::Uninitialized,
        "",
        12000,
        3,
    );
    state.principal = Some("owner".into());
    state.workspace = Some("default".into());
    state.task_id = Some("task-metadata".into());
    state.execution_id = Some("exec-metadata".into());
    state.root_execution_id = state.execution_id.clone();
    state.stateless_parked_segment = state.execution_id.clone();
    state.agent_id = Some("worker".into());
    state.active_owner_agent_id = state.agent_id.clone();
    let mut pause = FullPauseData::new(
        state,
        UserInputType::Text {
            placeholder: None,
            multiline: false,
        },
        grant.executors.as_ref().unwrap().as_ref().clone(),
        Some("Owner checkpoint".into()),
        None,
    );
    pause.harness_pause = Some(HarnessPauseContinuation {
        native_session_id: Some("native-session".into()),
        turn_tool_calls_spent: 2,
        ..Default::default()
    });
    let original = pause_authority_hash(&pause).unwrap();
    pause.harness_pause.as_mut().unwrap().native_session_id = Some("other-session".into());
    assert_ne!(pause_authority_hash(&pause).unwrap(), original);
    pause.harness_pause.as_mut().unwrap().native_session_id = Some("native-session".into());
    assert_eq!(pause_authority_hash(&pause).unwrap(), original);
    pause.harness_pause.as_mut().unwrap().turn_tool_calls_spent = 0;
    assert_ne!(
        pause_authority_hash(&pause).unwrap(),
        original,
        "metadata recovery cannot reset spent allowances"
    );
}

#[tokio::test]
async fn long_run_work_input_control_is_discoverable_typed_and_stops_further_calls() {
    use magician_v2::execution::agentic::Decision;
    use magician_v2::execution::plane::catalog::{plane_tool_search, plane_tools_list};
    let dir = tempfile::tempdir().unwrap();
    let grant = run_grant(dir.path(), false);
    let tools = plane_tools_list(&grant);
    for name in ["yield", "need_user_input"] {
        assert_eq!(tools.iter().filter(|tool| tool["name"] == name).count(), 1);
        let found = plane_tool_search(
            &grant.tool_index,
            &grant,
            &serde_json::json!({"query":format!("select:{name}")}),
        )
        .await;
        let body: serde_json::Value =
            serde_json::from_str(found["content"][0]["text"].as_str().unwrap()).unwrap();
        assert!(body["matches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == name));
    }
    let bad = plane_tools_call(
        &grant,
        "need_user_input",
        &serde_json::json!({"question":"missing type"}),
    )
    .await;
    assert_eq!(bad["isError"], true);
    assert_eq!(grant.turn_stop_reason(), None);
    let response = plane_tools_call(&grant, "need_user_input", &serde_json::json!({"question":"Owner checkpoint", "input_type":"text", "hint":"Local diagnostic"})).await;
    assert_eq!(response["isError"], false);
    assert_eq!(
        grant.turn_stop_reason(),
        Some(PlaneTurnStopReason::RunControl)
    );
    let clone = grant.clone();
    let Some(Decision::NeedUserInput {
        question,
        input_type,
        ..
    }) = clone.take_run_control()
    else {
        panic!("typed input was not captured");
    };
    assert_eq!(question, "Owner checkpoint");
    assert!(matches!(
        input_type,
        magician_v2::execution::agentic::UserInputType::Text { .. }
    ));
    assert!(grant.take_run_control().is_none());
    assert_eq!(
        plane_tools_call(&grant, "tool_search", &serde_json::json!({"query":"files"})).await
            ["isError"],
        true
    );
}

#[tokio::test]
async fn long_run_work_yield_preserves_open_requirements_without_running_an_action() {
    use magician_v2::execution::agentic::{
        yield_decision::{dispose_yield, YieldDisposition},
        Decision,
    };
    let dir = tempfile::tempdir().unwrap();
    let grant = run_grant(dir.path(), false);
    let response = plane_tools_call(&grant, "yield", &serde_json::json!({
        "summary":"Four outputs exist; the rest remain open.","completed":["four outputs"],"open":["remaining outputs"],"blockers":[]
    })).await;
    assert_eq!(response["isError"], false);
    let Some(Decision::Yield { payload }) = grant.take_run_control() else {
        panic!("missing governed yield");
    };
    assert!(matches!(
        dispose_yield(&payload),
        YieldDisposition::PartialSuccess { .. }
    ));
    assert_eq!(payload.open, ["remaining outputs"]);
    assert_eq!(grant.turn_tool_calls_spent(), 0);
    assert!(grant.take_pending_approval().is_none());
}

#[tokio::test]
async fn long_run_terminal_grant_cannot_discover_or_capture_work_lifecycle_tools() {
    use magician_v2::execution::plane::catalog::{plane_tool_search, plane_tools_list};
    let dir = tempfile::tempdir().unwrap();
    let work = run_grant(dir.path(), false);
    let terminal = PlaneGrant::for_terminal(
        work.ctx.clone(),
        "synthetic-terminal".into(),
        Vec::new(),
        work.tool_index.clone(),
    );
    assert!(!terminal.supports_run_controls());
    assert!(plane_tools_list(&terminal)
        .iter()
        .all(|tool| !["yield", "need_user_input"].contains(&tool["name"].as_str().unwrap())));
    let found = plane_tool_search(
        &terminal.tool_index,
        &terminal,
        &serde_json::json!({"query":"select:need_user_input"}),
    )
    .await;
    let body: serde_json::Value =
        serde_json::from_str(found["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(body["matches"].as_array().unwrap().is_empty());
    assert_eq!(
        plane_tools_call(
            &terminal,
            "need_user_input",
            &serde_json::json!({"question":"Must not ask", "input_type":"text"})
        )
        .await["isError"],
        true
    );
}

#[tokio::test]
async fn long_run_24_hour_slice_boundary_and_active_dispatch_use_injected_time() {
    use std::time::{Duration, Instant};
    let dir = tempfile::tempdir().unwrap();
    let start = Instant::now();
    let grant = run_grant(dir.path(), false)
        .with_turn_tool_budget(3, 0)
        .with_turn_time_budget_at(Duration::from_secs(86400), start);
    assert!(!grant.turn_budget_exhausted_at(start + Duration::from_secs(86399)));
    assert!(grant.turn_budget_exhausted_at(start + Duration::from_secs(86400)));
    let unlimited = grant
        .clone()
        .with_turn_time_budget_at(Duration::ZERO, start);
    assert!(!unlimited.turn_budget_exhausted_at(start + Duration::from_secs(86400 * 500)));
    let impossible = grant.clone().with_turn_time_budget_at(Duration::MAX, start);
    assert!(impossible.turn_budget_exhausted_at(start));
    let activity = grant.dispatch_activity.clone();
    let lease = grant.clone().lock_dispatch().await;
    assert!(!activity.idle_expired(
        start,
        start + Duration::from_secs(86400 * 500),
        Duration::from_secs(300)
    ));
    drop(lease);
    let finished = Instant::now();
    assert!(!activity.idle_expired(start, finished, Duration::from_secs(300)));
    assert!(activity.idle_expired(
        start,
        finished + Duration::from_secs(301),
        Duration::from_secs(300)
    ));
}

#[tokio::test]
async fn long_run_elapsed_time_slice_refuses_the_next_effect_without_waiting_hours() {
    use std::time::{Duration, Instant};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input.txt");
    std::fs::write(&path, "outside slice").unwrap();
    let grant = run_grant(dir.path(), false)
        .with_turn_time_budget_at(Duration::from_millis(1), Instant::now());
    tokio::time::sleep(Duration::from_millis(2)).await;
    let response = plane_tools_call(
        &grant,
        "read_file",
        &serde_json::json!({
            "file_path": path,
        }),
    )
    .await;
    assert_eq!(response["isError"], true);
    assert_eq!(
        grant.turn_stop_reason(),
        Some(PlaneTurnStopReason::TurnBudgetSpent)
    );
    assert!(!response.to_string().contains("outside slice"));
    assert_eq!(grant.turn_tool_calls_spent(), 0);
}

#[test]
fn long_run_coding_driver_unlimited_never_inherits_a_short_default() {
    use magician_v2::execution::plane::engine::harness_driver_timeout;
    use std::time::Duration;
    assert_eq!(
        harness_driver_timeout(Duration::ZERO),
        Duration::from_secs(365 * 86400)
    );
    assert_eq!(
        harness_driver_timeout(Duration::from_secs(86400)),
        Duration::from_secs(86400)
    );
    assert_eq!(
        harness_driver_timeout(Duration::from_millis(200)),
        Duration::from_millis(200)
    );
}
#[async_trait::async_trait]
impl LlmService for NoLlm {
    async fn call_function(
        &self,
        _: LlmFunctionCallRequest,
    ) -> anyhow::Result<LlmFunctionCallResponse> {
        panic!("provider-free execution contracts must not call an LLM")
    }
}

fn run_grant(dir: &std::path::Path, approval: bool) -> PlaneGrant {
    run_grant_for_files(dir, approval, false)
}

fn run_grant_for_files(dir: &std::path::Path, approval: bool, writable: bool) -> PlaneGrant {
    use magician_v2::agents::{AgentDefinitionStore, AgentMemoryResolver, AgentStorage};
    use magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    use magician_v2::execution::agent_resources::AgentResources;
    use magician_v2::execution::capability::CapabilityRegistry;
    use magician_v2::execution::compiled_providers::{
        default_compiled_handler_registry, embedded_compiled_pack_defs_ref, FileCapabilityProvider,
        GenericCompiledProvider,
    };
    use magician_v2::execution::flat_loop::build_tool_index;
    use magician_v2::prompts::json_storage::JsonStorageConfig;
    use magician_v2::prompts::{JsonPromptStorage, PromptManager};
    let storage = JsonPromptStorage::new(JsonStorageConfig {
        storage_dir: dir.join("prompts"),
        ..Default::default()
    })
    .unwrap();
    let mut executors = ActionExecutors::new(
        Arc::new(NoLlm),
        Arc::new(PromptManager::new(Arc::new(storage))),
    );
    executors
        .file_sandbox
        .allowed_roots
        .push(dir.to_string_lossy().into_owned());
    let resources = Arc::new(AgentResources {
        magician_config: Arc::new(RwLock::new(Default::default())),
        memory_resolver: Arc::new(AgentMemoryResolver::new(dir)),
        agent_definition_store: Arc::new(AgentDefinitionStore::new(AgentStorage::new(dir))),
        artifact_workspace: ArtifactV2Workspace::new(dir),
        artifact_v2_service: None,
        event_broadcaster: None,
        operation_llm_router: None,
        secret_store_resolver: None,
        content_acquisition_resolver: Arc::new(RwLock::new(None)),
        file_sandbox: executors.file_sandbox.clone(),
        relentless_research: None,
        tool_index: Arc::new(OnceLock::new()),
        user_request_service: None,
        agent_runtime: None,
    });
    let definitions: Vec<_> = embedded_compiled_pack_defs_ref()
        .iter()
        .filter(|def| def.name == "read_file" || writable && def.name == "files")
        .cloned()
        .collect();
    assert_eq!(definitions.len(), if writable { 2 } else { 1 });
    let index = Arc::new(build_tool_index(&definitions));
    let registry = Arc::new(CapabilityRegistry::new());
    for definition in definitions {
        if definition.name == "files" {
            registry.register(Arc::new(
                FileCapabilityProvider::new(executors.file_sandbox.clone())
                    .with_pack_def(definition.clone()),
            ));
            registry.set_pack_definition("files", definition);
            continue;
        }
        registry.register(Arc::new(
            GenericCompiledProvider::new(
                definition.name.clone(),
                default_compiled_handler_registry()
                    .get(&definition.name)
                    .unwrap(),
                resources.clone(),
                None,
            )
            .with_pack_def(definition.clone()),
        ));
        registry.set_pack_definition(&definition.name.clone(), definition);
    }
    executors.capability_registry = Some(registry);
    let mut ctx = AgenticContext::default();
    ctx.principal = Some("owner".into());
    ctx.workspace = Some("default".into());
    ctx.agent_id = Some("personal-assistant".into());
    ctx.tool_index = Some(index.clone());
    if approval {
        ctx.approval_rules = vec![magician_v2::agents::ApprovalRule {
            tool: "read_file".into(),
            action: magician_v2::agents::ActionPattern::Single("*".into()),
            when: None,
            ttl_secs: None,
        }];
    }
    let grant = PlaneGrant::for_run(ctx, Arc::new(executors), uuid::Uuid::new_v4().to_string());
    assert!(Arc::ptr_eq(&grant.tool_index, &index));
    grant
}

#[tokio::test]
async fn a_run_grant_reads_through_the_real_file_provider_and_enforces_its_turn_bound() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixture.txt");
    std::fs::write(&path, "hidden-marker\n").unwrap();
    let grant = run_grant(dir.path(), false).with_turn_tool_budget(1, 0);
    assert!(!grant.permits("write_file"));
    let args = serde_json::json!({"file_path":path});
    let result = plane_tools_call(&grant, "read_file", &args).await;
    assert_ne!(
        result.get("isError").and_then(serde_json::Value::as_bool),
        Some(true),
        "{result}"
    );
    assert!(result.to_string().contains("hidden-marker"), "{result}");
    assert_eq!(grant.turn_tool_calls_spent(), 1);
    let exhausted = plane_tools_call(&grant, "read_file", &args).await;
    assert_eq!(exhausted["isError"], true);
    assert_eq!(
        grant.turn_stop_reason(),
        Some(PlaneTurnStopReason::TurnBudgetSpent)
    );
    assert!(!grant.cancellation_token.as_ref().unwrap().is_cancelled());
}

#[tokio::test]
async fn an_execution_approval_is_captured_before_the_provider_reads() {
    let dir = tempfile::tempdir().unwrap();
    let grant = run_grant(dir.path(), true);
    let result = plane_tools_call(
        &grant,
        "read_file",
        &serde_json::json!({"file_path":dir.path().join("does-not-exist")}),
    )
    .await;
    assert_eq!(
        result["_meta"]["planeTurnStop"], "needs_approval",
        "{result}"
    );
    assert_eq!(grant.turn_tool_calls_spent(), 0);
    assert!(grant.take_pending_approval().is_some());
}

#[tokio::test]
async fn launch_attenuation_blocks_a_registered_provider_before_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secret.txt");
    std::fs::write(&path, "must-not-be-read").unwrap();
    let mut grant = run_grant(dir.path(), false).with_turn_tool_budget(1, 0);
    grant.ctx.plane_allowed_capability_names = Some(vec!["read_file".into()]);
    assert!(grant.permits("read_file"));
    grant
        .ctx
        .plane_denied_capability_names
        .push("read_file".into());
    let result =
        plane_tools_call(&grant, "read_file", &serde_json::json!({"file_path":path})).await;
    assert_eq!(result["isError"], true, "{result}");
    assert!(!result.to_string().contains("must-not-be-read"));
    assert_eq!(grant.turn_tool_calls_spent(), 0);
    grant.ctx.plane_denied_capability_names.clear();
    grant.ctx.plane_allowed_capability_names = Some(Vec::new());
    assert!(!grant.permits("read_file"));
    assert!(!grant.permits("tool_search"));
}

#[tokio::test]
async fn the_live_fixture_can_write_through_its_governed_file_provider() {
    let dir = tempfile::tempdir().unwrap();
    let grant = run_grant_for_files(dir.path(), false, true);
    let path = dir.path().join("preflight.json");
    let discovery = plane_tools_call(
        &grant,
        "tool_search",
        &serde_json::json!({"query":"select:files"}),
    )
    .await;
    assert_ne!(
        discovery
            .get("isError")
            .and_then(serde_json::Value::as_bool),
        Some(true),
        "{discovery}"
    );
    let result = plane_tools_call(
        &grant,
        "files",
        &serde_json::json!({
            "action":"write", "path":path, "content":"{\"line_count\":4}",
        }),
    )
    .await;
    assert_ne!(
        result.get("isError").and_then(serde_json::Value::as_bool),
        Some(true),
        "{result}"
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), "{\"line_count\":4}");
}

#[tokio::test]
async fn a_static_harness_can_call_deferred_tools_from_its_initial_catalog() {
    use magician_v2::execution::plane::catalog::plane_tools_list_configured;
    let dir = tempfile::tempdir().unwrap();
    let mut grant = run_grant_for_files(dir.path(), false, true);
    let listed = |grant: &PlaneGrant| {
        plane_tools_list_configured(grant, None)
            .iter()
            .any(|tool| tool["name"] == "files")
    };
    assert!(!listed(&grant));
    turn_engine::preload_run_tools(&mut grant, false);
    assert!(listed(&grant));
    assert!(grant.preloaded_deferred);
    // Selecting another tool cannot invalidate the client's fixed snapshot.
    plane_tools_call(
        &grant,
        "tool_search",
        &serde_json::json!({"query":"select:read_file"}),
    )
    .await;
    assert!(listed(&grant));

    let mut attenuated = run_grant_for_files(dir.path(), false, true);
    attenuated
        .ctx
        .plane_denied_capability_names
        .push("files".into());
    turn_engine::preload_run_tools(&mut attenuated, false);
    assert!(!listed(&attenuated));
    assert!(!attenuated.loaded_tool_names().contains("files"));
    let mut dynamic = run_grant_for_files(dir.path(), false, true);
    turn_engine::preload_run_tools(&mut dynamic, true);
    assert!(!listed(&dynamic));
    assert!(!dynamic.preloaded_deferred);
}

#[derive(Default)]
struct RecordedEvents(std::sync::Mutex<Vec<serde_json::Value>>);

impl magician_v2::artifact_v2::RuntimeCanonicalEventSink for RecordedEvents {
    fn emit(
        &self,
        _: magician_v2::artifact_v2::CanonicalEventScope,
        event_type: magician_v2::artifact_v2::ArtifactV2EventType,
        payload: serde_json::Value,
    ) {
        self.0.lock().unwrap().push(serde_json::json!({
            "event_type": event_type.as_str(), "payload": payload,
        }));
    }
}

/// The real HTTP door, real file provider and installed CLI, without a
/// shared-runtime restart or a fake model. Run explicitly; this uses the
/// operator's existing provider login and incurs provider usage.
#[actix_web::test]
#[ignore = "live provider check; make test-execution-harness-adapters-live"]
async fn installed_harnesses_execute_governed_file_flow() {
    use actix_web::{web, App, HttpServer};
    use magician_api::plane_api::{
        plane_mcp_delete_handler, plane_mcp_handler, plane_mcp_sse_handler,
    };
    use magician_v2::execution::agentic::types::{
        execution_token_budget_snapshot, with_execution_token_meter, EnvironmentState,
        ExecutionHistory, LoopProtectiveState,
    };
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/plane/mcp", listener.local_addr().unwrap());
    let server = HttpServer::new(|| {
        App::new().service(
            web::resource("/plane/mcp")
                .route(web::post().to(plane_mcp_handler))
                .route(web::get().to(plane_mcp_sse_handler))
                .route(web::delete().to(plane_mcp_delete_handler)),
        )
    })
    .workers(1)
    .listen(listener)
    .unwrap()
    .run();
    let handle = server.handle();
    actix_web::rt::spawn(server);
    turn_engine::install_harness_engine_snapshot(turn_engine::HarnessEngineSnapshot {
        plane_endpoint: endpoint,
        turn_max_seconds: 180,
        turn_max_tool_calls: 12,
        ..Default::default()
    });
    let engines: Vec<String> = std::env::var("HARNESS_EXECUTION_LIVE_ENGINES")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            value
                .split(',')
                .map(|name| name.trim().to_string())
                .collect()
        })
        .unwrap_or_else(|| {
            magician_v2::execution::plane::roster_with_install_status()
                .into_iter()
                .filter(|(_, installed)| *installed)
                .map(|(name, _)| name.to_string())
                .collect()
        });
    assert!(!engines.is_empty(), "no installed harnesses to evaluate");
    let report_dir = std::env::var_os("HARNESS_EXECUTION_ADAPTER_REPORT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("execution-harness-adapters"));
    std::fs::create_dir_all(&report_dir).unwrap();
    let mut reports = Vec::new();
    for engine in engines {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("input.txt");
        let destination = dir.path().join("result.json");
        let marker = uuid::Uuid::new_v4().simple().to_string();
        std::fs::write(&source, format!("amber\n{marker}\ncedar\nplum\n")).unwrap();
        let grant = run_grant_for_files(dir.path(), false, true);
        let mut ctx = grant.ctx.clone();
        let execution_id = uuid::Uuid::new_v4().to_string();
        ctx.execution_id = Some(execution_id.clone());
        ctx.task_id = Some(format!("fixture-{execution_id}"));
        ctx.plan_id = Some("fixture-plan".into());
        ctx.step_id = Some("fixture-step".into());
        ctx.harness_engine = Some(engine.clone());
        ctx.goal = format!("Use the governed read_file tool to read {}. Return the second line and exact line count. Do not use native tools or delegate.", source.display());
        ctx.goal.push_str(&format!(" Discover the governed files tool with tool_search if necessary, then use its write action to write {} as JSON with exactly two fields: marker (the second line) and line_count (the numeric line count).", destination.display()));
        let events = Arc::new(RecordedEvents::default());
        let executors = grant
            .executors
            .as_ref()
            .unwrap()
            .as_ref()
            .clone()
            .with_canonical_event_sink(events.clone())
            .with_canonical_event_scope(magician_v2::artifact_v2::CanonicalEventScope {
                principal: "owner".into(),
                workspace: "default".into(),
                task_id: ctx.task_id.clone().unwrap(),
                execution_id,
                ui_thread_id: String::new(),
            });
        let started = std::time::Instant::now();
        let (decision, tokens) = with_execution_token_meter(0, 2_000_000, async {
            let decision = turn_engine::maybe_harness_decide(
                &ctx,
                &executors,
                &ExecutionHistory::default(),
                &mut LoopProtectiveState::default(),
                &EnvironmentState::Uninitialized,
                &[],
                None,
                1,
            )
            .await;
            (decision, execution_token_budget_snapshot())
        })
        .await;
        let rows = events.0.lock().unwrap().clone();
        let answer = match &decision {
            Ok(Some(magician_v2::execution::agentic::Decision::Completed { evidence, .. })) => {
                evidence.clone().unwrap_or_default()
            },
            _ => String::new(),
        };
        let governed = ["pack:read_file(", "pack:files("].iter().all(|target| {
            rows.iter().any(|row| {
                row["event_type"] == "tool.succeeded"
                    && row["payload"]["target"]
                        .as_str()
                        .is_some_and(|name| name.starts_with(target))
            })
        });
        let metered = tokens.is_some_and(|(spent, max)| spent > 0 && spent < max);
        let attributed = rows.iter().any(|row| {
            row["payload"]["kind"] == "harness_turn_settled" && row["payload"]["engine"] == engine
        });
        let written = std::fs::read(&destination)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        let passed = written == Some(serde_json::json!({"marker": marker, "line_count": 4}))
            && answer.contains(&marker)
            && answer
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|word| word == "4")
            && governed
            && metered
            && attributed;
        let report = serde_json::json!({
            "engine": engine, "pass": passed, "answer": answer, "decision": format!("{decision:?}"),
            "tokens": tokens, "governed": governed, "attributed": attributed, "written": written,
            "latency_ms": started.elapsed().as_millis(), "events": rows,
        });
        eprintln!(
            "{}: pass={passed}, governed={governed}, metered={metered}, attributed={attributed}",
            engine
        );
        reports.push(report);
        turn_engine::forget_terminal_harness_continuation(ctx.execution_id.as_deref().unwrap());
        std::fs::write(
            report_dir.join("report.json"),
            serde_json::to_vec_pretty(&reports).unwrap(),
        )
        .unwrap();
    }
    handle.stop(true).await;
    assert!(
        reports.iter().all(|row| row["pass"] == true),
        "see {}",
        report_dir.join("report.json").display()
    );
}

#[derive(Debug)]
struct CheckpointHarness {
    path: std::path::PathBuf,
    cancel_at_exit: bool,
}
struct CheckpointSession {
    grant: String,
    home: std::path::PathBuf,
    resumed: bool,
    path: std::path::PathBuf,
    cancel: Option<tokio_util::sync::CancellationToken>,
}
impl std::fmt::Debug for CheckpointSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CheckpointSession")
    }
}
#[async_trait::async_trait]
impl magician_v2::execution::plane::engine::HarnessEngine for CheckpointHarness {
    fn name(&self) -> &'static str {
        "codex"
    }
    fn capabilities(&self) -> magician_v2::execution::plane::engine::HarnessCapabilities {
        magician_v2::execution::plane::engine::HarnessCapabilities {
            supports_resume: true,
            ..Default::default()
        }
    }
    async fn start(
        &self,
        req: &magician_v2::execution::plane::engine::HarnessSessionRequest,
    ) -> Result<
        Box<dyn magician_v2::execution::plane::engine::HarnessSession>,
        magician_v2::execution::plane::engine::HarnessError,
    > {
        assert_eq!(
            req.turn_timeout,
            std::time::Duration::ZERO,
            "internal slices must not interrupt an active effect"
        );
        let home = req.native_home.clone().expect("resumable native home");
        let resumed = req.resume_session_id.as_deref() == Some("checkpoint-session");
        if resumed {
            assert!(home.join("session-evidence").is_file());
        }
        Ok(Box::new(CheckpointSession {
            grant: req.grant.clone(),
            home,
            resumed,
            path: self.path.clone(),
            cancel: self.cancel_at_exit.then(|| req.cancel.clone().unwrap()),
        }))
    }
}
#[async_trait::async_trait]
impl magician_v2::execution::plane::engine::HarnessSession for CheckpointSession {
    async fn turn(
        &mut self,
        input: &magician_v2::execution::plane::engine::HarnessTurnInput,
        _: &magician_v2::execution::plane::engine::HarnessStreamSink,
    ) -> Result<
        magician_v2::execution::plane::engine::HarnessTurnSettled,
        magician_v2::execution::plane::engine::HarnessError,
    > {
        use magician_v2::execution::plane::{engine::*, grant::plane_grant_registry};
        let grant = plane_grant_registry()
            .resolve_run_scoped(&self.grant)
            .await
            .unwrap();
        let result =
            if !self.resumed {
                assert!(
                    !self.path.exists(),
                    "a completed write must not be replayed"
                );
                let result = plane_tools_call(
                &grant,
                "files",
                &serde_json::json!({"action":"write","path":self.path,"content":"written-once"}),
            )
            .await;
                assert_ne!(result["isError"], true, "{result}");
                std::fs::write(
                    self.home.join("session-evidence"),
                    "native-session-preserved",
                )
                .unwrap();
                let denied = plane_tools_call(
                    &grant,
                    "read_file",
                    &serde_json::json!({"file_path":self.path}),
                )
                .await;
                assert_eq!(denied["isError"], true);
                assert_eq!(
                    grant.turn_stop_reason(),
                    Some(PlaneTurnStopReason::TurnBudgetSpent)
                );
                "unfinished internal slice"
            } else {
                assert!(input.text.contains("Completed governed calls"));
                assert!(
                    input.text.contains("written-once"),
                    "host receipts retain effects even when CLI result text is incomplete"
                );
                let result = plane_tools_call(
                    &grant,
                    "read_file",
                    &serde_json::json!({"file_path":self.path}),
                )
                .await;
                assert_ne!(result["isError"], true, "{result}");
                assert_eq!(
                    grant.turn_tool_calls_spent(),
                    1,
                    "new slice gets a fresh call allowance"
                );
                let completion = plane_tools_call(&grant, "yield", &serde_json::json!({
                "summary":"written-once", "completed":["the exact local write was read back"],
                "open":[], "blockers":[]
            })).await;
                assert_eq!(completion["isError"], false);
                "written-once"
            };
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
        // Opaque CLI teardown can remove the registry entry before returning.
        // The host must retain the actual gate outcome rather than publish a
        // partial response as completed.
        plane_grant_registry().revoke(&self.grant).await;
        Ok(HarnessTurnSettled {
            assistant_text: result.into(),
            stop_reason: HarnessStopReason::Settled,
            usage: Some(HarnessUsage {
                cost_usd: Some(if self.resumed { 0.75 } else { 1.25 }),
                ..Default::default()
            }),
            native_session_id: Some("checkpoint-session".into()),
        })
    }
    async fn shutdown(&mut self) {}
}

#[tokio::test]
async fn long_run_internal_slices_resume_without_repeating_effects_or_resetting_cost() {
    use magician_v2::execution::agentic::types::{
        EnvironmentState, ExecutionHistory, LoopProtectiveState,
    };
    use magician_v2::execution::plane::engine_pin::RunEnginePin;
    let dir = tempfile::tempdir().unwrap();
    let grant = run_grant_for_files(dir.path(), false, true);
    let mut ctx = grant.ctx.clone();
    ctx.execution_id = Some(uuid::Uuid::new_v4().to_string());
    ctx = ctx.with_run_engine_pin(Some(RunEnginePin {
        engine: "codex".into(),
        harness_model: "default".into(),
        pi_profile: None,
    }));
    let prior = turn_engine::harness_engine_snapshot();
    let mut snapshot = prior.clone();
    snapshot.turn_max_tool_calls = 1;
    turn_engine::install_harness_engine_snapshot(snapshot);
    let mut protective = LoopProtectiveState::default();
    let path = dir.path().join("effect.txt");
    let engine = Arc::new(CheckpointHarness {
        path: path.clone(),
        cancel_at_exit: false,
    });
    let first = turn_engine::maybe_harness_decide_with_engine(
        &ctx,
        grant.executors.as_ref().unwrap(),
        &ExecutionHistory::default(),
        &mut protective,
        &EnvironmentState::Uninitialized,
        &[],
        None,
        1,
        Some(engine.clone()),
    )
    .await
    .unwrap()
    .unwrap();
    let checkpoint = match first {
        magician_v2::execution::agentic::Decision::HarnessCheckpoint { continuation, .. } => {
            continuation
        },
        other => panic!("internal boundary became terminal: {other:?}"),
    };
    assert_eq!(protective.cumulative_run_cost_usd, 1.25);
    assert_eq!(checkpoint.turn_tool_calls_spent, 0);
    assert_eq!(checkpoint.turn_wall_clock_spent_ms, 0);
    let home = checkpoint.native_home.clone().unwrap();
    // The durable phase checkpoint round-trips native state without replaying
    // an action or granting more overall budget.
    let checkpoint: magician_v2::execution::plane::engine::HarnessPauseContinuation =
        serde_json::from_value(serde_json::to_value(checkpoint).unwrap()).unwrap();
    // Lose process-local state while preserving the exact persisted home:
    // without rehydration the next start is cold and replays the write.
    let retained_home = dir.path().join("retained-native-home");
    std::fs::rename(&home, &retained_home).unwrap();
    turn_engine::forget_terminal_harness_continuation(ctx.execution_id.as_deref().unwrap());
    assert!(
        turn_engine::peek_harness_pause_continuation(ctx.execution_id.as_deref().unwrap())
            .is_none()
    );
    std::fs::rename(&retained_home, &home).unwrap();
    let stored_usage = serde_json::to_value(&protective).unwrap();
    protective = LoopProtectiveState::default();
    assert_eq!(protective.cumulative_run_cost_usd, 0.0);
    protective = serde_json::from_value(stored_usage).unwrap();
    turn_engine::restore_harness_checkpoint(&ctx, &checkpoint).await;
    let second = turn_engine::maybe_harness_decide_with_engine(
        &ctx,
        grant.executors.as_ref().unwrap(),
        &ExecutionHistory::default(),
        &mut protective,
        &EnvironmentState::Uninitialized,
        &[],
        None,
        2,
        Some(engine),
    )
    .await
    .unwrap()
    .unwrap();
    let magician_v2::execution::agentic::Decision::Yield { payload } = second else {
        panic!("completion must come from the governed yield");
    };
    assert!(matches!(
        magician_v2::execution::agentic::dispose_yield(&payload),
        magician_v2::execution::agentic::YieldDisposition::Completed { .. }
    ));
    assert_eq!(payload.summary, "written-once");
    assert_eq!(protective.cumulative_run_cost_usd, 2.0);
    assert_eq!(std::fs::read_to_string(path).unwrap(), "written-once");
    turn_engine::forget_terminal_harness_continuation(ctx.execution_id.as_deref().unwrap());
    assert!(!home.exists(), "terminal cleanup owns the native home");
    turn_engine::install_harness_engine_snapshot(prior);
}

#[tokio::test]
async fn long_run_owner_cancel_beats_an_internal_checkpoint() {
    use magician_v2::execution::agentic::types::{
        EnvironmentState, ExecutionHistory, LoopProtectiveState,
    };
    use magician_v2::execution::plane::engine_pin::RunEnginePin;
    let dir = tempfile::tempdir().unwrap();
    let grant = run_grant_for_files(dir.path(), false, true);
    let mut ctx = grant.ctx.clone();
    ctx.execution_id = Some(uuid::Uuid::new_v4().to_string());
    ctx = ctx.with_run_engine_pin(Some(RunEnginePin {
        engine: "codex".into(),
        harness_model: "default".into(),
        pi_profile: None,
    }));
    let prior = turn_engine::harness_engine_snapshot();
    let mut snapshot = prior.clone();
    snapshot.turn_max_tool_calls = 1;
    turn_engine::install_harness_engine_snapshot(snapshot);
    let cancel = tokio_util::sync::CancellationToken::new();
    let decision = turn_engine::maybe_harness_decide_with_engine(
        &ctx,
        grant.executors.as_ref().unwrap(),
        &ExecutionHistory::default(),
        &mut LoopProtectiveState::default(),
        &EnvironmentState::Uninitialized,
        &[],
        Some(&cancel),
        1,
        Some(Arc::new(CheckpointHarness {
            path: dir.path().join("effect.txt"),
            cancel_at_exit: true,
        })),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        decision,
        magician_v2::execution::agentic::Decision::Failed { .. }
    ));
    assert!(cancel.is_cancelled());
    turn_engine::forget_terminal_harness_continuation(ctx.execution_id.as_deref().unwrap());
    turn_engine::install_harness_engine_snapshot(prior);
}

#[cfg(unix)]
async fn short_process_turn(
    quiet_for: &str,
    idle_ms: u64,
    deadline_ms: u64,
    hold_dispatch: bool,
    cancel_after_ms: Option<u64>,
) -> magician_v2::execution::plane::engine::HarnessTurnSettled {
    use magician_v2::execution::plane::{
        engine::*, engines::CodexExecEngine, grant::plane_grant_registry,
    };
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    // Auth isolation is local and synthetic: this fixture requires no operator
    // sign-in, provider credential, or provider request.
    struct RestoreCodexHome(Option<std::ffi::OsString>);
    impl Drop for RestoreCodexHome {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => std::env::set_var("CODEX_HOME", value),
                None => std::env::remove_var("CODEX_HOME"),
            }
        }
    }
    let _restore_home = RestoreCodexHome(std::env::var_os("CODEX_HOME"));
    let fixture_home = dir.path().join("fixture-operator-home");
    std::fs::create_dir_all(&fixture_home).unwrap();
    std::fs::write(fixture_home.join("auth.json"), "{}").unwrap();
    std::env::set_var("CODEX_HOME", fixture_home);
    let program = dir.path().join("controlled-harness");
    std::fs::write(&program, format!("#!/bin/sh\nprintf '%s\\n' '{{\"type\":\"thread.started\",\"thread_id\":\"controlled-thread\"}}'\n/bin/sleep {quiet_for}\nprintf '%s\\n' '{{\"type\":\"item.completed\",\"item\":{{\"type\":\"agent_message\",\"text\":\"quiet operation completed\"}}}}'\n")).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    let grant = run_grant(dir.path(), false);
    let lease = if hold_dispatch {
        Some(grant.clone().lock_dispatch().await)
    } else {
        None
    };
    let token = plane_grant_registry().mint(grant).await;
    let cancel = tokio_util::sync::CancellationToken::new();
    let req = HarnessSessionRequest {
        endpoint: PlaneEndpoint {
            url: "http://127.0.0.1:1/unused".into(),
        },
        grant: token,
        system_prompt: "controlled driver qualification".into(),
        model: None,
        pi_profile: None,
        pi_images: vec![],
        cwd: dir.path().into(),
        env_allowlist: vec![],
        cancel: Some(cancel.clone()),
        resume_session_id: None,
        turn_timeout: Duration::from_millis(deadline_ms),
        turn_idle_timeout: (idle_ms > 0).then(|| Duration::from_millis(idle_ms)),
        native_home: Some(dir.path().join("native-session")),
        unrestricted: false,
    };
    let engine = CodexExecEngine { binary: program };
    let mut session = engine.start(&req).await.unwrap();
    let cancel_task = cancel_after_ms.map(|ms| {
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            cancel.cancel();
        })
    });
    let outcome = session
        .turn(
            &HarnessTurnInput {
                text: "finish controlled operation".into(),
                operator_steer: vec![],
            },
            &HarnessStreamSink::drain(),
        )
        .await
        .unwrap();
    session.shutdown().await;
    drop(lease);
    if let Some(task) = cancel_task {
        task.await.unwrap();
    }
    outcome
}

#[cfg(unix)]
#[tokio::test]
async fn long_run_real_child_quiet_dispatch_does_not_trip_stdout_idle() {
    use magician_v2::execution::plane::engine::HarnessStopReason;
    let outcome = short_process_turn("0.25", 40, 2000, true, None).await;
    assert_eq!(outcome.stop_reason, HarnessStopReason::Settled);
    assert_eq!(outcome.assistant_text, "quiet operation completed");
}

#[cfg(unix)]
#[tokio::test]
async fn long_run_real_child_stalls_without_active_dispatch() {
    use magician_v2::execution::plane::engine::HarnessStopReason;
    let outcome = short_process_turn("10", 80, 2000, false, None).await;
    assert_eq!(outcome.stop_reason, HarnessStopReason::Stalled);
}

#[cfg(unix)]
#[tokio::test]
async fn long_run_real_child_keeps_cancellation_and_explicit_driver_deadline() {
    use magician_v2::execution::plane::engine::HarnessStopReason;
    let cancelled = short_process_turn("10", 40, 2000, true, Some(100)).await;
    assert_eq!(cancelled.stop_reason, HarnessStopReason::Cancelled);
    let bounded = short_process_turn("10", 0, 100, false, None).await;
    assert_eq!(bounded.stop_reason, HarnessStopReason::TurnBudgetSpent);
    let unlimited = short_process_turn("0.1", 0, 0, false, None).await;
    assert_eq!(unlimited.stop_reason, HarnessStopReason::Settled);
}
