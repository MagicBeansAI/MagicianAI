//! Qualification through the ordinary owner/task factory, native HTTP model
//! dispatch, scoped file provider, controller, checks and output projection.
use super::*;
use crate::magician_v2::{
    agents::{
        definition_store::AgentDefinitionStore, AgentDefinition, AgentStorage, TrustPolicyFile,
    },
    artifact_v2::{capabilities::CapabilityWorkspaceManager, service::TaskProgressProjector},
    execution::pack_defs_to_tool_infos,
    test_support::build_test_artifact_v2_harness_with_tool_catalog,
};
use async_trait::async_trait;
use runtime_core::{ExecutionContext, ParameterDefinition, ToolCatalog, ToolInfo};
use serde_json::{json, Value};
use std::{
    collections::{BTreeSet, HashMap},
    time::Duration,
};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, Request, Respond, ResponseTemplate,
};

const FIRST: &str = "candidate awaiting declared JSON verification";
const REVISED: &str = "{\"result\":\"verified candidate\"}";

/// The native owner needs the same pack-derived metadata that the production
/// catalog exposes. Executable providers still come from the ordinary factory;
/// this fixture does not manufacture capability grants or replace file policy.
struct FixtureCatalog(CapabilityWorkspaceManager);

#[async_trait]
impl ToolCatalog for FixtureCatalog {
    async fn all_tools(&self, context: &ExecutionContext) -> Result<Vec<ToolInfo>> {
        let packs = self
            .0
            .load_pack_defs_for_scope(&context.principal, &context.workspace);
        Ok(pack_defs_to_tool_infos(&packs)
            .into_iter()
            .map(|(_, tool, guide)| {
                let required = tool.input_schema["required"].as_array();
                let parameters = tool.input_schema["properties"]
                    .as_object()
                    .into_iter()
                    .flat_map(|properties| properties.iter())
                    .map(|(name, schema)| ParameterDefinition {
                        name: name.clone(),
                        param_type: schema["type"].as_str().unwrap_or("string").into(),
                        required: required.is_some_and(|fields| {
                            fields
                                .iter()
                                .any(|field| field.as_str() == Some(name.as_str()))
                        }),
                        description: schema["description"].as_str().unwrap_or_default().into(),
                        validation_rules: vec![],
                        default_value: schema.get("default").cloned(),
                        enum_values: schema["enum"].as_array().map(|values| {
                            values
                                .iter()
                                .filter_map(|value| value.as_str().map(str::to_owned))
                                .collect()
                        }),
                        schema: schema.clone(),
                    })
                    .collect();
                ToolInfo {
                    name: tool.name,
                    description: tool.description,
                    category: tool
                        .categories
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "uncategorized".into()),
                    categories: tool.categories,
                    parameters,
                    enhanced_description: guide,
                    keywords: vec![],
                    use_cases: vec![],
                    composition_category: tool
                        .metadata
                        .get("composition_category")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    providing_agent_id: tool
                        .metadata
                        .get("providing_agent_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                }
            })
            .collect())
    }

    async fn list_tool_names(&self, context: &ExecutionContext) -> Vec<String> {
        self.all_tools(context)
            .await
            .unwrap()
            .into_iter()
            .map(|tool| tool.name)
            .collect()
    }

    async fn available_categories(&self, context: &ExecutionContext) -> Vec<String> {
        self.all_tools(context)
            .await
            .unwrap()
            .into_iter()
            .flat_map(|tool| tool.categories)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    async fn get_tool_metadata(
        &self,
        name: &str,
        context: &ExecutionContext,
    ) -> Option<HashMap<String, Value>> {
        self.all_tools(context)
            .await
            .unwrap()
            .into_iter()
            .find(|tool| tool.name == name)
            .map(|tool| serde_json::from_value(serde_json::to_value(tool).unwrap()).unwrap())
    }

    async fn filtered_tools_by_categories(
        &self,
        categories: &[String],
        context: &ExecutionContext,
    ) -> Result<Vec<ToolInfo>> {
        Ok(self
            .all_tools(context)
            .await?
            .into_iter()
            .filter(|tool| {
                tool.categories
                    .iter()
                    .any(|category| categories.contains(category))
            })
            .collect())
    }

    async fn category_tool_counts(
        &self,
        context: &ExecutionContext,
    ) -> Result<HashMap<String, usize>> {
        let mut counts = HashMap::new();
        for tool in self.all_tools(context).await? {
            for category in tool.categories {
                *counts.entry(category).or_default() += 1;
            }
        }
        Ok(counts)
    }
}

// Read the explicit actor envelope from the real provider prompt. This is a
// fixture-only transport parser; the production actor never discovers outputs.
fn actor_envelope(value: &Value) -> Option<Value> {
    match value {
        Value::String(text) => text.match_indices('{').find_map(|(offset, _)| {
            serde_json::Deserializer::from_str(&text[offset..])
                .into_iter::<Value>()
                .next()
                .and_then(Result::ok)
                .filter(|v| v.get("candidate_path").is_some() && v.get("attempt_number").is_some())
        }),
        Value::Array(items) => items.iter().find_map(actor_envelope),
        Value::Object(fields) => fields.values().find_map(actor_envelope),
        _ => None,
    }
}

#[derive(Clone, Default)]
struct NativeFixture {
    calls: Arc<Mutex<BTreeMap<u32, usize>>>,
    observed: Arc<Mutex<Vec<(u32, usize, String, Vec<String>)>>>,
    ask_after_write: bool,
}

impl Respond for NativeFixture {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = request.body_json().expect("native request JSON");
        let envelope = actor_envelope(&body).expect("frozen actor envelope in the real prompt");
        let attempt = envelope["attempt_number"].as_u64().unwrap() as u32;
        let candidate = envelope["candidate_path"].as_str().unwrap();
        let tools: Vec<_> = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect();
        assert!(
            tools.iter().any(|name| name == "files"),
            "the admitted provider must be callable: {tools:?}"
        );
        assert!(
            tools.iter().all(|name| matches!(
                name.as_str(),
                // `submit_plan` is a control tool offered to every non-chat
                // agentic run (native_catalog.rs); it adds no capability —
                // each planned step must still name a catalog tool.
                "files" | "yield" | "need_user_input" | "submit_plan" | "tool_search"
            )),
            "the actual native catalog must retain the admitted scope: {tools:?}"
        );
        let turn = {
            let mut calls = self.calls.lock().unwrap();
            let turn = calls.entry(attempt).or_default();
            *turn += 1;
            *turn
        };
        self.observed
            .lock()
            .unwrap()
            .push((attempt, turn, candidate.to_owned(), tools));
        let content = if attempt == 1 && !self.ask_after_write {
            FIRST
        } else {
            REVISED
        };
        let (name, arguments) = match turn {
            1 => {
                if attempt == 2 {
                    assert!(
                        envelope["feedback"] != Value::Null,
                        "revision must receive the recorded failed review"
                    );
                    assert!(
                        envelope["retained_files"]
                            .as_object()
                            .is_some_and(|files| !files.is_empty()),
                        "revision must retain the first candidate"
                    );
                }
                (
                    "files",
                    json!({"action":"write", "path":candidate, "content":content}),
                )
            },
            2 if self.ask_after_write => (
                "need_user_input",
                json!({"question":"Which audience should this result address?", "input_type":"text"}),
            ),
            2 => ("files", json!({"action":"read", "path":candidate})),
            3 if self.ask_after_write => {
                assert!(
                    serde_json::to_string(&body)
                        .unwrap()
                        .contains("independent founders"),
                    "continuation must receive the recorded answer in its actual prompt"
                );
                ("files", json!({"action":"read", "path":candidate}))
            },
            3 | 4 => {
                assert!(
                    turn == 3 || self.ask_after_write,
                    "unexpected extra actor call; last provider input: {:?}",
                    body.get("input")
                        .and_then(Value::as_array)
                        .and_then(|input| input.last())
                );
                let text = serde_json::to_string(&body).unwrap();
                assert!(
                    text.contains(if attempt == 1 && !self.ask_after_write {
                        FIRST
                    } else {
                        "verified candidate"
                    }),
                    "the next native prompt must carry the actual file-tool result"
                );
                (
                    "yield",
                    json!({
                        "summary": "Wrote and read back the declared candidate. Independent checks decide acceptance.",
                        "completed": ["Wrote and verified the declared candidate file"],
                        "open": [], "blockers": [],
                        "task_state_action": {"action":"none", "reason":"candidate attempt finished; controller owns review"}
                    }),
                )
            },
            other => panic!("unexpected native call {other} in attempt {attempt}"),
        };
        ResponseTemplate::new(200).set_body_json(json!({
            "id":format!("resp_{attempt}_{turn}"), "model":"gpt-6.1-sol", "status":"completed",
            "output":[{"type":"function_call", "id":format!("fc_{attempt}_{turn}"),
                "call_id":format!("call_{attempt}_{turn}"), "name":name,
                "arguments":serde_json::to_string(&arguments).unwrap(), "status":"completed"}],
            "usage":{"input_tokens":400,"output_tokens":50,"total_tokens":450}
        }))
    }
}

async fn seed_owner(root: &std::path::Path, orchestrator: &MagicianV2Orchestrator) {
    let store = AgentDefinitionStore::with_workspace_root(root);
    let definition_yaml = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/relentless_native_owner.agent.yaml"
    ))
    .unwrap();
    let mut definition: AgentDefinition = serde_yaml::from_str(&definition_yaml).unwrap();
    store.create_definition(definition.clone()).await.unwrap();
    definition.principal = Some("owner".into());
    definition.workspace = Some("workspace".into());
    let scoped = store.for_scope("owner", "workspace");
    let file = scoped
        .storage()
        .agent_definition_path(&definition.agent_id)
        .unwrap();
    tokio::fs::create_dir_all(file.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(file, serde_yaml::to_string(&definition).unwrap())
        .await
        .unwrap();
    let layout = store.workspace_layout().unwrap();
    let policy = AgentStorage::new(layout.scoped_agent_runtime_root("owner", "workspace"))
        .trust_policies_path();
    tokio::fs::create_dir_all(policy.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(policy, TrustPolicyFile::recommended_template_yaml())
        .await
        .unwrap();
    orchestrator.set_definition_store(Arc::new(store));
}

async fn wait_for(
    service: &RelentlessService,
    scope: &RunScope,
    id: Uuid,
    done: impl Fn(&RunState) -> bool,
) -> RunState {
    wait_for_with_intermediate(service, scope, id, done, |_| false).await
}

async fn wait_for_with_intermediate(
    service: &RelentlessService,
    scope: &RunScope,
    id: Uuid,
    done: impl Fn(&RunState) -> bool,
    intermediate: impl Fn(&RunState) -> bool,
) -> RunState {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let state = service.store(scope).load(id).unwrap();
            if done(&state) {
                return state;
            }
            assert!(
                intermediate(&state)
                    || state.lease.is_some()
                    || !matches!(
                        state.status,
                        RunStatus::WaitingOwner
                            | RunStatus::Failed
                            | RunStatus::BudgetExhausted
                            | RunStatus::Plateau
                    ),
                "native run stopped: {:?}: {:?}",
                state.status,
                state.reason
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        let state = service.store(scope).load(id).unwrap();
        panic!(
            "native run wait expired: {:?}, attempts={}, reason={:?}",
            state.status,
            state.attempts.len(),
            state.reason
        )
    })
}

#[tokio::test]
async fn native_task_survives_disconnected_admission_and_recovers_beside_damaged_journals() {
    Box::pin(native_task_fixture(
        false, false, false, false, false, false, false, None,
    ))
    .await;
}

#[tokio::test]
async fn native_task_time_budget_reset_keeps_checkpoint_and_continues_after_restart() {
    Box::pin(native_task_fixture(
        true, false, false, false, false, false, true, None,
    ))
    .await;
}

#[tokio::test]
async fn native_task_answers_material_question_and_recovers_original_paid_attempt() {
    Box::pin(native_task_fixture(
        true, false, false, false, false, false, false, None,
    ))
    .await;
}

#[tokio::test]
async fn native_task_validation_retry_republishes_exact_question_after_cold_restart() {
    Box::pin(native_task_fixture(
        true, true, false, false, false, false, false, None,
    ))
    .await;
}

#[tokio::test]
async fn native_task_campaign_capture_verifies_manifest_files_and_is_idempotent() {
    Box::pin(native_task_fixture(
        false, false, true, false, false, false, false, None,
    ))
    .await;
}

#[tokio::test]
async fn native_task_declined_handoff_publishes_report_and_preserves_exact_artifact() {
    Box::pin(native_task_fixture(
        false, false, false, true, false, false, false, None,
    ))
    .await;
}

#[tokio::test]
async fn native_task_call_exhaustion_reviews_saved_candidate_and_revises_on_same_root() {
    Box::pin(native_task_fixture(
        false,
        false,
        false,
        false,
        false,
        false,
        false,
        Some(1),
    ))
    .await;
}

#[tokio::test]
async fn native_task_iteration_limit_reviews_declared_file_without_growing_the_cap() {
    Box::pin(native_task_fixture(
        false, false, false, false, true, false, false, None,
    ))
    .await;
}

#[tokio::test]
async fn native_task_answer_at_iteration_limit_reviews_exact_candidate_without_reasking() {
    Box::pin(native_task_fixture(
        true, false, false, false, true, false, false, None,
    ))
    .await;
}

#[tokio::test]
async fn native_task_cancellation_retires_the_exact_unanswered_question_without_more_calls() {
    Box::pin(native_task_fixture(
        true, false, false, false, false, true, false, None,
    ))
    .await;
}

// Keep the live FullPauseData temporary out of the large async fixture's state.
fn assert_native_scoped_audit_owner(
    service: &RelentlessService,
    scope: &RunScope,
    pause_key: &str,
) {
    let live_pause = service
        .orchestrator
        .full_pause_store_handle()
        .unwrap()
        .load_durable_internal(pause_key)
        .unwrap()
        .expect("native question retains its live executors");
    let audit_store = service
        .orchestrator
        .secret_store_resolver()
        .resolve_for_scope(&scope.principal, &scope.workspace)
        .unwrap();
    assert!(Arc::ptr_eq(
        live_pause
            .executors
            .secret_store
            .as_ref()
            .expect("native launch must carry the scoped audit owner into governed skill dispatch"),
        &audit_store,
    ));
}

async fn native_task_fixture(
    ask_after_write: bool,
    validation_retry: bool,
    capture: bool,
    decline: bool,
    iteration_limit: bool,
    cancel_question: bool,
    budget_extension: bool,
    physical_call_limit: Option<u32>,
) {
    // These fixtures deliberately use the same native owner. The production
    // governor counts that owner's live loops process-wide, including loops
    // paused in other isolated stores. Avoid making parallel fixtures compete
    // for its real eight-loop allowance or consume the time-budget probe's
    // deadline while waiting for another fixture's local work.
    static NATIVE_OWNER: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _owner = NATIVE_OWNER.lock().await;
    if std::env::var_os("RELENTLESS_NATIVE_TRACE").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(std::env::var("RELENTLESS_NATIVE_TRACE_FILTER").unwrap_or_else(|_| "magician::magician_v2::execution::agentic::native_integration=debug,magician::magician_v2::execution::agentic::native_catalog=debug".into()))
            .with_test_writer()
            .with_ansi(false)
            .try_init();
    }
    static KEY: std::sync::Once = std::sync::Once::new();
    KEY.call_once(|| std::env::set_var("RELENTLESS_NATIVE_FIXTURE_KEY", "local-fixture-only"));
    let server = MockServer::start().await;
    let fixture = NativeFixture::default();
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(fixture.clone())
        .mount(&server)
        .await;
    let profile: magicllm::LLMProfile = serde_json::from_value(json!({
        "provider":"openai", "model":"gpt-6.1-sol", "api_key_env":"RELENTLESS_NATIVE_FIXTURE_KEY",
        "api_base_url":format!("{}/v1/responses", server.uri()), "max_output_tokens":4096,
        "context_window_tokens":128000, "timeout_secs":5, "supports_tool_calling":true,
        "metadata":{"model_family":"openai"}
    }))
    .unwrap();
    let mut config = magicllm::LLMRouterConfig::default();
    config.default_profile = "native-fixture".into();
    config.profiles.insert("native-fixture".into(), profile);
    let router = Arc::new(OperationLlmRouter::new(Some(config)));
    let queue = magicllm::LlmDispatchQueue::start(
        router.live_dispatch_router(),
        Arc::new(magicllm::NoopTaskStateView),
        Arc::new(magicllm::NoopTaskLedgerSink),
        magicllm::DispatchConfig {
            workers: 1,
            local_prep: magicllm::LocalPrepConfig {
                enabled: false,
                ..Default::default()
            },
            ..Default::default()
        },
    );
    router.set_dispatch_queue(queue.clone());
    let maximum = router
        .bounded_route("native-fixture")
        .unwrap()
        .maximum_microusd()
        * 8;
    let proposal: TextDraftProposal = serde_json::from_value(json!({
        "goal":"Produce a local JSON result and verify its declared syntax.",
        "artifact_file":"result.json", "maximum_artifact_bytes":1024,
        "actor_profile":"native-fixture", "critic_profiles":[],
        "handoff_kind":if capture { "capture" } else { "internal_draft" },
        "criteria":[{"id":"syntax", "specification":{"kind":"check", "assertions":[{"kind":"json", "file":"result.json"}]}, "must_pass":true, "weight":null, "pass_score":null}],
        "sources":{}, "score_at_least":null,
        "review":{"panel_size":1, "score_quorum":1, "min_confidence":0.8, "max_score_spread":0.2, "noise_margin":0.02, "max_retries_per_critic":0, "unknown_review":"pause"},
        "budget":{"max_attempts":2, "max_wall_clock_secs":if budget_extension { 10 } else { 600 }, "max_cost_microusd":maximum, "plateau_attempts":2},
        "native_harness":{"agent_id":"native-fixture", "maximum_iterations":if physical_call_limit.is_some() { 8 } else if iteration_limit { if ask_after_write { 3 } else { 2 } } else { 6 }, "maximum_physical_calls":physical_call_limit.unwrap_or(if ask_after_write { 500 } else { 4 }), "capabilities":["files"]}
    })).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let catalog = Arc::new(FixtureCatalog(CapabilityWorkspaceManager::new(
        ArtifactV2Workspace::new(temp.path().join("magician_data_v3")),
        temp.path(),
    )));
    let (artifacts, orchestrator) =
        build_test_artifact_v2_harness_with_tool_catalog(temp.path(), catalog);
    artifacts.set_event_broadcaster(orchestrator.event_broadcaster().unwrap());
    let attention_store =
        super::super::feed::FeedStore::open_workspace(artifacts.workspace().clone()).unwrap();
    artifacts.set_progress_projection(
        orchestrator.event_broadcaster().unwrap(),
        attention_store.clone(),
    );
    let executors = orchestrator.build_action_executors_with_router_override(Some(router.clone()));
    assert_eq!(
        executors.artifact_v2_workspace.base_root(),
        artifacts.workspace().base_root(),
        "native execution and the control plane must use the resolved runtime root"
    );
    assert_eq!(executors.taskplan_base_path, temp.path());
    seed_owner(artifacts.workspace().base_root(), &orchestrator).await;
    let requests = Arc::new(
        super::super::user_requests::UserRequestService::new(
            orchestrator.event_broadcaster().unwrap(),
        )
        .with_workspace_layout(artifacts.workspace().clone())
        .with_history_persist_path(
            artifacts
                .workspace()
                .base_root()
                .join("user_request_history.json"),
        )
        .with_pending_persist_path(
            artifacts
                .workspace()
                .base_root()
                .join("user_request_pending.json"),
        )
        .await,
    );
    orchestrator.set_user_request_service(requests.clone());
    let service = RelentlessService::new(
        artifacts.workspace().clone(),
        router.clone(),
        artifacts.clone(),
        orchestrator.clone(),
        None,
    )
    .with_user_requests(requests.clone());
    let scope = RunScope {
        principal: "owner".into(),
        workspace: "workspace".into(),
    };
    let id = Uuid::new_v4();
    let contract_hash = PreparedTextDraft::resolve(&router, &proposal)
        .unwrap()
        .contract
        .hash()
        .unwrap();
    if !ask_after_write {
        // Hold admission until the calling request is gone. The owned worker
        // must still admit exactly this run and dispatch its normal native work.
        let admission = service.admission_lock.lock().await;
        let owner = service.clone();
        let s = scope.clone();
        let p = proposal.clone();
        let hash = contract_hash.clone();
        let (ready, entered) = tokio::sync::oneshot::channel();
        let caller = tokio::spawn(async move {
            ready.send(()).unwrap();
            owner
                .start_owner_text_draft(
                    s,
                    id,
                    &p,
                    "native-fixture-thread",
                    "native-fixture",
                    &hash,
                    None,
                )
                .await
        });
        entered.await.unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        drop(admission);
        let initial = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(state) = service.store(&scope).load(id) {
                    break state;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("committed owner admission must outlive a disconnected request");
        let reviewed = wait_for(&service, &scope, id, |s| {
            s.status == RunStatus::Accepted && s.routing.is_some() && s.lease.is_none()
        })
        .await;
        assert_eq!(
            reviewed.attempts.len(),
            2,
            "the first failed check must drive a real native revision"
        );
        assert!(reviewed.handoff.is_none());
        assert_eq!(reviewed.owner_approved, None);
        let setup: TextDraftSetup = serde_json::from_slice(
            &service
                .store(&scope)
                .read_blob(id, &initial.admission.source_hash)
                .unwrap(),
        )
        .unwrap();
        let runtime = setup.runtime_execution_id.clone().unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while orchestrator.get_execution_status(&runtime).await.unwrap()
                != WaitingState::WaitingUser
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("attempt terminal must leave the task awaiting reviewed handoff");
        let accepted = reviewed.retained.artifact().unwrap();
        // Runtime WaitingUser can precede the notice's durable append. Join
        // the same projection boundary instead of racing its async publisher.
        service
            .project(service.store(&scope), &reviewed)
            .await
            .unwrap();
        let notice_id = format!(
            "{runtime}:relentless-review:{id}:{}",
            accepted.artifact_hash
        );
        let notice = requests
            .pending_request_snapshot(&notice_id, Some(&scope.principal), Some(&scope.workspace))
            .await
            .expect("accepted draft must have a durable review request");
        assert_eq!(notice.context["artifact_hash"], accepted.artifact_hash);
        assert_eq!(
            notice.context["review_href"],
            format!(
                "/work?view=quality&run={id}&task={}",
                reviewed.admission.task_id
            )
        );
        assert_eq!(notice.context["review_label"], "Review draft");
        assert_eq!(
            notice.task_id.as_deref(),
            Some(reviewed.admission.task_id.as_str())
        );
        let task_scope =
            ScopeRef::system_internal_unauthenticated(&scope.principal, &scope.workspace);
        let progress = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let progress = artifacts
                    .get_task_progress_projection(&task_scope, &reviewed.admission.task_id)
                    .await
                    .unwrap();
                if progress
                    .attention
                    .iter()
                    .any(|a| a.pause_state_id.as_deref() == Some(notice_id.as_str()))
                {
                    break progress;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("exact review request must reach the durable Work projection");
        assert!(
            progress.attention.iter().all(|a| a.attention_kind
                != super::super::artifact_v2::service::WAITING_WITHOUT_QUESTION_KIND),
            "an accepted artifact with an exact review request is not a stalled wait"
        );
        artifacts
            .project_task_progress(&task_scope, &reviewed.admission.task_id)
            .await
            .unwrap();
        assert!(
            attention_store
                .get_attention_item_by_alias(&scope.principal, &scope.workspace, &notice_id)
                .await
                .unwrap()
                .is_some(),
            "the exact review must first reach the indexed Attention lane"
        );
        let before_notice_replay = serde_json::to_value(&reviewed).unwrap();
        service
            .project(service.store(&scope), &reviewed)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(service.store(&scope).load(id).unwrap()).unwrap(),
            before_notice_replay,
            "review notice replay grants no approval, budget or provider work"
        );
        assert_eq!(
            requests
                .list_pending_for_scope(&scope.principal, &scope.workspace)
                .await
                .len(),
            1
        );
        let manifest = reviewed.attempts.last().unwrap().artifact.as_ref().unwrap();
        assert_eq!(manifest.hash, accepted.artifact_hash);
        assert_eq!(
            service
                .store(&scope)
                .read_blob(id, &manifest.files["result.json"])
                .unwrap(),
            REVISED.as_bytes()
        );
        let spend = reviewed.budget.spend().unwrap();
        assert!(spend.settled_microusd > 0);
        assert_eq!(spend.reserved_microusd, 0);
        let turns_per_attempt = if let Some(limit) = physical_call_limit {
            limit as usize
        } else if iteration_limit {
            2
        } else {
            3
        };
        assert_eq!(
            *fixture.calls.lock().unwrap(),
            BTreeMap::from([(1, turns_per_attempt), (2, turns_per_attempt)])
        );
        let observed = fixture.observed.lock().unwrap().clone();
        let work_areas: BTreeSet<_> = observed
            .iter()
            .map(|(_, _, path, _)| path.clone())
            .collect();
        assert_eq!(
            work_areas.len(),
            2,
            "each revision must use its admitted attempt workspace"
        );
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            turns_per_attempt * 2
        );
        if iteration_limit {
            assert!(
                orchestrator
                    .full_pause_store_handle()
                    .unwrap()
                    .get_pending_for_execution(&runtime)
                    .is_empty(),
                "the controller-owned limit must not leave an ordinary continuation prompt"
            );
        }

        // The owner answers the exact-review ask itself — the path the composer
        // dock, task panel, Attention and push share. Approval grants only the
        // local internal draft handoff, never external delivery.
        assert_eq!(
            notice
                .options
                .iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            ["approve", "reject"]
        );
        if !decline {
            // Live: the run follows the ask's resolution on the runtime bus.
            Arc::new(service.clone()).start_review_answers();
        }
        let answered = requests
            .respond_scoped(
                super::super::user_requests::UserResponse {
                    request_id: notice_id.clone(),
                    decision: if decline { "reject" } else { "approve" }.into(),
                    input: None,
                    channel: "task_panel".into(),
                    sensitive: vec![],
                },
                Some(&scope.principal),
                Some(&scope.workspace),
            )
            .await;
        assert!(matches!(
            answered,
            super::super::user_requests::ScopedResponseResult::Accepted
        ));
        if decline {
            // Missed resolution (no subscriber, as after a restart): the next
            // projection applies the answer from request history.
            service
                .project(service.store(&scope), &reviewed)
                .await
                .unwrap();
        }
        let finished = wait_for(&service, &scope, id, |s| {
            s.owner_approved == Some(!decline)
                && (decline || s.handoff.is_some())
                && s.lease.is_none()
        })
        .await;
        tokio::time::timeout(Duration::from_secs(10), async {
            while orchestrator.get_execution_status(&runtime).await.unwrap()
                != if decline {
                    WaitingState::Failed
                } else {
                    WaitingState::Completed
                }
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("local handoff decision must project its ordinary task status");
        service
            .project(service.store(&scope), &finished)
            .await
            .unwrap();
        assert!(
            requests
                .pending_request_snapshot(
                    &notice_id,
                    Some(&scope.principal),
                    Some(&scope.workspace)
                )
                .await
                .is_none(),
            "handoff approval or refusal must close its exact review notice"
        );
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let progress = artifacts
                    .get_task_progress_projection(&task_scope, &finished.admission.task_id)
                    .await
                    .unwrap();
                if progress
                    .attention
                    .iter()
                    .all(|a| a.pause_state_id.as_deref() != Some(notice_id.as_str()))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("exact review resolution must close the durable Work request");
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                artifacts.project_task_progress(&task_scope, &finished.admission.task_id).await.unwrap();
                if attention_store.get_attention_item_by_alias(&scope.principal, &scope.workspace, &notice_id).await.unwrap().is_none() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }).await.expect("closing an exact review must clear the indexed Attention row as well as the durable task projection");
        assert_eq!(
            artifacts
                .get_task(&task_scope, &finished.admission.task_id)
                .await
                .unwrap()
                .state
                .status,
            if decline { "failed" } else { "completed" },
            "closing a notice cannot reopen a terminal task"
        );
        if decline {
            assert_eq!(finished.status, RunStatus::Accepted);
            assert!(finished.handoff.is_none());
            let task_scope =
                ScopeRef::system_internal_unauthenticated(&scope.principal, &scope.workspace);
            let task = artifacts
                .get_task(&task_scope, &finished.admission.task_id)
                .await
                .unwrap();
            assert_eq!(task.state.status, "failed");
            let output = task
                .refs
                .outputs
                .iter()
                .find(|output| {
                    task.state.primary_user_output_id.as_ref() == Some(&output.output_id)
                })
                .unwrap();
            let text = std::fs::read_to_string(
                artifacts
                    .workspace()
                    .task_dir(
                        &scope.principal,
                        &scope.workspace,
                        &finished.admission.task_id,
                    )
                    .join(&output.relative_path),
            )
            .unwrap();
            assert!(
                text.contains("The owner declined handoff of the exact accepted artifact"),
                "declined handoff must be visible in the final answer: {text}"
            );
            assert!(text.contains("Handoff: none"));
            assert!(text.contains("## Available artifact"));
            assert!(text.contains(REVISED));
            assert_eq!(
                service
                    .store(&scope)
                    .read_blob(id, &manifest.files["result.json"])
                    .unwrap(),
                REVISED.as_bytes()
            );
        }
        if capture {
            use crate::magician_v2::{approval_envelopes::campaign::*, counterparties::*};
            let identities = CounterpartyStore::new(artifacts.workspace().clone());
            let identity_scope = CounterpartyScope::new(&scope.principal, &scope.workspace);
            let party = identities
                .record_counterparty(
                    &identity_scope,
                    &CreateCounterparty {
                        display_name: "Synthetic capture recipient".into(),
                        domain: None,
                        stage: None,
                        created_by: scope.principal.clone(),
                    },
                    Utc::now(),
                )
                .unwrap();
            let identity = identities
                .add_identity(
                    &identity_scope,
                    &AddIdentity {
                        counterparty_id: party.counterparty_id,
                        kind: IdentityKind::Email,
                        value: "recipient@example.invalid".into(),
                        source: MintSource::OwnerStated,
                        evidence_ref: "local fixture".into(),
                        recorded_by: scope.principal.clone(),
                        introduced_by: None,
                    },
                    Utc::now(),
                )
                .unwrap();
            let task_scope =
                ScopeRef::system_internal_unauthenticated(&scope.principal, &scope.workspace);
            let task = artifacts
                .get_task(&task_scope, &finished.admission.task_id)
                .await
                .unwrap();
            let campaign_id = Uuid::new_v4();
            let campaign = service
                .configure_owner_campaign(
                    scope.clone(),
                    campaign_id,
                    None,
                    Some(CampaignSpec {
                        name: "Synthetic exact capture".into(),
                        goal_id: None,
                        sender_identity: "owner".into(),
                        channel: "email".into(),
                        message_kind: "plain".into(),
                        targets: vec![CampaignTarget {
                            task_id: finished.admission.task_id.clone(),
                            task_context_hash: task_policy_context_hash(&task.manifest).unwrap(),
                            identity_id: identity.identity_id.clone(),
                            recipient: Some(identity.normalised),
                        }],
                        allowed_contract_hashes: BTreeSet::from([contract_hash.clone()]),
                        content_contract_hash: accepted.artifact_hash.clone(),
                        content_template: None,
                        maximum_acts: 1,
                        maximum_acts_per_day: 1,
                        maximum_acts_per_recipient: 1,
                        expires_at: Utc::now() + chrono::Duration::hours(1),
                        approval: CaptureApprovalPolicy {
                            source_document: "local fixture".into(),
                            always_review: true,
                            important_identity_ids: BTreeSet::new(),
                            allow_automatic_capture: false,
                            clean_approvals_before_automatic_capture: None,
                            audit_every: 1,
                        },
                    }),
                )
                .await
                .unwrap();
            // Artifact identity is the manifest digest, not a stored content blob.
            assert!(service
                .store(&scope)
                .read_blob(id, &accepted.artifact_hash)
                .is_err());
            let act = service
                .capture_owner_campaign_run(
                    scope.clone(),
                    campaign_id,
                    campaign.campaign.version_hash.clone(),
                    id,
                    accepted.artifact_hash.clone(),
                    identity.identity_id.clone(),
                )
                .await
                .unwrap();
            assert_eq!(act.status, CaptureStatus::Captured);
            assert_eq!(act.binding.artifact_files, manifest.files);
            assert_eq!(
                service
                    .capture_owner_campaign_run(
                        scope.clone(),
                        campaign_id,
                        campaign.campaign.version_hash.clone(),
                        id,
                        accepted.artifact_hash.clone(),
                        identity.identity_id.clone()
                    )
                    .await
                    .unwrap(),
                act
            );
            assert!(service
                .capture_owner_campaign_run(
                    scope.clone(),
                    campaign_id,
                    campaign.campaign.version_hash.clone(),
                    id,
                    "f".repeat(64),
                    identity.identity_id.clone()
                )
                .await
                .is_err());
            assert!(service
                .review_owner_campaign_capture(
                    scope.clone(),
                    campaign_id,
                    act.id.clone(),
                    "f".repeat(64),
                    true
                )
                .await
                .is_err());
            let stored = CampaignStore::new(artifacts.workspace().clone())
                .load(
                    &crate::magician_v2::approval_envelopes::EnvelopeStoreScope::new(
                        &scope.principal,
                        &scope.workspace,
                    ),
                    campaign_id,
                )
                .unwrap()
                .unwrap();
            assert_eq!(stored.captures.len(), 1);
            assert!(!stored.live_dispatch_enabled);
        }
        let archived_evidence = if decline {
            artifacts
                .archive_task(&task_scope, &finished.admission.task_id)
                .await
                .unwrap();
            let task = artifacts
                .get_task(&task_scope, &finished.admission.task_id)
                .await
                .unwrap();
            let execution = artifacts
                .get_execution(&task_scope, &finished.admission.task_id, &runtime)
                .await
                .unwrap();
            let task_dir = artifacts.workspace().task_dir(
                &scope.principal,
                &scope.workspace,
                &finished.admission.task_id,
            );
            let retained: Vec<_> = task
                .refs
                .outputs
                .iter()
                .chain(execution.refs.output_refs.iter())
                .map(|output| {
                    let path = task_dir.join(&output.relative_path);
                    let bytes = std::fs::read(&path).unwrap();
                    (path, bytes)
                })
                .collect();
            // Exercise the shared output sink as well as the controller's early
            // archive check. A terminal replay with different bytes is a no-op.
            artifacts
                .persist_precomputed_execution_outcome(
                    &task_scope,
                    &finished.admission.task_id,
                    &runtime,
                    &format!("relentless:{}:{}", finished.id, setup.admission_key()),
                    ExecutionOutcomeSnapshot {
                        execution_status: "failed".into(),
                        task_status: "failed".into(),
                        outcome_type: "relentless_failed".into(),
                        outcome_summary: "replayed".into(),
                        iterations_used: Some(2),
                        is_terminal: true,
                        completion_kind: None,
                        open_items: vec![],
                    },
                    "A later elapsed-time report must not replace the archive".into(),
                )
                .await
                .unwrap();
            service
                .project(service.store(&scope), &finished)
                .await
                .unwrap();
            Some((
                serde_json::to_value(task).unwrap(),
                serde_json::to_value(execution).unwrap(),
                retained,
            ))
        } else {
            None
        };
        let before = serde_json::to_value(&finished).unwrap();
        // One damaged journal in each optional subsystem must not prevent an
        // independently verified ordinary task from recovering its projection.
        let root = artifacts
            .workspace()
            .scope_root(&scope.principal, &scope.workspace);
        for directory in [
            "approval_envelopes/campaigns",
            "learning/relentless_policies",
        ] {
            let path = root
                .join(directory)
                .join(format!("{}.jsonl", Uuid::new_v4()));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"{broken\n").unwrap();
        }
        let broken = Uuid::new_v4();
        let journal = root
            .join("relentless/runs")
            .join(broken.to_string())
            .join("journal");
        std::fs::create_dir_all(&journal).unwrap();
        std::fs::write(journal.join("00000000000000000000.json"), b"{broken").unwrap();
        let recovered = RelentlessService::new(
            artifacts.workspace().clone(),
            router,
            artifacts,
            orchestrator,
            None,
        );
        assert!(recovered
            .recover()
            .await
            .unwrap_err()
            .to_string()
            .contains("blocked"));
        assert!(recovered.store(&scope).load(broken).is_err());
        tokio::time::timeout(Duration::from_secs(10), async {
            while !recovered.controls.lock().unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("recovered terminal projection should finish");
        assert_eq!(
            serde_json::to_value(recovered.store(&scope).load(id).unwrap()).unwrap(),
            before,
            "reconstruction must preserve the exact reviewed journal and spend"
        );
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            turns_per_attempt * 2,
            "recovery must not resend actor work"
        );
        if let Some((task, execution, retained)) = archived_evidence {
            assert_eq!(
                serde_json::to_value(
                    recovered
                        .artifacts
                        .get_task(&task_scope, &finished.admission.task_id)
                        .await
                        .unwrap()
                )
                .unwrap(),
                task
            );
            assert_eq!(
                serde_json::to_value(
                    recovered
                        .artifacts
                        .get_execution(&task_scope, &finished.admission.task_id, &runtime)
                        .await
                        .unwrap()
                )
                .unwrap(),
                execution
            );
            for (path, bytes) in retained {
                assert_eq!(
                    std::fs::read(&path).unwrap(),
                    bytes,
                    "cold recovery rewrote {}",
                    path.display()
                );
            }
        }
        queue.shutdown(Duration::from_secs(5)).await;
        return;
    }
    let recovered = service;

    // An ordinary task stops for a material missing preference. Its
    // real need_user_input call must park the paid parent, retain the written
    // file and use the existing sealed pause store rather than finalize Blocked.
    server.reset().await;
    let questioning = NativeFixture {
        ask_after_write: true,
        ..Default::default()
    };
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(questioning.clone())
        .mount(&server)
        .await;
    let paused_id = Uuid::new_v4();
    recovered
        .start_owner_text_draft(
            scope.clone(),
            paused_id,
            &proposal,
            "native-question-thread",
            "native-fixture",
            &contract_hash,
            None,
        )
        .await
        .unwrap();
    let paused = wait_for(&recovered, &scope, paused_id, |state| {
        state.status == RunStatus::WaitingOwner && state.lease.is_none()
    })
    .await;
    assert_eq!(paused.attempts.len(), 1);
    let checkpoint = paused.attempts[0]
        .checkpoint
        .as_ref()
        .unwrap_or_else(|| panic!("expected a native question checkpoint: {:?}", paused.reason));
    assert_eq!(
        paused.attention_id.as_deref(),
        Some(checkpoint.attention_id.as_str())
    );
    assert!(paused.attempts[0].artifact.is_none());
    assert_eq!(
        paused.budget.spend().unwrap().reserved_microusd,
        maximum,
        "the 500-call question fixture must retain its full funded parent"
    );
    assert_eq!(
        paused.budget.operations()[&paused.attempts[0].actor_operation_id].settlement,
        magician_relentless::budget::Settlement::Reserved,
        "a paused actor must keep its original paid parent reservation"
    );
    let store = recovered.store(&scope);
    let native: Value = serde_json::from_slice(
        &store
            .read_blob(paused_id, &checkpoint.content_hash)
            .unwrap(),
    )
    .unwrap();
    let work_file = questioning.observed.lock().unwrap()[0].2.clone();
    assert_eq!(std::fs::read(&work_file).unwrap(), REVISED.as_bytes());
    assert!(std::path::Path::new(&work_file).starts_with(native["work_root"].as_str().unwrap()));
    assert_native_scoped_audit_owner(&recovered, &scope, native["pause_key"].as_str().unwrap());
    let native_pauses = recovered
        .orchestrator
        .full_pause_store_handle()
        .unwrap()
        .load_serializable_from_disk()
        .unwrap();
    let native_pause = native_pauses
        .iter()
        .find(|pause| pause.pause_state.hitl_correlation_id() == checkpoint.attention_id)
        .expect("the exact question must be durably resumable in the ordinary pause store");
    assert_eq!(
        native_pause.pause_state.storage_key(),
        native["pause_key"].as_str().unwrap()
    );
    assert!(native_pause.pause_state.plan_id.is_none());
    assert!(native_pause.pause_state.step_id.is_none());
    assert!(native_pause
        .pause_state
        .stateless_terminal_preparation
        .is_some());
    assert_eq!(
        native["pause_body_hash"].as_str().unwrap(),
        blake3::hash(
            &super::super::execution::agentic::app_pause_serializable_continuation_bytes(
                native_pause
            )
            .unwrap()
        )
        .to_hex()
        .as_str(),
        "the core checkpoint must bind the full stored body, including terminal preparation"
    );
    assert!(paused.budget.spend().unwrap().reserved_microusd > 0);
    assert_eq!(server.received_requests().await.unwrap().len(), 2);

    // Production owns a terminal-outbox projector alongside the controller.
    // Run its existing bounded pass here so the fixture has the same canonical
    // question lifecycle, rather than treating a private pause as a UI event.
    let projection = Box::pin(
        super::super::execution::agentic::run_loop::terminal_outbox::project_terminal_outbox_debt(
            &super::super::execution::agentic::run_loop::store::fs::FsLoopStateStore::new(
                recovered.workspace.base_root(),
            ),
            &recovered.orchestrator.event_broadcaster().unwrap(),
            Some(&recovered.artifacts),
            &super::super::execution::agentic::run_loop::state::WorkerId::new(
                "native-fixture-projector",
            ),
            None,
            std::num::NonZeroUsize::new(64).unwrap(),
            &CancellationToken::new(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(projection.store_failures, 0);
    assert_eq!(projection.canonical_persistence_failures, 0);
    let original_request = recovered
        .orchestrator
        .event_broadcaster()
        .unwrap()
        .pending_hitl_request(&scope.principal, &scope.workspace, &checkpoint.attention_id)
        .expect("ordinary outbox must publish the exact material question");
    let task_scope = ScopeRef::system_internal_unauthenticated(&scope.principal, &scope.workspace);
    let progress = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let progress = recovered
                .artifacts
                .get_task_progress_projection(&task_scope, &paused.admission.task_id)
                .await
                .unwrap();
            if progress
                .attention
                .iter()
                .any(|a| a.pause_state_id.as_deref() == Some(checkpoint.attention_id.as_str()))
            {
                break progress;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("canonical question must finish its durable event projection");
    let question = progress
        .attention
        .iter()
        .find(|a| a.pause_state_id.as_deref() == Some(checkpoint.attention_id.as_str()))
        .expect("ordinary Work must render the controller's exact canonical question");
    assert_eq!(question.source.as_deref(), Some("agentic"));
    assert!(
        progress.attention.iter().all(|a| a.attention_kind
            != super::super::artifact_v2::service::WAITING_WITHOUT_QUESTION_KIND),
        "an answerable controller pause is not a stalled task"
    );

    let paused = if budget_extension {
        use magician_relentless::budget_round::BudgetReset;
        wait_for_with_intermediate(
            &recovered,
            &scope,
            paused_id,
            |s| s.status == RunStatus::BudgetExhausted && s.lease.is_none(),
            |s| s.status == RunStatus::WaitingOwner,
        )
        .await;
        let notice_id = format!(
            "{}:quality-budget:{}:0",
            native_pause
                .pause_state
                .root_execution_id
                .as_deref()
                .unwrap(),
            paused_id
        );
        // The owner sees the notice after terminal billing reconciliation,
        // which can advance the generation after the controller gives back
        // its lease. Review that published state, not the earlier stop.
        let stopped = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if requests
                    .pending_request_snapshot(
                        &notice_id,
                        Some(&scope.principal),
                        Some(&scope.workspace),
                    )
                    .await
                    .is_some()
                {
                    let current = recovered.store(&scope).load(paused_id).unwrap();
                    if current.status == RunStatus::BudgetExhausted && current.lease.is_none() {
                        break current;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("worker must publish the durable budget review after reconciliation");
        let continuation = recovered.budget_continuation_view(&stopped).await.unwrap();
        assert_eq!(continuation["available"], true, "{continuation}");
        recovered
            .project(recovered.store(&scope), &stopped)
            .await
            .unwrap();
        assert_eq!(
            stopped.budget.spend().unwrap(),
            paused.budget.spend().unwrap()
        );
        let mut monitor_setup: TextDraftSetup = serde_json::from_slice(
            &recovered
                .store(&scope)
                .read_blob(paused_id, &stopped.admission.source_hash)
                .unwrap(),
        )
        .unwrap();
        monitor_setup.guidance_origin = Some(
            super::super::learning::relentless_promotion::GuidanceRunOrigin::Adoption {
                policy_id: Uuid::new_v4(),
                version_hash: "unpublished-policy".into(),
                task_id: stopped.admission.task_id.clone(),
                baseline_contract_hash: contract_hash.clone(),
            },
        );
        recovered
            .project_guidance_monitor(recovered.store(&scope), &stopped, &monitor_setup)
            .await
            .expect("a resumable stop cannot publish a final regression or open a policy store");
        assert!(!stopped.budget.allowances()[&stopped.attempts[0].review_allowance_id].closed);
        let notice = requests
            .pending_request_snapshot(&notice_id, Some(&scope.principal), Some(&scope.workspace))
            .await
            .expect("budget stop has one durable continuation request");
        assert_eq!(notice.context["review_label"], "Review continuation");
        recovered
            .project(recovered.store(&scope), &stopped)
            .await
            .unwrap();
        assert!(recovered
            .orchestrator
            .event_broadcaster()
            .unwrap()
            .pending_hitl_request(&scope.principal, &scope.workspace, &checkpoint.attention_id)
            .is_some());
        let gateway = RelentlessService::new(
            recovered.workspace.clone(),
            recovered.router.clone(),
            recovered.artifacts.clone(),
            recovered.orchestrator.clone(),
            None,
        )
        .with_user_requests(requests.clone());
        gateway.controls.lock().unwrap().insert(
            (scope.principal.clone(), scope.workspace.clone(), paused_id),
            Control::default(),
        );
        assert!(gateway
            .approve_owner_budget_reset(
                scope.clone(),
                paused_id,
                Uuid::new_v4(),
                stopped.generation,
                contract_hash.clone(),
                BudgetReset {
                    wall_clock_secs: Some(86400),
                    cost_microusd: Some(100000000),
                    ..Default::default()
                }
            )
            .await
            .is_err());
        let reset_id = Uuid::new_v4();
        let reset = BudgetReset {
            wall_clock_secs: Some(86400),
            ..Default::default()
        };
        let resumed = gateway
            .approve_owner_budget_reset(
                scope.clone(),
                paused_id,
                reset_id,
                stopped.generation,
                contract_hash.clone(),
                reset.clone(),
            )
            .await
            .unwrap();
        assert_eq!(resumed.status, RunStatus::WaitingOwner);
        assert_eq!(
            resumed.budget.spend().unwrap(),
            paused.budget.spend().unwrap()
        );
        assert_eq!(
            resumed.attempts[0].checkpoint,
            paused.attempts[0].checkpoint
        );
        assert_eq!(resumed.contract, paused.contract);
        assert_eq!(resumed.budget_resets.len(), 1);
        assert_eq!(
            gateway
                .approve_owner_budget_reset(
                    scope.clone(),
                    paused_id,
                    reset_id,
                    stopped.generation,
                    contract_hash.clone(),
                    reset
                )
                .await
                .unwrap()
                .deadline_at,
            resumed.deadline_at
        );
        gateway
            .project(gateway.store(&scope), &resumed)
            .await
            .unwrap();
        assert!(requests
            .pending_request_snapshot(&notice_id, Some(&scope.principal), Some(&scope.workspace))
            .await
            .is_none());
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
        resumed
    } else {
        paused.clone()
    };

    let reconstructed = RelentlessService::new(
        recovered.workspace.clone(),
        recovered.router.clone(),
        recovered.artifacts.clone(),
        recovered.orchestrator.clone(),
        None,
    );
    reconstructed.recover().await.unwrap();
    let after = wait_for(&reconstructed, &scope, paused_id, |state| {
        state.status == RunStatus::WaitingOwner && state.lease.is_none()
    })
    .await;
    assert_eq!(
        serde_json::to_value(&after).unwrap(),
        serde_json::to_value(&paused).unwrap()
    );
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        2,
        "a waiting task must not repeat paid work or regenerate its question"
    );
    assert_eq!(std::fs::read(&work_file).unwrap(), REVISED.as_bytes());
    let parent = after.budget.operations()[&after.attempts[0].actor_operation_id]
        .intent
        .clone();
    if cancel_question {
        use super::super::realtime_events::RuntimeTransportEvent;
        let runtime = native_pause
            .pause_state
            .root_execution_id
            .as_deref()
            .unwrap();
        let attempt = native_pause
            .pause_state
            .controller_attempt_hash
            .as_deref()
            .unwrap();
        let bus = reconstructed.orchestrator.event_broadcaster().unwrap();
        let foreign_attempt = "b".repeat(64);
        let mut notices = Vec::new();
        for (ask, owner, workspace, task, execution, marker, source, retire) in [
            (
                "0123456789ab",
                scope.principal.as_str(),
                scope.workspace.as_str(),
                after.admission.task_id.as_str(),
                runtime,
                attempt,
                "agentic",
                true,
            ),
            (
                "0123456789ac",
                scope.principal.as_str(),
                scope.workspace.as_str(),
                after.admission.task_id.as_str(),
                runtime,
                foreign_attempt.as_str(),
                "agentic",
                false,
            ),
            (
                "0123456789ad",
                "another-owner",
                scope.workspace.as_str(),
                after.admission.task_id.as_str(),
                runtime,
                attempt,
                "agentic",
                false,
            ),
            (
                "0123456789ae",
                scope.principal.as_str(),
                scope.workspace.as_str(),
                "another-task",
                runtime,
                attempt,
                "agentic",
                false,
            ),
            (
                "0123456789af",
                scope.principal.as_str(),
                scope.workspace.as_str(),
                after.admission.task_id.as_str(),
                "another-execution",
                attempt,
                "agentic",
                false,
            ),
            (
                "0123456789b0",
                scope.principal.as_str(),
                scope.workspace.as_str(),
                after.admission.task_id.as_str(),
                runtime,
                attempt,
                "user_request",
                false,
            ),
            (
                "0123456789b1",
                scope.principal.as_str(),
                "another-workspace",
                after.admission.task_id.as_str(),
                runtime,
                attempt,
                "agentic",
                false,
            ),
        ] {
            let correlation = format!("{runtime}:controller:{marker}~{ask}");
            assert!(
                bus.emit_hitl_lifecycle_if_accepted(RuntimeTransportEvent::HitlRequested {
                    correlation_id: correlation.clone(),
                    source: source.into(),
                    input_type: "confirmation".into(),
                    prompt: "Opaque legacy limit notice".into(),
                    hint: None,
                    input_schema: Some(serde_json::json!({"pause_kind":"max_iterations"})),
                    task_id: Some(task.into()),
                    execution_id: Some(execution.into()),
                    agent_id: None,
                    principal: Some(owner.into()),
                    workspace: Some(workspace.into()),
                    timestamp: Utc::now().timestamp_millis(),
                })
            );
            notices.push((owner.to_owned(), workspace.to_owned(), correlation, retire));
        }
        // These are canonical notifications only: no ordinary pause was
        // persisted for the old bounded conclusion.
        assert_eq!(
            reconstructed
                .orchestrator
                .full_pause_store_handle()
                .unwrap()
                .get_pending_for_execution(runtime)
                .len(),
            1
        );
        reconstructed
            .store(&scope)
            .commit(
                paused_id,
                after.generation + 1,
                RunEvent::Cancelled,
                Utc::now(),
            )
            .unwrap();
        reconstructed.cancel_worker(&scope, paused_id);
        reconstructed.launch(scope.clone(), paused_id);
        wait_for(&reconstructed, &scope, paused_id, |state| {
            state.status == RunStatus::Cancelled && state.lease.is_none()
        })
        .await;
        let runtime = native_pause
            .pause_state
            .root_execution_id
            .as_deref()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while reconstructed
                .orchestrator
                .get_execution_status(runtime)
                .await
                .unwrap()
                != WaitingState::Cancelled
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect(
            "terminal billing and exact question retirement must precede cancellation projection",
        );
        let cancelled = reconstructed.store(&scope).load(paused_id).unwrap();
        reconstructed
            .project(reconstructed.store(&scope), &cancelled)
            .await
            .unwrap();
        assert!(reconstructed
            .orchestrator
            .full_pause_store_handle()
            .unwrap()
            .get_pending_for_execution(runtime)
            .is_empty());
        assert!(reconstructed
            .orchestrator
            .event_broadcaster()
            .unwrap()
            .pending_hitl_request(&scope.principal, &scope.workspace, &checkpoint.attention_id)
            .is_none());
        for (owner, workspace, correlation, retire) in &notices {
            assert_eq!(
                bus.pending_hitl_request(owner, workspace, correlation)
                    .is_none(),
                *retire,
                "only the exact terminal controller's canonical orphan may close: {correlation}"
            );
        }
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
        assert_eq!(cancelled.source_epoch, 0);
        assert!(cancelled.owner_inputs.is_empty());
        let spend = cancelled.budget.spend().unwrap();
        reconstructed.recover().await.unwrap();
        reconstructed
            .project(reconstructed.store(&scope), &cancelled)
            .await
            .unwrap();
        assert_eq!(
            reconstructed
                .store(&scope)
                .load(paused_id)
                .unwrap()
                .budget
                .spend()
                .unwrap(),
            spend
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
        queue.shutdown(Duration::from_secs(5)).await;
        return;
    }
    let before_answer = reconstructed
        .store(&scope)
        .adapter_budget(paused_id, &parent)
        .unwrap();
    assert_eq!(before_answer.physical_starts.len(), 2);
    let answer_gateway = RelentlessService::new(
        reconstructed.workspace.clone(),
        reconstructed.router.clone(),
        reconstructed.artifacts.clone(),
        reconstructed.orchestrator.clone(),
        None,
    );
    // Hold only the fixture's launch notification so this test can reconstruct
    // after durable answer commit and before any continuation dispatch.
    answer_gateway.controls.lock().unwrap().insert(
        (scope.principal.clone(), scope.workspace.clone(), paused_id),
        Control::default(),
    );
    let runtime = native_pause
        .pause_state
        .root_execution_id
        .as_deref()
        .unwrap();
    let answer_value =
        super::super::execution::agentic::UserInputValue::text("independent founders");
    assert!(answer_gateway
        .respond_owner_input(
            RunScope {
                principal: "another-owner".into(),
                workspace: scope.workspace.clone()
            },
            runtime,
            &checkpoint.attention_id,
            answer_value.clone(),
        )
        .await
        .is_err());
    assert!(answer_gateway
        .respond_owner_input(
            scope.clone(),
            runtime,
            "wrong-attention",
            answer_value.clone(),
        )
        .await
        .is_err());
    let gate = answer_gateway.admission_lock.lock().await;
    let mut disconnected_request = Box::pin(answer_gateway.respond_owner_input(
        scope.clone(),
        runtime,
        &checkpoint.attention_id,
        answer_value.clone(),
    ));
    assert!(futures::poll!(&mut disconnected_request).is_pending());
    drop(disconnected_request);
    drop(gate);
    let answered = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let state = answer_gateway.store(&scope).load(paused_id).unwrap();
            if state.owner_inputs.contains_key(&checkpoint.attention_id) {
                break state;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("a disconnected request must finish its authorized answer settlement");
    // A resident worker may acquire/renew its fenced lease during projection.
    // Compare all durable facts except that worker bookkeeping: answers,
    // authority, attempts, contract, deadline and spend must stay identical.
    let replay_facts = |state: &_| {
        let mut facts = serde_json::to_value(state).unwrap();
        let object = facts.as_object_mut().unwrap();
        object.remove("generation");
        object.remove("lease");
        facts
    };
    let replay = answer_gateway
        .respond_owner_input(
            scope.clone(),
            runtime,
            &checkpoint.attention_id,
            answer_value,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(replay_facts(&answered), replay_facts(&replay));
    assert!(answer_gateway
        .respond_owner_input(
            scope.clone(),
            runtime,
            &checkpoint.attention_id,
            super::super::execution::agentic::UserInputValue::text("conflicting audience"),
        )
        .await
        .is_err());
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        2,
        "routing and receipt replay cannot dispatch paid work"
    );
    let sealed = answer_gateway
        .store(&scope)
        .read_blob(paused_id, &answered.owner_inputs[&checkpoint.attention_id])
        .unwrap();
    let receipt: super::super::relentless_actor::RecordedOwnerInput =
        serde_json::from_slice(&sealed).unwrap();
    assert_eq!(receipt.response.input_type, native_pause.input_type);
    assert_eq!(receipt.fingerprint.len(), 64);
    assert_eq!(
        serde_json::to_value(receipt.request.as_deref()).unwrap(),
        serde_json::to_value(Some(&original_request)).unwrap()
    );
    assert_eq!(answered.source_epoch, after.source_epoch + 1);
    assert_eq!(
        answered.contract.hash().unwrap(),
        after.contract.hash().unwrap()
    );
    assert_eq!(answered.deadline_at, after.deadline_at);
    assert_eq!(answered.attempts[0].actor_operation_id, parent.id);
    assert_eq!(
        answered.budget.spend().unwrap(),
        after.budget.spend().unwrap()
    );
    // A cold runtime has none of the previous broadcaster's in-memory receipts.
    // Reconcile from the sealed answer before continuation: this covers a
    // process exit after answer commit but before resolution publication.
    let restart_catalog = Arc::new(FixtureCatalog(CapabilityWorkspaceManager::new(
        reconstructed.workspace.clone(),
        temp.path(),
    )));
    let (restart_artifacts, restart_orchestrator) =
        build_test_artifact_v2_harness_with_tool_catalog(temp.path(), restart_catalog);
    restart_orchestrator.set_definition_store(Arc::new(AgentDefinitionStore::with_workspace_root(
        reconstructed.workspace.base_root(),
    )));
    let bus = restart_orchestrator.event_broadcaster().unwrap();
    let resolution = super::super::realtime_events::RuntimeTransportEvent::HitlResolved {
        correlation_id: checkpoint.attention_id.clone(),
        source: "agentic".into(),
        outcome: "answered".into(),
        decision: None,
        task_id: Some(answered.admission.task_id.clone()),
        execution_id: Some(runtime.into()),
        agent_id: None,
        principal: Some(scope.principal.clone()),
        workspace: Some(scope.workspace.clone()),
        timestamp: receipt.response.timestamp.timestamp_millis(),
    };
    assert!(!bus.resolved_hitl_lifecycle_matches(
        &scope.principal,
        &scope.workspace,
        &checkpoint.attention_id,
        &resolution,
    ));
    let continuation = RelentlessService::new(
        reconstructed.workspace.clone(),
        reconstructed.router.clone(),
        restart_artifacts,
        restart_orchestrator,
        None,
    );
    let before_projection = replay_facts(&answered);
    continuation
        .project(continuation.store(&scope), &answered)
        .await
        .unwrap();
    assert!(bus.resolved_hitl_lifecycle_matches(
        &scope.principal,
        &scope.workspace,
        &checkpoint.attention_id,
        &resolution,
    ));
    continuation
        .project(continuation.store(&scope), &answered)
        .await
        .unwrap();
    assert_eq!(
        replay_facts(&continuation.store(&scope).load(paused_id).unwrap()),
        before_projection,
        "resolution replay must not record another answer or change spend"
    );
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        2,
        "projection recovery cannot dispatch productive work"
    );
    let (continuation, answered, active_attention) = if validation_retry {
        // Stage the ordinary validator's replacement-pause handoff after the
        // first answer, with the same paid parent and physical ledger. This
        // isolates publication/recovery from model-dependent interpretation.
        let runs = continuation.store(&scope);
        let holder = "validation-retry-fixture";
        let claimed = runs.claim(paused_id, holder.into(), Utc::now()).unwrap();
        runs.resume_budgeted_adapter_job(paused_id, holder, &parent, checkpoint, Utc::now())
            .unwrap();
        let (bytes, attention) =
            super::super::relentless_actor::validation_retry_checkpoint_fixture(
                &runs.read_blob(paused_id, &checkpoint.content_hash).unwrap(),
                native_pause,
            )
            .unwrap();
        let replacement = runs
            .checkpoint_budgeted_adapter_job(
                paused_id,
                holder,
                &parent,
                attention.clone(),
                &bytes,
                Utc::now(),
            )
            .unwrap();
        runs.commit(
            paused_id,
            claimed.generation,
            RunEvent::ActorCheckpointed {
                checkpoint: replacement,
                reason: "Validation fixture needs a corrected answer".into(),
            },
            Utc::now(),
        )
        .unwrap();
        let staged = runs
            .commit(
                paused_id,
                claimed.generation,
                RunEvent::LeaseReleased,
                Utc::now(),
            )
            .unwrap();
        assert_eq!(
            staged.budget.spend().unwrap(),
            answered.budget.spend().unwrap()
        );
        let stage_bytes = serde_json::to_value(&staged).unwrap();
        continuation.project(runs.clone(), &staged).await.unwrap();
        let first_publication = bus
            .pending_hitl_request(&scope.principal, &scope.workspace, &attention)
            .expect("replacement pause must publish its pinned question");

        let catalog = Arc::new(FixtureCatalog(CapabilityWorkspaceManager::new(
            continuation.workspace.clone(),
            temp.path(),
        )));
        let (artifacts, orchestrator) =
            build_test_artifact_v2_harness_with_tool_catalog(temp.path(), catalog);
        orchestrator.set_definition_store(Arc::new(AgentDefinitionStore::with_workspace_root(
            continuation.workspace.base_root(),
        )));
        let cold_bus = orchestrator.event_broadcaster().unwrap();
        assert!(cold_bus
            .pending_hitl_request(&scope.principal, &scope.workspace, &attention)
            .is_none());
        let cold = RelentlessService::new(
            continuation.workspace.clone(),
            continuation.router.clone(),
            artifacts,
            orchestrator,
            None,
        );
        cold.project(runs.clone(), &staged).await.unwrap();
        cold.project(runs.clone(), &staged).await.unwrap();
        assert_eq!(
            serde_json::to_value(
                cold_bus
                    .pending_hitl_request(&scope.principal, &scope.workspace, &attention)
                    .unwrap()
            )
            .unwrap(),
            serde_json::to_value(&first_publication).unwrap(),
            "cold retry must retain exact timestamp, schema and question"
        );
        assert_eq!(
            serde_json::to_value(runs.load(paused_id).unwrap()).unwrap(),
            stage_bytes
        );
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            2,
            "retry projection must not dispatch a model"
        );
        cold.controls.lock().unwrap().insert(
            (scope.principal.clone(), scope.workspace.clone(), paused_id),
            Control::default(),
        );
        let next = cold
            .respond_owner_input(
                scope.clone(),
                runtime,
                &attention,
                super::super::execution::agentic::UserInputValue::text("independent founders"),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(next.owner_inputs.len(), 2);
        assert_eq!(next.source_epoch, answered.source_epoch + 1);
        assert_eq!(
            next.budget.spend().unwrap(),
            answered.budget.spend().unwrap()
        );
        cold.controls.lock().unwrap().remove(&(
            scope.principal.clone(),
            scope.workspace.clone(),
            paused_id,
        ));
        (cold, next, attention)
    } else {
        (continuation, answered, checkpoint.attention_id.clone())
    };
    // Then exercise the native adapter, ordinary validator and original ledger
    // with the reconstructed orchestrator and pause store.
    continuation.recover().await.unwrap();
    let accepted = wait_for(&continuation, &scope, paused_id, |state| {
        state.status == RunStatus::Accepted && state.routing.is_some() && state.lease.is_none()
    })
    .await;
    assert_eq!(
        accepted.attempts.len(),
        1,
        "an answer must continue the original attempt"
    );
    assert_eq!(accepted.attempts[0].actor_operation_id, parent.id);
    assert_eq!(accepted.deadline_at, after.deadline_at);
    assert_eq!(accepted.source_epoch, answered.source_epoch);
    assert_eq!(
        accepted.contract.hash().unwrap(),
        after.contract.hash().unwrap()
    );
    assert_eq!(accepted.budget.spend().unwrap().reserved_microusd, 0);
    let after_answer = continuation
        .store(&scope)
        .adapter_budget(paused_id, &parent)
        .unwrap();
    assert_eq!(
        after_answer.maximum_operations,
        before_answer.maximum_operations
    );
    let settled_turns = if iteration_limit { 3 } else { 4 };
    assert_eq!(after_answer.physical_starts.len(), settled_turns);
    assert!(before_answer
        .physical_starts
        .iter()
        .all(|(id, started)| { after_answer.physical_starts.get(id) == Some(started) }));
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        settled_turns
    );
    assert_eq!(
        *questioning.calls.lock().unwrap(),
        BTreeMap::from([(1, settled_turns)])
    );
    assert_eq!(std::fs::read(&work_file).unwrap(), REVISED.as_bytes());
    if iteration_limit {
        assert!(
            continuation
                .orchestrator
                .full_pause_store_handle()
                .unwrap()
                .get_pending_for_execution(runtime)
                .is_empty(),
            "the answered continuation must not replace its material question with a limit prompt"
        );
    }
    assert!(
        continuation
            .orchestrator
            .full_pause_store_handle()
            .unwrap()
            .load_serializable_from_disk()
            .unwrap()
            .iter()
            .all(|pause| pause.pause_state.hitl_correlation_id() != active_attention),
        "a settled continuation must retire its exact answered pause after durable result sealing"
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        while continuation
            .orchestrator
            .get_execution_status(runtime)
            .await
            .unwrap()
            != WaitingState::WaitingUser
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("continued accepted draft must await its exact reviewed handoff");
    let observed = continuation
        .orchestrator
        .get_execution_status(runtime)
        .await
        .unwrap();
    continuation
        .orchestrator
        .transition_ordinary_status_if_current(
            runtime,
            &observed,
            super::super::orchestrator::WorkflowEvent::UserCancelled,
        )
        .await
        .unwrap();
    assert!(
        continuation
            .orchestrator
            .set_ordinary_execution_status_if_current(
                runtime,
                &observed,
                WaitingState::Executing,
                "stale fixture projection",
            )
            .await
            .is_err(),
        "a stale projection must not replace owner cancellation"
    );
    continuation.launch(scope.clone(), paused_id);
    let cancelled = wait_for(&continuation, &scope, paused_id, |state| {
        state.status == RunStatus::Cancelled && state.lease.is_none()
    })
    .await;
    assert_eq!(
        cancelled.budget.spend().unwrap(),
        accepted.budget.spend().unwrap()
    );
    assert_eq!(
        continuation
            .orchestrator
            .get_execution_status(runtime)
            .await
            .unwrap(),
        WaitingState::Cancelled
    );
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        settled_turns
    );
    queue.shutdown(Duration::from_secs(5)).await;
}

#[test]
fn review_ask_id_names_its_exact_run_and_artifact() {
    let run = Uuid::new_v4();
    let id = format!("exec_1:relentless-review:{run}:abc123");
    assert_eq!(review_ask_target(&id), Some((run, "abc123")));
    assert_eq!(
        review_ask_target(&format!("exec_1:relentless-review:{run}:")),
        None
    );
    assert_eq!(review_ask_target("exec_1:quality-budget:x:0"), None);
    assert_eq!(
        review_ask_target("exec_1:relentless-review:not-a-uuid:abc"),
        None
    );
}
