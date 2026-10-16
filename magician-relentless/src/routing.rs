//! Independent human-review routing after artifact acceptance. An acting agent
//! may draft freely; it cannot decide its own review requirement or delivery
//! permission. Decision model providers (including Jev) share this typed seam.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

use super::{
    contract::{unit, valid_hash, FrozenContract, ModelPin},
    decision::{Decision, DecisionKind},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewRoute {
    HumanReview,
    AutomaticInternalHandoff,
    AutomaticCapture,
}

/// A recommendation returned by a paid independent review leg. Identity is
/// supplied by the controller's frozen critic pin, never by the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewRecommendation {
    pub route: ReviewRoute,
    pub confidence: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffKind {
    #[default]
    InternalDraft,
    Capture,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handoff {
    pub artifact_hash: String,
    pub kind: HandoffKind,
}

/// Resolved owner-granted policy, supplied by the authenticated admission
/// surface. A model may recommend a route only within this policy's bounds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingPolicy {
    pub version_hash: String,
    pub reviewer: ModelPin,
    pub min_confidence: f64,
    pub owner_review_required: bool,
    pub allow_automatic_internal: bool,
    pub allow_automatic_capture: bool,
}

impl RoutingPolicy {
    pub fn validate(&self, contract: &FrozenContract) -> Result<()> {
        ensure!(
            valid_hash(&self.version_hash) && unit(self.min_confidence),
            "invalid routing policy"
        );
        self.reviewer.validate()?;
        ensure!(
            !self
                .reviewer
                .family
                .trim()
                .eq_ignore_ascii_case(contract.definition().actor.family.trim()),
            "actor cannot route its own human review"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingVerdict {
    pub artifact_hash: String,
    pub contract_hash: String,
    pub policy_hash: String,
    pub model: ModelPin,
    pub route: ReviewRoute,
    pub confidence: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingDecision {
    pub artifact_hash: String,
    pub policy_hash: String,
    pub route: ReviewRoute,
    pub reason: String,
}

pub fn resolve(
    contract: &FrozenContract,
    accepted: &Decision,
    policy: &RoutingPolicy,
    verdict: Option<&RoutingVerdict>,
) -> Result<RoutingDecision> {
    policy.validate(contract)?;
    let artifact = accepted
        .retained
        .champion
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no accepted champion to route"))?;
    let contract_hash = contract.hash()?;
    ensure!(
        accepted.kind == DecisionKind::Accept
            && accepted.accepted_hash.as_ref() == Some(&artifact.artifact_hash)
            && artifact.feasible
            && artifact.contract_hash == contract_hash
            && contract
                .definition()
                .score_at_least
                .is_none_or(|threshold| artifact
                    .quality_score
                    .is_some_and(|score| score >= threshold)),
        "routing requires an accepted exact artifact"
    );
    let mut route = ReviewRoute::HumanReview;
    let reason = if policy.owner_review_required {
        "owner policy requires human review"
    } else if let Some(verdict) = verdict {
        if verdict.artifact_hash != artifact.artifact_hash
            || verdict.contract_hash != contract_hash
            || verdict.policy_hash != policy.version_hash
            || verdict.model != policy.reviewer
            || !unit(verdict.confidence)
            || verdict.confidence < policy.min_confidence
        {
            "independent routing identity or confidence is unknown"
        } else {
            match verdict.route {
                ReviewRoute::AutomaticInternalHandoff if policy.allow_automatic_internal => {
                    route = verdict.route;
                    "independent model selected automatic internal handoff within owner policy"
                },
                ReviewRoute::AutomaticCapture if policy.allow_automatic_capture => {
                    route = verdict.route;
                    "independent model selected automatic capture within owner policy; nothing is sent"
                },
                ReviewRoute::HumanReview => "independent model requested human review",
                _ => "model recommendation exceeds owner-granted routing policy",
            }
        }
    } else {
        "independent routing unavailable; human review required"
    };
    Ok(RoutingDecision {
        artifact_hash: artifact.artifact_hash.clone(),
        policy_hash: policy.version_hash.clone(),
        route,
        reason: reason.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        decision::{decide, Retained},
        tests::*,
    };

    fn fixture() -> (FrozenContract, Decision, RoutingPolicy, RoutingVerdict) {
        let c = contract();
        let ctx = context("accepted-draft");
        let candidate = crate::review::aggregate(&c, &ctx, &verdicts(&c, &ctx, 0.9, true)).unwrap();
        let accepted = decide(&c, &Retained::default(), &candidate, &budget(1)).unwrap();
        let policy = RoutingPolicy {
            version_hash: hash("owner policy"),
            reviewer: c.definition().critics[0].model.clone().unwrap(),
            min_confidence: 0.8,
            owner_review_required: false,
            allow_automatic_internal: true,
            allow_automatic_capture: true,
        };
        let verdict = RoutingVerdict {
            artifact_hash: candidate.artifact_hash,
            contract_hash: c.hash().unwrap(),
            policy_hash: policy.version_hash.clone(),
            model: policy.reviewer.clone(),
            route: ReviewRoute::AutomaticInternalHandoff,
            confidence: 0.9,
        };
        (c, accepted, policy, verdict)
    }

    #[test]
    fn independent_model_can_route_a_draft_without_per_draft_human_review() {
        let (c, accepted, policy, mut verdict) = fixture();
        assert_eq!(
            resolve(&c, &accepted, &policy, Some(&verdict))
                .unwrap()
                .route,
            ReviewRoute::AutomaticInternalHandoff
        );
        verdict.route = ReviewRoute::AutomaticCapture;
        assert_eq!(
            resolve(&c, &accepted, &policy, Some(&verdict))
                .unwrap()
                .route,
            ReviewRoute::AutomaticCapture
        );
    }

    #[test]
    fn missing_stale_low_confidence_or_self_routing_requires_human_review() {
        let (c, accepted, policy, verdict) = fixture();
        assert_eq!(
            resolve(&c, &accepted, &policy, None).unwrap().route,
            ReviewRoute::HumanReview
        );
        for fault in 0..7 {
            let mut v = verdict.clone();
            match fault {
                0 => v.artifact_hash = hash("changed payload"),
                1 => v.policy_hash = hash("old policy"),
                2 => v.confidence = 0.79,
                3 => v.confidence = f64::NAN,
                4 => v.model = c.definition().actor.clone(),
                5 => v.confidence = 1.01,
                6 => v.confidence = -0.01,
                _ => unreachable!(),
            }
            assert_eq!(
                resolve(&c, &accepted, &policy, Some(&v)).unwrap().route,
                ReviewRoute::HumanReview
            );
        }
    }

    #[test]
    fn route_cannot_override_policy_or_turn_a_below_threshold_champion_into_acceptance() {
        let (c, mut accepted, mut policy, verdict) = fixture();
        policy.owner_review_required = true;
        assert_eq!(
            resolve(&c, &accepted, &policy, Some(&verdict))
                .unwrap()
                .route,
            ReviewRoute::HumanReview
        );
        policy.owner_review_required = false;
        policy.allow_automatic_internal = false;
        assert_eq!(
            resolve(&c, &accepted, &policy, Some(&verdict))
                .unwrap()
                .route,
            ReviewRoute::HumanReview
        );
        accepted.kind = DecisionKind::BudgetExhausted;
        assert!(resolve(&c, &accepted, &policy, Some(&verdict)).is_err());
    }
}
