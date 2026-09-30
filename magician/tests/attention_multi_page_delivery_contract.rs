use std::collections::{BTreeMap, BTreeSet};

use magician::config::{AttentionBanditConfig, AttentionBanditMode};
use magician::magician_v2::attention::learning::{
    AttentionImpressionReceipt, AttentionOutcomeAttribution, RecordAttentionImpression,
};
use magician_comms::channel_assist::canonical_attention::{
    AttentionDeliveryPageResponse, AttentionDeliveryRefreshRequired,
};
use serde_json::Value;

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../data/magician_v2/attention_learning/multi-page-delivery-frozen-v1.json"
    ))
    .expect("parse frozen multi-page attention delivery fixture")
}

fn delivery_pages<'a>(document: &'a Value, lane: &str) -> &'a [Value] {
    document[lane]["pages"]
        .as_array()
        .expect("ordered frozen delivery pages")
}

fn delivery_items<'a>(pages: &'a [Value]) -> impl Iterator<Item = &'a Value> {
    pages.iter().flat_map(|page| {
        page["items"]
            .as_array()
            .expect("frozen delivery items")
            .iter()
    })
}

#[test]
fn public_delivery_and_refresh_dtos_accept_the_frozen_wire_contract() {
    let document = fixture();
    for lane in ["follow_up_delivery", "worth_a_look_delivery"] {
        for page in delivery_pages(&document, lane) {
            let typed: AttentionDeliveryPageResponse = serde_json::from_value(page.clone())
                .expect("delivery page must deserialize through the public schema-v1 DTO");
            assert_eq!(
                serde_json::to_value(typed).expect("serialize delivery page"),
                *page
            );
        }
    }
    let fallback: AttentionDeliveryPageResponse =
        serde_json::from_value(document["disabled_fallback"].clone())
            .expect("baseline fallback must use the same complete public response DTO");
    assert_eq!(
        serde_json::to_value(fallback).expect("serialize fallback page"),
        document["disabled_fallback"]
    );

    for case in document["refresh_required_cases"]
        .as_array()
        .expect("refresh-required cases")
    {
        let typed: AttentionDeliveryRefreshRequired =
            serde_json::from_value(case["response"].clone())
                .expect("409 body must deserialize through the bounded public DTO");
        assert_eq!(
            serde_json::to_value(typed).expect("serialize refresh response"),
            case["response"]
        );
    }
}

#[test]
fn one_complete_lane_root_is_sampled_once_and_bound_for_every_page() {
    let document = fixture();
    let binding = &document["canonical_binding"];
    for (fixture_lane, wire_lane) in [
        ("follow_up_delivery", "follow_up"),
        ("worth_a_look_delivery", "worth_a_look"),
    ] {
        assert_eq!(document[fixture_lane]["sampler_invocations"], 1);
        let pages = delivery_pages(&document, fixture_lane);
        let first_root = &pages[0]["root_decision"];
        assert_eq!(first_root["lane"], wire_lane);
        assert_eq!(first_root["projection_id"], binding["projection_id"]);
        assert_eq!(first_root["universe_digest"], binding["universe_digest"]);
        assert_eq!(
            first_root["policy_snapshot_id"],
            binding["policy_snapshot_id"]
        );
        assert_eq!(
            first_root["policy_model_version"],
            binding["policy_model_version"]
        );
        assert_eq!(
            first_root["posterior_version"],
            binding["posterior_version"]
        );
        assert_eq!(first_root["seed_identity"], binding["seed_identity"]);
        assert_eq!(first_root["created_at"], binding["created_at"]);
        assert_eq!(first_root["expires_at"], binding["expires_at"]);
        assert!(pages.iter().all(|page| {
            page["root_decision"] == *first_root && page["health"]["root_sample_count"] == 1
        }));
    }
}

#[test]
fn candidate_identity_revision_and_root_propensity_are_exact_on_every_page() {
    let document = fixture();
    for fixture_lane in ["follow_up_delivery", "worth_a_look_delivery"] {
        let expected = document[fixture_lane]["root_candidate_revisions"]
            .as_array()
            .expect("root candidate revision ledger");
        let actual = delivery_items(delivery_pages(&document, fixture_lane))
            .map(|item| {
                (
                    item["position"].as_u64().expect("position"),
                    item["candidate_id"].as_str().expect("candidate id"),
                    item["source_revision"].as_str().expect("source revision"),
                    item["root_policy_propensity"]
                        .as_f64()
                        .expect("root propensity"),
                )
            })
            .collect::<Vec<_>>();
        let frozen = expected
            .iter()
            .map(|item| {
                (
                    item["position"].as_u64().unwrap(),
                    item["candidate_id"].as_str().unwrap(),
                    item["source_revision"].as_str().unwrap(),
                    item["root_policy_propensity"].as_f64().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, frozen);
        assert!(
            delivery_items(delivery_pages(&document, fixture_lane)).all(|item| {
                item["candidate_id"] == item["item"]["canonical_id"]
                    && item["source_revision"] == item["item"]["source_revision"]
            })
        );
    }
}

#[test]
fn page_positions_are_contiguous_and_exhaust_the_frozen_root_without_duplicate_or_skip() {
    let document = fixture();
    for fixture_lane in ["follow_up_delivery", "worth_a_look_delivery"] {
        let pages = delivery_pages(&document, fixture_lane);
        let root = &pages[0]["root_decision"];
        let mut expected_start = 0_u64;
        let mut positions = Vec::new();
        let mut candidate_ids = BTreeSet::new();
        for (page_index, page) in pages.iter().enumerate() {
            assert_eq!(page["page"]["page_index"], page_index as u64);
            assert_eq!(page["page"]["page_start"], expected_start);
            let items = page["items"].as_array().unwrap();
            assert!(items.len() as u64 <= page["page"]["page_size"].as_u64().unwrap());
            assert_eq!(page["page"]["page_size"], pages[0]["page"]["page_size"]);
            if page["page"]["has_more"] == true {
                assert_eq!(page["page"]["page_size"], items.len() as u64);
            }
            assert_eq!(
                page["health"]["delivered_count"],
                expected_start + items.len() as u64
            );
            assert_eq!(
                page["health"]["remaining_count"],
                root["universe_size"].as_u64().unwrap() - expected_start - items.len() as u64
            );
            for item in items {
                positions.push(item["position"].as_u64().unwrap());
                assert!(candidate_ids.insert(item["candidate_id"].as_str().unwrap()));
            }
            expected_start += items.len() as u64;
        }
        assert_eq!(
            positions,
            (1..=root["universe_size"].as_u64().unwrap()).collect::<Vec<_>>()
        );
        assert_eq!(candidate_ids.len() as u64, root["universe_size"]);
        assert_eq!(pages.last().unwrap()["page"]["has_more"], false);
        assert!(pages.last().unwrap()["page"]["next_cursor"].is_null());
    }
}

#[test]
fn cursor_retry_replays_the_identical_persisted_page() {
    let document = fixture();
    let replays = document["follow_up_delivery"]["cursor_replay"]
        .as_array()
        .expect("cursor retries");
    assert_eq!(replays.len(), 2);
    let first = &replays[0];
    let retry = &replays[1];
    for binding in [
        "cursor",
        "expected_page_index",
        "expected_delivery_id",
        "expected_response_digest",
    ] {
        assert_eq!(first[binding], retry[binding], "retry changed {binding}");
    }
    let page = &delivery_pages(&document, "follow_up_delivery")
        [first["expected_page_index"].as_u64().unwrap() as usize];
    assert_eq!(page["page"]["cursor"], first["cursor"]);
    assert_eq!(page["page"]["delivery_id"], first["expected_delivery_id"]);
    assert_eq!(page["health"]["root_sample_count"], 1);
    assert_eq!(page["health"]["replay"], true);
}

#[test]
fn stale_or_foreign_cursors_fail_closed_with_only_bounded_refresh_reasons() {
    let document = fixture();
    let cases = document["refresh_required_cases"].as_array().unwrap();
    assert_eq!(
        cases
            .iter()
            .map(|case| case["response"]["reason"].as_str().unwrap())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "binding_mismatch",
            "cursor_not_found",
            "expired",
            "projection_drift",
            "revision_drift",
            "scope_mismatch",
        ])
    );
    assert!(cases.iter().all(|case| {
        case["expected_http_status"] == 409
            && case["response"]["schema_version"] == 1
            && case["response"]["status"] == "refresh_required"
            && case["response"]["error"] == "attention_delivery_refresh_required"
            && case["response"]["lane"] == "follow_up"
            && case["response"]["refresh_href"]
                .as_str()
                .is_some_and(|href| href.contains("/canonical-deliveries/follow_up"))
    }));
}

#[test]
fn first_slate_logs_nontrivial_exact_root_propensities_and_later_delivery_is_conditional_one() {
    let document = fixture();
    for fixture_lane in ["follow_up_delivery", "worth_a_look_delivery"] {
        let pages = delivery_pages(&document, fixture_lane);
        let first_slate = pages[0]["items"].as_array().unwrap();
        assert!(first_slate.iter().all(|item| {
            let propensity = item["root_policy_propensity"].as_f64().unwrap();
            propensity > 0.0 && propensity < 1.0
        }));
        assert!(
            first_slate
                .iter()
                .map(|item| item["root_policy_propensity"].to_string())
                .collect::<BTreeSet<_>>()
                .len()
                > 1
        );
        assert!(delivery_items(pages).all(|item| {
            item["conditional_delivery_propensity"] == 1.0
                && item["root_policy_propensity"].as_f64().unwrap() > 0.0
        }));
    }
    assert!(["follow_up_delivery", "worth_a_look_delivery"]
        .into_iter()
        .any(|lane| {
            let pages = delivery_pages(&document, lane);
            delivery_items(&pages[1..]).any(|item| {
                item["root_policy_propensity"].as_f64().unwrap() < 1.0
                    && item["conditional_delivery_propensity"] == 1.0
            })
        }));
}

#[test]
fn exposure_tokens_bind_the_delivered_item_revision_position_scope_and_ope_values() {
    let document = fixture();
    let mut delivered = BTreeMap::new();
    for fixture_lane in ["follow_up_delivery", "worth_a_look_delivery"] {
        for page in delivery_pages(&document, fixture_lane) {
            for item in page["items"].as_array().unwrap() {
                delivered.insert(
                    item["exposure_token"].as_str().unwrap(),
                    (&page["root_decision"], &page["page"], item),
                );
            }
        }
    }
    for binding in document["exposure_token_bindings"].as_array().unwrap() {
        let (root, page, item) = delivered
            .get(binding["token"].as_str().unwrap())
            .copied()
            .expect("token must identify one delivered item");
        assert_eq!(binding["principal"], "owner");
        assert_eq!(binding["workspace"], "default");
        assert_eq!(binding["lane"], root["lane"]);
        assert_eq!(binding["decision_id"], root["decision_id"]);
        assert_eq!(binding["delivery_id"], page["delivery_id"]);
        assert_eq!(binding["page_index"], page["page_index"]);
        assert_eq!(binding["position"], item["position"]);
        assert_eq!(binding["candidate_id"], item["candidate_id"]);
        assert_eq!(binding["source_revision"], item["source_revision"]);
        assert_eq!(binding["universe_digest"], root["universe_digest"]);
        assert_eq!(
            binding["root_policy_propensity"],
            item["root_policy_propensity"]
        );
        assert_eq!(binding["conditional_delivery_propensity"], 1.0);
        assert_eq!(binding["expires_at"], root["expires_at"]);
    }
}

#[test]
fn only_delivery_verified_dwell_creates_an_idempotent_impression() {
    let document = fixture();
    let contract = &document["impression_contract"];
    let request = &contract["accepted_request"];
    let receipt = &contract["expected_receipt"];
    let policy = &delivery_pages(&document, "follow_up_delivery")[1]["impression_policy"];
    let _: RecordAttentionImpression = serde_json::from_value(request.clone())
        .expect("accepted request must deserialize through the delivery-bound impression DTO");
    let typed_receipt: AttentionImpressionReceipt = serde_json::from_value(receipt.clone())
        .expect("expected receipt must deserialize with copied delivery and propensity facts");
    assert_eq!(
        serde_json::to_value(typed_receipt).expect("serialize delivery-bound impression receipt"),
        *receipt
    );
    for binding in [
        "event_id",
        "decision_id",
        "delivery_id",
        "page_index",
        "position",
        "candidate_id",
        "source_revision",
        "surface",
        "visibility_rule_version",
        "exposure_token",
    ] {
        assert_eq!(
            request[binding], receipt[binding],
            "receipt changed {binding}"
        );
    }
    assert_eq!(request["visible_ms"], policy["min_visible_ms"]);
    assert_eq!(
        request["visibility_rule_version"],
        policy["visibility_rule_version"]
    );
    assert_eq!(receipt["verified"], true);
    assert_eq!(receipt["root_policy_propensity"], 0.57);
    assert_eq!(receipt["conditional_delivery_propensity"], 1.0);
    assert_eq!(contract["idempotent_retry"]["same_event_id"], true);
    assert_eq!(contract["idempotent_retry"]["same_binding"], true);
    assert_eq!(
        contract["idempotent_retry"]["same_impression_id"],
        receipt["impression_id"]
    );
    assert_eq!(contract["idempotent_retry"]["deduplicated"], true);
    assert_eq!(contract["idempotent_retry"]["posterior_exposure_count"], 1);
    assert!(contract["rejected_cases"]
        .as_array()
        .unwrap()
        .iter()
        .all(|case| { case["verified"] == false && case["posterior_exposure_count"] == 0 }));
}

#[test]
fn verified_delivery_outcome_resolves_the_projection_features_without_losing_root_policy_identity()
{
    let document = fixture();
    let contract = &document["outcome_attribution_contract"];
    let attribution: AttentionOutcomeAttribution =
        serde_json::from_value(contract["attribution"].clone())
            .expect("delivery-root attribution must use the canonical outcome DTO");
    assert_eq!(
        attribution.decision_id,
        document["follow_up_delivery"]["pages"][0]["root_decision"]["decision_id"]
    );
    assert_eq!(
        contract["resolved_projection_decision_id"],
        document["canonical_binding"]["projection_id"]
    );
    assert_eq!(
        contract["policy_snapshot_id"],
        document["canonical_binding"]["policy_snapshot_id"]
    );
    assert_eq!(
        contract["posterior_version_at_decision"],
        document["canonical_binding"]["posterior_version"]
    );
    assert_eq!(contract["verified_impression_required"], true);
    assert_eq!(
        contract["expected_attribution_quality"],
        "verified_impression"
    );
    assert_eq!(contract["expected_posterior_update_count"], 1);
    assert_eq!(contract["idempotent_outcome_replay_update_count"], 1);
}

#[test]
fn disabled_bandit_is_a_persisted_deterministic_baseline_delivery() {
    let document = fixture();
    let fallback = &document["disabled_fallback"];
    assert_eq!(fallback["status"], "baseline_fallback");
    assert_eq!(fallback["fallback_reason"], "bandit_disabled");
    assert!(fallback["root_decision"]["policy_snapshot_id"].is_null());
    assert!(fallback["root_decision"]["policy_model_version"].is_null());
    assert_eq!(fallback["root_decision"]["posterior_version"], 0);
    assert_eq!(fallback["root_decision"]["seed_identity"], "baseline");
    assert_eq!(fallback["health"]["root_sample_count"], 0);
    assert!(fallback["items"].as_array().unwrap().iter().all(|item| {
        item["root_policy_propensity"] == 1.0 && item["conditional_delivery_propensity"] == 1.0
    }));
    assert_eq!(
        document["fallback_reason_codes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "bandit_disabled",
            "canonical_projection_baseline_fallback",
            "feature_contract_mismatch",
            "policy_scope_not_canary",
            "policy_shadow_only",
            "posterior_unavailable",
            "snapshot_missing_or_invalid",
        ])
    );
}

#[test]
fn lane_roots_share_the_projection_but_never_share_a_decision_or_delivered_item() {
    let document = fixture();
    let follow_pages = delivery_pages(&document, "follow_up_delivery");
    let worth_pages = delivery_pages(&document, "worth_a_look_delivery");
    let follow_root = &follow_pages[0]["root_decision"];
    let worth_root = &worth_pages[0]["root_decision"];
    assert_ne!(follow_root["decision_id"], worth_root["decision_id"]);
    assert_ne!(follow_root["lane"], worth_root["lane"]);
    assert_eq!(follow_root["projection_id"], worth_root["projection_id"]);
    assert_eq!(
        follow_root["universe_digest"],
        worth_root["universe_digest"]
    );
    let follow_decisions = follow_pages
        .iter()
        .map(|page| page["root_decision"]["decision_id"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    let worth_decisions = worth_pages
        .iter()
        .map(|page| page["root_decision"]["decision_id"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert!(follow_decisions.is_disjoint(&worth_decisions));
    let follow_ids = delivery_items(follow_pages)
        .map(|item| item["candidate_id"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    let worth_ids = delivery_items(worth_pages)
        .map(|item| item["candidate_id"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert!(follow_ids.is_disjoint(&worth_ids));
}

#[test]
fn retention_and_scope_deletion_own_all_delivery_materialization_without_erasing_ope_facts() {
    let document = fixture();
    let contract = &document["retention_contract"];
    assert_eq!(
        contract["tables"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "attention_delivery_cursors",
            "attention_delivery_decision_items",
            "attention_delivery_decisions",
            "attention_delivery_page_items",
            "attention_delivery_pages",
        ])
    );
    assert_eq!(contract["manual_cascade_in_one_transaction"], true);
    assert_eq!(
        contract["impressions_retain_copied_delivery_binding_and_propensities"],
        true
    );
    assert_eq!(
        contract["scoped_deletion_removes_all_five_delivery_tables"],
        true
    );
    assert_eq!(contract["global_policy_snapshots_preserved"], true);
    assert!(contract["unexpired_root_ids_preserved"]
        .as_array()
        .unwrap()
        .iter()
        .all(|id| !contract["expired_root_ids_removed"]
            .as_array()
            .unwrap()
            .contains(id)));
}

#[test]
fn route_store_impression_and_config_sources_pin_the_static_integration_contract() {
    let server = include_str!("../../magician-bin/src/main.rs");
    let api = include_str!("../../magician-comms/src/channel_assist/canonical_attention.rs");
    let store = include_str!("../../magician/src/magician_v2/attention/learning/store.rs");
    let routing = include_str!("../../magician/src/magician_v2/attention/learning/routing.rs");
    assert!(server.contains("/attention-learning/canonical-deliveries/{lane}"));
    assert!(server.contains("get_canonical_attention_delivery_handler"));
    for public_name in [
        "AttentionDeliveryPageResponse",
        "AttentionDeliveryRefreshRequired",
        "impression_policy",
        "root_policy_propensity",
        "conditional_delivery_propensity",
        "exposure_token",
    ] {
        assert!(api.contains(public_name), "API is missing {public_name}");
    }
    for table in fixture()["retention_contract"]["tables"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
    {
        assert!(store.contains(table), "store is missing {table}");
    }
    for delivery_attribution_binding in [
        "attribution_item_json",
        "context_json",
        "attention_delivery_decisions d",
        "root_policy_propensity",
        "conditional_delivery_propensity",
    ] {
        assert!(
            store.contains(delivery_attribution_binding),
            "delivery outcome attribution is missing {delivery_attribution_binding}"
        );
    }
    for required in ["delivery_id", "page_index", "position", "exposure_token"] {
        assert!(
            routing.contains(required),
            "impression request is missing {required}"
        );
    }

    let config = AttentionBanditConfig::default();
    assert_eq!(config.mode, AttentionBanditMode::Disabled);
    assert!(config.snapshot_id.is_none());
    assert_eq!(config.canary_fraction, 0.0);
    assert_eq!(config.delivery_default_page_size, 50);
    assert_eq!(config.delivery_max_page_size, 200);
    assert_eq!(config.delivery_ttl_secs, 900);
    assert_eq!(config.delivery_retention_days, 30);
    assert!(config.delivery_default_page_size <= config.delivery_max_page_size);
    assert!(config.delivery_ttl_secs > 0);
    assert!(config.delivery_retention_days.saturating_mul(86_400) >= config.delivery_ttl_secs);

    for yaml in [&magician::config::shipped_repo_config_yaml()] {
        for expected in [
            "mode: disabled",
            "snapshot_id: null",
            "canary_fraction: 0.0",
            "delivery_default_page_size: 50",
            "delivery_max_page_size: 200",
            "delivery_ttl_secs: 900",
            "delivery_retention_days: 30",
        ] {
            assert!(yaml.contains(expected), "config seed is missing {expected}");
        }
    }
}
