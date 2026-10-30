//! Scope-owned journal with the verification store's lock + renewable lease +
//! generation fencing pattern. Each transition is one durable committed JSON
//! record. Replay reconstructs decisions from raw verdicts; caches are not
//! authority. Async callers must run these blocking methods on spawn_blocking.
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    path::{Path, PathBuf},
};

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Duration, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    budget::{Allowance, BudgetLedger, OperationClass, OperationIntent, Settlement},
    budget_round::{ApprovedBudgetReset, BudgetReset, ExhaustedLimits},
    contract::{valid_hash, FrozenContract, ModelPin, SourcePin},
    decision::{self, Decision, DecisionContext, DecisionKind, Retained},
    review::{self, ReviewContext, Verdict},
    routing::{
        self, Handoff, HandoffKind, ReviewRoute, RoutingDecision, RoutingPolicy, RoutingVerdict,
    },
};
use magicvault_primitives::durable_io::{sync_parent_dir_blocking, write_bytes_durably_sync};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunScope {
    pub principal: String,
    pub workspace: String,
}

pub const LEASE_SECS: i64 = 120;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Admission {
    pub owner_principal: String,
    pub source_id: String,
    pub source_hash: String,
    pub task_id: String,
    #[serde(default)]
    pub routing_policy: Option<RoutingPolicy>,
    #[serde(default)]
    pub handoff_kind: HandoffKind,
    /// Optional absolute parent ceiling. This can only shorten the contract's
    /// wall-clock budget and survives controller/adapter restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Admitted,
    Attempting,
    Reviewing,
    WaitingOwner,
    Accepted,
    Plateau,
    BudgetExhausted,
    Cancelled,
    Failed,
}

impl RunStatus {
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Accepted | Self::Plateau | Self::BudgetExhausted | Self::Cancelled | Self::Failed
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub holder: String,
    pub generation: u64,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRef {
    pub hash: String,
    pub files: BTreeMap<String, String>,
    pub evidence_ids: std::collections::BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub collected_sources: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationReceipt {
    pub input_hash: String,
    pub actual_cost_microusd: Option<u64>,
    pub receipt_hash: String,
}

/// A nonterminal adapter pause, distinct from a sealed final result. The
/// controller pins both the opaque checkpoint bytes and the paid-call ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActorCheckpoint {
    pub content_hash: String,
    pub budget_hash: String,
    pub source_epoch: u64,
    pub attention_id: String,
}

impl ActorCheckpoint {
    fn validate(&self) -> Result<()> {
        ensure!(
            valid_hash(&self.content_hash)
                && valid_hash(&self.budget_hash)
                && !self.attention_id.trim().is_empty()
                && self.attention_id.len() <= 1024
                && !self
                    .attention_id
                    .bytes()
                    .any(|byte| byte.is_ascii_control()),
            "invalid actor checkpoint binding"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Attempt {
    pub number: u32,
    pub actor_operation_id: String,
    pub review_allowance_id: String,
    pub artifact: Option<ArtifactRef>,
    pub verdicts: BTreeMap<String, Verdict>,
    pub verdict_operation_ids: BTreeMap<String, String>,
    pub critic_operations: BTreeMap<String, Vec<String>>,
    pub review: Option<review::ReviewedArtifact>,
    pub decided: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<ActorCheckpoint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BudgetPause {
    pub resume_status: RunStatus,
    pub stopped_at: DateTime<Utc>,
    pub limits: ExhaustedLimits,
    pub required_microusd: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunState {
    pub id: Uuid,
    pub scope: RunScope,
    pub contract: FrozenContract,
    pub admission: Admission,
    pub started_at: DateTime<Utc>,
    #[serde(skip_serializing)]
    pub last_committed_at: DateTime<Utc>,
    #[serde(skip_serializing)]
    pub finished_at: Option<DateTime<Utc>>,
    pub deadline_at: DateTime<Utc>,
    pub generation: u64,
    pub lease: Option<Lease>,
    pub source_epoch: u64,
    /// Authenticated input evidence added through the owner boundary. Empty
    /// legacy runs keep their original serialized shape and evaluation hash.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub owner_inputs: BTreeMap<String, String>,
    pub status: RunStatus,
    pub budget: BudgetLedger,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub budget_resets: Vec<ApprovedBudgetReset>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_pause: Option<BudgetPause>,
    pub attempts: Vec<Attempt>,
    pub retained: Retained,
    /// Useful bytes from an older source epoch, never an acceptance verdict.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference_artifact: Option<review::ReviewedArtifact>,
    pub decisions: Vec<Decision>,
    pub reason: Option<String>,
    pub attention_id: Option<String>,
    pub routing: Option<RoutingDecision>,
    pub owner_approved: Option<bool>,
    pub handoff: Option<Handoff>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunEvent {
    Admitted {
        contract: FrozenContract,
        admission: Admission,
    },
    LeaseClaimed {
        holder: String,
    },
    LeaseRenewed,
    LeaseReleased,
    AttemptStarted {
        actor: OperationIntent,
        review: Allowance,
    },
    ActorRestarted {
        previous_operation_id: String,
        replacement: OperationIntent,
    },
    ArtifactSealed {
        artifact: ArtifactRef,
    },
    ActorCompleted {
        artifact: ArtifactRef,
        model: ModelPin,
        receipt: OperationReceipt,
    },
    ActorCheckpointed {
        checkpoint: ActorCheckpoint,
        reason: String,
    },
    ActorFailed {
        reason: String,
        receipt: OperationReceipt,
    },
    ReviewStarted {
        critic_id: String,
        operation: OperationIntent,
    },
    ReviewRestarted {
        critic_id: String,
        previous_operation_id: String,
        replacement: OperationIntent,
    },
    VerdictRecorded {
        critic_id: String,
        operation_id: String,
        verdict: Verdict,
    },
    CriticCompleted {
        critic_id: String,
        operation_id: String,
        verdict: Verdict,
        receipt: OperationReceipt,
    },
    OperationReserved {
        operation: OperationIntent,
    },
    OperationSettled {
        operation_id: String,
        actual_cost_microusd: u64,
        receipt_hash: String,
    },
    OperationUnknown {
        operation_id: String,
    },
    OperationNotPerformed {
        operation_id: String,
        evidence_hash: String,
    },
    ReviewHeadroomReleased {
        allowance_id: String,
    },
    AttemptDecided,
    Blocked {
        reason: String,
        attention_id: String,
    },
    OwnerResumed {
        attention_id: String,
    },
    BudgetResetApproved {
        id: Uuid,
        approved_by: String,
        contract_hash: String,
        reset: BudgetReset,
    },
    OwnerInputRecorded {
        attention_id: String,
        response_hash: String,
    },
    ReviewRouted,
    OwnerReviewed {
        artifact_hash: String,
        approved: bool,
    },
    HandoffRecorded,
    Cancelled,
    DeadlineExpired,
    BudgetUnavailable {
        required_microusd: u64,
    },
    Failed {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    run_id: Uuid,
    scope: RunScope,
    sequence: u64,
    generation: u64,
    committed_at: DateTime<Utc>,
    previous_hash: String,
    event: RunEvent,
    hash: String,
}

impl Record {
    fn digest(&self) -> Result<String> {
        Ok(blake3::hash(&serde_json::to_vec(&(
            self.version,
            self.run_id,
            &self.scope,
            self.sequence,
            self.generation,
            self.committed_at,
            &self.previous_hash,
            &self.event,
        ))?)
        .to_hex()
        .to_string())
    }
}

impl RunState {
    /// Original contract and total ledger never change. Only the independently
    /// approved allowance windows move their measured admission ceilings.
    pub fn effective_budget(&self) -> Result<super::contract::Budget> {
        let mut budget = self.contract.definition().budget.clone();
        if let Some(grant) = self
            .budget_resets
            .iter()
            .rev()
            .find(|g| g.reset.cost_microusd.is_some())
        {
            let allowance = grant.reset.cost_microusd.expect("filtered");
            budget.max_cost_microusd = if allowance == 0 {
                0
            } else {
                grant
                    .prior_cost(&self.budget)?
                    .checked_add(allowance)
                    .context("approved cost ceiling overflow")?
            };
        }
        if let Some(grant) = self
            .budget_resets
            .iter()
            .rev()
            .find(|g| g.reset.attempts.is_some())
        {
            let allowance = grant.reset.attempts.expect("filtered");
            budget.max_attempts = if allowance == 0 {
                0
            } else {
                grant
                    .attempts_before
                    .checked_add(allowance)
                    .context("approved attempt ceiling overflow")?
            };
        }
        if let Some(grant) = self
            .budget_resets
            .iter()
            .rev()
            .find(|g| g.reset.wall_clock_secs.is_some())
        {
            budget.max_wall_clock_secs = grant.reset.wall_clock_secs.expect("filtered");
        }
        Ok(budget)
    }

    pub fn exhausted_limits(&self, now: DateTime<Utc>) -> Result<ExhaustedLimits> {
        if let Some(pause) = &self.budget_pause {
            return Ok(ExhaustedLimits {
                time: now >= self.deadline_at,
                ..pause.limits
            });
        }
        let budget = self.effective_budget()?;
        Ok(ExhaustedLimits {
            attempts: self.attempts.len() as u64 >= u64::from(budget.attempt_ceiling()),
            time: now >= self.deadline_at,
            cost: self.budget.spend()?.total()? >= budget.cost_ceiling(),
        })
    }

    /// Independent current windows, alongside lifetime totals. Clock usage is
    /// elapsed wall time (including owner waits), not billed provider time.
    pub fn budget_usage(&self, now: DateTime<Utc>) -> Result<serde_json::Value> {
        let now = self.finished_at.unwrap_or(now);
        let original = &self.contract.definition().budget;
        let cost = self
            .budget_resets
            .iter()
            .rev()
            .find(|g| g.reset.cost_microusd.is_some());
        let cost_max = cost
            .and_then(|g| g.reset.cost_microusd)
            .unwrap_or(original.max_cost_microusd);
        let used = self
            .budget
            .spend()?
            .total()?
            .checked_sub(
                cost.map(|g| g.prior_cost(&self.budget))
                    .transpose()?
                    .unwrap_or(0),
            )
            .context("cost window baseline exceeds ledger")?;
        let attempts = self
            .budget_resets
            .iter()
            .rev()
            .find(|g| g.reset.attempts.is_some());
        let attempts_used = self.attempts.len() as u64
            - u64::from(attempts.map(|g| g.attempts_before).unwrap_or(0));
        let attempts_max = attempts
            .and_then(|g| g.reset.attempts)
            .unwrap_or(original.max_attempts);
        let start = self
            .budget_resets
            .iter()
            .rev()
            .find(|g| g.reset.wall_clock_secs.is_some())
            .map(|g| g.approved_at)
            .unwrap_or(self.started_at);
        let mut previous_start = self.started_at;
        let mut consumed_seconds = 0u64;
        for grant in self
            .budget_resets
            .iter()
            .filter(|g| g.reset.wall_clock_secs.is_some())
        {
            consumed_seconds = consumed_seconds
                .checked_add(u64::try_from(
                    (grant.deadline_before - previous_start)
                        .num_seconds()
                        .max(0),
                )?)
                .context("cumulative elapsed allowance overflow")?;
            previous_start = grant.approved_at;
        }
        consumed_seconds = consumed_seconds
            .checked_add(u64::try_from(
                (now.min(self.deadline_at) - start).num_seconds().max(0),
            )?)
            .context("cumulative elapsed allowance overflow")?;
        Ok(serde_json::json!({
            "cost":{"allowance_microusd":cost_max,"committed_microusd":used,"unlimited":cost_max==0,"remaining_microusd":(cost_max>0).then(||cost_max.saturating_sub(used))},
            "attempts":{"allowance":attempts_max,"used":attempts_used,"unlimited":attempts_max==0,"remaining":(attempts_max>0).then(||u64::from(attempts_max).saturating_sub(attempts_used))},
            "time":{"started_at":start,"deadline_at":self.deadline_at,"unlimited":self.deadline_at==DateTime::<Utc>::MAX_UTC,"remaining_secs":(self.deadline_at!=DateTime::<Utc>::MAX_UTC).then(||(self.deadline_at-now).num_seconds().max(0))},
            "cumulative":{"attempts":self.attempts.len(),"spend":self.budget.spend()?,"elapsed_wall_clock_secs":(now-self.started_at).num_seconds().max(0),"time_allowance_used_secs":consumed_seconds},
        }))
    }

    /// The original acceptance definition stays frozen. Added owner evidence
    /// produces a separately hashed evaluation view for this source epoch.
    pub fn evaluation_contract(&self) -> Result<FrozenContract> {
        if self.owner_inputs.is_empty() {
            return Ok(self.contract.clone());
        }
        let mut definition = self.contract.definition().clone();
        for (attention, response_hash) in &self.owner_inputs {
            definition.sources.push(SourcePin {
                id: format!(
                    "owner-input:{}",
                    blake3::hash(attention.as_bytes()).to_hex()
                ),
                content_hash: response_hash.clone(),
            });
        }
        definition.try_into()
    }

    pub fn reference_for_actor(&self) -> Option<&review::ReviewedArtifact> {
        self.retained
            .artifact()
            .or(self.reference_artifact.as_ref())
    }

    fn fenced(&self, generation: u64, now: DateTime<Utc>) -> Result<()> {
        ensure!(
            self.generation == generation
                && self
                    .lease
                    .as_ref()
                    .is_some_and(|l| l.generation == generation && l.expires_at > now),
            "controller lease expired or generation superseded"
        );
        Ok(())
    }

    fn productive(&self, now: DateTime<Utc>) -> Result<()> {
        ensure!(
            !self.status.terminal() && now < self.deadline_at,
            "run terminal or deadline reached"
        );
        Ok(())
    }

    fn review_context(&self, store: &RunStore, artifact: &ArtifactRef) -> Result<ReviewContext> {
        let evaluation = self.evaluation_contract()?;
        let artifact_texts = artifact
            .files
            .values()
            .map(|hash| store.read_blob(self.id, hash))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter_map(|bytes| String::from_utf8(bytes).ok())
            .collect();
        let mut sources: BTreeMap<String, Vec<u8>> = evaluation
            .definition()
            .sources
            .iter()
            .map(|pin| Ok((pin.id.clone(), store.read_blob(self.id, &pin.content_hash)?)))
            .collect::<Result<_>>()?;
        for (id, hash) in &artifact.collected_sources {
            ensure!(
                !sources.contains_key(id),
                "collected source replaces pinned evidence"
            );
            sources.insert(id.clone(), store.read_blob(self.id, hash)?);
        }
        Ok(ReviewContext {
            artifact_hash: artifact.hash.clone(),
            source_epoch: self.source_epoch,
            evidence_ids: artifact
                .evidence_ids
                .iter()
                .cloned()
                .chain(evaluation.definition().sources.iter().map(|s| s.id.clone()))
                .collect(),
            artifact_texts,
            sources,
        })
    }

    fn current_attempt(&mut self) -> Result<&mut Attempt> {
        self.attempts
            .last_mut()
            .ok_or_else(|| anyhow::anyhow!("no admitted attempt"))
    }

    fn completion(&mut self, id: &str, receipt: &OperationReceipt) -> Result<()> {
        let operation = self
            .budget
            .operations()
            .get(id)
            .context("receipt without intent")?;
        ensure!(
            operation.intent.input_hash == receipt.input_hash && valid_hash(&receipt.receipt_hash),
            "receipt does not bind recorded operation"
        );
        if let Some(actual) = receipt.actual_cost_microusd {
            self.budget
                .settle(id, actual, receipt.receipt_hash.clone())?;
        } else {
            self.budget.mark_unknown(id)?;
        }
        Ok(())
    }

    fn apply(&mut self, record: &Record, store: &RunStore) -> Result<()> {
        let now = record.committed_at;
        let budget_ceiling = self.effective_budget()?;
        match &record.event {
            RunEvent::Admitted { .. } => anyhow::bail!("run cannot be admitted twice"),
            RunEvent::BudgetResetApproved {
                id,
                approved_by,
                contract_hash,
                reset,
            } => {
                ensure!(
                    self.status == RunStatus::BudgetExhausted
                        && record.generation
                            == self
                                .generation
                                .checked_add(1)
                                .context("generation overflow")?
                        && self
                            .lease
                            .as_ref()
                            .is_none_or(|lease| lease.expires_at <= now),
                    "budget reset needs a stopped, quiescent run and its exact generation"
                );
                ensure!(
                    approved_by == &self.admission.owner_principal
                        && *contract_hash == self.contract.hash()?,
                    "budget reset requires its exact owner and frozen contract"
                );
                let exhausted = self.exhausted_limits(now)?;
                reset.validate(exhausted)?;
                ensure!(
                    (!exhausted.cost || reset.cost_microusd.is_some())
                        && (!exhausted.time || reset.wall_clock_secs.is_some())
                        && (!exhausted.attempts || reset.attempts.is_some()),
                    "review all exhausted allowances before continuing"
                );
                let pause = self
                    .budget_pause
                    .as_ref()
                    .context("budget stop context missing")?;
                if let Some(cost) = reset.cost_microusd {
                    ensure!(
                        cost == 0 || cost >= pause.required_microusd,
                        "new cost allowance cannot fund the bounded next attempt"
                    );
                }
                let resume_status = pause.resume_status;
                let grant = ApprovedBudgetReset {
                    id: *id,
                    approved_by: approved_by.clone(),
                    approved_at: now,
                    reset: reset.clone(),
                    attempts_before: u32::try_from(self.attempts.len())?,
                    spend_before: self.budget.spend()?,
                    deadline_before: self.deadline_at,
                    prior_operation_ids: self.budget.operations().keys().cloned().collect(),
                    prior_allowance_ids: self.budget.allowances().keys().cloned().collect(),
                };
                if let Some(seconds) = reset.wall_clock_secs {
                    let deadline = super::contract::Budget {
                        max_wall_clock_secs: seconds,
                        ..Default::default()
                    }
                    .deadline_at(now)?;
                    let deadline = if seconds == 0 {
                        self.admission
                            .not_after
                            .map_or(deadline, |ceiling| ceiling.min(deadline))
                    } else {
                        deadline
                    };
                    ensure!(
                        self.admission
                            .not_after
                            .is_none_or(|ceiling| deadline <= ceiling),
                        "budget reset cannot extend the parent policy deadline"
                    );
                    self.deadline_at = deadline;
                }
                self.budget_resets.push(grant);
                self.effective_budget()?;
                self.generation = record.generation;
                self.lease = None;
                self.budget_pause = None;
                self.status = resume_status;
                self.reason = None;
            },
            RunEvent::LeaseClaimed { holder } => {
                ensure!(
                    !holder.trim().is_empty()
                        && record.generation
                            == self
                                .generation
                                .checked_add(1)
                                .context("generation overflow")?,
                    "invalid lease claim"
                );
                ensure!(
                    self.lease.as_ref().is_none_or(|l| l.expires_at <= now),
                    "active controller already holds lease"
                );
                self.generation = record.generation;
                self.lease = Some(Lease {
                    holder: holder.clone(),
                    generation: self.generation,
                    expires_at: now + Duration::seconds(LEASE_SECS),
                });
            },
            RunEvent::Cancelled => {
                ensure!(
                    !self.status.terminal()
                        || self.status == RunStatus::BudgetExhausted
                        || (self.status == RunStatus::Accepted
                            && self.handoff.is_none()
                            && self.owner_approved != Some(false)),
                    "cannot change a completed terminal handoff"
                );
                ensure!(
                    record.generation
                        == self
                            .generation
                            .checked_add(1)
                            .context("generation overflow")?,
                    "cancellation must fence active controller"
                );
                self.generation = record.generation;
                self.lease = None;
                self.status = RunStatus::Cancelled;
                self.reason =
                    Some("owner cancelled run; unresolved operation reservations retained".into());
            },
            RunEvent::OwnerResumed { attention_id } => {
                ensure!(
                    record.generation == self.generation
                        && self.status == RunStatus::WaitingOwner
                        && self.attention_id.as_ref() == Some(attention_id),
                    "owner response does not match waiting run"
                );
                self.productive(now)?;
                self.status = match self.attempts.last().filter(|a| !a.decided) {
                    Some(a) if a.artifact.is_some() => RunStatus::Reviewing,
                    Some(_) => RunStatus::Attempting,
                    None => RunStatus::Admitted,
                };
                self.attention_id = None;
            },
            RunEvent::OwnerInputRecorded {
                attention_id,
                response_hash,
            } => {
                ensure!(
                    record.generation == self.generation
                        && self.status == RunStatus::WaitingOwner
                        && self.attention_id.as_ref() == Some(attention_id)
                        && !attention_id.trim().is_empty()
                        && attention_id.len() <= 1024
                        && !attention_id.bytes().any(|byte| byte.is_ascii_control())
                        && valid_hash(response_hash)
                        && !self.owner_inputs.contains_key(attention_id),
                    "owner input does not match the pending exact request"
                );
                self.productive(now)?;
                if let Some(attempt) = self.attempts.last().filter(|attempt| !attempt.decided) {
                    ensure!(
                        attempt.artifact.is_none()
                            && self
                                .budget
                                .operations()
                                .get(&attempt.actor_operation_id)
                                .is_some_and(
                                    |operation| operation.settlement == Settlement::Reserved
                                ),
                        "owner input requires an unfinished actor with its original reservation"
                    );
                }
                let bytes = store.read_blob(self.id, response_hash)?;
                std::str::from_utf8(&bytes).context("owner evidence must be UTF-8")?;
                self.source_epoch = self
                    .source_epoch
                    .checked_add(1)
                    .context("source epoch overflow")?;
                self.owner_inputs
                    .insert(attention_id.clone(), response_hash.clone());
                self.evaluation_contract()?;
                if let Some(prior) = self.retained.artifact() {
                    self.reference_artifact = Some(prior.clone());
                }
                self.retained = Retained::default();
                self.routing = None;
                self.owner_approved = None;
                self.attention_id = None;
                self.reason = None;
                self.status = match self.attempts.last().filter(|attempt| !attempt.decided) {
                    Some(_) => RunStatus::Attempting,
                    None => RunStatus::Admitted,
                };
            },
            RunEvent::OwnerReviewed {
                artifact_hash,
                approved,
            } => {
                ensure!(
                    record.generation == self.generation
                        && self.status == RunStatus::Accepted
                        && self
                            .routing
                            .as_ref()
                            .is_some_and(|r| r.route == ReviewRoute::HumanReview
                                && &r.artifact_hash == artifact_hash)
                        && self.owner_approved.is_none()
                        && self.handoff.is_none(),
                    "owner review must match the pending exact artifact"
                );
                self.owner_approved = Some(*approved);
            },
            event => {
                self.fenced(record.generation, now)?;
                match event {
                    RunEvent::ReviewRouted => {
                        ensure!(
                            self.status == RunStatus::Accepted && self.routing.is_none(),
                            "only an unrouted accepted draft can be routed"
                        );
                        let accepted =
                            self.decisions.last().context("accepted decision missing")?;
                        let artifact_hash = accepted
                            .accepted_hash
                            .as_ref()
                            .context("accepted hash missing")?;
                        let evaluation = self.evaluation_contract()?;
                        let evaluation_hash = evaluation.hash()?;
                        let attempt = self.attempts.last().context("accepted attempt missing")?;
                        let artifact = attempt
                            .artifact
                            .as_ref()
                            .context("accepted artifact missing")?;
                        ensure!(
                            &artifact.hash == artifact_hash
                                && attempt.decided
                                && attempt.review.as_ref().is_some_and(|review| {
                                    review.complete
                                        && review.feasible
                                        && review.artifact_hash == *artifact_hash
                                        && review.contract_hash == evaluation_hash
                                        && review.source_epoch == self.source_epoch
                                }),
                            "routing requires the exact accepted attempt's current review"
                        );
                        let context = self.review_context(store, artifact)?;
                        self.routing = Some(if let Some(policy) = &self.admission.routing_policy {
                            let recommendation = self
                                .contract
                                .definition()
                                .critics
                                .iter()
                                .find(|pin| pin.model.as_ref() == Some(&policy.reviewer))
                                .and_then(|pin| {
                                    attempt.verdicts.get(&pin.id).filter(|verdict| {
                                        review::validate_verdict(
                                            pin,
                                            verdict,
                                            &evaluation,
                                            &evaluation_hash,
                                            &context,
                                        )
                                        .is_ok()
                                    })
                                })
                                .and_then(|v| v.review_recommendation.as_ref());
                            let verdict = recommendation.map(|r| RoutingVerdict {
                                artifact_hash: artifact_hash.clone(),
                                contract_hash: evaluation_hash.clone(),
                                policy_hash: policy.version_hash.clone(),
                                model: policy.reviewer.clone(),
                                route: r.route,
                                confidence: r.confidence,
                            });
                            routing::resolve(&evaluation, accepted, policy, verdict.as_ref())?
                        } else {
                            RoutingDecision { artifact_hash: artifact_hash.clone(), policy_hash: self.admission.source_hash.clone(), route: ReviewRoute::HumanReview, reason: "owner review required; independent routing policy was not admitted".into() }
                        });
                    },
                    RunEvent::HandoffRecorded => {
                        let route = self.routing.as_ref().context("review route missing")?;
                        ensure!(
                            self.status == RunStatus::Accepted && self.handoff.is_none(),
                            "handoff requires an accepted, undelivered draft"
                        );
                        let allowed = match route.route {
                            ReviewRoute::HumanReview => self.owner_approved == Some(true),
                            ReviewRoute::AutomaticInternalHandoff => {
                                self.admission.handoff_kind == HandoffKind::InternalDraft
                            },
                            ReviewRoute::AutomaticCapture => {
                                self.admission.handoff_kind == HandoffKind::Capture
                            },
                        };
                        ensure!(
                            allowed,
                            "human review or matching capture authority required"
                        );
                        self.handoff = Some(Handoff {
                            artifact_hash: route.artifact_hash.clone(),
                            kind: self.admission.handoff_kind,
                        });
                    },
                    RunEvent::LeaseRenewed => {
                        self.lease.as_mut().expect("fenced").expires_at =
                            now + Duration::seconds(LEASE_SECS);
                    },
                    RunEvent::LeaseReleased => self.lease = None,
                    RunEvent::ActorRestarted {
                        previous_operation_id,
                        replacement,
                    } => {
                        self.productive(now)?;
                        let attempt = self
                            .attempts
                            .last()
                            .context("no actor attempt to recover")?;
                        ensure!(
                            self.status == RunStatus::Attempting
                                && !attempt.decided
                                && attempt.artifact.is_none()
                                && &attempt.actor_operation_id == previous_operation_id,
                            "actor recovery phase mismatch"
                        );
                        let prior = self
                            .budget
                            .operations()
                            .get(previous_operation_id)
                            .context("prior actor reservation missing")?;
                        ensure!(matches!(prior.settlement, Settlement::NotPerformed { .. }) && replacement.id != *previous_operation_id && replacement.class == OperationClass::Actor && replacement.input_hash == prior.intent.input_hash && replacement.maximum_cost_microusd == prior.intent.maximum_cost_microusd, "actor recovery requires proof of no dispatch and unchanged frozen input/quote");
                        self.budget.reserve(
                            replacement.clone(),
                            self.effective_budget()?.cost_ceiling(),
                        )?;
                        self.current_attempt()?.actor_operation_id = replacement.id.clone();
                    },
                    RunEvent::AttemptStarted { actor, review } => {
                        self.productive(now)?;
                        ensure!(
                            self.status == RunStatus::Admitted,
                            "prior attempt not settled"
                        );
                        let number = u32::try_from(self.attempts.len())?
                            .checked_add(1)
                            .context("attempt count overflow")?;
                        ensure!(
                            number <= budget_ceiling.attempt_ceiling()
                                && actor.class == OperationClass::Actor,
                            "attempt ceiling or wrong adapter class"
                        );
                        self.budget.reserve_attempt(
                            actor.clone(),
                            review.clone(),
                            budget_ceiling.cost_ceiling(),
                        )?;
                        self.attempts.push(Attempt {
                            number,
                            actor_operation_id: actor.id.clone(),
                            review_allowance_id: review.id.clone(),
                            artifact: None,
                            verdicts: BTreeMap::new(),
                            verdict_operation_ids: BTreeMap::new(),
                            critic_operations: BTreeMap::new(),
                            review: None,
                            decided: false,
                            checkpoint: None,
                            failure: None,
                        });
                        self.status = RunStatus::Attempting;
                    },
                    RunEvent::ActorCheckpointed { checkpoint, reason } => {
                        self.productive(now)?;
                        checkpoint.validate()?;
                        ensure!(
                            self.status == RunStatus::Attempting
                                && checkpoint.source_epoch == self.source_epoch
                                && !self.owner_inputs.contains_key(&checkpoint.attention_id)
                                && !reason.trim().is_empty(),
                            "actor checkpoint does not match the current context"
                        );
                        let attempt = self
                            .attempts
                            .last()
                            .context("actor checkpoint has no attempt")?;
                        ensure!(
                            !attempt.decided
                                && attempt.artifact.is_none()
                                && self
                                    .budget
                                    .operations()
                                    .get(&attempt.actor_operation_id)
                                    .is_some_and(
                                        |operation| operation.settlement == Settlement::Reserved
                                    ),
                            "actor checkpoint requires its original unfinished reservation"
                        );
                        store.read_blob(self.id, &checkpoint.content_hash)?;
                        self.current_attempt()?.checkpoint = Some(checkpoint.clone());
                        self.attention_id = Some(checkpoint.attention_id.clone());
                        self.reason = Some(reason.clone());
                        self.status = RunStatus::WaitingOwner;
                    },
                    RunEvent::ActorFailed { reason, receipt } => {
                        self.productive(now)?;
                        ensure!(
                            self.status == RunStatus::Attempting && !reason.trim().is_empty(),
                            "failed attempt requires an active actor and diagnostic"
                        );
                        let attempt = self
                            .attempts
                            .last()
                            .context("failed actor has no attempt")?;
                        ensure!(
                            !attempt.decided && attempt.artifact.is_none(),
                            "actor already completed"
                        );
                        let operation_id = attempt.actor_operation_id.clone();
                        let review_id = attempt.review_allowance_id.clone();
                        self.completion(&operation_id, receipt)?;
                        self.budget.close_review(&review_id)?;
                        self.current_attempt()?.failure = Some(reason.clone());
                        self.current_attempt()?.decided = true;
                        self.reason = Some(reason.clone());
                        if receipt.actual_cost_microusd.is_some() {
                            self.status = RunStatus::Admitted;
                            self.attention_id = None;
                        } else {
                            self.status = RunStatus::WaitingOwner;
                            self.attention_id = Some(format!("failed-{operation_id}"));
                        }
                    },
                    RunEvent::ArtifactSealed { artifact } => {
                        self.productive(now)?;
                        ensure!(
                            self.status == RunStatus::Attempting
                                && valid_hash(&artifact.hash)
                                && !artifact.files.is_empty()
                                && artifact.files.values().all(|h| valid_hash(h)),
                            "invalid candidate or state"
                        );
                        for name in artifact.files.keys() {
                            valid_relative_name(name)?;
                        }
                        if !artifact.collected_sources.is_empty() {
                            let limits = self
                                .contract
                                .definition()
                                .collected_source_limits
                                .as_ref()
                                .context("collected evidence was not admitted")?;
                            let total = artifact.collected_sources.iter().try_fold(0u64, |sum, (id, hash)| {
                                ensure!(id.starts_with("collected:") && !self.contract.definition().sources.iter().any(|s| &s.id == id), "collected source identity is invalid or replaces pinned evidence");
                                sum.checked_add(store.read_blob(self.id, hash)?.len() as u64).context("collected evidence size overflow")
                            })?;
                            ensure!(
                                artifact.collected_sources.len() as u64
                                    <= u64::from(limits.maximum_sources)
                                    && total <= limits.maximum_bytes,
                                "collected evidence exceeds frozen limits"
                            );
                        }
                        self.current_attempt()?.artifact = Some(artifact.clone());
                        self.status = RunStatus::Reviewing;
                    },
                    RunEvent::ActorCompleted {
                        artifact,
                        model,
                        receipt,
                    } => {
                        ensure!(
                            model == &self.contract.definition().actor,
                            "actor departed from frozen model configuration"
                        );
                        let operation_id = self
                            .attempts
                            .last()
                            .context("no actor attempt")?
                            .actor_operation_id
                            .clone();
                        self.apply(
                            &Record {
                                event: RunEvent::ArtifactSealed {
                                    artifact: artifact.clone(),
                                },
                                ..record.clone()
                            },
                            store,
                        )?;
                        self.completion(&operation_id, receipt)?;
                    },
                    RunEvent::ReviewStarted {
                        critic_id,
                        operation,
                    } => {
                        self.productive(now)?;
                        ensure!(self.status == RunStatus::Reviewing, "run not reviewing");
                        ensure!(
                            self.contract
                                .definition()
                                .critics
                                .iter()
                                .any(|c| &c.id == critic_id),
                            "critic not in frozen panel"
                        );
                        let maximum =
                            u64::from(self.contract.definition().review.max_retries_per_critic) + 1;
                        if let Some(prior_id) = self
                            .attempts
                            .last()
                            .and_then(|a| a.critic_operations.get(critic_id))
                            .and_then(|ids| ids.last())
                        {
                            ensure!(self.budget.operations().get(prior_id).is_some_and(|op| matches!(op.settlement, Settlement::Settled { .. } | Settlement::NotPerformed { .. })), "prior critic operation needs reconciliation before another paid call");
                        }
                        let artifact = self
                            .attempts
                            .last()
                            .and_then(|a| a.artifact.as_ref())
                            .context("no sealed candidate")?;
                        let context = self.review_context(store, artifact)?;
                        let pin = self
                            .contract
                            .definition()
                            .critics
                            .iter()
                            .find(|c| &c.id == critic_id)
                            .expect("validated critic");
                        if let Some(verdict) =
                            self.attempts.last().and_then(|a| a.verdicts.get(critic_id))
                        {
                            ensure!(
                                review::validate_verdict(
                                    pin,
                                    verdict,
                                    &self.evaluation_contract()?,
                                    &self.evaluation_contract()?.hash()?,
                                    &context
                                )
                                .is_err(),
                                "completed valid critic leg cannot be rejection-shopped"
                            );
                        }
                        let attempt = self.attempts.last().context("no review attempt")?;
                        let prior = attempt.critic_operations.get(critic_id).map_or(0, |ids| {
                            ids.iter()
                                .filter(|id| {
                                    !matches!(
                                        self.budget.operations()[*id].settlement,
                                        Settlement::NotPerformed { .. }
                                    )
                                })
                                .count()
                        });
                        ensure!(
                            (prior as u64) < maximum
                                && !attempt
                                    .critic_operations
                                    .values()
                                    .flatten()
                                    .any(|id| id == &operation.id),
                            "critic retry ceiling or duplicate operation"
                        );
                        let allowance = attempt.review_allowance_id.clone();
                        self.budget.allocate_review(&allowance, operation.clone())?;
                        self.current_attempt()?
                            .critic_operations
                            .entry(critic_id.clone())
                            .or_default()
                            .push(operation.id.clone());
                    },
                    RunEvent::ReviewRestarted {
                        critic_id,
                        previous_operation_id,
                        replacement,
                    } => {
                        self.productive(now)?;
                        let attempt = self.attempts.last().context("no review to recover")?;
                        ensure!(
                            self.status == RunStatus::Reviewing
                                && !attempt.decided
                                && attempt.artifact.is_some()
                                && attempt
                                    .critic_operations
                                    .get(critic_id)
                                    .and_then(|ids| ids.last())
                                    == Some(previous_operation_id)
                                && attempt.verdict_operation_ids.get(critic_id)
                                    != Some(previous_operation_id)
                                && !self.budget.operations().contains_key(&replacement.id),
                            "review recovery phase/identity mismatch"
                        );
                        let prior = self
                            .budget
                            .operations()
                            .get(previous_operation_id)
                            .context("prior critic reservation missing")?;
                        ensure!(
                            matches!(prior.settlement, Settlement::NotPerformed { .. })
                                && replacement.class == OperationClass::Critic
                                && replacement.input_hash == prior.intent.input_hash
                                && replacement.maximum_cost_microusd == prior.intent.maximum_cost_microusd
                                && self.budget.allowances().get(&attempt.review_allowance_id).is_some_and(|allowance| !allowance.closed),
                            "review recovery requires proof of no dispatch and unchanged frozen input/quote"
                        );
                        // The first ReviewStarted already allocated this quote
                        // from review headroom. Its proven unused reservation
                        // funds this replacement within the same run ceiling.
                        self.budget.reserve(
                            replacement.clone(),
                            self.effective_budget()?.cost_ceiling(),
                        )?;
                        self.current_attempt()?
                            .critic_operations
                            .get_mut(critic_id)
                            .expect("validated review leg")
                            .push(replacement.id.clone());
                    },
                    RunEvent::VerdictRecorded {
                        critic_id,
                        operation_id,
                        verdict,
                    } => {
                        self.productive(now)?;
                        ensure!(
                            self.status == RunStatus::Reviewing && &verdict.critic_id == critic_id,
                            "wrong review state/identity"
                        );
                        let attempt = self.current_attempt()?;
                        ensure!(
                            attempt
                                .critic_operations
                                .get(critic_id)
                                .and_then(|ids| ids.last())
                                == Some(operation_id),
                            "review has no current dispatched intent"
                        );
                        if attempt.verdict_operation_ids.get(critic_id) == Some(operation_id) {
                            ensure!(
                                attempt.verdicts.get(critic_id) == Some(verdict),
                                "conflicting duplicate critic result"
                            );
                        } else {
                            attempt.verdicts.insert(critic_id.clone(), verdict.clone());
                            attempt
                                .verdict_operation_ids
                                .insert(critic_id.clone(), operation_id.clone());
                        }
                    },
                    RunEvent::CriticCompleted {
                        critic_id,
                        operation_id,
                        verdict,
                        receipt,
                    } => {
                        self.apply(
                            &Record {
                                event: RunEvent::VerdictRecorded {
                                    critic_id: critic_id.clone(),
                                    operation_id: operation_id.clone(),
                                    verdict: verdict.clone(),
                                },
                                ..record.clone()
                            },
                            store,
                        )?;
                        self.completion(operation_id, receipt)?;
                    },
                    RunEvent::OperationReserved { operation } => {
                        self.productive(now)?;
                        ensure!(
                            self.status != RunStatus::WaitingOwner,
                            "waiting run cannot admit productive work"
                        );
                        self.budget
                            .reserve(operation.clone(), budget_ceiling.cost_ceiling())?;
                    },
                    RunEvent::OperationSettled {
                        operation_id,
                        actual_cost_microusd,
                        receipt_hash,
                    } => self.budget.settle(
                        operation_id,
                        *actual_cost_microusd,
                        receipt_hash.clone(),
                    )?,
                    RunEvent::OperationUnknown { operation_id } => {
                        self.budget.mark_unknown(operation_id)?
                    },
                    RunEvent::OperationNotPerformed {
                        operation_id,
                        evidence_hash,
                    } => self
                        .budget
                        .not_performed(operation_id, evidence_hash.clone())?,
                    RunEvent::ReviewHeadroomReleased { allowance_id } => {
                        ensure!(
                            self.status.terminal(),
                            "review headroom may still be needed"
                        );
                        self.budget.close_review(allowance_id)?;
                    },
                    RunEvent::AttemptDecided => {
                        self.productive(now)?;
                        ensure!(self.status == RunStatus::Reviewing, "run not reviewing");
                        let attempt = self.attempts.last().context("no attempt")?;
                        ensure!(!attempt.decided, "attempt already decided");
                        let artifact = attempt.artifact.as_ref().context("no sealed candidate")?;
                        let context = self.review_context(store, artifact)?;
                        let verdicts: Vec<_> = attempt.verdicts.values().cloned().collect();
                        let evaluation = self.evaluation_contract()?;
                        let candidate = review::aggregate(&evaluation, &context, &verdicts)?;
                        // A completed panel being retired for a new draft no
                        // longer needs its unused retry headroom. Unknown paid
                        // operations retain their individual reservations.
                        if candidate.complete
                            || (candidate.disagreement_repair_eligible
                                && self.contract.definition().review.unknown_review
                                    == super::contract::UnknownReviewPolicy::ReviseSupportedDefects)
                        {
                            self.budget.close_review(&attempt.review_allowance_id)?;
                        }
                        let decision = decision::decide_with_budget(
                            &evaluation,
                            &self.effective_budget()?,
                            &self.retained,
                            &candidate,
                            &DecisionContext {
                                attempts_started: u32::try_from(self.attempts.len())?,
                                spent_and_reserved_microusd: self.budget.spend()?.total()?,
                                source_epoch: self.source_epoch,
                                deadline_expired: false,
                                cancelled: false,
                            },
                        )?;
                        self.retained = decision.retained.clone();
                        self.status = match decision.kind {
                            DecisionKind::Accept => RunStatus::Accepted,
                            DecisionKind::KeepAndRetry
                            | DecisionKind::RevertAndRetry
                            | DecisionKind::RepairAndRetry => RunStatus::Admitted,
                            DecisionKind::Plateau => RunStatus::Plateau,
                            DecisionKind::BudgetExhausted => RunStatus::BudgetExhausted,
                            DecisionKind::WaitingOwner => RunStatus::WaitingOwner,
                            DecisionKind::Cancelled => RunStatus::Cancelled,
                        };
                        self.reason = Some(decision.reason.clone());
                        self.current_attempt()?.review = Some(candidate);
                        self.current_attempt()?.decided =
                            decision.kind != DecisionKind::WaitingOwner;
                        self.decisions.push(decision);
                        if self.status == RunStatus::BudgetExhausted {
                            self.budget_pause = Some(BudgetPause {
                                resume_status: RunStatus::Admitted,
                                stopped_at: now,
                                limits: self.exhausted_limits(now)?,
                                required_microusd: 0,
                            });
                        }
                    },
                    RunEvent::Blocked {
                        reason,
                        attention_id,
                    } => {
                        self.productive(now)?;
                        ensure!(
                            !reason.trim().is_empty() && !attention_id.trim().is_empty(),
                            "blocker needs durable attention identity"
                        );
                        self.status = RunStatus::WaitingOwner;
                        self.reason = Some(reason.clone());
                        self.attention_id = Some(attention_id.clone());
                    },
                    RunEvent::DeadlineExpired => {
                        ensure!(
                            !self.status.terminal() && now >= self.deadline_at,
                            "deadline not reached or run terminal"
                        );
                        self.budget_pause = Some(BudgetPause {
                            resume_status: self.status,
                            stopped_at: now,
                            limits: ExhaustedLimits {
                                time: true,
                                ..if self.status == RunStatus::Admitted {
                                    self.exhausted_limits(now)?
                                } else {
                                    ExhaustedLimits::default()
                                }
                            },
                            required_microusd: 0,
                        });
                        self.status = RunStatus::BudgetExhausted;
                        self.reason = Some(
                            "elapsed wall-clock deadline reached; unresolved reservations retained"
                                .into(),
                        );
                    },
                    RunEvent::BudgetUnavailable { required_microusd } => {
                        self.productive(now)?;
                        ensure!(
                            self.status == RunStatus::Admitted
                                && (self.attempts.len() as u64
                                    >= u64::from(budget_ceiling.attempt_ceiling())
                                    || self
                                        .budget
                                        .spend()?
                                        .total()?
                                        .checked_add(*required_microusd)
                                        .is_none_or(|cost| cost > budget_ceiling.cost_ceiling())),
                            "no attempt/cost ceiling prevents admission"
                        );
                        self.status = RunStatus::BudgetExhausted;
                        let budget = self.effective_budget()?;
                        self.budget_pause = Some(BudgetPause {
                            resume_status: RunStatus::Admitted,
                            stopped_at: now,
                            limits: ExhaustedLimits {
                                attempts: self.attempts.len() as u64
                                    >= u64::from(budget.attempt_ceiling()),
                                time: false,
                                cost: self.budget.spend()?.total()? >= budget.cost_ceiling()
                                    || self
                                        .budget
                                        .spend()?
                                        .total()?
                                        .checked_add(*required_microusd)
                                        .is_none_or(|cost| cost > budget.cost_ceiling()),
                            },
                            required_microusd: *required_microusd,
                        });
                        self.reason = Some("remaining budget cannot fund another bounded actor attempt and required review".into());
                    },
                    RunEvent::Failed { reason } => {
                        ensure!(!self.status.terminal(), "cannot replace terminal decision");
                        self.status = RunStatus::Failed;
                        self.reason = Some(reason.clone());
                    },
                    _ => unreachable!("handled above"),
                }
            },
        }
        ensure!(
            self.budget.spend()?.total()? <= self.effective_budget()?.cost_ceiling(),
            "cumulative ledger exceeds its approved cost window"
        );
        self.last_committed_at = now.max(self.last_committed_at);
        if self.finished_at.is_none()
            && self.status.terminal()
            && self.status != RunStatus::BudgetExhausted
        {
            self.finished_at = Some(now);
        }
        Ok(())
    }
}

fn valid_relative_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && Path::new(name)
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_))),
        "artifact name must be a relative path without traversal"
    );
    Ok(())
}

pub struct RunStore {
    root: PathBuf,
    scope: RunScope,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdapterJob {
    intent: OperationIntent,
    result_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    receipt: Option<OperationReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    budget: Option<AdapterBudget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checkpoint: Option<ActorCheckpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checkpoint_claim_generation: Option<u64>,
}

/// Child calls consume the already-reserved parent maximum. This is the
/// existing ledger within one composite adapter job, not another run budget.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterBudget {
    pub binding_hash: String,
    pub maximum_operations: u32,
    pub ledger: BudgetLedger,
    pub physical_starts: BTreeMap<String, i64>,
}

impl AdapterBudget {
    fn hash(&self) -> Result<String> {
        Ok(blake3::hash(&serde_json::to_vec(self)?)
            .to_hex()
            .to_string())
    }

    fn validate(&self, parent: &OperationIntent) -> Result<()> {
        ensure!(
            valid_hash(&self.binding_hash) && self.maximum_operations > 0,
            "invalid composite adapter budget binding"
        );
        ensure!(
            self.ledger.operations().len() <= self.maximum_operations as usize
                && self.ledger.spend()?.total()? <= parent.maximum_cost_microusd,
            "composite adapter exceeds parent bounds"
        );
        for (id, operation) in self.ledger.operations() {
            ensure!(
                operation.intent.class == parent.class,
                "composite child changed operation class"
            );
            let started = self.physical_starts.contains_key(id);
            match operation.settlement {
                Settlement::Settled { .. } | Settlement::OutcomeUnknown => {
                    ensure!(started, "physical receipt has no durable start")
                },
                Settlement::NotPerformed { .. } => {
                    ensure!(!started, "started physical operation cannot be refunded")
                },
                Settlement::Reserved => {},
            }
        }
        ensure!(
            self.physical_starts
                .iter()
                .all(|(id, at)| *at > 0 && self.ledger.operations().contains_key(id)),
            "physical start has no matching child intent"
        );
        Ok(())
    }

    /// Zero-cost uncertain calls are unresolved too: inspecting spend alone
    /// would incorrectly turn them into a known aggregate receipt.
    pub fn actual_cost_microusd(&self) -> Result<Option<u64>> {
        if self.ledger.operations().values().any(|operation| {
            matches!(
                operation.settlement,
                Settlement::Reserved | Settlement::OutcomeUnknown
            )
        }) {
            return Ok(None);
        }
        Ok(Some(self.ledger.spend()?.settled_microusd))
    }
}

pub enum AdapterJobLookup {
    NotStarted,
    InFlight,
    Completed(Vec<u8>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CollectedReadLedger {
    policy_hash: String,
    maximum_reads: u32,
    maximum_bytes_per_read: u64,
    reads: BTreeMap<String, CollectedRead>,
}

impl CollectedReadLedger {
    fn validate(&self) -> Result<()> {
        ensure!(
            valid_hash(&self.policy_hash)
                && self.maximum_reads > 0
                && self.maximum_bytes_per_read > 0
                && self.reads.len() as u64 <= u64::from(self.maximum_reads)
                && u64::from(self.maximum_reads)
                    .checked_mul(self.maximum_bytes_per_read)
                    .is_some()
                && self.reads.iter().all(|(request, read)| valid_hash(request)
                    && read
                        .result_hash
                        .as_ref()
                        .is_none_or(|hash| valid_hash(hash))
                    && read
                        .source_hash
                        .as_ref()
                        .is_none_or(|hash| valid_hash(hash))
                    && (read.source_hash.is_none() || read.result_hash.is_some())),
            "collected-read ledger is corrupt"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CollectedRead {
    result_hash: Option<String>,
    source_hash: Option<String>,
}

pub enum CollectedReadClaim {
    Reserved,
    Completed(Vec<u8>),
    Unknown,
}

impl RunStore {
    /// Runtime-only public-read budget. Full byte ceilings are retained for
    /// every request, including errors/interruption, across all run attempts.
    /// A request identity is never resent when its outcome is unresolved.
    pub fn claim_collected_read(
        &self,
        id: Uuid,
        holder: &str,
        policy_hash: &str,
        request_hash: &str,
        maximum_reads: u32,
        maximum_bytes_per_read: u64,
        now: DateTime<Utc>,
    ) -> Result<CollectedReadClaim> {
        ensure!(
            valid_hash(policy_hash)
                && valid_hash(request_hash)
                && maximum_reads > 0
                && maximum_bytes_per_read > 0,
            "invalid collected-read identity or bounds"
        );
        let _lock = self.lock(id)?;
        let state = self.fold(&self.records(id)?)?;
        state.productive(now)?;
        ensure!(
            state.status == RunStatus::Attempting
                && state
                    .attempts
                    .last()
                    .and_then(|attempt| state.budget.operations().get(&attempt.actor_operation_id))
                    .is_some_and(|operation| operation.settlement == Settlement::Reserved)
                && state
                    .lease
                    .as_ref()
                    .is_some_and(|lease| lease.holder == holder && lease.expires_at > now),
            "collected-read lease lost"
        );
        let limits = state
            .contract
            .definition()
            .collected_source_limits
            .as_ref()
            .context("collected reads were not admitted")?;
        ensure!(
            maximum_reads <= limits.maximum_sources
                && u64::from(maximum_reads)
                    .checked_mul(maximum_bytes_per_read)
                    .is_some_and(|bytes| bytes <= limits.maximum_bytes),
            "collected-read budget exceeds contract"
        );
        let path = self.dir(id).join("collected_reads.json");
        let mut ledger: CollectedReadLedger = if path.exists() {
            ensure!(
                !std::fs::symlink_metadata(&path)?.file_type().is_symlink(),
                "collected-read ledger is a symlink"
            );
            serde_json::from_slice(&std::fs::read(&path)?)?
        } else {
            CollectedReadLedger {
                policy_hash: policy_hash.into(),
                maximum_reads,
                maximum_bytes_per_read,
                reads: BTreeMap::new(),
            }
        };
        ensure!(
            ledger.policy_hash == policy_hash
                && ledger.maximum_reads == maximum_reads
                && ledger.maximum_bytes_per_read == maximum_bytes_per_read,
            "collected-read policy changed"
        );
        ledger.validate()?;
        if let Some(read) = ledger.reads.get(request_hash) {
            return match &read.result_hash {
                Some(hash) => {
                    let bytes = self.read_blob(id, hash)?;
                    ensure!(
                        bytes.len() as u64 <= maximum_bytes_per_read,
                        "cached collected read exceeds its ceiling"
                    );
                    Ok(CollectedReadClaim::Completed(bytes))
                },
                None => Ok(CollectedReadClaim::Unknown),
            };
        }
        ensure!(
            ledger.reads.len() < maximum_reads as usize,
            "collected-read request ceiling reached"
        );
        ledger.reads.insert(
            request_hash.into(),
            CollectedRead {
                result_hash: None,
                source_hash: None,
            },
        );
        write_bytes_durably_sync(&path, &serde_json::to_vec(&ledger)?)?;
        Ok(CollectedReadClaim::Reserved)
    }

    pub fn finish_collected_read(
        &self,
        id: Uuid,
        policy_hash: &str,
        request_hash: &str,
        result: &[u8],
        source: Option<&[u8]>,
    ) -> Result<()> {
        let _lock = self.lock(id)?;
        let path = self.dir(id).join("collected_reads.json");
        ensure!(
            !std::fs::symlink_metadata(&path)?.file_type().is_symlink(),
            "collected-read ledger is a symlink"
        );
        let mut ledger: CollectedReadLedger = serde_json::from_slice(&std::fs::read(&path)?)?;
        ledger.validate()?;
        ensure!(
            ledger.policy_hash == policy_hash
                && result.len() as u64 <= ledger.maximum_bytes_per_read
                && source.is_none_or(|bytes| bytes.len() as u64 <= ledger.maximum_bytes_per_read),
            "collected-read result exceeds policy"
        );
        let read = ledger
            .reads
            .get_mut(request_hash)
            .context("collected-read result has no reservation")?;
        let result_hash = self.put_blob_locked(id, result)?;
        let source_hash = source
            .map(|bytes| self.put_blob_locked(id, bytes))
            .transpose()?;
        ensure!(
            read.result_hash.is_none()
                || (read.result_hash.as_ref() == Some(&result_hash)
                    && read.source_hash == source_hash),
            "collected-read receipt changed"
        );
        read.result_hash = Some(result_hash);
        read.source_hash = source_hash;
        write_bytes_durably_sync(&path, &serde_json::to_vec(&ledger)?)?;
        Ok(())
    }

    pub fn collected_read_sources(
        &self,
        id: Uuid,
        policy_hash: &str,
    ) -> Result<BTreeMap<String, Vec<u8>>> {
        let _lock = self.lock(id)?;
        let path = self.dir(id).join("collected_reads.json");
        if !path.exists() {
            return Ok(BTreeMap::new());
        }
        ensure!(
            !std::fs::symlink_metadata(&path)?.file_type().is_symlink(),
            "collected-read ledger is a symlink"
        );
        let ledger: CollectedReadLedger = serde_json::from_slice(&std::fs::read(path)?)?;
        ledger.validate()?;
        ensure!(
            ledger.policy_hash == policy_hash
                && ledger.reads.len() <= ledger.maximum_reads as usize,
            "collected-read ledger policy changed"
        );
        let maximum_bytes = ledger.maximum_bytes_per_read;
        ledger
            .reads
            .into_iter()
            .filter_map(|(request, read)| read.source_hash.map(|hash| (request, hash)))
            .map(|(request, hash)| {
                let bytes = self.read_blob(id, &hash)?;
                ensure!(
                    bytes.len() as u64 <= maximum_bytes,
                    "collected source exceeds its sealed ceiling"
                );
                Ok((format!("collected:{request}"), bytes))
            })
            .collect()
    }

    pub fn new(scope_root: impl Into<PathBuf>, scope: RunScope) -> Self {
        Self {
            root: scope_root.into().join("relentless").join("runs"),
            scope,
        }
    }

    fn dir(&self, id: Uuid) -> PathBuf {
        self.root.join(id.to_string())
    }

    fn job_path(&self, id: Uuid, operation: &OperationIntent) -> PathBuf {
        let key = blake3::hash(operation.id.as_bytes()).to_hex().to_string();
        self.dir(id)
            .join("adapter_jobs")
            .join(format!("{key}.json"))
    }

    /// Final local dispatch fence. A lease-losing worker cannot start a job,
    /// and an operation ID may cross this boundary only once. This record is
    /// written before model I/O, under the same run lock as controller intent.
    pub fn begin_adapter_job(
        &self,
        id: Uuid,
        holder: &str,
        operation: &OperationIntent,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.begin_adapter_job_inner(id, holder, operation, None, now)
    }

    pub fn begin_budgeted_adapter_job(
        &self,
        id: Uuid,
        holder: &str,
        operation: &OperationIntent,
        binding_hash: String,
        maximum_operations: u32,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let budget = AdapterBudget {
            binding_hash,
            maximum_operations,
            ledger: BudgetLedger::default(),
            physical_starts: BTreeMap::new(),
        };
        budget.validate(operation)?;
        self.begin_adapter_job_inner(id, holder, operation, Some(budget), now)
    }

    fn begin_adapter_job_inner(
        &self,
        id: Uuid,
        holder: &str,
        operation: &OperationIntent,
        budget: Option<AdapterBudget>,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let _lock = self.lock(id)?;
        let state = self.fold(&self.records(id)?)?;
        let lease = state
            .lease
            .as_ref()
            .context("adapter dispatch has no lease")?;
        ensure!(
            lease.holder == holder
                && lease.expires_at > now
                && !state.status.terminal()
                && state.deadline_at > now,
            "adapter dispatch lease/deadline lost"
        );
        let reserved = state
            .budget
            .operations()
            .get(&operation.id)
            .context("adapter operation not reserved")?;
        ensure!(
            &reserved.intent == operation && reserved.settlement == Settlement::Reserved,
            "adapter operation reservation mismatch"
        );
        let path = self.job_path(id, operation);
        ensure!(
            !path.exists(),
            "adapter job already started; reconciliation required"
        );
        create_dirs_durably(path.parent().expect("adapter job parent"))?;
        write_bytes_durably_sync(
            &path,
            &serde_json::to_vec(&AdapterJob {
                intent: operation.clone(),
                result_hash: None,
                receipt: None,
                budget,
                checkpoint: None,
                checkpoint_claim_generation: None,
            })?,
        )?;
        Ok(())
    }

    /// Durable response before the journal consumes it. Late receipts remain
    /// recoverable after cancellation, without changing terminal run status.
    pub fn finish_adapter_job(
        &self,
        id: Uuid,
        operation: &OperationIntent,
        bytes: &[u8],
    ) -> Result<()> {
        self.finish_adapter_job_inner(id, operation, bytes, None)
    }

    /// Pin billing evidence with the result, independently of later candidate
    /// publication. A terminal receipt lookup never decodes or adopts a result.
    pub fn finish_receipted_adapter_job(
        &self,
        id: Uuid,
        operation: &OperationIntent,
        bytes: &[u8],
        receipt: &OperationReceipt,
    ) -> Result<()> {
        ensure!(
            receipt.input_hash == operation.input_hash && valid_hash(&receipt.receipt_hash),
            "adapter receipt identity mismatch"
        );
        self.finish_adapter_job_inner(id, operation, bytes, Some(receipt))
    }

    pub fn finish_budgeted_adapter_job(
        &self,
        id: Uuid,
        operation: &OperationIntent,
        bytes: &[u8],
        receipt: &OperationReceipt,
    ) -> Result<()> {
        ensure!(
            receipt.input_hash == operation.input_hash && valid_hash(&receipt.receipt_hash),
            "composite result receipt identity mismatch"
        );
        self.finish_adapter_job_inner(id, operation, bytes, Some(receipt))
    }

    fn finish_adapter_job_inner(
        &self,
        id: Uuid,
        operation: &OperationIntent,
        bytes: &[u8],
        receipt: Option<&OperationReceipt>,
    ) -> Result<()> {
        let hash = self.put_blob(id, bytes)?;
        let _lock = self.lock(id)?;
        let path = self.job_path(id, operation);
        let mut job: AdapterJob = serde_json::from_slice(&std::fs::read(&path)?)?;
        ensure!(job.intent == *operation, "adapter job identity mismatch");
        ensure!(
            job.checkpoint.is_none() || job.checkpoint_claim_generation.is_some(),
            "parked adapter cannot publish a final result before continuation"
        );
        match (&job.budget, receipt) {
            (Some(budget), Some(receipt)) => {
                budget.validate(operation)?;
                ensure!(
                    receipt.actual_cost_microusd == budget.actual_cost_microusd()?,
                    "composite result cost disagrees with physical receipts"
                );
            },
            (None, _) => {},
            _ => anyhow::bail!("composite adapter requires its aggregate receipt"),
        }
        if let Some(previous) = &job.result_hash {
            ensure!(previous == &hash, "adapter result cannot be replaced");
            if let Some(existing) = &job.receipt {
                ensure!(
                    receipt.is_none_or(|receipt| receipt == existing),
                    "adapter receipt cannot be replaced"
                );
                return Ok(());
            }
        }
        job.result_hash = Some(hash);
        job.receipt = receipt.cloned();
        write_bytes_durably_sync(&path, &serde_json::to_vec(&job)?)?;
        Ok(())
    }

    /// Read billing proof only. It cannot resume a task, publish a question or
    /// adopt a candidate. A fully settled composite ledger is usable even when
    /// cancellation prevented its result publication; outstanding/unknown
    /// children retain the whole original parent ceiling.
    pub fn terminal_adapter_receipt(
        &self,
        id: Uuid,
        operation: &OperationIntent,
    ) -> Result<Option<OperationReceipt>> {
        let _lock = self.lock(id)?;
        let state = self.fold(&self.records(id)?)?;
        ensure!(
            state.status.terminal()
                && state
                    .budget
                    .operations()
                    .get(&operation.id)
                    .is_some_and(|record| record.intent == *operation),
            "terminal receipt does not bind a terminal recorded operation"
        );
        let path = self.job_path(id, operation);
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        ensure!(
            !metadata.file_type().is_symlink(),
            "adapter receipt job cannot be a symlink"
        );
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let job: AdapterJob = serde_json::from_slice(&bytes)?;
        ensure!(
            job.intent == *operation,
            "terminal adapter job identity mismatch"
        );
        if let Some(receipt) = job.receipt {
            ensure!(
                job.result_hash.is_some()
                    && receipt.input_hash == operation.input_hash
                    && valid_hash(&receipt.receipt_hash),
                "terminal adapter receipt lost its sealed result/input binding"
            );
            self.read_blob(id, job.result_hash.as_ref().expect("checked result"))?;
            if let Some(budget) = job.budget {
                budget.validate(operation)?;
                ensure!(
                    receipt.actual_cost_microusd == budget.actual_cost_microusd()?,
                    "terminal composite receipt disagrees with its physical ledger"
                );
            }
            return Ok(Some(receipt));
        }
        let Some(budget) = job.budget else {
            return Ok(None);
        };
        budget.validate(operation)?;
        let Some(actual) = budget.actual_cost_microusd()? else {
            return Ok(None);
        };
        Ok(Some(OperationReceipt {input_hash:operation.input_hash.clone(),actual_cost_microusd:Some(actual),
            receipt_hash:blake3::hash(&serde_json::to_vec(&serde_json::json!({"parent":operation,"binding_hash":budget.binding_hash,"budget":budget}))?).to_hex().to_string()}))
    }

    /// Settle available local billing proofs after terminal cancellation or
    /// deadline. No provider/model adapter is constructed and no output state
    /// changes. Missing or unknown receipts remain reserved. Unallocated review
    /// headroom is released through its own journal event once work is terminal.
    pub fn reconcile_terminal_receipts(&self, id: Uuid, worker: String) -> Result<RunState> {
        self.reconcile_terminal_receipts_with_continuation(id, worker, true)
    }

    /// Qualification trials close permanently. Ordinary budget pauses keep
    /// the current paid attempt available for exact checkpoint recovery.
    pub fn reconcile_terminal_receipts_with_continuation(
        &self,
        id: Uuid,
        worker: String,
        preserve_attempt: bool,
    ) -> Result<RunState> {
        let state = self.load(id)?;
        ensure!(
            state.status.terminal(),
            "cost-only reconciliation requires a terminal run"
        );
        let allowances: Vec<_> = state
            .budget
            .allowances()
            .values()
            .filter(|allowance| {
                !allowance.closed
                    && !(preserve_attempt
                        && state.status == RunStatus::BudgetExhausted
                        && state.attempts.last().is_some_and(|attempt| {
                            !attempt.decided && attempt.review_allowance_id == allowance.id
                        }))
            })
            .map(|allowance| allowance.id.clone())
            .collect();
        if allowances.is_empty()
            && !state.budget.operations().values().any(|operation| {
                matches!(
                    operation.settlement,
                    Settlement::Reserved | Settlement::OutcomeUnknown
                )
            })
        {
            return Ok(state);
        }
        let mut state = self.claim(id, worker, Utc::now())?;
        let generation = state.generation;
        let result = (|| -> Result<RunState> {
            for allowance_id in allowances {
                self.commit(id, generation, RunEvent::LeaseRenewed, Utc::now())?;
                state = self.commit(
                    id,
                    generation,
                    RunEvent::ReviewHeadroomReleased { allowance_id },
                    Utc::now(),
                )?;
            }
            let operations: Vec<_> = state
                .budget
                .operations()
                .values()
                .filter(|operation| {
                    !(preserve_attempt
                        && state.status == RunStatus::BudgetExhausted
                        && state.attempts.last().is_some_and(|attempt| {
                            !attempt.decided && attempt.actor_operation_id == operation.intent.id
                        }))
                        && matches!(
                            operation.settlement,
                            Settlement::Reserved | Settlement::OutcomeUnknown
                        )
                })
                .map(|operation| operation.intent.clone())
                .collect();
            for operation in operations {
                self.commit(id, generation, RunEvent::LeaseRenewed, Utc::now())?;
                if let Some(receipt) = self.terminal_adapter_receipt(id, &operation)? {
                    if let Some(actual) = receipt.actual_cost_microusd {
                        state = self.commit(
                            id,
                            generation,
                            RunEvent::OperationSettled {
                                operation_id: operation.id,
                                actual_cost_microusd: actual,
                                receipt_hash: receipt.receipt_hash,
                            },
                            Utc::now(),
                        )?;
                    }
                }
            }
            Ok(state)
        })();
        let released = self.commit(id, generation, RunEvent::LeaseReleased, Utc::now());
        match result {
            Ok(_) => released,
            Err(error) => {
                let _ = released;
                Err(error)
            },
        }
    }

    pub fn lookup_adapter_job(
        &self,
        id: Uuid,
        operation: &OperationIntent,
    ) -> Result<AdapterJobLookup> {
        let _lock = self.lock(id)?;
        let path = self.job_path(id, operation);
        if !path.exists() {
            return Ok(AdapterJobLookup::NotStarted);
        }
        ensure!(
            !std::fs::symlink_metadata(&path)?.file_type().is_symlink(),
            "adapter job cannot be a symlink"
        );
        let job: AdapterJob = serde_json::from_slice(&std::fs::read(path)?)?;
        ensure!(job.intent == *operation, "adapter job identity mismatch");
        if let Some(budget) = &job.budget {
            budget.validate(operation)?;
        }
        match job.result_hash {
            Some(hash) => Ok(AdapterJobLookup::Completed(self.read_blob(id, &hash)?)),
            None => Ok(AdapterJobLookup::InFlight),
        }
    }

    fn read_adapter_job(&self, id: Uuid, parent: &OperationIntent) -> Result<AdapterJob> {
        let path = self.job_path(id, parent);
        ensure!(
            !std::fs::symlink_metadata(&path)?.file_type().is_symlink(),
            "adapter job cannot be a symlink"
        );
        let job: AdapterJob = serde_json::from_slice(&std::fs::read(path)?)?;
        ensure!(job.intent == *parent, "adapter job identity mismatch");
        job.budget
            .as_ref()
            .context("adapter has no composite budget")?
            .validate(parent)?;
        ensure!(
            job.checkpoint_claim_generation.is_none()
                || (job.checkpoint.is_some()
                    && job
                        .checkpoint_claim_generation
                        .is_some_and(|generation| generation > 0)),
            "adapter continuation claim has no checkpoint"
        );
        Ok(job)
    }

    pub fn adapter_budget(&self, id: Uuid, parent: &OperationIntent) -> Result<AdapterBudget> {
        let _lock = self.lock(id)?;
        self.read_adapter_job(id, parent)?
            .budget
            .context("adapter budget missing")
    }

    /// Controller-owned work area for one exact composite actor operation.
    /// A resumed operation must retain its existing directory; missing files
    /// cannot be mistaken for a fresh attempt. Adapters may grant access to
    /// this leaf only, never the surrounding journal or other attempts.
    pub fn prepare_adapter_workspace(
        &self,
        id: Uuid,
        holder: &str,
        parent: &OperationIntent,
        now: DateTime<Utc>,
    ) -> Result<PathBuf> {
        let _lock = self.lock(id)?;
        let state = self.adapter_owner_state(id, holder, parent, now)?;
        let attempt = state
            .attempts
            .last()
            .context("workspace has no actor attempt")?;
        ensure!(
            state.status == RunStatus::Attempting
                && !attempt.decided
                && attempt.artifact.is_none()
                && attempt.actor_operation_id == parent.id,
            "workspace is not owned by the current unfinished actor"
        );
        let job = self.read_adapter_job(id, parent)?;
        ensure!(
            job.result_hash.is_none(),
            "completed adapter cannot create a work area"
        );
        let run_root = self.dir(id);
        let workspaces = run_root.join("workspaces");
        let leaf = workspaces.join(blake3::hash(parent.id.as_bytes()).to_hex().as_str());
        ensure!(
            (attempt.checkpoint.is_none() && job.checkpoint.is_none()) || leaf.exists(),
            "checkpoint work area is missing; exact continuation cannot recreate it"
        );
        for path in [&run_root, &workspaces, &leaf] {
            create_dirs_durably(path)?;
        }
        Ok(leaf.canonicalize()?)
    }

    /// Publish only after the adapter has closed and drained its runtime
    /// permits. Unknown or outstanding calls cannot form a resumable pause.
    pub fn checkpoint_budgeted_adapter_job(
        &self,
        id: Uuid,
        holder: &str,
        parent: &OperationIntent,
        attention_id: String,
        bytes: &[u8],
        now: DateTime<Utc>,
    ) -> Result<ActorCheckpoint> {
        let content_hash = self.put_blob(id, bytes)?;
        let _lock = self.lock(id)?;
        let state = self.adapter_owner_state(id, holder, parent, now)?;
        ensure!(
            state.status == RunStatus::Attempting
                && state.attempts.last().is_some_and(|attempt| !attempt.decided
                    && attempt.artifact.is_none()
                    && attempt.actor_operation_id == parent.id)
                && !state.owner_inputs.contains_key(&attention_id),
            "checkpoint is not a new question for the current actor"
        );
        let mut job = self.read_adapter_job(id, parent)?;
        ensure!(
            job.result_hash.is_none(),
            "completed adapter cannot be checkpointed"
        );
        let budget = job
            .budget
            .as_ref()
            .context("checkpoint has no composite budget")?;
        ensure!(
            budget.actual_cost_microusd()?.is_some(),
            "checkpoint has unresolved physical calls"
        );
        let checkpoint = ActorCheckpoint {
            content_hash,
            budget_hash: budget.hash()?,
            source_epoch: state.source_epoch,
            attention_id,
        };
        checkpoint.validate()?;
        if job.checkpoint.as_ref() == Some(&checkpoint) && job.checkpoint_claim_generation.is_none()
        {
            return Ok(checkpoint);
        }
        ensure!(
            job.checkpoint.is_none()
                || job.checkpoint_claim_generation == Some(state.generation)
                || (job
                    .checkpoint_claim_generation
                    .is_some_and(|generation| generation < state.generation)
                    && state
                        .attempts
                        .last()
                        .is_some_and(|attempt| attempt.checkpoint == job.checkpoint)
                    && job.checkpoint.as_ref().is_some_and(|previous| state
                        .owner_inputs
                        .contains_key(&previous.attention_id))),
            "parked checkpoint cannot be replaced without exact continuation"
        );
        job.checkpoint = Some(checkpoint.clone());
        job.checkpoint_claim_generation = None;
        write_bytes_durably_sync(&self.job_path(id, parent), &serde_json::to_vec(&job)?)?;
        Ok(checkpoint)
    }

    pub fn adapter_checkpoint(
        &self,
        id: Uuid,
        parent: &OperationIntent,
    ) -> Result<Option<ActorCheckpoint>> {
        let _lock = self.lock(id)?;
        let job = self.read_adapter_job(id, parent)?;
        if job.result_hash.is_some() {
            return Ok(None);
        }
        if let Some(checkpoint) = &job.checkpoint {
            checkpoint.validate()?;
            self.read_blob(id, &checkpoint.content_hash)?;
        }
        Ok(job.checkpoint)
    }

    /// Exact recovery entry. A later lease may reclaim a pause only while
    /// the physical ledger still proves no call occurred since that pause.
    /// The parent reservation, physical-call ceiling and prior usage survive.
    pub fn resume_budgeted_adapter_job(
        &self,
        id: Uuid,
        holder: &str,
        parent: &OperationIntent,
        checkpoint: &ActorCheckpoint,
        now: DateTime<Utc>,
    ) -> Result<AdapterBudget> {
        let _lock = self.lock(id)?;
        checkpoint.validate()?;
        let state = self.adapter_owner_state(id, holder, parent, now)?;
        ensure!(
            state.status == RunStatus::Attempting
                && state.source_epoch > checkpoint.source_epoch
                && state.owner_inputs.contains_key(&checkpoint.attention_id)
                && state.attempts.last().is_some_and(|attempt| !attempt.decided
                    && attempt.artifact.is_none()
                    && attempt.actor_operation_id == parent.id
                    && attempt.checkpoint.as_ref() == Some(checkpoint)),
            "checkpoint continuation needs its exact authenticated answer"
        );
        self.read_blob(id, &checkpoint.content_hash)?;
        let mut job = self.read_adapter_job(id, parent)?;
        ensure!(
            job.result_hash.is_none()
                && job.checkpoint.as_ref() == Some(checkpoint)
                && job
                    .checkpoint_claim_generation
                    .is_none_or(|generation| generation < state.generation),
            "checkpoint continuation is already claimed in this generation"
        );
        let budget = job
            .budget
            .as_ref()
            .context("checkpoint has no composite budget")?;
        ensure!(
            budget.actual_cost_microusd()?.is_some() && budget.hash()? == checkpoint.budget_hash,
            "physical work after checkpoint requires reconciliation before continuation"
        );
        let budget = budget.clone();
        job.checkpoint_claim_generation = Some(state.generation);
        write_bytes_durably_sync(&self.job_path(id, parent), &serde_json::to_vec(&job)?)?;
        Ok(budget)
    }

    fn mutate_adapter_budget<T>(
        &self,
        id: Uuid,
        parent: &OperationIntent,
        mutate: impl FnOnce(&mut AdapterBudget) -> Result<T>,
    ) -> Result<T> {
        let _lock = self.lock(id)?;
        let mut job = self.read_adapter_job(id, parent)?;
        ensure!(
            job.result_hash.is_none(),
            "completed adapter budget is immutable"
        );
        let budget = job.budget.as_mut().context("adapter budget missing")?;
        let result = mutate(budget)?;
        budget.validate(parent)?;
        write_bytes_durably_sync(&self.job_path(id, parent), &serde_json::to_vec(&job)?)?;
        Ok(result)
    }

    /// Called only while holding the run lock. A new physical call must still
    /// belong to the current controller lease and its exact parent reservation.
    fn adapter_owner_state(
        &self,
        id: Uuid,
        holder: &str,
        parent: &OperationIntent,
        now: DateTime<Utc>,
    ) -> Result<RunState> {
        let state = self.fold(&self.records(id)?)?;
        let lease = state
            .lease
            .as_ref()
            .context("adapter dispatch has no lease")?;
        ensure!(
            lease.holder == holder
                && lease.expires_at > now
                && !state.status.terminal()
                && state.deadline_at > now,
            "adapter dispatch lease/deadline lost"
        );
        let operation = state
            .budget
            .operations()
            .get(&parent.id)
            .context("adapter parent reservation missing")?;
        ensure!(
            operation.intent == *parent && operation.settlement == Settlement::Reserved,
            "adapter parent reservation mismatch"
        );
        Ok(state)
    }

    fn adapter_dispatch_deadline(
        &self,
        id: Uuid,
        holder: &str,
        parent: &OperationIntent,
        now: DateTime<Utc>,
    ) -> Result<DateTime<Utc>> {
        let state = self.adapter_owner_state(id, holder, parent, now)?;
        ensure!(
            state.status != RunStatus::WaitingOwner,
            "waiting adapter cannot dispatch physical work"
        );
        let job = self.read_adapter_job(id, parent)?;
        ensure!(
            job.checkpoint.is_none() || job.checkpoint_claim_generation == Some(state.generation),
            "parked adapter requires an exact continuation claim"
        );
        Ok(state
            .deadline_at
            .min(state.lease.as_ref().expect("validated lease").expires_at))
    }

    pub fn reserve_adapter_child(
        &self,
        id: Uuid,
        holder: &str,
        parent: &OperationIntent,
        child: OperationIntent,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.reserve_adapter_child_bounded(id, holder, parent, child, None, now)
    }

    /// An adapter may partition its existing call allowance into explicit
    /// namespaces. Count and admission are checked under the same durable lock.
    #[allow(clippy::too_many_arguments)]
    pub fn reserve_adapter_child_bounded(
        &self,
        id: Uuid,
        holder: &str,
        parent: &OperationIntent,
        child: OperationIntent,
        bucket: Option<(&str, u32)>,
        now: DateTime<Utc>,
    ) -> Result<()> {
        self.mutate_adapter_budget(id, parent, |budget| {
            self.adapter_dispatch_deadline(id, holder, parent, now)?;
            ensure!(
                !budget
                    .ledger
                    .operations()
                    .values()
                    .any(|operation| operation.settlement == Settlement::OutcomeUnknown),
                "physical outcome requires reconciliation before further work"
            );
            ensure!(
                !budget.ledger.operations().contains_key(&child.id),
                "physical child already reserved; reconciliation required"
            );
            ensure!(
                budget.ledger.operations().len() < budget.maximum_operations as usize,
                "composite physical call count reached"
            );
            if let Some((prefix, maximum)) = bucket {
                ensure!(
                    !prefix.is_empty() && maximum > 0 && child.id.starts_with(prefix),
                    "invalid composite call bucket"
                );
                ensure!(
                    budget
                        .ledger
                        .operations()
                        .keys()
                        .filter(|id| id.starts_with(prefix))
                        .count()
                        < maximum as usize,
                    "composite call bucket reached"
                );
            }
            budget.ledger.reserve(child, parent.maximum_cost_microusd)
        })
    }

    /// Sync the irreversible start before invoking the provider, with no
    /// awaited gate afterward. The returned absolute deadline also bounds I/O.
    pub fn start_adapter_child(
        &self,
        id: Uuid,
        holder: &str,
        parent: &OperationIntent,
        child: &OperationIntent,
        now: DateTime<Utc>,
    ) -> Result<DateTime<Utc>> {
        self.mutate_adapter_budget(id, parent, |budget| {
            let deadline = self.adapter_dispatch_deadline(id, holder, parent, now)?;
            // A sibling can become unknown after this child was reserved.
            // Check again under the same durable lock as its physical start.
            ensure!(
                !budget
                    .ledger
                    .operations()
                    .values()
                    .any(|operation| operation.settlement == Settlement::OutcomeUnknown),
                "physical outcome requires reconciliation before further work"
            );
            let reserved = budget
                .ledger
                .operations()
                .get(&child.id)
                .context("physical child reservation missing")?;
            ensure!(
                reserved.intent == *child
                    && reserved.settlement == Settlement::Reserved
                    && !budget.physical_starts.contains_key(&child.id),
                "physical child start identity changed"
            );
            ensure!(now.timestamp_millis() > 0, "physical start clock invalid");
            budget
                .physical_starts
                .insert(child.id.clone(), now.timestamp_millis());
            Ok(deadline)
        })
    }

    pub fn release_adapter_child_pre_io(
        &self,
        id: Uuid,
        parent: &OperationIntent,
        child: &OperationIntent,
        evidence_hash: String,
    ) -> Result<()> {
        self.mutate_adapter_budget(id, parent, |budget| {
            let reserved = budget
                .ledger
                .operations()
                .get(&child.id)
                .context("physical child reservation missing")?;
            ensure!(
                reserved.intent == *child && !budget.physical_starts.contains_key(&child.id),
                "started physical child cannot be released"
            );
            budget.ledger.not_performed(&child.id, evidence_hash)
        })
    }

    /// Late exact usage remains recordable after cancellation or lease loss.
    /// Unknown usage retains its original ceiling, including across restart.
    pub fn settle_adapter_child(
        &self,
        id: Uuid,
        parent: &OperationIntent,
        child: &OperationIntent,
        actual_cost_microusd: Option<u64>,
        receipt_hash: String,
    ) -> Result<()> {
        self.mutate_adapter_budget(id, parent, |budget| {
            let reserved = budget
                .ledger
                .operations()
                .get(&child.id)
                .context("physical child reservation missing")?;
            ensure!(
                reserved.intent == *child && budget.physical_starts.contains_key(&child.id),
                "physical child receipt has no exact durable start"
            );
            match actual_cost_microusd {
                Some(cost) => budget.ledger.settle(&child.id, cost, receipt_hash),
                None => budget.ledger.mark_unknown(&child.id),
            }
        })
    }

    fn lock(&self, id: Uuid) -> Result<std::fs::File> {
        let dir = self.dir(id);
        create_dirs_durably(&dir)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join("controller.lock"))?;
        file.lock_exclusive()?;
        Ok(file)
    }

    fn records(&self, id: Uuid) -> Result<Vec<Record>> {
        let dir = self.dir(id).join("journal");
        if !dir.exists() {
            return Ok(vec![]);
        }
        let mut paths = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .context("invalid journal filename")?;
            if name.starts_with('.') || name.ends_with(".tmp") {
                continue;
            }
            ensure!(
                name.len() == 25
                    && name.ends_with(".json")
                    && name.as_bytes()[..20].iter().all(|b| b.is_ascii_digit()),
                "unexpected committed journal file"
            );
            paths.push(path);
        }
        paths.sort();
        let mut records: Vec<Record> = Vec::new();
        for (sequence, path) in paths.iter().enumerate() {
            let record: Record = serde_json::from_slice(&std::fs::read(path)?)
                .with_context(|| format!("corrupt committed run journal {}", path.display()))?;
            ensure!(
                record.version == 1
                    && record.run_id == id
                    && record.scope == self.scope
                    && record.sequence == sequence as u64,
                "journal identity, version or sequence mismatch"
            );
            ensure!(
                path.file_stem().and_then(|n| n.to_str())
                    == Some(format!("{sequence:020}").as_str()),
                "journal filename/sequence mismatch"
            );
            ensure!(
                record.hash == record.digest()?
                    && record.previous_hash
                        == records.last().map_or_else(|| "".into(), |r| r.hash.clone()),
                "journal integrity/hash chain mismatch"
            );
            records.push(record);
        }
        Ok(records)
    }

    fn fold(&self, records: &[Record]) -> Result<RunState> {
        let first = records.first().context("run not found")?;
        let RunEvent::Admitted {
            contract,
            admission,
        } = &first.event
        else {
            anyhow::bail!("journal did not begin with admission");
        };
        ensure!(
            first.generation == 0
                && admission.owner_principal == self.scope.principal
                && !admission.task_id.trim().is_empty()
                && !admission.source_id.trim().is_empty()
                && valid_hash(&admission.source_hash),
            "invalid owner admission provenance"
        );
        let deadline_at = contract
            .definition()
            .budget
            .deadline_at(first.committed_at)?;
        let deadline_at = admission
            .not_after
            .map_or(deadline_at, |ceiling| ceiling.min(deadline_at));
        let mut state = RunState {
            id: first.run_id,
            scope: self.scope.clone(),
            contract: contract.clone(),
            admission: admission.clone(),
            started_at: first.committed_at,
            last_committed_at: first.committed_at,
            finished_at: None,
            deadline_at,
            generation: 0,
            lease: None,
            source_epoch: 0,
            owner_inputs: BTreeMap::new(),
            status: RunStatus::Admitted,
            budget: BudgetLedger::default(),
            budget_resets: vec![],
            budget_pause: None,
            attempts: vec![],
            retained: Retained::default(),
            reference_artifact: None,
            decisions: vec![],
            reason: None,
            attention_id: None,
            routing: None,
            owner_approved: None,
            handoff: None,
        };
        if let Some(policy) = &admission.routing_policy {
            policy.validate(contract)?;
            ensure!(
                contract
                    .definition()
                    .critics
                    .iter()
                    .any(|p| p.model.as_ref() == Some(&policy.reviewer)),
                "routing reviewer must be a frozen independent critic"
            );
            ensure!(
                !policy.allow_automatic_internal
                    || admission.handoff_kind == HandoffKind::InternalDraft,
                "internal route exceeds admitted handoff"
            );
            ensure!(
                !policy.allow_automatic_capture || admission.handoff_kind == HandoffKind::Capture,
                "capture route exceeds admitted handoff"
            );
        }
        for source in &contract.definition().sources {
            self.read_blob(first.run_id, &source.content_hash)?;
        }
        for record in &records[1..] {
            if let RunEvent::ArtifactSealed { artifact }
            | RunEvent::ActorCompleted { artifact, .. } = &record.event
            {
                self.verify_artifact(first.run_id, artifact)?;
            }
            state.apply(record, self)?;
        }
        Ok(state)
    }

    fn publish(&self, record: &mut Record) -> Result<()> {
        record.hash = record.digest()?;
        let path = self
            .dir(record.run_id)
            .join("journal")
            .join(format!("{:020}.json", record.sequence));
        ensure!(!path.exists(), "journal sequence already committed");
        create_dirs_durably(path.parent().expect("journal parent"))?;
        write_bytes_durably_sync(&path, &serde_json::to_vec(record)?)?;
        Ok(())
    }

    pub fn admit(
        &self,
        id: Uuid,
        contract: FrozenContract,
        admission: Admission,
        now: DateTime<Utc>,
    ) -> Result<RunState> {
        let _lock = self.lock(id)?;
        ensure!(self.records(id)?.is_empty(), "run already admitted");
        let mut record = Record {
            version: 1,
            run_id: id,
            scope: self.scope.clone(),
            sequence: 0,
            generation: 0,
            committed_at: now,
            previous_hash: String::new(),
            event: RunEvent::Admitted {
                contract,
                admission,
            },
            hash: String::new(),
        };
        let state = self.fold(std::slice::from_ref(&record))?;
        self.publish(&mut record)?;
        Ok(state)
    }

    pub fn load(&self, id: Uuid) -> Result<RunState> {
        let _lock = self.lock(id)?;
        self.fold(&self.records(id)?)
    }

    pub fn has_admission(&self, id: Uuid) -> Result<bool> {
        let _lock = self.lock(id)?;
        Ok(!self.records(id)?.is_empty())
    }

    pub fn list(&self) -> Result<Vec<RunState>> {
        self.list_projected(Ok)
    }

    /// Discovery only. An ID grants no authority until its journal is loaded.
    pub fn list_ids(&self) -> Result<Vec<Uuid>> {
        if !self.root.exists() {
            return Ok(vec![]);
        }
        let mut ids = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            ensure!(
                !entry.file_type()?.is_symlink(),
                "run directory cannot be a symlink"
            );
            if let Some(id) = entry
                .file_name()
                .to_str()
                .and_then(|name| Uuid::parse_str(name).ok())
            {
                ids.push(id);
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Scoped catalog scan that discards each full journal after projection.
    /// Derived rows never become storage authority. Newest first, UUID breaks ties.
    pub fn list_projected<T>(&self, project: impl Fn(RunState) -> Result<T>) -> Result<Vec<T>> {
        self.list_projected_results(project)?
            .into_iter()
            .map(|(_, row)| row)
            .collect()
    }

    /// Recovery and read-only catalogs may isolate damaged records, without
    /// inventing a state or allowing that record to dispatch or be approved.
    /// Healthy rows are newest first; unavailable records follow, ordered by ID.
    pub fn list_projected_results<T>(
        &self,
        project: impl Fn(RunState) -> Result<T>,
    ) -> Result<Vec<(Uuid, Result<T>)>> {
        let mut states = Vec::new();
        for id in self.list_ids()? {
            let result = (|| {
                if !self.has_admission(id)? {
                    return Ok(None);
                }
                let state = self.load(id)?;
                Ok(Some((state.started_at, project(state)?)))
            })();
            match result {
                Ok(Some((at, row))) => states.push((Some(at), id, Ok(row))),
                Ok(None) => {},
                Err(error) => states.push((None, id, Err(error))),
            }
        }
        states.sort_by_key(|(at, id, _)| (std::cmp::Reverse(*at), *id));
        Ok(states.into_iter().map(|(_, id, row)| (id, row)).collect())
    }

    /// Controller-owned mutation only. Owner-only cancellation/resume must be
    /// authenticated by the API before reaching this store.
    pub fn commit(
        &self,
        id: Uuid,
        generation: u64,
        event: RunEvent,
        now: DateTime<Utc>,
    ) -> Result<RunState> {
        let _lock = self.lock(id)?;
        let records = self.records(id)?;
        let mut state = self.fold(&records)?;
        if let RunEvent::BudgetResetApproved {
            id,
            approved_by,
            contract_hash,
            reset,
        } = &event
        {
            if let Some(previous) = state.budget_resets.iter().find(|grant| grant.id == *id) {
                ensure!(
                    previous.approved_by == *approved_by
                        && previous.reset == *reset
                        && state.contract.hash()? == *contract_hash,
                    "reset ID belongs to another approval"
                );
                return Ok(state);
            }
        }
        if let RunEvent::OwnerInputRecorded {
            attention_id,
            response_hash,
        } = &event
        {
            if let Some(previous) = state.owner_inputs.get(attention_id) {
                ensure!(
                    previous == response_hash,
                    "answered request cannot receive conflicting evidence"
                );
                return Ok(state);
            }
        }
        if let RunEvent::ActorCheckpointed { checkpoint, .. } = &event {
            let attempt = state
                .attempts
                .last()
                .context("checkpoint has no actor attempt")?;
            let parent = &state
                .budget
                .operations()
                .get(&attempt.actor_operation_id)
                .context("checkpoint has no parent reservation")?
                .intent;
            let job = self.read_adapter_job(id, parent)?;
            ensure!(
                job.result_hash.is_none()
                    && job.checkpoint.as_ref() == Some(checkpoint)
                    && job.checkpoint_claim_generation.is_none()
                    && job
                        .budget
                        .as_ref()
                        .context("checkpoint has no composite budget")?
                        .hash()?
                        == checkpoint.budget_hash,
                "actor checkpoint was not durably parked with its exact call ledger"
            );
        }
        let mut record = Record {
            version: 1,
            run_id: id,
            scope: self.scope.clone(),
            sequence: records.len() as u64,
            generation,
            committed_at: now,
            previous_hash: records.last().expect("folded admission").hash.clone(),
            event,
            hash: String::new(),
        };
        if let RunEvent::ArtifactSealed { artifact } | RunEvent::ActorCompleted { artifact, .. } =
            &record.event
        {
            self.verify_artifact(id, artifact)?;
        }
        state.apply(&record, self)?;
        self.publish(&mut record)?;
        Ok(state)
    }

    pub fn claim(&self, id: Uuid, holder: String, now: DateTime<Utc>) -> Result<RunState> {
        let _lock = self.lock(id)?;
        let records = self.records(id)?;
        let mut state = self.fold(&records)?;
        let generation = state
            .generation
            .checked_add(1)
            .context("generation overflow")?;
        let mut record = Record {
            version: 1,
            run_id: id,
            scope: self.scope.clone(),
            sequence: records.len() as u64,
            generation,
            committed_at: now,
            previous_hash: records.last().expect("folded admission").hash.clone(),
            event: RunEvent::LeaseClaimed { holder },
            hash: String::new(),
        };
        state.apply(&record, self)?;
        self.publish(&mut record)?;
        Ok(state)
    }

    /// Copy bytes into content-addressed controller storage before committing
    /// any reference. Actors never receive paths to these objects.
    pub fn put_blob(&self, id: Uuid, bytes: &[u8]) -> Result<String> {
        let _lock = self.lock(id)?;
        self.put_blob_locked(id, bytes)
    }

    fn put_blob_locked(&self, id: Uuid, bytes: &[u8]) -> Result<String> {
        let hash = blake3::hash(bytes).to_hex().to_string();
        let path = self.dir(id).join("objects").join(&hash);
        if path.exists() {
            self.read_blob(id, &hash)?;
        } else {
            create_dirs_durably(path.parent().expect("object parent"))?;
            write_bytes_durably_sync(&path, bytes)?;
        }
        Ok(hash)
    }

    pub fn read_blob(&self, id: Uuid, hash: &str) -> Result<Vec<u8>> {
        ensure!(valid_hash(hash), "invalid object hash");
        let path = self.dir(id).join("objects").join(hash);
        ensure!(
            !std::fs::symlink_metadata(&path)?.file_type().is_symlink(),
            "sealed object cannot be a symlink"
        );
        let bytes = std::fs::read(&path)?;
        ensure!(
            blake3::hash(&bytes).to_hex().as_str() == hash,
            "sealed object bytes changed"
        );
        Ok(bytes)
    }

    pub fn prepare_artifact(
        &self,
        id: Uuid,
        files: &BTreeMap<String, Vec<u8>>,
    ) -> Result<ArtifactRef> {
        self.prepare_artifact_with_sources(id, files, &BTreeMap::new())
    }

    pub fn prepare_artifact_with_sources(
        &self,
        id: Uuid,
        files: &BTreeMap<String, Vec<u8>>,
        sources: &BTreeMap<String, Vec<u8>>,
    ) -> Result<ArtifactRef> {
        ensure!(!files.is_empty(), "candidate has no files");
        let mut manifest = BTreeMap::new();
        for (name, bytes) in files {
            valid_relative_name(name)?;
            manifest.insert(name.clone(), self.put_blob(id, bytes)?);
        }
        let collected_sources = sources
            .iter()
            .map(|(name, bytes)| {
                ensure!(
                    name.starts_with("collected:")
                        && name.len() <= 512
                        && !name.bytes().any(|b| b.is_ascii_control()),
                    "invalid collected source ID"
                );
                Ok((name.clone(), self.put_blob(id, bytes)?))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let evidence_ids: std::collections::BTreeSet<_> = manifest
            .keys()
            .map(|name| format!("artifact:{name}"))
            .chain(collected_sources.keys().cloned())
            .collect();
        let hash = artifact_digest(&manifest, &evidence_ids, &collected_sources)?;
        Ok(ArtifactRef {
            hash,
            files: manifest,
            evidence_ids,
            collected_sources,
        })
    }

    fn prepared_evidence_path(
        &self,
        id: Uuid,
        artifact_hash: &str,
        recipe_hash: &str,
    ) -> Result<PathBuf> {
        ensure!(
            valid_hash(artifact_hash) && valid_hash(recipe_hash),
            "invalid evidence identity"
        );
        Ok(self
            .dir(id)
            .join("evidence")
            .join(artifact_hash)
            .join(format!("{recipe_hash}.json")))
    }

    /// Fence a zero-cost auxiliary preparation before dispatch. Unlike local
    /// deterministic media reads, a check/transcription with an unresolved
    /// dispatch marker is not repeated. Its result may still be sealed later.
    pub fn begin_evidence_preparation(
        &self,
        id: Uuid,
        holder: &str,
        operation: &OperationIntent,
        artifact_hash: &str,
        preparation_hash: &str,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let _lock = self.lock(id)?;
        let state = self.fold(&self.records(id)?)?;
        state.productive(now)?;
        ensure!(
            state
                .lease
                .as_ref()
                .is_some_and(|lease| lease.holder == holder && lease.expires_at > now)
                && state
                    .budget
                    .operations()
                    .get(&operation.id)
                    .is_some_and(|reserved| &reserved.intent == operation
                        && reserved.settlement == Settlement::Reserved)
                && state
                    .attempts
                    .last()
                    .and_then(|attempt| attempt.artifact.as_ref())
                    .is_some_and(|artifact| artifact.hash == artifact_hash),
            "evidence preparation lost its admitted review binding"
        );
        let path = self
            .prepared_evidence_path(id, artifact_hash, preparation_hash)?
            .with_extension("started.json");
        if path.exists() {
            ensure!(
                !std::fs::symlink_metadata(&path)?.file_type().is_symlink(),
                "preparation marker is a symlink"
            );
            return Ok(false);
        }
        create_dirs_durably(path.parent().context("preparation marker parent missing")?)?;
        write_bytes_durably_sync(
            &path,
            &serde_json::to_vec(&(
                "evidence-preparation-v1",
                artifact_hash,
                preparation_hash,
                operation,
            ))?,
        )?;
        Ok(true)
    }

    /// Read dispatch proof without granting new preparation authority. This
    /// remains available when the original reservation is unknown on recovery.
    pub fn evidence_preparation_started(
        &self,
        id: Uuid,
        artifact_hash: &str,
        preparation_hash: &str,
    ) -> Result<bool> {
        let _lock = self.lock(id)?;
        let path = self
            .prepared_evidence_path(id, artifact_hash, preparation_hash)?
            .with_extension("started.json");
        if !path.exists() {
            return Ok(false);
        }
        ensure!(
            std::fs::symlink_metadata(&path)?.is_file()
                && !std::fs::symlink_metadata(&path)?.file_type().is_symlink(),
            "preparation marker is not a regular file"
        );
        let (version, artifact, preparation, _operation): (
            String,
            String,
            String,
            OperationIntent,
        ) = serde_json::from_slice(&std::fs::read(path)?)?;
        ensure!(
            version == "evidence-preparation-v1"
                && artifact == artifact_hash
                && preparation == preparation_hash,
            "preparation marker binding changed"
        );
        Ok(true)
    }

    /// Derived evidence is produced by an admitted reviewer adapter from the
    /// sealed artifact, outside the actor's writable work area. Every reviewer
    /// and restart uses the same published sample bytes.
    pub fn prepared_evidence(
        &self,
        id: Uuid,
        artifact_hash: &str,
        recipe_hash: &str,
    ) -> Result<Option<BTreeMap<String, Vec<u8>>>> {
        let _lock = self.lock(id)?;
        let path = self.prepared_evidence_path(id, artifact_hash, recipe_hash)?;
        if !path.exists() {
            return Ok(None);
        }
        ensure!(
            !std::fs::symlink_metadata(&path)?.file_type().is_symlink(),
            "evidence manifest cannot be a symlink"
        );
        let references: BTreeMap<String, String> = serde_json::from_slice(&std::fs::read(path)?)?;
        Ok(Some(
            references
                .into_iter()
                .map(|(name, hash)| Ok((name, self.read_blob(id, &hash)?)))
                .collect::<Result<_>>()?,
        ))
    }

    pub fn seal_prepared_evidence(
        &self,
        id: Uuid,
        artifact_hash: &str,
        recipe_hash: &str,
        samples: &BTreeMap<String, Vec<u8>>,
    ) -> Result<()> {
        let references = samples
            .iter()
            .map(|(name, bytes)| {
                valid_relative_name(name)?;
                Ok((name.clone(), self.put_blob(id, bytes)?))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let _lock = self.lock(id)?;
        let state = self.fold(&self.records(id)?)?;
        ensure!(
            state.attempts.iter().any(|attempt| attempt
                .artifact
                .as_ref()
                .is_some_and(|artifact| artifact.hash == artifact_hash)),
            "evidence is not bound to a sealed run artifact"
        );
        let path = self.prepared_evidence_path(id, artifact_hash, recipe_hash)?;
        if path.exists() {
            ensure!(
                !std::fs::symlink_metadata(&path)?.file_type().is_symlink(),
                "evidence manifest cannot be a symlink"
            );
            let previous: BTreeMap<String, String> =
                serde_json::from_slice(&std::fs::read(&path)?)?;
            ensure!(previous == references, "published artifact samples changed");
            return Ok(());
        }
        create_dirs_durably(path.parent().context("evidence manifest parent missing")?)?;
        write_bytes_durably_sync(&path, &serde_json::to_vec(&references)?)?;
        Ok(())
    }

    fn verify_artifact(&self, id: Uuid, artifact: &ArtifactRef) -> Result<()> {
        let expected_ids: std::collections::BTreeSet<_> = artifact
            .files
            .keys()
            .map(|name| format!("artifact:{name}"))
            .chain(artifact.collected_sources.keys().cloned())
            .collect();
        ensure!(
            artifact.evidence_ids == expected_ids && !artifact.files.is_empty(),
            "artifact evidence does not match sealed manifest"
        );
        let expected = artifact_digest(
            &artifact.files,
            &artifact.evidence_ids,
            &artifact.collected_sources,
        )?;
        ensure!(artifact.hash == expected, "artifact manifest hash mismatch");
        for (name, hash) in &artifact.files {
            valid_relative_name(name)?;
            self.read_blob(id, hash)?;
        }
        for (name, hash) in &artifact.collected_sources {
            ensure!(
                name.starts_with("collected:"),
                "invalid collected source ID"
            );
            self.read_blob(id, hash)?;
        }
        Ok(())
    }
}

fn artifact_digest(
    files: &BTreeMap<String, String>,
    evidence: &std::collections::BTreeSet<String>,
    sources: &BTreeMap<String, String>,
) -> Result<String> {
    // Keep every historical candidate hash unchanged when no source was collected.
    let bytes = if sources.is_empty() {
        serde_json::to_vec(&(files, evidence))?
    } else {
        serde_json::to_vec(&(
            "artifact-with-collected-sources-v1",
            files,
            evidence,
            sources,
        ))?
    };
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn create_dirs_durably(path: &Path) -> Result<()> {
    if path.exists() {
        ensure!(
            path.is_dir() && !std::fs::symlink_metadata(path)?.file_type().is_symlink(),
            "trusted directory cannot be a symlink"
        );
        return Ok(());
    }
    let parent = path.parent().context("directory without parent")?;
    create_dirs_durably(parent)?;
    match std::fs::create_dir(path) {
        Ok(()) => sync_parent_dir_blocking(path)?,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            ensure!(
                path.is_dir() && !std::fs::symlink_metadata(path)?.file_type().is_symlink(),
                "trusted directory replaced during creation"
            );
        },
        Err(err) => return Err(err.into()),
    }
    Ok(())
}
