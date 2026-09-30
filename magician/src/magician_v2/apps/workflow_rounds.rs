//! Durable native rounds under the existing workflow and physical resource owners.
//! Context checkpoints contain references, not another copy of prompts or drafts.
use super::*;
use crate::magician_v2::apps::{
    contextual_round::{
        AppContextualRound, AppRoundCandidate, AppRoundLimits, AppRoundModelClaim,
        AppRoundModelResult, AppRoundParticipantState, AppRoundUsage,
    },
    resource_contract::{
        AppResourceJournalEvent, AppResourceObservationSource, AppResourceSettlementOutcome,
    },
};

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct AppNativeContextReference {
    pub checkpoint: AppToolResultCheckpoint,
    pub pointer: String,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AppNativePhysicalAttempt {
    participant_id: String,
    context_digest: String,
    reservation_id: AppReference,
    observation_id: AppReference,
    operation_key: AppReference,
    requested: AppResourceQuantity,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct AppNativeRoundState {
    node_binding_digest: AppDigest,
    pub(super) round: AppContextualRound,
    pub(super) prepared_at: DateTime<Utc>,
    pub(super) shared_context: BTreeMap<AppName, AppNativeContextReference>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    physical_attempts: BTreeMap<String, AppNativePhysicalAttempt>,
}

/// Issued only after the owning run durably enters InFlight. It identifies one
/// selected participant, while the physical resource owner still grants I/O.
#[derive(Clone)]
pub(super) struct AppNativeRoundModelClaim {
    round_ref: AppReference,
    node_binding_digest: AppDigest,
    model: AppRoundModelClaim,
}

impl AppNativeRoundModelClaim {
    pub(super) fn model(&self) -> &AppRoundModelClaim {
        &self.model
    }
    pub(super) fn node_binding_digest(&self) -> &AppDigest {
        &self.node_binding_digest
    }
}

/// A normal-dispatch completion marker, never a timer or a Drop callback.
/// The queue drains protected physical calls before returning; only a fully
/// returned call can prove that an absent reservation means no provider I/O.
#[derive(Clone)]
pub(crate) struct AppNativeModelCompletion {
    task_id: String,
    execution_id: String,
    round_ref: AppReference,
    attempt_id: String,
    finished: Arc<std::sync::atomic::AtomicBool>,
}

impl AppNativeModelCompletion {
    pub(super) fn new(task_id: &str, execution_id: &str, claim: &AppNativeRoundModelClaim) -> Self {
        Self {
            task_id: task_id.to_owned(),
            execution_id: execution_id.to_owned(),
            round_ref: claim.round_ref.clone(),
            attempt_id: claim.model.attempt_id.clone(),
            finished: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
    pub(super) fn mark_finished(&self) {
        self.finished
            .store(true, std::sync::atomic::Ordering::Release);
    }
    fn identity_matches(
        &self,
        task_id: &str,
        execution_id: &str,
        claim: &AppNativeRoundModelClaim,
    ) -> bool {
        self.task_id == task_id
            && self.execution_id == execution_id
            && self.round_ref == claim.round_ref
            && self.attempt_id == claim.model.attempt_id
    }
    fn finished(&self) -> bool {
        self.finished.load(std::sync::atomic::Ordering::Acquire)
    }
}

/// Narrows the normal physical owner. This never constructs a provider or a
/// second ledger; its only extra authority is a per-participant ceiling.
#[derive(Clone)]
pub(super) struct AppNativeRoundResourceOwner {
    base: AppWorkflowLlmResourceOwner,
    claim: AppNativeRoundModelClaim,
}

impl std::fmt::Debug for AppNativeRoundResourceOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppNativeRoundResourceOwner")
            .field("round_ref", &self.claim.round_ref)
            .field("attempt_id", &self.claim.model.attempt_id)
            .finish_non_exhaustive()
    }
}

impl AppNativeRoundResourceOwner {
    pub(super) fn new(base: AppWorkflowLlmResourceOwner, claim: AppNativeRoundModelClaim) -> Self {
        Self { base, claim }
    }
}

#[async_trait::async_trait]
impl LlmPhysicalResourceAuthorizer for AppNativeRoundResourceOwner {
    async fn reserve(
        &self,
        plan: LlmPhysicalAttemptPlan,
    ) -> Result<Box<dyn LlmPhysicalAttemptPermit>, String> {
        self.base
            .service
            .bind_native_physical_attempt(&self.base, &self.claim, &plan)
            .await
            .map_err(|_| "native participant physical resource binding refused".to_owned())?;
        // The App task guard was dropped before entering the resource root.
        // Settlement remains the existing physical owner's responsibility.
        self.base.reserve(plan).await
    }
}

pub(super) fn round_ref(binding_digest: &AppDigest) -> Result<AppReference, AppWorkflowError> {
    resource_operation_ref("native-round", binding_digest.as_str())
}

fn round_failure(_: crate::magician_v2::apps::contextual_round::AppRoundError) -> AppWorkflowError {
    AppWorkflowError::CorruptBinding
}

fn context_references(
    value: &Value,
) -> Result<BTreeMap<AppName, AppNativeContextReference>, AppWorkflowError> {
    let references: BTreeMap<AppName, AppNativeContextReference> =
        serde_json::from_value(value.clone())?;
    if references.is_empty()
        || references.len() > AppContractLimits::default().max_collection_items()
        || references.values().any(|item| {
            item.pointer.len() > 4096
                || (!item.pointer.is_empty() && !item.pointer.starts_with('/'))
        })
    {
        return Err(AppWorkflowError::ToolResultCheckpointMismatch);
    }
    Ok(references)
}

fn require_claim<'a>(
    state: &'a AppWorkflowRunState,
    claim: &AppNativeRoundModelClaim,
) -> Result<&'a AppNativeRoundState, AppWorkflowError> {
    let round = state
        .native_rounds
        .get(&claim.round_ref)
        .ok_or(AppWorkflowError::CorruptBinding)?;
    if round.node_binding_digest != claim.node_binding_digest
        || !round
            .round
            .active_claims()
            .map_err(round_failure)?
            .contains(&claim.model)
    {
        return Err(AppWorkflowError::CorruptBinding);
    }
    Ok(round)
}

pub(super) fn validate_native_rounds(
    task: &AppWorkflowTaskBinding,
    state: &AppWorkflowRunState,
) -> Result<(), AppWorkflowError> {
    if !state.native_rounds.is_empty() && task.recipe_binding.is_none() {
        return Err(AppWorkflowError::CorruptBinding);
    }
    let mut physical_reservations = BTreeSet::new();
    for (key, retained) in &state.native_rounds {
        if key != &round_ref(&retained.node_binding_digest)?
            || retained.round.round_ref() != key.as_str()
        {
            return Err(AppWorkflowError::CorruptBinding);
        }
        retained.round.validate().map_err(round_failure)?;
        for reference in retained.shared_context.values() {
            if reference.checkpoint.execution_ref() != &state.execution_id
                || !state
                    .labeled_tool_results
                    .iter()
                    .any(|record| record.checkpoint() == &reference.checkpoint)
            {
                return Err(AppWorkflowError::ToolResultCheckpointMismatch);
            }
        }
        for participant in retained.round.participants() {
            // Like reviewed behavior-step output, a prepared structured result
            // is retained once in this sealed state, with its settled spend.
            // Recovery must commit it rather than redraft it.
            if let AppRoundParticipantState::Prepared { draft, .. } = participant.state() {
                let metrics = inspect_json_bounded(draft, MAX_JSON_NODES)
                    .ok_or(AppWorkflowError::CorruptBinding)?;
                if !draft.is_object()
                    || metrics.max_depth > MAX_JSON_DEPTH
                    || exact_json_encoded_len(draft) > MAX_RUN_STATE_BYTES as usize
                {
                    return Err(AppWorkflowError::CorruptBinding);
                }
            }
            for reference in context_references(participant.context())?.values() {
                if reference.checkpoint.execution_ref() != &state.execution_id
                    || !state
                        .labeled_tool_results
                        .iter()
                        .any(|record| record.checkpoint() == &reference.checkpoint)
                {
                    return Err(AppWorkflowError::ToolResultCheckpointMismatch);
                }
            }
        }
        for (attempt_id, physical) in &retained.physical_attempts {
            if !physical_reservations.insert(&physical.reservation_id)
                || !retained.round.recognizes_attempt(
                    &physical.participant_id,
                    attempt_id,
                    &physical.context_digest,
                )
                || physical.observation_id
                    != resource_operation_ref("llm-observation", physical.reservation_id.as_str())?
                || physical.requested.cached_input_tokens > physical.requested.input_tokens
            {
                return Err(AppWorkflowError::CorruptBinding);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_runtime_concurrency_regression_receipt_matches_entity_qualified_identity() {
        let receipt = json!({"entity":"post", "record_id":"post:member-a", "revision":1});
        assert!(semantic_record_matches_receipt(
            "post:post:member-a",
            &receipt
        ));
        for wrong in [
            "post:member-a",
            "comment:post:member-a",
            "post:post:member-b",
        ] {
            assert!(!semantic_record_matches_receipt(wrong, &receipt));
        }
        assert!(!semantic_record_matches_receipt(
            "post:post:member-a",
            &json!({"record_id":"post:member-a"})
        ));
        assert!(!semantic_record_matches_receipt(
            "post:post:member-a",
            &json!({"entity":"post", "record_id":1})
        ));
    }

    fn reference(value: &str) -> AppReference {
        AppReference::parse(value).unwrap()
    }

    #[test]
    fn completion_requires_an_explicit_return_and_the_exact_attempt() {
        let digest = AppDigest::blake3(b"node");
        let round_ref = round_ref(&digest).unwrap();
        let mut round = AppContextualRound::plan(
            round_ref.to_string(),
            None,
            ["a", "b"]
                .into_iter()
                .map(|id| AppRoundCandidate {
                    preflight_failure: None,
                    participant_id: id.to_owned(),
                    context: json!({"member":id}),
                    exclusion: None,
                })
                .collect(),
            AppRoundLimits {
                max_participants: 2,
                max_concurrent: 2,
                max_attempts_per_participant: 1,
                aggregate: AppRoundUsage {
                    tokens: 200,
                    micro_usd: 200,
                },
                per_participant: AppRoundUsage {
                    tokens: 100,
                    micro_usd: 100,
                },
            },
        )
        .unwrap();
        let first = AppNativeRoundModelClaim {
            round_ref: round_ref.clone(),
            node_binding_digest: digest.clone(),
            model: round.claim_next().unwrap().unwrap(),
        };
        let second = AppNativeRoundModelClaim {
            round_ref,
            node_binding_digest: digest,
            model: round.claim_next().unwrap().unwrap(),
        };
        let completion = AppNativeModelCompletion::new("task", "execution", &first);
        let dropped_dispatch = completion.clone();
        drop(dropped_dispatch);
        assert!(
            !completion.finished(),
            "dropping work cannot prove that provider I/O drained"
        );
        assert!(completion.identity_matches("task", "execution", &first));
        assert!(!completion.identity_matches("other", "execution", &first));
        assert!(!completion.identity_matches("task", "other", &first));
        assert!(!completion.identity_matches("task", "execution", &second));
        completion.clone().mark_finished();
        assert!(completion.finished());
        assert!(!completion.identity_matches("task", "execution", &second));
    }

    fn fixture() -> (AppNativePhysicalAttempt, Vec<AppResourceJournalEvent>) {
        let physical = AppNativePhysicalAttempt {
            participant_id: "member-a".to_owned(),
            context_digest: AppDigest::blake3(b"context").to_string(),
            reservation_id: reference("reservation:model-a"),
            observation_id: reference("observation:model-a"),
            operation_key: reference("operation:model-a"),
            requested: AppResourceQuantity {
                input_tokens: 100,
                cached_input_tokens: 80,
                output_tokens: 20,
                cost_microusd: 500,
                ..Default::default()
            },
        };
        let events = vec![AppResourceJournalEvent::Reserved {
            sequence: 2,
            node_id: reference("node:root"),
            reservation_id: physical.reservation_id.clone(),
            operation_key: physical.operation_key.clone(),
            requested: physical.requested,
            capability_requests: Vec::new(),
            at_elapsed_ms: 1,
            expires_at_elapsed_ms: 60000,
        }];
        (physical, events)
    }
    fn committed(physical: &AppNativePhysicalAttempt) -> AppResourceJournalEvent {
        AppResourceJournalEvent::Settled {
            sequence: 3,
            node_id: reference("node:root"),
            reservation_id: physical.reservation_id.clone(),
            observation_id: physical.observation_id.clone(),
            outcome: AppResourceSettlementOutcome::Committed,
            observation_sources: vec![AppResourceObservationSource::LlmTaskLedger],
            actual: AppResourceQuantity {
                input_tokens: 50,
                cached_input_tokens: 20,
                output_tokens: 5,
                cost_microusd: 100,
                ..Default::default()
            },
            capability_usage: Vec::new(),
            active_intervals: Vec::new(),
            effect_binding_digest: None,
            effect_result: None,
            at_elapsed_ms: 20,
        }
    }

    #[test]
    fn physical_settlement_charges_input_once_and_uses_canonical_cost() {
        let (physical, mut events) = fixture();
        events.push(committed(&physical));
        let settled = physical_settlement(&events, &physical).unwrap().unwrap();
        assert_eq!(
            settled.usage,
            AppRoundUsage {
                tokens: 55,
                micro_usd: 100
            }
        );
        assert!(settled.model_completed);
        assert_eq!(settled.reference, physical.observation_id.to_string());
        // A duplicate observation cannot charge a participant twice.
        events.push(committed(&physical));
        assert!(physical_settlement(&events, &physical).is_err());
    }

    #[test]
    fn missing_and_uncertain_physical_settlement_are_never_refunded() {
        let (physical, mut events) = fixture();
        assert!(physical_settlement(&[], &physical).unwrap().is_none());
        assert!(physical_settlement(&events, &physical).unwrap().is_none());
        let mut observation = committed(&physical);
        if let AppResourceJournalEvent::Settled {
            outcome, actual, ..
        } = &mut observation
        {
            *outcome = AppResourceSettlementOutcome::OutcomeUncertain;
            *actual = AppResourceQuantity::default();
        }
        events.push(observation);
        assert!(physical_settlement(&events, &physical).unwrap().is_none());
    }

    #[test]
    fn substituted_route_source_and_excess_actual_usage_are_refused() {
        let (physical, events) = fixture();
        let mut changed = physical.clone();
        changed.operation_key = reference("operation:another-route");
        assert!(physical_settlement(&events, &changed).is_err());
        for case in 0..5 {
            let mut rows = events.clone();
            let mut observation = committed(&physical);
            if let AppResourceJournalEvent::Settled {
                node_id,
                observation_id,
                observation_sources,
                actual,
                ..
            } = &mut observation
            {
                match case {
                    0 => *node_id = reference("node:another-root"),
                    1 => *observation_id = reference("observation:another-member"),
                    2 => {
                        *observation_sources =
                            vec![AppResourceObservationSource::AppStoreTransaction]
                    },
                    3 => actual.cost_microusd = 501,
                    _ => actual.cached_input_tokens = 81,
                }
            }
            rows.push(observation);
            assert!(
                physical_settlement(&rows, &physical).is_err(),
                "case {case}"
            );
        }
    }

    #[test]
    fn zero_usage_requires_the_exact_pre_io_release_receipt() {
        let (physical, mut events) = fixture();
        let mut observation = committed(&physical);
        if let AppResourceJournalEvent::Settled {
            outcome,
            observation_id,
            actual,
            ..
        } = &mut observation
        {
            *outcome = AppResourceSettlementOutcome::ProvenUnspent;
            *actual = AppResourceQuantity::default();
            *observation_id =
                resource_operation_ref("llm-pre-io-observation", physical.reservation_id.as_str())
                    .unwrap();
        }
        events.push(observation);
        let settled = physical_settlement(&events, &physical).unwrap().unwrap();
        assert_eq!(settled.usage, AppRoundUsage::default());
        assert!(!settled.model_completed);
        if let AppResourceJournalEvent::Settled { actual, .. } = events.last_mut().unwrap() {
            actual.input_tokens = 1;
        }
        assert!(physical_settlement(&events, &physical).is_err());
    }
}

impl AppWorkflowService {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn begin_native_round(
        &self,
        scope: &ScopeRef,
        task: &AppWorkflowTaskBinding,
        context: &workflow_commits::AppNativeProgressContext<'_>,
        candidates: Vec<AppRoundCandidate>,
        limits: AppRoundLimits,
        after: Option<&str>,
        shared_context: BTreeMap<AppName, AppNativeContextReference>,
        prepared_at: DateTime<Utc>,
    ) -> Result<AppContextualRound, AppWorkflowError> {
        context.fence(scope, &task.task_id).await?;
        let node = context
            .run
            .plan()
            .node(context.node)
            .ok_or(AppWorkflowError::RecipeBindingUnavailable)?;
        let resources = &task.accepted_authority.effective_resources;
        if !node.permits_workflow_mutation()
            || limits.max_concurrent > node.resources().max_parallelism
            || limits.aggregate.micro_usd
                > node
                    .resources()
                    .max_cost_microusd
                    .min(resources.max_cost_microusd)
            || limits.aggregate.tokens
                > resources
                    .max_input_tokens
                    .saturating_add(resources.max_output_tokens)
        {
            return Err(AppWorkflowError::RecipeBindingUnavailable);
        }
        let key = round_ref(node.binding_digest())?;
        let guard = self.acquire_task_guard(scope, &task.task_id).await?;
        context
            .fence_under_task_guard(scope, &task.task_id, &guard)
            .await?;
        let mut state = self
            .require_run_state_unlocked(
                scope,
                task,
                context.run.execution_id(),
                task.resolved_agent_id.as_str(),
            )
            .await?;
        if let Some(existing) = state.native_rounds.get(&key) {
            return Ok(existing.round.clone());
        }
        let round = AppContextualRound::plan(key.to_string(), after, candidates, limits)
            .map_err(round_failure)?;
        state.native_rounds.insert(
            key,
            AppNativeRoundState {
                node_binding_digest: node.binding_digest().clone(),
                round: round.clone(),
                shared_context,
                prepared_at,
                physical_attempts: BTreeMap::new(),
            },
        );
        validate_native_rounds(task, &state)?;
        self.persist_run_state_unlocked(scope, &task.task_id, context.run.execution_id(), &state)
            .await?;
        Ok(round)
    }

    pub(super) async fn claim_native_round_participant(
        &self,
        scope: &ScopeRef,
        task: &AppWorkflowTaskBinding,
        context: &workflow_commits::AppNativeProgressContext<'_>,
    ) -> Result<Option<AppNativeRoundModelClaim>, AppWorkflowError> {
        context.fence(scope, &task.task_id).await?;
        let key = round_ref(context.permit.binding_digest())?;
        let guard = self.acquire_task_guard(scope, &task.task_id).await?;
        context
            .fence_under_task_guard(scope, &task.task_id, &guard)
            .await?;
        let mut state = self
            .require_run_state_unlocked(
                scope,
                task,
                context.run.execution_id(),
                task.resolved_agent_id.as_str(),
            )
            .await?;
        let retained = state
            .native_rounds
            .get_mut(&key)
            .ok_or(AppWorkflowError::CorruptBinding)?;
        let claim = retained
            .round
            .claim_next()
            .map_err(round_failure)?
            .map(|model| AppNativeRoundModelClaim {
                round_ref: key,
                node_binding_digest: retained.node_binding_digest.clone(),
                model,
            });
        // Even None can carry deterministic deferred/exhausted transitions.
        self.persist_run_state_unlocked(scope, &task.task_id, context.run.execution_id(), &state)
            .await?;
        Ok(claim)
    }

    /// Persist one permitted dispatch snapshot before claiming its first model
    /// attempt. No store read or provider call occurs under this task guard.
    pub(super) async fn refresh_native_round_participant(
        &self,
        scope: &ScopeRef,
        task: &AppWorkflowTaskBinding,
        context: &workflow_commits::AppNativeProgressContext<'_>,
        participant_id: &str,
        references: BTreeMap<AppName, AppNativeContextReference>,
        prepared_at: DateTime<Utc>,
        deferred: Option<String>,
    ) -> Result<(), AppWorkflowError> {
        context.fence(scope, &task.task_id).await?;
        let key = round_ref(context.permit.binding_digest())?;
        let guard = self.acquire_task_guard(scope, &task.task_id).await?;
        context
            .fence_under_task_guard(scope, &task.task_id, &guard)
            .await?;
        let mut state = self
            .require_run_state_unlocked(
                scope,
                task,
                context.run.execution_id(),
                task.resolved_agent_id.as_str(),
            )
            .await?;
        let retained = state
            .native_rounds
            .get_mut(&key)
            .ok_or(AppWorkflowError::CorruptBinding)?;
        retained
            .round
            .refresh_pending_context(
                participant_id,
                serde_json::to_value(references)?,
                prepared_at.timestamp_millis(),
                deferred,
            )
            .map_err(round_failure)?;
        self.persist_run_state_unlocked(scope, &task.task_id, context.run.execution_id(), &state)
            .await
    }

    pub(super) fn native_round_context(
        &self,
        state: &AppWorkflowRunState,
        claim: &AppNativeRoundModelClaim,
    ) -> Result<
        BTreeMap<AppName, workflow_model_context::AppNativeContextProjection>,
        AppWorkflowError,
    > {
        require_claim(state, claim)?;
        context_references(&claim.model.context)?
            .into_iter()
            .map(|(name, reference)| {
                let record = state
                    .labeled_tool_results
                    .iter()
                    .find(|record| record.checkpoint() == &reference.checkpoint)
                    .ok_or(AppWorkflowError::ToolResultCheckpointMismatch)?
                    .clone();
                Ok((
                    name,
                    workflow_model_context::AppNativeContextProjection {
                        record,
                        pointer: reference.pointer,
                    },
                ))
            })
            .collect()
    }

    async fn bind_native_physical_attempt(
        &self,
        base: &AppWorkflowLlmResourceOwner,
        claim: &AppNativeRoundModelClaim,
        plan: &LlmPhysicalAttemptPlan,
    ) -> Result<(), AppWorkflowError> {
        if plan.attempt_index() != 1
            || plan.task_id() != base.task_id
            || plan.root_execution_id() != base.root_execution_id
            || plan.execution_id() != base.execution_id
        {
            return Err(AppWorkflowError::CorruptBinding);
        }
        let (reservation_id, observation_id, operation_key, requested) =
            llm_attempt_resource_identity(plan)?;
        let tokens = requested
            .input_tokens
            .checked_add(requested.output_tokens)
            .ok_or(AppWorkflowError::ResourceActualExceedsReservation)?;
        if tokens > claim.model.maximum_usage.tokens
            || requested.cost_microusd > claim.model.maximum_usage.micro_usd
        {
            return Err(AppWorkflowError::ResourceActualExceedsReservation);
        }
        let task = self
            .read_execution_task_binding(&base.scope, &base.task_id, &base.execution_id)
            .await?
            .ok_or(AppWorkflowError::NotWorkflowTask)?;
        let _guard = self.acquire_task_guard(&base.scope, &base.task_id).await?;
        let mut state = self
            .require_run_state_unlocked(&base.scope, &task, &base.execution_id, &base.agent_id)
            .await?;
        let retained = require_claim(&state, claim)?;
        if retained
            .physical_attempts
            .contains_key(&claim.model.attempt_id)
            || state.native_rounds.values().any(|round| {
                round
                    .physical_attempts
                    .values()
                    .any(|physical| physical.reservation_id == reservation_id)
            })
        {
            // Queue retries/fallbacks cannot turn a logical participant attempt
            // into a second provider operation, even if they mint a new call ID.
            return Err(AppWorkflowError::CorruptBinding);
        }
        state
            .native_rounds
            .get_mut(&claim.round_ref)
            .ok_or(AppWorkflowError::CorruptBinding)?
            .physical_attempts
            .insert(
                claim.model.attempt_id.clone(),
                AppNativePhysicalAttempt {
                    participant_id: claim.model.participant_id.clone(),
                    context_digest: claim.model.context_digest.clone(),
                    reservation_id,
                    observation_id,
                    operation_key,
                    requested,
                },
            );
        self.persist_run_state_unlocked(&base.scope, &base.task_id, &base.execution_id, &state)
            .await
    }
}

/// Exact canonical evidence. Absence/uncertainty is not a zero-usage receipt.
struct AppNativeModelSettlement {
    usage: AppRoundUsage,
    reference: String,
    model_completed: bool,
}

fn physical_settlement(
    events: &[AppResourceJournalEvent],
    physical: &AppNativePhysicalAttempt,
) -> Result<Option<AppNativeModelSettlement>, AppWorkflowError> {
    let reserved = events
        .iter()
        .filter_map(|event| match event {
            AppResourceJournalEvent::Reserved {
                node_id,
                reservation_id,
                operation_key,
                requested,
                ..
            } if reservation_id == &physical.reservation_id => {
                Some((node_id, operation_key, requested))
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    let [(node_id, operation_key, requested)] = reserved.as_slice() else {
        return if reserved.is_empty() {
            Ok(None)
        } else {
            Err(AppWorkflowError::CorruptBinding)
        };
    };
    if *operation_key != &physical.operation_key || *requested != &physical.requested {
        return Err(AppWorkflowError::CorruptBinding);
    }
    let mut result = None;
    for event in events {
        let AppResourceJournalEvent::Settled {
            node_id: settled_node,
            reservation_id,
            observation_id,
            outcome,
            observation_sources,
            actual,
            ..
        } = event
        else {
            continue;
        };
        if reservation_id != &physical.reservation_id {
            continue;
        }
        if settled_node != *node_id {
            return Err(AppWorkflowError::CorruptBinding);
        }
        let accepted = match outcome {
            AppResourceSettlementOutcome::Committed => {
                observation_id == &physical.observation_id
                    && observation_sources.contains(&AppResourceObservationSource::LlmTaskLedger)
            },
            AppResourceSettlementOutcome::ProvenUnspent => {
                observation_id
                    == &resource_operation_ref(
                        "llm-pre-io-observation",
                        physical.reservation_id.as_str(),
                    )?
                    && *actual == AppResourceQuantity::default()
            },
            AppResourceSettlementOutcome::OutcomeUncertain => continue,
        };
        if !accepted
            || result.is_some()
            || actual.input_tokens > physical.requested.input_tokens
            || actual.cached_input_tokens > physical.requested.cached_input_tokens
            || actual.cached_input_tokens > actual.input_tokens
            || actual.output_tokens > physical.requested.output_tokens
            || actual.cost_microusd > physical.requested.cost_microusd
        {
            return Err(AppWorkflowError::CorruptBinding);
        }
        result = Some(AppNativeModelSettlement {
            usage: AppRoundUsage {
                tokens: actual
                    .input_tokens
                    .checked_add(actual.output_tokens)
                    .ok_or(AppWorkflowError::ResourceActualExceedsReservation)?,
                micro_usd: actual.cost_microusd,
            },
            reference: observation_id.to_string(),
            model_completed: *outcome == AppResourceSettlementOutcome::Committed,
        });
    }
    Ok(result)
}

pub(super) enum AppNativeRoundOutcome {
    Prepared { output: Value },
    Failed { code: AppName, retryable: bool },
}

impl AppWorkflowService {
    pub(super) async fn native_round_snapshot(
        &self,
        scope: &ScopeRef,
        task: &AppWorkflowTaskBinding,
        context: &workflow_commits::AppNativeProgressContext<'_>,
    ) -> Result<(AppContextualRound, Vec<AppNativeRoundModelClaim>), AppWorkflowError> {
        let state = self
            .require_run_state(
                scope,
                task,
                context.run.execution_id(),
                task.resolved_agent_id.as_str(),
            )
            .await?;
        let key = round_ref(context.permit.binding_digest())?;
        let retained = state
            .native_rounds
            .get(&key)
            .ok_or(AppWorkflowError::CorruptBinding)?;
        let claims = retained
            .round
            .active_claims()
            .map_err(round_failure)?
            .into_iter()
            .map(|model| AppNativeRoundModelClaim {
                round_ref: key.clone(),
                node_binding_digest: retained.node_binding_digest.clone(),
                model,
            })
            .collect();
        Ok((retained.round.clone(), claims))
    }

    /// Records accounting and receipt metadata only. This may settle accepted
    /// work after cancellation/revocation; it cannot dispatch or commit anew.
    pub(super) async fn settle_native_round_participant(
        &self,
        scope: &ScopeRef,
        task: &AppWorkflowTaskBinding,
        context: &workflow_commits::AppNativeProgressContext<'_>,
        claim: &AppNativeRoundModelClaim,
        outcome: AppNativeRoundOutcome,
        completion: Option<&AppNativeModelCompletion>,
    ) -> Result<(), AppWorkflowError> {
        if claim.node_binding_digest != *context.permit.binding_digest() {
            return Err(AppWorkflowError::CorruptBinding);
        }
        if completion.is_some_and(|proof| {
            !proof.identity_matches(&task.task_id, context.run.execution_id(), claim)
        }) {
            return Err(AppWorkflowError::CorruptBinding);
        }
        let completed_before_io = completion.is_some_and(AppNativeModelCompletion::finished)
            && matches!(&outcome, AppNativeRoundOutcome::Failed { .. });
        let guard = self.acquire_task_guard(scope, &task.task_id).await?;
        let (mut state, _) = self
            .require_run_state_for_accepted_cleanup_unlocked(
                scope,
                task,
                context.run.execution_id(),
                task.resolved_agent_id.as_str(),
            )
            .await?;
        if let AppNativeRoundOutcome::Prepared { output } = &outcome {
            let binding = context
                .run
                .plan()
                .node(context.node)
                .ok_or(AppWorkflowError::CorruptBinding)?;
            let AppRecipeNodeKind::ContextualRound { program, .. } = binding.operation() else {
                return Err(AppWorkflowError::RecipeBindingUnavailable);
            };
            let authenticated = task_execution_scope(
                scope,
                &task.accepted_authority.scope_binding_ref,
                context.run.execution_id(),
                Utc::now(),
            )?;
            let material = self
                .immutable_workflow_material(&authenticated, task, Utc::now())
                .await?;
            workflow_model_context::native_semantic_step(
                task,
                material.staged.candidate().manifest().manifest(),
                &AppName::parse(&program.semantic_step)?,
                program.max_output_tokens,
            )?
            .output_schema()
            .validate_value(output)
            .map_err(|_| AppWorkflowError::BackgroundBehaviorStepOutputRejected)?;
        }
        let retained = require_claim(&state, claim)?;
        let no_io = || -> Result<AppNativeModelSettlement, AppWorkflowError> {
            if !completed_before_io {
                return Err(AppWorkflowError::EffectSettlementUnresolved);
            }
            Ok(AppNativeModelSettlement {
                usage: AppRoundUsage::default(),
                model_completed: false,
                reference: resource_operation_ref(
                    "native-model-no-io",
                    &format!(
                        "{}:{}:{}",
                        task.task_id,
                        context.run.execution_id(),
                        claim.model.attempt_id
                    ),
                )?
                .to_string(),
            })
        };
        let settlement = if let Some(physical) =
            retained.physical_attempts.get(&claim.model.attempt_id)
        {
            let now = Utc::now();
            let authenticated = task_execution_scope(
                scope,
                &task.accepted_authority.scope_binding_ref,
                state.run_binding.execution_id.as_str(),
                now,
            )?;
            let journal = self
                .resource_runtime
                .as_ref()
                .ok_or(AppWorkflowError::ResourceRuntimeUnavailable)?
                .coordinator
                .accepted_journal(&authenticated, &state.run_binding, now)
                .await?;
            match physical_settlement(&journal.events, physical)? {
                Some(settled) => settled,
                None if !journal.events.iter().any(|event| matches!(event,
                    AppResourceJournalEvent::Reserved { reservation_id, .. } if reservation_id == &physical.reservation_id)) => no_io()?,
                None => return Err(AppWorkflowError::EffectSettlementUnresolved),
            }
        } else {
            no_io()?
        };
        if !settlement.model_completed && !matches!(outcome, AppNativeRoundOutcome::Failed { .. }) {
            return Err(AppWorkflowError::CorruptBinding);
        }
        let model_result = match outcome {
            AppNativeRoundOutcome::Prepared { output } => {
                Ok(AppRoundModelResult::Draft { value: output })
            },
            AppNativeRoundOutcome::Failed { code, retryable } => Err((code.to_string(), retryable)),
        };
        let retained = state
            .native_rounds
            .get_mut(&claim.round_ref)
            .ok_or(AppWorkflowError::CorruptBinding)?;
        retained
            .round
            .settle_model(
                &claim.model,
                settlement.usage,
                settlement.reference,
                model_result,
            )
            .map_err(round_failure)?;
        // Structured output and actual spend enter the same sealed checkpoint.
        // A later commit uses this exact result, never another model call.
        validate_native_rounds(task, &state)?;
        self.persist_run_state_unlocked(scope, &task.task_id, context.run.execution_id(), &state)
            .await?;
        drop(guard);
        // A durably settled participant advances the finite round even when
        // preparation/dispatch fails before physical I/O. No reservation
        // settlement exists to advance the resource owner's progress clock in
        // that case. Report only this completed transition, never claim/retry
        // activity or a timer heartbeat. Cleanup remains valid after revocation.
        if !settlement.model_completed {
            if let Err(error) = self
                .observe_resource_progress(
                    scope,
                    task,
                    &state,
                    context.run.execution_id(),
                    &format!("native-participant-settled:{}", claim.model.attempt_id),
                    Utc::now(),
                )
                .await
            {
                tracing::warn!(task_id = %task.task_id, %error, "native participant settled; live progress observation refused");
            }
        }
        Ok(())
    }

    pub(super) async fn cancel_native_round(
        &self,
        scope: &ScopeRef,
        task: &AppWorkflowTaskBinding,
        context: &workflow_commits::AppNativeProgressContext<'_>,
    ) -> Result<(), AppWorkflowError> {
        let _guard = self.acquire_task_guard(scope, &task.task_id).await?;
        let (mut state, _) = self
            .require_run_state_for_accepted_cleanup_unlocked(
                scope,
                task,
                context.run.execution_id(),
                task.resolved_agent_id.as_str(),
            )
            .await?;
        let key = round_ref(context.permit.binding_digest())?;
        state
            .native_rounds
            .get_mut(&key)
            .ok_or(AppWorkflowError::CorruptBinding)?
            .round
            .cancel();
        self.persist_run_state_unlocked(scope, &task.task_id, context.run.execution_id(), &state)
            .await
    }
}

pub(super) fn participant_reference(
    binding_digest: &AppDigest,
    participant: &str,
) -> Result<AppReference, AppWorkflowError> {
    let digest = AppDigest::blake3_canonical_json(&json!({
        "round": round_ref(binding_digest)?, "participant": participant,
    }))?;
    resource_operation_ref("native-round-participant", digest.as_str())
}

impl AppWorkflowService {
    pub(super) async fn finish_native_round_participant(
        &self,
        scope: &ScopeRef,
        task: &AppWorkflowTaskBinding,
        context: &workflow_commits::AppNativeProgressContext<'_>,
        participant_id: &str,
        semantic_record_ids: Vec<String>,
        quiet_reason: Option<String>,
        committed_result: Option<&AppActionResult<Value>>,
    ) -> Result<(), AppWorkflowError> {
        let _guard = self.acquire_task_guard(scope, &task.task_id).await?;
        let (mut state, _) = self
            .require_run_state_for_accepted_cleanup_unlocked(
                scope,
                task,
                context.run.execution_id(),
                task.resolved_agent_id.as_str(),
            )
            .await?;
        let node = context
            .run
            .plan()
            .node(context.node)
            .ok_or(AppWorkflowError::CorruptBinding)?;
        let key = workflow_commits::AppNativeCommitKey::for_participant(
            node,
            participant_reference(node.binding_digest(), participant_id)?,
        )?;
        let stored =
            workflow_commits::AppWorkflowCommitTarget::native(&key).retained_result(&state)?;
        if stored != committed_result {
            return Err(AppWorkflowError::CommitIntentConflict);
        }
        let receipt_refs = if let Some(result) = committed_result {
            let records = result
                .output
                .as_ref()
                .and_then(|output| output.value.get("committed_record_revisions"))
                .and_then(Value::as_array)
                .ok_or(AppWorkflowError::CorruptBinding)?;
            if semantic_record_ids.iter().any(|id| {
                !records
                    .iter()
                    .any(|row| semantic_record_matches_receipt(id, row))
            }) {
                return Err(AppWorkflowError::CommitIntentConflict);
            }
            result
                .mutation_receipt_refs
                .iter()
                .map(ToString::to_string)
                .collect()
        } else {
            Vec::new()
        };
        let retained = state
            .native_rounds
            .get_mut(&round_ref(node.binding_digest())?)
            .ok_or(AppWorkflowError::CorruptBinding)?;
        let mutation_key = retained
            .round
            .participants()
            .iter()
            .find(|participant| participant.participant_id() == participant_id)
            .and_then(|participant| match participant.state() {
                AppRoundParticipantState::Prepared { mutation_key, .. } => {
                    Some(mutation_key.clone())
                },
                _ => None,
            })
            .ok_or(AppWorkflowError::CorruptBinding)?;
        if let Some(reason) = quiet_reason {
            if !semantic_record_ids.is_empty() {
                return Err(AppWorkflowError::CorruptBinding);
            }
            retained
                .round
                .record_prepared_quiet(participant_id, &mutation_key, reason)
                .map_err(round_failure)?;
        } else {
            retained
                .round
                .record_commit(
                    participant_id,
                    &mutation_key,
                    semantic_record_ids,
                    receipt_refs,
                )
                .map_err(round_failure)?;
        }
        validate_native_rounds(task, &state)?;
        self.persist_run_state_unlocked(scope, &task.task_id, context.run.execution_id(), &state)
            .await
    }
}

// Round results retain entity-qualified identities. Matching only record_id
// both rejects valid results and would let another entity's same-named row
// stand in for the intended semantic write.
fn semantic_record_matches_receipt(identity: &str, row: &Value) -> bool {
    let (Some(entity), Some(record_id)) = (
        row.get("entity").and_then(Value::as_str),
        row.get("record_id").and_then(Value::as_str),
    ) else {
        return false;
    };
    identity
        .strip_prefix(entity)
        .and_then(|suffix| suffix.strip_prefix(':'))
        == Some(record_id)
}

/// Content-free completion evidence minted only from fully settled rounds.
/// It allows a legitimate quiet round to finish without an invented row write.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct AppNativeRoundCompletion {
    pub(super) rounds_digest: AppDigest,
    pub(super) summary: String,
}

pub(super) fn completion_evidence(
    state: &AppWorkflowRunState,
) -> Result<Option<AppNativeRoundCompletion>, AppWorkflowError> {
    if state.native_rounds.is_empty() {
        return Ok(None);
    }
    let mut evidence = Vec::new();
    let mut summary = String::new();
    let mut omitted = 0;
    for (key, retained) in &state.native_rounds {
        retained.round.validate().map_err(round_failure)?;
        if !retained.round.is_complete() {
            return Err(AppWorkflowError::EffectSettlementUnresolved);
        }
        let counts = retained.round.summary().map_err(round_failure)?;
        summary.push_str(&format!(
            "Round: {} considered; {} posted; {} quiet; {} failed; {} deferred; {} excluded.\n",
            counts.considered,
            counts.committed,
            counts.quiet,
            counts.failed,
            counts.deferred,
            counts.excluded
        ));
        let mut outcomes = Vec::new();
        for participant in retained.round.participants() {
            let spent = participant.spent().map_err(round_failure)?;
            let outcome = match participant.state() {
                AppRoundParticipantState::Committed { .. } => "posted".to_owned(),
                AppRoundParticipantState::Quiet { reason } => format!("quiet ({reason})"),
                AppRoundParticipantState::Failed { code } => format!("failed ({code})"),
                AppRoundParticipantState::Deferred { reason } => format!("deferred ({reason})"),
                _ => return Err(AppWorkflowError::EffectSettlementUnresolved),
            };
            let line = format!(
                "{}: {outcome}; {} tokens, {} micro-USD.\n",
                participant.participant_id(),
                spent.tokens,
                spent.micro_usd
            );
            if summary.len() + line.len() <= 3900 {
                summary.push_str(&line);
            } else {
                omitted += 1;
            }
            outcomes.push(json!({"participant":participant.participant_id(), "state":participant.state(), "spent":spent}));
        }
        evidence.push(
            json!({"round_ref":key,"node_binding":retained.node_binding_digest,
            "plan_digest":retained.round.plan_digest(),"summary":counts,"outcomes":outcomes,
            "next_cursor":retained.round.next_cursor()}),
        );
    }
    if omitted > 0 {
        summary.push_str(&format!(
            "{omitted} additional outcomes are retained in the run.\n"
        ));
    }
    if summary.len() > 4096 {
        return Err(AppWorkflowError::TerminalPayloadTooLarge);
    }
    Ok(Some(AppNativeRoundCompletion {
        rounds_digest: AppDigest::blake3_canonical_json(&json!(evidence))?,
        summary,
    }))
}

impl AppWorkflowService {
    pub(super) async fn fail_prepared_native_participant(
        &self,
        scope: &ScopeRef,
        task: &AppWorkflowTaskBinding,
        context: &workflow_commits::AppNativeProgressContext<'_>,
        participant_id: &str,
        code: AppName,
    ) -> Result<(), AppWorkflowError> {
        let _guard = self.acquire_task_guard(scope, &task.task_id).await?;
        let (mut state, _) = self
            .require_run_state_for_accepted_cleanup_unlocked(
                scope,
                task,
                context.run.execution_id(),
                task.resolved_agent_id.as_str(),
            )
            .await?;
        let node = context
            .run
            .plan()
            .node(context.node)
            .ok_or(AppWorkflowError::CorruptBinding)?;
        let key = workflow_commits::AppNativeCommitKey::for_participant(
            node,
            participant_reference(node.binding_digest(), participant_id)?,
        )?;
        let target = workflow_commits::AppWorkflowCommitTarget::native(&key);
        // Only a failure before the mutation owner retained an intent is known
        // not to have changed records. Any retained work must recover its receipt.
        if target.retained_intent(&state)?.is_some() || target.retained_result(&state)?.is_some() {
            return Err(AppWorkflowError::EffectSettlementUnresolved);
        }
        let retained = state
            .native_rounds
            .get_mut(&round_ref(node.binding_digest())?)
            .ok_or(AppWorkflowError::CorruptBinding)?;
        let mutation_key = retained
            .round
            .participants()
            .iter()
            .find(|participant| participant.participant_id() == participant_id)
            .and_then(|participant| match participant.state() {
                AppRoundParticipantState::Prepared { mutation_key, .. } => {
                    Some(mutation_key.clone())
                },
                _ => None,
            })
            .ok_or(AppWorkflowError::CorruptBinding)?;
        retained
            .round
            .record_commit_refusal(participant_id, &mutation_key, code.to_string())
            .map_err(round_failure)?;
        self.persist_run_state_unlocked(scope, &task.task_id, context.run.execution_id(), &state)
            .await
    }
}
