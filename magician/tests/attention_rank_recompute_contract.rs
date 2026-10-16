use std::collections::BTreeSet;

use magician::magician_v2::attention::learning::{
    ATTENTION_RANK_RECOMPUTE_RESULT_SEMANTICS, ATTENTION_RANK_RECOMPUTE_SERVED_UNIVERSE_SEMANTICS,
    ATTENTION_RANK_RECOMPUTE_WRONGLY_STALED_REASON,
};
use serde_json::Value;

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../data/magician_v2/attention_learning/rank-recompute-frozen-v1.json"
    ))
    .expect("rank recompute fixture must be valid JSON")
}

fn strings(value: &Value) -> BTreeSet<&str> {
    value
        .as_array()
        .expect("expected array")
        .iter()
        .map(|item| item.as_str().expect("expected string"))
        .collect()
}

fn keys(value: &Value) -> BTreeSet<&str> {
    value
        .as_object()
        .expect("expected object")
        .keys()
        .map(String::as_str)
        .collect()
}

#[test]
fn rank_recompute_wire_vocabulary_and_shipped_defaults_are_frozen() {
    let document = fixture();
    let api = &document["api_contract"];
    assert_eq!(document["schema_version"], 1);
    assert_eq!(
        strings(&api["job_statuses"]),
        BTreeSet::from([
            "dead",
            "in_flight",
            "pending",
            "retry",
            "stale",
            "succeeded"
        ])
    );
    assert_eq!(
        strings(&api["terminal_job_statuses"]),
        BTreeSet::from(["dead", "stale", "succeeded"])
    );
    assert_eq!(
        strings(&api["stale_reason_codes"]),
        BTreeSet::from([
            "candidate_inactive",
            "generation_changed_during_commit",
            "no_served_decision",
            "served_projection_unreadable",
            "snapshot_incompatible",
            "source_revision_changed",
            "universe_changed_during_commit",
        ])
    );
    assert_eq!(
        strings(&api["retry_and_dead_reason_codes"]),
        BTreeSet::from([
            "canonical_projection_failed",
            "lease_expired",
            "max_retries_exhausted",
            "posterior_read_failed",
            "rank_recompute_failed",
        ])
    );
    assert_eq!(api["max_reason_characters"], 120);
    assert_eq!(document["defaults"]["rank_recompute_enabled"], false);
    assert_eq!(document["defaults"]["bandit_mode"], "disabled");
    assert!(document["defaults"]["bandit_snapshot_id"].is_null());
    assert_eq!(document["defaults"]["bandit_canary_fraction"], 0.0);
    assert_eq!(document["defaults"]["health"]["enabled"], false);
    assert_eq!(document["defaults"]["health"]["paused"], true);
    assert_eq!(
        document["defaults"]["health"]["pause_reason"],
        "rank_recompute_disabled"
    );
    assert_eq!(
        keys(&document["defaults"]["health"]),
        BTreeSet::from([
            "enabled",
            "pause_reason",
            "paused",
            "queue",
            "schema_version",
            "worker",
        ])
    );
    assert_eq!(
        keys(&document["defaults"]["health"]["queue"]),
        BTreeSet::from([
            "dead",
            "in_flight",
            "next_retry_at",
            "oldest_pending_at",
            "pending",
            "retry",
            "stale",
            "succeeded",
        ])
    );
    assert_eq!(
        keys(&document["defaults"]["health"]["worker"]),
        BTreeSet::from([
            "batch_size",
            "concurrency",
            "interval_secs",
            "lease_secs",
            "max_retries",
            "retention_days",
        ])
    );
    assert_eq!(
        keys(&document["config_contract"]["value"]),
        BTreeSet::from([
            "batch_size",
            "concurrency",
            "enabled",
            "interval_secs",
            "lease_secs",
            "max_retries",
            "retention_days",
            "retry_base_secs",
            "retry_max_secs",
        ])
    );
}

#[test]
fn accepted_outcome_has_one_job_and_an_immediate_pending_receipt_without_after_rank() {
    let document = fixture();
    let outcome = &document["outcome_and_job"];
    let job = &outcome["job"];
    let receipt = &outcome["immediate_receipt"]["rank_recompute"];
    assert_eq!(
        keys(job),
        BTreeSet::from([
            "affected_rank_before",
            "attempts",
            "canonical_candidate_id",
            "completed_at",
            "created_at",
            "decision_id",
            "delivery_id",
            "enqueue_policy_snapshot_id",
            "enqueue_posterior_version",
            "impression_id",
            "job_id",
            "lease_expires_at",
            "next_retry_at",
            "origin_surface",
            "outcome",
            "outcome_id",
            "raw_candidate_id",
            "reason",
            "result",
            "source_revision",
            "status",
            "updated_at",
        ])
    );
    assert_eq!(
        keys(receipt),
        BTreeSet::from([
            "affected_rank_after",
            "affected_rank_before",
            "affected_rank_delta",
            "enqueue_status",
            "job_id",
            "job_status",
            "reason",
            "result_semantics",
            "status_href",
        ])
    );
    assert_eq!(receipt["enqueue_status"], "enqueued");
    assert_eq!(receipt["job_status"], "pending");
    assert_eq!(receipt["affected_rank_before"], 3);
    assert!(receipt["affected_rank_after"].is_null());
    assert!(receipt["affected_rank_delta"].is_null());
    assert_eq!(job["outcome_id"], outcome["outcome"]["outcome_id"]);
    assert_eq!(
        job["affected_rank_before"],
        outcome["outcome"]["served_rank_before"]
    );
    assert_eq!(outcome["uniqueness"]["jobs_after_first_accept"], 1);
    assert_eq!(
        outcome["uniqueness"]["jobs_after_idempotent_outcome_replay"],
        1
    );
    assert_eq!(outcome["uniqueness"]["jobs_after_reconciliation_scan"], 1);
    assert_eq!(
        strings(&outcome["uniqueness"]["database_key"]),
        BTreeSet::from(["outcome_id", "principal", "workspace"])
    );
    assert!(outcome["no_durable_outcome_receipt"]["rank_recompute"].is_null());
}

#[test]
fn accepted_outcome_survives_enqueue_worker_and_exhaustion_failures() {
    let document = fixture();
    let cases = document["outcome_and_job"]["accepted_outcome_survival"]
        .as_array()
        .expect("survival cases");
    assert!(cases.iter().all(|case| case["outcome_count"] == 1));
    let enqueue = cases
        .iter()
        .find(|case| case["failure"] == "enqueue_failed")
        .expect("enqueue case");
    assert_eq!(
        enqueue["immediate_rank_recompute"]["enqueue_status"],
        "failed"
    );
    assert!(enqueue["immediate_rank_recompute"]["job_id"].is_null());
    assert_eq!(
        enqueue["immediate_rank_recompute"]["reason"],
        "enqueue_failed"
    );
    assert_eq!(enqueue["job_count_before_reconciliation"], 0);
    assert_eq!(enqueue["job_count_after_reconciliation"], 1);
    assert_eq!(enqueue["accepted_outcome_rolled_back"], false);
    assert_eq!(cases[1]["lease_reclaimable"], true);
    assert_eq!(cases[2]["outcome_rolled_back"], false);
}

#[test]
fn worker_lifecycle_is_leased_retryable_bounded_and_idempotent() {
    let document = fixture();
    let worker = &document["worker_lifecycle"];
    let transitions = worker["attempts"].as_array().expect("transitions");
    assert_eq!(transitions[0]["from"], "pending");
    assert_eq!(transitions[0]["to"], "in_flight");
    assert_eq!(transitions[1]["from"], "in_flight");
    assert_eq!(transitions[1]["to"], "retry");
    assert_eq!(transitions[2]["from"], "retry");
    assert_eq!(transitions[2]["to"], "in_flight");
    assert_eq!(transitions[3]["to"], "succeeded");
    assert_eq!(
        worker["expired_lease"]["late_prior_owner_completion_accepted"],
        false
    );
    assert_eq!(worker["exhaustion"]["terminal_status"], "dead");
    assert_eq!(worker["exhaustion"]["reclaimed_automatically"], false);
    assert_eq!(worker["idempotent_completion"]["completion_writes"], 2);
    assert_eq!(worker["idempotent_completion"]["terminal_rows"], 1);
    assert_eq!(worker["idempotent_completion"]["posterior_updates"], 1);
    assert_eq!(worker["idempotent_completion"]["result_documents"], 1);
}

#[test]
fn completed_after_rank_is_current_universe_bound_and_before_rank_is_served_attribution() {
    let document = fixture();
    let completion = &document["completion_contract"];
    let before = &completion["before"];
    let after = &completion["after"];
    assert_eq!(before["served_rank"], 3);
    assert_eq!(
        before["decision_id"],
        document["outcome_and_job"]["outcome"]["decision_id"]
    );
    assert_eq!(before["attribution"], "verified_impression");
    assert_eq!(
        keys(after),
        BTreeSet::from([
            "affected_rank_after",
            "affected_rank_delta",
            "completed_at",
            "current_source_revision",
            "policy_snapshot_id",
            "posterior_version",
            "recompute_generation",
            "semantics",
            "universe_digest",
        ])
    );
    // This job carries the decision that served it, so its after-rank is bound
    // to the universe the owner saw — not the current one.
    assert_eq!(after["semantics"], "served_universe_diagnostic");
    assert_eq!(after["affected_rank_after"], 4);
    assert_eq!(after["affected_rank_delta"], 1);
    assert_eq!(
        after["universe_digest"],
        document["current_universe"]["universe_digest"]
    );
    assert_eq!(after["current_source_revision"], before["source_revision"]);
    assert_eq!(after["posterior_version"], 18);
    assert_eq!(after["policy_snapshot_id"], "bandit-delivery-fixture-v1");
    assert_eq!(
        after["recompute_generation"],
        document["current_universe"]["source_generations"]
    );
}

#[test]
fn source_revision_universe_generation_and_snapshot_are_compare_and_set_bound() {
    let document = fixture();
    let cas = &document["compare_and_set"];
    assert_eq!(
        strings(&cas["required_exact_matches"]),
        BTreeSet::from([
            "candidate_id",
            "policy_model_version",
            "policy_snapshot_id",
            "posterior_version",
            "principal",
            "source_generations.follow_up",
            "source_generations.worth_a_look",
            "source_revision",
            "universe_digest",
            "workspace",
        ])
    );
    let preconditions = cas["served_precondition_stale_cases"]
        .as_array()
        .expect("served preconditions");
    assert_eq!(
        preconditions
            .iter()
            .map(|case| case["reason"].as_str().unwrap())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["no_served_decision", "served_projection_unreadable"])
    );
    let cases = cas["stale_cases"]
        .as_array()
        .expect("stale cases")
        .iter()
        .chain(preconditions)
        .collect::<Vec<_>>();
    let reasons = cases
        .iter()
        .map(|case| case["reason"].as_str().expect("reason"))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        reasons,
        strings(&document["api_contract"]["stale_reason_codes"])
    );
    assert!(cases.iter().all(|case| {
        case["terminal_status"] == "stale"
            && case["after"].is_null()
            && case["posterior_updates"] == 0
    }));
    assert_eq!(
        cas["digest_or_generation_drift"]["completion_accepted"],
        false
    );
    assert_eq!(
        cas["digest_or_generation_drift"]["stale_worker_can_overwrite_current_result"],
        false
    );
}

/// The commit-time live re-projection is an optimistic-concurrency check on
/// the read, so it may only bind a projection that was itself read live.
/// Applying it to a served projection compares a historical universe against
/// the current one, and the owner action that enqueues the job has already
/// changed the current one — so every attributed job would go stale and none
/// could ever succeed.
#[test]
fn the_live_universe_compare_and_set_binds_only_a_live_read_projection() {
    let document = fixture();
    let provenance = &document["compare_and_set"]["universe_provenance"];
    let served = &provenance["served"];
    let recomputed = &provenance["recomputed"];

    assert_eq!(served["commit_time_live_reprojection"], false);
    assert_eq!(recomputed["commit_time_live_reprojection"], true);

    assert!(!strings(&served["reachable_stale_reasons"]).contains("universe_changed_during_commit"));
    assert!(
        strings(&recomputed["reachable_stale_reasons"]).contains("universe_changed_during_commit")
    );

    // Served-ledger preconditions precede ranking. The remaining shared
    // checks stay reachable from both provenances.
    assert_eq!(
        strings(&served["reachable_stale_reasons"])
            .into_iter()
            .filter(|reason| !matches!(
                *reason,
                "no_served_decision" | "served_projection_unreadable"
            ))
            .collect::<BTreeSet<_>>(),
        strings(&recomputed["reachable_stale_reasons"])
            .into_iter()
            .filter(|reason| *reason != "universe_changed_during_commit")
            .collect()
    );

    // The two provenances together must still cover the frozen vocabulary.
    let mut union = strings(&served["reachable_stale_reasons"]);
    union.extend(strings(&recomputed["reachable_stale_reasons"]));
    assert_eq!(
        union,
        strings(&document["api_contract"]["stale_reason_codes"])
    );
}

/// A result must say which universe its after-rank belongs to. Resolving
/// against the projection a decision was served from produces a historical
/// `universe_digest`, and labelling that "current" is a plausible, unfalsifiable
/// claim to whatever trains on it.
#[test]
fn a_result_names_the_universe_its_after_rank_is_bound_to() {
    let document = fixture();
    let api = &document["api_contract"];
    let semantics = strings(&api["result_semantics"]);
    // Bound to the constants the runtime actually emits, not to their spelling.
    // Every other vocabulary in this fixture is a literal the code repeats by
    // hand, so a rename there passes the suite while the wire changes under it.
    assert_eq!(
        semantics,
        BTreeSet::from([
            ATTENTION_RANK_RECOMPUTE_RESULT_SEMANTICS,
            ATTENTION_RANK_RECOMPUTE_SERVED_UNIVERSE_SEMANTICS
        ])
    );

    // Each universe provenance maps to exactly one label, and between them they
    // cover the vocabulary — so no provenance can go unnamed.
    let by_universe = &api["result_semantics_by_universe"];
    assert_eq!(keys(by_universe), BTreeSet::from(["recomputed", "served"]));
    assert_eq!(
        by_universe["served"],
        ATTENTION_RANK_RECOMPUTE_SERVED_UNIVERSE_SEMANTICS
    );
    assert_eq!(
        by_universe["recomputed"],
        ATTENTION_RANK_RECOMPUTE_RESULT_SEMANTICS
    );
    assert_eq!(
        by_universe
            .as_object()
            .expect("map")
            .values()
            .map(|value| value.as_str().expect("label"))
            .collect::<BTreeSet<_>>(),
        semantics
    );

    // The fixture's outcome records the decision that served it, so both the
    // receipt promised at enqueue and the completed result read served-bound.
    let job = &document["outcome_and_job"]["job"];
    assert!(job["decision_id"].as_str().is_some_and(|id| !id.is_empty()));
    assert_eq!(
        document["outcome_and_job"]["immediate_receipt"]["rank_recompute"]["result_semantics"],
        by_universe["served"]
    );
    assert_eq!(
        document["completion_contract"]["after"]["semantics"],
        by_universe["served"]
    );
    assert_eq!(
        document["job_polling"]["succeeded"]["job"]["result"]["semantics"],
        by_universe["served"]
    );

    // A failed enqueue produces no job and therefore no result, so its receipt
    // describes no provenance and must not claim the served one.
    assert_eq!(
        document["outcome_and_job"]["accepted_outcome_survival"][0]["immediate_rank_recompute"]
            ["result_semantics"],
        by_universe["recomputed"]
    );
}

/// Requeue returns only the cohort whose stale verdict carried no information
/// about the job. Every other stale reason is a correct verdict about the job's
/// own inputs, so a blanket reset would resurrect genuinely inactive candidates
/// and revive superseded revisions.
#[test]
fn requeue_revives_only_the_wrongly_staled_cohort_and_restores_its_before_rank() {
    let document = fixture();
    let requeue = &document["compare_and_set"]["requeue_wrongly_staled"];
    let provenance = &document["compare_and_set"]["universe_provenance"];

    // Exactly the reason the served-projection guard produced, and only it —
    // bound to the constant the requeue actually filters on, so renaming it
    // cannot leave this fixture describing a cohort nothing selects.
    assert_eq!(
        requeue["requeued_reason"],
        ATTENTION_RANK_RECOMPUTE_WRONGLY_STALED_REASON
    );
    assert!(strings(&document["api_contract"]["stale_reason_codes"])
        .contains(ATTENTION_RANK_RECOMPUTE_WRONGLY_STALED_REASON));
    assert!(!strings(&provenance["served"]["reachable_stale_reasons"])
        .contains(requeue["requeued_reason"].as_str().expect("reason")));
    let untouched = strings(&requeue["terminal_reasons_left_untouched"]);
    assert!(!untouched.contains(requeue["requeued_reason"].as_str().expect("reason")));
    let mut covered = untouched.clone();
    covered.insert(requeue["requeued_reason"].as_str().expect("reason"));
    assert_eq!(
        covered,
        strings(&document["api_contract"]["stale_reason_codes"])
    );

    // A repair, not a new source of work: it re-decides existing jobs under the
    // ordinary worker rather than minting any.
    assert_eq!(requeue["creates_jobs"], false);
    assert_eq!(requeue["requeued_status"], "pending");
    assert_eq!(requeue["requeued_attempts"], 0);
    assert_eq!(
        requeue["affected_rank_before_backfilled_from"],
        "served_rank"
    );

    // Same operator guarantees as every other admin path here.
    assert_eq!(requeue["apply_false_never_mutates"], true);
    assert_eq!(
        requeue["apply_false_never_mutates"],
        document["admin_contract"]["apply_false_never_mutates"]
    );
    assert_eq!(requeue["bounded_by_batch_size"], true);
    assert_eq!(requeue["idempotent"], true);
}

#[test]
fn recompute_uses_the_complete_union_and_reports_per_origin_evidence() {
    let document = fixture();
    let universe = &document["current_universe"];
    let items = universe["items"].as_array().expect("universe items");
    assert_eq!(
        items.len(),
        universe["candidate_total"].as_u64().unwrap() as usize
    );
    assert_eq!(
        universe["candidate_total"].as_u64().unwrap(),
        universe["origin_totals"]["follow_up"].as_u64().unwrap()
            + universe["origin_totals"]["worth_a_look"].as_u64().unwrap()
    );
    assert_eq!(
        items
            .iter()
            .map(|item| item["candidate_id"].as_str().unwrap())
            .collect::<BTreeSet<_>>()
            .len(),
        items.len()
    );
    assert_eq!(
        items
            .iter()
            .map(|item| item["current_rank"].as_u64().unwrap())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([1, 2, 3, 4])
    );
    assert_eq!(
        universe["evidence"]["compatible_feature_total"]
            .as_u64()
            .unwrap(),
        universe["evidence"]["follow_up"]["compatible_feature_total"]
            .as_u64()
            .unwrap()
            + universe["evidence"]["worth_a_look"]["compatible_feature_total"]
                .as_u64()
                .unwrap()
    );
    assert!(universe["reconciliation"]
        .as_object()
        .unwrap()
        .values()
        .all(|value| value.as_bool() == Some(true)));
}

#[test]
fn native_and_legacy_feedback_share_jobs_without_page_time_model_work() {
    let document = fixture();
    let ingress = &document["ingress_contract"];
    assert_eq!(ingress["delivery_native"]["jobs_created"], 1);
    assert_eq!(ingress["legacy_follow_up"]["jobs_created"], 1);
    assert_eq!(ingress["legacy_worth_a_look"]["jobs_created"], 1);
    assert_eq!(ingress["same_outcome_semantics"], true);
    assert_eq!(
        ingress["delivery_native"]["before_rank_source"],
        "delivery_root_served_position"
    );
    let isolation = &document["serving_isolation"];
    assert_eq!(isolation["list_and_page_model_invocations"], 0);
    assert_eq!(isolation["list_and_page_job_claims"], 0);
    assert_eq!(isolation["list_and_page_job_waits"], 0);
    assert_eq!(isolation["pending_job_changes_page_order"], false);
}

#[test]
fn polling_admin_retention_and_deletion_remain_scoped_and_paused_by_default() {
    let document = fixture();
    assert_eq!(document["job_polling"]["pending"]["schema_version"], 1);
    assert_eq!(
        document["job_polling"]["pending"]["job"]["status"],
        "pending"
    );
    assert!(document["job_polling"]["pending"]["job"]["result"].is_null());
    assert_eq!(
        keys(&document["job_polling"]["pending"]["job"]),
        keys(&document["outcome_and_job"]["job"])
    );
    assert_eq!(
        keys(&document["job_polling"]["succeeded"]["job"]["result"]),
        keys(&document["completion_contract"]["after"])
    );
    assert_eq!(document["job_polling"]["scope_mismatch_status"], 404);
    assert_eq!(document["job_polling"]["unknown_job_status"], 404);
    assert_eq!(
        document["admin_contract"]["process_disabled"]["paused"],
        true
    );
    assert_eq!(
        document["admin_contract"]["process_disabled"]["jobs_leased"],
        0
    );
    assert_eq!(
        document["admin_contract"]["apply_false_never_mutates"],
        true
    );
    let ownership = &document["retention_and_deletion"];
    assert_eq!(ownership["table"], "attention_rank_recompute_jobs");
    assert!(ownership["retention"]
        .as_object()
        .unwrap()
        .values()
        .all(|value| value.as_bool() == Some(true)));
    assert!(ownership["scoped_deletion"]
        .as_object()
        .unwrap()
        .values()
        .all(|value| value.as_bool() == Some(true)));
}

#[test]
fn expected_eval_report_is_a_checked_in_content_free_contract() {
    let document = fixture();
    let report: Value = serde_json::from_str(include_str!(
        "../../data/magician_v2/attention_learning/rank-recompute-report-v1.json"
    ))
    .expect("rank recompute report fixture must be valid JSON");
    assert_eq!(report, document["expected_report"]);
    assert_eq!(report["gate_passed"], true);
    assert_eq!(report["list_and_page_model_invocations"], 0);
}
