//! Validate pinned reviews before aggregation. Unknown never supplies a pass or
//! reduces a difficult criterion's fixed weight.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::contract::{unit, CriterionKind, CriticPin, FrozenContract};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Pass,
    Fail,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Complete,
    Unavailable,
    Malformed,
    TimedOut,
}

/// Independent inventory of the entire draft. Actor citations do not establish
/// coverage; an empty inventory alone cannot establish a claim-free artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimCoverage {
    pub whole_artifact_reviewed: bool,
    pub claims: Vec<ClaimReview>,
    pub confirmed_no_factual_claims: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimReview {
    pub id: String,
    pub artifact_quote: String,
    pub outcome: Outcome,
    pub sources: Vec<ClaimSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimSource {
    pub source_id: String,
    pub quote: String,
}

impl ClaimCoverage {
    fn validate_binding(
        &self,
        context: &ReviewContext,
        contract: &FrozenContract,
    ) -> Result<(), String> {
        if !self.whole_artifact_reviewed
            || (self.claims.is_empty() && !self.confirmed_no_factual_claims)
            || (self.confirmed_no_factual_claims && !self.claims.is_empty())
        {
            return Err("grounding inventory is incomplete or contradictory".into());
        }
        let mut ids = BTreeSet::new();
        for claim in &self.claims {
            if claim.id.trim().is_empty() || !ids.insert(&claim.id) {
                return Err("grounding claim identity is missing or duplicated".into());
            }
            if claim.artifact_quote.trim().is_empty()
                || !context
                    .artifact_texts
                    .iter()
                    .any(|text| text.contains(&claim.artifact_quote))
            {
                return Err(format!(
                    "claim {}: artifact quote is not an exact sealed substring",
                    claim.id
                ));
            }
            if claim.outcome == Outcome::Pass && claim.sources.is_empty() {
                return Err(format!(
                    "claim {}: supported claim omitted source evidence",
                    claim.id
                ));
            }
            for support in &claim.sources {
                let source = contract
                    .definition()
                    .sources
                    .iter()
                    .find(|pin| pin.id == support.source_id);
                let bytes = context.sources.get(&support.source_id);
                let allowed = match (source, bytes) {
                    (Some(pin), Some(bytes)) => {
                        blake3::hash(bytes).to_hex().as_str() == pin.content_hash
                    },
                    (None, Some(_)) => {
                        contract.definition().collected_source_limits.is_some()
                            && support.source_id.starts_with("collected:")
                            && context.evidence_ids.contains(&support.source_id)
                    },
                    _ => false,
                };
                if !allowed
                    || !bytes.is_some_and(|bytes| {
                        !support.quote.trim().is_empty()
                            && bytes
                                .windows(support.quote.len())
                                .any(|span| span == support.quote.as_bytes())
                    })
                {
                    return Err(format!(
                        "claim {}: source quote is not bound to the allowed sealed source",
                        claim.id
                    ));
                }
            }
        }
        Ok(())
    }

    fn outcome(&self, context: &ReviewContext, contract: &FrozenContract) -> Outcome {
        if self.validate_binding(context, contract).is_err() {
            Outcome::Unknown
        } else if self
            .claims
            .iter()
            .any(|claim| claim.outcome == Outcome::Fail)
        {
            Outcome::Fail
        } else if self
            .claims
            .iter()
            .any(|claim| claim.outcome == Outcome::Unknown)
        {
            Outcome::Unknown
        } else {
            Outcome::Pass
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CriterionScore {
    pub criterion_id: String,
    pub outcome: Outcome,
    pub score: Option<f64>,
    pub confidence: Option<f64>,
    pub coverage: Option<ClaimCoverage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub criterion_id: String,
    pub location: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verdict {
    pub critic_id: String,
    pub critic_configuration_hash: String,
    pub contract_hash: String,
    pub artifact_hash: String,
    pub source_epoch: u64,
    pub status: ReviewStatus,
    pub scores: Vec<CriterionScore>,
    pub findings: Vec<Finding>,
    pub evidence_ids: Vec<String>,
    #[serde(default)]
    pub review_recommendation: Option<super::routing::ReviewRecommendation>,
}

/// All identities are resolved by the controller from sealed bytes. Evidence
/// IDs include controller-sealed source, media-sample and check-receipt refs.
pub struct ReviewContext {
    pub artifact_hash: String,
    pub source_epoch: u64,
    pub evidence_ids: BTreeSet<String>,
    pub artifact_texts: Vec<String>,
    pub sources: BTreeMap<String, Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CriterionAggregate {
    pub criterion_id: String,
    pub outcome: Outcome,
    pub score: Option<f64>,
    pub eligible_critics: Vec<String>,
    pub reasons: Vec<String>,
}

/// Constructed only by aggregation, never deserialized from an actor output.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReviewedArtifact {
    pub artifact_hash: String,
    pub contract_hash: String,
    pub source_epoch: u64,
    pub criteria: Vec<CriterionAggregate>,
    pub complete: bool,
    pub feasible: bool,
    pub quality_score: Option<f64>,
    pub passed_must_pass: BTreeSet<String>,
    /// All required reviews were eligible; disagreement is the only unknown,
    /// and at least one qualified reviewer failed a required criterion.
    pub disagreement_repair_eligible: bool,
}

pub(super) fn validate_verdict(
    pin: &CriticPin,
    verdict: &Verdict,
    contract: &FrozenContract,
    contract_hash: &str,
    context: &ReviewContext,
) -> Result<(), String> {
    let fail = |message: &str| Err(message.to_string());
    if verdict.status != ReviewStatus::Complete {
        return fail("review did not complete");
    }
    if verdict.critic_configuration_hash != pin.configuration_hash
        || verdict.contract_hash != contract_hash
        || verdict.artifact_hash != context.artifact_hash
        || verdict.source_epoch != context.source_epoch
    {
        return fail("review identity/configuration does not match sealed input");
    }
    let ids: BTreeSet<_> = verdict.scores.iter().map(|s| &s.criterion_id).collect();
    let assigned: BTreeSet<_> = pin.criteria.iter().collect();
    if ids != assigned || ids.len() != verdict.scores.len() {
        return fail("missing, duplicate or unassigned criterion");
    }
    if verdict
        .evidence_ids
        .iter()
        .any(|id| !context.evidence_ids.contains(id))
        || verdict
            .findings
            .iter()
            .any(|f| !assigned.contains(&f.criterion_id))
    {
        return fail("unbound evidence or finding");
    }
    for score in &verdict.scores {
        let criterion = contract
            .definition()
            .criteria
            .iter()
            .find(|c| c.id == score.criterion_id)
            .expect("validated critic assignments");
        if score.score.is_some_and(|s| !unit(s)) || score.confidence.is_some_and(|c| !unit(c)) {
            return fail("non-finite or out-of-range score/confidence");
        }
        if criterion.kind == CriterionKind::Check {
            if score.score.is_some() || score.confidence.is_some() || score.coverage.is_some() {
                return fail("deterministic assertion has model scoring fields");
            }
        } else {
            if score.score.is_none() || score.confidence.is_none() {
                return fail("model criterion omitted score/confidence");
            }
            if criterion.kind != CriterionKind::Grounding && score.coverage.is_some() {
                return fail("claim coverage on non-grounding criterion");
            }
            if criterion.kind == CriterionKind::Grounding {
                let coverage = score
                    .coverage
                    .as_ref()
                    .ok_or_else(|| "grounding inventory missing".to_string())?;
                coverage.validate_binding(context, contract)?;
                if score.outcome != Outcome::Unknown
                    && coverage.outcome(context, contract) != score.outcome
                {
                    return fail("grounding verdict contradicts its claim inventory");
                }
            }
            if let Some(threshold) = criterion.pass_score {
                let expected = if score.score.expect("checked") >= threshold {
                    Outcome::Pass
                } else {
                    Outcome::Fail
                };
                if score.outcome != Outcome::Unknown && score.outcome != expected {
                    return fail("outcome contradicts pinned pass threshold");
                }
            }
        }
    }
    Ok(())
}

pub fn aggregate(
    contract: &FrozenContract,
    context: &ReviewContext,
    verdicts: &[Verdict],
) -> anyhow::Result<ReviewedArtifact> {
    anyhow::ensure!(
        super::contract::valid_hash(&context.artifact_hash),
        "invalid sealed artifact hash"
    );
    let contract_hash = contract.hash()?;
    let dod = contract.definition();
    let mut reviews = BTreeMap::new();
    for pin in &dod.critics {
        let matching: Vec<_> = verdicts.iter().filter(|v| v.critic_id == pin.id).collect();
        let result = if matching.len() != 1 {
            Err("missing or duplicate critic verdict".to_string())
        } else {
            validate_verdict(pin, matching[0], contract, &contract_hash, context)
                .map(|()| matching[0])
        };
        reviews.insert(pin.id.as_str(), result);
    }
    // Extraneous reviews cannot change panel composition or poison a leg, but
    // are never included in any aggregate.
    let mut aggregates = Vec::new();
    let mut all_reviews_eligible = reviews.values().all(Result::is_ok);
    let mut required_failure = false;
    for criterion in &dod.criteria {
        let mut eligible = Vec::new();
        let mut reasons = Vec::new();
        for pin in dod
            .critics
            .iter()
            .filter(|p| p.criteria.contains(&criterion.id))
        {
            match &reviews[pin.id.as_str()] {
                Err(reason) => reasons.push(format!("{}: {reason}", pin.id)),
                Ok(verdict) => {
                    let score = verdict
                        .scores
                        .iter()
                        .find(|s| s.criterion_id == criterion.id)
                        .expect("validated scores");
                    if criterion.kind == CriterionKind::Grounding
                        && score.coverage.as_ref().is_some_and(|coverage| {
                            coverage
                                .claims
                                .iter()
                                .any(|claim| claim.outcome == Outcome::Unknown)
                        })
                    {
                        all_reviews_eligible = false;
                    }
                    if score.outcome == Outcome::Unknown {
                        reasons.push(format!("{}: criterion unknown", pin.id));
                    } else if criterion.kind != CriterionKind::Check
                        && score.confidence.expect("validated confidence")
                            < dod.review.min_confidence
                    {
                        reasons.push(format!("{}: confidence below floor", pin.id));
                    } else if criterion.kind == CriterionKind::Grounding
                        && score
                            .coverage
                            .as_ref()
                            .map(|coverage| coverage.outcome(context, contract))
                            != Some(score.outcome)
                    {
                        reasons.push(format!(
                            "{}: full claim coverage unavailable or contradicts verdict",
                            pin.id
                        ));
                    } else {
                        eligible.push((pin.id.clone(), score));
                    }
                },
            }
        }
        let required = if criterion.kind == CriterionKind::Check {
            1
        } else if criterion.must_pass {
            dod.review.panel_size as usize
        } else {
            dod.review.score_quorum as usize
        };
        all_reviews_eligible &= reasons.is_empty();
        required_failure |= criterion.must_pass
            && eligible
                .iter()
                .any(|(_, score)| score.outcome == Outcome::Fail);
        let mut outcome = Outcome::Unknown;
        let mut median = None;
        if eligible.len() >= required {
            if criterion.kind == CriterionKind::Check {
                outcome = eligible[0].1.outcome;
            } else {
                let mut scores: Vec<_> = eligible
                    .iter()
                    .map(|(_, s)| s.score.expect("validated score"))
                    .collect();
                scores.sort_by(f64::total_cmp);
                if scores[scores.len() - 1] - scores[0] > dod.review.max_score_spread {
                    reasons.push("panel score spread exceeds frozen limit".into());
                } else if criterion.must_pass
                    && eligible
                        .iter()
                        .any(|(_, s)| s.outcome != eligible[0].1.outcome)
                {
                    reasons.push("must-pass panel disagrees".into());
                } else {
                    let middle = scores.len() / 2;
                    median = Some(if scores.len() % 2 == 0 {
                        (scores[middle - 1] + scores[middle]) / 2.0
                    } else {
                        scores[middle]
                    });
                    outcome = if criterion.must_pass {
                        eligible[0].1.outcome
                    } else {
                        Outcome::Pass
                    };
                }
            }
        } else {
            reasons.push("eligible critic quorum not met".into());
        }
        aggregates.push(CriterionAggregate {
            criterion_id: criterion.id.clone(),
            outcome,
            score: criterion.weight.and(median),
            eligible_critics: eligible.into_iter().map(|(id, _)| id).collect(),
            reasons,
        });
    }
    let complete = aggregates.iter().all(|a| a.outcome != Outcome::Unknown);
    let passed_must_pass: BTreeSet<_> = dod
        .criteria
        .iter()
        .zip(&aggregates)
        .filter(|(c, a)| c.must_pass && a.outcome == Outcome::Pass)
        .map(|(c, _)| c.id.clone())
        .collect();
    let feasible = complete
        && dod
            .criteria
            .iter()
            .filter(|c| c.must_pass)
            .all(|c| passed_must_pass.contains(&c.id));
    // Scale weights before multiplication to avoid overflow at valid finite
    // weights. All required scored criteria must remain in the denominator.
    let weight_sum: f64 = dod.criteria.iter().filter_map(|c| c.weight).sum();
    let quality_score = if complete && weight_sum > 0.0 {
        Some(
            dod.criteria
                .iter()
                .zip(&aggregates)
                .filter_map(|(c, a)| {
                    c.weight
                        .map(|w| (w / weight_sum) * a.score.expect("complete scored aggregate"))
                })
                .sum(),
        )
    } else {
        None
    };
    Ok(ReviewedArtifact {
        artifact_hash: context.artifact_hash.clone(),
        contract_hash,
        source_epoch: context.source_epoch,
        criteria: aggregates,
        complete,
        feasible,
        quality_score,
        passed_must_pass,
        disagreement_repair_eligible: !complete && all_reviews_eligible && required_failure,
    })
}
