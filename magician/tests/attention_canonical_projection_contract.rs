use std::collections::{BTreeMap, BTreeSet};

use magician::config::{AttentionLearningConfig, AttentionRoutingMode};
use magician_comms::channel_assist::canonical_attention::{
    CanonicalAttentionProjection, CanonicalAttentionProjectionResponse, CanonicalProjectionStatus,
};
use serde_json::Value;

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../data/magician_v2/attention_learning/canonical-union-frozen-v1.json"
    ))
    .expect("parse frozen canonical attention union fixture")
}

fn expected_projection(document: &Value) -> &Value {
    &document["complete_union"]["expected"]
}

fn projected_items(projection: &Value) -> Vec<(&str, &Value)> {
    ["follow_up", "worth_a_look", "non_surfaced"]
        .into_iter()
        .flat_map(|lane| {
            projection["lanes"][lane]
                .as_array()
                .expect("ordered canonical lane")
                .iter()
                .map(move |item| (lane, item))
        })
        .collect()
}

#[test]
fn frozen_union_pins_the_additive_public_api_contract() {
    let document = fixture();
    assert_eq!(document["schema_version"], 1);
    assert_eq!(
        document["api_contract"]["legacy_response_field"],
        "canonical_attention_projection"
    );
    assert_eq!(document["api_contract"]["standalone_method"], "GET");
    assert_eq!(
        document["api_contract"]["standalone_path"],
        "/api/magician/v2/channel-assist/attention-learning/canonical-projection"
    );

    let projection = expected_projection(&document);
    assert_eq!(projection["schema_version"], 1);
    for required in [
        "projection_id",
        "universe_digest",
        "status",
        "policy",
        "integrity",
        "lanes",
    ] {
        assert!(projection.get(required).is_some(), "missing {required}");
    }
    assert_eq!(
        projection["lanes"]
            .as_object()
            .expect("canonical lanes")
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["follow_up", "non_surfaced", "worth_a_look"])
    );

    let typed: CanonicalAttentionProjection = serde_json::from_value(projection.clone())
        .expect("fixture must deserialize through the public schema-v1 DTO");
    assert_eq!(typed.status, CanonicalProjectionStatus::Succeeded);
    let response = serde_json::to_value(CanonicalAttentionProjectionResponse {
        canonical_attention_projection: typed,
    })
    .expect("serialize additive legacy/standalone response envelope");
    let serialized = response["canonical_attention_projection"]
        .as_object()
        .expect("serialized canonical projection");
    let legacy = projection.as_object().expect("legacy canonical projection");
    for (key, expected) in legacy {
        assert_eq!(serialized.get(key), Some(expected), "legacy field {key}");
    }
    assert_eq!(
        serialized["cross_lane_reconciliation"]["status"],
        "unavailable"
    );
    assert_eq!(
        serialized["cross_lane_reconciliation"]["reason"],
        "canonical_projection_unavailable"
    );
    assert_eq!(serialized["duplicate_aliases"], serde_json::json!([]));
}

#[test]
fn both_legacy_lanes_and_standalone_replay_one_idempotent_projection() {
    let document = fixture();
    let projection = expected_projection(&document);
    let replays = document["complete_union"]["replays"]
        .as_array()
        .expect("projection replays");
    assert_eq!(replays.len(), 3);
    assert_eq!(
        replays
            .iter()
            .filter_map(|replay| replay["consumer"].as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["follow_up", "standalone", "worth_a_look"])
    );
    assert!(replays.iter().all(|replay| {
        replay["projection_id"] == projection["projection_id"]
            && replay["universe_digest"] == projection["universe_digest"]
    }));
    assert_eq!(
        replays
            .iter()
            .filter_map(|replay| replay["projection_id"].as_str())
            .collect::<BTreeSet<_>>()
            .len(),
        1
    );
    assert_eq!(
        replays
            .iter()
            .filter_map(|replay| replay["universe_digest"].as_str())
            .collect::<BTreeSet<_>>()
            .len(),
        1
    );
    let digest = projection["universe_digest"]
        .as_str()
        .expect("complete union digest");
    assert_eq!(digest.len(), 64);
    assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
}

#[test]
fn every_origin_qualified_source_is_reconciled_exactly_once() {
    let document = fixture();
    let projection = expected_projection(&document);
    let items = projected_items(projection);
    let integrity = &projection["integrity"];

    assert_eq!(integrity["load_complete"], true);
    assert_eq!(integrity["exact_once"], true);
    assert_eq!(integrity["follow_up_source_total"], 3);
    assert_eq!(integrity["worth_a_look_source_total"], 3);
    assert_eq!(integrity["source_total"], 6);
    assert_eq!(integrity["reconciled_total"], 6);
    assert_eq!(integrity["materialized_total"], 6);
    assert_eq!(integrity["duplicate_hidden_total"], 0);
    assert_eq!(integrity["unmatched_total"], 0);
    assert_eq!(integrity["fallback_reason"], Value::Null);
    assert_eq!(items.len(), 6);

    let projection_ids = items
        .iter()
        .filter_map(|(_, item)| item["canonical_id"].as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(projection_ids.len(), items.len());
    assert_eq!(
        projection_ids,
        BTreeSet::from([
            "follow_up:fu-campaign",
            "follow_up:fu-low",
            "follow_up:shared-001",
            "worth_a_look:shared-001",
            "worth_a_look:wa-campaign",
            "worth_a_look:wa-request",
        ])
    );

    // The two source stores intentionally contain the same raw ID. The
    // canonical identity must include the origin lane, so neither aliases.
    assert_eq!(
        items
            .iter()
            .filter(|(_, item)| {
                item["origin"]["annotation_id"] == "shared-001"
                    || item["origin"]["candidate_id"] == "shared-001"
            })
            .count(),
        2
    );
    assert!(items.iter().all(|(lane, item)| {
        item["served_lane"] == *lane
            && item["origin"]["kind"] == item["origin_lane"]
            && item["payload"]["kind"] == item["origin_lane"]
            && item["canonical_id"]
                .as_str()
                .is_some_and(|canonical_id| canonical_id.starts_with(lane_for_origin(item)))
    }));
}

fn lane_for_origin(item: &Value) -> &'static str {
    if item["origin_lane"] == "follow_up" {
        "follow_up:"
    } else {
        "worth_a_look:"
    }
}

#[test]
fn grouping_accounts_for_every_member_without_hiding_member_payloads() {
    let document = fixture();
    let projection = expected_projection(&document);
    let items = projected_items(projection);
    let all_ids = items
        .iter()
        .filter_map(|(_, item)| item["canonical_id"].as_str())
        .collect::<BTreeSet<_>>();
    let mut clusters: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut representatives = BTreeSet::new();

    for (_, item) in &items {
        let grouping = &item["group"];
        let cluster_id = grouping["cluster_id"].as_str().expect("cluster id");
        let members = grouping["member_ids"]
            .as_array()
            .expect("group members")
            .iter()
            .map(|member| member.as_str().expect("member projection id"))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            members.len(),
            grouping["member_count"].as_u64().unwrap() as usize
        );
        assert!(members.is_subset(&all_ids));
        assert!(members.contains(item["canonical_id"].as_str().expect("canonical item id")));
        match clusters.get(cluster_id) {
            Some(existing) => assert_eq!(existing, &members),
            None => {
                clusters.insert(cluster_id, members);
            },
        }
        representatives.insert(
            grouping["representative_id"]
                .as_str()
                .expect("representative id"),
        );
    }

    let unique_members = clusters
        .values()
        .flat_map(|members| members.iter().copied())
        .collect::<BTreeSet<_>>();
    assert_eq!(unique_members, all_ids);
    assert_eq!(
        unique_members.len(),
        projection["integrity"]["grouped_member_total"]
            .as_u64()
            .unwrap() as usize
    );
    assert_eq!(clusters.len(), 5);
    assert_eq!(representatives.len(), 5);
}

#[test]
fn either_origin_can_render_in_either_lane_with_origin_owned_actions() {
    let document = fixture();
    let projection = expected_projection(&document);
    let items = projected_items(projection);
    let moved_follow_up = items
        .iter()
        .find(|(_, item)| {
            item["origin_lane"] == "follow_up" && item["served_lane"] == "worth_a_look"
        })
        .map(|(_, item)| *item)
        .expect("Follow-up-origin card renderable in Worth-a-look");
    let moved_worth = items
        .iter()
        .find(|(_, item)| {
            item["origin_lane"] == "worth_a_look" && item["served_lane"] == "follow_up"
        })
        .map(|(_, item)| *item)
        .expect("Worth-origin card renderable in Follow-up");

    assert_eq!(moved_follow_up["payload"]["kind"], "follow_up");
    assert_eq!(moved_follow_up["payload"]["annotation_id"], "fu-campaign");
    assert!(moved_follow_up["payload"].get("source_kind").is_none());
    assert_eq!(
        moved_follow_up["actions"]
            .as_array()
            .expect("Follow-up origin actions")
            .iter()
            .filter_map(|action| action["kind"].as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "acknowledge",
            "approve",
            "dismiss",
            "open_source",
            "snooze",
            "useful",
        ])
    );
    assert!(moved_follow_up["actions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|action| action["method"] == "post")
        .all(|action| action["href"]
            .as_str()
            .is_some_and(|href| href.contains("/annotations/fu-campaign/"))));

    assert_eq!(moved_worth["payload"]["kind"], "worth_a_look");
    assert_eq!(moved_worth["payload"]["source_kind"], "task_episode");
    assert!(moved_worth["payload"].get("annotation_id").is_none());
    assert_eq!(
        moved_worth["actions"]
            .as_array()
            .expect("Worth origin actions")
            .iter()
            .filter_map(|action| action["kind"].as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["acknowledge", "dismiss", "open_source", "useful"])
    );
    assert!(moved_worth["actions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|action| action["method"] == "post")
        .all(|action| action["href"]
            == "/api/magician/v2/channel-assist/resurfacing/wa-request/action"));
}

#[test]
fn learned_evidence_stays_with_its_origin_surface_and_pair_ids_are_remapped() {
    let document = fixture();
    let projection = expected_projection(&document);
    let origin_by_id = projected_items(projection)
        .into_iter()
        .map(|(_, item)| {
            (
                item["canonical_id"].as_str().expect("canonical id"),
                item["origin_lane"].as_str().expect("origin surface"),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let contract = &document["learning_evidence_contract"];
    let candidate_evidence = contract["candidate_evidence"]
        .as_array()
        .expect("candidate evidence scopes");
    assert_eq!(candidate_evidence.len(), origin_by_id.len());
    assert_eq!(
        candidate_evidence
            .iter()
            .filter_map(|row| row["canonical_id"].as_str())
            .collect::<BTreeSet<_>>(),
        origin_by_id.keys().copied().collect::<BTreeSet<_>>()
    );
    for evidence in candidate_evidence {
        let canonical_id = evidence["canonical_id"].as_str().unwrap();
        let origin = origin_by_id[canonical_id];
        assert_eq!(evidence["origin_surface"], origin);
        assert_eq!(evidence["rank_surface"], origin);
        assert_eq!(evidence["posterior_surface"], origin);
        assert_eq!(evidence["pair_surface"], origin);
        assert_eq!(evidence["cannot_link_surface"], origin);
    }

    let remaps = contract["within_origin_pair_remaps"]
        .as_array()
        .expect("within-origin pair remaps");
    assert_eq!(remaps.len(), 2);
    for remap in remaps {
        let surface = remap["surface"].as_str().unwrap();
        let prefix = format!("{surface}:");
        let left = remap["canonical_left_id"].as_str().unwrap();
        let right = remap["canonical_right_id"].as_str().unwrap();
        assert!(left.starts_with(&prefix));
        assert!(right.starts_with(&prefix));
        assert_eq!(left.strip_prefix(&prefix), remap["raw_left_id"].as_str());
        assert_eq!(right.strip_prefix(&prefix), remap["raw_right_id"].as_str());
        assert_eq!(origin_by_id[left], surface);
        assert_eq!(origin_by_id[right], surface);
        assert_eq!(remap["cannot_link"], true);
    }

    let cross_origin = contract["unlabeled_cross_origin_pairs"]
        .as_array()
        .expect("unlabeled cross-origin pairs");
    assert!(!cross_origin.is_empty());
    for pair in cross_origin {
        let left = pair["left_canonical_id"].as_str().unwrap();
        let right = pair["right_canonical_id"].as_str().unwrap();
        assert_ne!(origin_by_id[left], origin_by_id[right]);
        assert_eq!(pair["owner_pair_label"], Value::Null);
        assert_eq!(pair["cannot_link"], false);
        assert_eq!(pair["borrowed_surface_evidence"], false);
        assert_eq!(pair["model_inference_allowed"], true);
    }
}

#[test]
fn baseline_and_dormant_zero_canary_preserve_atomic_origin_order() {
    let document = fixture();
    let contract = &document["baseline_order_contract"];
    assert_eq!(contract["mode"], "baseline");
    assert_eq!(contract["canary_fraction"], 0.0);
    assert_eq!(contract["route_changes_applied"], false);
    assert_eq!(contract["diagnostic_learned_order_may_differ"], true);
    for lane in ["follow_up", "worth_a_look", "non_surfaced"] {
        assert_eq!(
            contract["expected_served_order"][lane], contract["origin_order"][lane],
            "baseline must not silently apply diagnostic learned order in {lane}"
        );
    }
}

#[test]
fn stale_incomplete_or_unavailable_union_load_never_exposes_a_partial_projection() {
    let document = fixture();
    let cases = document["fail_closed_cases"]
        .as_array()
        .expect("fail-closed cases");
    for case_id in [
        "source_totals_changed_during_union_load",
        "worth_load_count_does_not_reconcile",
        "canonical_projector_unavailable",
    ] {
        let case = cases
            .iter()
            .find(|case| case["id"] == case_id)
            .unwrap_or_else(|| panic!("missing {case_id}"));
        assert_eq!(case["expected_status"], "baseline_fallback");
        assert_eq!(case["expected_effective_mode"], "baseline");
        assert_eq!(case["expected_effective_canary_fraction"], 0.0);
        assert_eq!(case["expected_route_changes_applied"], false);
        assert_eq!(case["expected_partial_projection_item_total"], 0);
        assert_eq!(case["legacy_response"], "original_atomic_payload");
    }
}

#[test]
fn public_fallback_reasons_are_bounded_codes_not_raw_error_chains() {
    let source = include_str!("../../magician-comms/src/channel_assist/canonical_attention.rs");
    for code in [
        "follow_up_load_stale",
        "worth_a_look_load_incomplete",
        "canonical_source_totals_changed",
        "follow_up_load_unavailable",
        "worth_a_look_load_unavailable",
        "canonical_learning_unavailable",
        "canonical_learning_evidence_changed",
        "canonical_projection_store_unavailable",
        "canonical_projector_unavailable",
    ] {
        assert!(
            source.contains(code),
            "missing bounded fallback code {code}"
        );
    }
    assert!(source.contains("canonical_fallback_reason(&error)"));
}

#[test]
fn canonical_projection_coalesces_by_scope_and_retries_only_authoritative_input_races() {
    let canonical = include_str!("../../magician-comms/src/channel_assist/canonical_attention.rs");
    assert!(canonical.contains("fn canonical_projection_scope_cache("));
    assert!(canonical.contains("AsyncOnceCell"));
    assert!(canonical.contains(".get_or_init(||"));
    assert!(canonical.contains("store_identity"));
    assert!(canonical.contains("retry_once_on_projection_input_change"));
    assert!(canonical.contains("is_retryable_canonical_projection_input_change"));
    assert!(canonical.contains("CanonicalLearningEvidenceChanged"));
    assert!(canonical.contains("CanonicalSourceGenerationChanged"));
    assert!(canonical.contains("retried,"));
}

#[test]
fn historical_bootstrap_stages_labels_before_one_cohort_refresh() {
    let bootstrap = include_str!(
        "../../magician-comms/src/channel_assist/attention_learning/historical_bootstrap.rs"
    );
    assert!(bootstrap.contains("stage_historical_outcome"));
    assert!(!bootstrap.contains("record_historical_and_propagate("));
    let final_import = bootstrap
        .find("HISTORICAL_WORTH_AFFINITY_SOURCE")
        .expect("final historical source");
    let refresh = bootstrap[final_import..]
        .find("refresh_current_cohorts")
        .expect("one post-import cohort refresh");
    assert!(refresh > 0);
}

#[test]
fn standalone_route_and_both_legacy_envelopes_are_statically_wired() {
    let server = include_str!("../../magician-bin/src/main.rs");
    let follow_up = include_str!("../../magician-api/src/channel_assist_api.rs");
    let worth = include_str!("../../magician-api/src/resurfacing_api.rs");
    assert!(server.contains("attention-learning/canonical-projection"));
    assert!(follow_up.contains("canonical_attention_projection"));
    assert!(worth.contains("canonical_attention_projection"));
}

#[test]
fn shipped_defaults_and_zero_fraction_canary_are_non_applying() {
    let document = fixture();
    assert_eq!(document["defaults"]["routing_mode"], "baseline");
    assert_eq!(document["defaults"]["routing_snapshot_id"], Value::Null);
    assert_eq!(document["defaults"]["canary_fraction"], 0.0);
    assert_eq!(document["defaults"]["route_changes_applied"], false);

    let config = AttentionLearningConfig::default();
    assert_eq!(config.routing.mode, AttentionRoutingMode::Baseline);
    assert!(config.routing.snapshot_id.is_none());
    assert_eq!(config.routing.canary_fraction, 0.0);

    let cases = document["fail_closed_cases"]
        .as_array()
        .expect("fallback cases");
    for case_id in [
        "baseline_default_is_complete_but_non_applying",
        "shipped_baseline_canary_zero_is_complete_but_non_applying",
    ] {
        let case = cases
            .iter()
            .find(|case| case["id"] == case_id)
            .unwrap_or_else(|| panic!("missing {case_id}"));
        assert_eq!(case["expected_effective_mode"], "baseline");
        assert_eq!(case["expected_effective_canary_fraction"], 0.0);
        assert_eq!(case["expected_route_changes_applied"], false);
        assert_eq!(case["expected_partial_projection_item_total"], 6);
        assert_eq!(case["legacy_response"], "canonical_baseline_payload");
    }
}

#[test]
fn shipped_repo_and_template_configs_keep_routing_baseline_and_canary_zero() {
    for (label, source) in [(
        "repository seed",
        &magician::config::shipped_repo_config_yaml(),
    )] {
        let config: serde_yaml::Value =
            serde_yaml::from_str(source).unwrap_or_else(|error| panic!("parse {label}: {error}"));
        let routing = &config["attention_learning"]["routing"];
        assert_eq!(routing["mode"].as_str(), Some("baseline"), "{label}");
        assert!(routing["snapshot_id"].is_null(), "{label}");
        assert_eq!(routing["canary_fraction"].as_f64(), Some(0.0), "{label}");
    }
}
