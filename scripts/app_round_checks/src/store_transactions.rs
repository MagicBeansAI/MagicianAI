use crate::{
    contextual_round_program::{
        AppRoundValueEnvironment, AppRoundValueExpression, AppRoundValueSource,
    },
    store_transaction::AppStoreTransactionDeclaration,
};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};

fn declaration(app: &str, workflow: &str) -> AppStoreTransactionDeclaration {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../magician_data_v3/system")
        .join(app)
        .join("app/recipes")
        .join(format!("{workflow}.json"));
    let bundle: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let program: AppStoreTransactionDeclaration =
        serde_json::from_value(bundle["recipe"]["nodes"]["root"]["node"]["program"].clone())
            .unwrap();
    program.validate().unwrap();
    program
}

fn env<'a>(input: &'a Value, context: &'a Value) -> AppRoundValueEnvironment<'a> {
    AppRoundValueEnvironment {
        input,
        context,
        participant: &Value::Null,
        model: None,
        item: None,
        now: DateTime::parse_from_rfc3339("2026-09-09T18:00:00Z")
            .unwrap()
            .with_timezone(&Utc),
        run_id: "fixture-execution",
        participant_id: "",
    }
}

#[test]
fn shipped_ledger_actions_are_deterministic_and_cannot_read_model_or_participant_state() {
    for (app, workflows) in [
        (
            "learning",
            vec!["approve-candidate", "reject-candidate", "snooze-candidate"],
        ),
        (
            "meetings",
            vec!["listen", "join", "pause", "resume", "stop"],
        ),
        (
            "claims_review",
            vec![
                "confirm-claim",
                "reject-claim",
                "record-commitment",
                "confirm-commitment",
            ],
        ),
        ("town_square", vec!["set-policy", "sync-feed"]),
    ] {
        for workflow in workflows {
            let original = declaration(app, workflow);
            for source in [AppRoundValueSource::Model, AppRoundValueSource::Participant] {
                let mut invalid = original.clone();
                invalid
                    .values
                    .expressions
                    .push(AppRoundValueExpression::Read {
                        source,
                        pointer: "/hidden".into(),
                    });
                assert!(
                    invalid.validate().is_err(),
                    "{app}/{workflow} admits a hidden semantic dependency"
                );
            }
        }
    }
}

#[test]
fn meeting_controls_preserve_exact_canonical_payload_and_idempotency() {
    let program = declaration("meetings", "listen");
    let input = json!({"request_id":"request-1","capture_mic":true,"gesture_id":"gesture-1","surface_session_id":"surface-1","gesture_observed_at_ms":100,"gesture_expires_at_ms":200});
    let empty = json!({"existing":[]});
    let plan = program.plan(&env(&input, &empty)).unwrap();
    assert_eq!(plan.operations.len(), 1);
    let payload = &plan.operations[0]["payload"];
    assert_eq!(payload["actor_ref"], Value::Null);
    assert_eq!(payload["apply_state"], "recorded");
    assert_eq!(payload["payload_json"], "{\"capture_mic\":true,\"date\":null,\"gesture_expires_at_ms\":200,\"gesture_id\":\"gesture-1\",\"gesture_observed_at_ms\":100,\"request_id\":\"request-1\",\"surface_session_id\":\"surface-1\",\"title\":null,\"url\":null,\"verb\":\"listen\"}");
    assert_eq!(
        program.plan(&env(&input, &empty)).unwrap().operations,
        plan.operations
    );
    let existing =
        json!({"existing":[{"record_id":"existing","record_revision":2,"fields":payload}]});
    assert!(program
        .plan(&env(&input, &existing))
        .unwrap()
        .operations
        .is_empty());
    let mut altered = input.clone();
    altered["capture_mic"] = json!(false);
    assert!(program.plan(&env(&altered, &existing)).is_err());
}

#[test]
fn claims_request_identity_cannot_change_decision_or_expected_revision() {
    let confirm = declaration("claims_review", "confirm-claim");
    let reject = declaration("claims_review", "reject-claim");
    let input =
        json!({"request_id":"r","claim_id":"c","expected_revision":7,"reason":"owner decision"});
    let plan = confirm.plan(&env(&input, &json!({"existing":[]}))).unwrap();
    let payload = &plan.operations[0]["payload"];
    assert_eq!(payload["expected_revision"], 7);
    assert_eq!(payload["actor_ref"], Value::Null);
    let existing =
        json!({"existing":[{"record_id":"existing","record_revision":2,"fields":payload}]});
    assert!(reject.plan(&env(&input, &existing)).is_err());
    let mut newer = input.clone();
    newer["expected_revision"] = json!(8);
    assert!(confirm.plan(&env(&newer, &existing)).is_err());
}

#[test]
fn feed_window_preserves_content_and_policy_updates_require_the_read_revision() {
    let feed = declaration("town_square", "sync-feed");
    let rows: Vec<_> = (0..20).map(|i| json!({"record_id":format!("post-{i}"),"record_revision":3,"fields":{"body":"historical text"}})).collect();
    let plan = feed
        .plan(&env(&json!({"limit":2}), &json!({"posts":rows})))
        .unwrap();
    assert_eq!(plan.operations.len(), 2);
    for operation in &plan.operations {
        assert_eq!(operation["kind"], "update");
        assert_eq!(operation["patch"].as_object().unwrap().len(), 1);
        assert!(operation["patch"].get("synced_at").is_some());
    }
    let policy = declaration("town_square", "set-policy");
    let input = json!({"autonomy_state":"on","cooldown_seconds":1800,"max_post_chars":300,"max_autonomous_replies":3});
    let created = policy.plan(&env(&input, &json!({"policy":[]}))).unwrap();
    assert_eq!(created.operations[0]["record_id"], "singleton");
    let context = json!({"policy":[{"record_id":"singleton","record_revision":9}]});
    let update = policy.plan(&env(&input, &context)).unwrap();
    assert_eq!(update.operations.len(), 1);
    assert_eq!(update.expected_record_revisions[0]["revision"], 9);
}

#[test]
fn manual_posts_preserve_supplied_text_and_deliver_only_known_distinct_mentions() {
    let program = declaration("town_square", "publish-post");
    let input = json!({"post_id":"manual","author_id":"author","surface":"feed","post_type":"thought","body":"Supplied text, unchanged.","mentioned_member_ids":"recipient, recipient, unknown, author"});
    let context = json!({"author":[{"record_id":"author"}],"parent":[],"policy":[],"existing":[],"members":[{"record_id":"author","fields":{"member_id":"author"}},{"record_id":"recipient","fields":{"member_id":"recipient"}}]});
    let plan = program.plan(&env(&input, &context)).unwrap();
    assert_eq!(plan.operations.len(), 2);
    let values = program.values.evaluate(&env(&input, &context)).unwrap();
    assert!(!program.query_enabled("parent", &values).unwrap());
    let mut reply = input.clone();
    reply["parent_id"] = json!("known-parent");
    let values = program.values.evaluate(&env(&reply, &context)).unwrap();
    assert!(program.query_enabled("parent", &values).unwrap());
    assert_eq!(plan.operations[0]["payload"]["body"], input["body"]);
    assert_eq!(plan.operations[0]["payload"]["parent_id"], Value::Null);
    assert_eq!(
        plan.operations[1]["payload"]["mentioned_member_id"],
        "recipient"
    );
    assert_eq!(
        plan.operations[1]["payload"]["delivery_kind"],
        "explicit_mention"
    );
    for (field, value) in [
        ("body", json!(" ")),
        ("body", json!("x".repeat(601))),
        ("parent_id", json!("missing")),
        ("group_id", json!("unexpected")),
    ] {
        let mut invalid = input.clone();
        invalid[field] = value;
        assert!(
            program.plan(&env(&invalid, &context)).is_err(),
            "accepted invalid {field}"
        );
    }
    let mut context = context;
    context["author"] = json!([]);
    assert!(program.plan(&env(&input, &context)).is_err());
}

#[test]
fn reaction_removal_has_an_exact_revision_and_never_deletes_a_different_members_reaction() {
    let program = declaration("town_square", "react-to-post");
    let input = json!({"reaction_id":"reaction","post_id":"post","member_id":"member","emoji":"heart","removed":true});
    let row = json!({"record_id":"stored-reaction","record_revision":12,"fields":{"reaction_id":"reaction","post_id":"post","member_id":"member","emoji":"heart"}});
    let mut context = json!({"post":[{"record_id":"post"}],"existing":[row.clone()],"tuple":[row]});
    let plan = program.plan(&env(&input, &context)).unwrap();
    assert_eq!(
        plan.operations,
        vec![json!({"kind":"delete","entity":"reaction","record_id":"stored-reaction"})]
    );
    assert_eq!(plan.expected_record_revisions[0]["revision"], 12);
    context["existing"][0]["fields"]["member_id"] = json!("different");
    assert!(program.plan(&env(&input, &context)).is_err());
}

#[test]
fn group_creation_commits_membership_together_and_refuses_missing_creator_or_existing_group() {
    let program = declaration("town_square", "create-group");
    let input = json!({"group_id":"group","name":"A group","created_by":"creator"});
    let context = json!({"group":[],"creator":[{"record_id":"creator"}]});
    let plan = program.plan(&env(&input, &context)).unwrap();
    assert_eq!(plan.operations.len(), 2);
    assert_eq!(plan.operations[1]["entity"], "group_membership");
    assert_eq!(plan.operations[1]["payload"]["member_id"], "creator");
    assert!(program
        .plan(&env(&input, &json!({"group":[],"creator":[]})))
        .is_err());
    assert!(program
        .plan(&env(
            &input,
            &json!({"group":[{"record_id":"existing"}],"creator":[{"record_id":"creator"}]})
        ))
        .is_err());
}
