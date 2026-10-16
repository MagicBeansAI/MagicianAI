//! Pure decision reducer. No agent prose or task-specific tests influence it.
use serde::{Deserialize, Serialize};

use super::{
    contract::{FrozenContract, UnknownReviewPolicy},
    review::ReviewedArtifact,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Accept,
    KeepAndRetry,
    RevertAndRetry,
    RepairAndRetry,
    Plateau,
    BudgetExhausted,
    WaitingOwner,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Retained {
    pub champion: Option<ReviewedArtifact>,
    pub best_draft: Option<ReviewedArtifact>,
    pub plateau_count: u32,
}

impl Retained {
    pub fn artifact(&self) -> Option<&ReviewedArtifact> {
        self.champion.as_ref().or(self.best_draft.as_ref())
    }
}

/// Measured controller state, never an actor's self-reported remaining budget.
pub struct DecisionContext {
    pub attempts_started: u32,
    pub spent_and_reserved_microusd: u64,
    pub source_epoch: u64,
    pub deadline_expired: bool,
    pub cancelled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Decision {
    pub kind: DecisionKind,
    pub retained: Retained,
    pub accepted_hash: Option<String>,
    pub reason: String,
}

fn better_score(new: Option<f64>, old: Option<f64>, margin: f64) -> bool {
    matches!((new, old), (Some(n), Some(o)) if n - o > margin)
}

pub fn decide(
    contract: &FrozenContract,
    prior: &Retained,
    candidate: &ReviewedArtifact,
    context: &DecisionContext,
) -> anyhow::Result<Decision> {
    decide_with_budget(
        contract,
        &contract.definition().budget,
        prior,
        candidate,
        context,
    )
}

pub fn decide_with_budget(
    contract: &FrozenContract,
    budget: &super::contract::Budget,
    prior: &Retained,
    candidate: &ReviewedArtifact,
    context: &DecisionContext,
) -> anyhow::Result<Decision> {
    let dod = contract.definition();
    let hash = contract.hash()?;
    anyhow::ensure!(
        candidate.contract_hash == hash && candidate.source_epoch == context.source_epoch,
        "candidate review is not comparable with frozen contract/source epoch"
    );
    for artifact in [prior.champion.as_ref(), prior.best_draft.as_ref()]
        .into_iter()
        .flatten()
    {
        anyhow::ensure!(
            artifact.complete
                && artifact.contract_hash == hash
                && artifact.source_epoch == context.source_epoch,
            "retained review must be re-reviewed before comparison"
        );
    }
    anyhow::ensure!(
        prior.champion.as_ref().is_none_or(|a| a.feasible),
        "invalid champion"
    );
    let mut retained = prior.clone();
    let result = |kind, retained, accepted_hash, reason: &str| {
        Ok(Decision {
            kind,
            retained,
            accepted_hash,
            reason: reason.to_string(),
        })
    };
    // A result arriving after a durable deadline or owner cancellation cannot
    // retroactively accept. A just-completed acceptance otherwise wins over
    // attempt/cost/plateau ceilings.
    if context.cancelled {
        return result(
            DecisionKind::Cancelled,
            retained,
            None,
            "owner cancelled the run",
        );
    }
    if context.deadline_expired {
        return result(
            DecisionKind::BudgetExhausted,
            retained,
            None,
            "elapsed wall-clock deadline reached",
        );
    }
    let exhausted = context.attempts_started >= budget.max_attempts
        || context.spent_and_reserved_microusd >= budget.max_cost_microusd;
    if !candidate.complete {
        if dod.review.unknown_review == UnknownReviewPolicy::ReviseSupportedDefects
            && candidate.disagreement_repair_eligible
        {
            return result(
                if exhausted {
                    DecisionKind::BudgetExhausted
                } else {
                    DecisionKind::RepairAndRetry
                },
                retained,
                None,
                if exhausted {
                    "attempt or cost ceiling reached; disputed artifact remains unaccepted"
                } else {
                    "review remains unknown; qualified required-criterion defect permits revision without retained improvement or plateau credit"
                },
            );
        }
        return result(
            if exhausted {
                DecisionKind::BudgetExhausted
            } else {
                DecisionKind::WaitingOwner
            },
            retained,
            None,
            "independent review unknown; no retained improvement or plateau counted",
        );
    }
    let accepted = candidate.feasible
        && dod
            .score_at_least
            .is_none_or(|threshold| candidate.quality_score.is_some_and(|s| s >= threshold));
    if accepted {
        retained.champion = Some(candidate.clone());
        retained.plateau_count = 0;
        return result(
            DecisionKind::Accept,
            retained,
            Some(candidate.artifact_hash.clone()),
            "exact candidate satisfies every required criterion and acceptance threshold",
        );
    }
    let improved = if candidate.feasible {
        if retained.champion.as_ref().is_none_or(|old| {
            better_score(
                candidate.quality_score,
                old.quality_score,
                dod.review.noise_margin,
            )
        }) {
            retained.champion = Some(candidate.clone());
            true
        } else {
            false
        }
    } else if retained.champion.is_some() {
        false
    } else {
        let wins = retained.best_draft.as_ref().is_none_or(|old| {
            (candidate
                .passed_must_pass
                .is_superset(&old.passed_must_pass)
                && candidate.passed_must_pass != old.passed_must_pass)
                || (candidate.passed_must_pass == old.passed_must_pass
                    && better_score(
                        candidate.quality_score,
                        old.quality_score,
                        dod.review.noise_margin,
                    ))
        });
        if wins {
            retained.best_draft = Some(candidate.clone());
        }
        wins
    };
    retained.plateau_count = if improved {
        0
    } else {
        retained.plateau_count.saturating_add(1)
    };
    if exhausted {
        return result(
            DecisionKind::BudgetExhausted,
            retained,
            None,
            "attempt or settled-plus-reserved cost ceiling reached; artifact is not accepted",
        );
    }
    if retained.plateau_count >= dod.budget.plateau_attempts {
        return result(
            DecisionKind::Plateau,
            retained,
            None,
            "comparable completed attempts did not improve retained artifact",
        );
    }
    result(
        if improved {
            DecisionKind::KeepAndRetry
        } else {
            DecisionKind::RevertAndRetry
        },
        retained,
        None,
        if improved {
            "retained validity or quality improved; acceptance criteria still unmet"
        } else {
            "retain earlier artifact; candidate did not improve validity or quality above noise margin"
        },
    )
}
