use magician::config::{AttentionActionabilityMode, AttentionLearningConfig};
use magician::magician_v2::attention::learning::{
    AttentionLearningService, AttentionLearningStore, AttentionSurface,
    ChannelAttentionSemanticEnvelope, ScheduleSemanticExtraction, SemanticExtractionContract,
    SemanticExtractionWorkStatus, SemanticExtractorIdentity, ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT,
    ATTENTION_SEMANTIC_SCHEMA_VERSION,
};

fn extraction_contract() -> SemanticExtractionContract {
    SemanticExtractionContract {
        semantic_schema_version: ATTENTION_SEMANTIC_SCHEMA_VERSION,
        extractor_contract: ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT.to_string(),
        prompt_version: "1.1.0".to_string(),
        model: Some("gemma4:12b".to_string()),
        profile: Some("op-channel-classify-local".to_string()),
    }
}

fn schedule_request(
    surface: AttentionSurface,
    candidate_id: &str,
    source_revision: &str,
    source_revision_number: i64,
) -> ScheduleSemanticExtraction {
    ScheduleSemanticExtraction {
        surface,
        candidate_id: candidate_id.to_string(),
        source_revision: source_revision.to_string(),
        source_revision_number,
        contract: extraction_contract(),
    }
}

fn valid_semantics() -> serde_json::Value {
    serde_json::json!({
        "communication_type": "newsletter",
        "requested_action": "none",
        "action_owner": "unknown",
        "direct_request_probability": 0.02,
        "broadcast_probability": 0.97,
        "personal_obligation_probability": 0.01,
        "information_value_probability": 0.78,
        "deadline": {"kind": "none", "value": null},
        "campaign_or_event_identity": "synthetic-weekly-edition",
        "evidence_refs": ["safe_brief.information_type", "safe_brief.key_facts"]
    })
}

#[test]
fn semantic_backfill_and_actionability_ship_disabled() {
    let config = AttentionLearningConfig::default();

    assert!(!config.enabled);
    assert!(!config.semantic_backfill.enabled);
    assert_eq!(
        config.actionability.mode,
        AttentionActionabilityMode::Disabled
    );
    assert!(config.actionability.snapshot_id.is_none());
}

#[tokio::test]
async fn explicit_scheduling_is_idempotent_while_the_worker_is_disabled() {
    let directory = tempfile::tempdir().expect("semantic coverage temp directory");
    let service =
        AttentionLearningService::open(directory.path(), AttentionLearningConfig::default())
            .expect("open attention learning service");
    let request = schedule_request(
        AttentionSurface::WorthALook,
        "worth-idempotent-001",
        "feed:revision-a",
        0,
    );

    assert!(!service.semantic_backfill_enabled());
    assert_eq!(service.semantic_backfill_config().enabled, false);
    assert!(service
        .schedule_semantic_extraction("owner-a", "workspace-a", &request, 100)
        .await
        .expect("schedule first exact revision"));
    assert!(!service
        .schedule_semantic_extraction("owner-a", "workspace-a", &request, 101)
        .await
        .expect("repeat exact schedule"));

    let counts = service
        .semantic_extraction_queue_counts("owner-a", "workspace-a")
        .await
        .expect("read semantic queue counts");
    assert_eq!(counts.pending, 1);
    assert_eq!(counts.in_flight, 0);
    assert_eq!(counts.succeeded, 0);
}

#[tokio::test]
async fn leases_never_cross_the_workers_explicit_scope() {
    let directory = tempfile::tempdir().expect("semantic scoped lease temp directory");
    let store =
        AttentionLearningStore::open(directory.path()).expect("open attention learning store");
    let request = schedule_request(
        AttentionSurface::FollowUp,
        "same-candidate-id",
        "distill:1",
        1,
    );
    store
        .schedule_semantic_extraction("owner-a", "workspace-a", &request, 100)
        .await
        .expect("schedule scope A");
    store
        .schedule_semantic_extraction("owner-b", "workspace-b", &request, 101)
        .await
        .expect("schedule scope B");

    let leased = store
        .lease_semantic_extraction_work("owner-a", "workspace-a", "worker-a", 110, 210, 10)
        .await
        .expect("lease only scope A");
    assert_eq!(leased.len(), 1);
    assert_eq!(leased[0].principal, "owner-a");
    assert_eq!(leased[0].workspace, "workspace-a");

    let other_counts = store
        .semantic_extraction_queue_counts("owner-b", "workspace-b")
        .await
        .expect("read untouched scope B");
    assert_eq!(other_counts.pending, 1);
    assert_eq!(other_counts.in_flight, 0);
}

#[tokio::test]
async fn revision_change_invalidates_an_in_flight_completion() {
    let directory = tempfile::tempdir().expect("semantic revision temp directory");
    let store =
        AttentionLearningStore::open(directory.path()).expect("open attention learning store");
    let original = schedule_request(
        AttentionSurface::WorthALook,
        "worth-race-001",
        "event:v1",
        0,
    );
    assert!(store
        .schedule_semantic_extraction("owner-b", "workspace-b", &original, 100)
        .await
        .expect("schedule original revision"));

    let leased = store
        .lease_semantic_extraction_work("owner-b", "workspace-b", "worker-a", 110, 210, 1)
        .await
        .expect("lease original revision");
    assert_eq!(leased.len(), 1);
    assert_eq!(leased[0].source_revision, "event:v1");
    assert_eq!(leased[0].status, SemanticExtractionWorkStatus::InFlight);

    let current = schedule_request(
        AttentionSurface::WorthALook,
        "worth-race-001",
        "event:v2",
        0,
    );
    assert!(store
        .schedule_semantic_extraction("owner-b", "workspace-b", &current, 120)
        .await
        .expect("schedule current revision"));
    assert!(!store
        .finish_semantic_extraction_work(
            &leased[0].work_id,
            "event:v1",
            "worker-a",
            SemanticExtractionWorkStatus::Succeeded,
            None,
            130,
        )
        .await
        .expect("reject stale completion"));

    let replacement = store
        .lease_semantic_extraction_work("owner-b", "workspace-b", "worker-b", 140, 240, 1)
        .await
        .expect("lease replacement revision");
    assert_eq!(replacement.len(), 1);
    assert_eq!(replacement[0].source_revision, "event:v2");
    assert_eq!(replacement[0].lease_owner.as_deref(), Some("worker-b"));
}

#[tokio::test]
async fn expired_lease_cannot_publish_a_completion() {
    let directory = tempfile::tempdir().expect("semantic lease temp directory");
    let store =
        AttentionLearningStore::open(directory.path()).expect("open attention learning store");
    let request = schedule_request(
        AttentionSurface::FollowUp,
        "follow-up-expired-001",
        "distill:12",
        12,
    );
    store
        .schedule_semantic_extraction("owner-expired", "workspace-a", &request, 100)
        .await
        .expect("schedule expiring work");
    let leased = store
        .lease_semantic_extraction_work(
            "owner-expired",
            "workspace-a",
            "worker-expired",
            110,
            120,
            1,
        )
        .await
        .expect("lease expiring work");

    assert!(!store
        .finish_semantic_extraction_work(
            &leased[0].work_id,
            "distill:12",
            "worker-expired",
            SemanticExtractionWorkStatus::Succeeded,
            None,
            121,
        )
        .await
        .expect("reject completion after lease expiry"));

    let replacement = store
        .lease_semantic_extraction_work(
            "owner-expired",
            "workspace-a",
            "worker-current",
            121,
            221,
            1,
        )
        .await
        .expect("recover expired lease");
    assert_eq!(replacement.len(), 1);
    assert_eq!(replacement[0].attempts, 2);
    assert_eq!(
        replacement[0].lease_owner.as_deref(),
        Some("worker-current")
    );
}

#[tokio::test]
async fn slow_inference_can_renew_its_exact_owned_lease_before_commit() {
    let directory = tempfile::tempdir().expect("semantic lease renewal temp directory");
    let store =
        AttentionLearningStore::open(directory.path()).expect("open attention learning store");
    let request = schedule_request(
        AttentionSurface::FollowUp,
        "follow-up-renewed-001",
        "distill:13",
        13,
    );
    store
        .schedule_semantic_extraction("owner-renewed", "workspace-a", &request, 100)
        .await
        .expect("schedule renewable work");
    let leased = store
        .lease_semantic_extraction_work(
            "owner-renewed",
            "workspace-a",
            "worker-renewed",
            110,
            120,
            1,
        )
        .await
        .expect("lease renewable work");

    assert!(store
        .renew_semantic_extraction_work_lease(
            &leased[0].work_id,
            "distill:13",
            "worker-renewed",
            300,
            119,
        )
        .await
        .expect("renew exact owned lease"));
    assert!(!store
        .renew_semantic_extraction_work_lease(
            &leased[0].work_id,
            "distill:13",
            "different-worker",
            301,
            120,
        )
        .await
        .expect("reject lease renewal by another worker"));
    assert!(store
        .finish_semantic_extraction_work(
            &leased[0].work_id,
            "distill:13",
            "worker-renewed",
            SemanticExtractionWorkStatus::Succeeded,
            None,
            200,
        )
        .await
        .expect("commit within renewed lease"));
}

#[tokio::test]
async fn expired_leases_are_recovered_before_untouched_backlog_rows() {
    let directory = tempfile::tempdir().expect("semantic recovery priority temp directory");
    let store =
        AttentionLearningStore::open(directory.path()).expect("open attention learning store");
    let pending = schedule_request(
        AttentionSurface::FollowUp,
        "follow-up-pending-001",
        "distill:20",
        20,
    );
    let expired = schedule_request(
        AttentionSurface::FollowUp,
        "follow-up-expired-priority-001",
        "distill:21",
        21,
    );
    store
        .schedule_semantic_extraction("owner-priority", "workspace-a", &pending, 50)
        .await
        .expect("schedule old pending work");
    store
        .schedule_semantic_extraction("owner-priority", "workspace-a", &expired, 100)
        .await
        .expect("schedule expiring work");
    let first = store
        .lease_semantic_extraction_work(
            "owner-priority",
            "workspace-a",
            "worker-expired",
            110,
            120,
            2,
        )
        .await
        .expect("lease initial work");
    let expired_work = first
        .iter()
        .find(|item| item.candidate_id == "follow-up-expired-priority-001")
        .expect("expiring candidate was leased");
    // Return the unrelated row to retry so the next claim has both an older
    // ready row and an expired lease to choose from.
    let pending_work = first
        .iter()
        .find(|item| item.candidate_id == "follow-up-pending-001")
        .expect("pending candidate was leased");
    assert!(store
        .retry_semantic_extraction_work(
            &pending_work.work_id,
            &pending_work.source_revision,
            "worker-expired",
            Some(121),
            "test_retry",
            115,
        )
        .await
        .expect("return pending candidate to ready backlog"));

    let recovered = store
        .lease_semantic_extraction_work(
            "owner-priority",
            "workspace-a",
            "worker-recovery",
            121,
            221,
            1,
        )
        .await
        .expect("recover one ready row");
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].work_id, expired_work.work_id);
}

#[tokio::test]
async fn retry_state_is_due_time_bound_and_terminal_dead_is_explicit() {
    let directory = tempfile::tempdir().expect("semantic retry temp directory");
    let store =
        AttentionLearningStore::open(directory.path()).expect("open attention learning store");
    let request = schedule_request(
        AttentionSurface::FollowUp,
        "follow-up-retry-001",
        "distill:9",
        9,
    );
    store
        .schedule_semantic_extraction("owner-c", "workspace-c", &request, 100)
        .await
        .expect("schedule retry candidate");
    let first = store
        .lease_semantic_extraction_work("owner-c", "workspace-c", "worker-a", 110, 210, 1)
        .await
        .expect("lease retry candidate");
    assert!(store
        .retry_semantic_extraction_work(
            &first[0].work_id,
            "distill:9",
            "worker-a",
            Some(300),
            "provider_timeout",
            120,
        )
        .await
        .expect("schedule bounded retry"));

    assert!(store
        .lease_semantic_extraction_work("owner-c", "workspace-c", "worker-b", 299, 399, 1)
        .await
        .expect("lease before retry due time")
        .is_empty());
    let retry = store
        .lease_semantic_extraction_work("owner-c", "workspace-c", "worker-b", 300, 400, 1)
        .await
        .expect("lease due retry");
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0].attempts, 2);
    assert!(store
        .retry_semantic_extraction_work(
            &retry[0].work_id,
            "distill:9",
            "worker-b",
            None,
            "retry_budget_exhausted",
            310,
        )
        .await
        .expect("mark exhausted work dead"));

    let counts = store
        .semantic_extraction_queue_counts("owner-c", "workspace-c")
        .await
        .expect("read retry queue counts");
    assert_eq!(counts.retry, 0);
    assert_eq!(counts.dead, 1);
    assert!(counts.next_retry_at.is_none());
}

#[tokio::test]
async fn checkpoints_are_independent_by_scope_and_surface() {
    let directory = tempfile::tempdir().expect("semantic checkpoint temp directory");
    let store =
        AttentionLearningStore::open(directory.path()).expect("open attention learning store");

    store
        .update_semantic_extraction_checkpoint(
            "owner-a",
            "workspace-a",
            AttentionSurface::FollowUp,
            Some("follow-up-cursor-10"),
            100,
        )
        .await
        .expect("write Follow-up checkpoint");
    store
        .update_semantic_extraction_checkpoint(
            "owner-a",
            "workspace-a",
            AttentionSurface::WorthALook,
            Some("worth-cursor-alpha"),
            101,
        )
        .await
        .expect("write Worth checkpoint");
    store
        .update_semantic_extraction_checkpoint(
            "owner-b",
            "workspace-a",
            AttentionSurface::FollowUp,
            Some("other-owner-cursor"),
            102,
        )
        .await
        .expect("write other-scope checkpoint");

    let checkpoints = store
        .semantic_extraction_checkpoints("owner-a", "workspace-a")
        .await
        .expect("read scoped checkpoints");
    assert_eq!(checkpoints.len(), 2);
    assert_eq!(checkpoints[0].surface, AttentionSurface::FollowUp);
    assert_eq!(
        checkpoints[0].cursor.as_deref(),
        Some("follow-up-cursor-10")
    );
    assert_eq!(checkpoints[1].surface, AttentionSurface::WorthALook);
    assert_eq!(checkpoints[1].cursor.as_deref(), Some("worth-cursor-alpha"));
}

#[test]
fn worth_semantics_require_exact_opaque_revision_and_extractor_identity() {
    let identity = SemanticExtractorIdentity {
        model: Some("gemma4:12b".to_string()),
        profile: Some("op-channel-classify-local".to_string()),
    };
    let envelope = ChannelAttentionSemanticEnvelope::from_optional_value_for_source(
        Some(&valid_semantics()),
        "provider-rev:alpha-10",
        0,
        "1.1.0",
        &identity,
    );

    assert!(envelope.is_compatible_with_extractor(
        Some("provider-rev:alpha-10"),
        "1.1.0",
        &identity,
    ));
    assert!(!envelope.is_compatible_with_extractor(
        Some("provider-rev:alpha-9"),
        "1.1.0",
        &identity,
    ));
    assert!(!envelope.is_compatible_with_extractor(Some("distill:10"), "1.1.0", &identity,));

    let migrated_identity = SemanticExtractorIdentity {
        model: Some("gemma4:12b".to_string()),
        profile: Some("op-channel-classify-v2".to_string()),
    };
    assert!(!envelope.is_compatible_with_extractor(
        Some("provider-rev:alpha-10"),
        "1.1.0",
        &migrated_identity,
    ));
}

#[test]
fn frozen_coverage_fixture_keeps_schedule_only_and_revision_cases() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../data/magician_v2/attention_learning/semantic-coverage-frozen-v1.json"
    ))
    .expect("parse frozen semantic coverage fixture");

    assert_eq!(fixture["schema_version"], 1);
    assert_eq!(
        fixture["coverage_contract"]["admin_authority"],
        "schedule_only"
    );
    assert_eq!(fixture["defaults"]["semantic_backfill_enabled"], false);
    assert_eq!(fixture["defaults"]["actionability_mode"], "disabled");

    let case_ids = fixture["cases"]
        .as_array()
        .expect("fixture cases")
        .iter()
        .filter_map(|case| case["id"].as_str())
        .collect::<Vec<_>>();
    for required in [
        "follow_up_resume_after_checkpoint",
        "worth_opaque_revision_exact_match",
        "source_revision_changes_while_leased",
        "extractor_contract_migration_reopens_coverage",
        "repeated_admin_schedule_is_idempotent",
        "disabled_defaults_are_inert",
    ] {
        assert!(
            case_ids.contains(&required),
            "missing fixture case {required}"
        );
    }
}
