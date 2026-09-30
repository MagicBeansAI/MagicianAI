//! Product wire-fixture oracles (plan workstream 0.3, corpus 2).
//!
//! Pins the wire contracts the first migrations depend on, exactly as
//! they behave today:
//!
//! * the Tutor/Copilot screen-draw storyboard payload contract — the
//!   `shape_json` grammar the LLM emits and all four client interpreters
//!   render (`validate_tutor_draw_storyboard_payload`),
//! * the Live Thinking Map operation wire contract — the serde forms the
//!   web/iOS/Android clients emit and parse (`MapOperation`,
//!   `MapOperationEnvelope`),
//! * the Channel Assist annotation transition vocabulary, audit-event
//!   wire shapes, and the `export-fixtures` row contract
//!   (`magician-comms` channel_assist types/fixtures),
//! * the recurring Monitor `MonitorSpecV1` / cadence / diff-policy wire
//!   forms, and
//! * the `tutor.*` event ids, taxonomy rows, and payload vocabulary the
//!   4.1 Tutor/Copilot split must not drift.
//!
//! Behavior-pinning only: if one of these assertions must change, the wire
//! contract changed, and the plan's additive-only rule (principle 3)
//! requires that to be a deliberate, client-coordinated event.

use magician::magician_v2::thinking_map_models::THINKING_MAP_SCHEMA_VERSION;
use magician::magician_v2::thinking_map_operations::{MapOperation, MapOperationEnvelope};
use magician::magician_v2::tutor::validate_tutor_draw_storyboard_payload;
use serde_json::json;

use magician::magician_v2::monitors::monitor_run::{
    would_notify, MonitorFindingClassification, MonitorRunStatus,
};
use magician::magician_v2::monitors::monitor_spec::{
    MonitorMatchMode, MonitorNotificationPolicy, MonitorSources, MonitorSpecV1,
    MONITOR_SPEC_SCHEMA_VERSION,
};
use magician::magician_v2::pipeline::agent::{ConcurrentExecutionPolicy, MissedFirePolicy};
use magician::magician_v2::storage::{TaskSchedule, TaskScheduleKind};
use magician::magician_v2::tutor::copilot_rail::TutorProductRail;
use magician::magician_v2::tutor::{
    TutorCanvasMode, TutorDrawStoryboardStep, TutorRun, TutorRunMode, TutorRunStatus,
    TutorSafetyLevel, TutorStep, TutorStepKind, TutorStepRecord, TutorStepStatus,
};
use magician_api::channel_assist_api::{ApproveBody, DismissBody, FeedbackBody};
use magician_comms::channel_assist::assist::fixtures::{
    fixture_row, render_jsonl, short_hash, MailFixtureRow, FIXTURE_HASH_LEN,
};
use magician_comms::channel_assist::types::{
    ChannelLane, MailAnnotationState, MailAssistActor, MailAssistEvent, MailAssistEventType,
    MailFeedbackVerdict, MailRecordOrigin, MailThreadAnnotation, MailThreadRecord,
    MessageDirection, MAIL_ASSIST_SCHEMA_VERSION, REDACTED_SUBJECT_PLACEHOLDER,
};
use magician_event_taxonomy::{
    lookup_agent_event_taxonomy, EventCategory, EventSeverity, RenderHint, RuntimeAgentEventType,
};

// --- Tutor screen-draw storyboard contract ----------------------------------

#[test]
fn storyboard_leaf_with_label_and_narration_is_a_figure_step() {
    let payload = json!({
        "type": "arrow",
        "tutor_step_label": "point at save",
        "narration": "click save after typing",
    });
    let steps = validate_tutor_draw_storyboard_payload(&payload).expect("valid leaf");
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].label, "point at save");
    assert_eq!(steps[0].narration, "click save after typing");
    assert!(steps[0].figure_backed, "arrow draws a figure");
    assert!(
        !steps[0].step_id.is_empty(),
        "step id is derived from the label"
    );
}

#[test]
fn storyboard_text_primitives_are_not_figure_backed() {
    let payload = json!({
        "type": "label",
        "tutor_step_label": "name the field",
        "narration": "this is the email field",
    });
    let steps = validate_tutor_draw_storyboard_payload(&payload).expect("valid leaf");
    assert_eq!(steps.len(), 1);
    assert!(!steps[0].figure_backed, "label is prose, not a figure");
}

#[test]
fn storyboard_groups_inherit_label_and_narration_to_children() {
    let payload = json!({
        "type": "group",
        "tutor_step_label": "walk through the save flow",
        "narration": "first the button, then the dialog",
        "shapes": [
            {"type": "rect"},
            {"type": "callout", "tutor_step_label": "override inside the group"},
        ],
    });
    let steps = validate_tutor_draw_storyboard_payload(&payload).expect("valid group");
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].label, "walk through the save flow");
    assert!(steps[0].figure_backed, "rect is a figure");
    assert_eq!(steps[1].label, "override inside the group");
}

#[test]
fn storyboard_group_without_children_is_rejected() {
    let payload = json!({"type": "group", "tutor_step_label": "l", "narration": "n"});
    let error = validate_tutor_draw_storyboard_payload(&payload).expect_err("group needs shapes");
    assert!(
        error.contains("draw group requires `shapes` or `children`"),
        "unexpected error: {error}"
    );
}

#[test]
fn storyboard_leaf_requires_label_then_narration() {
    let missing_label = json!({"type": "arrow", "narration": "n"});
    let error = validate_tutor_draw_storyboard_payload(&missing_label).expect_err("label required");
    assert!(
        error.contains("requires `tutor_step_label` or `step_label`"),
        "unexpected error: {error}"
    );

    let missing_narration = json!({"type": "arrow", "tutor_step_label": "l"});
    let error =
        validate_tutor_draw_storyboard_payload(&missing_narration).expect_err("narration required");
    assert!(
        error.contains("requires `narration`"),
        "unexpected error: {error}"
    );
}

#[test]
fn storyboard_clear_yields_no_steps() {
    let payload = json!({"type": "clear"});
    let steps = validate_tutor_draw_storyboard_payload(&payload).expect("clear is accepted");
    assert!(steps.is_empty());
}

#[test]
fn storyboard_duplicate_steps_merge_their_figure_flags() {
    // A group whose children share one step identity but mix prose and
    // figure primitives: coverage must be decided by the union, not by
    // child order.
    let payload = json!({
        "type": "group",
        "tutor_step_label": "mixed step",
        "narration": "same narration",
        "shapes": [
            {"type": "formula", "storyboard_step_id": "s1"},
            {"type": "path", "storyboard_step_id": "s1"},
        ],
    });
    let steps = validate_tutor_draw_storyboard_payload(&payload).expect("valid group");
    assert_eq!(steps.len(), 1, "same-id steps dedupe");
    assert_eq!(steps[0].step_id, "s1");
    assert!(
        steps[0].figure_backed,
        "the path leaf makes the step figure-backed"
    );
}

// --- Live Thinking Map operation wire contract -------------------------------

#[test]
fn map_operation_wire_tags_are_pinned() {
    // Internally tagged on `op`, snake_case — the exact wire form four
    // clients construct and parse.
    let tombstone: MapOperation =
        serde_json::from_value(json!({"op": "tombstone_node", "node_id": "n1"}))
            .expect("tombstone wire form");
    assert_eq!(
        serde_json::to_value(&tombstone).expect("serializes"),
        json!({"op": "tombstone_node", "node_id": "n1"}),
        "round-trip must be byte-identical for the minimal form"
    );

    let moved: MapOperation = serde_json::from_value(json!({
        "op": "move_node",
        "node_id": "n1",
        "position": {"x": 1.5, "y": 2.0},
    }))
    .expect("move wire form");
    assert_eq!(
        serde_json::to_value(&moved).expect("serializes"),
        json!({"op": "move_node", "node_id": "n1", "position": {"x": 1.5, "y": 2.0}}),
        "round-trip must preserve the position object"
    );
}

#[test]
fn map_operation_envelope_wire_form_is_pinned() {
    let envelope: MapOperationEnvelope = serde_json::from_value(json!({
        "schema_version": 1,
        "envelope_id": "e1",
        "map_id": "m1",
        "base_revision": 0,
        "actor": {"actor": "owner", "principal": "p1"},
        "idempotency_key": "k1",
        "operations": [{"op": "tombstone_node", "node_id": "n1"}],
        "created_at": "2026-08-26T00:00:00Z",
    }))
    .expect("envelope wire form");
    assert_eq!(envelope.schema_version, 1);
    assert_eq!(envelope.map_id, "m1");
    assert_eq!(envelope.base_revision, 0);
    assert_eq!(envelope.idempotency_key, "k1");
    assert_eq!(envelope.operations.len(), 1);
    assert!(matches!(
        envelope.operations[0],
        MapOperation::TombstoneNode { ref node_id } if node_id == "n1"
    ));

    let serialized = serde_json::to_value(&envelope).expect("serializes");
    assert_eq!(
        serialized["actor"]["actor"], "owner",
        "actor tag is snake_case"
    );
    assert_eq!(
        serialized["operations"][0]["op"], "tombstone_node",
        "operation tag is snake_case"
    );
}

#[test]
fn thinking_map_schema_version_is_pinned() {
    // Bumping this is a coordinated wire event across four clients, never
    // a drive-by.
    assert_eq!(THINKING_MAP_SCHEMA_VERSION, 1);
}

// --- Channel Assist transitions + fixture export (plan 0.3 corpus 2) --------
//
// Visibility note (characterization scope): the store's runtime guards
// (`transition_annotation*`, classification dispositions) are async and
// DB-bound inside `magician-comms`, so this corpus pins the
// pub-reachable transition VOCABULARY and its wire forms: the
// state/actor/event-kind enums, the serde shapes of the audit event and
// annotation record they serialize into, and the `channel-assist
// export-fixtures` row contract. The HTTP request bodies
// (`DismissBody`/`ApproveBody`/`FeedbackBody`) are Deserialize-only
// wrappers in `magician-api` and are pinned directly here since
// `magician-api` is a dev-dependency of this crate.

#[test]
fn channel_assist_annotation_state_vocabulary_is_pinned() {
    // The full 15-state lifecycle, happy path and side exits, in the
    // order the enum declares them. Serde token, db string, and the
    // parser must all agree on one snake_case vocabulary.
    let states = [
        MailAnnotationState::Observed,
        MailAnnotationState::Classified,
        MailAnnotationState::NeedsApproval,
        MailAnnotationState::Approved,
        MailAnnotationState::Acknowledged,
        MailAnnotationState::Scheduled,
        MailAnnotationState::DraftRequested,
        MailAnnotationState::DraftReady,
        MailAnnotationState::Inserted,
        MailAnnotationState::SentDetected,
        MailAnnotationState::Completed,
        MailAnnotationState::Dismissed,
        MailAnnotationState::Stale,
        MailAnnotationState::Superseded,
        MailAnnotationState::Errored,
    ];
    let tokens = [
        "observed",
        "classified",
        "needs_approval",
        "approved",
        "acknowledged",
        "scheduled",
        "draft_requested",
        "draft_ready",
        "inserted",
        "sent_detected",
        "completed",
        "dismissed",
        "stale",
        "superseded",
        "errored",
    ];
    assert_eq!(states.len(), 15);
    for (state, token) in states.iter().zip(tokens) {
        assert_eq!(state.as_db_str(), token, "db string for {state:?}");
        assert_eq!(
            serde_json::to_value(state).expect("state serializes"),
            json!(token),
            "serde token for {state:?}"
        );
        assert_eq!(
            MailAnnotationState::from_db_str(token).expect("db string parses"),
            *state
        );
        let wire: MailAnnotationState =
            serde_json::from_value(json!(token)).expect("serde token parses");
        assert_eq!(wire, *state);
    }
    assert!(MailAnnotationState::from_db_str("bogus").is_err());
    let unknown: Result<MailAnnotationState, _> = serde_json::from_value(json!("bogus"));
    assert!(unknown.is_err(), "unknown serde token is rejected");
}

#[test]
fn channel_assist_audit_event_kinds_and_actors_are_pinned() {
    let kinds = [
        MailAssistEventType::AnnotationCreated,
        MailAssistEventType::AnnotationUpdated,
        MailAssistEventType::StateTransition,
        MailAssistEventType::Dismissed,
        MailAssistEventType::Feedback,
        MailAssistEventType::ProviderChange,
    ];
    let tokens = [
        "annotation_created",
        "annotation_updated",
        "state_transition",
        "dismissed",
        "feedback",
        "provider_change",
    ];
    for (kind, token) in kinds.iter().zip(tokens) {
        assert_eq!(kind.as_db_str(), token);
        assert_eq!(
            serde_json::to_value(kind).expect("serializes"),
            json!(token)
        );
        assert_eq!(
            MailAssistEventType::from_db_str(token).expect("parses"),
            *kind
        );
    }
    assert!(MailAssistEventType::from_db_str("bogus").is_err());

    for (actor, token) in [
        (MailAssistActor::User, "user"),
        (MailAssistActor::Worker, "worker"),
    ] {
        assert_eq!(actor.as_db_str(), token);
        assert_eq!(
            serde_json::to_value(actor).expect("serializes"),
            json!(token)
        );
        assert_eq!(MailAssistActor::from_db_str(token).expect("parses"), actor);
    }

    // Supporting row enums ride on the same records: lane, direction,
    // provenance. Serde tokens only — these have no db-string parser.
    assert_eq!(
        serde_json::to_value(ChannelLane::UserAssist).unwrap(),
        json!("user_assist")
    );
    assert_eq!(
        serde_json::to_value(ChannelLane::Envoy).unwrap(),
        json!("envoy")
    );
    assert_eq!(
        serde_json::to_value(MessageDirection::Inbound).unwrap(),
        json!("inbound")
    );
    assert_eq!(
        serde_json::to_value(MessageDirection::Outbound).unwrap(),
        json!("outbound")
    );
    assert_eq!(
        serde_json::to_value(MailRecordOrigin::MetadataSync).unwrap(),
        json!("metadata_sync")
    );
    assert_eq!(
        serde_json::to_value(MailRecordOrigin::Seed).unwrap(),
        json!("seed")
    );
}

#[test]
fn channel_assist_transition_event_wire_shape_is_pinned() {
    // A `needs_approval → approved` owner transition is appended as a
    // `state_transition` audit event carrying both endpoints; optional
    // fields are absent, not null, on the wire.
    let event = MailAssistEvent {
        schema_version: MAIL_ASSIST_SCHEMA_VERSION,
        id: "evt-1".to_string(),
        annotation_id: Some("ann-1".to_string()),
        provider: "gmail".to_string(),
        account_alias: "acct-a".to_string(),
        thread_id: Some("t-1".to_string()),
        event_type: MailAssistEventType::StateTransition,
        actor: MailAssistActor::User,
        from_state: Some(MailAnnotationState::NeedsApproval),
        to_state: Some(MailAnnotationState::Approved),
        detail: None,
        created_at: 7,
    };
    assert_eq!(
        serde_json::to_value(&event).expect("serializes"),
        json!({
            "schema_version": 9,
            "id": "evt-1",
            "annotation_id": "ann-1",
            "provider": "gmail",
            "account_alias": "acct-a",
            "thread_id": "t-1",
            "event_type": "state_transition",
            "actor": "user",
            "from_state": "needs_approval",
            "to_state": "approved",
            "created_at": 7,
        })
    );

    // Dismissal is a state transition that gets its OWN event kind so the
    // audit trail reads cleanly — the endpoints still ride along.
    let dismissed = MailAssistEvent {
        event_type: MailAssistEventType::Dismissed,
        from_state: Some(MailAnnotationState::Classified),
        to_state: Some(MailAnnotationState::Dismissed),
        ..event.clone()
    };
    let wire = serde_json::to_value(&dismissed).expect("serializes");
    assert_eq!(wire["event_type"], "dismissed");
    assert_eq!(wire["from_state"], "classified");
    assert_eq!(wire["to_state"], "dismissed");

    // Legacy/compat direction: rows persisted before a schema bump decode
    // at the live version, and the optionals default to absent.
    let minimal: MailAssistEvent = serde_json::from_value(json!({
        "id": "evt-2",
        "provider": "gmail",
        "account_alias": "acct-a",
        "event_type": "feedback",
        "actor": "worker",
        "created_at": 9,
    }))
    .expect("minimal audit event decodes");
    assert_eq!(minimal.schema_version, MAIL_ASSIST_SCHEMA_VERSION);
    assert!(minimal.annotation_id.is_none());
    assert!(minimal.from_state.is_none());
    assert!(minimal.to_state.is_none());
}

#[test]
fn channel_assist_annotation_record_wire_shape_is_pinned() {
    let annotation = MailThreadAnnotation {
        schema_version: MAIL_ASSIST_SCHEMA_VERSION,
        id: "ann-1".to_string(),
        provider: "gmail".to_string(),
        account_alias: "acct-a".to_string(),
        thread_id: "t-1".to_string(),
        lane: ChannelLane::UserAssist,
        state: MailAnnotationState::NeedsApproval,
        label: None,
        confidence: None,
        reason: None,
        evidence_refs: Vec::new(),
        evidence_message_id: None,
        evidence_message_at: None,
        classification_input_revision: None,
        semantic_features: None,
        proposed_action: None,
        provenance: None,
        created_at: 1,
        updated_at: 2,
    };
    let wire = serde_json::to_value(&annotation).expect("serializes");
    assert_eq!(
        wire,
        json!({
            "schema_version": 9,
            "id": "ann-1",
            "provider": "gmail",
            "account_alias": "acct-a",
            "thread_id": "t-1",
            "lane": "user_assist",
            "state": "needs_approval",
            "evidence_refs": [],
            "created_at": 1,
            "updated_at": 2,
        })
    );
    let back: MailThreadAnnotation =
        serde_json::from_value(wire).expect("annotation record round-trips");
    assert_eq!(back, annotation);
}

#[test]
fn channel_assist_transition_request_bodies_are_pinned() {
    // The POST bodies of the transition endpoints. `dismiss` and
    // `approve` are fully optional (an empty body is legal); `feedback`
    // requires the verdict.
    let dismiss: DismissBody =
        serde_json::from_value(json!({})).expect("empty dismiss body is legal");
    assert!(dismiss.reason.is_none());
    assert!(dismiss.event_id.is_none());

    let dismiss: DismissBody =
        serde_json::from_value(json!({"reason": "spam"})).expect("dismiss reason decodes");
    assert_eq!(dismiss.reason.as_deref(), Some("spam"));

    let approve: ApproveBody = serde_json::from_value(json!({"hint": "use the monthly view"}))
        .expect("approve hint decodes");
    assert_eq!(approve.hint.as_deref(), Some("use the monthly view"));
    let approve: ApproveBody =
        serde_json::from_value(json!({})).expect("empty approve body is legal");
    assert!(approve.hint.is_none());

    let feedback: FeedbackBody = serde_json::from_value(json!({
        "verdict": "wrong_label",
        "comment": "this is a newsletter",
    }))
    .expect("feedback body decodes");
    assert_eq!(feedback.verdict, MailFeedbackVerdict::WrongLabel);
    assert_eq!(feedback.comment.as_deref(), Some("this is a newsletter"));

    let missing_verdict: Result<FeedbackBody, _> = serde_json::from_value(json!({
        "comment": "no verdict"
    }));
    assert!(missing_verdict.is_err(), "verdict is required on feedback");

    // The verdict vocabulary itself.
    for (verdict, token) in [
        (MailFeedbackVerdict::Helpful, "helpful"),
        (MailFeedbackVerdict::NotHelpful, "not_helpful"),
        (MailFeedbackVerdict::WrongLabel, "wrong_label"),
        (MailFeedbackVerdict::Other, "other"),
    ] {
        assert_eq!(
            serde_json::to_value(verdict).expect("serializes"),
            json!(token)
        );
    }
}

#[test]
fn channel_assist_fixture_export_row_round_trips_jsonl() {
    // The `magician channel-assist export-fixtures` row contract: one
    // compact JSON object per line, parseable back into the same row.
    let record = MailThreadRecord {
        schema_version: MAIL_ASSIST_SCHEMA_VERSION,
        provider: "gmail".to_string(),
        account_alias: "acct-a".to_string(),
        account_email: Some("owner@example.com".to_string()),
        thread_id: "t-secret-1".to_string(),
        lane: ChannelLane::UserAssist,
        subject: Some("Quarterly sync".to_string()),
        latest_summary: None,
        latest_from_name: Some("Sender One".to_string()),
        latest_from_address: Some("sender@partner.example".to_string()),
        recipient_domains: vec!["secret-recipient.example".to_string()],
        label_ids: vec!["INBOX".to_string()],
        message_count: 2,
        last_message_at: Some(86_400_000),
        provider_cursor: None,
        sensitive_suppressed: false,
        origin: MailRecordOrigin::MetadataSync,
        first_observed_at: 0,
        last_observed_at: 86_400_000,
    };
    let row = fixture_row(&record, 2 * 86_400_000);

    assert_eq!(row.schema_version, MAIL_ASSIST_SCHEMA_VERSION);
    assert_eq!(row.thread_ref, short_hash("t-secret-1"));
    assert_eq!(row.thread_ref.len(), FIXTURE_HASH_LEN);
    assert_eq!(row.sender_domain.as_deref(), Some("partner.example"));
    assert_eq!(row.last_message_age_days, Some(1));
    assert_eq!(row.first_observed_age_days, 2);
    assert_eq!(row.label, None, "hand-label slot exports empty");

    let jsonl = render_jsonl(&[row.clone()]).expect("renders jsonl");
    assert!(jsonl.ends_with('\n'), "one trailing newline per row");
    let parsed: MailFixtureRow = serde_json::from_str(jsonl.trim_end_matches('\n'))
        .expect("exported line parses back as a fixture row");
    assert_eq!(parsed, row, "JSONL round-trip is lossless");

    // The privacy contract of the export: hashed thread id, domain-only
    // sender, and NO raw ids, full addresses, or recipient data.
    assert!(!jsonl.contains("t-secret-1"));
    assert!(!jsonl.contains("sender@partner.example"));
    assert!(!jsonl.contains("secret-recipient.example"));
    assert!(!jsonl.contains("owner@example.com"));

    // Suppressed rows keep the redaction placeholder, never a subject.
    assert_eq!(REDACTED_SUBJECT_PLACEHOLDER, "[subject suppressed]");
    assert_eq!(MAIL_ASSIST_SCHEMA_VERSION, 9);
    assert_eq!(FIXTURE_HASH_LEN, 12);
}

// --- Recurring Monitor spec round-trips (plan 0.3 corpus 2) ------------------
//
// `MonitorSpecV1` is the sole discriminator of a monitor (it rides on
// `TaskManifest.monitor_spec`); cadence stays on `Task.schedule` as the
// `TaskSchedule` JSON; the diff policy is the notification gate
// (`would_notify`) over `MonitorNotificationPolicy` + finding
// materiality. All three serialize exactly as pinned below, and the
// canonical Phase 0 fixture shared with web/iOS round-trips losslessly.

#[test]
fn monitor_spec_wire_form_round_trips_exactly() {
    // A domain+authenticated-source binding (not just URLs), strict
    // matching, every-run notifications — the other end of the spec
    // vocabulary from the canonical fixture.
    let spec = MonitorSpecV1 {
        schema_version: MONITOR_SPEC_SCHEMA_VERSION,
        objective: "Watch competitor pricing".to_string(),
        query_seeds: vec!["competitor pricing".to_string()],
        sources: MonitorSources {
            urls: vec!["https://competitor.example/pricing".to_string()],
            domains: vec!["competitor.example".to_string()],
            authenticated_sources: vec!["gmail:acct-a".to_string()],
        },
        include_rules: vec!["plan tiers".to_string()],
        exclude_rules: vec!["blog posts".to_string()],
        match_mode: MonitorMatchMode::Strict,
        notification_policy: MonitorNotificationPolicy::EveryRun,
        notify_initial_baseline: true,
    };
    let wire = serde_json::to_value(&spec).expect("spec serializes");
    assert_eq!(
        wire,
        json!({
            "schema_version": 1,
            "objective": "Watch competitor pricing",
            "query_seeds": ["competitor pricing"],
            "sources": {
                "urls": ["https://competitor.example/pricing"],
                "domains": ["competitor.example"],
                "authenticated_sources": ["gmail:acct-a"],
            },
            "include_rules": ["plan tiers"],
            "exclude_rules": ["blog posts"],
            "match_mode": "strict",
            "notification_policy": "every_run",
            "notify_initial_baseline": true,
        })
    );
    let back: MonitorSpecV1 = serde_json::from_value(wire).expect("spec round-trips");
    assert_eq!(back, spec);

    // Enum vocabularies: three match modes, three notification policies,
    // one accepted schema version.
    for (mode, token) in [
        (MonitorMatchMode::Strict, "strict"),
        (MonitorMatchMode::Balanced, "balanced"),
        (MonitorMatchMode::Broad, "broad"),
    ] {
        assert_eq!(serde_json::to_value(mode).unwrap(), json!(token));
    }
    for (policy, token) in [
        (
            MonitorNotificationPolicy::MaterialChanges,
            "material_changes",
        ),
        (MonitorNotificationPolicy::EveryRun, "every_run"),
        (MonitorNotificationPolicy::Never, "never"),
    ] {
        assert_eq!(serde_json::to_value(policy).unwrap(), json!(token));
    }
    assert_eq!(MONITOR_SPEC_SCHEMA_VERSION, 1);
}

#[test]
fn monitor_spec_canonical_fixture_round_trips() {
    // The shared Phase 0 wire fixture — web (`ui/unified-ui`) and iOS
    // read the same bytes, so decode + re-serialize must be lossless.
    const SPEC_FIXTURE: &str = include_str!("fixtures/monitors/monitor_spec_v1.json");
    let spec: MonitorSpecV1 =
        serde_json::from_str(SPEC_FIXTURE).expect("canonical fixture decodes as MonitorSpecV1");
    assert_eq!(spec.schema_version, 1);
    assert_eq!(spec.match_mode, MonitorMatchMode::Balanced);
    assert_eq!(
        spec.notification_policy,
        MonitorNotificationPolicy::MaterialChanges
    );
    assert!(!spec.notify_initial_baseline);
    let original: serde_json::Value = serde_json::from_str(SPEC_FIXTURE).expect("fixture is JSON");
    assert_eq!(
        serde_json::to_value(&spec).expect("serializes"),
        original,
        "MonitorSpecV1 must round-trip the canonical fixture without drift"
    );
}

#[test]
fn monitor_cadence_schedule_wire_form_is_pinned() {
    // Cadence lives on `Task.schedule`, not on the spec. The kind is an
    // externally tagged enum with PascalCase variant keys — the exact
    // shape the chat tools' schedule hint documents.
    let schedule = TaskSchedule {
        kind: TaskScheduleKind::Cron {
            expression: "0 9 * * *".to_string(),
            timezone: Some("America/New_York".to_string()),
        },
        timezone: Some("UTC".to_string()),
        missed_fire_policy: MissedFirePolicy::Skip,
        concurrent_execution_policy: ConcurrentExecutionPolicy::Skip,
        execution_history_retention: None,
        max_runs: Some(90),
        paused: Some(false),
    };
    let wire = serde_json::to_value(&schedule).expect("schedule serializes");
    assert_eq!(
        wire,
        json!({
            "kind": {"Cron": {"expression": "0 9 * * *", "timezone": "America/New_York"}},
            "timezone": "UTC",
            "missed_fire_policy": "skip",
            "concurrent_execution_policy": "skip",
            "max_runs": 90,
            "paused": false,
        })
    );
    let back: TaskSchedule = serde_json::from_value(wire.clone()).expect("schedule round-trips");
    assert_eq!(
        serde_json::to_value(&back).expect("re-serializes"),
        wire,
        "TaskSchedule round-trip is lossless"
    );
    assert_eq!(back.max_runs, Some(90));

    // The interval variant in its minimal wire form (jitter omitted).
    let interval: TaskSchedule = serde_json::from_value(json!({
        "kind": {"Interval": {"seconds": 900}}
    }))
    .expect("minimal interval schedule decodes");
    match interval.kind {
        TaskScheduleKind::Interval {
            seconds,
            jitter_seconds,
        } => {
            assert_eq!(seconds, 900);
            assert_eq!(jitter_seconds, None);
        },
        other => panic!("expected Interval kind, got {other:?}"),
    }
}

#[test]
fn monitor_diff_policy_notification_gate_is_pinned() {
    // `would_notify` is the whole diff policy: material_changes notifies
    // only on material findings (or a opted-in baseline), every_run
    // always, never — never.
    let spec = |policy: MonitorNotificationPolicy, baseline: bool| MonitorSpecV1 {
        schema_version: MONITOR_SPEC_SCHEMA_VERSION,
        objective: "o".to_string(),
        query_seeds: Vec::new(),
        sources: MonitorSources {
            urls: vec!["https://example.com".to_string()],
            domains: Vec::new(),
            authenticated_sources: Vec::new(),
        },
        include_rules: Vec::new(),
        exclude_rules: Vec::new(),
        match_mode: MonitorMatchMode::Balanced,
        notification_policy: policy,
        notify_initial_baseline: baseline,
    };

    let quiet_baseline = spec(MonitorNotificationPolicy::MaterialChanges, false);
    assert!(would_notify(
        &quiet_baseline,
        MonitorRunStatus::Changed,
        true
    ));
    assert!(!would_notify(
        &quiet_baseline,
        MonitorRunStatus::Changed,
        false
    ));
    assert!(!would_notify(
        &quiet_baseline,
        MonitorRunStatus::Baseline,
        false
    ));

    let loud_baseline = spec(MonitorNotificationPolicy::MaterialChanges, true);
    assert!(would_notify(
        &loud_baseline,
        MonitorRunStatus::Baseline,
        false
    ));

    assert!(would_notify(
        &spec(MonitorNotificationPolicy::EveryRun, false),
        MonitorRunStatus::Unchanged,
        false
    ));
    assert!(!would_notify(
        &spec(MonitorNotificationPolicy::Never, true),
        MonitorRunStatus::Changed,
        true
    ));

    // Finding materiality vocabulary: new/updated/possibly_removed are
    // material, unchanged never is.
    let findings = [
        (MonitorFindingClassification::New, "new", true),
        (MonitorFindingClassification::Updated, "updated", true),
        (MonitorFindingClassification::Unchanged, "unchanged", false),
        (
            MonitorFindingClassification::PossiblyRemoved,
            "possibly_removed",
            true,
        ),
    ];
    for (finding, token, material) in findings {
        assert_eq!(finding.as_str(), token);
        assert_eq!(finding.is_material(), material);
        assert_eq!(serde_json::to_value(finding).unwrap(), json!(token));
    }
    for (status, token) in [
        (MonitorRunStatus::Baseline, "baseline"),
        (MonitorRunStatus::Changed, "changed"),
        (MonitorRunStatus::Unchanged, "unchanged"),
        (MonitorRunStatus::Degraded, "degraded"),
        (MonitorRunStatus::Failed, "failed"),
    ] {
        assert_eq!(serde_json::to_value(status).unwrap(), json!(token));
    }
}

// --- Tutor event sequences (plan 0.3 corpus 2, feeds the 4.1 split) ----------
//
// Visibility note (characterization scope): the payload builders
// (`emit_tutor_run_progress` / `emit_tutor_step_progress`) are private
// in `chat::tools_runtime` and assemble their payloads inline via
// `json!` — there is no pub payload type to import. This corpus pins
// the pub-reachable sequence contract instead: the `tutor.*` event ids
// and their taxonomy rows (serde form included), plus the run/step
// enums whose serde tokens ARE the payload field values (`mode`,
// `canvas_mode`, `status`, `step_kind`, `step_status`) and the
// `TutorRun` record round-trip the persisted sequence deserializes
// from.

#[test]
fn tutor_event_ids_and_taxonomy_rows_are_pinned() {
    // The full `tutor.*` sequence in emit order: run lifecycle, then the
    // observe/draw/act/verify step trail. (id, severity, user_relevant)
    // per event — the HUD's compact trail plus `/events` debugging both
    // consume this exact taxonomy.
    let events: [(RuntimeAgentEventType, &str, EventSeverity, bool); 13] = [
        (
            RuntimeAgentEventType::TutorRunStarted,
            "tutor.run.started",
            EventSeverity::Info,
            true,
        ),
        (
            RuntimeAgentEventType::TutorRunCompleted,
            "tutor.run.completed",
            EventSeverity::Info,
            true,
        ),
        (
            RuntimeAgentEventType::TutorRunFailed,
            "tutor.run.failed",
            EventSeverity::Warn,
            true,
        ),
        (
            RuntimeAgentEventType::TutorStepObserved,
            "tutor.step.observed",
            EventSeverity::Info,
            false,
        ),
        (
            RuntimeAgentEventType::TutorStepTargetResolved,
            "tutor.step.target_resolved",
            EventSeverity::Info,
            false,
        ),
        (
            RuntimeAgentEventType::TutorStepDrawing,
            "tutor.step.drawing",
            EventSeverity::Info,
            false,
        ),
        (
            RuntimeAgentEventType::TutorDrawShape,
            "tutor.draw.shape",
            EventSeverity::Info,
            true,
        ),
        (
            RuntimeAgentEventType::TutorStepActionDelegated,
            "tutor.step.action_delegated",
            EventSeverity::Info,
            true,
        ),
        (
            RuntimeAgentEventType::TutorStepVerifying,
            "tutor.step.verifying",
            EventSeverity::Info,
            false,
        ),
        (
            RuntimeAgentEventType::TutorStepVerified,
            "tutor.step.verified",
            EventSeverity::Info,
            true,
        ),
        (
            RuntimeAgentEventType::TutorStepFailed,
            "tutor.step.failed",
            EventSeverity::Warn,
            true,
        ),
        (
            RuntimeAgentEventType::TutorStepRecovering,
            "tutor.step.recovering",
            EventSeverity::Warn,
            true,
        ),
        (
            RuntimeAgentEventType::TutorStepClearing,
            "tutor.step.clearing",
            EventSeverity::Info,
            false,
        ),
    ];
    for (variant, id, severity, user_relevant) in events {
        assert_eq!(variant.as_str(), id, "wire id for {variant:?}");
        assert_eq!(
            RuntimeAgentEventType::from_str(id),
            Some(variant),
            "reverse lookup for {id}"
        );
        let taxonomy = lookup_agent_event_taxonomy(id).unwrap_or_else(|| {
            panic!("no taxonomy row for {id}");
        });
        assert_eq!(taxonomy.category, EventCategory::Execution);
        assert_eq!(taxonomy.severity, severity, "severity for {id}");
        assert_eq!(taxonomy.user_relevant, user_relevant, "relevance for {id}");
        assert_eq!(
            taxonomy.render,
            RenderHint::DEFAULT,
            "tutor rows carry no special render hint"
        );
    }
    assert_eq!(
        lookup_agent_event_taxonomy("tutor.run.silenced"),
        None,
        "no invented tutor ids resolve"
    );
}

#[test]
fn tutor_event_taxonomy_serde_form_is_pinned() {
    // One representative row: the operator metadata serializes as the
    // four-key snake_case object the TS mirror and `/events` consumers
    // read.
    let taxonomy = lookup_agent_event_taxonomy("tutor.run.started").expect("taxonomy row exists");
    assert_eq!(
        serde_json::to_value(&taxonomy).expect("serializes"),
        json!({
            "category": "execution",
            "severity": "info",
            "user_relevant": true,
            "render": {"chat_kind": "suppress", "coalesce_by": "none"},
        })
    );
}

#[test]
fn tutor_event_payload_vocabulary_tokens_are_pinned() {
    // Every payload enum: the serde token must equal the `as_str` value
    // the emitters interpolate into the payload — one vocabulary, no
    // drift between persisted and emitted forms.
    let modes = [
        TutorRunMode::ExplainOnly,
        TutorRunMode::GuidedAction,
        TutorRunMode::DemoAndCleanup,
        TutorRunMode::ConceptExplainer,
        TutorRunMode::GuidedSolution,
        TutorRunMode::ConceptDemo,
    ];
    for mode in modes {
        assert_eq!(
            serde_json::to_value(mode).expect("serializes"),
            json!(mode.as_str()),
            "serde token must match as_str for {mode:?}"
        );
    }
    let tokens = [
        "explain_only",
        "guided_action",
        "demo_and_cleanup",
        "concept_explainer",
        "guided_solution",
        "concept_demo",
    ];
    for (mode, token) in modes.iter().zip(tokens) {
        assert_eq!(mode.as_str(), token);
    }

    for (canvas, token) in [
        (TutorCanvasMode::ScreenOverlay, "screen_overlay"),
        (TutorCanvasMode::Blackboard, "blackboard"),
    ] {
        assert_eq!(canvas.as_str(), token);
        assert_eq!(serde_json::to_value(canvas).unwrap(), json!(token));
    }

    let run_statuses = [
        TutorRunStatus::Planned,
        TutorRunStatus::Running,
        TutorRunStatus::WaitingForConfirmation,
        TutorRunStatus::Completed,
        TutorRunStatus::Failed,
    ];
    for status in run_statuses {
        assert_eq!(
            serde_json::to_value(status).expect("serializes"),
            json!(status.as_str()),
            "serde token must match as_str for {status:?}"
        );
    }

    let step_statuses = [
        TutorStepStatus::Pending,
        TutorStepStatus::Running,
        TutorStepStatus::Succeeded,
        TutorStepStatus::Failed,
        TutorStepStatus::WaitingForConfirmation,
        TutorStepStatus::Skipped,
    ];
    for status in step_statuses {
        assert_eq!(
            serde_json::to_value(status).expect("serializes"),
            json!(status.as_str()),
            "serde token must match as_str for {status:?}"
        );
    }

    let step_kinds = [
        TutorStepKind::Observe,
        TutorStepKind::ResolveTarget,
        TutorStepKind::Draw,
        TutorStepKind::Say,
        TutorStepKind::Wait,
        TutorStepKind::Click,
        TutorStepKind::TypeText,
        TutorStepKind::Hotkey,
        TutorStepKind::Scroll,
        TutorStepKind::Verify,
        TutorStepKind::ClearDrawings,
        TutorStepKind::Confirm,
        TutorStepKind::Recover,
    ];
    let tokens = [
        "observe",
        "resolve_target",
        "draw",
        "say",
        "wait",
        "click",
        "type_text",
        "hotkey",
        "scroll",
        "verify",
        "clear_drawings",
        "confirm",
        "recover",
    ];
    for (kind, token) in step_kinds.iter().zip(tokens) {
        assert_eq!(kind.as_str(), token);
        assert_eq!(
            serde_json::to_value(kind).expect("serializes"),
            json!(token),
            "serde token for {kind:?}"
        );
    }
}

#[test]
fn tutor_run_record_round_trips_with_step_history() {
    // The persisted run record behind every `tutor.*` sequence: the
    // step history carries the storyboard coverage the completion
    // contract counts.
    let run = TutorRun {
        run_id: "run-1".to_string(),
        mode: TutorRunMode::ConceptExplainer,
        canvas_mode: TutorCanvasMode::ScreenOverlay,
        status: TutorRunStatus::WaitingForConfirmation,
        goal: "Explain the save flow".to_string(),
        product_rail: TutorProductRail::PersonalTutor,
        quick: false,
        lesson_contract: None,
        step_history: vec![TutorStepRecord {
            step: TutorStep {
                kind: TutorStepKind::Draw,
                label: "point at save".to_string(),
                target: Some("save_button".to_string()),
                expected_state: None,
                safety: TutorSafetyLevel::VisualOnly,
                source_entity_ids: Vec::new(),
                visual_entity_map: None,
            },
            status: TutorStepStatus::Succeeded,
            storyboard_steps: vec![TutorDrawStoryboardStep {
                step_id: "s1".to_string(),
                label: "point at save".to_string(),
                narration: "click save after typing".to_string(),
                figure_backed: true,
            }],
        }],
        created_objects: Vec::new(),
        has_fresh_observation: true,
        has_resolved_target: true,
        latest_visual_entity_map: None,
        pending_action: None,
        pending_action_execution_id: None,
        pending_created_object: None,
        removed_created_object_labels: Vec::new(),
        copilot_action_check: None,
        terminal_reason: None,
        destructive_confirmed: false,
        retry_count: 0,
        max_retries: 2,
        thinking_map_context: None,
    };
    let wire = serde_json::to_value(&run).expect("run serializes");
    assert_eq!(wire["run_id"], "run-1");
    assert_eq!(wire["mode"], "concept_explainer");
    assert_eq!(wire["canvas_mode"], "screen_overlay");
    assert_eq!(wire["status"], "waiting_for_confirmation");
    assert_eq!(wire["product_rail"], "personal_tutor");
    assert_eq!(wire["step_history"][0]["step"]["kind"], "draw");
    assert_eq!(wire["step_history"][0]["status"], "succeeded");
    assert_eq!(
        wire["step_history"][0]["storyboard_steps"][0]["step_id"],
        "s1"
    );
    let back: TutorRun = serde_json::from_value(wire).expect("run round-trips");
    assert_eq!(back, run, "TutorRun round-trip is lossless");
}
