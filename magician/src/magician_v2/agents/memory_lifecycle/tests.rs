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
