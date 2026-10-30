use super::*;

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-12T10:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

#[test]
fn memory_lifecycle_exact_replay_does_not_inflate_evidence() {
    let item = json!({"key":"units","value":"Celsius","source_type":"explicit_user_statement"});
    let first = merge_items(&[], &[item.clone()], now());
    let again = merge_items(&first, &[item], now());
    assert_eq!(again.len(), 1);
    assert_eq!(independent_observations(&again[0]), 1);
}

#[test]
fn memory_lifecycle_short_review_references_bind_to_offered_snapshots_only() {
    let (mut doc, old, new) = document(
        json!({"key":"home","value":"Delhi","source_type":"explicit_user_statement"}),
        json!({"key":"home","value":"Mumbai","source_type":"explicit_user_statement"}),
    );
    let mut plan = review(&old, Relation::Supersede, Kind::Fact);
    plan.relationships[0].existing_id = "m1".into();
    let bound = runtime::bind_review_references(plan.clone(), &[old.clone()]).unwrap();
    assert_eq!(bound.relationships[0].existing_id, old.id);
    apply_review(&mut doc, &new, &[old.clone()], &bound, now()).unwrap();
    assert!(retired(&doc["preferences"][0]));
    assert_eq!(state(&doc["preferences"][1]), "active");
    for forged in ["m0", "m2", "incoming", old.id.as_str()] {
        plan.relationships[0].existing_id = forged.into();
        assert!(runtime::bind_review_references(plan.clone(), &[old.clone()]).is_err());
    }
}

#[test]
fn memory_lifecycle_null_context_is_absent_context_not_invalid_memory() {
    let mut reply = json!({"kind":"fact","subject":"brother","aspect":"diet","context":null,
        "valid_until":null,"validity_quote":null,"relationships":[]});
    assert_eq!(
        serde_json::from_value::<Review>(reply.clone())
            .unwrap()
            .context,
        ""
    );
    reply["context"] = json!(123);
    assert!(serde_json::from_value::<Review>(reply).is_err());
}

#[test]
fn memory_lifecycle_legacy_schema_echo_is_quarantined_without_losing_its_history() {
    let schema = json!({"allowed_user_tiers":["preferences","skills"],"promotion_rule":"Promote facts",
        "preferences":{"type":"collection","item_schema":{"key":"stable key","value":"memory"}}});
    let mut document = json!({"preferences":schema,"identity":[{"key":"city","value":"Pune"}]});
    assert_eq!(quarantine_legacy_schema_echoes(&mut document, now()), 1);
    normalize_legacy_collections(&mut document, now());
    assert_eq!(sources(&document).len(), 1);
    assert_eq!(document["identity"][0]["value"], "Pune");
    assert_eq!(
        document[JOURNAL]["quarantined_schemas"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap()["value"],
        schema
    );
    assert_eq!(quarantine_legacy_schema_echoes(&mut document, now()), 0);
}

#[test]
fn memory_lifecycle_key_collision_preserves_both_until_review() {
    let old = json!({"key":"city","value":"Delhi"});
    let new = json!({"key":"city","value":"Mumbai"});
    let merged = merge_items(&[old], &[new], now());
    assert_eq!(merged.len(), 2);
    assert_eq!(state(&merged[1]), "pending_review");
}

#[test]
fn memory_lifecycle_primitive_bulk_items_cannot_bypass_review() {
    let items = merge_collection(
        &[],
        &[
            json!("Prefer short answers"),
            json!("Prefer detailed answers"),
        ],
        "preferences",
        now(),
    );
    assert_eq!(items.len(), 2);
    assert!(items
        .iter()
        .all(|item| item.is_object() && state(item) == "pending_review"));
    let replay = merge_collection(
        &items,
        &[json!("Prefer short answers")],
        "preferences",
        now(),
    );
    assert_eq!(replay.len(), 2);
}

#[test]
fn memory_lifecycle_app_envelopes_keep_their_separate_eligibility_path() {
    let app = json!({"key":"app-record","value":"A source-backed memory",
        "app_source_eligibility":{"candidate_id":"app-record","candidate_revision":"r1"}});
    let items = merge_collection(&[], &[app], "preferences", now());
    assert_eq!(state(&items[0]), "active");
    assert_eq!(items[0]["memory_review_needed"], false);
    assert!(items[0].get("app_source_eligibility").is_some());
}

fn document(old: Value, new: Value) -> (Value, Source, Source) {
    let document = json!({"preferences":merge_items(&[old],&[new],now())});
    let offered = sources(&document);
    (document, offered[0].clone(), offered[1].clone())
}

fn review(existing: &Source, relation: Relation, kind: Kind) -> Review {
    Review {
        kind,
        subject: "owner".into(),
        aspect: "preference".into(),
        context: "standing".into(),
        valid_until: None,
        validity_quote: None,
        relationships: vec![Relationship {
            existing_id: existing.id.clone(),
            relation,
            incoming_coverage: IncomingCoverage::Unknown,
            same_subject_and_aspect: true,
            same_context: true,
            explicit_correction: true,
            confidence: 0.99,
            rationale: "Evidence establishes the relationship".into(),
            question: Some("Has this preference changed, or is this temporary?".into()),
        }],
    }
}

#[test]
fn memory_lifecycle_partial_or_unknown_support_preserves_complete_statements() {
    for (before, after) in [
        (
            "I use Python.",
            "I use Python and also maintain Rust services.",
        ),
        (
            "My workday starts at 09:00.",
            "My workday starts at 09:00; on Fridays I finish at 13:00.",
        ),
        (
            "Send invoices to finance.",
            "Send invoices to finance and copy the project owner.",
        ),
    ] {
        for relation in [Relation::Duplicate, Relation::Reinforce] {
            for coverage in [IncomingCoverage::Partial, IncomingCoverage::Unknown] {
                let (mut doc, old, new) = document(
                    json!({"key":"earlier","value":before,"source_type":"explicit_user_statement"}),
                    json!({"key":"additional","value":after,"source_type":"explicit_user_statement"}),
                );
                let mut plan = review(&old, relation, Kind::Fact);
                plan.relationships[0].incoming_coverage = coverage;
                let applied = apply_review(&mut doc, &new, &[old], &plan, now()).unwrap();
                assert!(applied.applied);
                assert!(applied.conflicts.is_empty());
                assert_eq!(sources(&doc).len(), 2);
                assert_eq!(text(&doc["preferences"][0]), before);
                assert_eq!(text(&doc["preferences"][1]), after);
                assert!(doc["preferences"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|i| state(i) == "active"));
                assert_eq!(independent_observations(&doc["preferences"][0]), 1);
            }
        }
    }
}

#[test]
fn memory_lifecycle_full_coverage_still_deduplicates_paraphrases() {
    for relation in [Relation::Duplicate, Relation::Reinforce] {
        let (mut doc, old, new) = document(
            json!({"key":"earlier","value":"Prefer brief replies.","source_type":"explicit_user_statement"}),
            json!({"key":"restated","value":"Keep responses concise.","source_type":"explicit_user_statement"}),
        );
        let mut plan = review(&old, relation, Kind::Preference);
        plan.relationships[0].incoming_coverage = IncomingCoverage::Full;
        apply_review(&mut doc, &new, &[old], &plan, now()).unwrap();
        assert_eq!(sources(&doc).len(), 1);
        assert_eq!(state(&doc["preferences"][0]), "active");
        assert!(retired(&doc["preferences"][1]));
        assert_eq!(independent_observations(&doc["preferences"][0]), 2);
        assert_eq!(text(&doc["preferences"][1]), "Keep responses concise.");
    }
}

#[test]
fn memory_lifecycle_literal_identity_does_not_require_a_coverage_guess() {
    let (mut doc, old, new) = document(
        json!({"key":"earlier","value":"Use metric units.","source_type":"explicit_user_statement"}),
        json!({"key":"restated","value":"Use metric units.","source_type":"explicit_user_statement"}),
    );
    let plan = review(&old, Relation::Duplicate, Kind::Instruction);
    assert_eq!(
        plan.relationships[0].incoming_coverage,
        IncomingCoverage::Unknown
    );
    apply_review(&mut doc, &new, &[old], &plan, now()).unwrap();
    assert!(retired(&doc["preferences"][1]));
}

#[test]
fn memory_lifecycle_full_coverage_cannot_erase_declared_validity_boundaries() {
    for swap in [false, true] {
        let mut old = json!({"key":"earlier","value":"Available for evening support.","source_type":"explicit_user_statement"});
        let mut new = json!({"key":"additional","value":"I can cover evening support.","source_type":"explicit_user_statement"});
        if swap {
            old["valid_until"] = json!("2026-09-14T00:00:00Z");
        } else {
            new["valid_until"] = json!("2026-09-14T00:00:00Z");
        }
        let (mut doc, old, new) = document(old, new);
        let mut plan = review(&old, Relation::Duplicate, Kind::Fact);
        plan.relationships[0].incoming_coverage = IncomingCoverage::Full;
        apply_review(&mut doc, &new, &[old], &plan, now()).unwrap();
        assert!(doc["preferences"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| state(i) == "active"));
    }
}

#[test]
fn memory_lifecycle_partial_support_can_relate_to_multiple_old_claims() {
    let old = vec![
        json!({"key":"format","value":"Use metric units.","source_type":"explicit_user_statement"}),
        json!({"key":"length","value":"Prefer brief replies.","source_type":"explicit_user_statement"}),
    ];
    let new = json!({"key":"instructions","value":"Use metric units and brief replies, and include source links.","source_type":"explicit_user_statement"});
    let mut doc = json!({"preferences":merge_items(&old, &[new], now())});
    let all = sources(&doc);
    let mut plan = review(&all[0], Relation::Reinforce, Kind::Instruction);
    plan.relationships[0].incoming_coverage = IncomingCoverage::Partial;
    plan.relationships.push(Relationship {
        existing_id: all[1].id.clone(),
        ..plan.relationships[0].clone()
    });
    apply_review(&mut doc, &all[2], &all[0..2], &plan, now()).unwrap();
    assert_eq!(sources(&doc).len(), 3);
    assert!(doc["preferences"]
        .as_array()
        .unwrap()
        .iter()
        .all(|i| state(i) == "active"));
}

#[test]
fn memory_lifecycle_cross_key_correction_keeps_audit_history() {
    let (mut doc, old, new) = document(
        json!({"key":"city","value":"Delhi","source_type":"explicit_user_statement"}),
        json!({"key":"residence","value":"Mumbai","source_type":"explicit_user_statement"}),
    );
    apply_review(
        &mut doc,
        &new,
        &[old.clone()],
        &review(&old, Relation::Supersede, Kind::Fact),
        now(),
    )
    .unwrap();
    assert_eq!(doc["preferences"].as_array().unwrap().len(), 2);
    assert_eq!(state(&doc["preferences"][0]), "superseded");
    assert_eq!(doc["preferences"][0]["value"], "Delhi");
    assert_eq!(state(&doc["preferences"][1]), "active");
}

#[test]
fn memory_lifecycle_model_cannot_retire_explicit_goal_from_observations() {
    let observed = json!({"key":"cake","value":"Ordered cake","source_type":"observed_behavior",
        "memory_evidence":[{"id":"one","at":"2026-09-01T10:00:00Z"},{"id":"two","at":"2026-09-02T10:00:00Z"},{"id":"three","at":"2026-09-03T10:00:00Z"}]});
    let (mut doc, old, new) = document(
        json!({"key":"goal","value":"Eat less sugar","source_type":"explicit_user_statement"}),
        observed,
    );
    let outcome = apply_review(
        &mut doc,
        &new,
        &[old.clone()],
        &review(&old, Relation::Supersede, Kind::Observation),
        now(),
    )
    .unwrap();
    assert!(!retired(&doc["preferences"][0]));
    assert_eq!(outcome.conflicts.len(), 1);
    assert_eq!(state(&doc["preferences"][0]), "unresolved");
}

#[test]
fn memory_lifecycle_inferred_pattern_needs_independent_evidence_over_time() {
    for (count, want) in [(1, "pending_review"), (2, "pending_review"), (3, "active")] {
        let evidence: Vec<_> = (0..count)
            .map(|i| {
                json!({"id":format!("event-{i}"),
            "at":(now()-chrono::Duration::days(i)).to_rfc3339()})
            })
            .collect();
        let (mut doc, old, new) = document(
            json!({"key":"habit","value":"Works at 8","source_type":"inferred"}),
            json!({"key":"new_habit","value":"Works at 10","source_type":"inferred","memory_evidence":evidence}),
        );
        let outcome = apply_review(
            &mut doc,
            &new,
            &[old.clone()],
            &review(&old, Relation::Supersede, Kind::Observation),
            now(),
        )
        .unwrap();
        assert_eq!(state(&doc["preferences"][1]), want);
        assert_eq!(retired(&doc["preferences"][0]), count == 3);
        assert!(outcome.conflicts.is_empty());
    }
}

#[test]
fn memory_lifecycle_owner_question_requires_concrete_text_before_mutation() {
    for question in [
        None,
        Some(String::new()),
        Some(" \n ".into()),
        Some("?".repeat(601)),
    ] {
        let (mut doc, old, new) = document(
            json!({"key":"earlier","value":"Prefer a quiet room.","source_type":"explicit_user_statement"}),
            json!({"key":"later","value":"Prefer background music.","source_type":"explicit_user_statement"}),
        );
        let before = doc.clone();
        let mut plan = review(&old, Relation::Clarify, Kind::Preference);
        plan.relationships[0].question = question;
        assert!(apply_review(&mut doc, &new, &[old], &plan, now())
            .unwrap_err()
            .contains("concrete question"));
        assert_eq!(doc, before);
    }
}

#[test]
fn memory_lifecycle_weak_inference_cannot_bypass_evidence_floor_with_clarification() {
    for (count, span, mature) in [
        (1_i64, 0_i64, false),
        (2, 3, false),
        (3, 0, false),
        (3, 1, false),
        (3, 2, true),
    ] {
        for same_context in [true, false] {
            let evidence: Vec<_> = (0..count)
                .map(|i| json!({"id":format!("device-event-{i}"),
                    "at":(now()-chrono::Duration::days(i * span / (count - 1).max(1))).to_rfc3339()}))
                .collect();
            let (mut doc, old, new) = document(
                json!({"key":"usual_device","value":"Usually works on a laptop.","source_type":"inferred"}),
                json!({"key":"changed_device","value":"Now usually works on a tablet.","source_type":"inferred","memory_evidence":evidence}),
            );
            let mut plan = review(&old, Relation::Clarify, Kind::Observation);
            plan.relationships[0].same_context = same_context;
            let applied = apply_review(&mut doc, &new, &[old], &plan, now()).unwrap();
            assert_eq!(applied.conflicts.len(), usize::from(mature));
            assert_eq!(
                state(&doc["preferences"][0]),
                if mature { "unresolved" } else { "active" }
            );
            assert_eq!(
                state(&doc["preferences"][1]),
                if mature {
                    "unresolved"
                } else {
                    "pending_review"
                }
            );
            assert_eq!(doc["preferences"][1]["memory_review_needed"], false);
        }
    }
}

#[test]
fn memory_lifecycle_weak_inferred_change_does_not_ask_when_model_disagrees_on_context() {
    let (mut doc, old, new) = document(
        json!({"key":"work","value":"Starts at 8","source_type":"inferred"}),
        json!({"key":"work","value":"Starts at 10","source_type":"inferred"}),
    );
    let mut plan = review(&old, Relation::Supersede, Kind::Observation);
    plan.relationships[0].same_context = false;
    let applied = apply_review(&mut doc, &new, &[old], &plan, now()).unwrap();
    assert!(applied.conflicts.is_empty());
    assert_eq!(state(&doc["preferences"][0]), "active");
    assert_eq!(state(&doc["preferences"][1]), "pending_review");
}

#[test]
fn memory_lifecycle_copies_do_not_count_as_independent_observations() {
    let one = json!({"key":"habit","value":"Works at 10","source_type":"inferred",
        "root_source_id":"original","source_id":"copy-one","updated_at":"2026-09-01T00:00:00Z"});
    let mut two = one.clone();
    two["source_id"] = json!("copy-two");
    two["updated_at"] = json!("2026-09-12T00:00:00Z");
    let items = merge_items(&[one], &[two], now());
    assert_eq!(items.len(), 1);
    assert_eq!(independent_observations(&items[0]), 1);
    assert_eq!(evidence(&items[0])[0]["at"], "2026-09-01T00:00:00Z");
}

#[test]
fn memory_recall_citation_merge_preserves_quotes_without_inflating_observation_span() {
    let first = json!({"key":"city","value":"Mumbai","source_type":"inferred_pattern","memory_evidence":[
        {"id":"one","source_role":"user","quote":"I live in Mumbai.","at":"2026-09-01T00:00:00Z"},
        {"id":"two","source_role":"user","quote":"Mumbai is still my home.","at":"2026-09-02T00:00:00Z"}
    ]});
    let incoming = json!({"key":"city","value":"Mumbai","source_type":"inferred_pattern","memory_evidence":[
        {"id":"one","source_role":"assistant","quote":"Paris was a suggestion.","at":"2026-10-01T00:00:00Z"},
        {"id":"one","source_role":"user","quote":"I live in Mumbai.","at":"2026-10-01T00:00:00Z"}
    ]});
    let merged = merge_items(&[first], &[incoming.clone()], now());
    assert_eq!(merged.len(), 1);
    assert_eq!(evidence(&merged[0]).len(), 3);
    assert_eq!(independent_observations(&merged[0]), 2);
    assert_eq!(observation_span_days(&merged[0]), 1);
    assert_eq!(merged[0]["memory_review_needed"], true);
    assert!(evidence(&merged[0])
        .iter()
        .any(|e| e["quote"] == "I live in Mumbai." && e["at"] == "2026-09-01T00:00:00Z"));
    let mut settled = merged.clone();
    settled[0]
        .as_object_mut()
        .unwrap()
        .remove("memory_review_needed");
    let repeated = merge_items(&settled, &[incoming], now());
    assert_eq!(repeated[0]["memory_evidence"], merged[0]["memory_evidence"]);
    assert_eq!(observation_span_days(&repeated[0]), 1);
    assert!(repeated[0].get("memory_review_needed").is_none());
}

#[test]
fn memory_lifecycle_project_boundaries_override_model_similarity() {
    let (mut doc, old, new) = document(
        json!({"key":"database","value":"Postgres","project_id":"a","source_type":"explicit_user_statement"}),
        json!({"key":"database","value":"SQLite","project_id":"b","source_type":"explicit_user_statement"}),
    );
    apply_review(
        &mut doc,
        &new,
        &[old.clone()],
        &review(&old, Relation::Supersede, Kind::Instruction),
        now(),
    )
    .unwrap();
    assert_eq!(state(&doc["preferences"][0]), "active");
    assert_eq!(state(&doc["preferences"][1]), "active");
}

#[test]
fn memory_lifecycle_declared_scope_and_subject_cannot_be_overridden_by_review() {
    for (field, one, two) in [
        (
            "scope",
            json!({"topics":["work"]}),
            json!({"topics":["family"]}),
        ),
        ("subject", json!("owner"), json!("brother")),
    ] {
        let mut old =
            json!({"key":"language","value":"Use English","source_type":"explicit_user_statement"});
        let mut new =
            json!({"key":"language","value":"Use Hindi","source_type":"explicit_user_statement"});
        old[field] = one;
        new[field] = two;
        let (mut doc, old, new) = document(old, new);
        apply_review(
            &mut doc,
            &new,
            &[old.clone()],
            &review(&old, Relation::Supersede, Kind::Instruction),
            now(),
        )
        .unwrap();
        assert!(doc["preferences"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| state(item) == "active"));
    }
}

#[test]
fn memory_lifecycle_expiry_preserves_standing_preference() {
    let mut doc = json!({"preferences":[
        {"key":"budget","value":"150 dollars"},
        {"key":"trip","value":"300 dollars for this trip","valid_until":"2026-09-11T00:00:00Z"}
    ]});
    assert_eq!(expire(&mut doc, now()), 1);
    assert_eq!(state(&doc["preferences"][0]), "active");
    assert_eq!(state(&doc["preferences"][1]), "expired");
    assert_eq!(expire(&mut doc, now()), 0);
}

#[test]
fn memory_lifecycle_malformed_review_and_stale_snapshot_do_not_apply() {
    let (mut doc, old, new) = document(
        json!({"key":"city","value":"Delhi"}),
        json!({"key":"city","value":"Mumbai"}),
    );
    let before = doc.clone();
    let mut invalid = review(&old, Relation::Supersede, Kind::Fact);
    invalid.relationships[0].existing_id = "invented-id".into();
    assert!(apply_review(&mut doc, &new, &[old.clone()], &invalid, now()).is_err());
    assert_eq!(doc, before);
    doc["preferences"][0]["value"] = json!("Pune");
    let fresh = doc.clone();
    let result = apply_review(
        &mut doc,
        &new,
        &[old.clone()],
        &review(&old, Relation::Supersede, Kind::Fact),
        now(),
    )
    .unwrap();
    assert!(result.stale);
    assert_eq!(doc, fresh);
}

fn conflicted() -> (Value, String) {
    let (mut doc, old, new) = document(
        json!({"key":"exercise","value":"Morning","source_type":"explicit_user_statement"}),
        json!({"key":"pattern","value":"Evening","source_type":"observed_behavior"}),
    );
    let result = apply_review(
        &mut doc,
        &new,
        &[old.clone()],
        &review(&old, Relation::Clarify, Kind::Observation),
        now(),
    )
    .unwrap();
    (doc, result.conflicts[0].id.clone())
}

#[test]
fn memory_lifecycle_owner_answer_resolves_once_and_dismissal_preserves_uncertainty() {
    let (mut doc, id) = conflicted();
    assert!(answer_conflict(&mut doc, &id, "keep_existing", None, now()).unwrap());
    assert_eq!(state(&doc["preferences"][0]), "active");
    assert_eq!(state(&doc["preferences"][1]), "superseded");
    let settled = doc.clone();
    assert!(!answer_conflict(&mut doc, &id, "use_new", None, now()).unwrap());
    assert_eq!(doc, settled);
    let (mut doc, id) = conflicted();
    assert!(answer_conflict(&mut doc, &id, "dismiss", None, now()).unwrap());
    assert_eq!(doc[JOURNAL]["conflicts"][&id]["state"], "dismissed");
    assert!(doc["preferences"]
        .as_array()
        .unwrap()
        .iter()
        .all(|v| state(v) == "unresolved"));
}

#[test]
fn memory_lifecycle_new_evidence_invalidates_older_question() {
    let (mut doc, id) = conflicted();
    doc["preferences"][1]["value"] = json!("My exercise preference changed again");
    assert!(!answer_conflict(&mut doc, &id, "use_new", None, now()).unwrap());
    assert_eq!(doc[JOURNAL]["conflicts"][&id]["state"], "stale");
    assert!(!retired(&doc["preferences"][0]));
}

#[test]
fn memory_lifecycle_unrelated_write_does_not_invalidate_question() {
    let (mut doc, id) = conflicted();
    doc["preferences"]
        .as_array_mut()
        .unwrap()
        .push(json!({"key":"units","value":"Celsius"}));
    assert!(answer_conflict(&mut doc, &id, "keep_existing", None, now()).unwrap());
    assert_eq!(doc[JOURNAL]["conflicts"][&id]["state"], "resolved");
}

#[test]
fn memory_lifecycle_free_text_must_reconcile_cited_memories() {
    let (mut doc, id) = conflicted();
    answer_conflict(
        &mut doc,
        &id,
        "answer",
        Some("Evenings are temporary; I still prefer mornings."),
        now(),
    )
    .unwrap();
    let all = sources(&doc);
    let incoming = all.last().unwrap();
    let original = &all[0..2];
    let mut plan = review(&original[0], Relation::Coexist, Kind::Preference);
    assert!(apply_review(&mut doc, incoming, original, &plan, now()).is_err());
    plan.relationships.push(Relationship {
        existing_id: original[1].id.clone(),
        ..plan.relationships[0].clone()
    });
    apply_review(&mut doc, incoming, original, &plan, now()).unwrap();
    assert_eq!(doc[JOURNAL]["conflicts"][&id]["state"], "resolved");
    assert!(doc["preferences"]
        .as_array()
        .unwrap()
        .iter()
        .all(|v| state(v) == "active"));
}

#[test]
fn memory_lifecycle_clarification_retains_qualifiers_when_review_says_duplicate() {
    let (mut doc, old, new) = document(
        json!({"key":"exercise","value":"Morning","source_type":"explicit_user_statement",
            "valid_until":"2026-12-31T00:00:00Z"}),
        json!({"key":"pattern","value":"Evening","source_type":"observed_behavior"}),
    );
    let result = apply_review(
        &mut doc,
        &new,
        &[old.clone()],
        &review(&old, Relation::Clarify, Kind::Observation),
        now(),
    )
    .unwrap();
    let id = &result.conflicts[0].id;
    let answer = "I still prefer mornings; evenings are temporary during school holidays.";
    answer_conflict(&mut doc, id, "answer", Some(answer), now()).unwrap();
    let all = sources(&doc);
    let incoming = all.last().unwrap();
    let mut plan = review(&all[0], Relation::Duplicate, Kind::Preference);
    plan.relationships.push(Relationship {
        existing_id: all[1].id.clone(),
        relation: Relation::Coexist,
        ..plan.relationships[0].clone()
    });
    let mut unexplained_request = plan.clone();
    unexplained_request.relationships[1].relation = Relation::Clarify;
    unexplained_request.relationships[1].question = None;
    let before = doc.clone();
    assert!(apply_review(&mut doc, incoming, &all[0..2], &unexplained_request, now()).is_err());
    assert_eq!(doc, before);
    apply_review(&mut doc, incoming, &all[0..2], &plan, now()).unwrap();
    assert_eq!(doc[JOURNAL]["conflicts"][id]["state"], "resolved");
    assert!(retired(&doc["preferences"][0]));
    assert_eq!(doc["preferences"][0]["superseded_by"], incoming.id);
    assert_eq!(state(&doc["preferences"][2]), "active");
    assert_eq!(text(&doc["preferences"][2]), answer);
    assert_eq!(doc["preferences"][2]["valid_until"], "2026-12-31T00:00:00Z");
}

#[tokio::test]
async fn memory_lifecycle_concurrent_ingress_preserves_independent_writes() {
    use crate::magician_v2::{
        agents::AgentMemoryResolver, chat::service::merge_user_memory_tier_fields,
    };
    let root = tempfile::tempdir().unwrap();
    let resolver = AgentMemoryResolver::new(root.path());
    let a = serde_json::Map::from_iter([("one".into(), json!("first"))]);
    let b = serde_json::Map::from_iter([("two".into(), json!("second"))]);
    let (a, b) = tokio::join!(
        merge_user_memory_tier_fields(&resolver, "p", "w", "preferences", &a),
        merge_user_memory_tier_fields(&resolver, "p", "w", "preferences", &b)
    );
    assert_eq!(a["status"], "ok");
    assert_eq!(b["status"], "ok");
    let document = resolver
        .resolve_for_scope("p", "w")
        .unwrap()
        .load_user_knowledge()
        .await
        .unwrap();
    assert_eq!(document["preferences"].as_array().unwrap().len(), 2);
    assert!(resolver
        .resolve_for_scope("other", "w")
        .unwrap()
        .load_user_knowledge()
        .await
        .unwrap()
        .get("preferences")
        .is_none());
}

#[tokio::test]
async fn memory_lifecycle_provider_failure_malformed_and_budget_survive_reopen() {
    // Passes without a router read the global decision host, which other
    // tests switch on; it must stay off for these.
    let _serial = crate::magician_v2::decisions::test_engine::serial().await;
    use crate::magician_v2::agents::AgentMemoryResolver;
    use crate::magician_v2::query_analysis::operation_llm_router::SimplifiedLLMResponse;
    let root = tempfile::tempdir().unwrap();
    let resolver = AgentMemoryResolver::new(root.path());
    let service = resolver.resolve_for_scope("p", "w").unwrap();
    let initial =
        json!({"preferences":merge_items(&[], &[json!({"key":"city","value":"Pune"})], now())});
    service.save_user_knowledge(&initial).await.unwrap();
    let unbound = runtime::pass(&service, None, None, now(), None)
        .await
        .unwrap();
    assert_eq!(unbound.reviewed, 0);
    assert_eq!(service.load_user_knowledge().await.unwrap(), initial);
    let failed = runtime::pass_with_provider(
        &service,
        None,
        now(),
        None,
        Some(|_| async { anyhow::bail!("synthetic provider outage") }),
    )
    .await
    .unwrap();
    assert_eq!(failed.reviewed, 1);
    assert_eq!(failed.applied, 0);
    assert!(failed.error.unwrap().contains("outage"));
    drop(service);
    let service = AgentMemoryResolver::new(root.path())
        .resolve_for_scope("p", "w")
        .unwrap();
    let same_time = runtime::pass_with_provider(
        &service,
        None,
        now(),
        None,
        Some(|_| async {
            panic!("restart must retain cooldown");
            #[allow(unreachable_code)]
            Ok(SimplifiedLLMResponse::default())
        }),
    )
    .await
    .unwrap();
    assert_eq!(same_time.reviewed, 0);
    let invalid = runtime::pass_with_provider(
        &service,
        None,
        now() + chrono::Duration::minutes(16),
        None,
        Some(|_| async {
            Ok(SimplifiedLLMResponse {
                content: "{broken JSON".into(),
                ..Default::default()
            })
        }),
    )
    .await
    .unwrap();
    assert_eq!(invalid.applied, 0);
    assert!(invalid.error.unwrap().contains("invalid memory review"));
    let document = service.load_user_knowledge().await.unwrap();
    assert_eq!(document["preferences"], initial["preferences"]);
    assert_eq!(document[JOURNAL]["calls"].as_array().unwrap().len(), 2);
}

#[test]
fn memory_lifecycle_durable_request_restart_scope_answer_and_replay() {
    use crate::magician_v2::{
        agents::AgentMemoryResolver,
        artifact_v2::workspace::ArtifactV2Workspace,
        realtime_events::RuntimeTransportBroadcaster,
        user_requests::{ScopedResponseResult, UserRequestService, UserResponse},
    };
    fn phase<T>(future: impl std::future::Future<Output = T>) -> T {
        // Ending a runtime also ends detached writers, as a process restart does.
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }
    async fn requests(root: &std::path::Path) -> UserRequestService {
        UserRequestService::new(std::sync::Arc::new(RuntimeTransportBroadcaster::new(16)))
            .with_workspace_layout(ArtifactV2Workspace::new(root))
            .with_history_persist_path(root.join("history.json"))
            .with_pending_persist_path(root.join("pending.json"))
            .await
    }
    for decision in ["keep_existing", "dismiss"] {
        let root = tempfile::tempdir().unwrap();
        let resolver =
            AgentMemoryResolver::with_workspace_layout(ArtifactV2Workspace::new(root.path()));
        let service = resolver.resolve_for_scope("p", "w").unwrap();
        let (document, conflict_id) = conflicted();
        phase(async {
            service.save_user_knowledge(&document).await.unwrap();
            let request = requests(root.path()).await;
            assert_eq!(
                runtime::reconcile_questions(&service, &request, now())
                    .await
                    .unwrap(),
                1
            );
        });
        phase(async {
            let request = requests(root.path()).await;
            let answer = UserResponse {
                request_id: runtime::request_id("p", "w", &conflict_id),
                decision: decision.into(),
                input: None,
                channel: "test".into(),
                sensitive: Vec::new(),
            };
            let wrong = request
                .respond_scoped(answer.clone(), Some("other"), Some("w"))
                .await;
            assert!(!matches!(wrong, ScopedResponseResult::Accepted));
            assert_eq!(service.load_user_knowledge().await.unwrap(), document);
            assert!(matches!(
                request.respond_scoped(answer, Some("p"), Some("w")).await,
                ScopedResponseResult::Accepted
            ));
        });
        phase(async {
            let request = requests(root.path()).await;
            runtime::reconcile_questions(&service, &request, now())
                .await
                .unwrap();
            let settled = service.load_user_knowledge().await.unwrap();
            assert_eq!(
                settled[JOURNAL]["conflicts"][&conflict_id]["state"],
                if decision == "dismiss" {
                    "dismissed"
                } else {
                    "resolved"
                }
            );
            runtime::reconcile_questions(&service, &request, now())
                .await
                .unwrap();
            assert_eq!(service.load_user_knowledge().await.unwrap(), settled);
            assert_eq!(
                runtime::pass(&service, None, Some(&request), now(), None)
                    .await
                    .unwrap()
                    .questions,
                0
            );
        });
    }
}

#[test]
fn memory_lifecycle_retirement_replay_and_explicit_restoration() {
    let old = json!({"key":"units","value":"Celsius","source_type":"explicit_user_statement", "source_event_id":"first", "updated_at":now().to_rfc3339()});
    let mut items = merge_items(&[], &[old.clone()], now());
    items[0]["memory_lifecycle"] = json!("retracted");
    items[0]["retired_at"] = json!(now().to_rfc3339());
    assert_eq!(merge_items(&items, &[old.clone()], now()), items);
    let mut fresh = old;
    fresh["source_event_id"] = json!("second");
    fresh["updated_at"] = json!((now() + chrono::Duration::days(1)).to_rfc3339());
    let restored = merge_items(&items, &[fresh.clone()], now() + chrono::Duration::days(1));
    assert_eq!(restored.len(), 2);
    assert!(retired(&restored[0]));
    assert_eq!(state(&restored[1]), "pending_review");
    assert_ne!(record_id(&restored[0]), record_id(&restored[1]));
    assert_eq!(
        merge_items(&restored, &[fresh], now() + chrono::Duration::days(1)),
        restored
    );
}

/// A pass with nothing to maintain and the hourly review budget spent must
/// not take the exclusive `knowledge.json` lock: every agent sweep used to,
/// and with ~30 agents per scope half of them timed out each tick.
#[tokio::test]
async fn memory_lifecycle_saturated_budget_pass_never_takes_the_knowledge_lock() {
    // Passes without a router read the global decision host, which other
    // tests switch on; it must stay off for these.
    let _serial = crate::magician_v2::decisions::test_engine::serial().await;
    use crate::magician_v2::agents::{
        storage::{override_file_lock_timeout_for_test, AgentStorage},
        AgentMemoryResolver,
    };
    use crate::magician_v2::query_analysis::operation_llm_router::SimplifiedLLMResponse;
    let root = tempfile::tempdir().unwrap();
    let service = AgentMemoryResolver::new(root.path())
        .resolve_for_scope("p", "w")
        .unwrap();
    let mut initial =
        json!({"preferences":merge_items(&[], &[json!({"key":"city","value":"Pune"})], now())});
    let spent: Vec<i64> = (1..=runtime::MAX_REVIEWS_PER_HOUR as i64)
        .map(|minutes| now().timestamp() - minutes * 60)
        .collect();
    initial[JOURNAL]["calls"] = json!(spent);
    service.save_user_knowledge(&initial).await.unwrap();

    let _short = override_file_lock_timeout_for_test(std::time::Duration::from_millis(300));
    let _held = AgentStorage::acquire_file_lock_exclusive(&service.storage().user_knowledge_path())
        .await
        .unwrap();
    let result = runtime::pass_with_provider(
        &service,
        None,
        now(),
        None,
        Some(|_| async {
            panic!("the review budget is spent; no review may run");
            #[allow(unreachable_code)]
            Ok(SimplifiedLLMResponse::default())
        }),
    )
    .await
    .expect("a pass with nothing to write must not need the lock");
    assert_eq!(result.reviewed, 0);
    assert_eq!(result.expired, 0);
}

/// Many concurrent maintenance-only passes over a large store, while another
/// file descriptor holds the exclusive lock, all complete: none of them needs
/// the lock, so none can time out on it.
#[tokio::test]
async fn memory_lifecycle_concurrent_passes_over_a_large_store_do_not_contend() {
    // Passes without a router read the global decision host, which other
    // tests switch on; it must stay off for these.
    let _serial = crate::magician_v2::decisions::test_engine::serial().await;
    use crate::magician_v2::agents::{
        storage::{override_file_lock_timeout_for_test, AgentStorage},
        AgentMemoryResolver,
    };
    let root = tempfile::tempdir().unwrap();
    let service = AgentMemoryResolver::new(root.path())
        .resolve_for_scope("p", "w")
        .unwrap();
    let items: Vec<Value> = (0..2000)
        .map(|index| {
            json!({"key":format!("fact-{index}"),
                "value":format!("A durable remembered detail number {index} with some padding text")})
        })
        .collect();
    let initial = json!({"preferences":merge_items(&[], &items, now())});
    service.save_user_knowledge(&initial).await.unwrap();
    // Settle any one-time maintenance first, as the first tick would.
    runtime::pass(&service, None, None, now(), None)
        .await
        .unwrap();
    let settled = service.load_user_knowledge().await.unwrap();

    let _short = override_file_lock_timeout_for_test(std::time::Duration::from_millis(500));
    let _held = AgentStorage::acquire_file_lock_exclusive(&service.storage().user_knowledge_path())
        .await
        .unwrap();
    let passes = (0..16).map(|_| runtime::pass(&service, None, None, now(), None));
    for result in futures_util::future::join_all(passes).await {
        let result = result.expect("no pass may time out on the knowledge lock");
        assert_eq!(result.expired, 0);
    }
    assert_eq!(service.load_user_knowledge().await.unwrap(), settled);
}

fn two_waiting_records() -> Value {
    json!({
        "preferences": merge_items(&[], &[json!({"key":"city","value":"Pune"})], now()),
        // Older, so the old oldest-first order would have picked it first.
        "workflows": merge_items(
            &[],
            &[json!({"key":"deploy","value":"Run the deploy script"})],
            now() - chrono::Duration::days(1),
        ),
    })
}

/// Runs passes `minutes` after `now()` with a provider that answers `reply`
/// (`None`: a timeout-like outage) and returns the record keys picked.
async fn passes(
    service: &crate::magician_v2::agents::AgentMemoryService,
    minutes: &[i64],
    reply: Option<&'static str>,
) -> Vec<String> {
    use crate::magician_v2::query_analysis::operation_llm_router::SimplifiedLLMResponse;
    let mut picked = Vec::new();
    for &offset in minutes {
        let mut observe = |observation: runtime::Observation| {
            picked.push(
                observation.incoming.item["key"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            );
        };
        let result = runtime::pass_with_provider(
            service,
            None,
            now() + chrono::Duration::minutes(offset),
            Some(&mut observe),
            Some(move |_| async move {
                match reply {
                    Some(content) => Ok(SimplifiedLLMResponse {
                        content: content.into(),
                        ..Default::default()
                    }),
                    None => anyhow::bail!("synthetic reviewer timeout"),
                }
            }),
        )
        .await
        .unwrap();
        assert_eq!(result.reviewed, 1);
    }
    picked
}

/// Growth-bounds plan 5.1: owner sections are reviewed before agent sections,
/// and reviewer outages never count as tries — three timeouts leave the
/// record queued.
#[tokio::test]
async fn memory_lifecycle_reviewer_timeouts_never_set_a_record_aside() {
    // Passes without a router read the global decision host, which other
    // tests switch on; it must stay off for these.
    let _serial = crate::magician_v2::decisions::test_engine::serial().await;
    use crate::magician_v2::agents::AgentMemoryResolver;
    let root = tempfile::tempdir().unwrap();
    let service = AgentMemoryResolver::new(root.path())
        .resolve_for_scope("p", "w")
        .unwrap();
    service
        .save_user_knowledge(&two_waiting_records())
        .await
        .unwrap();
    let picked = passes(&service, &[0, 16, 32, 48], None).await;
    assert_eq!(picked, vec!["city", "city", "city", "city"]);
    let city = &service.load_user_knowledge().await.unwrap()["preferences"][0];
    assert_eq!(city["memory_review_needed"], true);
    assert!(city.get("memory_review_deferred_at").is_none());
}

/// Three verdicts the reviewer could not use for a record set it aside
/// (still in service, no longer queued) so the next record gets its turn;
/// after `review.retry_after_days` it is queued again with fresh tries.
#[tokio::test]
async fn memory_lifecycle_record_specific_failures_set_aside_then_rearm() {
    // Passes without a router read the global decision host, which other
    // tests switch on; it must stay off for these.
    let _serial = crate::magician_v2::decisions::test_engine::serial().await;
    use crate::magician_v2::agents::AgentMemoryResolver;
    let root = tempfile::tempdir().unwrap();
    let service = AgentMemoryResolver::new(root.path())
        .resolve_for_scope("p", "w")
        .unwrap();
    service
        .save_user_knowledge(&two_waiting_records())
        .await
        .unwrap();
    let picked = passes(&service, &[0, 16, 32, 48], Some("{broken JSON")).await;
    assert_eq!(picked, vec!["city", "city", "city", "deploy"]);
    let document = service.load_user_knowledge().await.unwrap();
    let city = &document["preferences"][0];
    assert_eq!(city["memory_review_needed"], false);
    assert_eq!(city["memory_review_deferred_reason"], "max_attempts");
    // Set aside, not retired: it stays in service as it was.
    assert_eq!(state(city), "pending_review");
    assert!(!retired(city));

    // Fifteen days later the wait is over: it is queued (owner first) again.
    let fifteen_days = 15 * 24 * 60;
    let picked = passes(&service, &[fifteen_days], None).await;
    assert_eq!(picked, vec!["city"]);
    let city = &service.load_user_knowledge().await.unwrap()["preferences"][0];
    assert_eq!(city["memory_review_needed"], true);
    assert!(city.get("memory_review_deferred_reason").is_none());
}

/// Records from before the reviewer existed (neither review flag) count as
/// reviewed, and activity logs are never queued.
#[tokio::test]
async fn memory_lifecycle_pre_review_records_and_activity_logs_are_not_queued() {
    // Passes without a router read the global decision host, which other
    // tests switch on; it must stay off for these.
    let _serial = crate::magician_v2::decisions::test_engine::serial().await;
    use crate::magician_v2::agents::AgentMemoryResolver;
    use crate::magician_v2::query_analysis::operation_llm_router::SimplifiedLLMResponse;
    let root = tempfile::tempdir().unwrap();
    let service = AgentMemoryResolver::new(root.path())
        .resolve_for_scope("p", "w")
        .unwrap();
    let initial = json!({
        "workflows": [{"key":"legacy","value":"An old step from before review existed"}],
        "user": {"email_evidence": merge_items(
            &[],
            &[json!({"key":"email:work:2026-10-17","summary":"2 email threads"})],
            now(),
        )},
    });
    service.save_user_knowledge(&initial).await.unwrap();
    let result = runtime::pass_with_provider(
        &service,
        None,
        now(),
        None,
        Some(|_| async {
            panic!("nothing here is waiting for review");
            #[allow(unreachable_code)]
            Ok(SimplifiedLLMResponse::default())
        }),
    )
    .await
    .unwrap();
    assert_eq!(result.reviewed, 0);
}

/// Three saved copies of one preference: two already in memory, one incoming.
fn three_copies() -> (Value, Vec<Source>) {
    let document = json!({"preferences": merge_items(
        &[
            json!({"key":"brief","value":"Prefer brief replies.","source_type":"explicit_user_statement"}),
            json!({"key":"concise","value":"Keep responses concise.","source_type":"explicit_user_statement"}),
        ],
        &[json!({"key":"short","value":"Short answers, please.","source_type":"explicit_user_statement"})],
        now(),
    )});
    let all = sources(&document);
    (document, all)
}

fn merge_plan(all: &[Source], relations: [(Relation, f64); 2]) -> Review {
    let mut plan = review(&all[0], relations[0].0, Kind::Preference);
    plan.relationships[0].incoming_coverage = IncomingCoverage::Full;
    plan.relationships[0].confidence = relations[0].1;
    let mut second = plan.relationships[0].clone();
    second.existing_id = all[1].id.clone();
    second.relation = relations[1].0;
    second.confidence = relations[1].1;
    plan.relationships.push(second);
    plan
}

/// Growth fix 2026-10-19: a record that repeats two saved copies used to be
/// rejected ("a merge cannot also dispute another claim"), three times, and
/// set aside. It merges into the copy the reviewer is surest of, and the
/// other duplicate folds into that one too.
#[test]
fn memory_lifecycle_a_repeat_of_several_copies_merges_into_one_and_folds_the_rest() {
    let (mut doc, all) = three_copies();
    let plan = merge_plan(
        &all,
        [(Relation::Duplicate, 0.95), (Relation::Duplicate, 0.99)],
    );
    let outcome = apply_review(&mut doc, &all[2], &all[..2], &plan, now()).unwrap();
    assert!(outcome.applied);
    assert_eq!(outcome.folded, 1);
    let items = doc["preferences"].as_array().unwrap();
    assert_eq!(state(&items[1]), "active", "the surest copy is kept");
    assert!(retired(&items[0]));
    assert_eq!(items[0]["superseded_by"], all[1].id);
    assert_eq!(items[0]["supersession_reason"], "duplicate_of_merged_copy");
    assert!(retired(&items[2]));
    assert_eq!(items[2]["superseded_by"], all[1].id);
    assert_eq!(
        independent_observations(&items[1]),
        3,
        "it keeps every copy's evidence"
    );
    assert_eq!(
        text(&items[0]),
        "Prefer brief replies.",
        "a folded copy keeps its words"
    );
}

#[test]
fn memory_lifecycle_a_supporting_copy_stays_and_a_tie_keeps_the_closest() {
    let (mut doc, all) = three_copies();
    let plan = merge_plan(
        &all,
        [(Relation::Duplicate, 0.95), (Relation::Reinforce, 0.95)],
    );
    let outcome = apply_review(&mut doc, &all[2], &all[..2], &plan, now()).unwrap();
    assert_eq!(
        outcome.folded, 0,
        "a supporting record may say more; it stays"
    );
    let items = doc["preferences"].as_array().unwrap();
    assert_eq!(state(&items[0]), "active");
    assert_eq!(state(&items[1]), "active");
    assert_eq!(
        items[2]["superseded_by"], all[0].id,
        "on a tie the earliest offered"
    );
}

#[test]
fn memory_lifecycle_a_repeat_that_also_disputes_a_claim_is_still_rejected() {
    let (mut doc, all) = three_copies();
    let mut plan = merge_plan(
        &all,
        [(Relation::Duplicate, 0.99), (Relation::Clarify, 0.99)],
    );
    plan.relationships[1].incoming_coverage = IncomingCoverage::Unknown;
    let before = doc.clone();
    assert!(apply_review(&mut doc, &all[2], &all[..2], &plan, now()).is_err());
    assert_eq!(doc, before);
}

/// The live 2026-10-20 shape: one audio-editing preference saved again and
/// again under key variants (`…_preference`, `…_preferences`, `…_deliverables`,
/// `…_artifact_preference`) and small rewordings.
fn audio_preference(key: &str, value: &str, evidence: &str, at: &str) -> Value {
    json!({"key":key,"value":value,"source_type":"insight","memory_kind":"preference",
        "memory_context":"For audio-editing requests","updated_at":at,
        "memory_evidence":[{"id":evidence,"at":at,"source_type":"insight"}]})
}

const AUDIO: &str = "For audio-editing requests, the user prefers a finished, usable artifact \
    over advice or a substitute, with requested export formats, technical validation, and \
    concise edit notes.";
const AUDIO_REWORDED: &str = "For audio-editing requests, the user prefers finished, usable \
    artifacts rather than advice or a substitute, with requested export formats, technical \
    validation, and concise edit notes.";

#[test]
fn memory_lifecycle_key_variant_restatements_fold_at_write_not_into_siblings() {
    let first = merge_collection(
        &[],
        &[audio_preference(
            "audio_editing_deliverable_preference",
            AUDIO,
            "e1",
            "2026-10-02T00:00:00Z",
        )],
        "preferences",
        now(),
    );
    let mut items = first;
    for (key, evidence) in [
        ("audio_editing_deliverable_preferences", "e2"),
        ("audio_editing_deliverables", "e3"),
        ("audio_editing_artifact_preference", "e4"),
    ] {
        items = merge_collection(
            &items,
            &[audio_preference(
                key,
                AUDIO_REWORDED,
                evidence,
                "2026-10-05T00:00:00Z",
            )],
            "preferences",
            now(),
        );
    }
    assert_eq!(items.len(), 1, "one memory, never a sibling: {items:#?}");
    assert_eq!(
        independent_observations(&items[0]),
        4,
        "every copy's evidence is kept"
    );
    assert_eq!(
        text(&items[0]),
        AUDIO,
        "the held wording stays; review weighs the rest"
    );
    assert_eq!(items[0]["memory_review_needed"], true);

    // A different claim, or the same words with a qualifier, stays separate.
    for (key, value) in [
        (
            "audio_editing_deliverable_preference",
            "For audio-editing requests, the user prefers a finished, usable artifact over \
             advice or a substitute, except for podcasts, with requested export formats and \
             concise edit notes.",
        ),
        (
            "weekly_report_format",
            "Weekly reports use concise markdown summaries with headings and links.",
        ),
    ] {
        let separate = merge_collection(
            &items,
            &[audio_preference(key, value, "e9", "2026-10-06T00:00:00Z")],
            "preferences",
            now(),
        );
        assert_eq!(separate.len(), 2, "{key}: {value}");
    }
}

#[test]
fn memory_lifecycle_near_duplicate_maintenance_folds_through_supersession() {
    let mut keeper = audio_preference(
        "audio_editing_deliverables",
        AUDIO_REWORDED,
        "e3",
        "2026-10-05T00:00:00Z",
    );
    keeper["memory_evidence"] = json!([
        {"id":"e3","at":"2026-10-05T00:00:00Z"},
        {"id":"e4","at":"2026-10-06T00:00:00Z"}
    ]);
    let mut document = json!({"preferences":[
        audio_preference("audio_editing_deliverable_preference", AUDIO, "e1", "2026-10-02T00:00:00Z"),
        keeper,
        audio_preference("audio_editing_deliverable_preferences", AUDIO, "e2", "2026-10-03T00:00:00Z"),
        audio_preference("weekly_report_format",
            "Weekly reports use concise markdown summaries with headings and links.",
            "e5", "2026-10-04T00:00:00Z"),
    ]});
    for (index, item) in document["preferences"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        item["memory_record_id"] = json!(format!("r{index}"));
        item["memory_lifecycle"] = json!("active");
    }

    let folds = near_duplicate_folds(&document);
    assert_eq!(folds.len(), 1, "{folds:#?}");
    assert_eq!(folds[0].keeper.id, "r1", "the most-confirmed copy is kept");
    assert_eq!(
        folds[0]
            .folded
            .iter()
            .map(|f| f.id.as_str())
            .collect::<Vec<_>>(),
        vec!["r2", "r0"],
        "newer first among equals"
    );
    assert_eq!(
        document["preferences"][0]["memory_lifecycle"], "active",
        "planning is read-only"
    );

    assert_eq!(apply_near_duplicate_folds(&mut document, &folds, now()), 2);
    let items = document["preferences"].as_array().unwrap();
    for folded in [0, 2] {
        assert!(retired(&items[folded]));
        assert_eq!(items[folded]["superseded_by"], "r1");
        assert_eq!(
            items[folded]["supersession_reason"],
            "near_duplicate_of_record"
        );
        assert!(
            !text(&items[folded]).is_empty(),
            "a folded copy keeps its words"
        );
    }
    assert_eq!(state(&items[1]), "active");
    assert_eq!(independent_observations(&items[1]), 4);
    assert_eq!(state(&items[3]), "active", "a different claim is untouched");
    assert!(
        near_duplicate_folds(&document).is_empty(),
        "folding is idempotent"
    );
}

/// The pass decides whether to take the lock from read-only detectors over
/// the shared parse; each must agree with the mutation it stands for.
#[test]
fn memory_lifecycle_read_only_maintenance_check_matches_the_mutations() {
    let at = now();
    let echo = json!({
        "allowed_user_tiers":["preferences"],
        "promotion_rule":"promote stated preferences",
        "preferences":{"type":"collection","item_schema":{"key":"string"}}
    });
    let ended = (at - chrono::Duration::days(1)).to_rfc3339();
    let later = (at + chrono::Duration::days(1)).to_rfc3339();
    let cases = [
        json!({}),
        json!({"preferences":merge_items(&[], &[json!({"key":"city","value":"Pune"})], at)}),
        json!({"notes":["a primitive legacy entry"]}),
        json!({"nested":{"list":[1, {"key":"x","value":"y"}]}}),
        json!({(JOURNAL):{"calls":[1, 2, 3]}}),
        echo.clone(),
        json!({"tier":echo}),
        json!({"facts":[{"key":"trip","value":"in Goa","valid_until":ended}]}),
        json!({"facts":[{"key":"trip","value":"in Goa","valid_until":later}]}),
        json!({"facts":[{"key":"trip","value":"in Goa","valid_until":ended,"memory_lifecycle":"expired"}]}),
    ];
    for document in cases {
        // The order the locked maintenance step applies them in.
        let mut mutated = document.clone();
        let quarantined = quarantine_legacy_schema_echoes(&mut mutated, at);
        let migrated = normalize_legacy_collections(&mut mutated, at);
        let expired = expire(&mut mutated, at);
        let changed = quarantined + migrated + expired > 0;
        assert_eq!(
            runtime::maintenance_needed(&document, at, 3, 30),
            changed,
            "{document}"
        );
        assert_eq!(
            has_legacy_schema_echoes(&document),
            quarantine_legacy_schema_echoes(&mut document.clone(), at) > 0,
            "{document}"
        );
        assert_eq!(
            has_legacy_collections(&document),
            normalize_legacy_collections(&mut document.clone(), at) > 0,
            "{document}"
        );
    }
}

/// Settle a just-written file outside the racy-mtime window.
fn settle_mtime(path: &std::path::Path) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(60))
        .unwrap();
}

/// An idle pass reads the shared parse instead of re-parsing an unchanged
/// `knowledge.json`; a write through the store drops it.
#[tokio::test]
async fn memory_lifecycle_idle_pass_reuses_the_knowledge_parse_until_a_write() {
    // Passes without a router read the global decision host, which other
    // tests switch on; it must stay off for these.
    let _serial = crate::magician_v2::decisions::test_engine::serial().await;
    use crate::magician_v2::agents::AgentMemoryResolver;
    use crate::magician_v2::query_analysis::operation_llm_router::SimplifiedLLMResponse;
    let root = tempfile::tempdir().unwrap();
    let service = AgentMemoryResolver::new(root.path())
        .resolve_for_scope("p", "w")
        .unwrap();
    let mut initial =
        json!({"preferences":merge_items(&[], &[json!({"key":"city","value":"Pune"})], now())});
    let spent: Vec<i64> = (1..=runtime::MAX_REVIEWS_PER_HOUR as i64)
        .map(|minutes| now().timestamp() - minutes * 60)
        .collect();
    initial[JOURNAL]["calls"] = json!(spent);
    service.save_user_knowledge(&initial).await.unwrap();
    let path = service.storage().user_knowledge_path();

    // Just written: inside the racy window every read parses afresh.
    let racy = service.load_user_knowledge_shared().await.unwrap();
    assert!(!std::sync::Arc::ptr_eq(
        &racy,
        &service.load_user_knowledge_shared().await.unwrap()
    ));

    settle_mtime(&path);
    let shared = service.load_user_knowledge_shared().await.unwrap();
    assert_eq!(shared["preferences"][0]["value"], "Pune");
    let result = runtime::pass_with_provider(
        &service,
        None,
        now(),
        None,
        Some(|_| async {
            panic!("the review budget is spent; no review may run");
            #[allow(unreachable_code)]
            Ok(SimplifiedLLMResponse::default())
        }),
    )
    .await
    .unwrap();
    assert_eq!(result.reviewed, 0);
    // Nothing changed, so the pass neither wrote nor displaced the parse.
    assert!(std::sync::Arc::ptr_eq(
        &shared,
        &service.load_user_knowledge_shared().await.unwrap()
    ));

    service
        .update_user_knowledge_as("test", |document| {
            document["preferences"][0]["value"] = json!("Mumbai");
            Ok(true)
        })
        .await
        .unwrap();
    settle_mtime(&path);
    let rewritten = service.load_user_knowledge_shared().await.unwrap();
    assert!(!std::sync::Arc::ptr_eq(&shared, &rewritten));
    assert_eq!(rewritten["preferences"][0]["value"], "Mumbai");
}
