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

/// Exercise the shared rail's real planner transport against an installed CLI.
/// The fixture exposes only a proposal collector; no work executor is granted.
#[actix_web::test]
#[ignore = "live provider check; make test-decision-planner-live"]
async fn installed_harness_returns_a_proposal_without_work_authority() {
    use actix_web::{web, App, HttpServer};
    use magician_api::plane_api::{
        plane_mcp_delete_handler, plane_mcp_handler, plane_mcp_sse_handler,
    };
    use magician_v2::execution::plane::{decision_planner, HarnessEngineSnapshot};
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
    let previous = magician_v2::execution::plane::harness_engine_snapshot();
    magician_v2::execution::plane::install_harness_engine_snapshot(HarnessEngineSnapshot {
        plane_endpoint: endpoint,
        turn_max_seconds: 120,
        ..Default::default()
    });
    let engines = std::env::var("DECISION_PLANNER_LIVE_ENGINES").unwrap_or_else(|_| "pi".into());
    let mut failures = Vec::new();
    for engine in engines
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        let dir = tempfile::tempdir().unwrap();
        let grant = run_grant(dir.path(), false);
        let mut ctx = grant.ctx.clone();
        ctx.harness_engine = Some(engine.into());
        ctx.execution_id = Some(format!("planner-fixture-{}", uuid::Uuid::new_v4()));
        let marker = uuid::Uuid::new_v4().simple().to_string();
        let prompt = format!("Propose exactly one call to fixture_record with arguments {{\"marker\":\"{marker}\"}}. Its schema is {{\"type\":\"object\",\"properties\":{{\"marker\":{{\"type\":\"string\"}}}},\"required\":[\"marker\"],\"additionalProperties\":false}}. This is the complete task and all required evidence. Do not execute the proposed call. Submit {{\"steps\":[{{\"id\":\"record\",\"call\":{{\"tool\":\"fixture_record\",\"arguments\":{{\"marker\":\"{marker}\"}}}}}}]}} through decision_submit_plan, or return only that JSON.");
        let result = decision_planner::propose(
            &ctx, grant.executors.as_ref().unwrap(),
            "You are an action-plan proposer. Use only the proposal collector. Do not read files, use work tools, run commands or contact services. Return only the requested plan.",
            &prompt, Vec::new(), None, 1,
        ).await;
        match result {
            Ok(output) => {
                let valid = output.plan.steps.len() == 1
                    && output.plan.steps[0].call.tool == "fixture_record"
                    && output.plan.steps[0].call.arguments == serde_json::json!({"marker":marker});
                println!(
                    "planner fixture {engine}: valid={valid}, steps={}",
                    output.plan.steps.len()
                );
                if !valid {
                    failures.push(format!("{engine}: invalid proposal"));
                }
            },
            Err(error) => failures.push(format!("{engine}: {error:#}")),
        }
    }
    magician_v2::execution::plane::install_harness_engine_snapshot(previous);
    handle.stop(true).await;
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
