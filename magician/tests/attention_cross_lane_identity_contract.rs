use std::collections::BTreeSet;

use serde_json::Value;

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../data/magician_v2/attention_learning/cross-lane-identity-frozen-v1.json"
    ))
    .expect("cross-lane identity fixture must be valid JSON")
}

fn strings(value: &Value) -> BTreeSet<&str> {
    value
        .as_array()
        .expect("expected array")
        .iter()
        .map(|item| item.as_str().expect("expected string"))
        .collect()
}

fn scenario<'a>(document: &'a Value, name: &str) -> &'a Value {
    document["scenarios"]
        .as_array()
        .expect("scenarios")
        .iter()
        .find(|case| case["case"] == name)
        .expect("named scenario")
}

#[test]
fn only_exact_typed_communication_thread_identity_can_create_an_alias() {
    let document = fixture();
    let contract = &document["contract"];
    assert_eq!(contract["identity_kind"], "communication_thread");
    assert_eq!(contract["match_operator"], "exact_typed_tuple_equality");
    assert_eq!(
        strings(&contract["typed_components"]),
        BTreeSet::from(["account_alias", "provider", "thread_id"])
    );
    assert_eq!(contract["worth_source_kind"], "comm");
    assert_eq!(contract["normalization"], "validated_percent_decode_only");
    assert_eq!(
        contract["malformed_ref_behavior"],
        "reconciliation_unavailable_and_not_published"
    );
    assert_eq!(
        strings(&contract["forbidden_match_signals"]),
        BTreeSet::from([
            "embedding",
            "llm_judgment",
            "message_text",
            "participants",
            "semantic_similarity",
            "sender",
            "subject",
            "summary",
            "title",
        ])
    );

    let similar = scenario(
        &document,
        "similar_content_different_thread_is_not_collapsed",
    );
    assert_eq!(similar["same_title"], true);
    assert_eq!(similar["same_summary"], true);
    assert!(similar["embedding_cosine_similarity"].as_f64().unwrap() > 0.99);
    assert_ne!(similar["follow_up_thread"], similar["worth_thread"]);
    assert_eq!(similar["expected_alias"], false);
}

#[test]
fn follow_up_owns_each_exact_active_match_and_worth_aliases_never_reach_learning_or_materialization(
) {
    let document = fixture();
    let contract = &document["contract"];
    assert_eq!(contract["owner_on_exact_active_match"], "follow_up");
    assert_eq!(contract["alias_origin"], "worth_a_look");
    for stage in ["ranked", "grouped", "routed", "materialized"] {
        assert_eq!(contract["alias_pipeline_admission"][stage], false);
    }
    assert_eq!(contract["alias_evidence"]["durable"], true);
    assert_eq!(
        contract["alias_evidence"]["worth_origin_evidence_preserved"],
        true
    );
    assert_eq!(
        contract["alias_evidence"]["worth_feedback_history_preserved"],
        true
    );
    assert_eq!(
        contract["alias_evidence"]["worth_source_lifecycle_unchanged"],
        true
    );

    let exact = scenario(
        &document,
        "two_worth_messages_alias_one_active_follow_up_thread",
    );
    let aliases = exact["durable_aliases"]
        .as_array()
        .expect("durable aliases");
    assert_eq!(aliases.len(), 2);
    assert!(aliases.iter().all(|alias| {
        alias["owner_canonical_id"] == "follow_up:fu-a"
            && alias["reason"] == "exact_source_identity"
    }));
    let admitted = strings(&exact["ranked_grouped_routed_materialized_ids"]);
    assert!(!admitted.contains("worth_a_look:wa-a1"));
    assert!(!admitted.contains("worth_a_look:wa-a2"));
    assert!(admitted.contains("follow_up:fu-a"));
}

#[test]
fn every_successful_scenario_obeys_raw_reconciled_materialized_grouped_and_lane_equations() {
    let document = fixture();
    assert_eq!(
        strings(&document["accounting_contract"]["equations"]),
        BTreeSet::from([
            "duplicate_hidden_total=worth_alias_total",
            "duplicate_exposure_total=0",
            "grouped_representative_total+grouped_duplicate_member_total=grouped_member_total",
            "lane_total=follow_up_lane_total+worth_a_look_lane_total+non_surfaced_total",
            "lane_total=grouped_representative_total",
            "raw_total=grouped_member_total+duplicate_hidden_total",
            "raw_total=materialized_total+duplicate_hidden_total",
            "raw_total=follow_up_raw_total+worth_a_look_raw_total",
            "reconciled_total=raw_total",
        ])
    );
    assert_eq!(
        document["accounting_contract"]["alias_is_not_group_duplicate"],
        true
    );

    for case in document["scenarios"].as_array().expect("scenarios") {
        let Some(counts) = case.get("counts") else {
            continue;
        };
        let value = |key: &str| counts[key].as_u64().expect("unsigned count");
        assert_eq!(
            value("raw_total"),
            value("follow_up_raw_total") + value("worth_a_look_raw_total")
        );
        assert_eq!(value("reconciled_total"), value("raw_total"));
        assert_eq!(value("duplicate_hidden_total"), value("worth_alias_total"));
        assert_eq!(
            value("raw_total"),
            value("materialized_total") + value("duplicate_hidden_total")
        );
        assert_eq!(
            value("raw_total"),
            value("grouped_member_total") + value("duplicate_hidden_total")
        );
        assert_eq!(
            value("grouped_representative_total") + value("grouped_duplicate_member_total"),
            value("grouped_member_total")
        );
        assert_eq!(
            value("lane_total"),
            value("follow_up_lane_total")
                + value("worth_a_look_lane_total")
                + value("non_surfaced_total")
        );
        assert_eq!(value("lane_total"), value("grouped_representative_total"));
        assert_eq!(value("duplicate_exposure_total"), 0);
    }
}

#[test]
fn durable_alias_stops_suppressing_when_the_follow_up_owner_is_inactive() {
    let document = fixture();
    let reappears = scenario(&document, "worth_alias_reappears_after_follow_up_inactive");
    assert_eq!(reappears["follow_up_active"], false);
    assert_eq!(reappears["worth_source_active"], true);
    assert_eq!(reappears["expected_alias"], false);
    assert_eq!(reappears["worth_materialized"], true);
    assert_eq!(reappears["worth_reappeared"], true);
    assert_eq!(reappears["counts"]["worth_alias_total"], 0);
    assert_eq!(reappears["counts"]["worth_a_look_lane_total"], 1);
}

#[test]
fn malformed_refs_remain_distinct_and_alias_or_revision_drift_changes_the_digest() {
    let document = fixture();
    let malformed = scenario(&document, "malformed_comm_ref_never_collapses");
    assert_eq!(malformed["refs"].as_array().unwrap().len(), 4);
    assert_eq!(malformed["parsed_identity_count"], 0);
    assert_eq!(malformed["expected_alias_count"], 0);
    assert_eq!(malformed["malformed_diagnostic_count"], 4);
    assert_eq!(malformed["malformed_identity_disclosed"], false);
    assert_eq!(malformed["reconciliation_status"], "unavailable");
    assert_eq!(malformed["reason"], "malformed_worth_comm_source_ref");
    assert_eq!(malformed["canonical_projection_published"], false);
    assert_eq!(malformed["partial_projection_published"], false);
    assert_eq!(
        malformed["legacy_worth_guard"]["comm_candidates_admitted"],
        0
    );
    assert_eq!(malformed["legacy_worth_guard"]["http_status"], 503);
    assert_eq!(malformed["legacy_worth_guard"]["cards_returned"], 0);

    let digest = scenario(
        &document,
        "typed_identity_or_revision_change_changes_digest",
    );
    assert_eq!(digest["all_digests_distinct"], true);
    let values = [
        "base_digest",
        "alias_added_digest",
        "owner_revision_changed_digest",
        "origin_revision_changed_digest",
    ]
    .map(|key| digest[key].as_str().expect("digest"));
    assert_eq!(values.into_iter().collect::<BTreeSet<_>>().len(), 4);
    assert!(values
        .iter()
        .all(|value| { value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) }));
}

#[test]
fn public_diagnostics_are_content_free_and_unavailable_reconciliation_fails_closed() {
    let document = fixture();
    let privacy = &document["privacy_contract"];
    let public = strings(&privacy["public_diagnostic_fields"]);
    let forbidden = strings(&privacy["forbidden_public_identity_fields"]);
    assert!(public.is_disjoint(&forbidden));
    assert_eq!(
        privacy["durable_alias_identity"],
        "persisted_canonical_projection_binding"
    );
    assert_eq!(privacy["raw_identity_in_logs"], false);
    assert_eq!(privacy["public_aliases"]["max_items"], 100);
    assert_eq!(
        privacy["public_aliases"]["identity"],
        "canonical_origin_ids"
    );
    assert_eq!(privacy["public_aliases"]["raw_identity_disclosed"], false);

    let unavailable = scenario(&document, "reconciliation_unavailable_fails_closed");
    assert_eq!(unavailable["reconciliation_status"], "unavailable");
    assert_eq!(unavailable["reason"], "follow_up_load_unavailable");
    assert_eq!(unavailable["canonical_projection_published"], false);
    assert_eq!(unavailable["partial_projection_published"], false);
    assert_eq!(unavailable["visible_diagnostic"], true);
    assert_eq!(unavailable["public_raw_identity_disclosed"], false);
    assert_eq!(
        unavailable["legacy_worth_guard"]["enforced_server_side"],
        true
    );
    assert_eq!(
        unavailable["legacy_worth_guard"]["comm_candidates_admitted"],
        0
    );
    assert_eq!(
        unavailable["legacy_worth_guard"]["non_comm_candidates_admitted"],
        0
    );
    assert_eq!(unavailable["legacy_worth_guard"]["http_status"], 503);
    assert_eq!(unavailable["legacy_worth_guard"]["cards_returned"], 0);
}

#[test]
fn reconciliation_precedes_every_page_and_no_alias_can_reappear_later() {
    let document = fixture();
    let paging = &document["paging_contract"];
    assert_eq!(paging["reconcile_before_pagination"], true);
    assert_eq!(paging["canonical_aliases_never_enter_cursor_order"], true);
    assert_eq!(paging["legacy_raw_cursor_advances_across_aliases"], true);
    assert_eq!(paging["legacy_hidden_aliases_consume_visible_slot"], false);
    assert_eq!(
        paging["legacy_response_total_uses_reconciled_visible_total"],
        true
    );
    assert_eq!(paging["stable_retry_no_skip_or_duplicate"], true);
    assert_eq!(paging["displayed_ids_exact_once_across_pages"], true);
    assert_eq!(paging["server_legacy_worth_guard_required"], true);
    assert_eq!(paging["client_dedupe_is_not_authoritative"], true);

    let health = &document["legacy_health_contract"];
    assert_eq!(
        strings(&health["global_fields"]),
        BTreeSet::from([
            "duplicate_hidden_total",
            "raw_source_total",
            "visible_source_total",
        ])
    );
    assert_eq!(
        strings(&health["page_fields"]),
        BTreeSet::from([
            "duplicate_hidden_page_total",
            "raw_scanned_total",
            "visible_page_total",
        ])
    );
    assert_eq!(
        strings(&health["equations"]),
        BTreeSet::from([
            "raw_scanned_total=visible_page_total+duplicate_hidden_page_total",
            "raw_source_total=visible_source_total+duplicate_hidden_total",
            "response_total=visible_source_total",
            "visible_page_total=returned_card_total",
        ])
    );
    assert_eq!(health["unavailable_counts"], "null");

    let unavailable = scenario(&document, "reconciliation_unavailable_fails_closed");
    let ids = unavailable["paging"]["page_ids"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|page| page.as_array().unwrap())
        .map(|id| id.as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        ids.len(),
        ids.iter().copied().collect::<BTreeSet<_>>().len()
    );
    assert_eq!(unavailable["paging"]["duplicate_exposure_total"], 0);
}
