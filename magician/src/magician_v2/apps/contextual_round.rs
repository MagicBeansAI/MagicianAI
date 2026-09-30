//! Deterministic progression for context-first, independently settled rounds.
//!
//! This is a planner/checkpoint, not an execution authority. The workflow owner
//! supplies already permitted context, persists every transition, reserves the
//! normal resource ledger, dispatches through the LLM queue and commits through
//! the App mutation owner. No provider, database, task or App identity is baked
//! into this component. In particular a draft is never counted as a committed result.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

const SCHEMA: &str = "magician.app-contextual-round.v1";

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRoundUsage {
    pub tokens: u64,
    pub micro_usd: u64,
}

impl AppRoundUsage {
    fn add(self, other: Self) -> Result<Self, AppRoundError> {
        Ok(Self {
            tokens: self
                .tokens
                .checked_add(other.tokens)
                .ok_or(AppRoundError::Budget)?,
            micro_usd: self
                .micro_usd
                .checked_add(other.micro_usd)
                .ok_or(AppRoundError::Budget)?,
        })
    }

    fn fits(self, ceiling: Self) -> bool {
        self.tokens <= ceiling.tokens && self.micro_usd <= ceiling.micro_usd
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRoundLimits {
    pub max_participants: u16,
    pub max_concurrent: u16,
    pub max_attempts_per_participant: u16,
    pub aggregate: AppRoundUsage,
    pub per_participant: AppRoundUsage,
}

impl AppRoundLimits {
    pub fn validate(&self) -> Result<(), AppRoundError> {
        if self.max_participants == 0
            || self.max_concurrent == 0
            || self.max_concurrent > self.max_participants
            || self.max_attempts_per_participant == 0
            || self.per_participant.tokens == 0
            || !self.per_participant.fits(self.aggregate)
        {
            return Err(AppRoundError::Budget);
        }
        Ok(())
    }
}

/// Produced after the owner checks live membership, enrollment, policy,
/// cooldowns and permitted context. Excluded candidates never buy a model call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRoundCandidate {
    pub participant_id: String,
    pub context: Value,
    pub exclusion: Option<String>,
    #[serde(default)]
    pub preflight_failure: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppRoundModelResult {
    Draft { value: Value },
    Quiet { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppRoundParticipantState {
    Pending,
    InFlight {
        attempt_id: String,
        reservation: AppRoundUsage,
    },
    Prepared {
        draft: Value,
        mutation_key: String,
    },
    Committed {
        record_ids: Vec<String>,
        receipt_refs: Vec<String>,
    },
    Quiet {
        reason: String,
    },
    Failed {
        code: String,
    },
    Deferred {
        reason: String,
    },
}

impl AppRoundParticipantState {
    fn terminal(&self) -> bool {
        matches!(
            self,
            Self::Committed { .. }
                | Self::Quiet { .. }
                | Self::Failed { .. }
                | Self::Deferred { .. }
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SettledAttempt {
    attempt_id: String,
    usage: AppRoundUsage,
    settlement_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRoundParticipant {
    participant_id: String,
    context: Value,
    #[serde(default)]
    preflight_failure: Option<String>,
    context_digest: String,
    /// An optional just-in-time snapshot, sealed before this participant's
    /// first attempt. The initial context still owns the immutable round plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dispatch_context: Option<AppRoundDispatchContext>,
    attempts_started: u16,
    settled: Vec<SettledAttempt>,
    state: AppRoundParticipantState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AppRoundDispatchContext {
    value: Value,
    digest: String,
    prepared_at_ms: i64,
}

impl AppRoundParticipant {
    pub fn participant_id(&self) -> &str {
        &self.participant_id
    }
    pub fn context(&self) -> &Value {
        self.dispatch_context
            .as_ref()
            .map_or(&self.context, |c| &c.value)
    }
    fn context_digest(&self) -> &str {
        self.dispatch_context
            .as_ref()
            .map_or(&self.context_digest, |c| &c.digest)
    }
    pub fn dispatch_prepared_at_ms(&self) -> Option<i64> {
        self.dispatch_context.as_ref().map(|c| c.prepared_at_ms)
    }
    pub fn state(&self) -> &AppRoundParticipantState {
        &self.state
    }
    pub fn spent(&self) -> Result<AppRoundUsage, AppRoundError> {
        self.settled
            .iter()
            .try_fold(AppRoundUsage::default(), |sum, attempt| {
                sum.add(attempt.usage)
            })
    }
}

/// The caller persists the InFlight transition before dispatch. This value
/// identifies a request; it cannot stand in for a physical resource permit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppRoundModelClaim {
    pub participant_id: String,
    pub attempt_id: String,
    pub context: Value,
    pub context_digest: String,
    pub maximum_usage: AppRoundUsage,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppContextualRound {
    schema: String,
    round_ref: String,
    plan_digest: String,
    limits: AppRoundLimits,
    participants: Vec<AppRoundParticipant>,
    excluded: Vec<(String, String)>,
    previous_cursor: Option<String>,
    next_cursor: Option<String>,
    cancelled: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
pub struct AppRoundSummary {
    pub considered: usize,
    pub selected: usize,
    pub committed: usize,
    pub quiet: usize,
    pub failed: usize,
    pub deferred: usize,
    pub excluded: usize,
    pub awaiting_model: usize,
    pub awaiting_commit: usize,
    pub spent: AppRoundUsage,
    pub reserved: AppRoundUsage,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AppRoundError {
    #[error("invalid contextual round identity or checkpoint")]
    Identity,
    #[error("invalid contextual round budget or settlement")]
    Budget,
    #[error("contextual round transition does not match its retained attempt")]
    Transition,
    #[error("a contextual round commit needs a nonempty record and receipt")]
    Receipt,
    #[error("contextual round model result is missing a draft or explicit quiet reason")]
    Output,
}

fn identity(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
}

fn digest(value: &impl Serialize) -> Result<String, AppRoundError> {
    let bytes = serde_json::to_vec(value).map_err(|_| AppRoundError::Identity)?;
    Ok(format!("blake3:{}", blake3::hash(&bytes)))
}

impl AppContextualRound {
    pub fn plan(
        round_ref: String,
        after_participant: Option<&str>,
        mut candidates: Vec<AppRoundCandidate>,
        limits: AppRoundLimits,
    ) -> Result<Self, AppRoundError> {
        limits.validate()?;
        if !identity(&round_ref) || after_participant.is_some_and(|id| !identity(id)) {
            return Err(AppRoundError::Identity);
        }
        let mut seen = BTreeSet::new();
        if candidates.iter().any(|candidate| {
            !identity(&candidate.participant_id)
                || !seen.insert(candidate.participant_id.clone())
                || candidate
                    .exclusion
                    .as_ref()
                    .is_some_and(|reason| !identity(reason))
                || candidate
                    .preflight_failure
                    .as_ref()
                    .is_some_and(|reason| !identity(reason))
                || (candidate.preflight_failure.is_some() && candidate.exclusion.is_some())
                || (candidate.exclusion.is_none() && !candidate.context.is_object())
        }) {
            return Err(AppRoundError::Identity);
        }
        candidates.sort_by(|a, b| a.participant_id.cmp(&b.participant_id));
        if let Some(after) = after_participant {
            let start =
                candidates.partition_point(|candidate| candidate.participant_id.as_str() <= after);
            candidates.rotate_left(start);
        }
        let mut participants = Vec::new();
        let mut excluded = Vec::new();
        let mut next_cursor = after_participant.map(str::to_owned);
        for candidate in candidates {
            if participants.len() == usize::from(limits.max_participants) {
                break;
            }
            next_cursor = Some(candidate.participant_id.clone());
            if let Some(reason) = candidate.exclusion {
                excluded.push((candidate.participant_id, reason));
                continue;
            }
            participants.push(AppRoundParticipant {
                participant_id: candidate.participant_id,
                context_digest: digest(&candidate.context)?,
                context: candidate.context,
                dispatch_context: None,
                preflight_failure: candidate.preflight_failure.clone(),
                attempts_started: 0,
                settled: Vec::new(),
                state: candidate
                    .preflight_failure
                    .map_or(AppRoundParticipantState::Pending, |code| {
                        AppRoundParticipantState::Failed { code }
                    }),
            });
        }
        let mut round = Self {
            schema: SCHEMA.to_owned(),
            round_ref,
            plan_digest: String::new(),
            limits,
            participants,
            excluded,
            previous_cursor: after_participant.map(str::to_owned),
            next_cursor,
            cancelled: false,
        };
        round.plan_digest = round.expected_plan_digest()?;
        round.validate()?;
        Ok(round)
    }

    fn expected_plan_digest(&self) -> Result<String, AppRoundError> {
        digest(&(
            SCHEMA,
            &self.round_ref,
            &self.limits,
            self.participants
                .iter()
                .map(|participant| {
                    (
                        &participant.participant_id,
                        &participant.context_digest,
                        &participant.preflight_failure,
                    )
                })
                .collect::<Vec<_>>(),
            &self.excluded,
            &self.previous_cursor,
            &self.next_cursor,
        ))
    }

    fn invocation_id(&self, participant: &str, ordinal: u16) -> Result<String, AppRoundError> {
        digest(&(
            "magician.app-round-model-attempt.v1",
            &self.round_ref,
            &self.plan_digest,
            participant,
            ordinal,
        ))
    }

    fn mutation_key(&self, participant: &str) -> Result<String, AppRoundError> {
        digest(&(
            "magician.app-round-participant-commit.v1",
            &self.round_ref,
            &self.plan_digest,
            participant,
        ))
    }

    pub fn participants(&self) -> &[AppRoundParticipant] {
        &self.participants
    }

    /// Sequential consumers may refresh the next never-attempted participant.
    /// Retries and retained outputs keep the exact context they already used.
    pub fn next_context_refresh(&self) -> Result<Option<&AppRoundParticipant>, AppRoundError> {
        self.validate()?;
        if self.cancelled
            || self.limits.max_concurrent != 1
            || self.participants.iter().any(|p| {
                matches!(
                    p.state,
                    AppRoundParticipantState::InFlight { .. }
                        | AppRoundParticipantState::Prepared { .. }
                )
            })
        {
            return Ok(None);
        }
        Ok(self
            .participants
            .iter()
            .find(|p| p.state == AppRoundParticipantState::Pending && p.attempts_started == 0)
            .filter(|p| p.dispatch_context.is_none()))
    }

    pub fn refresh_pending_context(
        &mut self,
        participant_id: &str,
        value: Value,
        prepared_at_ms: i64,
        deferred: Option<String>,
    ) -> Result<(), AppRoundError> {
        let expected = self
            .next_context_refresh()?
            .ok_or(AppRoundError::Transition)?;
        if expected.participant_id != participant_id
            || !value.is_object()
            || prepared_at_ms <= 0
            || deferred.as_ref().is_some_and(|r| !identity(r))
        {
            return Err(AppRoundError::Transition);
        }
        let updated = AppRoundDispatchContext {
            digest: digest(&value)?,
            value,
            prepared_at_ms,
        };
        let participant = self
            .participants
            .iter_mut()
            .find(|p| p.participant_id == participant_id)
            .ok_or(AppRoundError::Transition)?;
        participant.dispatch_context = Some(updated);
        if let Some(reason) = deferred {
            participant.state = AppRoundParticipantState::Deferred { reason };
        }
        self.validate()
    }

    /// Reopen already persisted attempts for owner-led recovery. These are
    /// identities, not new reservations or permission to call a model again.
    pub fn active_claims(&self) -> Result<Vec<AppRoundModelClaim>, AppRoundError> {
        self.validate()?;
        Ok(self
            .participants
            .iter()
            .filter_map(|participant| {
                let AppRoundParticipantState::InFlight {
                    attempt_id,
                    reservation,
                } = &participant.state
                else {
                    return None;
                };
                Some(AppRoundModelClaim {
                    participant_id: participant.participant_id.clone(),
                    attempt_id: attempt_id.clone(),
                    context: participant.context().clone(),
                    context_digest: participant.context_digest().to_owned(),
                    maximum_usage: *reservation,
                })
            })
            .collect())
    }

    pub fn recognizes_attempt(
        &self,
        participant_id: &str,
        attempt_id: &str,
        context_digest: &str,
    ) -> bool {
        self.participants.iter().find(|participant| participant.participant_id == participant_id)
            .is_some_and(|participant| participant.context_digest() == context_digest
                && (participant.settled.iter().any(|attempt| attempt.attempt_id == attempt_id)
                    || matches!(&participant.state, AppRoundParticipantState::InFlight { attempt_id: active, .. } if active == attempt_id)))
    }
    /// Budget/cancellation can leave selected participants unattempted. Move
    /// past actual attempted work, so a short budget cannot repeatedly favor
    /// the first members of a larger selected round.
    pub fn next_cursor(&self) -> Option<&str> {
        self.participants
            .iter()
            .rev()
            .find(|participant| {
                participant.attempts_started > 0
                    || participant.preflight_failure.is_some()
                    || participant.dispatch_context.is_some()
            })
            .map(|participant| participant.participant_id.as_str())
            .or_else(|| {
                if self.participants.is_empty() {
                    self.next_cursor.as_deref()
                } else {
                    self.previous_cursor.as_deref()
                }
            })
    }
    pub fn plan_digest(&self) -> &str {
        &self.plan_digest
    }
    pub fn round_ref(&self) -> &str {
        &self.round_ref
    }
    pub fn is_complete(&self) -> bool {
        self.participants
            .iter()
            .all(|participant| participant.state.terminal())
    }

    pub fn summary(&self) -> Result<AppRoundSummary, AppRoundError> {
        let mut summary = AppRoundSummary {
            considered: self.participants.len() + self.excluded.len(),
            selected: self.participants.len(),
            excluded: self.excluded.len(),
            ..Default::default()
        };
        for participant in &self.participants {
            summary.spent = summary.spent.add(participant.spent()?)?;
            match &participant.state {
                AppRoundParticipantState::Pending => summary.awaiting_model += 1,
                AppRoundParticipantState::InFlight { reservation, .. } => {
                    summary.awaiting_model += 1;
                    summary.reserved = summary.reserved.add(*reservation)?;
                },
                AppRoundParticipantState::Prepared { .. } => summary.awaiting_commit += 1,
                AppRoundParticipantState::Committed { .. } => summary.committed += 1,
                AppRoundParticipantState::Quiet { .. } => summary.quiet += 1,
                AppRoundParticipantState::Failed { .. } => summary.failed += 1,
                AppRoundParticipantState::Deferred { .. } => summary.deferred += 1,
            }
        }
        Ok(summary)
    }

    /// Validate every recovered checkpoint against immutable plan identity,
    /// attempt ordinals and aggregate/per-participant accounting. The owner
    /// also compares this plan digest with the retained recipe/node binding.
    pub fn validate(&self) -> Result<(), AppRoundError> {
        self.limits.validate()?;
        if self.schema != SCHEMA
            || !identity(&self.round_ref)
            || self.plan_digest != self.expected_plan_digest()?
            || self.participants.len() > usize::from(self.limits.max_participants)
        {
            return Err(AppRoundError::Identity);
        }
        let mut ids = BTreeSet::new();
        let mut in_flight = 0;
        for participant in &self.participants {
            if let Some(context) = &participant.dispatch_context {
                if self.limits.max_concurrent != 1
                    || !context.value.is_object()
                    || context.digest != digest(&context.value)?
                    || context.prepared_at_ms <= 0
                {
                    return Err(AppRoundError::Identity);
                }
            }
            if let Some(code) = &participant.preflight_failure {
                if !identity(code)
                    || participant.attempts_started != 0
                    || !participant.settled.is_empty()
                    || participant.state
                        != (AppRoundParticipantState::Failed { code: code.clone() })
                {
                    return Err(AppRoundError::Identity);
                }
            }
            if !identity(&participant.participant_id)
                || !ids.insert(&participant.participant_id)
                || !participant.context.is_object()
                || digest(&participant.context)? != participant.context_digest
                || participant.attempts_started > self.limits.max_attempts_per_participant
            {
                return Err(AppRoundError::Identity);
            }
            for (index, attempt) in participant.settled.iter().enumerate() {
                let ordinal = u16::try_from(index + 1).map_err(|_| AppRoundError::Identity)?;
                if attempt.attempt_id != self.invocation_id(&participant.participant_id, ordinal)?
                    || !identity(&attempt.settlement_ref)
                {
                    return Err(AppRoundError::Identity);
                }
            }
            let mut charged = participant.spent()?;
            let active = matches!(participant.state, AppRoundParticipantState::InFlight { .. });
            if participant.settled.len() + usize::from(active)
                != usize::from(participant.attempts_started)
            {
                return Err(AppRoundError::Identity);
            }
            if matches!(
                participant.state,
                AppRoundParticipantState::Prepared { .. }
                    | AppRoundParticipantState::Committed { .. }
                    | AppRoundParticipantState::Quiet { .. }
            ) && participant.settled.is_empty()
            {
                return Err(AppRoundError::Identity);
            }
            match &participant.state {
                AppRoundParticipantState::InFlight {
                    attempt_id,
                    reservation,
                } => {
                    in_flight += 1;
                    if attempt_id
                        != &self.invocation_id(
                            &participant.participant_id,
                            participant.attempts_started,
                        )?
                    {
                        return Err(AppRoundError::Identity);
                    }
                    charged = charged.add(*reservation)?;
                },
                AppRoundParticipantState::Prepared {
                    draft,
                    mutation_key,
                } => {
                    if draft.is_null()
                        || mutation_key != &self.mutation_key(&participant.participant_id)?
                    {
                        return Err(AppRoundError::Identity);
                    }
                },
                AppRoundParticipantState::Committed {
                    record_ids,
                    receipt_refs,
                } => validate_receipts(record_ids, receipt_refs)?,
                AppRoundParticipantState::Quiet { reason }
                | AppRoundParticipantState::Deferred { reason } => {
                    if !identity(reason) {
                        return Err(AppRoundError::Identity);
                    }
                },
                AppRoundParticipantState::Failed { code } => {
                    if !identity(code) {
                        return Err(AppRoundError::Identity);
                    }
                },
                AppRoundParticipantState::Pending => {},
            }
            if !charged.fits(self.limits.per_participant) {
                return Err(AppRoundError::Budget);
            }
        }
        if in_flight > self.limits.max_concurrent {
            return Err(AppRoundError::Budget);
        }
        for (id, reason) in &self.excluded {
            if !identity(id) || !identity(reason) || !ids.insert(id) {
                return Err(AppRoundError::Identity);
            }
        }
        let summary = self.summary()?;
        if !summary
            .spent
            .add(summary.reserved)?
            .fits(self.limits.aggregate)
        {
            return Err(AppRoundError::Budget);
        }
        Ok(())
    }

    pub fn claim_next(&mut self) -> Result<Option<AppRoundModelClaim>, AppRoundError> {
        self.validate()?;
        if self.cancelled {
            return Ok(None);
        }
        let active = self
            .participants
            .iter()
            .filter(|p| matches!(p.state, AppRoundParticipantState::InFlight { .. }))
            .count();
        if active >= usize::from(self.limits.max_concurrent) {
            return Ok(None);
        }
        let summary = self.summary()?;
        let charged = summary.spent.add(summary.reserved)?;
        let mut order = (0..self.participants.len()).collect::<Vec<_>>();
        order.sort_by_key(|index| self.participants[*index].attempts_started);
        for index in order {
            let participant = &self.participants[index];
            if participant.state != AppRoundParticipantState::Pending {
                continue;
            }
            let spent = participant.spent()?;
            let maximum_usage = AppRoundUsage {
                tokens: self.limits.per_participant.tokens - spent.tokens,
                micro_usd: self.limits.per_participant.micro_usd - spent.micro_usd,
            };
            if maximum_usage.tokens == 0
                || participant.attempts_started == self.limits.max_attempts_per_participant
            {
                self.participants[index].state = AppRoundParticipantState::Failed {
                    code: "participant_budget_exhausted".to_owned(),
                };
                continue;
            }
            if !charged.add(maximum_usage)?.fits(self.limits.aggregate) {
                // Existing calls may release unused reservations. No local
                // timer turns temporary aggregate contention into failure.
                if active == 0 {
                    self.participants[index].state = AppRoundParticipantState::Deferred {
                        reason: "round_budget_exhausted".to_owned(),
                    };
                }
                continue;
            }
            let ordinal = participant.attempts_started + 1;
            let attempt_id = self.invocation_id(&participant.participant_id, ordinal)?;
            let claim = AppRoundModelClaim {
                participant_id: participant.participant_id.clone(),
                attempt_id: attempt_id.clone(),
                context: participant.context().clone(),
                context_digest: participant.context_digest().to_owned(),
                maximum_usage,
            };
            self.participants[index].attempts_started = ordinal;
            self.participants[index].state = AppRoundParticipantState::InFlight {
                attempt_id,
                reservation: maximum_usage,
            };
            return Ok(Some(claim));
        }
        Ok(None)
    }

    /// The settlement reference must come from the normal resource owner.
    /// Unknown consumption retains InFlight; recovery cannot refund it to zero.
    pub fn settle_model(
        &mut self,
        claim: &AppRoundModelClaim,
        usage: AppRoundUsage,
        settlement_ref: String,
        result: Result<AppRoundModelResult, (String, bool)>,
    ) -> Result<(), AppRoundError> {
        self.validate()?;
        if !identity(&settlement_ref) || !usage.fits(claim.maximum_usage) {
            return Err(AppRoundError::Budget);
        }
        let index = self
            .participants
            .iter()
            .position(|p| p.participant_id == claim.participant_id)
            .ok_or(AppRoundError::Transition)?;
        let participant = &self.participants[index];
        if participant.state
            != (AppRoundParticipantState::InFlight {
                attempt_id: claim.attempt_id.clone(),
                reservation: claim.maximum_usage,
            })
            || participant.context_digest() != claim.context_digest
            || participant.context() != &claim.context
        {
            return Err(AppRoundError::Transition);
        }
        let next = match result {
            Ok(AppRoundModelResult::Draft { value })
                if value.as_object().is_some_and(|value| !value.is_empty()) =>
            {
                AppRoundParticipantState::Prepared {
                    draft: value,
                    mutation_key: self.mutation_key(&claim.participant_id)?,
                }
            },
            Ok(AppRoundModelResult::Quiet { reason }) if identity(&reason) => {
                AppRoundParticipantState::Quiet { reason }
            },
            // Malformed output still consumed resources. Do not strand its
            // settlement or pretend its cost disappeared with its draft.
            Ok(_) => AppRoundParticipantState::Failed {
                code: "invalid_model_result".to_owned(),
            },
            Err((code, retryable)) if identity(&code) => {
                if retryable
                    && !self.cancelled
                    && participant.attempts_started < self.limits.max_attempts_per_participant
                {
                    AppRoundParticipantState::Pending
                } else {
                    AppRoundParticipantState::Failed { code }
                }
            },
            Err(_) => return Err(AppRoundError::Output),
        };
        let participant = &mut self.participants[index];
        participant.settled.push(SettledAttempt {
            attempt_id: claim.attempt_id.clone(),
            usage,
            settlement_ref,
        });
        participant.state = next;
        self.validate()
    }

    /// Called only after the mutation owner returned/reopened the receipt for
    /// this exact prepared mutation key. A crash before this checkpoint leaves
    /// Prepared, so recovery repeats the same idempotent commit, not drafting.
    pub fn record_commit(
        &mut self,
        participant_id: &str,
        mutation_key: &str,
        record_ids: Vec<String>,
        receipt_refs: Vec<String>,
    ) -> Result<(), AppRoundError> {
        self.validate()?;
        validate_receipts(&record_ids, &receipt_refs)?;
        if mutation_key != self.mutation_key(participant_id)? {
            return Err(AppRoundError::Transition);
        }
        let participant = self
            .participants
            .iter_mut()
            .find(|p| p.participant_id == participant_id)
            .ok_or(AppRoundError::Transition)?;
        match &participant.state {
            AppRoundParticipantState::Prepared {
                mutation_key: expected,
                ..
            } if expected == mutation_key => {},
            AppRoundParticipantState::Committed {
                record_ids: ids,
                receipt_refs: refs,
            } if ids == &record_ids && refs == &receipt_refs => return Ok(()),
            _ => return Err(AppRoundError::Transition),
        }
        participant.state = AppRoundParticipantState::Committed {
            record_ids,
            receipt_refs,
        };
        Ok(())
    }

    /// Only a proven precommit refusal can terminalize a prepared draft.
    /// Uncertain I/O leaves Prepared so the owner must reconcile its receipt.
    pub fn record_commit_refusal(
        &mut self,
        participant_id: &str,
        mutation_key: &str,
        code: String,
    ) -> Result<(), AppRoundError> {
        self.validate()?;
        if !identity(&code) || mutation_key != self.mutation_key(participant_id)? {
            return Err(AppRoundError::Transition);
        }
        let participant = self
            .participants
            .iter_mut()
            .find(|p| p.participant_id == participant_id)
            .ok_or(AppRoundError::Transition)?;
        if !matches!(&participant.state, AppRoundParticipantState::Prepared { mutation_key: expected, .. } if expected == mutation_key)
        {
            return Err(AppRoundError::Transition);
        }
        participant.state = AppRoundParticipantState::Failed { code };
        Ok(())
    }

    /// A reviewed quiet result may still have deterministic bookkeeping.
    /// Its owner settles that work before calling this; it is never a post.
    pub fn record_prepared_quiet(
        &mut self,
        participant_id: &str,
        mutation_key: &str,
        reason: String,
    ) -> Result<(), AppRoundError> {
        self.validate()?;
        if !identity(&reason) || mutation_key != self.mutation_key(participant_id)? {
            return Err(AppRoundError::Transition);
        }
        let participant = self
            .participants
            .iter_mut()
            .find(|p| p.participant_id == participant_id)
            .ok_or(AppRoundError::Transition)?;
        if !matches!(&participant.state, AppRoundParticipantState::Prepared { mutation_key: expected, .. } if expected == mutation_key)
        {
            return Err(AppRoundError::Transition);
        }
        participant.state = AppRoundParticipantState::Quiet { reason };
        self.validate()
    }

    pub fn cancel(&mut self) {
        self.cancelled = true;
        for participant in &mut self.participants {
            if participant.state == AppRoundParticipantState::Pending {
                participant.state = AppRoundParticipantState::Deferred {
                    reason: "cancelled".to_owned(),
                };
            }
        }
        // In-flight calls keep their reservations until owner settlement.
        // Prepared writes remain distinct from committed, even after cancellation.
    }
}

fn validate_receipts(records: &[String], receipts: &[String]) -> Result<(), AppRoundError> {
    if records.is_empty()
        || receipts.is_empty()
        || records.iter().chain(receipts).any(|id| !identity(id))
    {
        return Err(AppRoundError::Receipt);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn usage(tokens: u64) -> AppRoundUsage {
        AppRoundUsage {
            tokens,
            micro_usd: tokens * 10,
        }
    }
    fn limits(count: u16) -> AppRoundLimits {
        AppRoundLimits {
            max_participants: count,
            max_concurrent: 2.min(count),
            max_attempts_per_participant: 2,
            aggregate: usage(30 * u64::from(count)),
            per_participant: usage(30),
        }
    }
    fn candidates() -> Vec<AppRoundCandidate> {
        ["d", "b", "a", "c"].into_iter().map(|id| AppRoundCandidate {
            preflight_failure: None,
            participant_id: id.to_owned(),
            context: json!({"member": id, "permitted_persona": format!("Persona {id}"), "feed": ["Known context"]}),
            exclusion: None,
        }).collect()
    }
    fn round(count: u16) -> AppContextualRound {
        AppContextualRound::plan("round:test".into(), None, candidates(), limits(count)).unwrap()
    }
    fn recover(round: &AppContextualRound) -> AppContextualRound {
        let restored: AppContextualRound =
            serde_json::from_slice(&serde_json::to_vec(round).unwrap()).unwrap();
        restored.validate().unwrap();
        restored
    }
    fn quiet(round: &mut AppContextualRound, claim: &AppRoundModelClaim, spent: u64) {
        round
            .settle_model(
                claim,
                usage(spent),
                format!("settlement:{}", claim.attempt_id),
                Ok(AppRoundModelResult::Quiet {
                    reason: "No useful addition to the permitted context".into(),
                }),
            )
            .unwrap();
    }
    fn draft(round: &mut AppContextualRound, claim: &AppRoundModelClaim) -> String {
        round
            .settle_model(
                claim,
                usage(5),
                format!("settlement:{}", claim.attempt_id),
                Ok(AppRoundModelResult::Draft {
                    value: json!({"body": format!("Thought from {}", claim.participant_id)}),
                }),
            )
            .unwrap();
        match round
            .participants()
            .iter()
            .find(|p| p.participant_id() == claim.participant_id)
            .unwrap()
            .state()
        {
            AppRoundParticipantState::Prepared { mutation_key, .. } => mutation_key.clone(),
            _ => panic!("draft must remain prepared until an owner receipt exists"),
        }
    }

    #[test]
    fn progressive_context_observes_committed_predecessor_and_preserves_plan_and_claims() {
        let mut config = limits(3);
        config.max_concurrent = 1;
        let mut round =
            AppContextualRound::plan("round:conversation".into(), None, candidates(), config)
                .unwrap();
        let plan = round.plan_digest.clone();
        assert_eq!(
            round
                .next_context_refresh()
                .unwrap()
                .unwrap()
                .participant_id(),
            "a"
        );
        let first_view = json!({"member":"a", "feed":[]});
        round
            .refresh_pending_context("a", first_view.clone(), 1000, None)
            .unwrap();
        // Recovery between refresh and claim must not prefetch speaker B early.
        let mut round = recover(&round);
        assert!(round.next_context_refresh().unwrap().is_none());
        let a = round.claim_next().unwrap().unwrap();
        assert_eq!(a.context, first_view);
        assert!(round.next_context_refresh().unwrap().is_none());
        assert!(round
            .refresh_pending_context("a", json!({}), 1001, None)
            .is_err());
        let key = draft(&mut round, &a);
        assert!(round.next_context_refresh().unwrap().is_none());
        round
            .record_commit("a", &key, vec!["post:a".into()], vec!["receipt:a".into()])
            .unwrap();
        let second_view = json!({"member":"b", "feed":[{"post_id":"post:a", "body":"An actual committed question"}]});
        round
            .refresh_pending_context("b", second_view.clone(), 2000, None)
            .unwrap();
        let mut round = recover(&round);
        assert_eq!(round.plan_digest, plan);
        assert!(round.recognizes_attempt("a", &a.attempt_id, &a.context_digest));
        let b = round.claim_next().unwrap().unwrap();
        assert_eq!(b.context, second_view);
        assert_eq!(
            round.participants()[1].dispatch_prepared_at_ms(),
            Some(2000)
        );
        quiet(&mut round, &b, 5);
        assert_eq!(round.summary().unwrap().committed, 1);
        assert_eq!(round.summary().unwrap().quiet, 1);
        recover(&round);
    }

    #[test]
    fn progressive_context_deferral_spends_nothing_and_cannot_mutate_retry_context() {
        let mut config = limits(2);
        config.max_concurrent = 1;
        let mut round =
            AppContextualRound::plan("round:conversation".into(), None, candidates(), config)
                .unwrap();
        round
            .refresh_pending_context(
                "a",
                json!({"policy":"off"}),
                1000,
                Some("autonomy_off".into()),
            )
            .unwrap();
        assert_eq!(round.summary().unwrap().spent, usage(0));
        assert_eq!(
            round
                .next_context_refresh()
                .unwrap()
                .unwrap()
                .participant_id(),
            "b"
        );
        round
            .refresh_pending_context("b", json!({"feed":["real question"]}), 2000, None)
            .unwrap();
        let b = round.claim_next().unwrap().unwrap();
        round
            .settle_model(
                &b,
                usage(1),
                "receipt:failed".into(),
                Err(("interrupted".into(), true)),
            )
            .unwrap();
        let mut restored = recover(&round);
        assert!(restored.next_context_refresh().unwrap().is_none());
        assert!(restored
            .refresh_pending_context("b", json!({"feed":[]}), 3000, None)
            .is_err());
        let retry = restored.claim_next().unwrap().unwrap();
        assert_eq!(retry.context, b.context);
        assert_eq!(retry.context_digest, b.context_digest);
        assert_ne!(retry.attempt_id, b.attempt_id);
        quiet(&mut restored, &retry, 2);
        assert!(restored.is_complete());
    }

    #[test]
    fn recovered_claims_preserve_attempt_identity_without_new_reservations() {
        let mut round = round(3);
        let first = round.claim_next().unwrap().unwrap();
        let second = round.claim_next().unwrap().unwrap();
        let mut restored = recover(&round);
        assert_eq!(
            restored.active_claims().unwrap(),
            vec![first.clone(), second.clone()]
        );
        assert_eq!(restored.summary().unwrap().reserved, usage(60));
        assert!(restored.recognizes_attempt(
            &first.participant_id,
            &first.attempt_id,
            &first.context_digest
        ));
        assert!(!restored.recognizes_attempt(
            &second.participant_id,
            &first.attempt_id,
            &first.context_digest
        ));
        quiet(&mut restored, &first, 5);
        assert_eq!(restored.active_claims().unwrap(), vec![second]);
        assert!(restored.recognizes_attempt(
            &first.participant_id,
            &first.attempt_id,
            &first.context_digest
        ));
        assert_eq!(restored.summary().unwrap().spent, usage(5));
        assert_eq!(restored.summary().unwrap().reserved, usage(30));
    }

    #[test]
    fn context_failure_is_independent_and_spends_no_model_budget() {
        let mut inputs = candidates();
        inputs
            .iter_mut()
            .find(|item| item.participant_id == "b")
            .unwrap()
            .preflight_failure = Some("context_unavailable".to_owned());
        let mut round =
            AppContextualRound::plan("round:preflight".into(), None, inputs, limits(4)).unwrap();
        assert_eq!(round.summary().unwrap().failed, 1);
        assert_eq!(round.summary().unwrap().spent, usage(0));
        for expected in ["a", "c", "d"] {
            let claim = round.claim_next().unwrap().unwrap();
            assert_eq!(claim.participant_id, expected);
            quiet(&mut round, &claim, 5);
        }
        let restored = recover(&round);
        assert!(restored.is_complete());
        assert_eq!(restored.summary().unwrap().quiet, 3);
        assert_eq!(restored.summary().unwrap().spent, usage(15));
        assert_eq!(restored.next_cursor(), Some("d"));
    }

    #[test]
    fn prepared_quiet_recovery_keeps_spend_without_claiming_a_post() {
        let mut round = round(1);
        let claim = round.claim_next().unwrap().unwrap();
        let key = draft(&mut round, &claim);
        let mut restored = recover(&round);
        assert!(restored.claim_next().unwrap().is_none());
        assert_eq!(
            restored.record_prepared_quiet("a", "wrong", "nothing_useful".into()),
            Err(AppRoundError::Transition)
        );
        restored
            .record_prepared_quiet("a", &key, "nothing_useful".into())
            .unwrap();
        let restored = recover(&restored);
        assert!(restored.is_complete());
        assert_eq!(restored.summary().unwrap().quiet, 1);
        assert_eq!(restored.summary().unwrap().committed, 0);
        assert_eq!(restored.summary().unwrap().spent, usage(5));
    }

    #[test]
    fn multiple_agents_have_distinct_context_calls_and_truthful_outcomes() {
        let mut round = round(3);
        let a = round.claim_next().unwrap().unwrap();
        let b = round.claim_next().unwrap().unwrap();
        assert_ne!(a.attempt_id, b.attempt_id);
        assert_eq!(a.context["member"], "a");
        assert_eq!(b.context["member"], "b");
        assert!(round.claim_next().unwrap().is_none());
        let key = draft(&mut round, &a);
        assert_eq!(round.summary().unwrap().committed, 0);
        assert_eq!(round.summary().unwrap().awaiting_commit, 1);
        quiet(&mut round, &b, 7);
        let c = round.claim_next().unwrap().unwrap();
        round
            .settle_model(
                &c,
                usage(3),
                "settlement:failure".into(),
                Err(("provider_unavailable".into(), false)),
            )
            .unwrap();
        round
            .record_commit("a", &key, vec!["record:a".into()], vec!["receipt:a".into()])
            .unwrap();
        let summary = round.summary().unwrap();
        assert_eq!(
            (summary.committed, summary.quiet, summary.failed),
            (1, 1, 1)
        );
        assert_eq!(summary.spent, usage(15));
        assert_eq!(summary.reserved, AppRoundUsage::default());
        assert!(round.is_complete());
    }

    #[test]
    fn budget_limited_rounds_rotate_past_attempted_work_instead_of_starving_later_agents() {
        let mut cursor = None;
        let mut visited = Vec::new();
        for ordinal in 0..4 {
            let mut small = limits(4);
            small.aggregate = usage(30);
            let mut round = AppContextualRound::plan(
                format!("round:{ordinal}"),
                cursor.as_deref(),
                candidates(),
                small,
            )
            .unwrap();
            let claim = round.claim_next().unwrap().unwrap();
            quiet(&mut round, &claim, 30);
            assert!(round.claim_next().unwrap().is_none());
            assert_eq!(round.summary().unwrap().deferred, 3);
            visited.push(claim.participant_id);
            cursor = round.next_cursor().map(str::to_owned);
        }
        assert_eq!(visited, ["a", "b", "c", "d"]);
    }

    #[test]
    fn exclusions_and_cancel_before_dispatch_spend_nothing() {
        let mut candidates = candidates();
        for candidate in &mut candidates {
            candidate.exclusion = Some("opted_out".into());
        }
        let mut excluded =
            AppContextualRound::plan("round:excluded".into(), Some("b"), candidates, limits(4))
                .unwrap();
        assert!(excluded.claim_next().unwrap().is_none());
        assert!(excluded.is_complete());
        assert_eq!(excluded.summary().unwrap().excluded, 4);
        assert_eq!(excluded.summary().unwrap().spent, AppRoundUsage::default());
        assert_eq!(excluded.next_cursor(), Some("b"));
        let mut round = round(4);
        round.cancel();
        assert!(round.claim_next().unwrap().is_none());
        assert_eq!(round.next_cursor(), None);
    }

    #[test]
    fn retries_keep_spend_and_yield_to_unattempted_participants() {
        let mut round = round(2);
        let a = round.claim_next().unwrap().unwrap();
        round
            .settle_model(
                &a,
                usage(10),
                "settlement:first".into(),
                Err(("transient".into(), true)),
            )
            .unwrap();
        let mut restored = recover(&round);
        let b = restored.claim_next().unwrap().unwrap();
        assert_eq!(b.participant_id, "b");
        let retry = restored.claim_next().unwrap().unwrap();
        assert_eq!(retry.participant_id, "a");
        assert_ne!(retry.attempt_id, a.attempt_id);
        assert_eq!(retry.maximum_usage, usage(20));
        assert_eq!(restored.summary().unwrap().spent, usage(10));
        assert_eq!(restored.summary().unwrap().reserved, usage(50));
        quiet(&mut restored, &retry, 20);
        quiet(&mut restored, &b, 30);
        assert_eq!(restored.summary().unwrap().spent, usage(60));
        assert!(restored.is_complete());
    }

    #[test]
    fn unknown_in_flight_usage_survives_restart_and_cancellation() {
        let mut round = round(1);
        let claim = round.claim_next().unwrap().unwrap();
        let mut restored = recover(&round);
        restored.cancel();
        assert!(restored.claim_next().unwrap().is_none());
        assert!(!restored.is_complete());
        assert_eq!(restored.summary().unwrap().reserved, usage(30));
        quiet(&mut restored, &claim, 30);
        assert_eq!(
            restored.summary().unwrap().reserved,
            AppRoundUsage::default()
        );
        assert_eq!(restored.summary().unwrap().spent, usage(30));
    }

    #[test]
    fn prepared_recovery_reuses_commit_identity_without_redrafting_or_fabricating_posts() {
        let mut round = round(1);
        let claim = round.claim_next().unwrap().unwrap();
        let key = draft(&mut round, &claim);
        let mut restored = recover(&round);
        assert!(restored.claim_next().unwrap().is_none());
        assert_eq!(
            restored.record_commit("a", &key, vec![], vec!["receipt:a".into()]),
            Err(AppRoundError::Receipt)
        );
        assert_eq!(restored.summary().unwrap().committed, 0);
        restored
            .record_commit("a", &key, vec!["record:a".into()], vec!["receipt:a".into()])
            .unwrap();
        restored
            .record_commit("a", &key, vec!["record:a".into()], vec!["receipt:a".into()])
            .unwrap();
        assert_eq!(
            restored.record_commit(
                "a",
                "substituted",
                vec!["record:a".into()],
                vec!["receipt:a".into()]
            ),
            Err(AppRoundError::Transition)
        );
        assert_eq!(restored.summary().unwrap().committed, 1);
    }

    #[test]
    fn partial_commit_survives_sibling_failure_and_cancellation() {
        let mut round = round(3);
        let a = round.claim_next().unwrap().unwrap();
        let b = round.claim_next().unwrap().unwrap();
        let a_key = draft(&mut round, &a);
        let b_key = draft(&mut round, &b);
        round
            .record_commit(
                "a",
                &a_key,
                vec!["record:a".into()],
                vec!["receipt:a".into()],
            )
            .unwrap();
        round.cancel();
        round
            .record_commit_refusal("b", &b_key, "cancelled_before_io".into())
            .unwrap();
        let restored = recover(&round);
        let summary = restored.summary().unwrap();
        assert_eq!(
            (summary.committed, summary.failed, summary.deferred),
            (1, 1, 1)
        );
        assert_eq!(summary.spent, usage(10));
        assert_eq!(restored.next_cursor(), Some("b"));
        assert!(restored.is_complete());
    }

    #[test]
    fn malformed_semantic_output_still_settles_usage_and_is_not_quiet() {
        let mut round = round(1);
        let claim = round.claim_next().unwrap().unwrap();
        round
            .settle_model(
                &claim,
                usage(9),
                "settlement:invalid".into(),
                Ok(AppRoundModelResult::Quiet { reason: "".into() }),
            )
            .unwrap();
        let summary = round.summary().unwrap();
        assert_eq!((summary.failed, summary.quiet), (1, 0));
        assert_eq!(summary.spent, usage(9));
        assert_eq!(summary.reserved, AppRoundUsage::default());
    }

    #[test]
    fn changed_context_and_overspend_are_rejected_without_releasing_the_claim() {
        let mut round = round(1);
        let claim = round.claim_next().unwrap().unwrap();
        assert_eq!(
            round.settle_model(
                &claim,
                usage(31),
                "settlement:overspend".into(),
                Err(("failure".into(), false))
            ),
            Err(AppRoundError::Budget)
        );
        assert_eq!(round.summary().unwrap().reserved, usage(30));
        let mut forged = serde_json::to_value(&round).unwrap();
        forged["participants"][0]["context"]["member"] = json!("another-agent");
        assert_eq!(
            serde_json::from_value::<AppContextualRound>(forged)
                .unwrap()
                .validate(),
            Err(AppRoundError::Identity)
        );
        let mut duplicate = candidates();
        duplicate.push(duplicate[0].clone());
        assert!(matches!(
            AppContextualRound::plan("round:duplicate".into(), None, duplicate, limits(4)),
            Err(AppRoundError::Identity)
        ));
    }
}
