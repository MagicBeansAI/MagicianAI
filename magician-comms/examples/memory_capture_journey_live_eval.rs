//! Persist a chat, really distil/approve it, then test recall and connection delivery.
//! The capture service is driven explicitly; background scheduling is a separate gate.
use anyhow::{ensure, Context, Result};
use magician::{
    config::load_magician_config_from_path,
    magician_v2::{
        agents::AgentMemoryResolver,
        artifact_v2::workspace::ArtifactV2Workspace,
        attention::resurfacing::{memory_connections::*, store::ResurfacingStore, types::*},
        attention_funnel::AttentionScope,
        chat::{
            models::*,
            storage::{ChatStore, FileChatStore},
        },
        feed::FeedStore,
        llm_dispatch_seam::{DistillLlm, RouterDistillLlm},
        notes::NotesSettingsStore,
        prompts::{self, JsonPromptStorage, PromptManager},
        query_analysis::operation_llm_router::OperationLlmRouter,
        realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent},
        taste_capture::{
            transcript_from_messages, TasteCaptureService, TASTE_PROFILE_DISTILL_OPERATION,
        },
        taste_profile::{
            install_global_taste_profile_loader, TasteProfileLoader, TasteProfileSettings,
        },
        user_requests::UserRequestService,
    },
};
use magician_comms::channel_assist::resurfacing::{
    interaction::{MemoryInteractionAdapter, ResurfacingInteractionRegistry},
    memory_connections::ConnectionRuntime,
};
use serde_json::{json, Value};
use std::{collections::HashMap, path::PathBuf, sync::Arc};

struct ObservedDistiller {
    inner: RouterDistillLlm,
    replies: std::sync::Mutex<Vec<String>>,
}
#[async_trait::async_trait]
impl DistillLlm for ObservedDistiller {
    async fn complete(&self, system: &str, user: &str) -> Result<String> {
        let reply = self.inner.complete(system, user).await?;
        self.replies.lock().unwrap().push(reply.clone());
        Ok(reply)
    }
}

async fn run(config: &str, root: PathBuf) -> Result<Value> {
    std::fs::create_dir(&root)?;
    let config_path = PathBuf::from(config);
    let config_root = config_path
        .parent()
        .context("config needs a parent directory")?;
    for name in [".env.development", ".env"] {
        let env_path = config_root.join(name);
        if env_path.exists() {
            // Never print dotenv parser errors: they can include secret-bearing lines.
            ensure!(
                dotenvy::from_path(&env_path).is_ok(),
                "could not parse runtime environment file {}",
                env_path.display()
            );
        }
    }
    magician::magician_v2::query_analysis::operation_llm_router::install_llm_routing_overrides(
        config_path
            .parent()
            .context("config needs a parent directory")?,
    );
    let layout = ArtifactV2Workspace::new(&root);
    let scope = AttentionScope {
        principal: "memory-eval".into(),
        workspace: "capture-journey".into(),
    };
    let router = Arc::new(OperationLlmRouter::new(Some(
        load_magician_config_from_path(&config_path)?
            .router_config()
            .cloned()
            .context("missing router")?,
    )));
    if router.shared_configured_router().is_none() {
        let diagnostics = load_magician_config_from_path(&config_path)?;
        magician::magician_v2::llm_chunking::validate_router_chunking_config(
            diagnostics.router_config().context("missing router")?,
        )?;
        magicllm::ConfiguredRouter::from_router_config(
            diagnostics
                .router_config()
                .cloned()
                .context("missing router")?,
        )?;
        anyhow::bail!("operation router initialization failed validation");
    }
    ensure!(
        router
            .explicit_binding_for_operation(TASTE_PROFILE_DISTILL_OPERATION)
            .is_some(),
        "capture model unbound"
    );
    let manager = Arc::new(PromptManager::new(Arc::new(
        JsonPromptStorage::with_default_config()?,
    )));
    prompts::set_global_prompt_manager(manager);
    let notes = NotesSettingsStore::with_workspace_layout(layout.clone());
    let settings = TasteProfileSettings {
        capture_enabled: true,
        ..Default::default()
    };
    let loader = Arc::new(TasteProfileLoader::new(notes, settings.clone()));
    install_global_taste_profile_loader(loader.clone());
    let capture = TasteCaptureService::new(loader.clone(), root.join("capture-state"));
    let chat = FileChatStore::with_workspace_layout_index(layout.clone()).await?;
    let session = chat
        .new_session(
            &scope.principal,
            &scope.workspace,
            "general",
            &ChatChannel::web(),
            "personal-assistant",
        )
        .await?;
    let owner_text="I am vegetarian. As a standing preference, whenever you help plan my meals, flag any menu that has no vegetarian main course.";
    chat.append_message(
        &session.id,
        ChatMessage::new(
            "owner-preference",
            &session.id,
            ChatMessageDirection::User,
            ChatMessageContent::Text {
                text: owner_text.into(),
                plan_reply: None,
            },
            chrono::Utc::now().timestamp_millis(),
        ),
    )
    .await?;
    drop(chat);
    let chat = FileChatStore::with_workspace_layout_index(layout.clone()).await?;
    let messages = chat.get_messages(&session.id, 20).await?;
    let transcript = transcript_from_messages(&messages);
    let sections = vec![
        "Voice".to_string(),
        "Process".to_string(),
        "Boundaries".to_string(),
    ];
    let variables = HashMap::from([
        ("current_profile".into(), "(no profile written yet)".into()),
        (
            "destination_sections".into(),
            sections
                .iter()
                .map(|s| format!("- {s}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        (
            "max_candidates".into(),
            settings.max_proposals_per_day.to_string(),
        ),
    ]);
    let system = prompts::rendered_prompt(
        prompts::names::TASTE_PROFILE_DISTILL,
        prompts::versions::TASTE_PROFILE_DISTILL,
        variables,
    )
    .await?;
    let events = Arc::new(RuntimeTransportBroadcaster::new(64));
    let mut receiver = events.subscribe();
    let llm = ObservedDistiller {
        inner: RouterDistillLlm::new_for_operation(
            router.clone(),
            Some(events),
            &scope.principal,
            &scope.workspace,
            TASTE_PROFILE_DISTILL_OPERATION,
            false,
        ),
        replies: Default::default(),
    };
    let outcome = capture
        .distill_session(
            &scope.principal,
            &scope.workspace,
            &llm,
            &system,
            &transcript,
            &session.id,
            &sections,
            chrono::Utc::now(),
        )
        .await?;
    let mut usage = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        if let RuntimeTransportEvent::LLMResponseReceived {
            provider,
            model,
            cost,
            input_tokens,
            output_tokens,
            usage_reported,
            ..
        } = event
        {
            usage.push(json!({"provider":provider,"model":model,"router_estimated_cost_usd":if usage_reported && cost>0.0{Some(cost)}else{None},"usage_reported":usage_reported,"input_tokens":input_tokens,"output_tokens":output_tokens}));
        }
    }
    let capture_outcome = json!({"filed":outcome.filed,"retryable_failure":outcome.retryable_failure,
        "suppressed_decided":outcome.suppressed_decided,"suppressed_duplicate":outcome.suppressed_duplicate,
        "suppressed_capped":outcome.suppressed_capped});
    if outcome.retryable_failure.is_some() || !usage.iter().any(|u| u["usage_reported"] == true) {
        return Ok(
            json!({"inconclusive":true,"error":"capture failed or usage unavailable; no subsequent model calls",
            "capture_outcome":capture_outcome,"capture_usage":usage,"capture_replies":llm.replies.into_inner().unwrap()}),
        );
    }
    let proposals = capture
        .store_for(&scope.principal, &scope.workspace)
        .await
        .pending()
        .await?;
    let before_approval = loader.load(&scope.principal, &scope.workspace).await;
    let mut approvals = Vec::new();
    for proposal in &proposals {
        // Only approve the fixture's stated preference with an actual owner quote.
        if proposal.evidence.iter().any(|q| owner_text.contains(q))
            && proposal.directive.to_lowercase().contains("vegetarian")
        {
            let result = capture
                .approve(
                    &scope.principal,
                    &scope.workspace,
                    &proposal.id,
                    chrono::Utc::now(),
                )
                .await?;
            approvals.push(json!({"id":proposal.id,"result":format!("{result:?}")}));
        }
    }
    let profile = loader.load(&scope.principal, &scope.workspace).await;
    let resolver = AgentMemoryResolver::with_workspace_layout(layout.clone());
    let memory = resolver.resolve_for_scope(&scope.principal, &scope.workspace)?;
    let activity="The proposed dinner menu has only beef and chicken mains, with no vegetarian substitutions.";
    memory.persist_user_knowledge(&json!({"user.knowledge":[{"key":"current_activity","value":activity,"updated_at":"2026-09-12T00:00:00Z"}]})).await?;
    let recalled = recall(&memory, activity).await?;
    let structured_profile_recalled = recalled
        .iter()
        .any(|s| s.text.to_lowercase().contains("vegetarian") && !s.text.contains(activity));
    let requests = Arc::new(
        UserRequestService::new(Arc::new(RuntimeTransportBroadcaster::new(64)))
            .with_workspace_layout(layout.clone())
            .with_history_persist_path(root.join("request-history.json"))
            .with_pending_persist_path(root.join("request-pending.json"))
            .await,
    );
    let runtime = ConnectionRuntime {
        store: ResurfacingStore::open(&root)?,
        resolver: resolver.clone(),
        interactions: ResurfacingInteractionRegistry::from_adapters(vec![Arc::new(
            MemoryInteractionAdapter::new(resolver),
        )]),
        feed: FeedStore::open_workspace(layout)?,
        requests: Some(requests),
        router: Some(router),
        attention: None,
        taste: Some(loader.clone()),
    };
    let now = chrono::Utc::now().timestamp();
    let candidate = Candidate {
        candidate_id: "capture-journey".into(),
        source_kind: SourceKind::Memory,
        source_ref: "user.knowledge#current_activity".into(),
        title: "Dinner planning".into(),
        content_digest: activity.into(),
        content_revision: Some("2026-09-12T00:00:00Z".into()),
        content_details: None,
        semantic_features: None,
        salience_score: 0.9,
        signals: Default::default(),
        temporal_anchor_at: None,
        embedding_id: None,
        state: CandidateState::Candidate,
        first_seen_at: now,
        last_scored_at: now,
        last_surfaced_at: None,
        cooldown_until: 0,
        surface_count: 0,
        dismiss_count: 0,
    };
    runtime
        .store
        .upsert_candidate(&scope.principal, &scope.workspace, &candidate)
        .await?;
    let mut observations = Vec::new();
    runtime.pass_with_observer(&scope,&mut None,now,&mut |o|observations.push(json!({"sources":o.sources,"response":o.response.as_ref().map(|r|&r.content),"error":o.error,"latency_ms":o.elapsed_ms,
        "usage_reported":o.response.as_ref().and_then(|r|r.telemetry.as_ref()).is_some_and(|t|t.usage_reported),"usage":o.response.as_ref().and_then(|r|r.usage.as_ref()).map(|u|json!({"total_tokens":u.total_tokens})),"cost_usd":o.response.as_ref().and_then(|r|r.telemetry.as_ref()).filter(|t|t.usage_reported && t.cost_usd>0.0).map(|t|t.cost_usd)}))).await?;
    let profile_recalled = observations.iter().any(|o| {
        o["sources"].as_array().is_some_and(|sources| {
            sources.iter().any(|s| {
                s["id"] == "owner_profile"
                    && s["text"]
                        .as_str()
                        .is_some_and(|t| t.to_lowercase().contains("vegetarian"))
            })
        })
    });
    let record = runtime
        .store
        .get_connection(&scope.principal, &scope.workspace, &candidate.candidate_id)
        .await?;
    Ok(
        json!({"kind":"persisted_chat_capture_approval_connection","fixture_root":root,"transcript":transcript,
        "capture_prompt_blake3":blake3::hash(system.as_bytes()).to_hex().to_string(),"capture_outcome":capture_outcome,
        "capture_replies":llm.replies.into_inner().unwrap(),"capture_usage":usage,"proposals":proposals,"approvals":approvals,
        "profile_before_approval":before_approval.map(|p|p.injectable),"profile_after_approval":profile.as_ref().map(|p|&p.injectable),
        "structured_recalled_sources":recalled,"structured_profile_recalled":structured_profile_recalled,"connection_observations":observations,"record":record,
        "checks":{"stored_chat_reloaded":messages.len()==1,"proposal_filed":!proposals.is_empty(),"approved":!approvals.is_empty(),"profile_written":profile.is_some(),"approved_preference_recalled":profile_recalled,
        "connection_usage_reported":!observations.is_empty() && observations.iter().all(|o|o["usage_reported"]==true),"connection_delivered":record.as_ref().is_some_and(|r|r.state==ConnectionState::Published)},
        "background_scheduling":"not_exercised_service_driven_explicitly","owner_review":"pending"}),
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    magician_chunking::register_builtin_chunk_adapters()?;
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        args.len() == 2,
        "usage: memory_capture_journey_live_eval CONFIG FRESH_OUTPUT_DIR"
    );
    let root = PathBuf::from(&args[1]);
    ensure!(!root.exists(), "fresh output required");
    let result = run(&args[0], root.clone()).await;
    std::fs::create_dir_all(&root)?;
    let report = match result {
        Ok(v) => v,
        Err(e) => json!({"error":format!("{e:#}"),"inconclusive":true}),
    };
    std::fs::write(
        root.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", root.join("report.json").display());
    ensure!(
        report.get("error").is_none()
            && report["checks"]
                .as_object()
                .is_some_and(|c| c.values().all(|v| v == true)),
        "capture-to-connection journey did not pass; see report"
    );
    Ok(())
}
