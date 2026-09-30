use serde_json::Value;

#[test]
fn delivery_attribution_fixture_keeps_root_policy_and_impression_identity() {
    let fixture: Value = serde_json::from_str(include_str!(
        "fixtures/attention_delivery_attribution_v1.json"
    ))
    .expect("valid delivery attribution fixture");
    assert_eq!(fixture["schema_version"], 1);
    assert_eq!(
        fixture["delivery"]["decision_id"],
        fixture["outcome"]["decision_id"]
    );
    assert_eq!(
        fixture["item"]["candidate_id"],
        fixture["outcome"]["candidate_id"]
    );
    assert_eq!(fixture["item"]["root_policy_propensity"], 0.25);
    assert_eq!(fixture["item"]["conditional_delivery_propensity"], 1.0);
    assert_eq!(
        fixture["outcome"]["expected_attribution_quality"],
        "verified_impression"
    );
    assert_eq!(fixture["outcome"]["expected_single_update"], true);
}
