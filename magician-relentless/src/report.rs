//! Read-only reports derived from the controller journal. Reports cannot
//! accept an artifact, change its review or authorize a handoff.
use std::collections::BTreeMap;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::{
    budget::{Settlement, Spend},
    contract::Budget,
    review::{CriterionAggregate, Outcome, ReviewedArtifact},
    routing::{Handoff, ReviewRoute},
    store::{RunState, RunStatus},
};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputRole {
    Retained,
    EarlierSourceReference,
    LatestCandidate,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReportOutput {
    pub artifact_hash: String,
    pub files: BTreeMap<String, String>,
    pub attempt_number: u32,
    pub role: OutputRole,
    pub review: Option<ReviewedArtifact>,
    pub locally_accepted: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReportFinding {
    pub critic_id: String,
    pub criterion_id: String,
    pub location: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunReport {
    pub version: &'static str,
    pub run_id: Uuid,
    pub task_id: String,
    pub goal: String,
    pub contract_hash: String,
    pub evaluation_contract_hash: String,
    pub source_epoch: u64,
    pub generation: u64,
    pub status: RunStatus,
    pub started_at: DateTime<Utc>,
    pub deadline_at: DateTime<Utc>,
    pub reason: String,
    pub attempts_used: usize,
    pub budget: Budget,
    pub original_budget: Budget,
    pub budget_resets: Vec<crate::budget_round::ApprovedBudgetReset>,
    pub spend: Spend,
    pub elapsed_wall_clock_secs: u64,
    pub time_allowance_used_secs: u64,
    pub unsettled_operation_ids: Vec<String>,
    pub output: Option<ReportOutput>,
    pub criteria: Vec<CriterionAggregate>,
    pub findings: Vec<ReportFinding>,
    pub open_items: Vec<String>,
    pub owner_approved: Option<bool>,
    pub handoff: Option<Handoff>,
    /// Draft/capture completion is not a confirmed external outcome.
    pub outward_delivery_confirmed: bool,
}

impl RunReport {
    pub fn from_state(state: &RunState) -> Result<Self> {
        let retained = state.retained.artifact();
        let reference = state.reference_artifact.as_ref();
        let selected = retained
            .map(|review| {
                (
                    review.artifact_hash.as_str(),
                    OutputRole::Retained,
                    Some(review),
                )
            })
            .or_else(|| {
                reference.map(|review| {
                    (
                        review.artifact_hash.as_str(),
                        OutputRole::EarlierSourceReference,
                        Some(review),
                    )
                })
            })
            .or_else(|| {
                state.attempts.iter().rev().find_map(|attempt| {
                    attempt.artifact.as_ref().map(|artifact| {
                        (
                            artifact.hash.as_str(),
                            OutputRole::LatestCandidate,
                            attempt.review.as_ref(),
                        )
                    })
                })
            });
        let attempt = selected
            .map(|(hash, _, review)| {
                state
                    .attempts
                    .iter()
                    .rev()
                    .find(|attempt| {
                        attempt
                            .artifact
                            .as_ref()
                            .is_some_and(|artifact| artifact.hash == hash)
                            && review.is_none_or(|review| attempt.review.as_ref() == Some(review))
                    })
                    .context("report artifact has no sealed attempt")
            })
            .transpose()?;
        let output = selected
            .zip(attempt)
            .map(|((hash, role, review), attempt)| ReportOutput {
                artifact_hash: hash.to_owned(),
                files: attempt
                    .artifact
                    .as_ref()
                    .expect("selected artifact")
                    .files
                    .clone(),
                attempt_number: attempt.number,
                role,
                review: review.cloned(),
                locally_accepted: state.status == RunStatus::Accepted
                    && state
                        .decisions
                        .iter()
                        .rev()
                        .find_map(|decision| decision.accepted_hash.as_deref())
                        == Some(hash),
            });
        let criteria = output
            .as_ref()
            .and_then(|output| output.review.as_ref())
            .map(|review| review.criteria.clone())
            .unwrap_or_else(|| {
                state
                    .contract
                    .definition()
                    .criteria
                    .iter()
                    .map(|criterion| CriterionAggregate {
                        criterion_id: criterion.id.clone(),
                        outcome: Outcome::Unknown,
                        score: None,
                        eligible_critics: vec![],
                        reasons: vec!["No completed aggregate review for this output".into()],
                    })
                    .collect()
            });
        let findings = attempt
            .into_iter()
            .flat_map(|attempt| {
                attempt.verdicts.iter().flat_map(|(critic, verdict)| {
                    verdict.findings.iter().map(move |finding| ReportFinding {
                        critic_id: critic.clone(),
                        criterion_id: finding.criterion_id.clone(),
                        location: finding.location.clone(),
                        detail: finding.detail.clone(),
                    })
                })
            })
            .collect();
        let spend = state.budget.spend()?;
        let time_usage = state.budget_usage(state.last_committed_at)?;
        let mut open_items = Vec::new();
        if let Some(output) = &output {
            if !output.locally_accepted {
                open_items.push(
                    "The selected artifact is available for inspection and has not been accepted"
                        .into(),
                );
            }
            if matches!(output.role, OutputRole::EarlierSourceReference) {
                open_items.push("This artifact used earlier sources; its review does not cover the current source epoch".into());
            }
            for criterion in &criteria {
                if criterion.outcome != Outcome::Pass {
                    open_items.push(format!(
                        "{}: {}{}",
                        criterion.criterion_id,
                        outcome(criterion.outcome),
                        if criterion.reasons.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", criterion.reasons.join("; "))
                        }
                    ));
                }
            }
            if output
                .review
                .as_ref()
                .is_some_and(|review| review.complete && review.feasible)
                && !output.locally_accepted
            {
                open_items.push("Required validity checks passed, but acceptance was not reached under the frozen quality and stop rules".into());
            }
        } else {
            open_items.push("No candidate artifact was sealed".into());
        }
        if state.status != RunStatus::Accepted {
            if let Some(failure) = state
                .attempts
                .last()
                .and_then(|attempt| attempt.failure.as_ref())
            {
                open_items.push(format!(
                    "Latest actor attempt produced no valid declared candidate: {failure}"
                ));
            }
        }
        if state.status == RunStatus::WaitingOwner {
            open_items.push(
                state
                    .reason
                    .clone()
                    .unwrap_or_else(|| "Owner input or reconciliation is required".into()),
            );
        }
        if state.owner_approved == Some(false) {
            open_items.push("The owner declined handoff of the exact accepted artifact".into());
        } else if state.status == RunStatus::Accepted && state.handoff.is_none() {
            open_items.push(
                if state
                    .routing
                    .as_ref()
                    .is_some_and(|routing| routing.route == ReviewRoute::HumanReview)
                    && state.owner_approved != Some(true)
                {
                    "The accepted artifact awaits exact owner handoff review"
                } else {
                    "Local handoff projection is pending"
                }
                .into(),
            );
        }
        if spend.reserved_microusd > 0 {
            open_items.push("Reserved charges remain unsettled; unavailable usage proofs retain their original ceiling".into());
        }
        Ok(Self {
            version: "relentless-run-report-v1",
            run_id: state.id,
            task_id: state.admission.task_id.clone(),
            goal: state.contract.definition().goal.clone(),
            contract_hash: state.contract.hash()?,
            evaluation_contract_hash: state.evaluation_contract()?.hash()?,
            source_epoch: state.source_epoch,
            generation: state.generation,
            status: state.status,
            started_at: state.started_at,
            deadline_at: state.deadline_at,
            reason: state
                .reason
                .clone()
                .or_else(|| {
                    state
                        .decisions
                        .last()
                        .map(|decision| decision.reason.clone())
                })
                .unwrap_or_else(|| "Execution or independent review is in progress".into()),
            attempts_used: state.attempts.len(),
            budget: state.effective_budget()?,
            original_budget: state.contract.definition().budget.clone(),
            budget_resets: state.budget_resets.clone(),
            spend,
            elapsed_wall_clock_secs: time_usage["cumulative"]["elapsed_wall_clock_secs"]
                .as_u64()
                .context("report elapsed time missing")?,
            time_allowance_used_secs: time_usage["cumulative"]["time_allowance_used_secs"]
                .as_u64()
                .context("report time allowance usage missing")?,
            unsettled_operation_ids: state
                .budget
                .operations()
                .iter()
                .filter(|(_, operation)| {
                    matches!(
                        &operation.settlement,
                        Settlement::Reserved | Settlement::OutcomeUnknown
                    )
                })
                .map(|(id, _)| id.clone())
                .collect(),
            output,
            criteria,
            findings,
            open_items,
            owner_approved: state.owner_approved,
            handoff: state.handoff.clone(),
            outward_delivery_confirmed: false,
        })
    }

    pub fn hash(&self) -> Result<String> {
        Ok(blake3::hash(&serde_json::to_vec(self)?)
            .to_hex()
            .to_string())
    }

    pub fn markdown(&self) -> String {
        let mut text = format!(
            "# Relentless run report\n\n{}\n\nRun: `{}` · task: {}\n\nStatus: {}\n\n{}\n\n",
            line(&self.goal),
            self.run_id,
            line(&self.task_id),
            label(self.status),
            line(&self.reason)
        );
        text.push_str(&format!("Attempts: {}/{} · settled: {} micro-USD · reserved: {} micro-USD · ceiling: {} micro-USD\n\nSource epoch: {} · journal generation: {}\n\n",
            self.attempts_used, if self.budget.max_attempts==0 { "Unlimited".into() } else { self.budget.max_attempts.to_string() }, self.spend.settled_microusd, self.spend.reserved_microusd,
            if self.budget.max_cost_microusd==0 { "Unlimited".into() } else { self.budget.max_cost_microusd.to_string() }, self.source_epoch, self.generation));
        text.push_str(&format!("Elapsed wall time: {} seconds (including waits). Time allowance used across approved windows: {} seconds.\n\n",self.elapsed_wall_clock_secs,self.time_allowance_used_secs));
        if !self.budget_resets.is_empty() {
            text.push_str("## Approved allowance resets\n\nTotals above include every round. Only the selected exhausted allowances reset.\n\n");
            for grant in &self.budget_resets {
                text.push_str(&format!("- {} by {}: cost {:?} micro-USD; time {:?} seconds; attempts {:?}. Before approval: {} attempts, {} settled micro-USD, {} reserved micro-USD.\n",
                    grant.approved_at, line(&grant.approved_by), grant.reset.cost_microusd,
                    grant.reset.wall_clock_secs, grant.reset.attempts, grant.attempts_before,
                    grant.spend_before.settled_microusd, grant.spend_before.reserved_microusd));
            }
            text.push('\n');
        }
        if let Some(output) = &self.output {
            text.push_str(&format!(
                "## Selected artifact\n\nAttempt {} · {} · {}\n\nArtifact: `{}`\n\n",
                output.attempt_number,
                label(output.role),
                if output.locally_accepted {
                    "locally accepted"
                } else {
                    "unaccepted"
                },
                output.artifact_hash
            ));
            for (name, hash) in &output.files {
                text.push_str(&format!("- {}: `{}`\n", line(name), hash));
            }
            text.push('\n');
        }
        text.push_str("## Criterion results\n\n");
        for criterion in &self.criteria {
            text.push_str(&format!(
                "- {}: {}{}\n",
                line(&criterion.criterion_id),
                outcome(criterion.outcome),
                criterion
                    .score
                    .map(|score| format!(" · score {score:.4}"))
                    .unwrap_or_default()
            ));
        }
        text.push_str("\n## Open items\n\n");
        if self.open_items.is_empty() {
            text.push_str("No unresolved local acceptance or handoff items.\n");
        }
        for item in &self.open_items {
            text.push_str(&format!("- {}\n", line(item)));
        }
        if !self.findings.is_empty() {
            text.push_str("\n## Independent findings for the selected artifact\n\n");
            for finding in &self.findings {
                text.push_str(&format!(
                    "- {} / {} / {}: {}\n",
                    line(&finding.critic_id),
                    line(&finding.criterion_id),
                    line(&finding.location),
                    line(&finding.detail)
                ));
            }
        }
        text.push_str(&format!("\nHandoff: {}\n\nContract: `{}`\n\nEvaluation contract: `{}`\n\nExternal delivery is not confirmed by this draft/capture report.\n",
            self.handoff.as_ref().map(|handoff| label(handoff.kind)).unwrap_or_else(|| "none".into()), self.contract_hash, self.evaluation_contract_hash));
        text
    }
}

fn label(value: impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".into())
        .replace('_', " ")
}
fn outcome(value: Outcome) -> &'static str {
    match value {
        Outcome::Pass => "pass",
        Outcome::Fail => "fail",
        Outcome::Unknown => "unknown",
    }
}
fn line(value: &str) -> String {
    value
        .chars()
        .flat_map(|ch| match ch {
            ch if ch.is_control() => vec![' '],
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '#' | '|' => vec!['\\', ch],
            ch => vec![ch],
        })
        .collect()
}
