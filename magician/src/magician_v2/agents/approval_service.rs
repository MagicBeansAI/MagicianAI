//! Phase 3 durable approval service.

use std::{collections::HashMap, sync::Arc};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::magician_v2::execution::{
    actions::{ExecutableAction, FileAction},
    agentic::AgenticPauseState,
};

use super::{
    approval::{ApprovalGate, ApprovalResult, ExecutionPlan, ExecutionStep, PendingApproval},
    approval_store::{
        ApprovalDelivery, ApprovalRequest, ApprovalStatus, ApprovalStore, ApprovalStoreError,
        DeliveryStatus,
    },
    state_machine::{
        StateMachineAction, StateMachineDefinition, StateMachineError, StateMachineGuard,
        StateMachineInterpreter, StateMachineTransition, StateMachineTransitionResolution,
        StateMachineTrigger, TransitionContext,
    },
    storage::{AgentStorage, AgentStorageError},
    types::{AgentConstraints, ChannelConfig},
};

const DEFAULT_APPROVAL_TTL_SECS: u64 = 86_400;
const VALIDATING_RECOVERY_GRACE_SECS: i64 = 30;
const MAX_RESOLVE_ATTEMPTS: usize = 4;
const APPROVAL_MACHINE_NAME: &str = "approval";
const TRIGGER_DECISION_APPROVE: &str = "decision_approve";
const TRIGGER_DECISION_REJECT: &str = "decision_reject";
const TRIGGER_DELIVERY_FAILED: &str = "delivery_failed";
const TRIGGER_TTL_ELAPSED: &str = "ttl_elapsed";
const GUARD_PLAN_HASH_VALID: &str = "plan_hash_valid";
const GUARD_PLAN_HASH_INVALID: &str = "plan_hash_invalid";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approve,
    Reject,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FanOutResult {
    Delivered,
    AllChannelsFailed,
}

#[derive(Debug, Clone)]
pub struct MaybeCreateForConfirmationPauseOutcome {
    pub request: Option<ApprovalRequest>,
    pub created_new: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalResolveOutcome {
    Resolved,
    Noop,
    Expired { transitioned: bool },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ApprovalServiceError {
    #[error("approval `{0}` not found")]
    NotFound(String),
    #[error("approval `{0}` is expired")]
    Expired(String),
    #[error(
        "approval `{approval_id}` plan drift detected (expected `{expected_hash}`, got `{actual_hash}`)"
    )]
    PlanDrift {
        approval_id: String,
        expected_hash: String,
        actual_hash: String,
    },
    #[error("invalid approval request: {0}")]
    InvalidRequest(String),
    #[error("approval state machine error: {0}")]
    StateMachine(String),
    #[error("approval storage error: {0}")]
    Storage(String),
}

impl From<ApprovalStoreError> for ApprovalServiceError {
    fn from(value: ApprovalStoreError) -> Self {
        match value {
            ApprovalStoreError::NotFound(approval_id) => Self::NotFound(approval_id),
            ApprovalStoreError::Storage { details, .. } => Self::Storage(details),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ApprovalService {
    store: Arc<ApprovalStore>,
    state_machine: Arc<StateMachineInterpreter>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ChannelConfigFile {
    Wrapped { channels: Vec<ChannelConfig> },
    Bare(Vec<ChannelConfig>),
}

impl ApprovalService {
    pub fn with_storage(storage: AgentStorage) -> Result<Self, ApprovalServiceError> {
        Self::with_store(Arc::new(ApprovalStore::new(storage)))
    }

    pub fn with_store(store: Arc<ApprovalStore>) -> Result<Self, ApprovalServiceError> {
        let state_machine = build_approval_state_machine(store.as_ref())?;
        Ok(Self {
            state_machine: Arc::new(state_machine),
            store,
        })
    }

    pub fn store(&self) -> Arc<ApprovalStore> {
        self.store.clone()
    }

    fn resolve_transition(
        &self,
        expected_status: &ApprovalStatus,
        trigger: &str,
        context: &TransitionContext,
    ) -> Result<StateMachineTransitionResolution, ApprovalServiceError> {
        self.state_machine
            .transition(
                APPROVAL_MACHINE_NAME,
                approval_status_state_name(expected_status),
                trigger,
                context,
            )
            .map_err(|err| ApprovalServiceError::StateMachine(err.to_string()))
    }

    async fn apply_transition(
        &self,
        approval_id: &str,
        expected_status: ApprovalStatus,
        transition: &StateMachineTransitionResolution,
        resolved_by: Option<&str>,
        resolved_at: Option<DateTime<Utc>>,
    ) -> Result<bool, ApprovalServiceError> {
        if !transition_actions_supported_for_approval(&transition.actions) {
            return Err(ApprovalServiceError::StateMachine(format!(
                "transition `{}` -> `{}` must include `persist_state` before any side effects and use only approval-supported actions",
                transition.from, transition.to
            )));
        }

        let target_status = approval_status_from_state_name(&transition.to)
            .map_err(|err| ApprovalServiceError::StateMachine(err.to_string()))?;
        let transitioned = self
            .store
            .compare_and_set_status(
                approval_id,
                expected_status,
                target_status,
                resolved_by,
                resolved_at,
            )
            .await?;
        if !transitioned {
            return Ok(false);
        }

        for action in &transition.actions {
            match action {
                StateMachineAction::PersistState => {},
                StateMachineAction::DismissOtherDeliveries => {
                    let channel = resolved_by
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .unwrap_or("system:state_machine");
                    self.store
                        .mark_other_deliveries_dismissed(approval_id, channel)
                        .await?;
                },
                StateMachineAction::EmitEvent
                | StateMachineAction::ResumeCycle
                | StateMachineAction::FailEpisode { .. } => unreachable!(
                    "unsupported approval actions are validated before compare-and-set"
                ),
            }
        }

        Ok(true)
    }

    async fn reconcile_terminal_delivery_state(
        &self,
        approval_id: &str,
        request: &ApprovalRequest,
        fallback_channel: Option<&str>,
    ) -> Result<(), ApprovalServiceError> {
        if !matches!(
            request.status,
            ApprovalStatus::Approved | ApprovalStatus::Rejected
        ) {
            return Ok(());
        }

        let channel = request
            .resolved_by
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .or_else(|| {
                fallback_channel
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
            });
        if let Some(channel) = channel {
            self.store
                .mark_other_deliveries_dismissed(approval_id, channel)
                .await?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_request(
        &self,
        agent_id: &str,
        goal_id: &str,
        trigger_seq: u64,
        cycle_id: &str,
        pending_actions: Vec<PendingApproval>,
        ttl_secs: u64,
        plan_hash: String,
    ) -> Result<ApprovalRequest, ApprovalServiceError> {
        validate_request_inputs(
            agent_id,
            goal_id,
            trigger_seq,
            cycle_id,
            &pending_actions,
            ttl_secs,
            &plan_hash,
        )?;

        let created_at = Utc::now();
        let request = ApprovalRequest {
            approval_id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent_id.to_string(),
            goal_id: goal_id.to_string(),
            trigger_seq,
            cycle_id: cycle_id.to_string(),
            execution_id: None,
            pending_actions,
            plan_hash,
            status: ApprovalStatus::Pending,
            created_at,
            expires_at: compute_expires_at(created_at, ttl_secs),
            resolved_by: None,
            resolved_at: None,
            // No pause/execution scope at this lower-level entry point; the
            // resolve emit falls back to the resolver's scope for these.
            principal: None,
            workspace: None,
        };
        self.store.save_request(&request).await?;
        Ok(request)
    }

    /// Creates/fans out approval for a confirmation pause when an approval rule matched.
    /// Returns `Ok(None)` for non-approval confirmations.
    pub async fn maybe_create_for_confirmation_pause(
        &self,
        pause_state: &AgenticPauseState,
        action_summary: &str,
        _reason: &str,
        action_json: &str,
    ) -> Result<Option<ApprovalRequest>, ApprovalServiceError> {
        let outcome = self
            .maybe_create_for_confirmation_pause_with_outcome(
                pause_state,
                action_summary,
                _reason,
                action_json,
            )
            .await?;
        Ok(outcome.request)
    }

    /// Creates/fans out approval for a confirmation pause and reports if this call created
    /// the durable request (`created_new = true`) or returned an existing deduped request.
    pub async fn maybe_create_for_confirmation_pause_with_outcome(
        &self,
        pause_state: &AgenticPauseState,
        action_summary: &str,
        _reason: &str,
        action_json: &str,
    ) -> Result<MaybeCreateForConfirmationPauseOutcome, ApprovalServiceError> {
        let (Some(agent_id), Some(goal_id), Some(cycle_id)) = (
            pause_state.agent_id.as_deref(),
            pause_state.goal_id.as_deref(),
            pause_state.cycle_id.as_deref(),
        ) else {
            return Ok(MaybeCreateForConfirmationPauseOutcome {
                request: None,
                created_new: false,
            });
        };

        if pause_state.approval_rules.is_empty() {
            return Ok(MaybeCreateForConfirmationPauseOutcome {
                request: None,
                created_new: false,
            });
        }

        let pending_actions =
            pending_approvals_for_action_json(pause_state, action_json, action_summary);
        if pending_actions.is_empty() {
            return Ok(MaybeCreateForConfirmationPauseOutcome {
                request: None,
                created_new: false,
            });
        }

        let now = Utc::now();
        let plan_hash = Self::hash_plan_payload(action_json);
        let ttl_secs = pending_actions
            .iter()
            .filter_map(|pending| pending.matched_rule.ttl_secs)
            .min()
            .unwrap_or(DEFAULT_APPROVAL_TTL_SECS);

        let trigger_seq = trigger_seq_from_cycle_id(cycle_id);
        validate_request_inputs(
            agent_id,
            goal_id,
            trigger_seq,
            cycle_id,
            &pending_actions,
            ttl_secs,
            &plan_hash,
        )?;

        let request_candidate = ApprovalRequest {
            approval_id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent_id.to_string(),
            goal_id: goal_id.to_string(),
            trigger_seq,
            cycle_id: cycle_id.to_string(),
            execution_id: pause_state.execution_id.clone(),
            pending_actions,
            plan_hash,
            status: ApprovalStatus::Pending,
            created_at: now,
            expires_at: compute_expires_at(now, ttl_secs),
            resolved_by: None,
            resolved_at: None,
            // Capture the originating scope so the resolution event can be
            // announced to the same surface(s) that received the request.
            principal: pause_state.principal.clone(),
            workspace: pause_state.workspace.clone(),
        };
        let (request, created_new) = self
            .store
            .save_request_if_no_live_pending_match(&request_candidate, now)
            .await?;
        if !created_new {
            return Ok(MaybeCreateForConfirmationPauseOutcome {
                request: Some(request),
                created_new: false,
            });
        }

        let fan_out = self.fan_out(&request).await?;
        if fan_out == FanOutResult::AllChannelsFailed {
            warn!(
                approval_id = %request.approval_id,
                agent_id = %request.agent_id,
                "all approval channels failed; request auto-rejected"
            );
        }
        Ok(MaybeCreateForConfirmationPauseOutcome {
            request: Some(request),
            created_new: true,
        })
    }

    pub async fn get_request(
        &self,
        approval_id: &str,
    ) -> Result<Option<ApprovalRequest>, ApprovalServiceError> {
        let (request, _) = self.get_request_with_transition(approval_id).await?;
        Ok(request)
    }

    pub async fn get_request_with_transition(
        &self,
        approval_id: &str,
    ) -> Result<(Option<ApprovalRequest>, bool), ApprovalServiceError> {
        let mut request = match self.store.get_request(approval_id).await? {
            Some(request) => request,
            None => return Ok((None, false)),
        };
        let now = Utc::now();
        if matches!(
            request.status,
            ApprovalStatus::Pending | ApprovalStatus::Validating
        ) && request.expires_at <= now
        {
            let expected_status = request.status.clone();
            let transition = self.resolve_transition(
                &expected_status,
                TRIGGER_TTL_ELAPSED,
                &TransitionContext::default(),
            )?;
            let transitioned = self
                .apply_transition(
                    approval_id,
                    expected_status,
                    &transition,
                    Some("system:expiry"),
                    Some(now),
                )
                .await?;
            if transitioned {
                request.status = approval_status_from_state_name(&transition.to)
                    .map_err(|err| ApprovalServiceError::StateMachine(err.to_string()))?;
                request.resolved_by = Some("system:expiry".to_string());
                request.resolved_at = Some(now);
                return Ok((Some(request), true));
            }
            let latest = self.store.get_request(approval_id).await?;
            return Ok((latest, false));
        }
        Ok((Some(request), false))
    }

    pub async fn list_requests(&self) -> Result<Vec<ApprovalRequest>, ApprovalServiceError> {
        self.list_requests_raw().await
    }

    pub async fn list_requests_raw(&self) -> Result<Vec<ApprovalRequest>, ApprovalServiceError> {
        self.store.list_requests().await.map_err(Into::into)
    }

    pub async fn list_pending(&self) -> Result<Vec<ApprovalRequest>, ApprovalServiceError> {
        let now = Utc::now();
        let _ = self.expire_pending(now).await?;
        let mut out = self
            .store
            .list_requests()
            .await?
            .into_iter()
            .filter(|request| request.status == ApprovalStatus::Pending && request.expires_at > now)
            .collect::<Vec<_>>();
        out.sort_by_key(|request| request.created_at);
        Ok(out)
    }

    pub async fn list_deliveries(
        &self,
        approval_id: &str,
    ) -> Result<Vec<ApprovalDelivery>, ApprovalServiceError> {
        self.store
            .load_deliveries(approval_id)
            .await
            .map_err(Into::into)
    }

    pub async fn fan_out(
        &self,
        request: &ApprovalRequest,
    ) -> Result<FanOutResult, ApprovalServiceError> {
        let channels = self.load_channel_configs().await;
        let mut any_succeeded = false;

        for channel in channels {
            let mut delivery = ApprovalDelivery::sent(&request.approval_id, &channel.name);
            if !channel_delivery_supported(&channel) {
                delivery.status = DeliveryStatus::Failed;
            } else {
                any_succeeded = true;
            }
            self.store.save_delivery(&delivery).await?;
        }

        if !any_succeeded {
            let transition = self.resolve_transition(
                &ApprovalStatus::Pending,
                TRIGGER_DELIVERY_FAILED,
                &TransitionContext::default(),
            )?;
            let _ = self
                .apply_transition(
                    &request.approval_id,
                    ApprovalStatus::Pending,
                    &transition,
                    Some("system:delivery_failure"),
                    Some(Utc::now()),
                )
                .await?;
            return Ok(FanOutResult::AllChannelsFailed);
        }

        Ok(FanOutResult::Delivered)
    }

    /// Finds the latest approval for an exact `(agent_id, goal_id, cycle_id, plan_hash)` tuple.
    ///
    /// Used to reconcile confirmation-driven resumes with durable approval state.
    pub async fn find_latest_request_for_cycle_plan(
        &self,
        agent_id: &str,
        goal_id: &str,
        cycle_id: &str,
        plan_hash: &str,
    ) -> Result<Option<ApprovalRequest>, ApprovalServiceError> {
        let agent_id = agent_id.trim();
        let goal_id = goal_id.trim();
        let cycle_id = cycle_id.trim();
        let plan_hash = plan_hash.trim();
        if agent_id.is_empty() || goal_id.is_empty() || cycle_id.is_empty() || plan_hash.is_empty()
        {
            return Ok(None);
        }

        let requests = self.list_requests().await?;
        Ok(requests.into_iter().rev().find(|request| {
            request.agent_id == agent_id
                && request.goal_id == goal_id
                && request.cycle_id == cycle_id
                && request.plan_hash == plan_hash
        }))
    }

    /// First-writer-wins CAS resolution.
    ///
    /// Returns:
    /// - `Ok(true)` when this call resolved the approval.
    /// - `Ok(false)` when another writer already resolved/claimed it.
    pub async fn resolve(
        &self,
        approval_id: &str,
        decision: ApprovalDecision,
        resolved_by: &str,
        current_plan_hash: Option<&str>,
    ) -> Result<bool, ApprovalServiceError> {
        match self
            .resolve_with_outcome(approval_id, decision, resolved_by, current_plan_hash)
            .await?
        {
            ApprovalResolveOutcome::Resolved => Ok(true),
            ApprovalResolveOutcome::Noop => Ok(false),
            ApprovalResolveOutcome::Expired { .. } => {
                Err(ApprovalServiceError::Expired(approval_id.to_string()))
            },
        }
    }

    pub async fn resolve_with_outcome(
        &self,
        approval_id: &str,
        decision: ApprovalDecision,
        resolved_by: &str,
        current_plan_hash: Option<&str>,
    ) -> Result<ApprovalResolveOutcome, ApprovalServiceError> {
        let resolved_by = resolved_by.trim();
        if resolved_by.is_empty() {
            return Err(ApprovalServiceError::InvalidRequest(
                "resolved_by must not be empty".to_string(),
            ));
        }

        for _ in 0..MAX_RESOLVE_ATTEMPTS {
            let now = Utc::now();
            let request = self
                .store
                .get_request(approval_id)
                .await?
                .ok_or_else(|| ApprovalServiceError::NotFound(approval_id.to_string()))?;

            let mut expected_status = match request.status {
                ApprovalStatus::Approved | ApprovalStatus::Rejected => {
                    self.reconcile_terminal_delivery_state(
                        approval_id,
                        &request,
                        Some(resolved_by),
                    )
                    .await?;
                    return Ok(ApprovalResolveOutcome::Noop);
                },
                ApprovalStatus::Expired => {
                    return Ok(ApprovalResolveOutcome::Expired {
                        transitioned: false,
                    })
                },
                ApprovalStatus::Pending => ApprovalStatus::Pending,
                ApprovalStatus::Validating => {
                    if request.expires_at <= now {
                        let expiry_transition = self.resolve_transition(
                            &ApprovalStatus::Validating,
                            TRIGGER_TTL_ELAPSED,
                            &TransitionContext::default(),
                        )?;
                        let expired = self
                            .apply_transition(
                                approval_id,
                                ApprovalStatus::Validating,
                                &expiry_transition,
                                Some("system:expiry"),
                                Some(now),
                            )
                            .await?;
                        if expired {
                            return Ok(ApprovalResolveOutcome::Expired { transitioned: true });
                        }
                        continue;
                    }

                    let owner_matches = request
                        .resolved_by
                        .as_deref()
                        .map(str::trim)
                        .is_some_and(|owner| owner == resolved_by);
                    if owner_matches {
                        ApprovalStatus::Validating
                    } else if validating_recovery_ready(&request, now) {
                        let recovered = self
                            .store
                            .compare_and_set_status(
                                approval_id,
                                ApprovalStatus::Validating,
                                ApprovalStatus::Pending,
                                None,
                                None,
                            )
                            .await?;
                        if recovered {
                            continue;
                        }
                        continue;
                    } else {
                        return Ok(ApprovalResolveOutcome::Noop);
                    }
                },
            };

            if request.expires_at <= now {
                let expiry_transition = self.resolve_transition(
                    &expected_status,
                    TRIGGER_TTL_ELAPSED,
                    &TransitionContext::default(),
                )?;
                let expired = self
                    .apply_transition(
                        approval_id,
                        expected_status.clone(),
                        &expiry_transition,
                        Some("system:expiry"),
                        Some(now),
                    )
                    .await?;
                if expired {
                    return Ok(ApprovalResolveOutcome::Expired { transitioned: true });
                }
                continue;
            }

            match decision {
                ApprovalDecision::Reject => {
                    let reject_transition = self.resolve_transition(
                        &expected_status,
                        TRIGGER_DECISION_REJECT,
                        &TransitionContext::default(),
                    )?;
                    let won = self
                        .apply_transition(
                            approval_id,
                            expected_status,
                            &reject_transition,
                            Some(resolved_by),
                            Some(now),
                        )
                        .await?;
                    if !won {
                        continue;
                    }
                    return Ok(ApprovalResolveOutcome::Resolved);
                },
                ApprovalDecision::Approve => {
                    if expected_status == ApprovalStatus::Pending {
                        let claim_transition = self.resolve_transition(
                            &ApprovalStatus::Pending,
                            TRIGGER_DECISION_APPROVE,
                            &TransitionContext::default(),
                        )?;
                        let claimed = self
                            .apply_transition(
                                approval_id,
                                ApprovalStatus::Pending,
                                &claim_transition,
                                Some(resolved_by),
                                Some(now),
                            )
                            .await?;
                        if !claimed {
                            continue;
                        }
                        expected_status = ApprovalStatus::Validating;
                    }

                    let actual_hash = current_plan_hash
                        .map(str::trim)
                        .filter(|hash| !hash.is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(|| "unavailable".to_string());
                    let transition_context =
                        approval_plan_hash_context(&request.plan_hash, &actual_hash);
                    let approve_transition = self.resolve_transition(
                        &expected_status,
                        TRIGGER_DECISION_APPROVE,
                        &transition_context,
                    )?;
                    let target_status = approval_status_from_state_name(&approve_transition.to)
                        .map_err(|err| ApprovalServiceError::StateMachine(err.to_string()))?;
                    let applied = self
                        .apply_transition(
                            approval_id,
                            expected_status,
                            &approve_transition,
                            Some(resolved_by),
                            Some(Utc::now()),
                        )
                        .await?;
                    if !applied {
                        continue;
                    }

                    match target_status {
                        ApprovalStatus::Approved => return Ok(ApprovalResolveOutcome::Resolved),
                        ApprovalStatus::Rejected => {
                            return Err(ApprovalServiceError::PlanDrift {
                                approval_id: approval_id.to_string(),
                                expected_hash: request.plan_hash,
                                actual_hash,
                            });
                        },
                        other => {
                            return Err(ApprovalServiceError::StateMachine(format!(
                                "unexpected terminal transition target `{}` for decision_approve",
                                approval_status_state_name(&other)
                            )));
                        },
                    }
                },
            }
        }

        Ok(ApprovalResolveOutcome::Noop)
    }

    pub async fn expire_pending(&self, now: DateTime<Utc>) -> Result<usize, ApprovalServiceError> {
        Ok(self.expire_pending_requests(now).await?.len())
    }

    pub async fn expire_pending_requests(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<ApprovalRequest>, ApprovalServiceError> {
        let requests = self.store.list_requests().await?;
        let mut expired = Vec::new();

        for mut request in requests {
            if !matches!(
                request.status,
                ApprovalStatus::Pending | ApprovalStatus::Validating
            ) || request.expires_at > now
            {
                continue;
            }

            let expected_status = request.status.clone();
            let transition = self.resolve_transition(
                &expected_status,
                TRIGGER_TTL_ELAPSED,
                &TransitionContext::default(),
            )?;
            let transitioned = self
                .apply_transition(
                    &request.approval_id,
                    expected_status,
                    &transition,
                    Some("system:expiry"),
                    Some(now),
                )
                .await?;
            if transitioned {
                request.status = approval_status_from_state_name(&transition.to)
                    .map_err(|err| ApprovalServiceError::StateMachine(err.to_string()))?;
                request.resolved_by = Some("system:expiry".to_string());
                request.resolved_at = Some(now);
                expired.push(request);
            }
        }

        Ok(expired)
    }

    pub fn hash_plan_payload(payload: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(payload.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    async fn load_channel_configs(&self) -> Vec<ChannelConfig> {
        let default_channel = || ChannelConfig {
            name: "in_app".to_string(),
            adapter: "in_app".to_string(),
            settings: HashMap::new(),
            enabled: true,
        };

        let storage = self.store.storage();
        let path = storage.channels_path();
        let exists = match storage.exists(&path).await {
            Ok(value) => value,
            Err(err) => {
                warn!(
                    path = %path.display(),
                    error = %err,
                    "failed to check channels config; using default in_app channel"
                );
                return vec![default_channel()];
            },
        };
        if !exists {
            return vec![default_channel()];
        }

        match storage.read_yaml::<ChannelConfigFile>(&path).await {
            Ok(ChannelConfigFile::Wrapped { channels }) => channels
                .into_iter()
                .filter(|channel| channel.enabled)
                .collect(),
            Ok(ChannelConfigFile::Bare(channels)) => channels
                .into_iter()
                .filter(|channel| channel.enabled)
                .collect(),
            Err(err) => {
                warn!(
                    path = %path.display(),
                    error = %err,
                    "failed to parse channels config; using default in_app channel"
                );
                vec![default_channel()]
            },
        }
    }
}

fn default_approval_state_machine_definition() -> StateMachineDefinition {
    StateMachineDefinition {
        initial_state: "pending".to_string(),
        states: vec![
            "pending".to_string(),
            "validating".to_string(),
            "approved".to_string(),
            "rejected".to_string(),
            "expired".to_string(),
        ],
        transitions: vec![
            StateMachineTransition {
                from: "pending".to_string(),
                to: "rejected".to_string(),
                on: StateMachineTrigger::Single(TRIGGER_DECISION_REJECT.to_string()),
                when: None,
                actions: vec![
                    StateMachineAction::PersistState,
                    StateMachineAction::DismissOtherDeliveries,
                ],
            },
            StateMachineTransition {
                from: "validating".to_string(),
                to: "rejected".to_string(),
                on: StateMachineTrigger::Single(TRIGGER_DECISION_REJECT.to_string()),
                when: None,
                actions: vec![
                    StateMachineAction::PersistState,
                    StateMachineAction::DismissOtherDeliveries,
                ],
            },
            StateMachineTransition {
                from: "pending".to_string(),
                to: "validating".to_string(),
                on: StateMachineTrigger::Single(TRIGGER_DECISION_APPROVE.to_string()),
                when: None,
                actions: vec![
                    StateMachineAction::PersistState,
                    StateMachineAction::DismissOtherDeliveries,
                ],
            },
            StateMachineTransition {
                from: "validating".to_string(),
                to: "approved".to_string(),
                on: StateMachineTrigger::Single(TRIGGER_DECISION_APPROVE.to_string()),
                when: Some(GUARD_PLAN_HASH_VALID.to_string()),
                actions: vec![
                    StateMachineAction::PersistState,
                    StateMachineAction::DismissOtherDeliveries,
                ],
            },
            StateMachineTransition {
                from: "validating".to_string(),
                to: "rejected".to_string(),
                on: StateMachineTrigger::Single(TRIGGER_DECISION_APPROVE.to_string()),
                when: Some(GUARD_PLAN_HASH_INVALID.to_string()),
                actions: vec![
                    StateMachineAction::PersistState,
                    StateMachineAction::DismissOtherDeliveries,
                ],
            },
            StateMachineTransition {
                from: "pending".to_string(),
                to: "expired".to_string(),
                on: StateMachineTrigger::Single(TRIGGER_TTL_ELAPSED.to_string()),
                when: None,
                actions: vec![StateMachineAction::PersistState],
            },
            StateMachineTransition {
                from: "validating".to_string(),
                to: "expired".to_string(),
                on: StateMachineTrigger::Single(TRIGGER_TTL_ELAPSED.to_string()),
                when: None,
                actions: vec![StateMachineAction::PersistState],
            },
            StateMachineTransition {
                from: "pending".to_string(),
                to: "rejected".to_string(),
                on: StateMachineTrigger::Single(TRIGGER_DELIVERY_FAILED.to_string()),
                when: None,
                actions: vec![
                    StateMachineAction::PersistState,
                    StateMachineAction::DismissOtherDeliveries,
                ],
            },
        ],
        guards: HashMap::from([
            (
                GUARD_PLAN_HASH_VALID.to_string(),
                StateMachineGuard {
                    expr: "context.current_plan_hash == context.approval.plan_hash".to_string(),
                },
            ),
            (
                GUARD_PLAN_HASH_INVALID.to_string(),
                StateMachineGuard {
                    expr: "context.current_plan_hash != context.approval.plan_hash".to_string(),
                },
            ),
        ]),
    }
}

fn build_approval_state_machine(
    store: &ApprovalStore,
) -> Result<StateMachineInterpreter, ApprovalServiceError> {
    let path = store.storage().system_root().join("state_machines.yaml");
    let mut interpreter = StateMachineInterpreter::new();

    match store.storage().read_to_string_sync(&path) {
        Ok(raw) => {
            interpreter
                .register_machines_from_yaml_str(&raw)
                .map_err(|err| {
                    ApprovalServiceError::StateMachine(format!(
                        "failed to parse state machine catalog `{}`: {}",
                        path.display(),
                        err
                    ))
                })?;
            if !interpreter.has_machine(APPROVAL_MACHINE_NAME) {
                return Err(ApprovalServiceError::StateMachine(format!(
                    "state machine catalog `{}` is missing required machine `{}`",
                    path.display(),
                    APPROVAL_MACHINE_NAME
                )));
            }
            if !approval_machine_semantics_supported(&interpreter) {
                return Err(ApprovalServiceError::StateMachine(format!(
                    "state machine catalog `{}` has incompatible `{}` lifecycle semantics",
                    path.display(),
                    APPROVAL_MACHINE_NAME
                )));
            }
            Ok(interpreter)
        },
        Err(AgentStorageError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
            interpreter
                .register_machine(
                    APPROVAL_MACHINE_NAME,
                    default_approval_state_machine_definition(),
                )
                .map_err(|err| {
                    ApprovalServiceError::StateMachine(format!(
                        "failed to register built-in approval state machine: {err}"
                    ))
                })?;
            Ok(interpreter)
        },
        Err(err) => Err(ApprovalServiceError::StateMachine(format!(
            "failed to read state machine catalog `{}`: {}",
            path.display(),
            err
        ))),
    }
}

fn approval_machine_semantics_supported(interpreter: &StateMachineInterpreter) -> bool {
    let empty = TransitionContext::default();
    let valid_hash_ctx = approval_plan_hash_context("expected", "expected");
    let invalid_hash_ctx = approval_plan_hash_context("expected", "actual");
    let persist_only = &[StateMachineAction::PersistState];
    let persist_and_dismiss = &[
        StateMachineAction::PersistState,
        StateMachineAction::DismissOtherDeliveries,
    ];

    transition_matches_expectations(
        interpreter,
        "pending",
        TRIGGER_DECISION_APPROVE,
        &empty,
        "validating",
        persist_and_dismiss,
    ) && transition_matches_expectations(
        interpreter,
        "pending",
        TRIGGER_DECISION_REJECT,
        &empty,
        "rejected",
        persist_and_dismiss,
    ) && transition_matches_expectations(
        interpreter,
        "pending",
        TRIGGER_DELIVERY_FAILED,
        &empty,
        "rejected",
        persist_and_dismiss,
    ) && transition_matches_expectations(
        interpreter,
        "pending",
        TRIGGER_TTL_ELAPSED,
        &empty,
        "expired",
        persist_only,
    ) && transition_matches_expectations(
        interpreter,
        "validating",
        TRIGGER_DECISION_REJECT,
        &empty,
        "rejected",
        persist_and_dismiss,
    ) && transition_matches_expectations(
        interpreter,
        "validating",
        TRIGGER_DECISION_APPROVE,
        &valid_hash_ctx,
        "approved",
        persist_and_dismiss,
    ) && transition_matches_expectations(
        interpreter,
        "validating",
        TRIGGER_DECISION_APPROVE,
        &invalid_hash_ctx,
        "rejected",
        persist_and_dismiss,
    ) && transition_matches_expectations(
        interpreter,
        "validating",
        TRIGGER_TTL_ELAPSED,
        &empty,
        "expired",
        persist_only,
    )
}

fn transition_matches_expectations(
    interpreter: &StateMachineInterpreter,
    current_state: &str,
    trigger: &str,
    context: &TransitionContext,
    expected_target: &str,
    required_actions: &[StateMachineAction],
) -> bool {
    interpreter
        .transition(APPROVAL_MACHINE_NAME, current_state, trigger, context)
        .map(|resolution| {
            resolution.to == expected_target
                && transition_actions_supported_for_approval(&resolution.actions)
                && required_actions
                    .iter()
                    .all(|action| resolution.actions.contains(action))
        })
        .unwrap_or(false)
}

fn transition_actions_supported_for_approval(actions: &[StateMachineAction]) -> bool {
    let mut saw_persist = false;
    for action in actions {
        match action {
            StateMachineAction::PersistState => {
                saw_persist = true;
            },
            StateMachineAction::DismissOtherDeliveries => {
                if !saw_persist {
                    return false;
                }
            },
            StateMachineAction::EmitEvent
            | StateMachineAction::ResumeCycle
            | StateMachineAction::FailEpisode { .. } => return false,
        }
    }
    saw_persist
}

fn approval_status_state_name(status: &ApprovalStatus) -> &'static str {
    match status {
        ApprovalStatus::Pending => "pending",
        ApprovalStatus::Validating => "validating",
        ApprovalStatus::Approved => "approved",
        ApprovalStatus::Rejected => "rejected",
        ApprovalStatus::Expired => "expired",
    }
}

fn approval_status_from_state_name(state: &str) -> Result<ApprovalStatus, StateMachineError> {
    match state.trim() {
        "pending" => Ok(ApprovalStatus::Pending),
        "validating" => Ok(ApprovalStatus::Validating),
        "approved" => Ok(ApprovalStatus::Approved),
        "rejected" => Ok(ApprovalStatus::Rejected),
        "expired" => Ok(ApprovalStatus::Expired),
        other => Err(StateMachineError::InvalidDefinition {
            machine: APPROVAL_MACHINE_NAME.to_string(),
            reason: format!("unsupported approval state `{other}`"),
        }),
    }
}

fn approval_plan_hash_context(expected_hash: &str, actual_hash: &str) -> TransitionContext {
    let mut context = TransitionContext::new();
    context.insert_string("approval.plan_hash", expected_hash.to_string());
    context.insert_string("current_plan_hash", actual_hash.to_string());
    context
}

fn validate_request_inputs(
    agent_id: &str,
    goal_id: &str,
    trigger_seq: u64,
    cycle_id: &str,
    pending_actions: &[PendingApproval],
    ttl_secs: u64,
    plan_hash: &str,
) -> Result<(), ApprovalServiceError> {
    if agent_id.trim().is_empty() {
        return Err(ApprovalServiceError::InvalidRequest(
            "agent_id must not be empty".to_string(),
        ));
    }
    if goal_id.trim().is_empty() {
        return Err(ApprovalServiceError::InvalidRequest(
            "goal_id must not be empty".to_string(),
        ));
    }
    if cycle_id.trim().is_empty() {
        return Err(ApprovalServiceError::InvalidRequest(
            "cycle_id must not be empty".to_string(),
        ));
    }
    if trigger_seq == 0 {
        return Err(ApprovalServiceError::InvalidRequest(
            "trigger_seq must be > 0".to_string(),
        ));
    }
    if pending_actions.is_empty() {
        return Err(ApprovalServiceError::InvalidRequest(
            "pending_actions must not be empty".to_string(),
        ));
    }
    if ttl_secs == 0 {
        return Err(ApprovalServiceError::InvalidRequest(
            "ttl_secs must be > 0".to_string(),
        ));
    }
    if plan_hash.trim().is_empty() {
        return Err(ApprovalServiceError::InvalidRequest(
            "plan_hash must not be empty".to_string(),
        ));
    }
    Ok(())
}

fn compute_expires_at(created_at: DateTime<Utc>, ttl_secs: u64) -> DateTime<Utc> {
    let ttl_i64 = i64::try_from(ttl_secs).unwrap_or(i64::MAX);
    let Some(duration) = Duration::try_seconds(ttl_i64) else {
        return DateTime::<Utc>::MAX_UTC;
    };
    created_at
        .checked_add_signed(duration)
        .unwrap_or(DateTime::<Utc>::MAX_UTC)
}

fn validating_recovery_ready(request: &ApprovalRequest, now: DateTime<Utc>) -> bool {
    let Some(marked_at) = request.resolved_at else {
        return true;
    };
    now.signed_duration_since(marked_at) >= Duration::seconds(VALIDATING_RECOVERY_GRACE_SECS)
}

fn channel_delivery_supported(channel: &ChannelConfig) -> bool {
    let adapter = channel.adapter.trim().to_ascii_lowercase();
    match adapter.as_str() {
        "in_app" => true,
        "smtp" | "email" => channel.settings.contains_key("from"),
        "slack_webhook" => channel.settings.contains_key("webhook_url"),
        _ => false,
    }
}

fn parse_positive_u64_cycle_suffix(cycle_id: &str, delimiter: &str) -> Option<u64> {
    cycle_id
        .rsplit_once(delimiter)
        .and_then(|(_, suffix)| suffix.parse::<u64>().ok())
        .filter(|seq| *seq > 0)
}

fn trigger_seq_from_cycle_id(cycle_id: &str) -> u64 {
    parse_positive_u64_cycle_suffix(cycle_id, "::")
        .or_else(|| parse_positive_u64_cycle_suffix(cycle_id, "-"))
        .unwrap_or(1)
}

fn pending_approvals_for_action_json(
    pause_state: &AgenticPauseState,
    action_json: &str,
    action_summary: &str,
) -> Vec<PendingApproval> {
    let action: ExecutableAction = match serde_json::from_str(action_json) {
        Ok(action) => action,
        Err(err) => {
            warn!(
                error = %err,
                "failed to parse confirmation action JSON while creating approval request"
            );
            return Vec::new();
        },
    };
    let plan = ExecutionPlan {
        plan_id: pause_state
            .plan_id
            .clone()
            .unwrap_or_else(|| "approval-pause".to_string()),
        steps: vec![approval_step_from_action(&action, action_summary)],
    };
    let constraints = AgentConstraints {
        requires_approval: pause_state.approval_rules.clone(),
        ..AgentConstraints::default()
    };

    match ApprovalGate.check(&plan, &constraints) {
        ApprovalResult::Approved(_) => Vec::new(),
        ApprovalResult::NeedsApproval {
            pending_actions, ..
        } => pending_actions,
    }
}

fn file_action_for_approval(action: &FileAction) -> String {
    match action {
        FileAction::Read { .. } => "read".to_string(),
        FileAction::Write { .. } => "write".to_string(),
        FileAction::Append { .. } => "append".to_string(),
        FileAction::Delete { .. } => "delete".to_string(),
        FileAction::Copy { .. } => "copy".to_string(),
        FileAction::Move { .. } => "move".to_string(),
        FileAction::Exists { .. } => "exists".to_string(),
        FileAction::List { .. } => "list".to_string(),
        FileAction::CreateDir { .. } => "create_dir".to_string(),
    }
}

fn pack_params_for_approval(
    resolved_params: &HashMap<String, serde_json::Value>,
) -> HashMap<String, serde_json::Value> {
    let mut params = resolved_params.clone();
    if let Some(serde_json::Value::String(arguments_json)) = resolved_params.get("arguments_json") {
        if let Ok(serde_json::Value::Object(arguments)) =
            serde_json::from_str::<serde_json::Value>(arguments_json)
        {
            for key in ["confirmOrder", "confirm_order", "order_amount_inr"] {
                if let Some(value) = arguments.get(key) {
                    params
                        .entry(key.to_string())
                        .or_insert_with(|| value.clone());
                }
            }
        }
    }
    params
}

fn value_to_param_map(
    value: serde_json::Value,
    discriminator_keys: &[&str],
) -> std::collections::HashMap<String, serde_json::Value> {
    let mut params = match value {
        serde_json::Value::Object(map) => {
            map.into_iter().collect::<std::collections::HashMap<_, _>>()
        },
        _ => return HashMap::new(),
    };
    for key in discriminator_keys {
        params.remove(*key);
    }
    params
}

fn approval_step_from_action(action: &ExecutableAction, action_summary: &str) -> ExecutionStep {
    let (tool, action_type, params) = match action {
        ExecutableAction::File(action) => {
            let params = serde_json::to_value(action)
                .map(|value| value_to_param_map(value, &["operation"]))
                .unwrap_or_default();
            (
                "files".to_string(),
                file_action_for_approval(action),
                params,
            )
        },
        ExecutableAction::Http(action) => {
            let params = serde_json::to_value(action)
                .map(|value| value_to_param_map(value, &[]))
                .unwrap_or_default();
            (
                "http".to_string(),
                format!("{:?}", action.method).to_ascii_lowercase(),
                params,
            )
        },
        ExecutableAction::Bash(action) => {
            let params = serde_json::to_value(action)
                .map(|value| value_to_param_map(value, &[]))
                .unwrap_or_default();
            let _ = action; // Keeps pattern exhaustive without exposing command in summary fields.
            ("shell".to_string(), "execute".to_string(), params)
        },
        ExecutableAction::DuckDb(action) => {
            let params = serde_json::to_value(action)
                .map(|value| value_to_param_map(value, &[]))
                .unwrap_or_default();
            ("duckdb".to_string(), "query".to_string(), params)
        },
        ExecutableAction::Pack {
            capability_name,
            resolved_params,
            ..
        } => {
            let (tool, action_type) =
                super::approval::tool_action_for_approval(capability_name, resolved_params);
            (tool, action_type, pack_params_for_approval(resolved_params))
        },
        ExecutableAction::SpawnSubGoal { goal, budget } => {
            let mut params = HashMap::new();
            params.insert("goal".to_string(), serde_json::json!(goal));
            params.insert("budget".to_string(), serde_json::json!(budget));
            (
                "orchestrator".to_string(),
                "spawn_sub_goal".to_string(),
                params,
            )
        },
        ExecutableAction::DelegateToAgent { targets } => {
            let mut params = HashMap::new();
            params.insert("delegation_targets".to_string(), serde_json::json!(targets));
            (
                "orchestrator".to_string(),
                "delegate_to_agent".to_string(),
                params,
            )
        },
        ExecutableAction::HandoverToAgent {
            target_agent_id,
            context,
        } => {
            let mut params = HashMap::new();
            params.insert(
                "target_agent_id".to_string(),
                serde_json::json!(target_agent_id),
            );
            params.insert("context".to_string(), serde_json::json!(context));
            (
                "orchestrator".to_string(),
                "handover_to_agent".to_string(),
                params,
            )
        },
        ExecutableAction::SleepUntil { wake_at, .. } => {
            let mut params = HashMap::new();
            params.insert(
                "wake_at".to_string(),
                serde_json::json!(wake_at.to_rfc3339()),
            );
            ("scheduler".to_string(), "sleep_until".to_string(), params)
        },
    };

    ExecutionStep {
        id: "approval-pending-step".to_string(),
        goal: action_summary.to_string(),
        tool: Some(tool),
        action_type: Some(action_type),
        params,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::agents::types::{ActionPattern, ApprovalRule};
    use crate::magician_v2::execution::{
        actions::BashAction,
        agentic::{AgenticPauseState, EnvironmentState, ShellState},
    };
    use std::sync::Arc;

    fn stub_pending() -> Vec<PendingApproval> {
        vec![PendingApproval {
            step_id: "s1".to_string(),
            action_description: "test action".to_string(),
            matched_rule: ApprovalRule {
                tool: "browser".to_string(),
                action: ActionPattern::Single("navigate".to_string()),
                when: None,
                ttl_secs: None,
            },
        }]
    }

    fn test_service() -> ApprovalService {
        let root = std::env::temp_dir().join(format!(
            "magician-approval-service-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        ApprovalService::with_storage(AgentStorage::new(root))
            .expect("approval service should initialize")
    }

    #[test]
    fn pack_action_for_approval_uses_tool_name_discriminator() {
        let params = HashMap::from([("tool_name".to_string(), serde_json::json!("create_order"))]);

        assert_eq!(
            super::super::approval::pack_tool_for_approval("zepto-mcp__call_tool"),
            "zepto-mcp"
        );
        assert_eq!(
            super::super::approval::pack_action_for_approval("zepto-mcp__call_tool", &params),
            "create_order"
        );
    }

    #[test]
    fn pack_action_for_approval_falls_back_to_flat_leaf_action() {
        assert_eq!(
            super::super::approval::pack_tool_for_approval("browser__click"),
            "browser"
        );
        assert_eq!(
            super::super::approval::pack_action_for_approval("browser__click", &HashMap::new()),
            "click"
        );
    }

    #[test]
    fn durable_confirmation_projection_uses_canonical_lifecycle_coordinates() {
        let action = ExecutableAction::Pack {
            capability_name: "yield".to_string(),
            implementation:
                crate::magician_v2::execution::capability::ImplementationType::Compiled {
                    provider_name: "yield".to_string(),
                },
            resolved_params: HashMap::from([("summary".to_string(), serde_json::json!("done"))]),
        };

        let step = approval_step_from_action(&action, "Finish execution");

        assert_eq!(step.tool.as_deref(), Some("orchestrator"));
        assert_eq!(step.action_type.as_deref(), Some("yield"));
        assert_eq!(step.params.get("summary"), Some(&serde_json::json!("done")));
    }

    #[test]
    fn pack_params_for_approval_extracts_confirm_order_from_arguments_json() {
        let params = HashMap::from([
            ("tool_name".to_string(), serde_json::json!("create_order")),
            (
                "arguments_json".to_string(),
                serde_json::json!(
                    "{\n  \"cartId\": \"cart-1\",\n  \"confirmOrder\": true,\n  \"order_amount_inr\": 842\n}"
                ),
            ),
        ]);

        let approval_params = pack_params_for_approval(&params);

        assert_eq!(
            approval_params.get("confirmOrder"),
            Some(&serde_json::json!(true))
        );
        // The rupee order amount is hoisted onto the Attention card too.
        assert_eq!(
            approval_params.get("order_amount_inr"),
            Some(&serde_json::json!(842))
        );
        assert_eq!(
            approval_params.get("arguments_json"),
            params.get("arguments_json")
        );
    }

    async fn test_service_with_state_machine_catalog(
        catalog_yaml: &str,
    ) -> Result<ApprovalService, ApprovalServiceError> {
        let root = std::env::temp_dir().join(format!(
            "magician-approval-service-state-machine-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();

        let storage = AgentStorage::new(root);
        storage.ensure_base_layout().await.unwrap();
        let path = storage.system_root().join("state_machines.yaml");
        storage
            .write_bytes_atomic(path, catalog_yaml.as_bytes())
            .await
            .unwrap();
        ApprovalService::with_storage(storage)
    }

    fn pause_state_with_shell_approval(
        agent_id: &str,
        goal_id: &str,
        cycle_id: &str,
    ) -> AgenticPauseState {
        AgenticPauseState::new(
            1,
            "goal",
            "criteria",
            EnvironmentState::Shell(ShellState::default()),
            "history",
            16,
            3,
        )
        .with_agent_routing(agent_id, goal_id, cycle_id)
        .with_approval_rules(vec![ApprovalRule {
            tool: "shell".to_string(),
            action: ActionPattern::Single("execute".to_string()),
            when: None,
            ttl_secs: Some(60),
        }])
    }

    #[tokio::test]
    async fn resolve_is_first_writer_wins_with_concurrency() {
        let service = test_service();
        let plan_hash = ApprovalService::hash_plan_payload("{\"action\":\"navigate\"}");
        let request = service
            .create_request(
                "a1",
                "g1",
                1,
                "a1::g1::1",
                stub_pending(),
                60,
                plan_hash.clone(),
            )
            .await
            .unwrap();

        let left = {
            let service = service.clone();
            let approval_id = request.approval_id.clone();
            let plan_hash = plan_hash.clone();
            tokio::spawn(async move {
                service
                    .resolve(
                        &approval_id,
                        ApprovalDecision::Approve,
                        "in_app",
                        Some(plan_hash.as_str()),
                    )
                    .await
                    .unwrap()
            })
        };

        let right = {
            let service = service.clone();
            let approval_id = request.approval_id.clone();
            tokio::spawn(async move {
                service
                    .resolve(&approval_id, ApprovalDecision::Reject, "email", None)
                    .await
                    .unwrap()
            })
        };

        let left = left.await.unwrap();
        let right = right.await.unwrap();
        assert_ne!(left, right, "exactly one resolver should win CAS");
    }

    #[tokio::test]
    async fn resolve_returns_expired_error_when_request_expired() {
        let service = test_service();
        let request = service
            .create_request(
                "a1",
                "g1",
                1,
                "a1::g1::1",
                stub_pending(),
                60,
                ApprovalService::hash_plan_payload("payload"),
            )
            .await
            .unwrap();

        let mut stored = service
            .store
            .get_request(&request.approval_id)
            .await
            .unwrap()
            .unwrap();
        stored.expires_at = Utc::now() - Duration::seconds(1);
        service.store.save_request(&stored).await.unwrap();

        let err = service
            .resolve(
                &request.approval_id,
                ApprovalDecision::Approve,
                "in_app",
                Some(stored.plan_hash.as_str()),
            )
            .await
            .unwrap_err();
        assert_eq!(err, ApprovalServiceError::Expired(request.approval_id));
    }

    #[tokio::test]
    async fn get_request_with_transition_is_idempotent() {
        let service = test_service();
        let request = service
            .create_request(
                "a1",
                "g1",
                1,
                "a1::g1::1",
                stub_pending(),
                60,
                ApprovalService::hash_plan_payload("payload"),
            )
            .await
            .unwrap();

        let mut stored = service
            .store
            .get_request(&request.approval_id)
            .await
            .unwrap()
            .unwrap();
        stored.expires_at = Utc::now() - Duration::seconds(1);
        service.store.save_request(&stored).await.unwrap();

        let (first, first_transitioned) = service
            .get_request_with_transition(&request.approval_id)
            .await
            .unwrap();
        assert!(first_transitioned);
        assert_eq!(first.unwrap().status, ApprovalStatus::Expired);

        let (second, second_transitioned) = service
            .get_request_with_transition(&request.approval_id)
            .await
            .unwrap();
        assert!(!second_transitioned);
        assert_eq!(second.unwrap().status, ApprovalStatus::Expired);
    }

    #[tokio::test]
    async fn resolve_with_outcome_reports_expiry_transition_once() {
        let service = test_service();
        let request = service
            .create_request(
                "a1",
                "g1",
                1,
                "a1::g1::1",
                stub_pending(),
                60,
                ApprovalService::hash_plan_payload("payload"),
            )
            .await
            .unwrap();

        let mut stored = service
            .store
            .get_request(&request.approval_id)
            .await
            .unwrap()
            .unwrap();
        stored.expires_at = Utc::now() - Duration::seconds(1);
        service.store.save_request(&stored).await.unwrap();

        let first = service
            .resolve_with_outcome(
                &request.approval_id,
                ApprovalDecision::Approve,
                "in_app",
                Some(stored.plan_hash.as_str()),
            )
            .await
            .unwrap();
        assert_eq!(
            first,
            ApprovalResolveOutcome::Expired { transitioned: true }
        );

        let second = service
            .resolve_with_outcome(
                &request.approval_id,
                ApprovalDecision::Approve,
                "in_app",
                Some(stored.plan_hash.as_str()),
            )
            .await
            .unwrap();
        assert_eq!(
            second,
            ApprovalResolveOutcome::Expired {
                transitioned: false
            }
        );
    }

    #[tokio::test]
    async fn resolve_rejects_when_plan_hash_drifted() {
        let service = test_service();
        let request = service
            .create_request(
                "a1",
                "g1",
                1,
                "a1::g1::1",
                stub_pending(),
                60,
                ApprovalService::hash_plan_payload("{\"action\":\"expected\"}"),
            )
            .await
            .unwrap();
        let actual_hash = ApprovalService::hash_plan_payload("{\"action\":\"actual\"}");

        let err = service
            .resolve(
                &request.approval_id,
                ApprovalDecision::Approve,
                "in_app",
                Some(actual_hash.as_str()),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ApprovalServiceError::PlanDrift { .. }));

        let updated = service
            .get_request(&request.approval_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.status, ApprovalStatus::Rejected);
    }

    #[tokio::test]
    async fn resolve_recovers_stale_validating_claim_and_finalizes() {
        let service = test_service();
        let plan_hash = ApprovalService::hash_plan_payload("{\"action\":\"navigate\"}");
        let request = service
            .create_request(
                "a1",
                "g1",
                1,
                "a1::g1::1",
                stub_pending(),
                60,
                plan_hash.clone(),
            )
            .await
            .unwrap();

        let mut stored = service
            .store
            .get_request(&request.approval_id)
            .await
            .unwrap()
            .unwrap();
        stored.status = ApprovalStatus::Validating;
        stored.resolved_by = Some("email".to_string());
        stored.resolved_at =
            Some(Utc::now() - Duration::seconds(VALIDATING_RECOVERY_GRACE_SECS + 5));
        service.store.save_request(&stored).await.unwrap();

        let resolved = service
            .resolve(
                &request.approval_id,
                ApprovalDecision::Approve,
                "in_app",
                Some(plan_hash.as_str()),
            )
            .await
            .unwrap();
        assert!(resolved);

        let updated = service
            .get_request(&request.approval_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.status, ApprovalStatus::Approved);
        assert_eq!(updated.resolved_by.as_deref(), Some("in_app"));
    }

    #[tokio::test]
    async fn resolve_allows_owner_retry_from_validating() {
        let service = test_service();
        let plan_hash = ApprovalService::hash_plan_payload("{\"action\":\"navigate\"}");
        let request = service
            .create_request(
                "a1",
                "g1",
                1,
                "a1::g1::1",
                stub_pending(),
                60,
                plan_hash.clone(),
            )
            .await
            .unwrap();

        let mut stored = service
            .store
            .get_request(&request.approval_id)
            .await
            .unwrap()
            .unwrap();
        stored.status = ApprovalStatus::Validating;
        stored.resolved_by = Some("in_app".to_string());
        stored.resolved_at = Some(Utc::now());
        service.store.save_request(&stored).await.unwrap();

        let resolved = service
            .resolve(
                &request.approval_id,
                ApprovalDecision::Approve,
                "in_app",
                Some(plan_hash.as_str()),
            )
            .await
            .unwrap();
        assert!(resolved);

        let updated = service
            .get_request(&request.approval_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.status, ApprovalStatus::Approved);
    }

    #[tokio::test]
    async fn expire_pending_expires_validating_entries() {
        let service = test_service();
        let request = service
            .create_request(
                "a1",
                "g1",
                1,
                "a1::g1::1",
                stub_pending(),
                60,
                ApprovalService::hash_plan_payload("payload"),
            )
            .await
            .unwrap();

        let mut stored = service
            .store
            .get_request(&request.approval_id)
            .await
            .unwrap()
            .unwrap();
        stored.status = ApprovalStatus::Validating;
        stored.expires_at = Utc::now() - Duration::seconds(1);
        stored.resolved_by = Some("in_app".to_string());
        stored.resolved_at = Some(Utc::now() - Duration::seconds(1));
        service.store.save_request(&stored).await.unwrap();

        let expired = service.expire_pending(Utc::now()).await.unwrap();
        assert_eq!(expired, 1);

        let updated = service
            .get_request(&request.approval_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.status, ApprovalStatus::Expired);
        assert_eq!(updated.resolved_by.as_deref(), Some("system:expiry"));
    }

    #[tokio::test]
    async fn find_latest_request_for_cycle_plan_prefers_newest_match() {
        let service = test_service();
        let plan_hash = ApprovalService::hash_plan_payload("{\"action\":\"navigate\"}");

        let first = service
            .create_request(
                "a1",
                "g1",
                1,
                "a1::g1::1",
                stub_pending(),
                60,
                plan_hash.clone(),
            )
            .await
            .unwrap();
        // Ensure deterministic ordering by created_at.
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        let second = service
            .create_request(
                "a1",
                "g1",
                1,
                "a1::g1::1",
                stub_pending(),
                60,
                plan_hash.clone(),
            )
            .await
            .unwrap();

        let found = service
            .find_latest_request_for_cycle_plan("a1", "g1", "a1::g1::1", &plan_hash)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.approval_id, second.approval_id);
        assert_ne!(found.approval_id, first.approval_id);
    }

    #[tokio::test]
    async fn fan_out_all_channels_failed_is_policy_outcome_not_error() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = AgentStorage::new(tmp.path());
        storage.ensure_base_layout().await.unwrap();
        storage
            .write_yaml_atomic(
                storage.channels_path(),
                &serde_json::json!({
                    "channels": [
                        { "name": "broken", "adapter": "unknown_adapter", "enabled": true }
                    ]
                }),
            )
            .await
            .unwrap();

        let service =
            ApprovalService::with_storage(storage).expect("approval service should initialize");
        let request = service
            .create_request(
                "a1",
                "g1",
                1,
                "a1::g1::1",
                stub_pending(),
                60,
                ApprovalService::hash_plan_payload("payload"),
            )
            .await
            .unwrap();

        let result = service.fan_out(&request).await.unwrap();
        assert_eq!(result, FanOutResult::AllChannelsFailed);

        let updated = service
            .get_request(&request.approval_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.status, ApprovalStatus::Rejected);
    }

    #[tokio::test]
    async fn with_storage_rejects_invalid_state_machine_catalog() {
        let err = test_service_with_state_machine_catalog(
            r#"
state_machines:
  approval:
    initial_state: pending
"#,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ApprovalServiceError::StateMachine(_)));
    }

    #[tokio::test]
    async fn with_storage_rejects_incompatible_state_machine_catalog() {
        let err = test_service_with_state_machine_catalog(
            r#"
state_machines:
  approval:
    initial_state: pending
    states: [pending, approved, rejected]
    transitions:
      - from: pending
        to: approved
        on: decision_approve
        actions: [persist_state]
      - from: pending
        to: rejected
        on: decision_reject
        actions: [persist_state]
"#,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ApprovalServiceError::StateMachine(_)));
    }

    #[tokio::test]
    async fn resolve_noop_reconciles_terminal_delivery_state() {
        let service = test_service();
        let plan_hash = ApprovalService::hash_plan_payload("{\"action\":\"navigate\"}");
        let request = service
            .create_request(
                "a1",
                "g1",
                1,
                "a1::g1::1",
                stub_pending(),
                60,
                plan_hash.clone(),
            )
            .await
            .unwrap();

        let mut stored = service
            .store
            .get_request(&request.approval_id)
            .await
            .unwrap()
            .unwrap();
        stored.status = ApprovalStatus::Approved;
        stored.resolved_by = Some("in_app".to_string());
        stored.resolved_at = Some(Utc::now());
        service.store.save_request(&stored).await.unwrap();
        service
            .store
            .save_delivery(&ApprovalDelivery::sent(&request.approval_id, "in_app"))
            .await
            .unwrap();
        service
            .store
            .save_delivery(&ApprovalDelivery::sent(&request.approval_id, "email"))
            .await
            .unwrap();

        let outcome = service
            .resolve_with_outcome(
                &request.approval_id,
                ApprovalDecision::Approve,
                "in_app",
                Some(plan_hash.as_str()),
            )
            .await
            .unwrap();
        assert_eq!(outcome, ApprovalResolveOutcome::Noop);

        let deliveries = service.list_deliveries(&request.approval_id).await.unwrap();
        let in_app = deliveries
            .iter()
            .find(|delivery| delivery.channel == "in_app")
            .unwrap();
        let email = deliveries
            .iter()
            .find(|delivery| delivery.channel == "email")
            .unwrap();
        assert_eq!(in_app.status, DeliveryStatus::Resolved);
        assert_eq!(email.status, DeliveryStatus::Dismissed);
    }

    #[tokio::test]
    async fn maybe_create_for_confirmation_pause_dedupes_atomically_under_concurrency() {
        let service = test_service();
        let pause_state = pause_state_with_shell_approval("a1", "g1", "a1::g1::1");
        let action_summary = "Execute shell command".to_string();
        let action_json =
            serde_json::to_string(&ExecutableAction::Bash(BashAction::new("echo hi")))
                .expect("action_json should serialize");

        let fanout = 8usize;
        let barrier = Arc::new(tokio::sync::Barrier::new(fanout));
        let mut handles = Vec::new();
        for _ in 0..fanout {
            let svc = service.clone();
            let ps = pause_state.clone();
            let summary = action_summary.clone();
            let json = action_json.clone();
            let barrier = barrier.clone();
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                svc.maybe_create_for_confirmation_pause(&ps, &summary, "approval required", &json)
                    .await
                    .expect("approval creation should succeed")
                    .expect("approval request should exist")
                    .approval_id
            }));
        }

        let mut approval_ids = Vec::new();
        for handle in handles {
            approval_ids.push(handle.await.expect("task should join"));
        }
        assert!(
            !approval_ids.is_empty(),
            "at least one request should be returned"
        );
        let winner = approval_ids[0].clone();
        assert!(
            approval_ids.iter().all(|id| id == &winner),
            "all concurrent callers should receive the same deduped approval id"
        );

        let plan_hash = ApprovalService::hash_plan_payload(&action_json);
        let matching = service
            .list_pending()
            .await
            .expect("pending list should load")
            .into_iter()
            .filter(|request| {
                request.agent_id == "a1"
                    && request.goal_id == "g1"
                    && request.cycle_id == "a1::g1::1"
                    && request.plan_hash == plan_hash
            })
            .collect::<Vec<_>>();
        assert_eq!(
            matching.len(),
            1,
            "atomic dedup should keep exactly one live pending request"
        );

        let deliveries = service
            .list_deliveries(&winner)
            .await
            .expect("deliveries should load");
        assert_eq!(
            deliveries.len(),
            1,
            "fan-out should run only for the single created request"
        );
    }

    #[tokio::test]
    async fn maybe_create_for_confirmation_pause_reports_created_new_once_under_concurrency() {
        let service = test_service();
        let pause_state = pause_state_with_shell_approval("a1", "g1", "a1::g1::2");
        let action_summary = "Execute shell command".to_string();
        let action_json =
            serde_json::to_string(&ExecutableAction::Bash(BashAction::new("echo hi")))
                .expect("action_json should serialize");

        let fanout = 8usize;
        let barrier = Arc::new(tokio::sync::Barrier::new(fanout));
        let mut handles = Vec::new();
        for _ in 0..fanout {
            let svc = service.clone();
            let ps = pause_state.clone();
            let summary = action_summary.clone();
            let json = action_json.clone();
            let barrier = barrier.clone();
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                let outcome = svc
                    .maybe_create_for_confirmation_pause_with_outcome(
                        &ps,
                        &summary,
                        "approval required",
                        &json,
                    )
                    .await
                    .expect("approval creation should succeed");
                (
                    outcome.created_new,
                    outcome
                        .request
                        .expect("approval request should exist")
                        .approval_id,
                )
            }));
        }

        let mut created_new_true_count = 0usize;
        let mut approval_ids = Vec::new();
        for handle in handles {
            let (created_new, approval_id) = handle.await.expect("task should join");
            if created_new {
                created_new_true_count += 1;
            }
            approval_ids.push(approval_id);
        }

        assert_eq!(
            created_new_true_count, 1,
            "exactly one caller should observe created_new=true for a deduped approval"
        );
        let winner = approval_ids[0].clone();
        assert!(
            approval_ids.iter().all(|id| id == &winner),
            "all concurrent callers should receive the same deduped approval id"
        );
    }

    #[test]
    fn trigger_seq_from_cycle_id_supports_legacy_and_current_formats() {
        assert_eq!(trigger_seq_from_cycle_id("agent::goal::17"), 17);
        assert_eq!(trigger_seq_from_cycle_id("agent-id-goal-id-42"), 42);
    }

    #[test]
    fn trigger_seq_from_cycle_id_defaults_to_one_for_invalid_suffix() {
        assert_eq!(trigger_seq_from_cycle_id("agent-id-goal-id"), 1);
        assert_eq!(trigger_seq_from_cycle_id("agent::goal::0"), 1);
        assert_eq!(trigger_seq_from_cycle_id(""), 1);
    }

    #[test]
    fn transition_actions_supported_for_approval_requires_persist_before_dismiss() {
        assert!(super::transition_actions_supported_for_approval(&[
            StateMachineAction::PersistState,
            StateMachineAction::DismissOtherDeliveries,
        ]));
        assert!(!super::transition_actions_supported_for_approval(&[
            StateMachineAction::DismissOtherDeliveries,
            StateMachineAction::PersistState,
        ]));
        assert!(!super::transition_actions_supported_for_approval(&[
            StateMachineAction::EmitEvent,
            StateMachineAction::PersistState,
        ]));
    }
}
