//! A natural utterance enters the normal consolidation rule executor, then the
//! shared lifecycle and actual prompt renderer. No structured expected memory
//! is injected. Background scheduling and browser input are separate boundaries.
use super::*;
use magician::magician_v2::{
    agents::{
        memory_consolidator::MemoryConsolidator,
        memory_tiers::{
            ConsolidationTransform, ConsolidationTrigger, MemoryConsolidationOperation,
            MemoryConsolidationRule,
        },
        AgentDefinition,
    },
    attention::resurfacing::memory_connections::recall,
    chat::{
        models::*,
        storage::{ChatStore, FileChatStore},
    },
    realtime_events::RuntimeTransportEvent,
};

pub async fn run(root: &Path, router: &OperationLlmRouter, fixture: &Value) -> Result<Value> {
    fs::create_dir(root)?;
    let layout = ArtifactV2Workspace::new(root);
    let resolver = AgentMemoryResolver::with_workspace_layout(layout.clone());
    let service = resolver.resolve_for_scope("capture-owner", "capture-workspace")?;
    let chat = FileChatStore::with_workspace_layout_index(layout.clone()).await?;
    let session = chat
        .new_session(
            "capture-owner",
            "capture-workspace",
            "general",
            &ChatChannel::web(),
            "memory-eval",
        )
        .await?;
    let mut definition: AgentDefinition = serde_json::from_value(json!({
        "agent_id":"memory-eval","name":"Memory journey","persona":"Capture durable owner memories","tools":[]
    }))?;
    definition.memory_consolidation=vec![MemoryConsolidationRule {
        name:"capture_owner_memory".into(),trigger:ConsolidationTrigger::StepCompleted,
        source:"step_result".into(),target:"user.preferences".into(),
        transform:ConsolidationTransform::Llm {
            prompt:"Extract durable owner memory from the supplied finished conversation turn. Return a JSON array of objects with key, value, source_type. Use explicit_user_statement only for the owner's own statement. Preserve correction language, conditions and negation. Exclude assistant claims. Keep each distinct assertion intact and use concise meaningful keys. Do not invent lifecycle decisions or evidence IDs. Source: {source_json}".into(),
            operation:Some(MemoryConsolidationOperation::MemoryUserPromotion),system_prompt:None,merge:None,
        },
    }];
    let events = Arc::new(RuntimeTransportBroadcaster::new(64));
    let mut receiver = events.subscribe();
    let consolidator =
        MemoryConsolidator::new(service.clone(), Some(Arc::new(router.clone())), None)
            .with_llm_telemetry_broadcaster(events);
    let mut stages = Vec::new();
    for (index, words) in fixture["utterances"]
        .as_array()
        .context("capture utterances")?
        .iter()
        .enumerate()
    {
        let words = words.as_str().context("capture words")?;
        let message_id = format!("owner-statement-{index}");
        chat.append_message(
            &session.id,
            ChatMessage::new(
                &message_id,
                &session.id,
                ChatMessageDirection::User,
                ChatMessageContent::Text {
                    text: words.into(),
                    plan_reply: None,
                },
                chrono::Utc::now().timestamp_millis(),
            ),
        )
        .await?;
        let reopened = FileChatStore::with_workspace_layout_index(layout.clone()).await?;
        let stored = reopened.get_messages(&session.id, 20).await?;
        let message = stored
            .iter()
            .find(|m| m.id == message_id)
            .context("persisted owner message missing")?;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            consolidator.consolidate_step_completed(
                &definition,
                "memory-eval",
                &session.id,
                &message_id,
                &json!({"conversation_turn":message}),
            ),
        )
        .await??;
        let proposed = service.load_user_knowledge().await?;
        let mut observations = Vec::new();
        let review = runtime::pass(
            &service,
            Some(router),
            None,
            chrono::Utc::now(),
            Some(&mut |o| observations.push(observe(o))),
        )
        .await?;
        let stage_recall = recall(&service, fixture["recall_query"].as_str().unwrap()).await?;
        stages.push(
            json!({"utterance":words,"consolidation":format!("{result:?}"),
            "proposed":proposed,"review":review,"observations":observations,
            "document":service.load_user_knowledge().await?,"recall":stage_recall}),
        );
        fs::write(
            root.join("capture-stages.json"),
            serde_json::to_vec_pretty(&stages)?,
        )?;
    }
    let document = service.load_user_knowledge().await?;
    let current: Vec<_> = lifecycle::sources(&document)
        .into_iter()
        .filter(|s| lifecycle::state(&s.item) == "active")
        .collect();
    let old = fixture["earlier_current"].as_str().unwrap();
    let new = fixture["final_current"].as_str().unwrap();
    let earlier_was_current = stages.first().is_some_and(|stage| {
        lifecycle::sources(&stage["document"]).iter().any(|s| {
            lifecycle::state(&s.item) == "active" && lifecycle::text(&s.item).contains(old)
        })
    });
    let retired = document
        .as_object()
        .into_iter()
        .flat_map(|tiers| tiers.values())
        .filter_map(Value::as_array)
        .flatten()
        .any(|s| {
            lifecycle::retired(s)
                && lifecycle::text(s).contains(old)
                && !lifecycle::text(s).contains(new)
        });
    let current_only = current.len() == 1 && lifecycle::text(&current[0].item).contains(new);
    let recalled = recall(&service, fixture["recall_query"].as_str().unwrap()).await?;
    let current_recalled = recalled.iter().any(|s| s.text.contains(new));
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
            usage.push(json!({"provider":provider,"model":model,"reported":usage_reported,
                "cost_usd":if usage_reported {Some(cost)}else{None},"input_tokens":input_tokens,"output_tokens":output_tokens}));
        }
    }
    let checks = json!({"earlier_was_current":earlier_was_current,"old_revision_retired":retired,
        "one_current_revision":current_only,"current_recalled":current_recalled,
        "all_reviews_applied":stages.iter().all(|s|s["review"]["applied"]==1 && s["review"]["error"].is_null())});
    Ok(
        json!({"passed":checks.as_object().unwrap().values().all(|v|v==true),"checks":checks,
        "fixture":fixture,"stages":stages,"recalled":recalled,"promotion_usage":usage,
        "boundary":"persisted natural chat turn -> production rule executor -> lifecycle -> production recall; scheduler explicitly driven"}),
    )
}
