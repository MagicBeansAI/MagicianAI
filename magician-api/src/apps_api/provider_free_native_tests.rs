//! Real workflow admission, canonical execution and encrypted commits with no model service.
use super::*;
use magician::magician_v2::{
    agents::{definition_store::AgentDefinitionStore, memory::AgentMemoryResolver},
    chat::{
        models::{ChatChannel, ChatMessage, ChatMessageContent, ChatMessageDirection},
        storage::{publish_global_chat_store, ChatStore, FileChatStore},
    },
    query_analysis::operation_llm_router::{QueryAnalysisLLM, SimplifiedLLMResponse},
    test_support::build_test_artifact_v2_harness_with_llm,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    OnceLock,
};

// This focused binary otherwise has no subscriber, which discards the owner's
// invariant diagnostics and leaves a failed root with only its public status.
struct OwnerWarnings;
impl tracing::Subscriber for OwnerWarnings {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        *metadata.level() <= tracing::Level::WARN
            && (metadata.target().contains("apps") || metadata.target().contains("artifact_v2"))
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Fields(String);
        impl tracing::field::Visit for Fields {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                use std::fmt::Write;
                let _ = write!(&mut self.0, " {}={value:?}", field.name());
            }
        }
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        eprintln!("owner warning {}{}", event.metadata().target(), fields.0);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

struct FixtureRoot(Option<tempfile::TempDir>);
impl FixtureRoot {
    fn path(&self) -> &std::path::Path {
        self.0.as_ref().unwrap().path()
    }
}
impl Drop for FixtureRoot {
    fn drop(&mut self) {
        if std::thread::panicking() {
            if let Some(temp) = self.0.take() {
                eprintln!(
                    "native owner failed fixture retained at {}",
                    temp.keep().display()
                );
            }
        }
    }
}

#[derive(Default)]
struct UnavailableModel {
    attempts: AtomicUsize,
}
#[async_trait::async_trait]
impl QueryAnalysisLLM for UnavailableModel {
    async fn generate_analysis(&self, _: &str) -> anyhow::Result<SimplifiedLLMResponse> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("fixture model provider is unavailable")
    }
    async fn is_available(&self) -> bool {
        false
    }
    fn provider_name(&self) -> String {
        "unavailable-fixture".to_owned()
    }
}

fn owner() -> AuthenticatedAppScope {
    let now = Utc::now();
    let scope = AppScope {
        principal: AppReference::parse("anonymous").unwrap(),
        workspace: AppReference::parse("default").unwrap(),
    };
    let binding = scope_binding_ref(&scope).unwrap();
    AuthenticatedAppScope::from_verified_session(
        scope,
        binding,
        AppReference::parse("actor:owner").unwrap(),
        AppReference::parse("session:native-fixture").unwrap(),
        magician::magician_v2::apps::models::AppRevision::new(1).unwrap(),
        now,
        now + ChronoDuration::minutes(30),
    )
    .unwrap()
}

fn completed<'a>(
    artifact: &'a magician::magician_v2::artifact_v2::ArtifactV2Service,
    workflow: &'a AppWorkflowService,
    owner: &'a AuthenticatedAppScope,
    run: &'a str,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = AppActionResult<serde_json::Value>> + 'a>> {
    // Each independent poll is heap-owned; this scenario must not store all
    // four complete polling state machines in its ordinary test-thread frame.
    Box::pin(async move {
        // Observe the native node's existing 600-second execution deadline and
        // allow its owner to publish terminal cleanup. Latency is measured;
        // it is not a second production deadline imposed by this observer.
        tokio::time::timeout(std::time::Duration::from_secs(660), async {
            let mut previous_status = None;
            loop {
                let (handle, task_id, result) = workflow
                    .result_for_run(owner, run)
                    .await
                    .expect("read ordinary App run")
                    .into_parts();
                let scope = ScopeRef::system_internal_unauthenticated(
                    owner.scope().principal.as_str(),
                    owner.scope().workspace.as_str(),
                );
                let task = artifact
                    .get_task(&scope, &task_id)
                    .await
                    .expect("read canonical task status");
                let status =
                    app_run_status_from_task(&task.state.status).expect("canonical App status");
                if previous_status != Some(status) {
                    eprintln!(
                        "native owner run {}: status={status:?} active={:?} latest={:?}",
                        task_id,
                        task.state.active_root_execution_id,
                        task.state.latest_root_execution_id
                    );
                    previous_status = Some(status);
                }
                if status.is_terminal() {
                    assert_eq!(
                        status,
                        AppRunStatus::Completed,
                        "native workflow failed: {} state={:?} error={:?}",
                        handle.run_ref,
                        task.state,
                        result.as_ref().and_then(|value| value.error.as_ref())
                    );
                    return result.expect("completed App must expose its durable result");
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("provider-free native workflow must finish")
    })
}

#[actix_web::test]
async fn native_store_actions_run_concurrently_and_replay_without_a_model_provider() {
    tracing::subscriber::set_global_default(OwnerWarnings).expect("install owner test diagnostics");
    eprintln!("native owner fixture: setup");
    let temp_root = std::fs::canonicalize(std::env::temp_dir()).unwrap();
    let temp = FixtureRoot(Some(tempfile::tempdir_in(temp_root).unwrap()));
    let model = Arc::new(UnavailableModel::default());
    let (artifact, orchestrator) =
        build_test_artifact_v2_harness_with_llm(temp.path(), model.clone());
    let seed = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../magician_data_v3")
        .canonicalize()
        .unwrap();
    let workspace = artifact.workspace().clone().with_seed_root(&seed);
    let chat_store = Arc::new(FileChatStore::with_workspace_layout(workspace.clone()));
    let session = chat_store
        .get_or_create_active_session(
            "anonymous",
            "default",
            "meeting-native-fixture",
            &ChatChannel::web(),
            "personal-assistant",
        )
        .await
        .unwrap();
    chat_store
        .append_message(
            &session.id,
            ChatMessage::new(
                "native-transcript-line".to_owned(),
                session.id.clone(),
                ChatMessageDirection::System,
                ChatMessageContent::Text {
                    text: "Operator: Verify deterministic App synchronization.".to_owned(),
                    plan_reply: None,
                },
                Utc::now().timestamp_millis(),
            )
            .with_source_surface(Some("meeting_transcript".to_owned())),
        )
        .await
        .unwrap();
    assert!(
        publish_global_chat_store(chat_store),
        "fixture owns the process chat store"
    );
    let config = magician::config::MagicianConfig::default();
    artifact
        .set_app_resource_policy(config.app_platform.resources.enforcement_policy())
        .unwrap();
    let definitions = Arc::new(AgentDefinitionStore::with_workspace_layout(
        workspace.clone(),
    ));
    orchestrator.set_definition_store(definitions.clone());
    let resources = Arc::new(AgentResources {
        magician_config: Arc::new(std::sync::RwLock::new(config)),
        memory_resolver: Arc::new(AgentMemoryResolver::with_workspace_layout(
            workspace.clone(),
        )),
        agent_definition_store: definitions,
        artifact_workspace: workspace.clone(),
        artifact_v2_service: Some(artifact.clone()),
        event_broadcaster: None,
        operation_llm_router: None,
        secret_store_resolver: None,
        content_acquisition_resolver: Arc::new(std::sync::RwLock::new(None)),
        file_sandbox: Default::default(),
        tool_index: Arc::new(OnceLock::new()),
        user_request_service: None,
        agent_runtime: None,
    });
    assert!(resources.operation_llm_router.is_none());
    let resolver = orchestrator.build_scoped_capability_resolver();
    resolver.set_agent_resources(resources.clone());
    orchestrator.set_scoped_capability_resolver(resolver.clone());
    let api = AppPlatformApi::new(workspace);
    let workflow = artifact.app_workflow_service();
    api.set_workflow_service(workflow.clone());
    api.set_scoped_capability_resolver(resolver);
    eprintln!("native owner fixture: boot admission");
    let reports = Box::pin(api.admit_system_packages_at_boot(AppSystemPackageSettings {
        admit_at_boot: true,
        enable_at_boot: true,
    }))
    .await;
    eprintln!("native owner fixture: boot admission returned");
    let mut installations = BTreeMap::new();
    for summary in reports.iter().filter(|summary| {
        summary.scope_principal == DEFAULT_SCOPE_PRINCIPAL
            && summary.scope_workspace == DEFAULT_SCOPE_WORKSPACE
    }) {
        assert!(
            summary.enablement_failures.is_empty(),
            "{:?}",
            summary.enablement_failures
        );
        for outcome in &summary.report.outcomes {
            let receipt = outcome
                .result
                .as_ref()
                .unwrap_or_else(|error| panic!("{}: {error}", outcome.package_dir));
            installations.insert(
                outcome.package_dir.as_str(),
                receipt.installation_id.clone(),
            );
        }
    }
    assert_eq!(
        installations.len(),
        5,
        "all five production packages must admit"
    );
    let owner = owner();
    let learning_action = AppName::parse("approve_candidate").unwrap();
    let meetings_action = AppName::parse("pause").unwrap();
    let learning_key = AppReference::parse("request:native-learning").unwrap();
    let meetings_key = AppReference::parse("request:native-meetings").unwrap();
    let caller = AppReference::parse("surface:native-fixture").unwrap();
    let learning_input =
        json!({"candidate_id":"candidate-fixture","reason":"Explicit owner decision"});
    let meetings_input = json!({"request_id":"control-fixture","session_id":"fixture-session"});
    let before_actions = api.registry.connection_pool_stats();
    let pair_started = std::time::Instant::now();
    let (learning, meetings) = tokio::join!(
        Box::pin(workflow.invoke_direct_input(
            &owner,
            &installations["learning"],
            &learning_action,
            learning_key.clone(),
            learning_input.clone(),
            None,
            caller.clone(),
            &resources,
            Utc::now()
        )),
        Box::pin(workflow.invoke_direct_input(
            &owner,
            &installations["meetings"],
            &meetings_action,
            meetings_key.clone(),
            meetings_input.clone(),
            None,
            caller.clone(),
            &resources,
            Utc::now()
        )),
    );
    let learning = learning.expect("accept learning action with no model provider");
    let meetings = meetings.expect("accept meetings action with no model provider");
    eprintln!("native owner accepted learning={learning:?} meetings={meetings:?}");
    assert!(
        learning.execution_id.is_some(),
        "accepted fresh learning action must start its canonical root"
    );
    assert!(
        meetings.execution_id.is_some(),
        "accepted fresh meeting action must start its canonical root"
    );
    let accepted_ms = pair_started.elapsed().as_millis();
    let (first, second) = tokio::join!(
        completed(
            &artifact,
            &workflow,
            &owner,
            learning.run_handle.run_ref.as_str()
        ),
        completed(
            &artifact,
            &workflow,
            &owner,
            meetings.run_handle.run_ref.as_str()
        )
    );
    let completed_ms = pair_started.elapsed().as_millis();
    for launch in [&learning, &meetings] {
        let (_, task) = artifact
            .get_task_by_id(&launch.task_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            task.manifest.lifecycle,
            magician::magician_v2::artifact_v2::models::TaskLifecycle::Internal
        );
    }
    assert_eq!(first.mutation_receipt_refs.len(), 1);
    assert_eq!(second.mutation_receipt_refs.len(), 1);
    assert_eq!(
        model.attempts.load(Ordering::SeqCst),
        0,
        "mechanical tasks cannot fall back to query-analysis generation"
    );
    let replay = workflow
        .invoke_direct_input(
            &owner,
            &installations["meetings"],
            &meetings_action,
            meetings_key,
            meetings_input.clone(),
            None,
            caller.clone(),
            &resources,
            Utc::now(),
        )
        .await
        .unwrap();
    assert_eq!(
        replay.task_id, meetings.task_id,
        "same request must keep its canonical task"
    );
    assert_eq!(
        replay.result.as_ref().unwrap().mutation_receipt_refs,
        second.mutation_receipt_refs
    );
    // A new invocation of the same already-recorded control request has an
    // empty plan. It must publish a readable result without inventing a receipt.
    let no_change = workflow
        .invoke_direct_input(
            &owner,
            &installations["meetings"],
            &meetings_action,
            AppReference::parse("request:native-meetings-no-change").unwrap(),
            meetings_input,
            None,
            caller,
            &resources,
            Utc::now(),
        )
        .await
        .unwrap();
    let empty = completed(
        &artifact,
        &workflow,
        &owner,
        no_change.run_handle.run_ref.as_str(),
    )
    .await;
    assert!(empty.mutation_receipt_refs.is_empty());
    assert_eq!(model.attempts.load(Ordering::SeqCst), 0);
    // Exercise a real, attested host-read primitive as well as own-store
    // decisions. The ordinary scoped resolver owns provider selection; no
    // fixture callback supplies rows or a forged builtin witness.
    let roster = workflow
        .invoke_direct_input(
            &owner,
            &installations["town_square"],
            &AppName::parse("sync_roster").unwrap(),
            AppReference::parse("request:native-roster").unwrap(),
            json!({"mode":"snapshot"}),
            None,
            AppReference::parse("surface:native-fixture").unwrap(),
            &resources,
            Utc::now(),
        )
        .await
        .expect("accept native roster read without a model");
    let roster_result = completed(
        &artifact,
        &workflow,
        &owner,
        roster.run_handle.run_ref.as_str(),
    )
    .await;
    assert!(
        !roster_result.mutation_receipt_refs.is_empty(),
        "real roster must populate the empty App store"
    );
    assert_eq!(model.attempts.load(Ordering::SeqCst), 0);
    // Cover every remaining shipped mechanical workflow through the same
    // real owner. Inputs and source data are confined to this isolated scope.
    let cases = remaining_mechanical_cases();
    assert_eq!(cases.len() + 3, 29);
    for (package, action, input) in cases {
        let started = std::time::Instant::now();
        let launch = Box::pin(workflow.invoke_direct_input(
            &owner,
            &installations[package],
            &AppName::parse(action).unwrap(),
            AppReference::parse(format!("request:native-{package}-{action}")).unwrap(),
            input,
            None,
            AppReference::parse("surface:native-fixture").unwrap(),
            &resources,
            Utc::now(),
        ))
        .await
        .unwrap_or_else(|error| panic!("{package}.{action} admission: {error}"));
        let result = completed(
            &artifact,
            &workflow,
            &owner,
            launch.run_handle.run_ref.as_str(),
        )
        .await;
        assert_eq!(
            model.attempts.load(Ordering::SeqCst),
            0,
            "{package}.{action} called a model"
        );
        println!(
            "native_mechanical_case={}",
            json!({
                "package": package, "action": action, "completed_ms": started.elapsed().as_millis(),
                "receipts": result.mutation_receipt_refs.len(), "model_attempts": 0,
            })
        );
    }
    let after_actions = api.registry.connection_pool_stats();
    assert!(
        after_actions.reused > before_actions.reused,
        "ordinary actions must reuse authenticated connections"
    );
    println!(
        "native_owner_measurements={}",
        json!({
            "concurrent_actions": 2,
        "attested_host_read_actions": 1,
            "both_accepted_ms": accepted_ms,
            "both_completed_ms": completed_ms,
            "model_attempts": model.attempts.load(Ordering::SeqCst),
            "registry_operations": after_actions.completed_operations - before_actions.completed_operations,
            "connection_opens": after_actions.opened - before_actions.opened,
            "connection_reuses": after_actions.reused - before_actions.reused,
            "statements": after_actions.sql.statements - before_actions.sql.statements,
            "statement_ns": after_actions.sql.statement_ns - before_actions.sql.statement_ns,
            "transaction_ns": after_actions.sql.transaction_ns - before_actions.sql.transaction_ns,
        })
    );
    Box::pin(round_without_model_runtime_settles_failures(
        &api, &artifact, &owner, &resources,
    ))
    .await;
}

async fn round_without_model_runtime_settles_failures(
    api: &AppPlatformApi,
    artifact: &Arc<magician::magician_v2::artifact_v2::ArtifactV2Service>,
    owner: &AuthenticatedAppScope,
    resources: &AgentResources,
) {
    use magician::magician_v2::apps::contextual_round::{AppContextualRound, AppRoundUsage};
    let workflow = api.workflow_service();
    workflow.set_background_behavior_runtime_admission(true);
    let scheduler = AppBehaviorScheduler::new(
        api.registry.clone(),
        api.stager.clone(),
        AppBehaviorRuntimeLimits {
            max_installations_per_scope: 64,
            max_claims_per_scope_tick: 8,
            lease_seconds: 120,
            retry_seconds: 10,
        },
    )
    .unwrap();
    let scope = ScopeRef::system_internal_unauthenticated("anonymous", "default");
    let probe = AppArtifactTaskAcceptanceProbe::new(Some(artifact.clone()), scope.clone());
    let worker = AppReference::parse("run:native-round-fixture").unwrap();
    // Reconcile through the real scheduler, then make only the isolated
    // fixture's due timestamp eligible through the explicit test-only seam.
    // Claim, grants, source binding, launch and settlement remain real owners.
    assert!(scheduler
        .claim_due(owner, &worker, &probe, Utc::now())
        .await
        .unwrap()
        .is_empty());
    let due = (Utc::now() - ChronoDuration::seconds(1))
        .to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    let changed = api.registry.execute_scoped_test_write(owner, &Utc::now(), move |connection, _| {
        Ok(connection.execute(
            "UPDATE app_behavior_heads SET next_due_at = ?1, available_at = ?1, revision = revision + 1, fence = fence + 1
             WHERE behavior_id = 'ambient_turn' AND state = 'idle' AND pending_fire_ref IS NULL AND lease_token IS NULL",
            [due],
        )?)
    }).await.unwrap();
    assert_eq!(
        changed, 1,
        "exactly the fixture's idle native round becomes due"
    );
    let dispatches = scheduler
        .claim_due(owner, &worker, &probe, Utc::now())
        .await
        .unwrap();
    let dispatch = dispatches
        .iter()
        .find(|dispatch| dispatch.behavior_id().as_str() == "ambient_turn")
        .expect("the enabled native round must be claimed by its normal scheduler");
    let authority = scheduler
        .authorize_dispatch(owner, dispatch, Utc::now())
        .await
        .unwrap();
    let round_started = std::time::Instant::now();
    let launch = Box::pin(api.invoke_scheduled_app_behavior(
        owner,
        dispatch,
        resources,
        authority,
        Utc::now(),
    ))
    .await
    .expect("normal native round admission");
    scheduler
        .settle(
            owner,
            dispatch,
            AppBehaviorSettlement::Accepted,
            None,
            Utc::now(),
        )
        .await
        .unwrap();
    completed(
        artifact,
        &workflow,
        owner,
        launch.run_handle.run_ref.as_str(),
    )
    .await;
    let path = artifact
        .workspace()
        .execution_dir(
            "anonymous",
            "default",
            &launch.task_id,
            launch.execution_id.as_deref().unwrap(),
        )
        .join("app_workflow_run.json");
    let state: serde_json::Value = artifact.workspace().read_json_path(path).await.unwrap();
    let rounds = state["payload"]["native_rounds"].as_object().unwrap();
    assert_eq!(rounds.len(), 1);
    let round: AppContextualRound =
        serde_json::from_value(rounds.values().next().unwrap()["round"].clone()).unwrap();
    let summary = round.summary().unwrap();
    assert!(round.is_complete());
    for participant in round.participants() {
        let state = serde_json::to_value(participant.state()).unwrap();
        assert_eq!(
            state["code"], "semantic_dispatch_failed",
            "every eligible agent must reach semantic dispatch preparation: {state}"
        );
    }
    assert!(
        summary.failed > 1,
        "provider absence must settle independent participant failures: {summary:?}"
    );
    assert_eq!(summary.committed, 0);
    assert_eq!(
        summary.quiet, 0,
        "provider absence is a failure, not an agent's quiet decision"
    );
    assert_eq!(summary.spent, AppRoundUsage::default());
    assert_eq!(summary.reserved, AppRoundUsage::default());
    println!(
        "native_round_model_runtime_unavailable={}",
        serde_json::to_string(&summary).unwrap()
    );
    println!(
        "native_round_host_measurement={}",
        serde_json::json!({
            "elapsed_ms": round_started.elapsed().as_millis(),
            "retained_records": state["payload"]["labeled_tool_results"].as_array().unwrap().len(),
            "encoded_state_bytes": serde_json::to_vec(&state).unwrap().len(),
            "eligible_participants": round.participants().len(),
            "model_calls": 0,
        })
    );
}

fn remaining_mechanical_cases() -> Vec<(&'static str, &'static str, serde_json::Value)> {
    let now = Utc::now().timestamp_millis();
    vec![
        (
            "town_square",
            "create_group",
            json!({"created_by":"operator","group_id":"fixture-group","name":"Fixture group"}),
        ),
        (
            "town_square",
            "publish_post",
            json!({"author_id":"operator","body":"Fixture post","post_id":"fixture-post","post_type":"thought","surface":"feed"}),
        ),
        (
            "town_square",
            "react_to_post",
            json!({"emoji":"👍","member_id":"operator","post_id":"fixture-post","reaction_id":"fixture-reaction","removed":false}),
        ),
        (
            "town_square",
            "set_policy",
            json!({"autonomy_state":"on","cooldown_seconds":300,"max_autonomous_replies":4,"max_post_chars":1600}),
        ),
        ("town_square", "sync_feed", json!({})),
        (
            "meetings",
            "join",
            json!({"gesture_expires_at_ms":now+60000,"gesture_id":"fixture-join-gesture","gesture_observed_at_ms":now,"request_id":"fixture-join","surface_session_id":"fixture-surface","url":"https://example.com/fixture-meeting"}),
        ),
        (
            "meetings",
            "listen",
            json!({"capture_mic":false,"gesture_expires_at_ms":now+60000,"gesture_id":"fixture-listen-gesture","gesture_observed_at_ms":now,"request_id":"fixture-listen","surface_session_id":"fixture-surface"}),
        ),
        (
            "meetings",
            "resume",
            json!({"request_id":"fixture-resume","session_id":"fixture-session"}),
        ),
        (
            "meetings",
            "stop",
            json!({"request_id":"fixture-stop","session_id":"fixture-session"}),
        ),
        ("meetings", "sync_sessions", json!({})),
        ("meetings", "sync_threads", json!({})),
        (
            "meetings",
            "read_transcript",
            json!({"thread_id":"meeting-native-fixture"}),
        ),
        (
            "meetings",
            "search_meetings",
            json!({"text":"deterministic"}),
        ),
        ("meetings", "sync_takeaways", json!({})),
        ("meetings", "sync_upcoming", json!({})),
        (
            "learning",
            "reject_candidate",
            json!({"candidate_id":"fixture-rejected","reason":"Owner rejects fixture"}),
        ),
        (
            "learning",
            "snooze_candidate",
            json!({"candidate_id":"fixture-snoozed","reason":"Owner defers fixture"}),
        ),
        ("learning", "sync_queue", json!({})),
        ("thinking_map", "sync_maps", json!({})),
        (
            "claims_review",
            "confirm_claim",
            json!({"claim_id":"fixture-claim","expected_revision":1,"reason":"Owner confirms fixture","request_id":"fixture-confirm"}),
        ),
        (
            "claims_review",
            "reject_claim",
            json!({"claim_id":"fixture-claim","expected_revision":1,"reason":"Owner rejects fixture","request_id":"fixture-reject"}),
        ),
        (
            "claims_review",
            "record_commitment",
            json!({"claim_id":"fixture-claim","expected_revision":1,"request_id":"fixture-record"}),
        ),
        (
            "claims_review",
            "confirm_commitment",
            json!({"audience_id":"owner","audience_kind":"person","commitment_id":"fixture-commitment","expected_revision":1,"request_id":"fixture-commitment-confirm"}),
        ),
        (
            "claims_review",
            "stage_ingest",
            json!({"audience_id":"owner","audience_kind":"person","ingest_id":"fixture-ingest","outwardness_reason":"Owner supplied fixture","speaker_mapping_json":json!({"speakers":{"operator":"Operator"},"utterances":[{"speaker":"operator","text":"Verify deterministic App synchronization."}]}).to_string(),"transcript_text":"Operator: Verify deterministic App synchronization."}),
        ),
        ("claims_review", "sync_claims", json!({})),
        (
            "claims_review",
            "sync_context",
            json!({"agent_id":"personal-assistant","audience_id":"owner","audience_kind":"person"}),
        ),
    ]
}

#[actix_web::test]
async fn a_blocked_task_does_not_stop_an_unrelated_tasks_execution_owner() {
    use magician::magician_v2::artifact_v2::{
        reducer::{ArtifactV2Reducer, FilesystemArtifactV2Reducer},
        task_writes::TaskWriteReconciler,
    };
    let root = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(root.path());
    let reducer = Arc::new(FilesystemArtifactV2Reducer::new(
        workspace.clone(),
        Arc::new(TaskWriteReconciler::new(workspace.clone())),
    ));
    let scope = ScopeRef::system_internal_unauthenticated("anonymous", "default");
    let path = workspace.task_lock_path("anonymous", "default", "task_busy");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    held.lock().unwrap();
    let pending_reducer = reducer.clone();
    let pending_scope = scope.clone();
    let (entered, started) = tokio::sync::oneshot::channel();
    let blocked = tokio::spawn(async move {
        let _ = entered.send(());
        pending_reducer
            .list_executions(&pending_scope, "task_busy")
            .await
    });
    started.await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(
        !blocked.is_finished(),
        "the first task must still be excluded by its file owner"
    );
    let unrelated = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        reducer.list_executions(&scope, "task_free"),
    )
    .await;
    // Always release the fixture lock before asserting, including on the
    // pre-fix failure path, so a blocked filesystem worker cannot leak.
    drop(held);
    tokio::time::timeout(std::time::Duration::from_secs(2), blocked)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(unrelated
        .expect("an unrelated task must not wait behind the busy task")
        .unwrap()
        .is_empty());
}

#[actix_web::test]
async fn background_scope_sweeps_progress_while_another_workspace_waits() {
    let release_slow = Arc::new(tokio::sync::Notify::new());
    let finished = Arc::new(AtomicUsize::new(0));
    let scopes = ["slow", "ready-a", "ready-b", "ready-c", "ready-d"]
        .into_iter()
        .map(|name| ("anonymous".to_owned(), name.to_owned()))
        .collect();
    let sweep = run_background_scope_batch(scopes, |(_, workspace)| {
        let release_slow = release_slow.clone();
        let finished = finished.clone();
        async move {
            if workspace == "slow" {
                release_slow.notified().await;
            } else if finished.fetch_add(1, Ordering::SeqCst) + 1 == 4 {
                release_slow.notify_one();
            }
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), sweep)
        .await
        .expect("one workspace cannot serialize unrelated scope sweeps");
    assert_eq!(finished.load(Ordering::SeqCst), 4);
}
