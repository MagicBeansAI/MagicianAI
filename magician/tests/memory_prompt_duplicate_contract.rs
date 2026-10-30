//! Rendering keeps the same retained claim and count in both prompt scopes.
use magician::magician_v2::{
    agents::{
        memory_prompt_blocks::{render_memory_tiers_for_prompt_result, MemoryRenderRequest},
        memory_tiers::{
            MemoryTierDefinition, RenderConfig, RetentionMode, TierFieldSchema, TierScope,
        },
        AgentMemoryService,
    },
    artifact_v2::memory::V3MemoryTierRecord,
};
use serde_json::json;
use std::collections::BTreeMap;

#[tokio::test]
async fn borrowed_app_metadata_still_fails_closed_without_source_authority() {
    use magician::magician_v2::apps::{
        memory_bridge::parse_source_eligibility_envelope, models::AppDigest,
    };
    let temp = tempfile::tempdir().unwrap();
    let service = AgentMemoryService::with_scoped_memory_root(temp.path());
    let metadata = [
        json!({"candidate_kind":"tier_field"}),
        json!({"app_source_eligibility":{"invalid":true}}),
        json!({"app_source_eligibility":{
            "schema_version":1, "candidate_id":"memory:candidate:missing",
            "candidate_revision":1, "candidate_fingerprint":AppDigest::blake3(b"fixture"),
            "sources":[]
        }}),
    ];
    assert!(matches!(
        parse_source_eligibility_envelope(&metadata[1]),
        Some(Err(_))
    ));
    assert!(matches!(
        parse_source_eligibility_envelope(&metadata[2]),
        Some(Ok(_))
    ));
    let result = service
        .app_memory_prompt_eligibility(metadata.iter(), chrono::Utc::now())
        .await;
    assert_eq!(result.len(), 1);
    assert_eq!(result.get("memory:candidate:missing"), Some(&false));
    assert!(service
        .app_memory_prompt_eligibility(metadata[..1].iter(), chrono::Utc::now())
        .await
        .is_empty());
}

#[tokio::test]
async fn duplicate_collapse_preserves_winner_count_and_distinct_claim_in_both_scopes() {
    for scope in [TierScope::User, TierScope::Agent] {
        let (tier_name, field_name) = match scope {
            TierScope::User => ("preferences", "preferences"),
            TierScope::Agent => ("decisions", "entries"),
            TierScope::AgentGoal => unreachable!(),
        };
        let temp = tempfile::tempdir().unwrap();
        let service = AgentMemoryService::with_scoped_memory_root(temp.path());
        let tier = MemoryTierDefinition {
            name: tier_name.into(),
            scope: scope.clone(),
            description: "Retrieval contract fixture".into(),
            schema: BTreeMap::from([(
                field_name.into(),
                TierFieldSchema::Collection {
                    max_items: Some(10),
                    item_schema: None,
                },
            )]),
            render: RenderConfig {
                format: "compact_summary".into(),
                template: format!("{{{field_name}}}"),
            },
            retention: RetentionMode::Forever,
        };
        let text = "For audio-editing requests, the user prefers a finished, usable artifact over advice or a substitute, with requested export formats, technical validation, and concise edit notes.";
        let reworded = "For audio-editing requests, the user prefers finished, usable artifacts rather than advice or a substitute, with requested export formats, technical validation, and concise edit notes.";
        let mut record = V3MemoryTierRecord::new(tier_name, scope.clone(), None, None, None, None);
        record.fields.insert(field_name.into(), json!([
            {"key":"audio_editing_deliverable_preference","value":text,
             "last_seen":"2026-10-02T00:00:00Z","memory_evidence":[{"id":"e1"}]},
            {"key":"audio_editing_deliverables","value":reworded,
             "last_seen":"2026-10-05T00:00:00Z","memory_evidence":[{"id":"e2"},{"id":"e3"}]},
            {"key":"audio_editing_artifact_preference","value":reworded,
             "last_seen":"2026-10-09T00:00:00Z","memory_evidence":[{"id":"e4"}]},
            {"key":"weekly_report_format","value":"Weekly audio reports use concise markdown summaries with headings."}
        ]));
        service
            .save_native_tier("retrieval-contract-agent", &tier, None, &record)
            .await
            .unwrap();
        let query = "audio editing artifact export formats";
        let mut request = match scope {
            TierScope::User => MemoryRenderRequest::user(query),
            TierScope::Agent => MemoryRenderRequest::agent(query),
            TierScope::AgentGoal => unreachable!(),
        }
        .with_emit_audit(false);
        request.judge_preferences = false;
        let rendered = render_memory_tiers_for_prompt_result(
            &service,
            "retrieval-contract-agent",
            &[tier],
            &request,
        )
        .await
        .unwrap();
        let section = rendered
            .section
            .unwrap_or_else(|| panic!("{scope:?}: tier renders"));
        assert_eq!(
            section.matches("For audio-editing requests").count(),
            1,
            "{scope:?}: {section}"
        );
        assert!(
            section.contains("(2 similar) audio_editing_deliverables (as of 2026-10-05):"),
            "{scope:?}: {section}"
        );
        assert!(
            section.contains("weekly_report_format"),
            "{scope:?}: {section}"
        );
        assert_eq!(rendered.selected_candidate_keys.len(), 2);
    }
}
