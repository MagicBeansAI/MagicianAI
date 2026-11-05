use magician::magician_v2::{
    agents::{
        memory_prompt_snapshot::{
            load_memory_prompt_candidate_snapshot, load_memory_recall_candidate_documents,
            memory_prompt_snapshot_cache_stats,
        },
        memory_tiers::{
            MemoryTierDefinition, RenderConfig, RetentionMode, TierFieldSchema, TierScope,
        },
        AgentDefinition, AgentMemoryService, MemoryCandidateRequest, RetrievalScope,
    },
    artifact_v2::memory::V3MemoryTierRecord,
    chat::service::rank_memory_candidates_hybrid,
};
use serde_json::json;
use std::collections::BTreeMap;

static CACHE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn goal_recall_keeps_one_snapshot_and_observes_added_and_edited_goals() {
    let _cache_guard = CACHE_TEST_LOCK.lock().await;
    let temp = tempfile::tempdir().unwrap();
    let service = AgentMemoryService::with_scoped_memory_root(temp.path());
    let mut definition = AgentDefinition::from_yaml_str(
        "agent_id: goal-recall-contract\nname: Goal recall\nkind: worker\n\
         description: Retrieval fixture\npersona: Records shipment decisions\n",
    )
    .unwrap();
    definition.memory_tiers = vec![MemoryTierDefinition {
        name: "decisions".into(),
        scope: TierScope::AgentGoal,
        description: "Goal decisions".into(),
        schema: BTreeMap::from([(
            "entries".into(),
            TierFieldSchema::Collection {
                max_items: Some(10),
                item_schema: None,
            },
        )]),
        render: RenderConfig {
            format: "compact_summary".into(),
            template: "{entries}".into(),
        },
        retention: RetentionMode::Forever,
    }];
    let tier = &definition.memory_tiers[0];
    let mut record =
        V3MemoryTierRecord::new("decisions", TierScope::AgentGoal, None, None, None, None);
    for n in 1..=12 {
        record.goal_id = Some(format!("goal-{n}"));
        record.fields.insert(
            "entries".into(),
            json!([{ "key": "route", "value": format!("Shipment route {n}") }]),
        );
        service
            .save_native_tier(
                &definition.agent_id,
                tier,
                Some(&format!("goal-{n}")),
                &record,
            )
            .await
            .unwrap();
    }
    let request = MemoryCandidateRequest {
        scope: TierScope::AgentGoal,
        goal_id: None,
        recency_cutoff: None,
        include_environment_knowledge: true,
        retrieval_scope: RetrievalScope::Unbound,
        isolated_user_root: None,
    };
    let first = load_memory_recall_candidate_documents(
        &service,
        &definition.agent_id,
        &definition.memory_tiers,
        &request,
    )
    .await
    .unwrap();
    assert_eq!(first.len(), 12);
    // A search also reads the agent scope (episodes, environment knowledge),
    // a separate snapshot whose first build is not part of the goal scan.
    load_memory_recall_candidate_documents(
        &service,
        &definition.agent_id,
        &definition.memory_tiers,
        &MemoryCandidateRequest {
            scope: TierScope::Agent,
            ..request.clone()
        },
    )
    .await
    .unwrap();
    let before_rank = memory_prompt_snapshot_cache_stats();
    let (ranked, meta) = rank_memory_candidates_hybrid(
        None,
        "fixture",
        "default",
        &definition.agent_id,
        &definition,
        &service,
        "shipment",
        Some("decisions"),
        None,
        &RetrievalScope::Unbound,
    )
    .await;
    assert!(!meta.source_read_failed);
    assert_eq!(ranked.len(), 12);
    let reused = load_memory_recall_candidate_documents(
        &service,
        &definition.agent_id,
        &definition.memory_tiers,
        &request,
    )
    .await
    .unwrap();
    assert_eq!(reused.len(), 12);
    let after_rank = memory_prompt_snapshot_cache_stats();
    assert_eq!(
        after_rank.builds, before_rank.builds,
        "a goal scan must not build per-goal entries or evict the aggregate snapshot"
    );
    assert!(after_rank.hits > before_rank.hits);
    record.goal_id = Some("goal-13".into());
    service
        .save_native_tier(&definition.agent_id, tier, Some("goal-13"), &record)
        .await
        .unwrap();
    let added = load_memory_recall_candidate_documents(
        &service,
        &definition.agent_id,
        &definition.memory_tiers,
        &request,
    )
    .await
    .unwrap();
    assert_eq!(added.len(), 13);
    record.fields.insert(
        "entries".into(),
        json!([{ "key": "route", "value": "Shipment UPDATED route" }]),
    );
    record.goal_id = Some("goal-1".into());
    service
        .save_native_tier(&definition.agent_id, tier, Some("goal-1"), &record)
        .await
        .unwrap();
    let edited = load_memory_recall_candidate_documents(
        &service,
        &definition.agent_id,
        &definition.memory_tiers,
        &request,
    )
    .await
    .unwrap();
    assert_eq!(edited.len(), 13);
    assert!(edited
        .iter()
        .any(|document| document.text.contains("UPDATED")));
}

#[tokio::test]
async fn recall_keeps_engagements_separate_and_observes_canonical_edits() {
    let _cache_guard = CACHE_TEST_LOCK.lock().await;
    let temp = tempfile::tempdir().unwrap();
    let service = AgentMemoryService::with_scoped_memory_root(temp.path());
    let mut definition = AgentDefinition::from_yaml_str(
        "agent_id: recall-contract\nname: Recall contract\nkind: worker\n\
         description: Retrieval fixture\npersona: Records shipment decisions\n",
    )
    .unwrap();
    definition.memory_tiers = vec![MemoryTierDefinition {
        name: "decisions".into(),
        scope: TierScope::Agent,
        description: "Decisions".into(),
        schema: BTreeMap::from([(
            "entries".into(),
            TierFieldSchema::Collection {
                max_items: Some(10),
                item_schema: None,
            },
        )]),
        render: RenderConfig {
            format: "compact_summary".into(),
            template: "{entries}".into(),
        },
        retention: RetentionMode::Forever,
    }];
    let tier = &definition.memory_tiers[0];
    let cache_before = memory_prompt_snapshot_cache_stats();
    let mut record = V3MemoryTierRecord::new("decisions", TierScope::Agent, None, None, None, None);
    record.fields.insert("entries".into(), json!([
        {"key":"alpha", "value":"Shipment alpha uses the northern route", "engagement_scope":"engagement:alpha"},
        {"key":"beta", "value":"Shipment beta uses the southern route", "engagement_scope":"engagement:beta"},
        {"key":"neutral", "value":"Shipment tracking needs a dispatch receipt", "engagement_scope":"neutral"},
        {"key":"private", "value":"Shipment private owner notes are unlabelled"}
    ]));
    service
        .save_native_tier(&definition.agent_id, tier, None, &record)
        .await
        .unwrap();
    let request = MemoryCandidateRequest {
        scope: TierScope::Agent,
        goal_id: None,
        recency_cutoff: None,
        include_environment_knowledge: true,
        retrieval_scope: RetrievalScope::Unbound,
        isolated_user_root: None,
    };
    let documents = load_memory_recall_candidate_documents(
        &service,
        &definition.agent_id,
        &definition.memory_tiers,
        &request,
    )
    .await
    .unwrap();
    let prompt = load_memory_prompt_candidate_snapshot(
        &service,
        &definition.agent_id,
        &definition.memory_tiers,
        &request,
    )
    .await
    .unwrap();
    assert_eq!(documents.len(), 4);
    assert_eq!(prompt.documents.len(), documents.len());
    assert_eq!(
        prompt.profiles.len(),
        documents.len(),
        "document-only cache entries must never be served to the prompt renderer"
    );
    assert!(documents
        .iter()
        .zip(prompt.documents.iter())
        .all(|(a, b)| a.text == b.text));
    for (scope, expected) in [
        (
            RetrievalScope::bound("alpha").unwrap(),
            vec!["key:alpha", "key:neutral"],
        ),
        (
            RetrievalScope::bound("beta").unwrap(),
            vec!["key:beta", "key:neutral"],
        ),
        (
            RetrievalScope::Unbound,
            vec!["key:alpha", "key:beta", "key:neutral", "key:private"],
        ),
    ] {
        let (ranked, meta) = rank_memory_candidates_hybrid(
            None,
            "fixture",
            "default",
            &definition.agent_id,
            &definition,
            &service,
            "shipment",
            Some("decisions"),
            None,
            &scope,
        )
        .await;
        assert!(!meta.source_read_failed);
        let mut keys: Vec<_> = ranked
            .iter()
            .map(|row| row.candidate.item_key.as_str())
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, expected, "{scope:?}");
    }
    assert!(memory_prompt_snapshot_cache_stats().hits > cache_before.hits);
    record.fields.insert("entries".into(), json!([
        {"key":"beta", "value":"Shipment UPDATED beta uses the eastern route", "engagement_scope":"engagement:beta"}
    ]));
    service
        .save_native_tier(&definition.agent_id, tier, None, &record)
        .await
        .unwrap();
    let (ranked, meta) = rank_memory_candidates_hybrid(
        None,
        "fixture",
        "default",
        &definition.agent_id,
        &definition,
        &service,
        "shipment",
        Some("decisions"),
        None,
        &RetrievalScope::bound("beta").unwrap(),
    )
    .await;
    assert!(!meta.source_read_failed);
    assert_eq!(ranked.len(), 1);
    assert!(ranked[0].candidate.text.contains("UPDATED"));
}
