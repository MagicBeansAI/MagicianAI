//! Generic controller around admitted actor/critic adapters. Planning and tools
//! remain with the actor. Adapters must enforce their quoted maximum, preserve
//! the frozen model pin and capability ceiling, and reconcile uncertain jobs.
use std::{collections::BTreeMap, sync::Arc};

use anyhow::{ensure, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    budget::{Allowance, OperationClass, OperationIntent, Settlement},
    contract::{CriticPin, FrozenContract, ModelPin},
    review::{self, ReviewContext, Verdict},
    store::{
        ActorCheckpoint, OperationReceipt, RunEvent, RunState, RunStatus, RunStore, LEASE_SECS,
    },
};

#[cfg(test)]
#[path = "controller_tests.rs"]
mod tests;

/// Wait without constructing an overflowing monotonic timer for an unlimited
/// wall-clock budget. Finite deadlines are checked in portable timer windows.
pub async fn wait_for_deadline(deadline: chrono::DateTime<Utc>) {
    if deadline == chrono::DateTime::<Utc>::MAX_UTC {
        std::future::pending::<()>().await;
    }
    loop {
        let remaining = (deadline - Utc::now()).to_std().unwrap_or_default();
        if remaining.is_zero() {
            return;
        }
        tokio::time::sleep(remaining.min(std::time::Duration::from_secs(86_400))).await;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActorRequest {
    pub contract: FrozenContract,
    pub attempt_number: u32,
    pub source_epoch: u64,
    pub sources: BTreeMap<String, Vec<u8>>,
    pub retained_files: BTreeMap<String, Vec<u8>>,
    /// Structured reviews, decisions and failed changes, framed as data by the
    /// adapter. No mutable ledger path or controller capability is exposed.
    pub feedback: serde_json::Value,
    pub remaining_attempts: u32,
    pub remaining_cost_microusd: u64,
    pub deadline_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptPlan {
    pub model: ModelPin,
    pub actor_maximum_microusd: u64,
    pub maximum_artifact_bytes: u64,
    /// Upper bound for one invocation of each frozen critic, including its
    /// input at the maximum candidate size. Retry headroom is added by code.
    pub critic_maximums_microusd: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ActorEnvelope {
    request: ActorRequest,
    plan: AttemptPlan,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ActorOutput {
    Candidate {
        files: BTreeMap<String, Vec<u8>>,
        model: ModelPin,
        receipt: OperationReceipt,
    },
    /// Only trusted adapter code supplies these bytes. Model-written files
    /// cannot register a source; evidence is part of the candidate identity.
    CandidateWithSources {
        files: BTreeMap<String, Vec<u8>>,
        collected_sources: BTreeMap<String, Vec<u8>>,
        model: ModelPin,
        receipt: OperationReceipt,
    },
    /// A completed attempt produced no contract-valid candidate. Known usage
    /// permits a bounded retry; this is not a request for owner clarification.
    FailedAttempt {
        reason: String,
        receipt: OperationReceipt,
    },
    Blocked {
        reason: String,
        receipt: OperationReceipt,
    },
    /// Known nonterminal pause. Its physical usage remains inside the same
    /// parent reservation; only a qualified exact continuation may resume it.
    Paused {
        reason: String,
        checkpoint: ActorCheckpoint,
    },
}

pub enum Recovery<T> {
    Completed(T),
    /// Provider/local operation lookup proves no work happened. A newly
    /// billable call still needs a new operation ID and reservation.
    NotPerformed {
        evidence_hash: String,
    },
    Unknown {
        reason: String,
    },
}

#[async_trait]
pub trait ActorAdapter: Send + Sync {
    /// Stop and join local adapter work after a pass ends or is interrupted.
    /// This hook must not dispatch productive work or alter the frozen budget.
    async fn quiesce(&self) -> Result<()> {
        Ok(())
    }
    /// Non-billable metadata/math only. Refuse if an enforceable
    /// cost/output/capability bound is unavailable; never use a model to quote.
    fn plan(&self, request: &ActorRequest) -> Result<AttemptPlan>;
    async fn run(
        &self,
        request: &ActorRequest,
        plan: &AttemptPlan,
        operation: &OperationIntent,
        cancel: CancellationToken,
    ) -> Result<ActorOutput>;
    async fn reconcile(
        &self,
        _operation: &OperationIntent,
        _cancel: CancellationToken,
    ) -> Result<Recovery<ActorOutput>> {
        Ok(Recovery::Unknown {
            reason: "actor adapter has no durable job lookup".into(),
        })
    }

    async fn resume(
        &self,
        _request: &ActorRequest,
        _plan: &AttemptPlan,
        _operation: &OperationIntent,
        _checkpoint: &ActorCheckpoint,
        _cancel: CancellationToken,
    ) -> Result<ActorOutput> {
        anyhow::bail!("actor adapter has no qualified checkpoint continuation")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriticRetryFeedback {
    pub validation_error: String,
    pub previous_verdict: Verdict,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriticRequest {
    pub contract: FrozenContract,
    pub critic: CriticPin,
    pub artifact_hash: String,
    pub source_epoch: u64,
    pub artifact_files: BTreeMap<String, Vec<u8>>,
    pub sources: BTreeMap<String, Vec<u8>>,
    pub evidence_ids: std::collections::BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_feedback: Option<CriticRetryFeedback>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriticOutput {
    pub verdict: Verdict,
    pub receipt: OperationReceipt,
}

#[async_trait]
pub trait CriticAdapter: Send + Sync {
    async fn quiesce(&self) -> Result<()> {
        Ok(())
    }
    async fn run(
        &self,
        request: &CriticRequest,
        operation: &OperationIntent,
        cancel: CancellationToken,
    ) -> Result<CriticOutput>;
    async fn reconcile(
        &self,
        _operation: &OperationIntent,
        _cancel: CancellationToken,
    ) -> Result<Recovery<CriticOutput>> {
        Ok(Recovery::Unknown {
            reason: "critic adapter has no durable job lookup".into(),
        })
    }
}

pub struct Controller {
    pub store: Arc<RunStore>,
    pub actor: Arc<dyn ActorAdapter>,
    pub critic: Arc<dyn CriticAdapter>,
    pub worker_id: String,
}

impl Controller {
    async fn io<T: Send + 'static>(
        &self,
        f: impl FnOnce(&RunStore) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || f(&store))
            .await
            .context("run store worker join")?
    }

    async fn commit(&self, state: &RunState, event: RunEvent) -> Result<RunState> {
        let id = state.id;
        let generation = state.generation;
        self.io(move |s| s.commit(id, generation, event, Utc::now()))
            .await
    }

    async fn block(&self, state: &RunState, reason: String) -> Result<RunState> {
        // Stable identity across resume; the runtime attention projection uses
        // this key rather than minting a second owner question after restart.
        let attention_id = format!("relentless-{}-{}", state.id, state.attempts.len());
        self.commit(
            state,
            RunEvent::Blocked {
                reason,
                attention_id,
            },
        )
        .await
    }

    async fn sources(&self, state: &RunState) -> Result<BTreeMap<String, Vec<u8>>> {
        let id = state.id;
        let pins = state.evaluation_contract()?.definition().sources.clone();
        self.io(move |s| {
            pins.into_iter()
                .map(|p| Ok((p.id, s.read_blob(id, &p.content_hash)?)))
                .collect()
        })
        .await
    }

    async fn files(
        &self,
        state: &RunState,
        files: BTreeMap<String, String>,
    ) -> Result<BTreeMap<String, Vec<u8>>> {
        let id = state.id;
        self.io(move |s| {
            files
                .into_iter()
                .map(|(name, hash)| Ok((name, s.read_blob(id, &hash)?)))
                .collect()
        })
        .await
    }

    async fn actor_request(&self, state: &RunState) -> Result<ActorRequest> {
        let retained_files = if let Some(retained) = state.reference_for_actor() {
            let files = state
                .attempts
                .iter()
                .filter_map(|a| a.artifact.as_ref())
                .find(|a| a.hash == retained.artifact_hash)
                .context("retained artifact manifest missing")?
                .files
                .clone();
            self.files(state, files).await?
        } else {
            BTreeMap::new()
        };
        let spent = state.budget.spend()?.total()?;
        let limits = state.effective_budget()?;
        Ok(ActorRequest {
            contract: state.evaluation_contract()?,
            attempt_number: u32::try_from(state.attempts.len())? + 1,
            source_epoch: state.source_epoch,
            sources: self.sources(state).await?,
            retained_files,
            feedback: serde_json::json!({"retained": state.retained, "attempts": state.attempts, "decisions": state.decisions}),
            remaining_attempts: limits
                .attempt_ceiling()
                .saturating_sub(u32::try_from(state.attempts.len())?),
            remaining_cost_microusd: limits.cost_ceiling().saturating_sub(spent),
            deadline_at: state.deadline_at,
        })
    }

    async fn prepare_attempt(&self, state: &RunState) -> Result<RunState> {
        let request = self.actor_request(state).await?;
        if request.remaining_attempts == 0 {
            return self
                .commit(
                    state,
                    RunEvent::BudgetUnavailable {
                        required_microusd: 0,
                    },
                )
                .await;
        }
        let plan = self.actor.plan(&request)?;
        ensure!(
            plan.model == state.contract.definition().actor && plan.maximum_artifact_bytes > 0,
            "adapter did not preserve actor model/output bound"
        );
        let expected: std::collections::BTreeSet<_> = state
            .contract
            .definition()
            .critics
            .iter()
            .map(|c| c.id.clone())
            .collect();
        ensure!(
            plan.critic_maximums_microusd
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>()
                == expected,
            "adapter omitted required critic headroom"
        );
        let invocations = u64::from(state.contract.definition().review.max_retries_per_critic) + 1;
        let headroom = plan
            .critic_maximums_microusd
            .values()
            .try_fold(0u64, |sum, cost| {
                sum.checked_add(
                    cost.checked_mul(invocations)
                        .context("critic headroom overflow")?,
                )
                .context("critic headroom overflow")
            })?;
        let required = plan
            .actor_maximum_microusd
            .checked_add(headroom)
            .context("attempt quote overflow")?;
        if required > request.remaining_cost_microusd {
            return self
                .commit(
                    state,
                    RunEvent::BudgetUnavailable {
                        required_microusd: required,
                    },
                )
                .await;
        }
        let bytes = serde_json::to_vec(&ActorEnvelope {
            request,
            plan: plan.clone(),
        })?;
        let id = state.id;
        let input_hash = self.io(move |s| s.put_blob(id, &bytes)).await?;
        self.commit(
            state,
            RunEvent::AttemptStarted {
                actor: OperationIntent {
                    id: format!("actor-{}", Uuid::new_v4()),
                    input_hash: input_hash.clone(),
                    class: OperationClass::Actor,
                    maximum_cost_microusd: plan.actor_maximum_microusd,
                },
                review: Allowance {
                    id: format!("reviews-{}", Uuid::new_v4()),
                    input_hash,
                    maximum_microusd: headroom,
                    remaining_microusd: headroom,
                    closed: false,
                },
            },
        )
        .await
    }

    async fn envelope(&self, state: &RunState) -> Result<ActorEnvelope> {
        let attempt = state.attempts.last().context("missing actor attempt")?;
        let hash = state
            .budget
            .operations()
            .get(&attempt.actor_operation_id)
            .context("missing actor reservation")?
            .intent
            .input_hash
            .clone();
        let id = state.id;
        self.io(move |s| Ok(serde_json::from_slice(&s.read_blob(id, &hash)?)?))
            .await
    }

    async fn actor_output(
        &self,
        state: &RunState,
        envelope: &ActorEnvelope,
        output: ActorOutput,
    ) -> Result<RunState> {
        let output = match output {
            ActorOutput::CandidateWithSources {
                files,
                collected_sources,
                model,
                receipt,
            } => {
                return self
                    .seal_candidate(state, envelope, files, collected_sources, model, receipt)
                    .await;
            },
            other => other,
        };
        match output {
            ActorOutput::Candidate {
                files,
                model,
                receipt,
            } => {
                self.seal_candidate(state, envelope, files, BTreeMap::new(), model, receipt)
                    .await
            },
            ActorOutput::CandidateWithSources { .. } => unreachable!("handled above"),
            ActorOutput::FailedAttempt { reason, receipt } => {
                self.commit(state, RunEvent::ActorFailed { reason, receipt })
                    .await
            },
            ActorOutput::Blocked { reason, receipt } => {
                let operation_id = state
                    .attempts
                    .last()
                    .context("missing attempt")?
                    .actor_operation_id
                    .clone();
                ensure!(
                    state.budget.operations()[&operation_id].intent.input_hash
                        == receipt.input_hash,
                    "blocker receipt does not bind actor input"
                );
                let charged = if let Some(actual) = receipt.actual_cost_microusd {
                    self.commit(
                        state,
                        RunEvent::OperationSettled {
                            operation_id,
                            actual_cost_microusd: actual,
                            receipt_hash: receipt.receipt_hash,
                        },
                    )
                    .await?
                } else {
                    self.commit(state, RunEvent::OperationUnknown { operation_id })
                        .await?
                };
                self.block(&charged, reason).await
            },
            ActorOutput::Paused { reason, checkpoint } => {
                self.commit(state, RunEvent::ActorCheckpointed { checkpoint, reason })
                    .await
            },
        }
    }

    async fn seal_candidate(
        &self,
        state: &RunState,
        envelope: &ActorEnvelope,
        files: BTreeMap<String, Vec<u8>>,
        collected_sources: BTreeMap<String, Vec<u8>>,
        model: ModelPin,
        receipt: OperationReceipt,
    ) -> Result<RunState> {
        ensure!(
            model == state.contract.definition().actor,
            "actor departed from frozen model configuration"
        );
        let total = files.values().try_fold(0u64, |sum, b| {
            sum.checked_add(u64::try_from(b.len())?)
                .context("artifact byte size overflow")
        })?;
        if total > envelope.plan.maximum_artifact_bytes {
            return self
                .commit(
                    state,
                    RunEvent::ActorFailed {
                        reason: format!(
                            "candidate exceeds admitted byte ceiling: {total} > {}",
                            envelope.plan.maximum_artifact_bytes
                        ),
                        receipt,
                    },
                )
                .await;
        }
        let id = state.id;
        let artifact = self
            .io(move |s| s.prepare_artifact_with_sources(id, &files, &collected_sources))
            .await?;
        self.commit(
            state,
            RunEvent::ActorCompleted {
                artifact,
                model,
                receipt,
            },
        )
        .await
    }

    async fn actor_step(
        &self,
        state: &RunState,
        fresh: bool,
        cancel: CancellationToken,
    ) -> Result<RunState> {
        let envelope = self.envelope(state).await?;
        let attempt = state.attempts.last().context("missing actor attempt")?;
        let operation = state.budget.operations()[&attempt.actor_operation_id]
            .intent
            .clone();
        if fresh {
            let output = self
                .actor
                .run(&envelope.request, &envelope.plan, &operation, cancel)
                .await?;
            return self.actor_output(state, &envelope, output).await;
        }
        let recovery = match &state.budget.operations()[&operation.id].settlement {
            Settlement::NotPerformed { evidence_hash } => Recovery::NotPerformed {
                evidence_hash: evidence_hash.clone(),
            },
            _ => self.actor.reconcile(&operation, cancel.clone()).await?,
        };
        match recovery {
            Recovery::Completed(ActorOutput::Paused { reason, checkpoint }) => {
                if !state.owner_inputs.contains_key(&checkpoint.attention_id) {
                    return self
                        .actor_output(state, &envelope, ActorOutput::Paused { reason, checkpoint })
                        .await;
                }
                ensure!(
                    attempt.checkpoint.as_ref() == Some(&checkpoint),
                    "adapter checkpoint differs from the journaled pause"
                );
                // Keep the original operation/input and quote. The resume view
                // adds authenticated evidence to this same attempt; it cannot
                // create another attempt or replenish actor/review headroom.
                let mut request = self.actor_request(state).await?;
                request.attempt_number = envelope.request.attempt_number;
                // The parent and review reservations already fund this exact
                // attempt. Requoting against unreserved run dollars would
                // shrink a budget-capped actor after its first question. Keep
                // its original planning allowance; the same operation/child
                // ledger still enforces all previously consumed resources.
                request.remaining_cost_microusd = envelope.request.remaining_cost_microusd;
                ensure!(
                    serde_json::to_value(self.actor.plan(&request)?)?
                        == serde_json::to_value(&envelope.plan)?,
                    "checkpoint continuation changed the frozen actor quote"
                );
                let output = self
                    .actor
                    .resume(&request, &envelope.plan, &operation, &checkpoint, cancel)
                    .await?;
                self.actor_output(state, &envelope, output).await
            },
            Recovery::Completed(output) => self.actor_output(state, &envelope, output).await,
            Recovery::NotPerformed { evidence_hash } => {
                let released = self
                    .commit(
                        state,
                        RunEvent::OperationNotPerformed {
                            operation_id: operation.id.clone(),
                            evidence_hash,
                        },
                    )
                    .await?;
                // Proven pre-I/O interruption is safe to readmit, preserving
                // this attempt, envelope and review headroom. Unknown jobs
                // never enter this path. Journal the new reservation first.
                let replacement = OperationIntent {
                    id: format!("actor-{}", Uuid::new_v4()),
                    ..operation.clone()
                };
                let restarted = self
                    .commit(
                        &released,
                        RunEvent::ActorRestarted {
                            previous_operation_id: operation.id,
                            replacement: replacement.clone(),
                        },
                    )
                    .await?;
                let output = self
                    .actor
                    .run(&envelope.request, &envelope.plan, &replacement, cancel)
                    .await?;
                self.actor_output(&restarted, &envelope, output).await
            },
            Recovery::Unknown { reason } => {
                let held = self
                    .commit(
                        state,
                        RunEvent::OperationUnknown {
                            operation_id: operation.id,
                        },
                    )
                    .await?;
                self.block(&held, reason).await
            },
        }
    }

    async fn critic_request(&self, state: &RunState, pin: CriticPin) -> Result<CriticRequest> {
        let artifact = state
            .attempts
            .last()
            .and_then(|a| a.artifact.as_ref())
            .context("missing sealed artifact")?;
        let mut sources = self.sources(state).await?;
        let id = state.id;
        let collected = artifact.collected_sources.clone();
        sources.extend(
            self.io(move |s| {
                collected
                    .into_iter()
                    .map(|(name, hash)| Ok((name, s.read_blob(id, &hash)?)))
                    .collect::<Result<BTreeMap<_, _>>>()
            })
            .await?,
        );
        Ok(CriticRequest {
            retry_feedback: None,
            contract: state.evaluation_contract()?,
            critic: pin,
            artifact_hash: artifact.hash.clone(),
            source_epoch: state.source_epoch,
            artifact_files: self.files(state, artifact.files.clone()).await?,
            sources,
            evidence_ids: artifact
                .evidence_ids
                .iter()
                .cloned()
                .chain(
                    state
                        .evaluation_contract()?
                        .definition()
                        .sources
                        .iter()
                        .map(|s| s.id.clone()),
                )
                .collect(),
        })
    }

    async fn review_step(
        &self,
        mut state: RunState,
        cancel: CancellationToken,
    ) -> Result<RunState> {
        let envelope = self.envelope(&state).await?;
        for pin in state.contract.definition().critics.clone() {
            loop {
                let mut request = self.critic_request(&state, pin.clone()).await?;
                let context = ReviewContext {
                    artifact_hash: request.artifact_hash.clone(),
                    source_epoch: request.source_epoch,
                    evidence_ids: request.evidence_ids.clone(),
                    artifact_texts: request
                        .artifact_files
                        .values()
                        .filter_map(|bytes| std::str::from_utf8(bytes).ok().map(str::to_owned))
                        .collect(),
                    sources: request.sources.clone(),
                };
                let attempt = state.attempts.last().expect("sealed attempt");
                if let Some(verdict) = attempt.verdicts.get(&pin.id) {
                    match review::validate_verdict(
                        &pin,
                        verdict,
                        &state.evaluation_contract()?,
                        &state.evaluation_contract()?.hash()?,
                        &context,
                    ) {
                        Ok(()) => break,
                        Err(error) => {
                            request.retry_feedback = Some(CriticRetryFeedback {
                                validation_error: error.to_string(),
                                previous_verdict: verdict.clone(),
                            });
                        },
                    }
                }
                let prior_id = attempt
                    .critic_operations
                    .get(&pin.id)
                    .and_then(|ids| ids.last())
                    .cloned();
                if let Some(operation_id) = prior_id {
                    if !attempt
                        .verdict_operation_ids
                        .values()
                        .any(|id| id == &operation_id)
                    {
                        let operation = state.budget.operations()[&operation_id].intent.clone();
                        let recovery = match &state.budget.operations()[&operation_id].settlement {
                            Settlement::NotPerformed { evidence_hash } => Recovery::NotPerformed {
                                evidence_hash: evidence_hash.clone(),
                            },
                            _ => {
                                self.critic
                                    .reconcile(&operation, cancel.child_token())
                                    .await?
                            },
                        };
                        match recovery {
                            Recovery::Completed(output) => {
                                state = self
                                    .commit(
                                        &state,
                                        RunEvent::CriticCompleted {
                                            critic_id: pin.id.clone(),
                                            operation_id,
                                            verdict: output.verdict,
                                            receipt: output.receipt,
                                        },
                                    )
                                    .await?;
                                continue;
                            },
                            Recovery::NotPerformed { evidence_hash } => {
                                state = self
                                    .commit(
                                        &state,
                                        RunEvent::OperationNotPerformed {
                                            operation_id: operation_id.clone(),
                                            evidence_hash,
                                        },
                                    )
                                    .await?;
                                // Proven absence is a continuation of the same
                                // review leg, not a paid/malformed retry. Its
                                // original allocation was already consumed;
                                // reserve exactly that freed quote, not a second
                                // slice of the remaining retry allowance.
                                let replacement = OperationIntent {
                                    id: format!("critic-{}", Uuid::new_v4()),
                                    ..operation
                                };
                                state = self
                                    .commit(
                                        &state,
                                        RunEvent::ReviewRestarted {
                                            critic_id: pin.id.clone(),
                                            previous_operation_id: operation_id,
                                            replacement: replacement.clone(),
                                        },
                                    )
                                    .await?;
                                let output = self
                                    .critic
                                    .run(&request, &replacement, cancel.child_token())
                                    .await?;
                                state = self
                                    .commit(
                                        &state,
                                        RunEvent::CriticCompleted {
                                            critic_id: pin.id.clone(),
                                            operation_id: replacement.id,
                                            verdict: output.verdict,
                                            receipt: output.receipt,
                                        },
                                    )
                                    .await?;
                                continue;
                            },
                            Recovery::Unknown { reason } => {
                                state = self
                                    .commit(&state, RunEvent::OperationUnknown { operation_id })
                                    .await?;
                                return self.block(&state, reason).await;
                            },
                        }
                    }
                }
                let used = state
                    .attempts
                    .last()
                    .expect("attempt")
                    .critic_operations
                    .get(&pin.id)
                    .map_or(0, |ids| {
                        ids.iter()
                            .filter(|id| {
                                !matches!(
                                    state.budget.operations()[*id].settlement,
                                    Settlement::NotPerformed { .. }
                                )
                            })
                            .count()
                    });
                if used as u64
                    > u64::from(state.contract.definition().review.max_retries_per_critic)
                {
                    break;
                }
                let bytes = serde_json::to_vec(&request)?;
                let id = state.id;
                let input_hash = self.io(move |s| s.put_blob(id, &bytes)).await?;
                let operation = OperationIntent {
                    id: format!("critic-{}", Uuid::new_v4()),
                    input_hash,
                    class: OperationClass::Critic,
                    maximum_cost_microusd: envelope.plan.critic_maximums_microusd[&pin.id],
                };
                state = self
                    .commit(
                        &state,
                        RunEvent::ReviewStarted {
                            critic_id: pin.id.clone(),
                            operation: operation.clone(),
                        },
                    )
                    .await?;
                let output = self
                    .critic
                    .run(&request, &operation, cancel.child_token())
                    .await?;
                state = self
                    .commit(
                        &state,
                        RunEvent::CriticCompleted {
                            critic_id: pin.id.clone(),
                            operation_id: operation.id,
                            verdict: output.verdict,
                            receipt: output.receipt,
                        },
                    )
                    .await?;
            }
        }
        state = self.commit(&state, RunEvent::AttemptDecided).await?;
        if state.status == RunStatus::WaitingOwner {
            let reason = state
                .reason
                .clone()
                .unwrap_or_else(|| "review unknown".into());
            state = self.block(&state, reason).await?;
        }
        Ok(state)
    }

    async fn drive(&self, mut state: RunState, cancel: CancellationToken) -> Result<RunState> {
        let mut fresh = false;
        loop {
            if cancel.is_cancelled() {
                anyhow::bail!("controller cancelled or lease lost");
            }
            if state.status.terminal() {
                return Ok(state);
            }
            if Utc::now() >= state.deadline_at {
                return self.commit(&state, RunEvent::DeadlineExpired).await;
            }
            match state.status {
                RunStatus::Admitted => match self.prepare_attempt(&state).await {
                    Ok(admitted) => {
                        state = admitted;
                        fresh = true;
                    },
                    Err(error) => {
                        return self
                            .block(&state, format!("attempt admission unavailable: {error:#}"))
                            .await
                    },
                },
                RunStatus::Attempting => {
                    state = self.actor_step(&state, fresh, cancel.child_token()).await?;
                    fresh = false;
                },
                RunStatus::Reviewing => {
                    state = self.review_step(state, cancel.child_token()).await?
                },
                _ => return Ok(state),
            }
        }
    }

    pub async fn run_pass(&self, id: Uuid, parent_cancel: CancellationToken) -> Result<RunState> {
        let worker = self.worker_id.clone();
        let state = self.io(move |s| s.claim(id, worker, Utc::now())).await?;
        let generation = state.generation;
        let deadline = state.deadline_at;
        let cancel = parent_cancel.child_token();
        let _cancel_on_drop = cancel.clone().drop_guard();
        let renewal_store = self.store.clone();
        let renewal_cancel = CancellationToken::new();
        let work_cancel = cancel.clone();
        let heartbeat = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs((LEASE_SECS / 2) as u64));
            interval.tick().await;
            loop {
                tokio::select! {
                    _ = renewal_cancel.cancelled() => break,
                    _ = interval.tick() => {
                        let store = renewal_store.clone();
                        if !matches!(tokio::task::spawn_blocking(move || store.commit(id, generation, RunEvent::LeaseRenewed, Utc::now())).await, Ok(Ok(_))) { work_cancel.cancel(); break; }
                    }
                }
            }
        }));
        let result = tokio::select! {
            result = self.drive(state, cancel.clone()) => result,
            _ = cancel.cancelled() => self.io(move |s| s.load(id)).await,
            _ = wait_for_deadline(deadline) => self.io(move |s| {
                let current = s.load(id)?;
                if current.status.terminal() { Ok(current) }
                else { s.commit(id, generation, RunEvent::DeadlineExpired, Utc::now()) }
            }).await,
        };
        cancel.cancel();
        let actor_drain = self.actor.quiesce().await;
        let critic_drain = self.critic.quiesce().await;
        heartbeat.abort();
        let result = match (actor_drain, critic_drain) {
            (Ok(()), Ok(())) => result,
            (actor, critic) => Err(anyhow::anyhow!(
                "adapter local work did not drain: actor={actor:?}; critic={critic:?}"
            )),
        };
        let result = match result {
            Err(error) => {
                match self.io(move |s| s.load(id)).await {
                    // Cleanup belongs to the generation claimed by this
                    // pass. Reloading a successor is not authority to mutate
                    // its run after this worker has lost its lease.
                    Ok(current) if current.generation != generation => Ok(current),
                    Ok(current) if current.status.terminal() || current.status == RunStatus::WaitingOwner => Ok(current),
                    Ok(current) if Utc::now() >= current.deadline_at => self.commit(&current, RunEvent::DeadlineExpired).await,
                    Ok(current) => self.block(&current, format!("adapter/controller unavailable; outstanding operations require reconciliation: {error:#}")).await,
                    Err(_) => Err(error),
                }
            },
            result => result,
        };
        // Returning an error still gives the lease back; unknown provider work
        // keeps its reservation and gets reconciled on the next pass.
        let _ = self
            .io(move |s| s.commit(id, generation, RunEvent::LeaseReleased, Utc::now()))
            .await;
        result
    }
}
