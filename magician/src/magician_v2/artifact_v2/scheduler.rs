use std::sync::Arc;

use async_trait::async_trait;
use tracing::warn;

use crate::magician_v2::{
    agents::AgentRuntime,
    execution::{
        actions::DelegationTargetRequest,
        agentic::delegation_dispatch::{
            DelegationDispatcher, DelegationSpawnResult, DelegationTarget, DispatchError,
        },
    },
    orchestrator::MagicianV2Orchestrator,
};

use super::{
    models::{ExecutionScheduleReadinessRecord, ScheduleReadinessStep},
    service::{ArtifactV2Error, ArtifactV2Service, ScopeRef, V3ReadApi},
};

#[derive(Debug, Clone)]
struct LaunchableDelegationMatch {
    step_id: String,
    sub_goal: String,
    target_agent_id: String,
}

pub struct V3DelegationDispatcher {
    inner: Arc<dyn DelegationDispatcher>,
    runtime: Arc<AgentRuntime>,
    orchestrator: Arc<MagicianV2Orchestrator>,
    v3_service: Arc<ArtifactV2Service>,
}

impl V3DelegationDispatcher {
    pub fn new(
        inner: Arc<dyn DelegationDispatcher>,
        runtime: Arc<AgentRuntime>,
        orchestrator: Arc<MagicianV2Orchestrator>,
        v3_service: Arc<ArtifactV2Service>,
    ) -> Self {
        Self {
            inner,
            runtime,
            orchestrator,
            v3_service,
        }
    }

    async fn spawn_children_with_admission(
        &self,
        source_agent_id: &str,
        source_execution_id: &str,
        source_chain_id: Option<&str>,
        targets: Vec<DelegationTargetRequest>,
        expected_child_execution_id: Option<&str>,
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<DelegationSpawnResult, DispatchError> {
        if expected_child_execution_id.is_some() && targets.len() != 1 {
            return Err(DispatchError::DispatchFailed(
                "exact V3 delegation requires one target".to_owned(),
            ));
        }
        let maybe_v3_scope = self
            .resolve_v3_schedule_scope(source_execution_id)
            .await
            .map_err(DispatchError::Runtime)?;

        let Some((scope, task_id, schedule)) = maybe_v3_scope else {
            return Err(DispatchError::Runtime(format!(
                "delegation requires a V3-backed root execution: {source_execution_id}"
            )));
        };

        let launch_matches = match_launchable_delegations(&schedule, &targets)?;
        for launch_match in &launch_matches {
            self.v3_service
                .record_delegation_launch_authorized(
                    &scope,
                    &task_id,
                    source_execution_id,
                    &launch_match.step_id,
                    &launch_match.sub_goal,
                    &launch_match.target_agent_id,
                )
                .await
                .map_err(|error| DispatchError::Runtime(error.to_string()))?;
        }

        let runtime_result = match expected_child_execution_id {
            Some(expected_child_execution_id) => {
                let target = targets.into_iter().next().ok_or_else(|| {
                    DispatchError::DispatchFailed(
                        "exact V3 delegation target disappeared before admission".to_owned(),
                    )
                })?;
                crate::magician_v2::agents::runtime::spawn_exact_delegated_child_from_runtime(
                    Arc::clone(&self.runtime),
                    source_agent_id,
                    source_execution_id,
                    source_chain_id,
                    target,
                    expected_child_execution_id,
                    cancel,
                )
                .await
            },
            None => {
                crate::magician_v2::agents::runtime::spawn_delegated_children_from_runtime(
                    Arc::clone(&self.runtime),
                    source_agent_id,
                    source_execution_id,
                    source_chain_id,
                    targets,
                    cancel,
                )
                .await
            },
        };
        let result = match runtime_result {
            Ok(result) => result,
            Err(error) => {
                for launch_match in &launch_matches {
                    let _ = self
                        .v3_service
                        .record_delegation_launch_failed(
                            &scope,
                            &task_id,
                            source_execution_id,
                            &launch_match.step_id,
                            &launch_match.sub_goal,
                            &launch_match.target_agent_id,
                            &error.to_string(),
                        )
                        .await;
                }
                return Err(error);
            },
        };

        if result.child_executions.len() != launch_matches.len() {
            warn!(
                execution_id = %source_execution_id,
                authorized_launches = launch_matches.len(),
                returned_children = result.child_executions.len(),
                "[ARTIFACT-V2-SCHEDULER] Runtime returned a different child count than the V3 schedule authorized; continuing with best-effort reconciliation"
            );
        }

        for (launch_match, child) in launch_matches.iter().zip(result.child_executions.iter()) {
            if let Err(error) = self
                .v3_service
                .record_delegation_launch_dispatched(
                    &scope,
                    &task_id,
                    source_execution_id,
                    &launch_match.step_id,
                    &launch_match.sub_goal,
                    &launch_match.target_agent_id,
                    &child.execution_id,
                )
                .await
            {
                warn!(
                    error = %error,
                    execution_id = %source_execution_id,
                    child_execution_id = %child.execution_id,
                    step_id = %launch_match.step_id,
                    "[ARTIFACT-V2-SCHEDULER] Failed to mirror dispatched delegated child into V3; continuing because the runtime child already exists"
                );
                let _ = self
                    .v3_service
                    .record_unmatched_child_launch_dispatched(
                        &scope,
                        &task_id,
                        source_execution_id,
                        &child.execution_id,
                        &child.target_agent_id,
                    )
                    .await;
            }
        }

        for child in result.child_executions.iter().skip(launch_matches.len()) {
            let _ = self
                .v3_service
                .record_unmatched_child_launch_dispatched(
                    &scope,
                    &task_id,
                    source_execution_id,
                    &child.execution_id,
                    &child.target_agent_id,
                )
                .await;
        }

        Ok(result)
    }
}

impl std::fmt::Debug for V3DelegationDispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("V3DelegationDispatcher")
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl DelegationDispatcher for V3DelegationDispatcher {
    async fn available_targets(
        &self,
        source_agent_id: &str,
        principal: Option<&str>,
        workspace: Option<&str>,
    ) -> Vec<DelegationTarget> {
        self.inner
            .available_targets(source_agent_id, principal, workspace)
            .await
    }

    async fn spawn_children(
        &self,
        source_agent_id: &str,
        source_execution_id: &str,
        source_chain_id: Option<&str>,
        targets: Vec<DelegationTargetRequest>,
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<DelegationSpawnResult, DispatchError> {
        self.spawn_children_with_admission(
            source_agent_id,
            source_execution_id,
            source_chain_id,
            targets,
            None,
            cancel,
        )
        .await
    }

    async fn spawn_exact_child(
        &self,
        source_agent_id: &str,
        source_execution_id: &str,
        source_chain_id: Option<&str>,
        target: DelegationTargetRequest,
        expected_child_execution_id: &str,
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<DelegationSpawnResult, DispatchError> {
        self.spawn_children_with_admission(
            source_agent_id,
            source_execution_id,
            source_chain_id,
            vec![target],
            Some(expected_child_execution_id),
            cancel,
        )
        .await
    }

    async fn spawn_agent_tool_child(
        &self,
        source_agent_id: &str,
        source_execution_id: &str,
        source_chain_id: Option<&str>,
        target: DelegationTargetRequest,
        launch: super::app_agent_tool::AppAgentToolReservedLaunch,
        cancel: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<DelegationSpawnResult, DispatchError> {
        let Some((scope, task_id, _schedule)) = self
            .resolve_v3_schedule_scope(source_execution_id)
            .await
            .map_err(DispatchError::Runtime)?
        else {
            return Err(DispatchError::Runtime(format!(
                "agent_as_tool requires a V3-backed root execution: {source_execution_id}"
            )));
        };
        if launch.scope() != &scope || launch.task_id() != task_id {
            return Err(DispatchError::DispatchFailed(
                "agent_as_tool launch intent does not match the canonical parent task scope"
                    .to_owned(),
            ));
        }
        self.inner
            .spawn_agent_tool_child(
                source_agent_id,
                source_execution_id,
                source_chain_id,
                target,
                launch,
                cancel,
            )
            .await
    }

    async fn recover_sleeping_child(
        &self,
        binding: crate::magician_v2::execution::agentic::delegation_dispatch::DelegatedChildRecoveryBinding,
        stateless_source_segment: Option<String>,
        execution_retry_due_at: chrono::DateTime<chrono::Utc>,
        execution_retry_claimed_until: chrono::DateTime<chrono::Utc>,
    ) -> Result<crate::magician_v2::execution::AgenticOutcome, DispatchError> {
        self.inner
            .recover_sleeping_child(
                binding,
                stateless_source_segment,
                execution_retry_due_at,
                execution_retry_claimed_until,
            )
            .await
    }

    async fn recover_interrupted_child(
        &self,
        binding: crate::magician_v2::execution::agentic::delegation_dispatch::DelegatedChildRecoveryBinding,
    ) -> Result<crate::magician_v2::execution::AgenticOutcome, DispatchError> {
        self.inner.recover_interrupted_child(binding).await
    }
}

impl V3DelegationDispatcher {
    async fn resolve_v3_schedule_scope(
        &self,
        source_execution_id: &str,
    ) -> Result<Option<(ScopeRef, String, ExecutionScheduleReadinessRecord)>, String> {
        let parent_execution = self.orchestrator.get_execution(source_execution_id).await?;
        let Some(task_id) = parent_execution.task_id.clone() else {
            return Ok(None);
        };
        let scope = ScopeRef::system_internal_unauthenticated(
            &parent_execution.principal,
            &parent_execution.workspace,
        );

        match self.v3_service.get_task(&scope, &task_id).await {
            Ok(_) => {},
            Err(ArtifactV2Error::TaskNotFound(_)) => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "failed to load V3 task scope for execution '{}': {}",
                    source_execution_id, error
                ));
            },
        }

        match self
            .v3_service
            .get_execution_schedule_readiness(&scope, &task_id, source_execution_id)
            .await
        {
            Ok(schedule) => Ok(Some((scope, task_id, schedule))),
            Err(error) => Err(format!(
                "failed to load V3 schedule readiness for execution '{}': {}",
                source_execution_id, error
            )),
        }
    }
}

fn match_launchable_delegations(
    schedule: &ExecutionScheduleReadinessRecord,
    targets: &[DelegationTargetRequest],
) -> Result<Vec<LaunchableDelegationMatch>, DispatchError> {
    // ─── Seed-plan ad-hoc delegation bypass ──────────────────────────────
    //
    // Seed-style plans (`runtime_context_seed`, chat-inline direct
    // executions, anything created without an upfront planner pass)
    // carry steps that belong to the source agent itself — they
    // don't pre-declare any delegate slots. For these plans the V3
    // schedule is NOT authoritative about which delegations are
    // allowed: the agentic loop decides at runtime which specialist
    // to hand work to.
    //
    // Without this bypass, every `delegate_to_agent` decision from
    // such a plan gets denied with "no ready_to_launch step matched
    // the target agent" because the filter below produces an empty
    // candidate set (no step has a `delegate_agent_id`). The agent's
    // prompt advertises 17 delegation targets via `[DELEGATION] N
    // targets injected into prompt` — the scheduler rejects every
    // one. See the SDA delegation investigation.
    //
    // Detection signature: NO step in the schedule has any
    // `delegate_agent_id` set. If even one step pre-declares a
    // delegate slot, we fall through to the strict matching path
    // (legacy fully-planned executions, those still want the
    // pre-authorization).
    //
    // Synthetic `step_id` lets `record_delegation_launch_authorized`
    // still write its telemetry row — the launch is real, it just
    // wasn't pre-planned against a specific step. Downstream
    // consumers reading delegation launch records by `step_id` see
    // the sentinel and can branch (or ignore) accordingly.
    if schedule
        .steps
        .iter()
        .all(|step| step.delegate_agent_id.is_none())
    {
        return Ok(targets
            .iter()
            .map(|target| LaunchableDelegationMatch {
                step_id: "ad-hoc-delegation".to_string(),
                sub_goal: target.context.clone(),
                target_agent_id: target.target_agent_id.clone(),
            })
            .collect());
    }

    let mut available_steps = schedule
        .steps
        .iter()
        .filter(|step| step.readiness == "ready_to_launch" && step.delegate_agent_id.is_some())
        .cloned()
        .collect::<Vec<ScheduleReadinessStep>>();
    available_steps.sort_by_key(|step| step.order);

    let mut matches = Vec::with_capacity(targets.len());
    for target in targets {
        let candidate_indices = available_steps
            .iter()
            .enumerate()
            .filter_map(|(index, step)| {
                (step.delegate_agent_id.as_deref() == Some(target.target_agent_id.as_str()))
                    .then_some(index)
            })
            .collect::<Vec<_>>();

        let Some(index) = select_matching_step_index(&available_steps, &candidate_indices, target)
        else {
            if candidate_indices.len() > 1 {
                return Err(DispatchError::DispatchFailed(format!(
                    "V3 schedule denied delegation launch for target '{}': multiple ready_to_launch steps matched and none uniquely matched the delegation context",
                    target.target_agent_id
                )));
            }
            return Err(DispatchError::DispatchFailed(format!(
                "V3 schedule denied delegation launch for target '{}': no ready_to_launch step matched the target agent",
                target.target_agent_id
            )));
        };
        let step = available_steps.remove(index);
        matches.push(LaunchableDelegationMatch {
            step_id: step.step_id,
            sub_goal: target.context.clone(),
            target_agent_id: target.target_agent_id.clone(),
        });
    }

    Ok(matches)
}

fn select_matching_step_index(
    available_steps: &[ScheduleReadinessStep],
    candidate_indices: &[usize],
    target: &DelegationTargetRequest,
) -> Option<usize> {
    if candidate_indices.is_empty() {
        return None;
    }
    if candidate_indices.len() == 1 {
        let index = candidate_indices.first().copied()?;
        return context_matches_step(&available_steps[index], target).then_some(index);
    }

    let exact_matches = candidate_indices
        .iter()
        .copied()
        .filter(|index| context_matches_step(&available_steps[*index], target))
        .collect::<Vec<_>>();

    if exact_matches.len() == 1 {
        return exact_matches.first().copied();
    }

    None
}

fn context_matches_step(step: &ScheduleReadinessStep, target: &DelegationTargetRequest) -> bool {
    let normalized_target = normalize_schedule_text(&target.context);
    if normalized_target.is_empty() {
        return true;
    }

    let normalized_title = normalize_schedule_text(&step.title);
    if !normalized_title.is_empty()
        && (normalized_target == normalized_title
            || normalized_target.contains(normalized_title.as_str())
            || normalized_title.contains(normalized_target.as_str()))
    {
        return true;
    }

    step.sub_step_labels.iter().any(|label| {
        let normalized_label = normalize_schedule_text(label);
        !normalized_label.is_empty()
            && (normalized_target == normalized_label
                || normalized_target.contains(normalized_label.as_str())
                || normalized_label.contains(normalized_target.as_str()))
    })
}

fn normalize_schedule_text(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use serde_json::json;

    use super::*;

    fn ready_step(
        step_id: &str,
        title: &str,
        order: usize,
        delegate_agent_id: &str,
    ) -> ScheduleReadinessStep {
        ScheduleReadinessStep {
            step_id: step_id.to_string(),
            title: title.to_string(),
            order,
            depends_on_step_ids: Vec::new(),
            blocked_by_step_ids: Vec::new(),
            capability: None,
            delegate_agent_id: Some(delegate_agent_id.to_string()),
            taskplan_status: "pending".to_string(),
            taskplan_progress: "0/1".to_string(),
            sub_step_labels: Vec::new(),
            readiness: "ready_to_launch".to_string(),
            child_execution_id: None,
            child_output_id: None,
            child_execution_status: None,
            detail: None,
        }
    }

    fn request(target_agent_id: &str, context: &str) -> DelegationTargetRequest {
        DelegationTargetRequest {
            target_agent_id: target_agent_id.to_string(),
            context: context.to_string(),
            input_artifact_ids: Vec::new(),
            input_data: Some(json!({"context": context})),
            depth: None,
            timeout_secs: None,
            spend_token_ids: Vec::new(),
            required_capability: None,
            expected_artifacts: Vec::new(),
        }
    }

    fn schedule_with_steps(steps: Vec<ScheduleReadinessStep>) -> ExecutionScheduleReadinessRecord {
        ExecutionScheduleReadinessRecord {
            execution_id: "exec-root".to_string(),
            task_id: "task-1".to_string(),
            plan_id: Some("plan-1".to_string()),
            updated_at: "2026-03-28T00:00:00Z".to_string(),
            ready_step_ids: steps.iter().map(|step| step.step_id.clone()).collect(),
            blocked_step_ids: Vec::new(),
            waiting_step_ids: Vec::new(),
            satisfied_step_ids: Vec::new(),
            running_step_ids: Vec::new(),
            failed_step_ids: Vec::new(),
            waiting_for_children: false,
            steps,
        }
    }

    /// Seed-style plans (`runtime_context_seed`, chat-inline direct
    /// executions) have no pre-declared delegate slots. The agentic
    /// loop chooses delegates at runtime. The scheduler must let
    /// these through with synthetic launch matches; the legacy
    /// strict-match path applies only to plans that *do* pre-plan
    /// delegations. See SDA delegation investigation.
    fn unplanned_agent_step(step_id: &str, title: &str, order: usize) -> ScheduleReadinessStep {
        ScheduleReadinessStep {
            step_id: step_id.to_string(),
            title: title.to_string(),
            order,
            depends_on_step_ids: Vec::new(),
            blocked_by_step_ids: Vec::new(),
            capability: None,
            delegate_agent_id: None,
            taskplan_status: "in_progress".to_string(),
            taskplan_progress: "0/1".to_string(),
            sub_step_labels: Vec::new(),
            readiness: "running".to_string(),
            child_execution_id: None,
            child_output_id: None,
            child_execution_status: None,
            detail: None,
        }
    }

    #[test]
    fn match_launchable_delegations_permits_ad_hoc_on_seed_plans() {
        let schedule = schedule_with_steps(vec![unplanned_agent_step(
            "direct-step-1",
            "Analyze SKU margin leaders for 20 May",
            0,
        )]);

        let matches = match_launchable_delegations(
            &schedule,
            &[request(
                "simple-data-analyst",
                "Analyze SKU margin leaders for 20 May",
            )],
        )
        .expect("seed-style schedule should allow ad-hoc delegation");

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].target_agent_id, "simple-data-analyst");
        // Synthetic step_id flags the launch as not bound to a
        // pre-planned slot — telemetry consumers can branch on this
        // sentinel if they need to distinguish.
        assert_eq!(matches[0].step_id, "ad-hoc-delegation");
    }

    #[test]
    fn match_launchable_delegations_handles_multiple_targets_on_seed_plans() {
        let schedule = schedule_with_steps(vec![unplanned_agent_step(
            "direct-step-1",
            "Triage incoming requests",
            0,
        )]);

        let matches = match_launchable_delegations(
            &schedule,
            &[
                request("simple-data-analyst", "Compute SKU margin"),
                request("web-researcher", "Look up market trends"),
            ],
        )
        .expect("seed-style schedule should allow multiple ad-hoc targets");

        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].target_agent_id, "simple-data-analyst");
        assert_eq!(matches[1].target_agent_id, "web-researcher");
        assert_eq!(matches[0].step_id, "ad-hoc-delegation");
        assert_eq!(matches[1].step_id, "ad-hoc-delegation");
    }

    #[test]
    fn match_launchable_delegations_keeps_strict_match_when_plan_has_delegate_slots() {
        // Mix: one pre-planned delegate slot + one unplanned step.
        // The presence of ANY pre-declared delegate_agent_id flips
        // the schedule into "fully planned" mode and strict matching
        // applies — the unplanned step doesn't trigger the bypass.
        let schedule = schedule_with_steps(vec![
            ready_step("step-a", "Research suppliers", 0, "agent:ops"),
            unplanned_agent_step("direct-step-1", "Source agent's own work", 1),
        ]);

        let error =
            match_launchable_delegations(&schedule, &[request("agent:writer", "Write report")])
                .expect_err("strict matching applies once any step pre-declares a delegate");
        let message = error.to_string();
        assert!(message.contains("no ready_to_launch step matched"));
    }

    #[test]
    fn match_launchable_delegations_prefers_exact_context_match() {
        let schedule = schedule_with_steps(vec![
            ready_step("step-a", "Research suppliers", 0, "agent:ops"),
            ready_step("step-b", "Draft customer email", 1, "agent:ops"),
        ]);

        let matches = match_launchable_delegations(
            &schedule,
            &[request("agent:ops", "Draft customer email")],
        )
        .expect("exact context match should succeed");

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].step_id, "step-b");
    }

    #[test]
    fn match_launchable_delegations_rejects_ambiguous_same_agent_match() {
        let schedule = schedule_with_steps(vec![
            ready_step("step-a", "Research suppliers", 0, "agent:ops"),
            ready_step("step-b", "Draft customer email", 1, "agent:ops"),
        ]);

        let error = match_launchable_delegations(&schedule, &[request("agent:ops", "Do the work")])
            .expect_err("ambiguous same-agent match should fail");

        let message = error.to_string();
        assert!(message.contains("multiple ready_to_launch steps matched"));
    }

    #[test]
    fn match_launchable_delegations_rejects_single_candidate_context_mismatch() {
        let schedule = schedule_with_steps(vec![ready_step(
            "step-a",
            "Research suppliers",
            0,
            "agent:ops",
        )]);

        let error = match_launchable_delegations(
            &schedule,
            &[request("agent:ops", "Draft customer email")],
        )
        .expect_err("single-candidate context mismatch should fail");

        let message = error.to_string();
        assert!(message.contains("no ready_to_launch step matched"));
    }

    #[test]
    fn match_launchable_delegations_preserves_order_for_distinct_targets() {
        let schedule = schedule_with_steps(vec![
            ready_step("step-a", "Research suppliers", 0, "agent:ops"),
            ready_step("step-b", "Draft customer email", 1, "agent:writer"),
        ]);

        let matches = match_launchable_delegations(
            &schedule,
            &[
                request("agent:ops", "Research suppliers"),
                request("agent:writer", "Draft customer email"),
            ],
        )
        .expect("distinct target mapping should succeed");

        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].step_id, "step-a");
        assert_eq!(matches[1].step_id, "step-b");
    }
}
