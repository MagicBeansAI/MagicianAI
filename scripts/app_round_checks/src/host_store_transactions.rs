use crate::{
    contextual_round_program::{
        AppRoundValueEnvironment, AppRoundValueExpression, AppRoundValueSource,
    },
    store_transaction::AppStoreTransactionDeclaration,
};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};

fn load(app: &str, action: &str) -> AppStoreTransactionDeclaration {
    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../magician_data_v3/system/{app}/app/recipes/{action}.json"
    ));
    let bundle: Value = serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    let p: AppStoreTransactionDeclaration =
        serde_json::from_value(bundle["recipe"]["nodes"]["root"]["node"]["program"].clone())
            .unwrap();
    p.validate().unwrap();
    p
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
        run_id: "host-store-fixture",
        participant_id: "",
    }
}
#[test]
fn mechanical_host_pages_preserve_source_truth_and_update_only_selected_keys() {
    let cases: Value =
        serde_json::from_str(include_str!("fixtures/host_store_sources.json")).unwrap();
    for (action, case) in cases.as_object().unwrap() {
        let p = load(case["app"].as_str().unwrap(), action);
        let mut context = case["context"].clone();
        for query in &p.queries {
            context[&query.name] = json!([]);
        }
        let prepared = p.values.evaluate(&env(&case["input"], &context)).unwrap();
        for query in &p.queries {
            if p.query_enabled(&query.name, &prepared).unwrap() {
                let parameters = p.query_parameters(query, &prepared).unwrap();
                assert!(
                    parameters.contains_key("predicate"),
                    "{action} widened to a table scan"
                );
            }
        }
        for overrides in p.source_parameters.values() {
            assert_eq!(
                prepared.get(overrides["limit"]).unwrap(),
                &json!(20),
                "{action}"
            );
        }
        let plan = p.plan(&env(&case["input"], &context)).unwrap();
        let expected = match action.as_str() {
            "search-meetings" => 2,
            "sync-claims" => 3,
            "sync-context" => 3,
            _ => 1,
        };
        assert_eq!(plan.operations.len(), expected, "{action}");
        let summary: Value = serde_json::from_str(&p.summary(&prepared).unwrap()).unwrap();
        assert_eq!(
            summary["source_pages"].as_object().unwrap().len(),
            if action == "sync-claims" {
                2
            } else {
                case["context"].as_object().unwrap().len()
            },
            "{action}"
        );
        match action.as_str() {
            "sync-sessions" => {
                let f = &plan.operations[0]["payload"];
                assert_eq!(f["in_scope"], false);
                assert_eq!(f["title"], Value::Null);
                assert_eq!(f["capture_mic"], Value::Null);
            },
            "sync-threads" => {
                assert_eq!(
                    summary["source_pages"]["threads"]["next_cursor"],
                    "thread-next"
                );
                assert_eq!(summary["source_pages"]["threads"]["scan_truncated"], true);
            },
            "read-transcript" => {
                assert_eq!(
                    plan.operations[0]["payload"]["line_text"],
                    "Original words, without summary."
                );
                assert_eq!(summary["source_pages"]["transcript"]["skipped_non_text"], 3);
            },
            "sync-takeaways" => {
                let f = &plan.operations[0]["payload"];
                assert_eq!(f["decisions"], "- First decision\n- Second decision");
                assert_eq!(f["action_items"], Value::Null);
                assert_eq!(f["summary"], "Already written summary");
            },
            "sync-upcoming" => {
                assert_eq!(plan.operations[0]["payload"]["event_id"], "event-1");
                assert_eq!(
                    summary["source_pages"]["upcoming"]["errors"][0]["error"],
                    "unavailable"
                );
            },
            "search-meetings" => {
                assert_eq!(plan.operations[0]["payload"]["hit_id"], "message-2");
                assert_eq!(plan.operations[1]["payload"]["hit_id"], "thread-2:takeaway");
            },
            "sync-claims" => {
                assert_eq!(plan.operations[0]["payload"]["claim_id"], "claim-1");
                let unbound = &plan.operations[1]["payload"];
                assert_eq!(unbound["claim_id"], "claim-no-audience");
                assert_eq!(unbound["claim_text"], "No inferred audience");
                assert_eq!(unbound["audience_kind"], Value::Null);
                assert_eq!(unbound["audience_id"], Value::Null);
                assert_eq!(
                    plan.operations[0]["payload"]["context_excerpt"],
                    Value::Null
                );
            },
            "sync-context" => {
                assert_eq!(
                    summary["source_pages"]["evidence"]["next_cursor"],
                    "evidence-next"
                );
                assert_eq!(
                    summary["source_pages"]["entities"]["next_cursor"],
                    "entity-next"
                );
            },
            _ => unreachable!(),
        }
        for operation in &plan.operations {
            let entity = operation["entity"].as_str().unwrap();
            context[format!("{entity}_existing")].as_array_mut().unwrap().push(json!({"record_id":operation["record_id"],"record_revision":7,"fields":operation["payload"]}));
        }
        let updates = p.plan(&env(&case["input"], &context)).unwrap();
        assert_eq!(updates.operations.len(), expected);
        assert_eq!(updates.expected_record_revisions.len(), expected);
        assert!(updates.operations.iter().all(|op| op["kind"] == "update"));
        for source in [AppRoundValueSource::Model, AppRoundValueSource::Participant] {
            let mut invalid = p.clone();
            invalid
                .values
                .expressions
                .push(AppRoundValueExpression::Read {
                    source,
                    pointer: "/hidden".into(),
                });
            assert!(invalid.validate().is_err());
        }
    }
}

#[test]
fn transcript_stage_uses_bytes_and_preserves_verbatim_input_without_applying_it() {
    let p = load("claims_review", "stage-ingest");
    let input = json!({"ingest_id":"ingest-1","transcript_text":"exact transcript", "speaker_mapping_json":"{\"speakers\":{\"a\":\"Alice\"},\"utterances\":[{\"speaker\":\"a\",\"text\":\"Hello\"}]}","audience_kind":"person","audience_id":"bob","outwardness_reason":"Explicit meeting"});
    let empty = json!({"existing":[]});
    let plan = p.plan(&env(&input, &empty)).unwrap();
    assert_eq!(plan.operations.len(), 1);
    let fields = &plan.operations[0]["payload"];
    for (name, value) in input.as_object().unwrap() {
        assert_eq!(&fields[name], value);
    }
    assert_eq!(fields["actor_ref"], Value::Null);
    assert_eq!(fields["act_ref"], Value::Null);
    assert_eq!(fields["apply_state"], "recorded");
    let mut large = input.clone();
    large["transcript_text"] = json!("é".repeat(65536));
    assert!(p.plan(&env(&large, &empty)).is_ok());
    large["transcript_text"] = json!("é".repeat(65537));
    assert!(p.plan(&env(&large, &empty)).is_err());
    let existing = json!({"existing":[{"record_id":plan.operations[0]["record_id"],"record_revision":2,"fields":fields}]});
    assert!(p
        .plan(&env(&input, &existing))
        .unwrap()
        .operations
        .is_empty());
    let mut different = input.clone();
    different["transcript_text"] = json!("different request under same ID");
    assert!(p.plan(&env(&different, &existing)).is_err());
    different = input.clone();
    different["speaker_mapping_json"] =
        json!("{\"speakers\":{\"a\":\"Alice\",\"a\":\"Bob\"},\"utterances\":[]}");
    assert!(p.plan(&env(&different, &empty)).is_err());
}

#[test]
fn envoy_claims_sync_retains_unbound_detail_without_inventing_a_relationship() {
    let program = load("claims_review", "sync-claims");
    let input = json!({"claim_id":"envoy-claim"});
    let context = json!({
        "claims":{"claims":[],"count":0,"scanned":0,"scan_truncated":false},
        "claim_summary_existing":[],"claim_detail_existing":[],
        "detail":{
            "claim":{"claim_id":"envoy-claim","status":"pending","audience_ref":null,"revision":1,
                "extracted_at":"2026-09-27T10:00:00Z","extracted_by":"envoy-reply-capture"},
            "utterance_context":{"transcript_key":"envoy:session","segment_key":"message",
                "speaker":"envoy","stated_text":"The exact words"}
        }
    });
    let plan = program.plan(&env(&input, &context)).unwrap();
    assert_eq!(plan.operations.len(), 3);
    let payload = &plan
        .operations
        .iter()
        .find(|op| op["entity"] == "claim_detail")
        .unwrap()["payload"];
    assert_eq!(payload["claim_text"], "The exact words");
    assert_eq!(payload["transcript_id"], "envoy:session");
    assert_eq!(payload["audience_kind"], Value::Null);
    assert_eq!(payload["audience_id"], Value::Null);
}

#[test]
fn envoy_claims_sync_persists_empty_scan_continuation_and_refreshes_selected_summary() {
    let program = load("claims_review", "sync-claims");
    let input = json!({"after_claim_id":"previous", "claim_id":"selected"});
    let selected = json!({"claim_id":"selected","status":"confirmed","revision":9,
        "audience_ref":null,"extracted_at":"2026-09-27T10:00:00Z"});
    let mut context = json!({
        "claims":{"claims":[],"count":0,"scanned":100,"scan_truncated":true,"next_cursor":"continue-here"},
        "selected_summary_existing":[{"record_id":"stored-selected","record_revision":3,"fields":{"claim_id":"selected"}}],
        "claim_sync_page_existing":[], "claim_detail_existing":[], "claim_summary_existing":[],
        "detail":{"claim":selected,"utterance_context":{"transcript_key":"envoy:session","segment_key":"message","speaker":"envoy","stated_text":"All exact words"}}
    });
    let plan = program.plan(&env(&input, &context)).unwrap();
    let page = &plan
        .operations
        .iter()
        .find(|op| op["entity"] == "claim_sync_page")
        .unwrap()["payload"];
    assert_eq!(page["next_cursor"], "continue-here");
    assert_eq!(page["after_claim_id"], "previous");
    assert_eq!(page["claim_ids_json"], "[]");
    let summary = plan
        .operations
        .iter()
        .find(|op| op["entity"] == "claim_summary")
        .unwrap();
    assert_eq!(summary["kind"], "update");
    assert_eq!(summary["record_id"], "stored-selected");
    assert_eq!(summary["patch"]["status"], "confirmed");
    assert_eq!(summary["patch"]["expected_revision"], 9);
    // If the list and detail overlap, the later detail wins once; two writes to
    // the same row in one transaction must never compete on its revision.
    context["claims"]["claims"] = json!([{"claim_id":"selected","status":"pending","revision":8,
        "speaker":"envoy","stated_text":"All exact words","extracted_at":"2026-09-27T10:00:00Z"}]);
    let plan = program.plan(&env(&input, &context)).unwrap();
    let summaries: Vec<_> = plan
        .operations
        .iter()
        .filter(|op| op["entity"] == "claim_summary")
        .collect();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0]["patch"]["status"], "confirmed");
}
