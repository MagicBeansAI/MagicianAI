use std::collections::BTreeSet;

use super::{contract::*, decision::*, review::*};

pub(super) fn hash(label: &str) -> String {
    blake3::hash(label.as_bytes()).to_hex().to_string()
}

fn model(family: &str) -> ModelPin {
    ModelPin {
        profile: family.into(),
        model: format!("{family}-pinned"),
        family: family.into(),
        configuration_hash: hash(family),
    }
}

pub(super) fn proposal() -> DefinitionOfDone {
    DefinitionOfDone {
        goal: "Produce an artifact meeting the owner's frozen criteria".into(),
        artifact_kind: "document".into(),
        actor: model("actor"),
        criteria: vec![
            Criterion {
                id: "quality".into(),
                kind: CriterionKind::Rubric,
                specification_hash: hash("quality-v1"),
                weight: Some(3.0),
                must_pass: false,
                pass_score: None,
            },
            Criterion {
                id: "grounded".into(),
                kind: CriterionKind::Grounding,
                specification_hash: hash("grounding-v1"),
                weight: None,
                must_pass: true,
                pass_score: Some(1.0),
            },
            Criterion {
                id: "complete".into(),
                kind: CriterionKind::Check,
                specification_hash: hash("schema-v1"),
                weight: None,
                must_pass: true,
                pass_score: None,
            },
        ],
        critics: (0..3)
            .map(|n| CriticPin {
                id: format!("critic-{n}"),
                criteria: vec!["quality".into(), "grounded".into()],
                model: Some(model("reviewer")),
                configuration_hash: hash(&format!("critic-{n}")),
            })
            .chain([CriticPin {
                id: "check".into(),
                criteria: vec!["complete".into()],
                model: None,
                configuration_hash: hash("check"),
            }])
            .collect(),
        sources: vec![SourcePin {
            id: "owner-note".into(),
            content_hash: hash("owner source bytes"),
        }],
        collected_source_limits: None,
        score_at_least: Some(0.8),
        review: ReviewPolicy {
            panel_size: 3,
            score_quorum: 3,
            min_confidence: 0.8,
            max_score_spread: 0.2,
            noise_margin: 0.02,
            max_retries_per_critic: 1,
            unknown_review: UnknownReviewPolicy::Pause,
        },
        budget: Budget {
            max_attempts: 8,
            max_wall_clock_secs: 7200,
            max_cost_microusd: 5_000_000,
            plateau_attempts: 3,
        },
    }
}

pub(super) fn contract() -> FrozenContract {
    proposal().try_into().unwrap()
}

pub(super) fn context(label: &str) -> ReviewContext {
    ReviewContext {
        artifact_hash: hash(label),
        source_epoch: 0,
        evidence_ids: BTreeSet::from(["owner-note".into(), "check-receipt".into()]),
        artifact_texts: vec!["sealed factual draft".into()],
        sources: std::collections::BTreeMap::from([(
            "owner-note".into(),
            b"owner source bytes".to_vec(),
        )]),
    }
}

pub(super) fn verdicts(
    c: &FrozenContract,
    ctx: &ReviewContext,
    quality: f64,
    grounded: bool,
) -> Vec<Verdict> {
    c.definition()
        .critics
        .iter()
        .map(|pin| Verdict {
            critic_id: pin.id.clone(),
            critic_configuration_hash: pin.configuration_hash.clone(),
            contract_hash: c.hash().unwrap(),
            artifact_hash: ctx.artifact_hash.clone(),
            source_epoch: ctx.source_epoch,
            status: ReviewStatus::Complete,
            review_recommendation: None,
            scores: pin
                .criteria
                .iter()
                .map(|id| match id.as_str() {
                    "quality" => CriterionScore {
                        criterion_id: id.clone(),
                        outcome: Outcome::Pass,
                        score: Some(quality),
                        confidence: Some(0.9),
                        coverage: None,
                    },
                    "grounded" => CriterionScore {
                        criterion_id: id.clone(),
                        outcome: if grounded {
                            Outcome::Pass
                        } else {
                            Outcome::Fail
                        },
                        score: Some(if grounded { 1.0 } else { 0.0 }),
                        confidence: Some(0.9),
                        coverage: Some(ClaimCoverage {
                            whole_artifact_reviewed: true,
                            claims: vec![ClaimReview {
                                id: "claim-1".into(),
                                artifact_quote: ctx
                                    .artifact_texts
                                    .first()
                                    .cloned()
                                    .unwrap_or_default(),
                                outcome: if grounded {
                                    Outcome::Pass
                                } else {
                                    Outcome::Fail
                                },
                                sources: if grounded {
                                    vec![ClaimSource {
                                        source_id: "owner-note".into(),
                                        quote: "owner source bytes".into(),
                                    }]
                                } else {
                                    vec![]
                                },
                            }],
                            confirmed_no_factual_claims: false,
                        }),
                    },
                    "complete" => CriterionScore {
                        criterion_id: id.clone(),
                        outcome: Outcome::Pass,
                        score: None,
                        confidence: None,
                        coverage: None,
                    },
                    _ => unreachable!(),
                })
                .collect(),
            findings: vec![],
            evidence_ids: vec!["owner-note".into(), "check-receipt".into()],
        })
        .collect()
}

fn reviewed(c: &FrozenContract, label: &str, quality: f64, grounded: bool) -> ReviewedArtifact {
    let ctx = context(label);
    aggregate(c, &ctx, &verdicts(c, &ctx, quality, grounded)).unwrap()
}

pub(super) fn budget(attempts: u32) -> DecisionContext {
    DecisionContext {
        attempts_started: attempts,
        spent_and_reserved_microusd: 100_000,
        source_epoch: 0,
        deadline_expired: false,
        cancelled: false,
    }
}

#[test]
fn invalid_high_score_loses_to_valid_below_threshold() {
    let c = contract();
    let invalid = reviewed(&c, "invalid", 0.95, false);
    let first = decide(&c, &Retained::default(), &invalid, &budget(1)).unwrap();
    assert!(first.retained.champion.is_none());
    assert_eq!(
        first.retained.best_draft.as_ref().unwrap().artifact_hash,
        invalid.artifact_hash
    );
    let valid = reviewed(&c, "valid", 0.79, true);
    let next = decide(&c, &first.retained, &valid, &budget(8)).unwrap();
    assert_eq!(next.kind, DecisionKind::BudgetExhausted);
    assert_eq!(
        next.retained.champion.as_ref().unwrap().artifact_hash,
        valid.artifact_hash
    );
    assert!(next.accepted_hash.is_none());
}

#[test]
fn exact_threshold_crossing_wins_despite_noise_and_budget() {
    let c = contract();
    let old = reviewed(&c, "old", 0.799, true);
    let first = decide(&c, &Retained::default(), &old, &budget(1)).unwrap();
    let new = reviewed(&c, "new", 0.801, true);
    let mut limits = budget(8);
    limits.spent_and_reserved_microusd = c.definition().budget.max_cost_microusd;
    let next = decide(&c, &first.retained, &new, &limits).unwrap();
    assert_eq!(next.kind, DecisionKind::Accept);
    assert_eq!(
        next.accepted_hash.as_deref(),
        Some(new.artifact_hash.as_str())
    );
    assert_eq!(
        next.retained.champion.unwrap().artifact_hash,
        new.artifact_hash
    );
}

#[test]
fn check_only_accepts_first_attempt_without_quality_score() {
    let mut p = proposal();
    p.criteria.retain(|c| c.kind == CriterionKind::Check);
    p.critics.retain(|c| c.model.is_none());
    p.score_at_least = None;
    let c: FrozenContract = p.try_into().unwrap();
    let artifact = reviewed(&c, "check-only", 0.0, false);
    assert_eq!(artifact.quality_score, None);
    assert_eq!(
        decide(&c, &Retained::default(), &artifact, &budget(1))
            .unwrap()
            .kind,
        DecisionKind::Accept
    );
}

#[test]
fn disagreement_revision_policy_is_frozen_and_preserves_legacy_contract_hashes() {
    let p = proposal();
    let legacy = serde_json::to_value(&p).unwrap();
    assert!(legacy["review"].get("unknown_review").is_none());
    let decoded: FrozenContract = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(
        decoded.definition().review.unknown_review,
        UnknownReviewPolicy::Pause
    );
    assert_eq!(
        decoded.hash().unwrap(),
        blake3::hash(&serde_json::to_vec(&p).unwrap())
            .to_hex()
            .to_string()
    );
    let mut repair = p;
    repair.review.unknown_review = UnknownReviewPolicy::ReviseSupportedDefects;
    assert_ne!(
        FrozenContract::try_from(repair).unwrap().hash().unwrap(),
        decoded.hash().unwrap()
    );
}

#[test]
fn qualified_defect_can_permit_revision_without_acceptance_or_retention() {
    let mut p = proposal();
    p.review.unknown_review = UnknownReviewPolicy::ReviseSupportedDefects;
    let c: FrozenContract = p.try_into().unwrap();
    let ctx = context("disputed");
    let mut reviews = verdicts(&c, &ctx, 0.95, false);
    reviews[0] = verdicts(&c, &ctx, 0.95, true).remove(0);
    let candidate = aggregate(&c, &ctx, &reviews).unwrap();
    assert!(!candidate.complete && !candidate.feasible && candidate.quality_score.is_none());
    assert!(candidate.disagreement_repair_eligible);
    let retained = Retained {
        champion: Some(reviewed(&c, "prior", 0.7, true)),
        plateau_count: 2,
        ..Default::default()
    };
    let next = decide(&c, &retained, &candidate, &budget(1)).unwrap();
    assert_eq!(next.kind, DecisionKind::RepairAndRetry);
    assert_eq!(next.accepted_hash, None);
    assert_eq!(next.retained, retained);
    assert_eq!(
        decide(
            &c,
            &retained,
            &candidate,
            &budget(c.definition().budget.max_attempts)
        )
        .unwrap()
        .kind,
        DecisionKind::BudgetExhausted
    );
    let mut passing = verdicts(&c, &ctx, 0.95, true);
    passing[0].scores[0].score = Some(0.4);
    let quality_dispute = aggregate(&c, &ctx, &passing).unwrap();
    assert!(!quality_dispute.disagreement_repair_eligible);
    assert_eq!(
        decide(&c, &retained, &quality_dispute, &budget(1))
            .unwrap()
            .kind,
        DecisionKind::WaitingOwner
    );
}

#[test]
fn disagreement_revision_requires_every_review_to_be_complete_and_eligible() {
    let mut p = proposal();
    p.review.unknown_review = UnknownReviewPolicy::ReviseSupportedDefects;
    let c: FrozenContract = p.try_into().unwrap();
    let ctx = context("candidate");
    for fault in 0..6 {
        let mut reviews = verdicts(&c, &ctx, 0.95, false);
        reviews[0] = verdicts(&c, &ctx, 0.95, true).remove(0);
        match fault {
            0 => reviews[0].status = ReviewStatus::TimedOut,
            1 => reviews[0].scores[0].confidence = Some(0.79),
            2 => {
                reviews.remove(0);
            },
            3 => {
                reviews[0].scores[1]
                    .coverage
                    .as_mut()
                    .unwrap()
                    .whole_artifact_reviewed = false
            },
            4 => reviews[0].source_epoch = 1,
            5 => {
                let coverage = reviews[1].scores[1].coverage.as_mut().unwrap();
                let mut uncertain = coverage.claims[0].clone();
                uncertain.id = "uncertain-claim".into();
                uncertain.outcome = Outcome::Unknown;
                uncertain.sources.clear();
                coverage.claims.push(uncertain);
            },
            _ => unreachable!(),
        }
        let candidate = aggregate(&c, &ctx, &reviews).unwrap();
        assert!(!candidate.disagreement_repair_eligible, "fault {fault}");
        let decision = decide(&c, &Retained::default(), &candidate, &budget(1)).unwrap();
        assert_eq!(decision.kind, DecisionKind::WaitingOwner, "fault {fault}");
        assert!(decision.retained.artifact().is_none());
    }
}

#[test]
fn malformed_stale_missing_and_unavailable_reviews_never_accept() {
    let c = contract();
    let ctx = context("candidate");
    for fault in 0..14 {
        let mut reviews = verdicts(&c, &ctx, 0.95, true);
        match fault {
            0 => {
                reviews.remove(0);
            },
            1 => reviews.push(reviews[0].clone()),
            2 => {
                let duplicate = reviews[0].scores[0].clone();
                reviews[0].scores.push(duplicate);
            },
            3 => {
                reviews[0].scores.remove(0);
            },
            4 => reviews[0].scores[0].score = Some(f64::NAN),
            5 => reviews[0].scores[0].confidence = Some(0.79),
            6 => reviews[0].status = ReviewStatus::TimedOut,
            7 => reviews[0].source_epoch = 1,
            8 => reviews[0].critic_configuration_hash = hash("other-config"),
            9 => reviews[0].evidence_ids.push("invented-source".into()),
            10 => reviews[0].scores[0].score = Some(1.01),
            11 => reviews[0].scores[0].score = Some(-0.01),
            12 => reviews[0].scores[0].confidence = Some(1.01),
            13 => reviews[0].scores[0].confidence = Some(f64::NAN),
            _ => unreachable!(),
        }
        let candidate = aggregate(&c, &ctx, &reviews).unwrap();
        assert!(!candidate.complete, "fault {fault}");
        assert!(!candidate.feasible, "fault {fault}");
        assert!(candidate.quality_score.is_none(), "fault {fault}");
        let result = decide(&c, &Retained::default(), &candidate, &budget(1)).unwrap();
        assert_eq!(result.kind, DecisionKind::WaitingOwner, "fault {fault}");
        assert_eq!(result.retained.plateau_count, 0);
        assert!(result.retained.artifact().is_none());
    }
}

#[test]
fn confidence_is_eligibility_not_a_weight_discount() {
    let c = contract();
    let ctx = context("candidate");
    let mut reviews = verdicts(&c, &ctx, 0.7, true);
    reviews[0].scores[0].confidence = Some(0.8);
    reviews[1].scores[0].confidence = Some(1.0);
    assert_eq!(
        aggregate(&c, &ctx, &reviews).unwrap().quality_score,
        Some(0.7)
    );
}

#[test]
fn score_quorum_cannot_weaken_a_must_pass_panel() {
    let mut p = proposal();
    p.review.score_quorum = 2;
    let c: FrozenContract = p.try_into().unwrap();
    let ctx = context("candidate");
    let mut reviews = verdicts(&c, &ctx, 0.9, true);
    reviews[0].status = ReviewStatus::Unavailable;
    let result = aggregate(&c, &ctx, &reviews).unwrap();
    assert_eq!(result.criteria[0].score, Some(0.9));
    assert_eq!(result.criteria[1].outcome, Outcome::Unknown);
    assert!(!result.feasible);
}

#[test]
fn disagreement_and_excessive_spread_are_unknown() {
    let c = contract();
    let ctx = context("candidate");
    let mut reviews = verdicts(&c, &ctx, 0.9, true);
    reviews[0].scores[0].score = Some(0.6);
    assert_eq!(
        aggregate(&c, &ctx, &reviews).unwrap().criteria[0].outcome,
        Outcome::Unknown
    );
    let mut reviews = verdicts(&c, &ctx, 0.9, true);
    reviews[0].scores[1] = verdicts(&c, &ctx, 0.9, false)[0].scores[1].clone();
    assert_eq!(
        aggregate(&c, &ctx, &reviews).unwrap().criteria[1].outcome,
        Outcome::Unknown
    );
}

#[test]
fn zero_claims_needs_independent_confirmation() {
    let c = contract();
    let ctx = context("candidate");
    let mut reviews = verdicts(&c, &ctx, 0.9, true);
    for verdict in reviews.iter_mut().filter(|v| v.critic_id != "check") {
        verdict.scores[1].coverage = Some(ClaimCoverage {
            whole_artifact_reviewed: true,
            claims: vec![],
            confirmed_no_factual_claims: false,
        });
    }
    assert!(!aggregate(&c, &ctx, &reviews).unwrap().complete);
    for verdict in reviews.iter_mut().filter(|v| v.critic_id != "check") {
        verdict.scores[1]
            .coverage
            .as_mut()
            .unwrap()
            .confirmed_no_factual_claims = true;
    }
    assert!(aggregate(&c, &ctx, &reviews).unwrap().feasible);
}

#[test]
fn deadline_and_cancellation_override_a_late_pass() {
    let c = contract();
    let pass = reviewed(&c, "pass", 0.9, true);
    let mut ctx = budget(1);
    ctx.deadline_expired = true;
    assert_eq!(
        decide(&c, &Retained::default(), &pass, &ctx).unwrap().kind,
        DecisionKind::BudgetExhausted
    );
    ctx.cancelled = true;
    assert_eq!(
        decide(&c, &Retained::default(), &pass, &ctx).unwrap().kind,
        DecisionKind::Cancelled
    );
}

#[test]
fn must_pass_gains_reset_plateau_and_regressions_do_not_win() {
    let c = contract();
    let draft = reviewed(&c, "draft", 0.6, false);
    let initial = decide(&c, &Retained::default(), &draft, &budget(1)).unwrap();
    let duplicate = decide(&c, &initial.retained, &draft, &budget(2)).unwrap();
    assert_eq!(duplicate.retained.plateau_count, 1);
    let valid = reviewed(&c, "valid", 0.5, true);
    let gained = decide(&c, &duplicate.retained, &valid, &budget(3)).unwrap();
    assert_eq!(gained.retained.plateau_count, 0);
    let regression = reviewed(&c, "regression", 0.95, false);
    let lost = decide(&c, &gained.retained, &regression, &budget(4)).unwrap();
    assert_eq!(lost.kind, DecisionKind::RevertAndRetry);
    assert_eq!(
        lost.retained.champion.unwrap().artifact_hash,
        valid.artifact_hash
    );
}

#[test]
fn plateau_counts_only_comparable_complete_attempts_and_keeps_earlier_ties() {
    let c = contract();
    let draft = reviewed(&c, "first", 0.6, false);
    let mut retained = decide(&c, &Retained::default(), &draft, &budget(1))
        .unwrap()
        .retained;
    for attempt in 2..=4 {
        let tie = reviewed(&c, &format!("tie-{attempt}"), 0.61, false);
        let result = decide(&c, &retained, &tie, &budget(attempt)).unwrap();
        assert_eq!(
            result.kind,
            if attempt == 4 {
                DecisionKind::Plateau
            } else {
                DecisionKind::RevertAndRetry
            }
        );
        assert_eq!(
            result.retained.best_draft.as_ref().unwrap().artifact_hash,
            draft.artifact_hash
        );
        retained = result.retained;
    }
    let mut epoch = budget(5);
    epoch.source_epoch = 1;
    assert!(decide(&c, &retained, &draft, &epoch).is_err());
}

#[test]
fn deserialization_preserves_frozen_contract_validation() {
    let frozen = contract();
    let bytes = serde_json::to_vec(&frozen).unwrap();
    assert_eq!(
        serde_json::from_slice::<FrozenContract>(&bytes)
            .unwrap()
            .hash()
            .unwrap(),
        frozen.hash().unwrap()
    );
    let mut p = proposal();
    p.critics[0].model = Some(model("ACTOR"));
    assert!(serde_json::from_slice::<FrozenContract>(&serde_json::to_vec(&p).unwrap()).is_err());
}

#[test]
fn admission_rejects_unresolved_ambiguous_and_nonfinite_contracts() {
    for fault in 0..12 {
        let mut p = proposal();
        match fault {
            0 => p.criteria.clear(),
            1 => p.criteria.push(p.criteria[0].clone()),
            2 => p.criteria[0].weight = Some(f64::NAN),
            3 => p.criteria[0].weight = Some(0.0),
            4 => p.criteria[0].specification_hash = "latest".into(),
            5 => p.criteria[1].pass_score = None,
            6 => p.score_at_least = None,
            7 => p.review.score_quorum = 4,
            8 => p.critics[0].criteria.push("invented".into()),
            9 => p.critics[0].model = Some(model("actor")),
            10 => p.budget.max_wall_clock_secs = u64::MAX,
            11 => p.sources.push(p.sources[0].clone()),
            _ => unreachable!(),
        }
        assert!(p.validate().is_err(), "fault {fault}");
    }
}

#[test]
fn omitted_and_zero_budget_limits_are_unlimited_but_positive_caps_still_apply() {
    let partial: Budget = serde_json::from_value(serde_json::json!({"max_attempts": 2})).unwrap();
    assert_eq!(partial.max_cost_microusd, 0);
    assert_eq!(partial.attempt_ceiling(), 2);
    assert_eq!(partial.cost_ceiling(), u64::MAX);
    assert_eq!(
        partial.deadline_at(chrono::Utc::now()).unwrap(),
        chrono::DateTime::<chrono::Utc>::MAX_UTC
    );
    for invalid in [
        serde_json::json!(-1),
        serde_json::json!(1.5),
        serde_json::json!("unlimited"),
    ] {
        assert!(
            serde_json::from_value::<Budget>(serde_json::json!({"max_cost_microusd":invalid}))
                .is_err()
        );
    }
    let mut value = serde_json::to_value(proposal()).unwrap();
    value.as_object_mut().unwrap().remove("budget");
    let definition: DefinitionOfDone = serde_json::from_value(value).unwrap();
    assert_eq!(definition.budget, Budget::default());
    let c: FrozenContract = definition.try_into().unwrap();
    let candidate = reviewed(&c, "below-target", 0.5, true);
    let mut context = budget(100);
    context.spent_and_reserved_microusd = 9_000_000;
    let prior = Retained {
        plateau_count: 100,
        ..Default::default()
    };
    let next = decide(&c, &prior, &candidate, &context).unwrap();
    assert_eq!(next.kind, DecisionKind::KeepAndRetry);
    let next = decide(&c, &next.retained, &candidate, &context).unwrap();
    assert_eq!(next.kind, DecisionKind::RevertAndRetry);
    context.cancelled = true;
    assert_eq!(
        decide(&c, &prior, &candidate, &context).unwrap().kind,
        DecisionKind::Cancelled
    );
}

#[test]
fn median_and_fixed_weights_use_every_scored_criterion() {
    let mut p = proposal();
    p.criteria.push(Criterion {
        id: "concise".into(),
        kind: CriterionKind::Rubric,
        specification_hash: hash("concise-v1"),
        weight: Some(1.0),
        must_pass: false,
        pass_score: None,
    });
    for critic in p.critics.iter_mut().filter(|c| c.model.is_some()) {
        critic.criteria.push("concise".into());
    }
    let c: FrozenContract = p.try_into().unwrap();
    let ctx = context("candidate");
    let mut reviews = verdicts(&contract(), &ctx, 0.9, true);
    for (index, verdict) in reviews.iter_mut().enumerate() {
        verdict.contract_hash = c.hash().unwrap();
        if verdict.critic_id != "check" {
            verdict.scores[0].score = Some([0.8, 0.9, 0.95][index]);
            verdict.scores.push(CriterionScore {
                criterion_id: "concise".into(),
                outcome: Outcome::Pass,
                score: Some(0.5),
                confidence: Some(0.9),
                coverage: None,
            });
        }
    }
    let result = aggregate(&c, &ctx, &reviews).unwrap();
    assert!((result.quality_score.unwrap() - 0.8).abs() < 1e-12);
}

#[test]
fn grounding_requires_per_claim_support_from_exact_allowed_snapshot_bytes() {
    let c = contract();
    for fault in 0..7 {
        let mut ctx = context("candidate");
        let mut reviews = verdicts(&c, &ctx, 0.95, true);
        for v in reviews.iter_mut().filter(|v| v.critic_id != "check") {
            let coverage = v.scores[1].coverage.as_mut().unwrap();
            match fault {
                0 => coverage.claims[0].sources.clear(),
                1 => coverage.claims[0].sources[0].source_id = "unapproved-note".into(),
                2 => coverage.claims[0].sources[0].quote = "invented source quote".into(),
                3 => {
                    ctx.sources
                        .insert("owner-note".into(), b"changed source bytes".to_vec());
                },
                4 => coverage.claims[0].artifact_quote = "claim not present in draft".into(),
                5 => {
                    let duplicate = coverage.claims[0].clone();
                    coverage.claims.push(duplicate);
                },
                6 => coverage.whole_artifact_reviewed = false,
                _ => unreachable!(),
            }
        }
        let result = aggregate(&c, &ctx, &reviews).unwrap();
        assert_eq!(
            result.criteria[1].outcome,
            Outcome::Unknown,
            "fault {fault}"
        );
        assert!(!result.feasible, "fault {fault}");
    }
}
