use std::{collections::HashMap, path::Component, sync::Arc};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use serde_json::{json, Map, Value};

use super::actions::{ActionResult, ExecutableAction};
use super::capability::{CapabilityPackDefinition, CapabilityProvider, ImplementationType};
use super::error::ExecutionError;
use crate::magician_v2::{
    agents::{
        focus_area_goal_id, types::AgentKind, AgentDefinition, AgentDefinitionStore,
        NewDefinitionProposal, ProposalFilter, ProposalStatus, ProposalStore,
    },
    artifact_v2::{
        execution_artifacts::FilesystemExecutionArtifactIndexStore,
        memory::V3EpisodeRecord,
        models::{
            build_dashboard_publish_input, default_ui_thread_id, TaskListItemV3, TaskOutputMode,
            TaskRecord, TaskTagRecord,
        },
        service::{CreateTaskInput, ScopeRef, V3ReadApi},
    },
    harness::{
        BacklogDeliveryDisposition, BacklogItem, BacklogPriority, BacklogStatus, BacklogStore,
        HarnessExecutionContext, HarnessScope, HarnessScopeTarget, HarnessServices,
        HarnessTraceReader, CREATE_AGENT_TOOL_NAME, CREATE_DASHBOARD_TOOL_NAME,
        CREATE_PROPOSAL_TOOL_NAME, CREATE_TASK_TOOL_NAME, EVALUATE_HARNESS_TOOL_NAME,
        HARNESS_ACTION_TOOL_NAMES, HARNESS_READ_TOOL_NAMES, INSPECT_AGENT_TOOL_NAME,
        INSPECT_BACKLOG_DELIVERY_TOOL_NAME, LIST_AGENTS_TOOL_NAME, LIST_EPISODES_TOOL_NAME,
        LIST_PROPOSALS_TOOL_NAME, PROMOTE_BACKLOG_ITEM_TOOL_NAME, PROPOSE_BACKLOG_ITEM_TOOL_NAME,
        READ_PROGRAM_STATE_TOOL_NAME, READ_TRACE_TOOL_NAME, REASSIGN_TASK_TOOL_NAME,
        RETIRE_AGENT_TOOL_NAME, REVIEW_BACKLOG_DELIVERY_TOOL_NAME, SYSTEM_STATUS_TOOL_NAME,
        UPDATE_AGENT_TOOL_NAME, UPDATE_DELEGATION_TOOL_NAME, UPDATE_PROGRAM_STATE_TOOL_NAME,
        WORK_LEDGER_TOOL_NAME,
    },
    resource_authority::gated_action::MaybeGatedAction,
    strategy::plan::PlanStep,
};

#[derive(Clone)]
pub struct HarnessCapabilityProvider {
    tool_name: &'static str,
    services: HarnessServices,
    pack_def: Option<CapabilityPackDefinition>,
}

impl std::fmt::Debug for HarnessCapabilityProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HarnessCapabilityProvider")
            .field("tool_name", &self.tool_name)
            .finish()
    }
}

impl HarnessCapabilityProvider {
    pub fn new(tool_name: &'static str, services: HarnessServices) -> Self {
        Self {
            tool_name,
            services,
            pack_def: None,
        }
    }

    pub fn with_pack_def(mut self, pack_def: CapabilityPackDefinition) -> Self {
        self.pack_def = Some(pack_def);
        self
    }

    fn tool_name_static(&self) -> &'static str {
        self.tool_name
    }

    fn lower_harness_action(
        &self,
        resolved_params: HashMap<String, Value>,
    ) -> Result<MaybeGatedAction, ExecutionError> {
        let action = ExecutableAction::Pack {
            capability_name: self.tool_name.to_string(),
            implementation: ImplementationType::Compiled {
                provider_name: self.tool_name.to_string(),
            },
            resolved_params: resolved_params.clone(),
        };
        Ok(super::pack_provider::maybe_wrap_with_spend_gate(
            action,
            self.pack_def.as_ref(),
            &resolved_params,
        ))
    }
}

#[async_trait]
impl CapabilityProvider for HarnessCapabilityProvider {
    fn tool_name(&self) -> &str {
        self.tool_name
    }

    fn lower(&self, step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
        let resolved_params = if let Some(pack_def) = &self.pack_def {
            pack_def.resolve_params(&step.parameters)?
        } else {
            step.parameters.clone()
        };
        self.lower_harness_action(resolved_params)
    }

    async fn execute(
        &self,
        action: &ExecutableAction,
        _session_id: Option<String>,
        _timeout_secs: u64,
    ) -> Result<ActionResult, ExecutionError> {
        let params = match action {
            ExecutableAction::Pack {
                resolved_params, ..
            } => resolved_params,
            _ => {
                return Err(ExecutionError::Step(format!(
                    "{}: unexpected action type",
                    self.tool_name
                )))
            },
        };

        let context = HarnessExecutionContext::from_params(&self.services, params)
            .await
            .map_err(|error| ExecutionError::Step(format!("{}: {error}", self.tool_name)))?;

        let response = match self.tool_name_static() {
            LIST_EPISODES_TOOL_NAME => {
                execute_list_episodes(&self.services, &context, params).await?
            },
            READ_TRACE_TOOL_NAME => execute_read_trace(&self.services, &context, params).await?,
            LIST_AGENTS_TOOL_NAME => execute_list_agents(&self.services, &context).await?,
            INSPECT_AGENT_TOOL_NAME => {
                execute_inspect_agent(&self.services, &context, params).await?
            },
            INSPECT_BACKLOG_DELIVERY_TOOL_NAME => {
                execute_inspect_backlog_delivery(&self.services, &context, params).await?
            },
            SYSTEM_STATUS_TOOL_NAME => {
                execute_system_status(&self.services, &context, params).await?
            },
            EVALUATE_HARNESS_TOOL_NAME => {
                execute_evaluate_harness(&self.services, &context, params).await?
            },
            READ_PROGRAM_STATE_TOOL_NAME => {
                execute_read_program_state(&self.services, &context).await?
            },
            LIST_PROPOSALS_TOOL_NAME => {
                execute_list_proposals(&self.services, &context, params).await?
            },
            WORK_LEDGER_TOOL_NAME => execute_work_ledger(&self.services, &context, params).await?,
            CREATE_TASK_TOOL_NAME => execute_create_task(&self.services, &context, params).await?,
            REASSIGN_TASK_TOOL_NAME => {
                execute_reassign_task(&self.services, &context, params).await?
            },
            CREATE_AGENT_TOOL_NAME => {
                execute_create_agent(&self.services, &context, params).await?
            },
            UPDATE_AGENT_TOOL_NAME => {
                execute_update_agent(&self.services, &context, params).await?
            },
            RETIRE_AGENT_TOOL_NAME => {
                execute_retire_agent(&self.services, &context, params).await?
            },
            UPDATE_DELEGATION_TOOL_NAME => {
                execute_update_delegation(&self.services, &context, params).await?
            },
            CREATE_PROPOSAL_TOOL_NAME => {
                execute_create_proposal(&self.services, &context, params).await?
            },
            CREATE_DASHBOARD_TOOL_NAME => {
                execute_create_dashboard(&self.services, &context, params).await?
            },
            UPDATE_PROGRAM_STATE_TOOL_NAME => {
                execute_update_program_state(&self.services, &context, params).await?
            },
            PROPOSE_BACKLOG_ITEM_TOOL_NAME => {
                execute_propose_backlog_item(&self.services, &context, params).await?
            },
            PROMOTE_BACKLOG_ITEM_TOOL_NAME => {
                execute_promote_backlog_item(&self.services, &context, params).await?
            },
            REVIEW_BACKLOG_DELIVERY_TOOL_NAME => {
                execute_review_backlog_delivery(&self.services, &context, params).await?
            },
            other => {
                return Err(ExecutionError::Step(format!(
                    "unsupported harness tool `{other}`"
                )))
            },
        };

        Ok(ActionResult::text(
            serde_json::to_string_pretty(&response)
                .map_err(|error| ExecutionError::Step(format!("{}: {error}", self.tool_name)))?,
        ))
    }
}

#[derive(Debug, Clone, Serialize)]
struct HarnessContextSummary {
    owner_agent_id: String,
    principal: String,
    workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    goal_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    focus_area: Option<String>,
    scope_agent_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    program: Option<ProgramSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    program_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ProgramSummary {
    relative_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    section: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    state_relative_path: String,
}

#[derive(Debug, Clone, Serialize)]
struct EpisodeListEntry {
    agent_id: String,
    episode_id: String,
    goal_id: String,
    record_type: String,
    started_at: String,
    completed_at: String,
    outcome_kind: String,
    outcome_summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task_title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
struct TaskStats {
    total: usize,
    pending: usize,
    planning: usize,
    ready: usize,
    running: usize,
    paused: usize,
    completed: usize,
    failed: usize,
    cancelled: usize,
    deferred: usize,
}

#[derive(Debug, Clone, Serialize)]
struct EpisodeStats {
    total: usize,
    successful: usize,
    failed: usize,
    paused: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_episode_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_outcome: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct AgentSummary {
    #[serde(flatten)]
    target: HarnessScopeTarget,
    status: String,
    active_cycles: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    active_goal_ids: Vec<String>,
    task_stats: TaskStats,
    episode_stats: EpisodeStats,
}

#[derive(Debug, Clone, Serialize)]
struct ActiveCycleSummary {
    cycle_id: String,
    goal_id: String,
    trigger: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    execution_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct TaskPreview {
    task_id: String,
    title: String,
    status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    priority: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    due_date: Option<String>,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
struct SystemStatusEntry {
    agent_id: String,
    agent_name: String,
    status: String,
    active_cycles: usize,
    task_stats: TaskStats,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pending_tasks: Vec<TaskPreview>,
}

#[derive(Debug, Clone, Serialize)]
struct FocusAreaEvaluationEntry {
    goal_id: String,
    name: String,
    description: String,
    priority: String,
    episode_stats: EpisodeStats,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    success_rate: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Default)]
struct EpisodeImpactStats {
    total: usize,
    completed: usize,
    successful: usize,
    failed: usize,
    paused: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    success_rate: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
struct ProposalImpactEntry {
    proposal_id: String,
    agent_id: String,
    status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    action_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    applied_at: Option<String>,
    impact_window_days: i64,
    window_complete: bool,
    before: EpisodeImpactStats,
    after: EpisodeImpactStats,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    success_rate_delta: Option<f64>,
}

#[derive(Debug, Clone)]
struct ProgramMetricSpec {
    metric_id: String,
    comparator: &'static str,
    target: f64,
}

#[derive(Debug, Clone, Serialize)]
struct ProgramMetricEvaluation {
    metric_id: String,
    comparator: String,
    target: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current: Option<f64>,
    unit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    met: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ProgramMetricsDocument {
    relative_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    section: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    metrics: Vec<ProgramMetricEvaluation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    parse_errors: Vec<String>,
    /// Declared metric ids the harness does not know how to evaluate (the
    /// `evaluate_program_metric` fallthrough). Surfaced explicitly so a program
    /// author sees an unrecognized Success-Metric instead of a silently
    /// undecorated (`current: n/a`) line.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    unsupported_metric_ids: Vec<String>,
}

async fn execute_list_episodes(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let selected_agent_ids = selected_scope_agents(context, params.get("agent_id"))?;
    let goal_id = optional_string(params, "goal_id");
    let outcome = optional_string(params, "outcome");
    let since_hours = optional_i64(params, "since_hours").unwrap_or(24);
    let limit = optional_usize(params, "limit").unwrap_or(20).min(100);
    let since_cutoff = if since_hours > 0 {
        Some(Utc::now() - Duration::hours(since_hours))
    } else {
        None
    };

    let memory = context.memory_service(services);
    let mut entries = Vec::new();
    for agent_id in selected_agent_ids {
        let episodes = if let Some(goal_id) = goal_id.as_deref() {
            memory
                .load_native_episodes_for_goal(&agent_id, goal_id)
                .await
                .map_err(|error| step_error(LIST_EPISODES_TOOL_NAME, error))?
        } else {
            memory
                .load_native_episodes(&agent_id)
                .await
                .map_err(|error| step_error(LIST_EPISODES_TOOL_NAME, error))?
        };
        for episode in episodes {
            if !matches_episode_filters(&episode, outcome.as_deref(), since_cutoff.as_ref()) {
                continue;
            }
            entries.push(EpisodeListEntry {
                agent_id: episode.agent_id.clone(),
                episode_id: episode.episode_id.clone(),
                goal_id: episode.goal_id().to_string(),
                record_type: episode.record_type.clone(),
                started_at: episode.started_at.clone(),
                completed_at: episode.completed_at.clone(),
                outcome_kind: episode.outcome_kind.clone(),
                outcome_summary: episode.outcome_summary.clone(),
                task_id: episode.task_id.clone(),
                execution_id: episode.execution_id.clone(),
                task_title: episode.task_title.clone(),
            });
        }
    }

    entries.sort_by(|left, right| {
        right
            .completed_at
            .cmp(&left.completed_at)
            .then_with(|| right.episode_id.cmp(&left.episode_id))
    });
    let total_count = entries.len();
    entries.truncate(limit);

    Ok(serde_json::json!({
        "context": build_context_summary(services, context).await,
        "filters": {
            "agent_id": optional_string(params, "agent_id"),
            "goal_id": goal_id,
            "outcome": outcome,
            "since_hours": since_hours,
            "limit": limit,
        },
        "total_count": total_count,
        "episodes": entries,
    }))
}

async fn execute_read_trace(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let episode_id = required_param_string(params, "episode_id", READ_TRACE_TOOL_NAME)?;
    let selected_agent_ids = selected_scope_agents(context, params.get("agent_id"))?;
    let output_max_chars = optional_usize(params, "output_max_chars").unwrap_or(20_000);
    let memory = context.memory_service(services);

    let mut matched: Option<V3EpisodeRecord> = None;
    for agent_id in selected_agent_ids {
        let episodes = memory
            .load_native_episodes(&agent_id)
            .await
            .map_err(|error| step_error(READ_TRACE_TOOL_NAME, error))?;
        if let Some(episode) = episodes
            .into_iter()
            .find(|episode| episode.episode_id == episode_id)
        {
            matched = Some(episode);
            break;
        }
    }

    let episode = matched.ok_or_else(|| {
        ExecutionError::Step(format!(
            "{}: episode `{episode_id}` was not found in the active harness scope",
            READ_TRACE_TOOL_NAME
        ))
    })?;

    let trace = HarnessTraceReader::new(services.artifact_service.workspace().clone())
        .load_trace(episode, output_max_chars)
        .await
        .map_err(|error| ExecutionError::Step(format!("{READ_TRACE_TOOL_NAME}: {error}")))?;

    Ok(serde_json::json!({
        "context": build_context_summary(services, context).await,
        "trace": trace,
    }))
}

async fn execute_list_agents(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
) -> Result<Value, ExecutionError> {
    let scope_ref = ScopeRef::system_internal_unauthenticated(
        &context.principal.clone(),
        &context.workspace.clone(),
    );
    let tasks = services
        .artifact_service
        .list_tasks(&scope_ref)
        .await
        .map_err(|error| step_error(LIST_AGENTS_TOOL_NAME, error))?;
    let tasks_by_agent = group_tasks_by_agent(tasks);
    let memory = context.memory_service(services);

    let mut agents = Vec::new();
    for target in context.scope.targets_summary() {
        let task_stats =
            TaskStats::from_tasks(tasks_by_agent.get(&target.agent_id).map(Vec::as_slice));
        let active_cycles = services
            .runtime
            .active_cycles_for_agent_in_scope(
                &context.principal,
                &context.workspace,
                &target.agent_id,
            )
            .await;
        let paused = is_harness_agent_paused(
            services,
            &context.principal,
            &context.workspace,
            &target.agent_id,
        )
        .await;
        let episodes = memory
            .load_native_episodes(&target.agent_id)
            .await
            .map_err(|error| step_error(LIST_AGENTS_TOOL_NAME, error))?;
        let episode_stats = summarize_episodes(&episodes);
        agents.push(AgentSummary {
            status: derive_agent_status(paused, active_cycles.len(), &task_stats).to_string(),
            active_goal_ids: active_cycles
                .iter()
                .map(|reservation| reservation.goal_id.clone())
                .collect(),
            active_cycles: active_cycles.len(),
            task_stats,
            episode_stats,
            target,
        });
    }

    Ok(serde_json::json!({
        "context": build_context_summary(services, context).await,
        "total_count": agents.len(),
        "agents": agents,
    }))
}

async fn execute_inspect_agent(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let agent_id = required_param_string(params, "agent_id", INSPECT_AGENT_TOOL_NAME)?;
    context
        .scope
        .require_contains(&agent_id)
        .map_err(|error| ExecutionError::Step(format!("{INSPECT_AGENT_TOOL_NAME}: {error}")))?;

    let scoped_store = context.scoped_definition_store(services);
    let record = scoped_store
        .get_definition(&agent_id)
        .await
        .map_err(|error| step_error(INSPECT_AGENT_TOOL_NAME, error))?
        .ok_or_else(|| {
            ExecutionError::Step(format!(
                "{INSPECT_AGENT_TOOL_NAME}: agent `{agent_id}` was not found in scope"
            ))
        })?;

    let scope_ref = ScopeRef::system_internal_unauthenticated(
        &context.principal.clone(),
        &context.workspace.clone(),
    );
    let tasks = services
        .artifact_service
        .list_tasks(&scope_ref)
        .await
        .map_err(|error| step_error(INSPECT_AGENT_TOOL_NAME, error))?;
    let mut agent_tasks = tasks
        .into_iter()
        .filter(|task| task.agent_id == agent_id)
        .collect::<Vec<_>>();
    agent_tasks.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| right.id.cmp(&left.id))
    });
    let task_stats = TaskStats::from_tasks(Some(agent_tasks.as_slice()));
    let recent_tasks = agent_tasks
        .iter()
        .take(10)
        .map(task_preview)
        .collect::<Vec<_>>();

    let active_cycles = services
        .runtime
        .active_cycles_for_agent_in_scope(&context.principal, &context.workspace, &agent_id)
        .await;
    let paused =
        is_harness_agent_paused(services, &context.principal, &context.workspace, &agent_id).await;
    let active_cycle_summaries = active_cycles
        .iter()
        .map(|reservation| ActiveCycleSummary {
            cycle_id: reservation.cycle_id.clone(),
            goal_id: reservation.goal_id.clone(),
            trigger: reservation.trigger.clone(),
            execution_id: reservation.execution_id.clone(),
        })
        .collect::<Vec<_>>();
    let memory = context.memory_service(services);
    let mut episodes = memory
        .load_native_episodes(&agent_id)
        .await
        .map_err(|error| step_error(INSPECT_AGENT_TOOL_NAME, error))?;
    episodes.sort_by(|left, right| {
        right
            .completed_at
            .cmp(&left.completed_at)
            .then_with(|| right.episode_id.cmp(&left.episode_id))
    });
    let recent_episodes = episodes
        .iter()
        .take(
            optional_usize(params, "recent_episode_limit")
                .unwrap_or(10)
                .min(20),
        )
        .map(|episode| {
            serde_json::json!({
                "episode_id": episode.episode_id,
                "goal_id": episode.goal_id(),
                "record_type": episode.record_type,
                "completed_at": episode.completed_at,
                "outcome_kind": episode.outcome_kind,
                "outcome_summary": episode.outcome_summary,
                "task_id": episode.task_id,
                "execution_id": episode.execution_id,
            })
        })
        .collect::<Vec<_>>();
    let definition_yaml = record
        .definition
        .to_yaml_string()
        .map_err(|error| step_error(INSPECT_AGENT_TOOL_NAME, error))?;

    Ok(serde_json::json!({
        "context": build_context_summary(services, context).await,
        "agent": {
            "definition": &record.definition,
            "definition_yaml": definition_yaml,
            "version": record.version(),
            "created_at": &record.created_at,
            "updated_at": &record.updated_at,
        },
        "status": {
            "derived_status": derive_agent_status(paused, active_cycles.len(), &task_stats),
            "active_cycles": active_cycle_summaries,
            "task_stats": task_stats,
            "recent_tasks": recent_tasks,
            "recent_episodes": recent_episodes,
            "episode_stats": summarize_episodes(&episodes),
        }
    }))
}

async fn execute_system_status(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let selected_agent_ids = selected_scope_agents(context, params.get("agent_id"))?;
    let scope_ref = ScopeRef::system_internal_unauthenticated(
        &context.principal.clone(),
        &context.workspace.clone(),
    );
    let tasks = services
        .artifact_service
        .list_tasks(&scope_ref)
        .await
        .map_err(|error| step_error(SYSTEM_STATUS_TOOL_NAME, error))?;
    let tasks_by_agent = group_tasks_by_agent(tasks);

    let include_tasks = optional_bool(params, "include_tasks").unwrap_or(true);
    let pending_task_limit = optional_usize(params, "pending_task_limit")
        .unwrap_or(10)
        .min(25);

    let mut statuses = Vec::new();
    for target in context.scope.targets_summary() {
        if !selected_agent_ids
            .iter()
            .any(|agent_id| agent_id == &target.agent_id)
        {
            continue;
        }
        let agent_tasks = tasks_by_agent
            .get(&target.agent_id)
            .cloned()
            .unwrap_or_default();
        let task_stats = TaskStats::from_tasks(Some(agent_tasks.as_slice()));
        let active_cycles = services
            .runtime
            .active_cycles_for_agent_in_scope(
                &context.principal,
                &context.workspace,
                &target.agent_id,
            )
            .await;
        let paused = is_harness_agent_paused(
            services,
            &context.principal,
            &context.workspace,
            &target.agent_id,
        )
        .await;
        let pending_tasks = if include_tasks {
            agent_tasks
                .iter()
                .filter(|task| !matches!(task.status.as_str(), "completed" | "cancelled"))
                .take(pending_task_limit)
                .map(task_preview)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        statuses.push(SystemStatusEntry {
            agent_id: target.agent_id,
            agent_name: target.name,
            status: derive_agent_status(paused, active_cycles.len(), &task_stats).to_string(),
            active_cycles: active_cycles.len(),
            task_stats,
            pending_tasks,
        });
    }

    Ok(serde_json::json!({
        "context": build_context_summary(services, context).await,
        "total_count": statuses.len(),
        "agents": statuses,
    }))
}

async fn execute_evaluate_harness(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let proposal_limit = optional_usize(params, "proposal_limit")
        .unwrap_or(20)
        .min(50);
    let impact_window_days = optional_i64(params, "impact_window_days")
        .unwrap_or(7)
        .clamp(1, 90);

    let memory = context.memory_service(services);
    let owner_agent_id = context.owner_record.definition.agent_id.clone();
    let owner_episodes = memory
        .load_native_episodes(&owner_agent_id)
        .await
        .map_err(|error| step_error(EVALUATE_HARNESS_TOOL_NAME, error))?;

    let scope_ref = ScopeRef::system_internal_unauthenticated(
        &context.principal.clone(),
        &context.workspace.clone(),
    );
    let scope_tasks = services
        .artifact_service
        .list_tasks(&scope_ref)
        .await
        .map_err(|error| step_error(EVALUATE_HARNESS_TOOL_NAME, error))?;
    let scoped_store = context.scoped_definition_store(services);
    let proposal_store = ProposalStore::new(scoped_store.storage().clone());
    let all_proposals = proposal_store
        .list_proposals(ProposalFilter::default())
        .await
        .map_err(|error| step_error(EVALUATE_HARNESS_TOOL_NAME, error))?;
    let harness_source = proposal_source(context);

    let focus_areas = context
        .owner_record
        .definition
        .autonomous_config
        .as_ref()
        .map(|config| {
            config
                .focus_areas
                .iter()
                .map(|focus_area| {
                    let goal_id = focus_area_goal_id(&owner_agent_id, focus_area);
                    let episodes = owner_episodes
                        .iter()
                        .filter(|episode| episode.goal_id() == goal_id)
                        .cloned()
                        .collect::<Vec<_>>();
                    let episode_stats = summarize_episodes(&episodes);
                    FocusAreaEvaluationEntry {
                        goal_id,
                        name: focus_area.name.clone(),
                        description: focus_area.description.clone(),
                        priority: focus_area_priority_label(&focus_area.priority).to_string(),
                        success_rate: episode_success_rate(&episodes),
                        episode_stats,
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut applied_proposals = all_proposals
        .iter()
        .filter(|proposal| proposal.source == harness_source)
        .filter(|proposal| proposal.applied_at.is_some())
        .cloned()
        .collect::<Vec<_>>();
    applied_proposals.sort_by(|left, right| {
        right
            .applied_at
            .cmp(&left.applied_at)
            .then_with(|| right.created_at.cmp(&left.created_at))
    });
    let total_applied_proposals = applied_proposals.len();
    applied_proposals.truncate(proposal_limit);

    let mut proposal_impacts = Vec::new();
    for proposal in &applied_proposals {
        let target_episodes = memory
            .load_native_episodes(&proposal.agent_id)
            .await
            .map_err(|error| step_error(EVALUATE_HARNESS_TOOL_NAME, error))?;
        proposal_impacts.push(build_proposal_impact_entry(
            proposal,
            &target_episodes,
            impact_window_days,
        )?);
    }

    let program_documents = load_program_metric_documents(
        services,
        context,
        &owner_episodes,
        &scope_tasks,
        &all_proposals,
    )
    .await
    .map_err(|error| step_error(EVALUATE_HARNESS_TOOL_NAME, error))?;
    let total_program_metrics = program_documents
        .iter()
        .map(|document| document.metrics.len())
        .sum::<usize>();
    let met_program_metrics = program_documents
        .iter()
        .flat_map(|document| document.metrics.iter())
        .filter(|metric| metric.met == Some(true))
        .count();
    let active_program_state = match context
        .load_program(services)
        .await
        .map_err(|error| step_error(EVALUATE_HARNESS_TOOL_NAME, error))?
    {
        Some(program) => Some(
            context
                .load_program_runtime_state(services, &program)
                .await
                .map_err(|error| step_error(EVALUATE_HARNESS_TOOL_NAME, error))?,
        ),
        None => None,
    };

    Ok(serde_json::json!({
        "context": build_context_summary(services, context).await,
        "focus_areas": focus_areas,
        "active_program_state": active_program_state,
        "proposal_impact": {
            "total_count": total_applied_proposals,
            "window_days": impact_window_days,
            "proposals": proposal_impacts,
        },
        "program_metrics": {
            "document_count": program_documents.len(),
            "total_metrics": total_program_metrics,
            "met_metrics": met_program_metrics,
            "documents": program_documents,
        }
    }))
}

async fn execute_read_program_state(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
) -> Result<Value, ExecutionError> {
    let Some(program) = context
        .load_program(services)
        .await
        .map_err(|error| step_error(READ_PROGRAM_STATE_TOOL_NAME, error))?
    else {
        return Ok(serde_json::json!({
            "context": build_context_summary(services, context).await,
            "program": null,
            "runtime_state": null,
            "reason": "no active program document was found for this harness context",
        }));
    };
    let runtime_state = context
        .load_program_runtime_state(services, &program)
        .await
        .map_err(|error| step_error(READ_PROGRAM_STATE_TOOL_NAME, error))?;

    // Boundary D. Supplemental guidance is versioned operating guidance for
    // this program — never its authority — and it is read here because a
    // versioned artifact nothing reads is decoration. A program with no
    // guidance renders nothing rather than an empty heading, which would
    // train a cycle to skim the section that will one day matter.
    let profile = crate::magician_v2::harness::SupplementalProfileStore::new(
        services.artifact_service.workspace().clone(),
    )
    .load(
        &context.principal,
        &context.workspace,
        &program.relative_path,
    );
    let supplemental_guidance = profile.render_block().map(|block| {
        serde_json::json!({
            "revision": profile.revision,
            "block": block,
        })
    });

    Ok(serde_json::json!({
        "context": build_context_summary(services, context).await,
        "program": {
            "relative_path": program.relative_path,
            "section": program.section,
            "title": program.title,
        },
        "runtime_state": runtime_state,
        "supplemental_guidance": supplemental_guidance,
    }))
}

async fn execute_list_proposals(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let agent_id = optional_string(params, "agent_id");

    let status = optional_string(params, "status")
        .map(|value| parse_proposal_status(&value))
        .transpose()?;
    let scoped_store = context.scoped_definition_store(services);
    let proposal_store =
        crate::magician_v2::agents::ProposalStore::new(scoped_store.storage().clone());
    let mut proposals = proposal_store
        .list_proposals(ProposalFilter {
            agent_id: agent_id.clone(),
            status,
        })
        .await
        .map_err(|error| step_error(LIST_PROPOSALS_TOOL_NAME, error))?;

    let harness_source = proposal_source(context);
    proposals.retain(|proposal| {
        proposal_visible_to_harness(context, proposal, agent_id.as_deref(), &harness_source)
    });
    proposals.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| right.proposal_id.cmp(&left.proposal_id))
    });

    let limit = optional_usize(params, "limit").unwrap_or(50).min(100);
    let total_count = proposals.len();
    proposals.truncate(limit);

    Ok(serde_json::json!({
        "context": build_context_summary(services, context).await,
        "total_count": total_count,
        "proposals": proposals,
    }))
}

/// Compact, prompt-friendly projection of one `work_outcome` ledger record.
/// The producer-specific fields (`outcome` / `open_loops` / `agent_id`) are read
/// out of [`EvidenceRecord::metadata`], which is where
/// [`EvidenceRecord::from_work_outcome`] stamps them.
#[derive(Debug, Clone, PartialEq, Serialize)]
struct WorkLedgerEntry {
    evidence_id: String,
    summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    open_loops: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent_id: Option<String>,
    last_seen_at: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    artifact_refs: Vec<String>,
}

/// Read a string out of an evidence record's metadata bag.
fn metadata_string(metadata: &Value, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Read a string array out of an evidence record's metadata bag (skips
/// non-string / empty entries).
fn metadata_string_array(metadata: &Value, key: &str) -> Vec<String> {
    metadata
        .get(key)
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Parse an optional RFC3339 `since` bound into an instant. Accepts BOTH the
/// numeric-offset form (`...+00:00`, what chrono's `to_rfc3339` stamps into
/// `last_seen_at`) and the `Z` form (what the pack-def guide tells the LLM to
/// pass) — [`DateTime::parse_from_rfc3339`] handles both. A present-but-
/// unparseable `since` is a hard error so we never silently mis-filter.
fn parse_work_ledger_since(since: Option<&str>) -> Result<Option<DateTime<Utc>>, ExecutionError> {
    match since {
        Some(raw) => DateTime::parse_from_rfc3339(raw)
            .map(|dt| Some(dt.with_timezone(&Utc)))
            .map_err(|error| {
                ExecutionError::Step(format!(
                    "{WORK_LEDGER_TOOL_NAME}: `since` must be an RFC3339 timestamp \
                     (e.g. `2026-06-13T09:00:00Z`); got `{raw}`: {error}"
                ))
            }),
        None => Ok(None),
    }
}

/// Whether a record's `last_seen_at` is at/after the (already-parsed) `since`
/// bound. Compares INSTANTS, not strings, so a `Z`-form `since` correctly
/// filters records stored in `+00:00` form (and vice versa). Mirrors
/// `matches_episode_filters`: a record whose stored timestamp fails to parse is
/// conservatively EXCLUDED from a `since`-bounded query (stored `last_seen_at`
/// is always `to_rfc3339()`, so this should never happen in practice).
fn record_at_or_after_since(
    record: &crate::magician_v2::evidence::EvidenceRecord,
    since_cutoff: Option<&DateTime<Utc>>,
) -> bool {
    match since_cutoff {
        Some(cutoff) => DateTime::parse_from_rfc3339(&record.last_seen_at)
            .map(|ts| ts.with_timezone(&Utc) >= *cutoff)
            .unwrap_or(false),
        None => true,
    }
}

/// Filter, sort (newest-first by `last_seen_at`), and project a set of loaded
/// evidence records into compact work-ledger entries. Pure so the filtering
/// contract is unit-testable without a harness-services harness.
///
/// - `producer == "work_outcome"` is always required (this is a direct ledger
///   read, never a semantic lane).
/// - `kind`, when provided, matches the metadata `outcome` (terminal outcome:
///   `success` | `failed` | `cannot_proceed` | `yield`).
/// - `since`, when provided, is an inclusive RFC3339 lower bound on
///   `last_seen_at`, compared as an INSTANT (accepts both `Z` and `+00:00`
///   forms; a present-but-unparseable `since` is a hard error).
/// - `limit` is clamped to `[1, WORK_LEDGER_MAX_LIMIT]`.
fn select_work_ledger_records(
    records: Vec<crate::magician_v2::evidence::EvidenceRecord>,
    kind: Option<&str>,
    since: Option<&str>,
    limit: usize,
) -> Result<Vec<WorkLedgerEntry>, ExecutionError> {
    let limit = limit.clamp(1, WORK_LEDGER_MAX_LIMIT);
    let since_cutoff = parse_work_ledger_since(since)?;
    let mut entries = records
        .into_iter()
        .filter(|record| record.producer == "work_outcome")
        .filter(|record| record_at_or_after_since(record, since_cutoff.as_ref()))
        .filter(|record| match kind {
            Some(kind) => metadata_string(&record.metadata, "outcome").as_deref() == Some(kind),
            None => true,
        })
        .map(|record| WorkLedgerEntry {
            evidence_id: record.evidence_id.clone(),
            summary: record.summary.clone(),
            outcome: metadata_string(&record.metadata, "outcome"),
            open_loops: metadata_string_array(&record.metadata, "open_loops"),
            agent_id: metadata_string(&record.metadata, "agent_id"),
            last_seen_at: record.last_seen_at.clone(),
            artifact_refs: record.artifact_refs.clone(),
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        right
            .last_seen_at
            .cmp(&left.last_seen_at)
            .then_with(|| right.evidence_id.cmp(&left.evidence_id))
    });
    entries.truncate(limit);
    Ok(entries)
}

const WORK_LEDGER_DEFAULT_LIMIT: usize = 20;
const WORK_LEDGER_MAX_LIMIT: usize = 50;

const SOURCE_EXECUTION_TAG_PREFIX: &str = "agentic-source-execution:";
const SOURCE_TASK_TAG_PREFIX: &str = "agentic-source-task:";
/// `magician_work_ledger`: on-demand recall of prior work-outcomes. Reads the
/// per-agent evidence store for the selected scope agents and returns the
/// `work_outcome` ledger records (deterministically stamped by terminal agentic
/// runs) as compact JSON. Read-only — never mutates the store.
async fn execute_work_ledger(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let selected_agent_ids = selected_scope_agents(context, params.get("agent_id"))?;
    let kind = optional_string(params, "kind");
    let since = optional_string(params, "since");
    let limit = optional_usize(params, "limit")
        .unwrap_or(WORK_LEDGER_DEFAULT_LIMIT)
        .clamp(1, WORK_LEDGER_MAX_LIMIT);

    let memory = context.memory_service(services);
    let mut records = Vec::new();
    for agent_id in &selected_agent_ids {
        let mut agent_records = memory
            .load_native_evidence(agent_id)
            .await
            .map_err(|error| step_error(WORK_LEDGER_TOOL_NAME, error))?;
        records.append(&mut agent_records);
    }

    // Parse `since` once (instant, not string) so the total_count below and the
    // filtered set agree — and a bad `since` errors here instead of mis-filtering.
    let since_cutoff = parse_work_ledger_since(since.as_deref())?;

    // Total across the scope BEFORE clamping, for legibility (mirrors the other
    // read tools' `total_count`).
    let total_count = records
        .iter()
        .filter(|record| record.producer == "work_outcome")
        .filter(|record| record_at_or_after_since(record, since_cutoff.as_ref()))
        .filter(|record| match kind.as_deref() {
            Some(kind) => metadata_string(&record.metadata, "outcome").as_deref() == Some(kind),
            None => true,
        })
        .count();

    let entries = select_work_ledger_records(records, kind.as_deref(), since.as_deref(), limit)?;

    Ok(serde_json::json!({
        "context": build_context_summary(services, context).await,
        "filters": {
            "agent_id": optional_string(params, "agent_id"),
            "kind": kind,
            "since": since,
            "limit": limit,
        },
        "total_count": total_count,
        "records": entries,
    }))
}

async fn execute_create_task(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let agent_id = required_param_string(params, "agent_id", CREATE_TASK_TOOL_NAME)?;
    context
        .scope
        .require_contains(&agent_id)
        .map_err(|error| ExecutionError::Step(format!("{CREATE_TASK_TOOL_NAME}: {error}")))?;

    let title = required_param_string(params, "title", CREATE_TASK_TOOL_NAME)?;
    let description = required_param_string(params, "description", CREATE_TASK_TOOL_NAME)?;
    let priority = optional_string(params, "priority")
        .map(|value| parse_task_priority(&value, CREATE_TASK_TOOL_NAME))
        .transpose()?;
    let due_date = optional_string(params, "due_date");
    let goal_id = optional_string(params, "goal_id").or_else(|| context.goal_id.clone());
    let reference_task_ids =
        validated_reference_task_ids(services, context, params, CREATE_TASK_TOOL_NAME).await?;
    let output_mode = optional_string(params, "output_mode")
        .map(|value| parse_task_output_mode(&value, CREATE_TASK_TOOL_NAME))
        .transpose()?
        .unwrap_or(TaskOutputMode::Accumulate);
    let start_immediately = optional_bool(params, "start_immediately").unwrap_or(true);
    let lifecycle = harness_created_task_lifecycle(params);
    let task = create_harness_task_with_budget(
        services,
        context,
        CREATE_TASK_TOOL_NAME,
        CreateTaskInput {
            principal: context.principal.clone(),
            workspace: context.workspace.clone(),
            title,
            description,
            agent_id,
            goal_id,
            ui_thread_id: current_ui_thread_id(context),
            priority,
            due_date,
            tags: Vec::new(),
            created_by: harness_created_by(context),
            depends_on: reference_task_ids,
            approved: true,
            schedule: None,
            output_mode,
            chat_session_id: None,
            lifecycle,
            sync_mode: crate::magician_v2::artifact_v2::models::TaskSyncMode::default(),
        },
    )
    .await?;
    let (task, execution) = start_created_harness_task_if_requested(
        services,
        context,
        task,
        start_immediately,
        CREATE_TASK_TOOL_NAME,
    )
    .await?;

    Ok(serde_json::json!({
        "context": build_context_summary(services, context).await,
        "task": task,
        "execution": execution,
    }))
}

async fn create_harness_task_with_budget(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
    tool_name: &str,
    mut input: CreateTaskInput,
) -> Result<TaskRecord, ExecutionError> {
    let provenance_tags =
        harness_task_provenance_tags(context.task_id.as_deref(), context.execution_id.as_deref());
    let max_tasks_per_cycle = context
        .owner_record
        .definition
        .autonomous_config
        .as_ref()
        .map(|config| config.max_tasks_per_cycle);

    // Keep the count and write in one critical section. Artifact task creation
    // does not expose an atomic conditional-create API, so serializing harness
    // producers prevents concurrent tool calls from both consuming one slot.
    let _budget_guard = if let Some(max_tasks_per_cycle) = max_tasks_per_cycle {
        let guard = super::compiled_handlers::create_task::agentic_task_creation_gate()
            .lock()
            .await;
        if max_tasks_per_cycle == 0 {
            return Err(ExecutionError::Step(format!(
                "{tool_name}: autonomous_config.max_tasks_per_cycle task creation limit reached: 0/0"
            )));
        }
        let source_tag = harness_task_source_tag(
            context.task_id.as_deref(),
            context.execution_id.as_deref(),
        )
        .ok_or_else(|| {
            ExecutionError::Step(format!(
                "{tool_name}: autonomous_config.max_tasks_per_cycle is configured but cycle provenance (__execution_id or __task_id) is missing; cannot enforce the per-cycle task cap"
            ))
        })?;
        let tasks = services
            .artifact_service
            .list_tasks(&scope_ref(context))
            .await
            .map_err(|error| {
                step_error(
                    tool_name,
                    format!(
                        "could not enforce autonomous_config.max_tasks_per_cycle because task listing failed: {error}"
                    ),
                )
            })?;
        let task_count = count_tasks_for_source(&tasks, &source_tag);
        if task_count >= usize::try_from(max_tasks_per_cycle).unwrap_or(usize::MAX) {
            return Err(ExecutionError::Step(format!(
                "{tool_name}: autonomous_config.max_tasks_per_cycle task creation limit reached: {task_count}/{max_tasks_per_cycle} tasks already created for this execution"
            )));
        }
        Some(guard)
    } else {
        None
    };

    merge_task_tags(&mut input.tags, provenance_tags);
    services
        .artifact_service
        .create_task(input)
        .await
        .map_err(|error| step_error(tool_name, error))
}

fn harness_task_provenance_tags(
    source_task_id: Option<&str>,
    source_execution_id: Option<&str>,
) -> Vec<TaskTagRecord> {
    let mut tags = Vec::new();
    if let Some(tag) = harness_task_tag(SOURCE_EXECUTION_TAG_PREFIX, source_execution_id) {
        tags.push(tag);
    }
    if let Some(tag) = harness_task_tag(SOURCE_TASK_TAG_PREFIX, source_task_id) {
        tags.push(tag);
    }
    tags
}

fn harness_task_source_tag(
    source_task_id: Option<&str>,
    source_execution_id: Option<&str>,
) -> Option<String> {
    source_execution_id
        .and_then(|id| harness_task_tag_value(SOURCE_EXECUTION_TAG_PREFIX, id))
        .or_else(|| {
            source_task_id.and_then(|id| harness_task_tag_value(SOURCE_TASK_TAG_PREFIX, id))
        })
}

fn harness_task_tag(prefix: &str, value: Option<&str>) -> Option<TaskTagRecord> {
    let id = value.and_then(|value| harness_task_tag_value(prefix, value))?;
    Some(TaskTagRecord {
        id: id.clone(),
        name: id,
        color: None,
    })
}

fn harness_task_tag_value(prefix: &str, value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| format!("{prefix}{trimmed}"))
}

fn count_tasks_for_source(tasks: &[TaskListItemV3], source_tag: &str) -> usize {
    tasks
        .iter()
        .filter(|task| {
            task.tags
                .iter()
                .any(|tag| task_tag_matches(tag, source_tag))
        })
        .count()
}

fn task_tag_matches(tag: &TaskTagRecord, expected: &str) -> bool {
    tag.id == expected || tag.name == expected
}

fn merge_task_tags(existing: &mut Vec<TaskTagRecord>, additional: Vec<TaskTagRecord>) {
    for tag in additional {
        if !existing
            .iter()
            .any(|existing| task_tag_matches(existing, &tag.id))
        {
            existing.push(tag);
        }
    }
}

async fn start_created_harness_task_if_requested(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
    task: TaskRecord,
    start_immediately: bool,
    tool_name: &str,
) -> Result<(TaskRecord, Value), ExecutionError> {
    if !start_immediately {
        return Ok((
            task,
            json!({
                "status": "skipped",
                "reason": "start_immediately_false",
            }),
        ));
    }

    let scope = scope_ref(context);
    let task_id = task.manifest.task_id.clone();
    match Arc::clone(&services.artifact_service)
        .start_execution(scope.clone(), task_id.clone(), None, false)
        .await
    {
        Ok((started_task, execution)) => {
            let execution_summary = json!({
                "status": "started",
                "task_id": started_task.manifest.task_id.clone(),
                "execution_id": execution.state.execution_id.clone(),
                "task_status": started_task.state.status.clone(),
                "execution_status": execution.state.status.clone(),
            });
            Ok((started_task, execution_summary))
        },
        Err(error) => {
            if let Err(status_error) = services
                .artifact_service
                .update_task_status(&scope, &task_id, "failed")
                .await
            {
                tracing::warn!(
                    task_id = %task_id,
                    error = %status_error,
                    "Harness task auto-start failed and task status could not be marked failed"
                );
            }
            Err(step_error(
                tool_name,
                format!(
                    "created task `{}` but failed to start execution: {error}",
                    task_id
                ),
            ))
        },
    }
}

async fn execute_reassign_task(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let task_id = required_param_string(params, "task_id", REASSIGN_TASK_TOOL_NAME)?;
    if context.task_id.as_deref() == Some(task_id.as_str()) {
        return Err(ExecutionError::Step(format!(
            "{REASSIGN_TASK_TOOL_NAME}: cannot reassign the active harness task"
        )));
    }

    let new_agent_id = required_param_string(params, "agent_id", REASSIGN_TASK_TOOL_NAME)?;
    context
        .scope
        .require_contains(&new_agent_id)
        .map_err(|error| ExecutionError::Step(format!("{REASSIGN_TASK_TOOL_NAME}: {error}")))?;

    let scope = scope_ref(context);
    let task = services
        .artifact_service
        .get_task(&scope, &task_id)
        .await
        .map_err(|error| step_error(REASSIGN_TASK_TOOL_NAME, error))?;
    if matches!(task.state.status.as_str(), "completed" | "cancelled") {
        return Err(ExecutionError::Step(format!(
            "{REASSIGN_TASK_TOOL_NAME}: task `{task_id}` is already {}",
            task.state.status
        )));
    }
    context
        .scope
        .require_contains(&task.manifest.agent_id)
        .map_err(|error| ExecutionError::Step(format!("{REASSIGN_TASK_TOOL_NAME}: {error}")))?;
    if task.manifest.agent_id == new_agent_id {
        return Err(ExecutionError::Step(format!(
            "{REASSIGN_TASK_TOOL_NAME}: task `{task_id}` is already assigned to `{new_agent_id}`"
        )));
    }

    let title = optional_string(params, "title").unwrap_or_else(|| task.manifest.title.clone());
    let reason = optional_string(params, "reason");
    let description =
        build_reassignment_description(params, &task, context, &new_agent_id, reason.as_deref());
    let priority = optional_string(params, "priority")
        .map(|value| parse_task_priority(&value, REASSIGN_TASK_TOOL_NAME))
        .transpose()?
        .or_else(|| task.manifest.priority.clone());
    let due_date = optional_string(params, "due_date").or_else(|| task.manifest.due_date.clone());

    let new_task = create_harness_task_with_budget(
        services,
        context,
        REASSIGN_TASK_TOOL_NAME,
        CreateTaskInput {
            principal: context.principal.clone(),
            workspace: context.workspace.clone(),
            title,
            description,
            agent_id: new_agent_id,
            goal_id: task.manifest.goal_id.clone(),
            ui_thread_id: task.manifest.ui_thread_id.clone(),
            priority,
            due_date,
            tags: task.manifest.tags.clone(),
            created_by: harness_created_by(context),
            depends_on: task.manifest.depends_on.clone(),
            approved: task.manifest.approved,
            schedule: task.manifest.schedule.clone(),
            output_mode: task.manifest.output_mode.clone(),
            chat_session_id: None,
            // Reassignment changes ownership, not audience. In particular, an
            // internal harness job must not leak into the user's task list.
            lifecycle: task.manifest.lifecycle.clone(),
            sync_mode: crate::magician_v2::artifact_v2::models::TaskSyncMode::default(),
        },
    )
    .await?;
    let cancelled_task = match services
        .artifact_service
        .update_task_status(&scope, &task_id, "cancelled")
        .await
    {
        Ok(cancelled_task) => cancelled_task,
        Err(error) => {
            let rollback_error = services
                .artifact_service
                .update_task_status(&scope, &new_task.manifest.task_id, "cancelled")
                .await
                .err();
            let rollback_suffix = rollback_error
                .map(|rollback| format!("; rollback also failed: {rollback}"))
                .unwrap_or_default();
            return Err(step_error(
                REASSIGN_TASK_TOOL_NAME,
                format!(
                    "failed to cancel original task `{task_id}` after creating reassignment `{}`: {error}{rollback_suffix}",
                    new_task.manifest.task_id
                ),
            ));
        },
    };

    Ok(serde_json::json!({
        "context": build_context_summary(services, context).await,
        "cancelled_task": cancelled_task,
        "new_task": new_task,
        "reason": reason,
    }))
}

async fn execute_create_proposal(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let agent_id = required_param_string(params, "agent_id", CREATE_PROPOSAL_TOOL_NAME)?;
    if agent_id == context.owner_record.definition.agent_id {
        return Err(ExecutionError::Step(format!(
            "{CREATE_PROPOSAL_TOOL_NAME}: harness agents cannot create self-targeting proposals"
        )));
    }
    context
        .scope
        .require_contains(&agent_id)
        .map_err(|error| ExecutionError::Step(format!("{CREATE_PROPOSAL_TOOL_NAME}: {error}")))?;

    let summary = required_param_string(params, "summary", CREATE_PROPOSAL_TOOL_NAME)?;
    let yaml_after = required_param_string(params, "yaml_after", CREATE_PROPOSAL_TOOL_NAME)?;
    let parsed_yaml_after = AgentDefinition::from_yaml_str(&yaml_after).map_err(|error| {
        step_error(
            CREATE_PROPOSAL_TOOL_NAME,
            format!("`yaml_after` must deserialize to a full agent definition: {error}"),
        )
    })?;
    let scoped_store = context.scoped_definition_store(services);
    validate_harness_structural_definition(
        context,
        &scoped_store,
        CREATE_PROPOSAL_TOOL_NAME,
        &agent_id,
        &parsed_yaml_after,
    )
    .await?;
    let rationale = optional_string(params, "rationale");
    let mut payload =
        optional_object(params, "payload", CREATE_PROPOSAL_TOOL_NAME)?.unwrap_or_default();
    payload.insert("summary".to_string(), Value::String(summary.clone()));
    if let Some(rationale) = rationale.clone() {
        payload.insert("rationale".to_string(), Value::String(rationale));
    }
    payload.insert(
        "owner_agent_id".to_string(),
        Value::String(context.owner_record.definition.agent_id.clone()),
    );
    if let Some(goal_id) = context.goal_id.clone() {
        payload.insert("goal_id".to_string(), Value::String(goal_id));
    }
    if let Some(task_id) = context.task_id.clone() {
        payload.insert("task_id".to_string(), Value::String(task_id));
    }
    if let Some(focus_area) = context.scope.focus_area_name() {
        payload.insert(
            "focus_area".to_string(),
            Value::String(focus_area.to_string()),
        );
    }

    let _control_guard = match &services.control_gate {
        Some(control_gate) => Some(control_gate.lock().await),
        None => None,
    };

    let record = scoped_store
        .get_definition(&agent_id)
        .await
        .map_err(|error| step_error(CREATE_PROPOSAL_TOOL_NAME, error))?
        .ok_or_else(|| {
            ExecutionError::Step(format!(
                "{CREATE_PROPOSAL_TOOL_NAME}: agent `{agent_id}` was not found in scope"
            ))
        })?;
    let yaml_before = record
        .definition
        .to_yaml_string()
        .map_err(|error| step_error(CREATE_PROPOSAL_TOOL_NAME, error))?;
    let proposal = ProposalStore::new(scoped_store.storage().clone())
        .create_proposal(NewDefinitionProposal {
            agent_id,
            source: proposal_source(context),
            payload: Value::Object(payload),
            yaml_before,
            yaml_after,
        })
        .await
        .map_err(|error| step_error(CREATE_PROPOSAL_TOOL_NAME, error))?;

    Ok(serde_json::json!({
        "context": build_context_summary(services, context).await,
        "proposal": proposal,
        "summary": summary,
    }))
}

async fn execute_propose_backlog_item(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let title = required_param_string(params, "title", PROPOSE_BACKLOG_ITEM_TOOL_NAME)?;
    let description = required_param_string(params, "description", PROPOSE_BACKLOG_ITEM_TOOL_NAME)?;
    // Case-insensitive like parse_task_priority — the schema enum is only a
    // hint, not enforced at execution time, so an LLM may emit "High"/"HIGH".
    let priority = match optional_string(params, "priority")
        .map(|p| p.to_ascii_lowercase())
        .as_deref()
    {
        Some("high") => BacklogPriority::High,
        Some("low") => BacklogPriority::Low,
        _ => BacklogPriority::Medium,
    };
    let source_agent = &context.owner_record.definition.agent_id;
    let item = BacklogItem::new(
        &context.principal,
        &context.workspace,
        source_agent,
        &title,
        &description,
        priority,
    );
    let store = BacklogStore::new(services.artifact_service.workspace().clone());
    store.upsert(&item).map_err(|error| {
        ExecutionError::Step(format!("{PROPOSE_BACKLOG_ITEM_TOOL_NAME}: {error}"))
    })?;
    Ok(serde_json::json!({
        "backlog_item_id": item.id,
        "title": item.title,
        "priority": item.priority_wire(),
        "status": item.status_wire(),
    }))
}

/// Build the graceful, non-fatal result for a harness MUTATION whose target agent is
/// outside the acting harness's scope. Returning this (an `Ok`, not an `ExecutionError`)
/// instead of a hard error keeps an autonomous harness run from dead-pausing on a human
/// it will never get (the mode-3 finding): the request is routed to the owner's review
/// lane and the caller is told to continue. `extra` fields are merged into the object.
fn owner_approval_required_result(
    tool_name: &str,
    requested_owner_agent: &str,
    requesting_officer: &str,
    extra: serde_json::Map<String, Value>,
) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("status".into(), Value::from("owner_approval_required"));
    obj.insert("tool".into(), Value::from(tool_name));
    obj.insert(
        "requested_owner_agent".into(),
        Value::from(requested_owner_agent),
    );
    obj.insert("requesting_officer".into(), Value::from(requesting_officer));
    obj.insert(
        "guidance".into(),
        Value::from(format!(
            "`{requested_owner_agent}` is outside your harness scope, so `{tool_name}` cannot act in their domain directly. This request has been routed to the owner's review lane. Do NOT pause for the user — continue your cycle; the owner picks this up on their next review."
        )),
    );
    for (key, value) in extra {
        obj.insert(key, value);
    }
    Value::Object(obj)
}

async fn execute_promote_backlog_item(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let backlog_item_id =
        required_param_string(params, "backlog_item_id", PROMOTE_BACKLOG_ITEM_TOOL_NAME)?;
    let agent_id = required_param_string(params, "agent_id", PROMOTE_BACKLOG_ITEM_TOOL_NAME)?;

    let store = BacklogStore::new(services.artifact_service.workspace().clone());
    let item = store
        .get(&context.principal, &context.workspace, &backlog_item_id)
        .map_err(|error| {
            ExecutionError::Step(format!("{PROMOTE_BACKLOG_ITEM_TOOL_NAME}: {error}"))
        })?;

    // Cross-owner guard (mode-3 fix). Promotion assigns the resulting task to `agent_id`.
    // If that agent is outside this harness's scope, promoting would create a task in
    // another officer's domain. Instead of hard-erroring — which makes an autonomous
    // harness run dead-pause waiting for a human who never answers — leave the item
    // Proposed in the SHARED backlog (which the target owner's harness reviews and can
    // promote within its own scope) and return a non-fatal result telling the caller to
    // continue. This routes the request to the owner via the existing backlog lane.
    // (The sibling cross-owner guards on harness create_task / create_proposal still
    // hard-error; they can adopt `owner_approval_required_result` the same way.)
    if !context.scope.contains(&agent_id) {
        let requesting_officer = context.owner_record.definition.agent_id.clone();
        // Persist the cross-owner request onto the item so it is DIRECTED, not just left
        // to rot: `set_owner_request` stamps `requested_owner_agent`/`requested_by_officer`
        // (keeping it `Proposed`, no cross-scope task), and the shared "Company Backlog"
        // block the target agent's owner sees each cycle renders the flag so they can
        // promote it in their own scope. Idempotent: if the same request is already
        // recorded, don't re-write (breaks a per-cycle re-promote loop) — just report it
        // pending.
        let already = item.requested_owner_agent.as_deref() == Some(agent_id.as_str());
        if !already {
            store
                .set_owner_request(
                    &context.principal,
                    &context.workspace,
                    &item.id,
                    &agent_id,
                    &requesting_officer,
                )
                .map_err(|error| {
                    ExecutionError::Step(format!("{PROMOTE_BACKLOG_ITEM_TOOL_NAME}: {error}"))
                })?;
        }
        let mut extra = serde_json::Map::new();
        extra.insert("backlog_item_id".into(), Value::from(item.id.clone()));
        extra.insert("title".into(), Value::from(item.title.clone()));
        extra.insert("backlog_status".into(), Value::from(item.status_wire()));
        extra.insert("already_requested".into(), Value::from(already));
        return Ok(owner_approval_required_result(
            PROMOTE_BACKLOG_ITEM_TOOL_NAME,
            &agent_id,
            &requesting_officer,
            extra,
        ));
    }

    let goal_id = optional_string(params, "goal_id").or_else(|| context.goal_id.clone());
    let start_immediately = optional_bool(params, "start_immediately").unwrap_or(true);
    let lifecycle = harness_created_task_lifecycle(params);
    let priority = Some(parse_task_priority(
        item.priority_wire(),
        PROMOTE_BACKLOG_ITEM_TOOL_NAME,
    )?);
    let description = format!(
        "{}\n\n(Promoted from the company backlog; originally proposed by {}.)",
        item.description, item.source_agent
    );
    let task = create_harness_task_with_budget(
        services,
        context,
        PROMOTE_BACKLOG_ITEM_TOOL_NAME,
        CreateTaskInput {
            principal: context.principal.clone(),
            workspace: context.workspace.clone(),
            title: item.title.clone(),
            description,
            agent_id,
            goal_id,
            ui_thread_id: current_ui_thread_id(context),
            priority,
            due_date: None,
            tags: Vec::new(),
            created_by: harness_created_by(context),
            depends_on: Vec::new(),
            approved: true,
            schedule: None,
            output_mode: TaskOutputMode::Accumulate,
            chat_session_id: None,
            lifecycle,
            sync_mode: crate::magician_v2::artifact_v2::models::TaskSyncMode::default(),
        },
    )
    .await?;
    let (task, execution) = start_created_harness_task_if_requested(
        services,
        context,
        task,
        start_immediately,
        PROMOTE_BACKLOG_ITEM_TOOL_NAME,
    )
    .await?;

    // Mark after the task exists: if this write fails the item stays Proposed, so
    // a retry would create a second task (task ids are generated, not derived).
    // A rare, non-fatal duplicate is preferable to a promoted-but-taskless item.
    store
        .mark(
            &context.principal,
            &context.workspace,
            &item.id,
            BacklogStatus::Promoted,
            Some(task.manifest.task_id.clone()),
        )
        .map_err(|error| {
            ExecutionError::Step(format!("{PROMOTE_BACKLOG_ITEM_TOOL_NAME}: {error}"))
        })?;

    Ok(serde_json::json!({
        "backlog_item_id": item.id,
        "promoted_task_id": task.manifest.task_id,
        "task": task,
        "execution": execution,
    }))
}

fn persisted_artifact_is_material_delivery(
    artifact: &crate::magician_v2::artifact_v2::models::PersistedExecutionArtifactRecord,
) -> bool {
    let artifact_type = artifact.artifact_type.trim().to_ascii_lowercase();
    !artifact_type.is_empty()
        && !matches!(
            artifact_type.as_str(),
            "tool_inline_result" | "tool_call_evidence"
        )
        && !artifact_type.contains("inline_result")
}

fn bounded_delivery_preview(value: &str, max_chars: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut preview = trimmed
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    preview.push_str("...");
    preview
}

async fn execute_inspect_backlog_delivery(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let backlog_item_id = required_param_string(
        params,
        "backlog_item_id",
        INSPECT_BACKLOG_DELIVERY_TOOL_NAME,
    )?;
    let store = BacklogStore::new(services.artifact_service.workspace().clone());
    let item = store
        .get(&context.principal, &context.workspace, &backlog_item_id)
        .map_err(|error| {
            ExecutionError::Step(format!("{INSPECT_BACKLOG_DELIVERY_TOOL_NAME}: {error}"))
        })?;
    let task_id = item
        .promoted_task_id
        .as_deref()
        .map(str::trim)
        .filter(|task_id| !task_id.is_empty())
        .ok_or_else(|| {
            ExecutionError::Step(format!(
                "{INSPECT_BACKLOG_DELIVERY_TOOL_NAME}: backlog item `{backlog_item_id}` has no promoted task id"
            ))
        })?;
    let scope = ScopeRef::system_internal_unauthenticated(
        &context.principal.clone(),
        &context.workspace.clone(),
    );
    let task = services
        .artifact_service
        .get_task(&scope, task_id)
        .await
        .map_err(|error| {
            ExecutionError::Step(format!(
                "{INSPECT_BACKLOG_DELIVERY_TOOL_NAME}: reading promoted task `{task_id}`: {error}"
            ))
        })?;

    let workspace = services.artifact_service.workspace();
    let task_dir = workspace.task_dir(&context.principal, &context.workspace, task_id);
    let mut outputs = Vec::new();
    for output in task.refs.outputs.iter().take(8) {
        let relative = std::path::Path::new(&output.relative_path);
        let safe_relative = !relative.is_absolute()
            && relative
                .components()
                .all(|component| matches!(component, Component::Normal(_) | Component::CurDir));
        let media_type = output
            .media_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let preview = if safe_relative
            && (media_type.starts_with("text/") || media_type == "application/json")
        {
            workspace
                .read_to_string_path(task_dir.join(relative))
                .await
                .ok()
                .map(|body| bounded_delivery_preview(&body, 1200))
        } else {
            None
        };
        outputs.push(serde_json::json!({
            "evidence_ref": format!("output:{}", output.output_id),
            "output_id": output.output_id,
            "role": output.role,
            "audience": output.audience,
            "media_type": output.media_type,
            "relative_path": output.relative_path,
            "preview": preview,
        }));
    }

    let execution_id = task
        .state
        .last_completed_root_execution_id
        .as_deref()
        .or(task.state.latest_root_execution_id.as_deref());
    let persisted_artifacts = if let Some(execution_id) = execution_id {
        FilesystemExecutionArtifactIndexStore::new(workspace.clone())
            .list_artifacts(&scope, task_id, execution_id)
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let material_evidence_refs = persisted_artifacts
        .iter()
        .filter(|artifact| persisted_artifact_is_material_delivery(artifact))
        .map(|artifact| format!("artifact:{}", artifact.artifact_id))
        .collect::<Vec<_>>();
    let artifacts = persisted_artifacts
        .iter()
        .take(24)
        .map(|artifact| {
            serde_json::json!({
                "evidence_ref": format!("artifact:{}", artifact.artifact_id),
                "artifact_id": artifact.artifact_id,
                "artifact_type": artifact.artifact_type,
                "content_type": artifact.content_type,
                "material_delivery_evidence": persisted_artifact_is_material_delivery(artifact),
            })
        })
        .collect::<Vec<_>>();

    Ok(serde_json::json!({
        "backlog_item_id": item.id,
        "backlog_status": item.status_wire(),
        "title": item.title,
        "requested_outcome": item.description,
        "promoted_task_id": task_id,
        "task_status": task.state.status,
        "latest_execution_id": execution_id,
        "outputs": outputs,
        "persisted_artifacts": artifacts,
        "material_evidence_refs": material_evidence_refs,
    }))
}

async fn execute_review_backlog_delivery(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let backlog_item_id =
        required_param_string(params, "backlog_item_id", REVIEW_BACKLOG_DELIVERY_TOOL_NAME)?;
    let disposition = match required_param_string(
        params,
        "disposition",
        REVIEW_BACKLOG_DELIVERY_TOOL_NAME,
    )?
    .to_ascii_lowercase()
    .as_str()
    {
        "accepted" => BacklogDeliveryDisposition::Accepted,
        "rework" => BacklogDeliveryDisposition::Rework,
        other => {
            return Err(ExecutionError::Step(format!(
                "{REVIEW_BACKLOG_DELIVERY_TOOL_NAME}: disposition must be `accepted` or `rework`, got `{other}`"
            )))
        },
    };
    let summary = required_param_string(params, "summary", REVIEW_BACKLOG_DELIVERY_TOOL_NAME)?;
    let revised_description = optional_string(params, "revised_description");

    let store = BacklogStore::new(services.artifact_service.workspace().clone());
    let item = store
        .get(&context.principal, &context.workspace, &backlog_item_id)
        .map_err(|error| {
            ExecutionError::Step(format!("{REVIEW_BACKLOG_DELIVERY_TOOL_NAME}: {error}"))
        })?;
    if item.status != BacklogStatus::Promoted {
        return Err(ExecutionError::Step(format!(
            "{REVIEW_BACKLOG_DELIVERY_TOOL_NAME}: backlog item `{backlog_item_id}` is `{}`, expected `promoted`",
            item.status_wire()
        )));
    }
    let task_id = item
        .promoted_task_id
        .as_deref()
        .map(str::trim)
        .filter(|task_id| !task_id.is_empty())
        .ok_or_else(|| {
            ExecutionError::Step(format!(
                "{REVIEW_BACKLOG_DELIVERY_TOOL_NAME}: promoted item `{backlog_item_id}` has no task id"
            ))
        })?;
    let scope = ScopeRef::system_internal_unauthenticated(
        &context.principal.clone(),
        &context.workspace.clone(),
    );
    let task = services
        .artifact_service
        .get_task(&scope, task_id)
        .await
        .map_err(|error| {
            ExecutionError::Step(format!(
                "{REVIEW_BACKLOG_DELIVERY_TOOL_NAME}: reading promoted task `{task_id}`: {error}"
            ))
        })?;
    let task_status = task.state.status.trim().to_ascii_lowercase();
    let completed_execution_id = task
        .state
        .last_completed_root_execution_id
        .as_deref()
        .or(task.state.latest_root_execution_id.as_deref());
    let material_evidence_count = if let Some(execution_id) = completed_execution_id {
        FilesystemExecutionArtifactIndexStore::new(services.artifact_service.workspace().clone())
            .list_artifacts(&scope, task_id, execution_id)
            .await
            .unwrap_or_default()
            .iter()
            .filter(|artifact| persisted_artifact_is_material_delivery(artifact))
            .count()
    } else {
        0
    };
    match disposition {
        BacklogDeliveryDisposition::Accepted if task_status != "completed" => {
            return Err(ExecutionError::Step(format!(
                "{REVIEW_BACKLOG_DELIVERY_TOOL_NAME}: cannot accept task `{task_id}` while status is `{task_status}`"
            )));
        },
        BacklogDeliveryDisposition::Accepted if material_evidence_count == 0 => {
            return Err(ExecutionError::Step(format!(
                "{REVIEW_BACKLOG_DELIVERY_TOOL_NAME}: cannot accept task `{task_id}` with material_persisted_delivery_evidence=0"
            )));
        },
        BacklogDeliveryDisposition::Rework
            if matches!(task_status.as_str(), "running" | "planning" | "paused") =>
        {
            return Err(ExecutionError::Step(format!(
                "{REVIEW_BACKLOG_DELIVERY_TOOL_NAME}: cannot return active task `{task_id}` for rework while status is `{task_status}`"
            )));
        },
        _ => {},
    }

    let reviewed = store
        .review_delivery(
            &context.principal,
            &context.workspace,
            &backlog_item_id,
            disposition,
            &summary,
            revised_description.as_deref(),
        )
        .map_err(|error| {
            ExecutionError::Step(format!("{REVIEW_BACKLOG_DELIVERY_TOOL_NAME}: {error}"))
        })?;

    Ok(serde_json::json!({
        "backlog_item_id": reviewed.id,
        "backlog_status": reviewed.status_wire(),
        "promoted_task_id": task_id,
        "task_status": task_status,
        "material_evidence_count": material_evidence_count,
        "delivery_review": reviewed.delivery_review,
    }))
}

async fn execute_create_dashboard(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let task_id = required_param_string(params, "task_id", CREATE_DASHBOARD_TOOL_NAME)?;
    let scope = ScopeRef::system_internal_unauthenticated(
        &context.principal.clone(),
        &context.workspace.clone(),
    );

    let route_raw = optional_string(params, "route");
    let route = route_raw
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let placement_kind = optional_string(params, "placement_kind")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "workspace".to_string());

    let task_record = services
        .artifact_service
        .get_task(&scope, &task_id)
        .await
        .map_err(|error| step_error(CREATE_DASHBOARD_TOOL_NAME, error))?;

    let pinned = optional_bool(params, "pinned").unwrap_or(true);
    let input = build_dashboard_publish_input(
        &task_id,
        &scope.workspace(),
        &task_record.manifest.ui_thread_id,
        &placement_kind,
        pinned,
        route,
        optional_string(params, "title"),
        optional_string(params, "summary"),
        optional_string(params, "source_output_id"),
    )
    .map_err(|error| step_error(CREATE_DASHBOARD_TOOL_NAME, error))?;

    let record = services
        .artifact_service
        .publish_surface_record(&scope, input)
        .await
        .map_err(|error| step_error(CREATE_DASHBOARD_TOOL_NAME, error))?;

    Ok(serde_json::json!({
        "context": build_context_summary(services, context).await,
        "surface_id": record.surface_id,
        "route": record.route,
        "surface_kind": record.surface_kind,
        "task_id": record.task_id,
        "title": record.title,
        "summary": record.summary,
        "placement": record.placement,
        "published_at": record.published_at,
    }))
}

async fn execute_update_program_state(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let patch = required_object_value(params, "patch", UPDATE_PROGRAM_STATE_TOOL_NAME)?;
    let reason = optional_string(params, "reason")
        .unwrap_or_else(|| "harness updated mutable program runtime state".to_string());
    let actor = optional_string(params, "actor")
        .unwrap_or_else(|| format!("harness_tool:{}", context.owner_record.definition.agent_id));
    let candidate_id = optional_string(params, "candidate_id");

    let Some(program) = context
        .load_program(services)
        .await
        .map_err(|error| step_error(UPDATE_PROGRAM_STATE_TOOL_NAME, error))?
    else {
        return Err(ExecutionError::Step(format!(
            "{UPDATE_PROGRAM_STATE_TOOL_NAME}: no active program document was found for this harness context"
        )));
    };

    let mut runtime_state = context
        .load_program_runtime_state(services, &program)
        .await
        .map_err(|error| step_error(UPDATE_PROGRAM_STATE_TOOL_NAME, error))?;
    runtime_state
        .apply_update(
            patch.clone(),
            actor,
            reason.clone(),
            candidate_id,
            context.task_id.clone(),
            context.execution_id.clone(),
        )
        .map_err(|error| step_error(UPDATE_PROGRAM_STATE_TOOL_NAME, error))?;

    crate::magician_v2::harness::ProgramLoader::new(services.artifact_service.workspace().clone())
        .write_runtime_state(
            &context.principal,
            &context.workspace,
            &program,
            context.goal_id.as_deref(),
            &runtime_state,
        )
        .await
        .map_err(|error| step_error(UPDATE_PROGRAM_STATE_TOOL_NAME, error))?;

    Ok(serde_json::json!({
        "context": build_context_summary(services, context).await,
        "program": {
            "relative_path": program.relative_path,
            "section": program.section,
            "title": program.title,
        },
        "runtime_state": runtime_state,
        "summary": reason,
    }))
}

async fn execute_create_agent(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let summary = required_param_string(params, "summary", CREATE_AGENT_TOOL_NAME)?;
    let rationale = optional_string(params, "rationale");
    let evidence_refs = required_string_array(params, "evidence_refs", CREATE_AGENT_TOOL_NAME)?;
    let definition_value = required_object_value(params, "definition", CREATE_AGENT_TOOL_NAME)?;
    let mut definition: AgentDefinition =
        serde_json::from_value(definition_value).map_err(|error| {
            step_error(
                CREATE_AGENT_TOOL_NAME,
                format!("`definition` must deserialize to an agent definition: {error}"),
            )
        })?;
    if definition.agent_id.trim().is_empty() {
        return Err(ExecutionError::Step(format!(
            "{CREATE_AGENT_TOOL_NAME}: `definition.agent_id` must not be empty"
        )));
    }
    if definition.agent_id == context.owner_record.definition.agent_id {
        return Err(ExecutionError::Step(format!(
            "{CREATE_AGENT_TOOL_NAME}: harness agents cannot create themselves"
        )));
    }

    definition.version = 1;
    definition.principal = Some(context.principal.clone());
    definition.workspace = Some(context.workspace.clone());
    definition.is_primary = false;
    definition.onboarding_completed = false;
    definition.apply_defaults();
    definition
        .validate()
        .map_err(|error| step_error(CREATE_AGENT_TOOL_NAME, error))?;
    let scoped_store = context.scoped_definition_store(services);
    validate_harness_delegation_targets(
        context,
        &scoped_store,
        CREATE_AGENT_TOOL_NAME,
        &definition,
    )
    .await?;

    let _control_guard = match &services.control_gate {
        Some(control_gate) => Some(control_gate.lock().await),
        None => None,
    };

    if scoped_store
        .get_definition(&definition.agent_id)
        .await
        .map_err(|error| step_error(CREATE_AGENT_TOOL_NAME, error))?
        .is_some()
    {
        return Err(ExecutionError::Step(format!(
            "{CREATE_AGENT_TOOL_NAME}: agent `{}` already exists in this scope",
            definition.agent_id
        )));
    }

    let yaml_after = definition
        .to_yaml_string()
        .map_err(|error| step_error(CREATE_AGENT_TOOL_NAME, error))?;
    let mut payload =
        optional_object(params, "payload", CREATE_AGENT_TOOL_NAME)?.unwrap_or_default();
    payload.insert(
        "new_agent_id".to_string(),
        Value::String(definition.agent_id.clone()),
    );
    payload.insert(
        "new_agent_kind".to_string(),
        Value::String(definition.kind.to_string()),
    );
    let proposal = create_structural_definition_proposal(
        context,
        &scoped_store,
        CREATE_AGENT_TOOL_NAME,
        definition.agent_id.clone(),
        "# agent does not exist yet\n".to_string(),
        yaml_after,
        summary.clone(),
        rationale.clone(),
        evidence_refs.clone(),
        payload,
    )
    .await?;

    Ok(serde_json::json!({
        "context": build_context_summary(services, context).await,
        "proposal": proposal,
        "summary": summary,
        "action_kind": CREATE_AGENT_TOOL_NAME,
        "evidence_refs": evidence_refs,
    }))
}

async fn execute_update_agent(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let agent_id = required_param_string(params, "agent_id", UPDATE_AGENT_TOOL_NAME)?;
    if agent_id == context.owner_record.definition.agent_id {
        return Err(ExecutionError::Step(format!(
            "{UPDATE_AGENT_TOOL_NAME}: harness agents cannot update themselves"
        )));
    }
    context
        .scope
        .require_contains(&agent_id)
        .map_err(|error| ExecutionError::Step(format!("{UPDATE_AGENT_TOOL_NAME}: {error}")))?;

    let summary = required_param_string(params, "summary", UPDATE_AGENT_TOOL_NAME)?;
    let rationale = optional_string(params, "rationale");
    let evidence_refs = required_string_array(params, "evidence_refs", UPDATE_AGENT_TOOL_NAME)?;
    let patch = required_object_value(params, "patch", UPDATE_AGENT_TOOL_NAME)?;

    let _control_guard = match &services.control_gate {
        Some(control_gate) => Some(control_gate.lock().await),
        None => None,
    };

    let scoped_store = context.scoped_definition_store(services);
    let record = scoped_store
        .get_definition(&agent_id)
        .await
        .map_err(|error| step_error(UPDATE_AGENT_TOOL_NAME, error))?
        .ok_or_else(|| {
            ExecutionError::Step(format!(
                "{UPDATE_AGENT_TOOL_NAME}: agent `{agent_id}` was not found in scope"
            ))
        })?;
    let yaml_before = record
        .definition
        .to_yaml_string()
        .map_err(|error| step_error(UPDATE_AGENT_TOOL_NAME, error))?;

    let mut merged_value = serde_json::to_value(&record.definition)
        .map_err(|error| step_error(UPDATE_AGENT_TOOL_NAME, error))?;
    apply_json_merge_patch(&mut merged_value, &patch);
    pin_immutable_definition_fields(&mut merged_value, &record.definition);

    let mut merged: AgentDefinition = serde_json::from_value(merged_value).map_err(|error| {
        step_error(
            UPDATE_AGENT_TOOL_NAME,
            format!("merged definition is invalid: {error}"),
        )
    })?;
    merged.apply_defaults();
    merged
        .validate()
        .map_err(|error| step_error(UPDATE_AGENT_TOOL_NAME, error))?;
    validate_harness_delegation_targets(context, &scoped_store, UPDATE_AGENT_TOOL_NAME, &merged)
        .await?;
    let yaml_after = merged
        .to_yaml_string()
        .map_err(|error| step_error(UPDATE_AGENT_TOOL_NAME, error))?;

    let mut payload =
        optional_object(params, "payload", UPDATE_AGENT_TOOL_NAME)?.unwrap_or_default();
    payload.insert("patch".to_string(), patch);
    let proposal = create_structural_definition_proposal(
        context,
        &scoped_store,
        UPDATE_AGENT_TOOL_NAME,
        agent_id.clone(),
        yaml_before,
        yaml_after,
        summary.clone(),
        rationale.clone(),
        evidence_refs.clone(),
        payload,
    )
    .await?;

    Ok(serde_json::json!({
        "context": build_context_summary(services, context).await,
        "proposal": proposal,
        "summary": summary,
        "action_kind": UPDATE_AGENT_TOOL_NAME,
        "evidence_refs": evidence_refs,
    }))
}

async fn execute_retire_agent(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let agent_id = required_param_string(params, "agent_id", RETIRE_AGENT_TOOL_NAME)?;
    if agent_id == context.owner_record.definition.agent_id {
        return Err(ExecutionError::Step(format!(
            "{RETIRE_AGENT_TOOL_NAME}: harness agents cannot retire themselves"
        )));
    }
    context
        .scope
        .require_contains(&agent_id)
        .map_err(|error| ExecutionError::Step(format!("{RETIRE_AGENT_TOOL_NAME}: {error}")))?;

    let summary = required_param_string(params, "summary", RETIRE_AGENT_TOOL_NAME)?;
    let rationale = optional_string(params, "rationale");
    let evidence_refs = required_string_array(params, "evidence_refs", RETIRE_AGENT_TOOL_NAME)?;

    let _control_guard = match &services.control_gate {
        Some(control_gate) => Some(control_gate.lock().await),
        None => None,
    };

    let scoped_store = context.scoped_definition_store(services);
    let record = scoped_store
        .get_definition(&agent_id)
        .await
        .map_err(|error| step_error(RETIRE_AGENT_TOOL_NAME, error))?
        .ok_or_else(|| {
            ExecutionError::Step(format!(
                "{RETIRE_AGENT_TOOL_NAME}: agent `{agent_id}` was not found in scope"
            ))
        })?;
    let yaml_before = record
        .definition
        .to_yaml_string()
        .map_err(|error| step_error(RETIRE_AGENT_TOOL_NAME, error))?;

    let mut retired = record.definition.clone();
    retired.autonomous_config = None;
    let yaml_after = retired
        .to_yaml_string()
        .map_err(|error| step_error(RETIRE_AGENT_TOOL_NAME, error))?;

    let mut payload =
        optional_object(params, "payload", RETIRE_AGENT_TOOL_NAME)?.unwrap_or_default();
    payload.insert(
        "retire_mode".to_string(),
        Value::String("pause".to_string()),
    );
    let proposal = create_structural_definition_proposal(
        context,
        &scoped_store,
        RETIRE_AGENT_TOOL_NAME,
        agent_id.clone(),
        yaml_before,
        yaml_after,
        summary.clone(),
        rationale.clone(),
        evidence_refs.clone(),
        payload,
    )
    .await?;

    Ok(serde_json::json!({
        "context": build_context_summary(services, context).await,
        "proposal": proposal,
        "summary": summary,
        "action_kind": RETIRE_AGENT_TOOL_NAME,
        "evidence_refs": evidence_refs,
    }))
}

async fn execute_update_delegation(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
    params: &HashMap<String, Value>,
) -> Result<Value, ExecutionError> {
    let agent_id = required_param_string(params, "agent_id", UPDATE_DELEGATION_TOOL_NAME)?;
    if agent_id != context.owner_record.definition.agent_id {
        context.scope.require_contains(&agent_id).map_err(|error| {
            ExecutionError::Step(format!("{UPDATE_DELEGATION_TOOL_NAME}: {error}"))
        })?;
    }

    let summary = required_param_string(params, "summary", UPDATE_DELEGATION_TOOL_NAME)?;
    let rationale = optional_string(params, "rationale");
    let evidence_refs =
        required_string_array(params, "evidence_refs", UPDATE_DELEGATION_TOOL_NAME)?;
    let add = optional_string_array(params, "add", UPDATE_DELEGATION_TOOL_NAME)?;
    let remove = optional_string_array(params, "remove", UPDATE_DELEGATION_TOOL_NAME)?;
    if add.is_empty() && remove.is_empty() {
        return Err(ExecutionError::Step(format!(
            "{UPDATE_DELEGATION_TOOL_NAME}: at least one of `add` or `remove` must be provided"
        )));
    }

    let _control_guard = match &services.control_gate {
        Some(control_gate) => Some(control_gate.lock().await),
        None => None,
    };

    let scoped_store = context.scoped_definition_store(services);
    let record = scoped_store
        .get_definition(&agent_id)
        .await
        .map_err(|error| step_error(UPDATE_DELEGATION_TOOL_NAME, error))?
        .ok_or_else(|| {
            ExecutionError::Step(format!(
                "{UPDATE_DELEGATION_TOOL_NAME}: agent `{agent_id}` was not found in scope"
            ))
        })?;
    let yaml_before = record
        .definition
        .to_yaml_string()
        .map_err(|error| step_error(UPDATE_DELEGATION_TOOL_NAME, error))?;

    let mut updated = record.definition.clone();
    let remove_set = remove
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    updated
        .delegation_targets
        .retain(|target| !remove_set.contains(target));
    updated.delegation_targets.extend(add.iter().cloned());
    updated.delegation_targets.sort();
    updated.delegation_targets.dedup();
    if updated.delegation_targets == record.definition.delegation_targets {
        return Err(ExecutionError::Step(format!(
            "{UPDATE_DELEGATION_TOOL_NAME}: requested change does not modify delegation_targets"
        )));
    }
    updated
        .validate()
        .map_err(|error| step_error(UPDATE_DELEGATION_TOOL_NAME, error))?;
    validate_harness_delegation_targets(
        context,
        &scoped_store,
        UPDATE_DELEGATION_TOOL_NAME,
        &updated,
    )
    .await?;
    let yaml_after = updated
        .to_yaml_string()
        .map_err(|error| step_error(UPDATE_DELEGATION_TOOL_NAME, error))?;

    let mut payload =
        optional_object(params, "payload", UPDATE_DELEGATION_TOOL_NAME)?.unwrap_or_default();
    payload.insert("add".to_string(), serde_json::json!(add));
    payload.insert("remove".to_string(), serde_json::json!(remove));
    let proposal = create_structural_definition_proposal(
        context,
        &scoped_store,
        UPDATE_DELEGATION_TOOL_NAME,
        agent_id.clone(),
        yaml_before,
        yaml_after,
        summary.clone(),
        rationale.clone(),
        evidence_refs.clone(),
        payload,
    )
    .await?;

    Ok(serde_json::json!({
        "context": build_context_summary(services, context).await,
        "proposal": proposal,
        "summary": summary,
        "action_kind": UPDATE_DELEGATION_TOOL_NAME,
        "evidence_refs": evidence_refs,
    }))
}

async fn create_structural_definition_proposal(
    context: &HarnessExecutionContext,
    scoped_store: &crate::magician_v2::agents::AgentDefinitionStore,
    action_kind: &'static str,
    agent_id: String,
    yaml_before: String,
    yaml_after: String,
    summary: String,
    rationale: Option<String>,
    evidence_refs: Vec<String>,
    mut payload: Map<String, Value>,
) -> Result<crate::magician_v2::agents::DefinitionProposal, ExecutionError> {
    payload.insert(
        "action_kind".to_string(),
        Value::String(action_kind.to_string()),
    );
    payload.insert("summary".to_string(), Value::String(summary));
    if let Some(rationale) = rationale {
        payload.insert("rationale".to_string(), Value::String(rationale));
    }
    payload.insert(
        "evidence_refs".to_string(),
        Value::Array(evidence_refs.into_iter().map(Value::String).collect()),
    );
    payload.insert(
        "owner_agent_id".to_string(),
        Value::String(context.owner_record.definition.agent_id.clone()),
    );
    if let Some(goal_id) = context.goal_id.clone() {
        payload.insert("goal_id".to_string(), Value::String(goal_id));
    }
    if let Some(task_id) = context.task_id.clone() {
        payload.insert("task_id".to_string(), Value::String(task_id));
    }
    if let Some(focus_area) = context.scope.focus_area_name() {
        payload.insert(
            "focus_area".to_string(),
            Value::String(focus_area.to_string()),
        );
    }

    ProposalStore::new(scoped_store.storage().clone())
        .create_proposal(NewDefinitionProposal {
            agent_id,
            source: proposal_source(context),
            payload: Value::Object(payload),
            yaml_before,
            yaml_after,
        })
        .await
        .map_err(|error| step_error(action_kind, error))
}

async fn build_context_summary(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
) -> HarnessContextSummary {
    let loader = crate::magician_v2::harness::ProgramLoader::new(
        services.artifact_service.workspace().clone(),
    );
    let (program, program_error) = match context.load_program(services).await {
        Ok(program) => (
            program.map(|loaded| ProgramSummary {
                state_relative_path: loader
                    .runtime_state_relative_path(&loaded, context.goal_id.as_deref()),
                relative_path: loaded.relative_path,
                section: loaded.section,
                title: loaded.title,
            }),
            None,
        ),
        Err(error) => (None, Some(error)),
    };

    HarnessContextSummary {
        owner_agent_id: context.owner_record.definition.agent_id.clone(),
        principal: context.principal.clone(),
        workspace: context.workspace.clone(),
        task_id: context.task_id.clone(),
        execution_id: context.execution_id.clone(),
        goal_id: context.goal_id.clone(),
        focus_area: context.scope.focus_area_name().map(str::to_string),
        scope_agent_ids: context.scope.target_ids(),
        program,
        program_error,
    }
}

async fn validate_harness_structural_definition(
    context: &HarnessExecutionContext,
    scoped_store: &AgentDefinitionStore,
    tool_name: &str,
    expected_agent_id: &str,
    definition: &AgentDefinition,
) -> Result<(), ExecutionError> {
    validate_harness_structural_definition_with_scope(
        &context.owner_record.definition.agent_id,
        &context.principal,
        &context.workspace,
        &context.scope,
        scoped_store,
        expected_agent_id,
        definition,
    )
    .await
    .map_err(|error| step_error(tool_name, error))
}

async fn validate_harness_delegation_targets(
    context: &HarnessExecutionContext,
    scoped_store: &AgentDefinitionStore,
    tool_name: &str,
    definition: &AgentDefinition,
) -> Result<(), ExecutionError> {
    validate_harness_definition_access(
        &context.owner_record.definition.agent_id,
        &context.scope,
        scoped_store,
        definition,
    )
    .await
    .map_err(|error| step_error(tool_name, error))
}

async fn validate_harness_structural_definition_with_scope(
    owner_agent_id: &str,
    principal: &str,
    workspace: &str,
    scope: &HarnessScope,
    scoped_store: &AgentDefinitionStore,
    expected_agent_id: &str,
    definition: &AgentDefinition,
) -> Result<(), String> {
    if definition.agent_id != expected_agent_id {
        return Err(format!(
            "`yaml_after.agent_id` `{}` does not match target agent `{expected_agent_id}`",
            definition.agent_id
        ));
    }
    if definition.principal.as_deref() != Some(principal)
        || definition.workspace.as_deref() != Some(workspace)
    {
        return Err(format!(
            "`yaml_after` must stay in the active harness scope `{principal}/{workspace}`"
        ));
    }
    definition.validate().map_err(|error| error.to_string())?;
    validate_harness_definition_access(owner_agent_id, scope, scoped_store, definition).await
}

async fn validate_harness_definition_access(
    owner_agent_id: &str,
    scope: &HarnessScope,
    scoped_store: &AgentDefinitionStore,
    definition: &AgentDefinition,
) -> Result<(), String> {
    validate_harness_delegation_targets_with_scope(owner_agent_id, scope, scoped_store, definition)
        .await?;
    validate_harness_readable_agents_with_scope(scope, definition)
}

async fn validate_harness_delegation_targets_with_scope(
    owner_agent_id: &str,
    scope: &HarnessScope,
    scoped_store: &AgentDefinitionStore,
    definition: &AgentDefinition,
) -> Result<(), String> {
    for target in &definition.delegation_targets {
        let normalized = target.trim();
        if normalized.is_empty() {
            continue;
        }
        if normalized == "*" {
            return Err(
                "`delegation_targets` must use explicit in-scope agent ids; wildcard `*` is not allowed in harness structural changes"
                    .to_string(),
            );
        }
        if definition.agent_id == owner_agent_id {
            if normalized == owner_agent_id {
                return Err(
                    "owner delegation_targets must not include the harness owner itself"
                        .to_string(),
                );
            }
            let record = scoped_store
                .get_definition(normalized)
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(|| {
                    format!(
                        "delegation target `{normalized}` must already exist in the active harness scope workspace"
                    )
                })?;
            if record.definition.is_system_agent() {
                return Err(format!(
                    "delegation target `{normalized}` is a system agent and cannot be adopted into a harness roster"
                ));
            }
            continue;
        }
        scope.require_contains(normalized).map_err(|error| {
            format!("delegation target `{normalized}` is outside the active harness scope: {error}")
        })?;
    }
    Ok(())
}

fn validate_harness_readable_agents_with_scope(
    scope: &HarnessScope,
    definition: &AgentDefinition,
) -> Result<(), String> {
    for agent_id in &definition.readable_agents {
        let normalized = agent_id.trim();
        if normalized.is_empty() {
            continue;
        }
        if normalized == "*" {
            return Err(
                "`readable_agents` must use explicit in-scope agent ids; wildcard `*` is not allowed in harness structural changes"
                    .to_string(),
            );
        }
        if normalized == definition.agent_id {
            continue;
        }
        scope.require_contains(normalized).map_err(|error| {
            format!("readable agent `{normalized}` is outside the active harness scope: {error}")
        })?;
    }
    Ok(())
}

pub fn harness_owner_agent_id_from_source(source: &str) -> Option<&str> {
    source
        .strip_prefix("harness:")
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn proposal_payload_string<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload
        .as_object()
        .and_then(|object| object.get(key))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub async fn validate_harness_structural_proposal(
    scoped_store: &AgentDefinitionStore,
    proposal_principal: &str,
    proposal_workspace: &str,
    proposal_source: &str,
    proposal_payload: &Value,
    target_agent_id: &str,
    definition: &AgentDefinition,
) -> Result<(), String> {
    let owner_agent_id = harness_owner_agent_id_from_source(proposal_source)
        .ok_or_else(|| "proposal source is not harness-backed".to_string())?;
    if let Some(payload_owner_agent_id) =
        proposal_payload_string(proposal_payload, "owner_agent_id")
    {
        if payload_owner_agent_id != owner_agent_id {
            return Err(format!(
                "harness proposal source `{proposal_source}` does not match payload owner_agent_id `{payload_owner_agent_id}`"
            ));
        }
    }

    let owner_record = scoped_store
        .get_definition(owner_agent_id)
        .await
        .map_err(|error| format!("failed to load harness owner `{owner_agent_id}`: {error}"))?
        .ok_or_else(|| format!("harness owner `{owner_agent_id}` was not found in scope"))?;
    if owner_record.definition.kind != AgentKind::Personal {
        return Err(format!(
            "harness proposals require a personal owner; `{owner_agent_id}` is {:?}",
            owner_record.definition.kind
        ));
    }
    if owner_record.definition.harness.is_none() {
        return Err(format!(
            "harness proposals require owner `{owner_agent_id}` to be harness-enabled"
        ));
    }

    let goal_id = proposal_payload_string(proposal_payload, "goal_id");
    let scope = HarnessScope::resolve(scoped_store, &owner_record.definition, goal_id).await?;
    let action_kind = proposal_payload_string(proposal_payload, "action_kind");
    if target_agent_id == owner_agent_id && action_kind != Some(UPDATE_DELEGATION_TOOL_NAME) {
        return Err(
            "harness structural proposals must not target the harness owner except update_delegation"
                .to_string(),
        );
    }
    if target_agent_id != owner_agent_id && action_kind != Some(CREATE_AGENT_TOOL_NAME) {
        scope.require_contains(target_agent_id).map_err(|error| {
            format!(
                "proposal target `{target_agent_id}` is outside the active harness scope: {error}"
            )
        })?;
    }

    validate_harness_structural_definition_with_scope(
        owner_agent_id,
        proposal_principal,
        proposal_workspace,
        &scope,
        scoped_store,
        target_agent_id,
        definition,
    )
    .await
}

fn selected_scope_agents(
    context: &HarnessExecutionContext,
    requested_agent_id: Option<&Value>,
) -> Result<Vec<String>, ExecutionError> {
    let requested = requested_agent_id
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(agent_id) = requested {
        context
            .scope
            .require_contains(agent_id)
            .map_err(ExecutionError::Step)?;
        Ok(vec![agent_id.to_string()])
    } else {
        Ok(context.scope.target_ids())
    }
}

fn matches_episode_filters(
    episode: &V3EpisodeRecord,
    outcome: Option<&str>,
    since_cutoff: Option<&chrono::DateTime<Utc>>,
) -> bool {
    if let Some(outcome) = outcome {
        let outcome_matches = match outcome {
            "succeeded" => episode.outcome_is_succeeded(),
            "failed" => episode.outcome_is_failed(),
            "paused" => episode.outcome_is_paused(),
            "completed" => episode.outcome_is_completed(),
            _ => true,
        };
        if !outcome_matches {
            return false;
        }
    }

    if let Some(cutoff) = since_cutoff {
        let Ok(completed_at) = episode.completed_at_dt() else {
            return false;
        };
        if completed_at < *cutoff {
            return false;
        }
    }

    true
}

fn group_tasks_by_agent(tasks: Vec<TaskListItemV3>) -> HashMap<String, Vec<TaskListItemV3>> {
    let mut grouped = HashMap::new();
    for task in tasks {
        grouped
            .entry(task.agent_id.clone())
            .or_insert_with(Vec::new)
            .push(task);
    }
    for entries in grouped.values_mut() {
        entries.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    }
    grouped
}

fn summarize_episodes(episodes: &[V3EpisodeRecord]) -> EpisodeStats {
    let mut successful = 0usize;
    let mut failed = 0usize;
    let mut paused = 0usize;
    let mut last_episode: Option<&V3EpisodeRecord> = None;

    for episode in episodes {
        if episode.outcome_is_succeeded() {
            successful += 1;
        }
        if episode.outcome_is_failed() {
            failed += 1;
        }
        if episode.outcome_is_paused() {
            paused += 1;
        }
        if last_episode
            .map(|current| episode.completed_at > current.completed_at)
            .unwrap_or(true)
        {
            last_episode = Some(episode);
        }
    }

    EpisodeStats {
        total: episodes.len(),
        successful,
        failed,
        paused,
        last_episode_at: last_episode.map(|episode| episode.completed_at.clone()),
        last_outcome: last_episode.map(|episode| episode.outcome_kind.clone()),
    }
}

fn summarize_episode_impact_stats(episodes: &[V3EpisodeRecord]) -> EpisodeImpactStats {
    let completed = episodes
        .iter()
        .filter(|episode| episode.outcome_is_completed())
        .count();
    let successful = episodes
        .iter()
        .filter(|episode| episode.outcome_is_succeeded())
        .count();
    let failed = episodes
        .iter()
        .filter(|episode| episode.outcome_is_failed())
        .count();
    let paused = episodes
        .iter()
        .filter(|episode| episode.outcome_is_paused())
        .count();

    EpisodeImpactStats {
        total: episodes.len(),
        completed,
        successful,
        failed,
        paused,
        success_rate: if completed > 0 {
            Some(successful as f64 / completed as f64)
        } else {
            None
        },
    }
}

fn episode_success_rate(episodes: &[V3EpisodeRecord]) -> Option<f64> {
    summarize_episode_impact_stats(episodes).success_rate
}

fn focus_area_priority_label(
    priority: &crate::magician_v2::agents::types::FocusAreaPriority,
) -> &'static str {
    match priority {
        crate::magician_v2::agents::types::FocusAreaPriority::Low => "low",
        crate::magician_v2::agents::types::FocusAreaPriority::Medium => "medium",
        crate::magician_v2::agents::types::FocusAreaPriority::High => "high",
    }
}

fn episode_within_window(
    episode: &V3EpisodeRecord,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> bool {
    episode
        .completed_at_dt()
        .map(|completed_at| completed_at >= start && completed_at < end)
        .unwrap_or(false)
}

fn build_proposal_impact_entry(
    proposal: &crate::magician_v2::agents::DefinitionProposal,
    target_episodes: &[V3EpisodeRecord],
    impact_window_days: i64,
) -> Result<ProposalImpactEntry, ExecutionError> {
    let applied_at =
        proposal.applied_at.as_ref().cloned().ok_or_else(|| {
            step_error(EVALUATE_HARNESS_TOOL_NAME, "proposal is missing applied_at")
        })?;
    let before_start = applied_at - Duration::days(impact_window_days);
    let after_end = std::cmp::min(applied_at + Duration::days(impact_window_days), Utc::now());
    let before = target_episodes
        .iter()
        .filter(|episode| episode_within_window(episode, before_start, applied_at))
        .cloned()
        .collect::<Vec<_>>();
    let after = target_episodes
        .iter()
        .filter(|episode| episode_within_window(episode, applied_at, after_end))
        .cloned()
        .collect::<Vec<_>>();
    let before_stats = summarize_episode_impact_stats(&before);
    let after_stats = summarize_episode_impact_stats(&after);

    Ok(ProposalImpactEntry {
        proposal_id: proposal.proposal_id.clone(),
        agent_id: proposal.agent_id.clone(),
        status: format!("{:?}", proposal.status).to_ascii_lowercase(),
        action_kind: proposal_action_kind_value(proposal),
        summary: proposal
            .payload
            .get("summary")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        applied_at: proposal.applied_at.as_ref().map(|value| value.to_rfc3339()),
        impact_window_days,
        window_complete: applied_at + Duration::days(impact_window_days) <= Utc::now(),
        success_rate_delta: match (after_stats.success_rate, before_stats.success_rate) {
            (Some(after), Some(before)) => Some(after - before),
            _ => None,
        },
        before: before_stats,
        after: after_stats,
    })
}

async fn load_program_metric_documents(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
    owner_episodes: &[V3EpisodeRecord],
    scope_tasks: &[TaskListItemV3],
    proposals: &[crate::magician_v2::agents::DefinitionProposal],
) -> Result<Vec<ProgramMetricsDocument>, String> {
    let memory = context.memory_service(services);
    let mut scope_episodes = Vec::new();
    for agent_id in context.scope.target_ids() {
        let episodes = memory
            .load_native_episodes(&agent_id)
            .await
            .map_err(|error| error.to_string())?;
        scope_episodes.extend(episodes);
    }

    let loader = crate::magician_v2::harness::ProgramLoader::new(
        services.artifact_service.workspace().clone(),
    );
    let mut documents = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let focus_areas = context
        .owner_record
        .definition
        .autonomous_config
        .as_ref()
        .map(|config| config.focus_areas.iter().map(Some).collect::<Vec<_>>())
        .unwrap_or_else(|| vec![None]);

    for focus_area in focus_areas {
        let Some(loaded) = loader
            .load_for_focus_area(
                &context.principal,
                &context.workspace,
                &context.owner_record.definition,
                focus_area,
            )
            .await?
        else {
            continue;
        };

        let key = format!(
            "{}::{}",
            loaded.relative_path,
            loaded.section.clone().unwrap_or_default()
        );
        if !seen.insert(key) {
            continue;
        }

        let (specs, parse_errors) = parse_program_metric_specs(&loaded.content);
        let metrics = specs
            .iter()
            .map(|spec| {
                evaluate_program_metric(
                    spec,
                    owner_episodes,
                    &scope_episodes,
                    scope_tasks,
                    proposals,
                    &proposal_source(context),
                )
            })
            .collect::<Vec<_>>();
        // The unsupported branch of `evaluate_program_metric` is the only one
        // that leaves `unit == "unknown"`; collect those ids so unrecognized
        // metric ids surface explicitly rather than as a silent `n/a` line.
        let unsupported_metric_ids = metrics
            .iter()
            .filter(|metric| metric.unit == "unknown")
            .map(|metric| metric.metric_id.clone())
            .collect::<Vec<_>>();
        documents.push(ProgramMetricsDocument {
            relative_path: loaded.relative_path,
            section: loaded.section,
            title: loaded.title,
            metrics,
            parse_errors,
            unsupported_metric_ids,
        });
    }

    Ok(documents)
}

fn parse_program_metric_specs(markdown: &str) -> (Vec<ProgramMetricSpec>, Vec<String>) {
    let Some(section) =
        crate::magician_v2::harness::program::extract_markdown_section(markdown, "Success Metrics")
    else {
        return (Vec::new(), Vec::new());
    };

    let mut specs = Vec::new();
    let mut parse_errors = Vec::new();
    for line in section.lines() {
        let trimmed = line.trim();
        let metric_line = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .map(str::trim);
        let Some(metric_line) = metric_line else {
            continue;
        };
        match parse_program_metric_spec(metric_line) {
            Ok(spec) => specs.push(spec),
            Err(error) => parse_errors.push(error),
        }
    }
    (specs, parse_errors)
}

fn parse_program_metric_spec(raw: &str) -> Result<ProgramMetricSpec, String> {
    for comparator in [">=", "<=", ">", "<", "="] {
        if let Some((metric_id, target)) = raw.split_once(comparator) {
            let metric_id = metric_id.trim();
            if metric_id.is_empty() {
                return Err(format!("invalid success metric `{raw}`: missing metric id"));
            }
            let target = target.trim().parse::<f64>().map_err(|error| {
                format!("invalid success metric `{raw}`: invalid numeric target ({error})")
            })?;
            return Ok(ProgramMetricSpec {
                metric_id: metric_id.to_string(),
                comparator,
                target,
            });
        }
    }

    Err(format!(
        "invalid success metric `{raw}`: expected one of `>=`, `<=`, `>`, `<`, `=`"
    ))
}

fn evaluate_program_metric(
    spec: &ProgramMetricSpec,
    owner_episodes: &[V3EpisodeRecord],
    scope_episodes: &[V3EpisodeRecord],
    scope_tasks: &[TaskListItemV3],
    proposals: &[crate::magician_v2::agents::DefinitionProposal],
    harness_source: &str,
) -> ProgramMetricEvaluation {
    let (current, unit, reason) =
        if let Some(days) = metric_window_days(&spec.metric_id, "harness_success_rate_") {
            (
                success_rate_for_recent_episodes(owner_episodes, days),
                "ratio".to_string(),
                Some("no completed harness episodes in the requested window".to_string()),
            )
        } else if let Some(days) = metric_window_days(&spec.metric_id, "scope_success_rate_") {
            (
                success_rate_for_recent_episodes(scope_episodes, days),
                "ratio".to_string(),
                Some("no completed scoped episodes in the requested window".to_string()),
            )
        } else if let Some(days) = metric_window_days(&spec.metric_id, "completed_tasks_") {
            (
                Some(recent_completed_tasks(scope_tasks, days) as f64),
                "count".to_string(),
                None,
            )
        } else if spec.metric_id == "open_tasks" {
            (
                Some(open_tasks(scope_tasks) as f64),
                "count".to_string(),
                None,
            )
        } else if let Some(days) = metric_window_days(&spec.metric_id, "applied_proposals_") {
            (
                Some(recent_applied_proposals(proposals, harness_source, days) as f64),
                "count".to_string(),
                None,
            )
        } else {
            (
                None,
                "unknown".to_string(),
                Some("unsupported success metric id".to_string()),
            )
        };

    let met = current.map(|current| compare_metric(current, spec.comparator, spec.target));
    ProgramMetricEvaluation {
        metric_id: spec.metric_id.clone(),
        comparator: spec.comparator.to_string(),
        target: spec.target,
        current,
        unit,
        met,
        reason: if current.is_none() { reason } else { None },
    }
}

/// Render a single evaluated Success Metric as a compact, deterministic line
/// for the program-context block (`- <id> <cmp> <target> — current: <value> (<status>)`).
fn format_program_metric_line(evaluation: &ProgramMetricEvaluation) -> String {
    let current = match evaluation.current {
        Some(current) => format_metric_number(current),
        None => "n/a".to_string(),
    };
    let status = match evaluation.met {
        Some(true) => "met",
        Some(false) => "not met",
        None => "unevaluated",
    };
    let mut line = format!(
        "- {} {} {} — current: {} ({})",
        evaluation.metric_id,
        evaluation.comparator,
        format_metric_number(evaluation.target),
        current,
        status
    );
    if let Some(reason) = evaluation
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
    {
        line.push_str(" [");
        line.push_str(reason);
        line.push(']');
    }
    line
}

/// Format a metric value compactly: whole numbers render without a decimal
/// point (counts), fractional values render with up to three decimals (ratios).
fn format_metric_number(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value:.3}")
    }
}

/// Evaluate the Success Metrics declared in a harness goal's program document
/// and render them — with live current values — as a compact block suitable for
/// pre-injection into the deterministic program-context section of a cycle
/// prompt.
///
/// This reuses the same parsing (`parse_program_metric_specs`) and evaluation
/// (`evaluate_program_metric`) used by the on-demand `evaluate_harness` tool, so
/// the pre-injected values match the tool exactly and the loop no longer depends
/// on the model voluntarily calling `evaluate_harness` to see its gradient.
///
/// Returns `Ok(None)` when the goal has no resolvable program document or the
/// document declares no Success Metrics.
pub async fn render_goal_program_metrics_block(
    services: &HarnessServices,
    principal: &str,
    workspace: &str,
    owner_agent_id: &str,
    goal_id: &str,
) -> Result<Option<String>, String> {
    let scoped_store = services.definition_store.for_scope(principal, workspace);
    let owner_record = scoped_store
        .get_definition(owner_agent_id)
        .await
        .map_err(|error| format!("failed to load harness owner `{owner_agent_id}`: {error}"))?
        .ok_or_else(|| format!("harness owner `{owner_agent_id}` was not found in scope"))?;

    let loader = crate::magician_v2::harness::ProgramLoader::new(
        services.artifact_service.workspace().clone(),
    );
    let Some(loaded) = loader
        .load_for_goal(
            principal,
            workspace,
            &owner_record.definition,
            Some(goal_id),
        )
        .await?
    else {
        return Ok(None);
    };
    let (specs, _parse_errors) = parse_program_metric_specs(&loaded.content);
    if specs.is_empty() {
        return Ok(None);
    }

    let scope =
        HarnessScope::resolve(&scoped_store, &owner_record.definition, Some(goal_id)).await?;
    let context = HarnessExecutionContext {
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        owner_record,
        goal_id: Some(goal_id.to_string()),
        task_id: None,
        execution_id: None,
        task: None,
        scope,
    };

    let memory = context.memory_service(services);
    let owner_episodes = memory
        .load_native_episodes(owner_agent_id)
        .await
        .map_err(|error| error.to_string())?;
    let mut scope_episodes = Vec::new();
    for agent_id in context.scope.target_ids() {
        let episodes = memory
            .load_native_episodes(&agent_id)
            .await
            .map_err(|error| error.to_string())?;
        scope_episodes.extend(episodes);
    }

    let scope_ref =
        ScopeRef::system_internal_unauthenticated(&principal.to_string(), &workspace.to_string());
    let scope_tasks = services
        .artifact_service
        .list_tasks(&scope_ref)
        .await
        .map_err(|error| error.to_string())?;

    let proposal_store = ProposalStore::new(scoped_store.storage().clone());
    let all_proposals = proposal_store
        .list_proposals(ProposalFilter::default())
        .await
        .map_err(|error| error.to_string())?;
    let harness_source = proposal_source(&context);

    let lines = specs
        .iter()
        .map(|spec| {
            format_program_metric_line(&evaluate_program_metric(
                spec,
                &owner_episodes,
                &scope_episodes,
                &scope_tasks,
                &all_proposals,
                &harness_source,
            ))
        })
        .collect::<Vec<_>>();
    if lines.is_empty() {
        Ok(None)
    } else {
        Ok(Some(lines.join("\n")))
    }
}

fn metric_window_days(metric_id: &str, prefix: &str) -> Option<i64> {
    let raw = metric_id.strip_prefix(prefix)?.strip_suffix('d')?;
    raw.parse::<i64>().ok().filter(|days| *days > 0)
}

fn success_rate_for_recent_episodes(episodes: &[V3EpisodeRecord], days: i64) -> Option<f64> {
    let cutoff = Utc::now() - Duration::days(days);
    let recent = episodes
        .iter()
        .filter(|episode| {
            episode
                .completed_at_dt()
                .map(|completed_at| completed_at >= cutoff)
                .unwrap_or(false)
        })
        .cloned()
        .collect::<Vec<_>>();
    episode_success_rate(&recent)
}

fn recent_completed_tasks(tasks: &[TaskListItemV3], days: i64) -> usize {
    let cutoff = Utc::now() - Duration::days(days);
    tasks
        .iter()
        .filter(|task| task.status == "completed")
        .filter(|task| {
            DateTime::parse_from_rfc3339(&task.updated_at)
                .map(|value| value.with_timezone(&Utc) >= cutoff)
                .unwrap_or(false)
        })
        .count()
}

fn open_tasks(tasks: &[TaskListItemV3]) -> usize {
    tasks
        .iter()
        .filter(|task| !matches!(task.status.as_str(), "completed" | "cancelled"))
        .count()
}

fn recent_applied_proposals(
    proposals: &[crate::magician_v2::agents::DefinitionProposal],
    harness_source: &str,
    days: i64,
) -> usize {
    let cutoff = Utc::now() - Duration::days(days);
    proposals
        .iter()
        .filter(|proposal| proposal.source == harness_source)
        .filter_map(|proposal| proposal.applied_at.as_ref().cloned())
        .filter(|applied_at| *applied_at >= cutoff)
        .count()
}

fn compare_metric(current: f64, comparator: &str, target: f64) -> bool {
    match comparator {
        ">=" => current >= target,
        "<=" => current <= target,
        ">" => current > target,
        "<" => current < target,
        "=" => (current - target).abs() < f64::EPSILON,
        _ => false,
    }
}

fn proposal_action_kind_value(
    proposal: &crate::magician_v2::agents::DefinitionProposal,
) -> Option<String> {
    proposal
        .payload
        .get("action_kind")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

impl TaskStats {
    fn from_tasks(tasks: Option<&[TaskListItemV3]>) -> Self {
        let mut stats = Self::default();
        let Some(tasks) = tasks else {
            return stats;
        };
        for task in tasks {
            stats.total += 1;
            match task.status.as_str() {
                "pending" => stats.pending += 1,
                "planning" => stats.planning += 1,
                "ready" => stats.ready += 1,
                "running" => stats.running += 1,
                "paused" => stats.paused += 1,
                "completed" => stats.completed += 1,
                "failed" => stats.failed += 1,
                "cancelled" => stats.cancelled += 1,
                "deferred" => stats.deferred += 1,
                _ => {},
            }
        }
        stats
    }
}

fn derive_agent_status(paused: bool, active_cycles: usize, task_stats: &TaskStats) -> &'static str {
    if paused {
        "paused"
    } else if task_stats.running > 0 || task_stats.planning > 0 {
        "working"
    } else if task_stats.paused > 0 {
        "paused"
    } else if active_cycles > 0 {
        "working"
    } else if task_stats.failed > 0 {
        "stuck"
    } else {
        "idle"
    }
}

fn task_preview(task: &TaskListItemV3) -> TaskPreview {
    TaskPreview {
        task_id: task.id.clone(),
        title: task.title.clone(),
        status: task.status.clone(),
        priority: task.priority.clone(),
        due_date: task.due_date.clone(),
        updated_at: task.updated_at.clone(),
    }
}

fn parse_proposal_status(raw: &str) -> Result<ProposalStatus, ExecutionError> {
    match raw.trim() {
        "pending" => Ok(ProposalStatus::Pending),
        "approved" => Ok(ProposalStatus::Approved),
        "rejected" => Ok(ProposalStatus::Rejected),
        "deferred" => Ok(ProposalStatus::Deferred),
        other => Err(ExecutionError::Step(format!(
            "{LIST_PROPOSALS_TOOL_NAME}: unsupported proposal status `{other}`"
        ))),
    }
}

fn required_param_string(
    params: &HashMap<String, Value>,
    key: &str,
    tool_name: &str,
) -> Result<String, ExecutionError> {
    params
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            ExecutionError::Step(format!("{tool_name}: missing required `{key}` parameter"))
        })
}

fn optional_string(params: &HashMap<String, Value>, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn optional_bool(params: &HashMap<String, Value>, key: &str) -> Option<bool> {
    params.get(key).and_then(|value| value.as_bool())
}

fn optional_usize(params: &HashMap<String, Value>, key: &str) -> Option<usize> {
    params
        .get(key)
        .and_then(|value| value.as_u64())
        .and_then(|value| usize::try_from(value).ok())
}

fn optional_i64(params: &HashMap<String, Value>, key: &str) -> Option<i64> {
    params.get(key).and_then(|value| value.as_i64())
}

fn optional_object(
    params: &HashMap<String, Value>,
    key: &str,
    tool_name: &str,
) -> Result<Option<Map<String, Value>>, ExecutionError> {
    match params.get(key) {
        Some(Value::Object(object)) => Ok(Some(object.clone())),
        Some(_) => Err(ExecutionError::Step(format!(
            "{tool_name}: `{key}` must be an object"
        ))),
        None => Ok(None),
    }
}

fn required_object_value(
    params: &HashMap<String, Value>,
    key: &str,
    tool_name: &str,
) -> Result<Value, ExecutionError> {
    match params.get(key) {
        Some(Value::Object(object)) => Ok(Value::Object(object.clone())),
        Some(_) => Err(ExecutionError::Step(format!(
            "{tool_name}: `{key}` must be an object"
        ))),
        None => Err(ExecutionError::Step(format!(
            "{tool_name}: missing required `{key}` parameter"
        ))),
    }
}

fn required_string_array(
    params: &HashMap<String, Value>,
    key: &str,
    tool_name: &str,
) -> Result<Vec<String>, ExecutionError> {
    let values = optional_string_array(params, key, tool_name)?;
    if values.is_empty() {
        return Err(ExecutionError::Step(format!(
            "{tool_name}: `{key}` must contain at least one entry"
        )));
    }
    Ok(values)
}

fn optional_string_array(
    params: &HashMap<String, Value>,
    key: &str,
    tool_name: &str,
) -> Result<Vec<String>, ExecutionError> {
    let Some(value) = params.get(key) else {
        return Ok(Vec::new());
    };
    let Value::Array(entries) = value else {
        return Err(ExecutionError::Step(format!(
            "{tool_name}: `{key}` must be an array of strings"
        )));
    };

    let mut normalized = Vec::new();
    for entry in entries {
        let Some(text) = entry
            .as_str()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Err(ExecutionError::Step(format!(
                "{tool_name}: `{key}` must contain only non-empty strings"
            )));
        };
        normalized.push(text.to_string());
    }
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

async fn validated_reference_task_ids(
    services: &HarnessServices,
    context: &HarnessExecutionContext,
    params: &HashMap<String, Value>,
    tool_name: &str,
) -> Result<Vec<String>, ExecutionError> {
    let ids = parse_reference_task_ids(params, tool_name)?;
    if ids.is_empty() {
        return Ok(ids);
    }

    let scope = scope_ref(context);
    for task_id in &ids {
        match services.artifact_service.get_task(&scope, task_id).await {
            Ok(task) if task.state.status == "completed" => {},
            Ok(task) => {
                return Err(ExecutionError::Step(format!(
                    "{tool_name}: reference_task_ids must refer to completed tasks; `{task_id}` is currently {}",
                    task.state.status
                )));
            },
            Err(error) => {
                return Err(ExecutionError::Step(format!(
                    "{tool_name}: reference_task_ids contains a task that is not available in this scope: `{task_id}`: {error}"
                )));
            },
        }
    }

    Ok(ids)
}

fn parse_reference_task_ids(
    params: &HashMap<String, Value>,
    tool_name: &str,
) -> Result<Vec<String>, ExecutionError> {
    let mut ids = Vec::new();
    for key in ["reference_task_ids", "reference_task_id", "depends_on"] {
        let Some(value) = params.get(key) else {
            continue;
        };
        parse_reference_task_id_value(key, value, tool_name, &mut ids)?;
    }
    Ok(ids)
}

fn parse_reference_task_id_value(
    key: &str,
    value: &Value,
    tool_name: &str,
    ids: &mut Vec<String>,
) -> Result<(), ExecutionError> {
    match value {
        Value::Null => Ok(()),
        Value::String(raw) => push_reference_task_id(key, raw, tool_name, ids),
        Value::Array(values) => {
            for value in values {
                match value {
                    Value::Null => {},
                    Value::String(raw) => push_reference_task_id(key, raw, tool_name, ids)?,
                    _ => {
                        return Err(ExecutionError::Step(format!(
                            "{tool_name}: `{key}` must contain only task id strings"
                        )));
                    },
                }
            }
            Ok(())
        },
        _ => Err(ExecutionError::Step(format!(
            "{tool_name}: `{key}` must be a task id string or an array of task id strings"
        ))),
    }
}

fn push_reference_task_id(
    key: &str,
    raw: &str,
    tool_name: &str,
    ids: &mut Vec<String>,
) -> Result<(), ExecutionError> {
    let id = raw.trim();
    if id.is_empty() {
        return Ok(());
    }
    if id
        .chars()
        .any(|ch| ch == '/' || ch == '\\' || ch == '\0' || ch.is_control())
    {
        return Err(ExecutionError::Step(format!(
            "{tool_name}: `{key}` contains an invalid task id; pass plain task ids such as `task_...`"
        )));
    }
    if !ids.iter().any(|existing| existing == id) {
        ids.push(id.to_string());
    }
    Ok(())
}

fn apply_json_merge_patch(target: &mut Value, patch: &Value) {
    let Value::Object(patch_map) = patch else {
        *target = patch.clone();
        return;
    };

    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    let Value::Object(target_map) = target else {
        return;
    };

    for (key, patch_value) in patch_map {
        if patch_value.is_null() {
            target_map.remove(key);
            continue;
        }
        match target_map.get_mut(key) {
            Some(entry) if entry.is_object() && patch_value.is_object() => {
                apply_json_merge_patch(entry, patch_value);
            },
            _ => {
                target_map.insert(key.clone(), patch_value.clone());
            },
        }
    }
}

fn pin_immutable_definition_fields(target: &mut Value, current: &AgentDefinition) {
    let Value::Object(object) = target else {
        return;
    };

    object.insert(
        "agent_id".to_string(),
        Value::String(current.agent_id.clone()),
    );
    object.insert("version".to_string(), serde_json::json!(current.version));
    match current.principal.clone() {
        Some(principal) => {
            object.insert("principal".to_string(), Value::String(principal));
        },
        None => {
            object.remove("principal");
        },
    }
    match current.workspace.clone() {
        Some(workspace) => {
            object.insert("workspace".to_string(), Value::String(workspace));
        },
        None => {
            object.remove("workspace");
        },
    }
    object.insert("is_primary".to_string(), Value::Bool(current.is_primary));
    object.insert(
        "onboarding_completed".to_string(),
        Value::Bool(current.onboarding_completed),
    );
}

fn parse_task_priority(raw: &str, tool_name: &str) -> Result<String, ExecutionError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "urgent" | "p1" => Ok("p1".to_string()),
        "high" | "p2" => Ok("p2".to_string()),
        "medium" | "p3" => Ok("p3".to_string()),
        "low" | "p4" => Ok("p4".to_string()),
        other => Err(ExecutionError::Step(format!(
            "{tool_name}: unsupported priority `{other}`; use one of `urgent`, `high`, `medium`, `low`, `p1`, `p2`, `p3`, `p4`"
        ))),
    }
}

fn parse_task_output_mode(raw: &str, tool_name: &str) -> Result<TaskOutputMode, ExecutionError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "accumulate" => Ok(TaskOutputMode::Accumulate),
        "overwrite" => Ok(TaskOutputMode::Overwrite),
        other => Err(ExecutionError::Step(format!(
            "{tool_name}: unsupported output_mode `{other}`; use `accumulate` or `overwrite`"
        ))),
    }
}

fn scope_ref(context: &HarnessExecutionContext) -> ScopeRef {
    ScopeRef::system_internal_unauthenticated(
        &context.principal.clone(),
        &context.workspace.clone(),
    )
}

fn current_ui_thread_id(context: &HarnessExecutionContext) -> String {
    context
        .task
        .as_ref()
        .map(|task| task.manifest.ui_thread_id.trim())
        .filter(|ui_thread_id| !ui_thread_id.is_empty())
        .map(str::to_string)
        .unwrap_or_else(default_ui_thread_id)
}

fn harness_created_by(context: &HarnessExecutionContext) -> String {
    format!(
        "system:harness:{}",
        context.owner_record.definition.agent_id.as_str()
    )
}

/// Autonomous harness work is implementation machinery by default. A harness
/// may create a durable user commitment only through the explicit, narrowly
/// documented `user_visible: true` tool argument.
fn harness_created_task_lifecycle(
    params: &HashMap<String, Value>,
) -> crate::magician_v2::artifact_v2::models::TaskLifecycle {
    if optional_bool(params, "user_visible").unwrap_or(false) {
        crate::magician_v2::artifact_v2::models::TaskLifecycle::Persistent
    } else {
        crate::magician_v2::artifact_v2::models::TaskLifecycle::Internal
    }
}

fn build_reassignment_description(
    params: &HashMap<String, Value>,
    task: &TaskRecord,
    context: &HarnessExecutionContext,
    new_agent_id: &str,
    reason: Option<&str>,
) -> String {
    if let Some(description) = optional_string(params, "description") {
        return description;
    }

    let mut description = task.manifest.description.trim().to_string();
    if !description.is_empty() {
        description.push_str("\n\n");
    }
    description.push_str("Harness reassignment: ");
    description.push_str(&task.manifest.agent_id);
    description.push_str(" -> ");
    description.push_str(new_agent_id);
    description.push_str(" by ");
    description.push_str(&context.owner_record.definition.agent_id);
    description.push_str(" for task ");
    description.push_str(&task.manifest.task_id);
    description.push('.');

    if let Some(reason) = reason.map(str::trim).filter(|value| !value.is_empty()) {
        description.push_str(" Reason: ");
        description.push_str(reason);
        description.push('.');
    }
    if let Some(focus_area) = context
        .scope
        .focus_area_name()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        description.push_str(" Focus area: ");
        description.push_str(focus_area);
        description.push('.');
    }
    if let Some(goal_id) = context
        .goal_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        description.push_str(" Goal: ");
        description.push_str(goal_id);
        description.push('.');
    }

    description
}

fn proposal_source(context: &HarnessExecutionContext) -> String {
    format!("harness:{}", context.owner_record.definition.agent_id)
}

async fn is_harness_agent_paused(
    services: &HarnessServices,
    principal: &str,
    workspace: &str,
    agent_id: &str,
) -> bool {
    let Some(index) = services.scoped_paused_agents.as_ref() else {
        return false;
    };
    let paused_set = {
        index
            .read()
            .await
            .get(&(principal.to_string(), workspace.to_string()))
            .cloned()
    };
    let Some(paused_set) = paused_set else {
        return false;
    };
    let is_paused = paused_set.read().await.contains(agent_id);
    is_paused
}

fn proposal_visible_to_harness(
    context: &HarnessExecutionContext,
    proposal: &crate::magician_v2::agents::DefinitionProposal,
    requested_agent_id: Option<&str>,
    harness_source: &str,
) -> bool {
    let requested_agent_id = requested_agent_id
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let harness_owned = proposal.source == harness_source;

    if let Some(requested_agent_id) = requested_agent_id {
        if proposal.agent_id != requested_agent_id {
            return false;
        }
        return harness_owned || context.scope.contains(requested_agent_id);
    }

    harness_owned || context.scope.contains(&proposal.agent_id)
}

fn step_error(tool_name: &str, error: impl std::fmt::Display) -> ExecutionError {
    ExecutionError::Step(format!("{tool_name}: {error}"))
}

fn register_harness_providers_for(
    registry: &Arc<super::capability::CapabilityRegistry>,
    services: HarnessServices,
    tool_names: &'static [&'static str],
    skip_if_present: bool,
) {
    for &tool_name in tool_names {
        // Scope-safe re-bind (`skip_if_present`): never clobber a provider the
        // scope registry already bound. `list_agents` is BOTH a harness read
        // tool AND a universal handler-backed `GenericCompiledProvider` that
        // `build_compiled_registry` binds in every scope (returning the FULL
        // roster). Overriding it with the harness `execute_list_agents`
        // (narrower: only harness delegation targets) would silently change a
        // universal read tool's result across all scopes — so on the scope path
        // we register a harness read provider only for tools the scope has NO
        // provider for. The base-registry path keeps `skip_if_present = false`,
        // where the harness `list_agents` override IS intended.
        if skip_if_present && registry.has(tool_name) {
            continue;
        }
        if let Some(pack_def) = registry.get_pack_definition(tool_name) {
            let provider = HarnessCapabilityProvider::new(tool_name, services.clone())
                .with_pack_def(pack_def.clone());
            registry.register_override(Arc::new(provider));
            if let Some(description) = &pack_def.description {
                registry.set_description(tool_name, description.clone());
            }
            if let Some(guide) = &pack_def.guide {
                registry.set_guide(tool_name, guide.clone());
            }
            if !pack_def.parameters.is_empty() {
                registry.set_param_defs(tool_name, pack_def.parameters.clone());
            }
        }
    }
}

pub fn register_harness_read_providers(
    registry: &Arc<super::capability::CapabilityRegistry>,
    services: HarnessServices,
) {
    register_harness_providers_for(registry, services, HARNESS_READ_TOOL_NAMES, false);
}

/// Scope-safe variant of [`register_harness_read_providers`]: binds a harness
/// read provider only when the scope registry does NOT already have one for
/// that tool. Used by `ScopedCapabilityResolver::registry_for_scope` so the
/// per-scope re-bind fills in the genuinely-unbound harness introspection tools
/// (`list_episodes`, `magician_work_ledger`, …) without clobbering the
/// universal handler-backed `list_agents` provider that `build_compiled_registry`
/// already installed (see the note in `register_harness_providers_for`).
pub fn register_harness_read_providers_if_absent(
    registry: &Arc<super::capability::CapabilityRegistry>,
    services: HarnessServices,
) {
    register_harness_providers_for(registry, services, HARNESS_READ_TOOL_NAMES, true);
}

pub fn register_harness_action_providers(
    registry: &Arc<super::capability::CapabilityRegistry>,
    services: HarnessServices,
) {
    register_harness_providers_for(registry, services, HARNESS_ACTION_TOOL_NAMES, false);
}

/// Scope-safe binder for the FULL harness ACTION set (`HARNESS_ACTION_TOOL_NAMES`)
/// so every action tool dispatches inside an autonomous cycle. The
/// autonomous/harness path dispatches through the per-scope registry built by
/// `build_compiled_registry`, which only carries handler-backed providers — so a
/// `HarnessCapabilityProvider`-only action tool is visible to the agent (its
/// deferred pack-def is kept) but returns `ungranted_capability_error` at call
/// time unless rebound here. `if_absent` avoids clobbering any handler-backed
/// provider already installed (e.g. `create_task`). The consequential mutation
/// tools (`create_agent`, `update_agent`, `retire_agent`, `update_delegation`,
/// `update_program_state`) are protected by the owner-approval gate — a central
/// `requires_approval` rule set (see `agents::approval::harness_mutation_approval_rules`)
/// — NOT by withholding dispatch here.
pub fn register_harness_action_providers_if_absent(
    registry: &Arc<super::capability::CapabilityRegistry>,
    services: HarnessServices,
) {
    register_harness_providers_for(registry, services, HARNESS_ACTION_TOOL_NAMES, true);
}

pub async fn build_harness_operator_overview(
    services: &HarnessServices,
    principal: &str,
    workspace: &str,
    owner_agent_id: &str,
) -> Result<Value, String> {
    let mut context_params = HashMap::new();
    context_params.insert(
        "__principal".to_string(),
        Value::String(principal.to_string()),
    );
    context_params.insert(
        "__workspace".to_string(),
        Value::String(workspace.to_string()),
    );
    context_params.insert(
        "__agent_id".to_string(),
        Value::String(owner_agent_id.to_string()),
    );

    let context = HarnessExecutionContext::from_params(services, &context_params).await?;
    let system_status = execute_system_status(services, &context, &HashMap::new())
        .await
        .map_err(|error| error.to_string())?;
    let evaluation = execute_evaluate_harness(services, &context, &HashMap::new())
        .await
        .map_err(|error| error.to_string())?;

    let scoped_store = context.scoped_definition_store(services);
    let proposal_store = ProposalStore::new(scoped_store.storage().clone());
    let mut recent_proposals = proposal_store
        .list_proposals(ProposalFilter::default())
        .await
        .map_err(|error| error.to_string())?;
    let source = proposal_source(&context);
    recent_proposals.retain(|proposal| proposal.source == source);
    recent_proposals.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| right.proposal_id.cmp(&left.proposal_id))
    });
    recent_proposals.truncate(12);

    Ok(serde_json::json!({
        "context": build_context_summary(services, &context).await,
        "system_status": system_status,
        "evaluation": evaluation,
        "recent_proposals": recent_proposals,
    }))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::magician_v2::{
        agents::{AgentDefinition, AgentDefinitionStore, AgentMemoryResolver, AgentRuntime},
        artifact_v2::{
            service::CreateTaskInput, workspace::DEFAULT_SCOPE_WORKSPACE, ArtifactV2Service,
        },
        gaui::MuijStorage,
        test_support::build_test_orchestrator,
    };
    use serde_json::json;
    use tempfile::TempDir;

    const TEST_PRINCIPAL: &str = "owner";
    const TEST_WORKSPACE: &str = DEFAULT_SCOPE_WORKSPACE;

    #[test]
    fn format_metric_number_renders_counts_and_ratios() {
        assert_eq!(format_metric_number(4.0), "4");
        assert_eq!(format_metric_number(0.0), "0");
        assert_eq!(format_metric_number(0.8333333), "0.833");
    }

    #[test]
    fn format_program_metric_line_includes_current_and_status() {
        let met = ProgramMetricEvaluation {
            metric_id: "harness_success_rate_30d".to_string(),
            comparator: ">=".to_string(),
            target: 0.7,
            current: Some(0.82),
            unit: "ratio".to_string(),
            met: Some(true),
            reason: None,
        };
        assert_eq!(
            format_program_metric_line(&met),
            "- harness_success_rate_30d >= 0.700 — current: 0.820 (met)"
        );

        let missing = ProgramMetricEvaluation {
            metric_id: "scope_success_rate_30d".to_string(),
            comparator: ">=".to_string(),
            target: 0.6,
            current: None,
            unit: "ratio".to_string(),
            met: None,
            reason: Some("no completed scoped episodes in the requested window".to_string()),
        };
        assert_eq!(
            format_program_metric_line(&missing),
            "- scope_success_rate_30d >= 0.600 — current: n/a (unevaluated) [no completed scoped episodes in the requested window]"
        );
    }

    fn work_outcome_record(
        root: &str,
        agent_id: &str,
        outcome: &str,
        open_loops: &[&str],
        artifacts: &[&str],
        stamped_at: &str,
    ) -> crate::magician_v2::evidence::EvidenceRecord {
        let stamped_ms = chrono::DateTime::parse_from_rfc3339(stamped_at)
            .expect("valid rfc3339")
            .timestamp_millis();
        crate::magician_v2::evidence::EvidenceRecord::from_work_outcome(
            crate::magician_v2::evidence::WorkOutcomeInput {
                root_execution_id: root.to_string(),
                task_id: None,
                agent_id: agent_id.to_string(),
                outcome: outcome.to_string(),
                summary: format!("run {root} outcome {outcome}"),
                artifacts: artifacts.iter().map(|s| s.to_string()).collect(),
                open_loops: open_loops.iter().map(|s| s.to_string()).collect(),
                next_step_hint: None,
                entity_keys: Vec::new(),
                timestamp_ms: stamped_ms,
            },
        )
    }

    #[test]
    fn select_work_ledger_records_filters_sorts_and_projects() {
        // A non-work_outcome record from the same store must be excluded — this
        // tool is a direct ledger read, not a general evidence scan.
        let mut ambient = work_outcome_record(
            "noise",
            "worker-a",
            "success",
            &[],
            &[],
            "2026-06-13T08:00:00+00:00",
        );
        ambient.producer = "ambient_browser".to_string();

        let older = work_outcome_record(
            "run-old",
            "worker-a",
            "failed",
            &["retry deploy"],
            &["pr:100"],
            "2026-06-13T09:00:00+00:00",
        );
        let newer = work_outcome_record(
            "run-new",
            "worker-b",
            "success",
            &[],
            &["pr:200", "doc:x"],
            "2026-06-13T11:00:00+00:00",
        );

        let entries =
            select_work_ledger_records(vec![ambient, older.clone(), newer.clone()], None, None, 20)
                .expect("no since bound never errors");
        assert_eq!(entries.len(), 2, "non-work_outcome record must be dropped");
        // Newest-first.
        assert_eq!(entries[0].evidence_id, "evd:run:run-new");
        assert_eq!(entries[1].evidence_id, "evd:run:run-old");
        // Producer-specific fields are projected out of metadata.
        assert_eq!(entries[0].outcome.as_deref(), Some("success"));
        assert_eq!(entries[0].agent_id.as_deref(), Some("worker-b"));
        assert_eq!(entries[0].artifact_refs, vec!["pr:200", "doc:x"]);
        assert_eq!(entries[1].outcome.as_deref(), Some("failed"));
        assert_eq!(entries[1].open_loops, vec!["retry deploy"]);

        // kind filter keys off the metadata outcome.
        let failed_only = select_work_ledger_records(
            vec![older.clone(), newer.clone()],
            Some("failed"),
            None,
            20,
        )
        .expect("kind filter never errors");
        assert_eq!(failed_only.len(), 1);
        assert_eq!(failed_only[0].evidence_id, "evd:run:run-old");

        // since is an inclusive RFC3339 lower bound on last_seen_at.
        let recent = select_work_ledger_records(
            vec![older.clone(), newer.clone()],
            None,
            Some("2026-06-13T10:00:00+00:00"),
            20,
        )
        .expect("valid since parses");
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].evidence_id, "evd:run:run-new");

        // limit is clamped to [1, WORK_LEDGER_MAX_LIMIT].
        let capped = select_work_ledger_records(vec![older, newer], None, None, 0)
            .expect("no since bound never errors");
        assert_eq!(capped.len(), 1, "limit clamps up to at least 1");
    }

    /// Regression: records are stamped by chrono `to_rfc3339()` (numeric-offset
    /// `+00:00` form), but the pack-def guide tells the LLM to pass the `Z` form.
    /// The `since` filter must compare INSTANTS, not strings — a naive string
    /// `>=` would wrongly drop a record stamped `...+00:00` when `since` is the
    /// equal `...Z` (since `'+'` < `'Z'` lexically).
    #[test]
    fn select_work_ledger_records_since_compares_instants_across_z_and_offset_forms() {
        // Stored in +00:00 form, exactly what from_work_outcome stamps.
        let at_nine = work_outcome_record(
            "run-nine",
            "worker-a",
            "success",
            &[],
            &[],
            "2026-06-13T09:00:00+00:00",
        );
        assert_eq!(
            at_nine.last_seen_at, "2026-06-13T09:00:00+00:00",
            "records stamp the numeric-offset form, so this is the case under test"
        );

        // Z-form `since` at the SAME instant: inclusive bound keeps the record.
        let inclusive = select_work_ledger_records(
            vec![at_nine.clone()],
            None,
            Some("2026-06-13T09:00:00Z"),
            20,
        )
        .expect("Z-form since parses");
        assert_eq!(
            inclusive.len(),
            1,
            "a record at the bound instant is included even when since uses Z and the record uses +00:00"
        );
        assert_eq!(inclusive[0].evidence_id, "evd:run:run-nine");

        // Z-form `since` one second later: the record falls below the bound.
        let excluded =
            select_work_ledger_records(vec![at_nine], None, Some("2026-06-13T09:00:01Z"), 20)
                .expect("Z-form since parses");
        assert!(
            excluded.is_empty(),
            "a record strictly before the since instant is excluded"
        );
    }

    #[test]
    fn select_work_ledger_records_unparseable_since_errors() {
        let record = work_outcome_record(
            "run-x",
            "worker-a",
            "success",
            &[],
            &[],
            "2026-06-13T09:00:00+00:00",
        );
        let err = select_work_ledger_records(vec![record], None, Some("not-a-timestamp"), 20)
            .expect_err(
                "a present-but-unparseable since must be a hard error, not silently ignored",
            );
        match err {
            ExecutionError::Step(message) => {
                assert!(
                    message.contains("since") && message.contains("not-a-timestamp"),
                    "error names the bad `since`: {message}"
                );
            },
            other => panic!("expected ExecutionError::Step, got {other:?}"),
        }
    }

    fn owner_definition() -> AgentDefinition {
        AgentDefinition::from_yaml_str(
            r#"
agent_id: "ceo"
name: "CEO"
persona: "Harness owner"
principal: "owner"
workspace: "default"
tools: []
harness: {}
delegation_targets:
  - "worker-a"
"#,
        )
        .expect("owner definition")
    }

    fn worker_definition() -> AgentDefinition {
        AgentDefinition::from_yaml_str(
            r#"
agent_id: "worker-a"
name: "Worker A"
kind: worker
persona: "Worker"
principal: "owner"
workspace: "default"
tools: []
"#,
        )
        .expect("worker definition")
    }

    fn second_worker_definition() -> AgentDefinition {
        AgentDefinition::from_yaml_str(
            r#"
agent_id: "worker-b"
name: "Worker B"
kind: worker
persona: "Worker"
principal: "owner"
workspace: "default"
tools: []
"#,
        )
        .expect("worker definition")
    }

    async fn harness_services_fixture(tempdir: &TempDir) -> HarnessServices {
        let definition_store = Arc::new(AgentDefinitionStore::with_workspace_root(tempdir.path()));
        let scoped_store = definition_store.for_scope(TEST_PRINCIPAL, TEST_WORKSPACE);
        scoped_store
            .create_definition(owner_definition())
            .await
            .expect("owner definition should persist");
        scoped_store
            .create_definition(worker_definition())
            .await
            .expect("worker definition should persist");
        scoped_store
            .create_definition(second_worker_definition())
            .await
            .expect("second worker definition should persist");

        let orchestrator = build_test_orchestrator(tempdir.path());
        let artifact_service = Arc::new(ArtifactV2Service::with_orchestrator(
            tempdir.path().join("magician_data_v3"),
            orchestrator,
            MuijStorage::new(tempdir.path().join("muij")),
        ));

        HarnessServices {
            definition_store,
            memory_resolver: AgentMemoryResolver::new(tempdir.path()),
            artifact_service,
            runtime: Arc::new(AgentRuntime::new()),
            user_request_service: None,
            control_gate: None,
            scoped_paused_agents: Some(Arc::new(tokio::sync::RwLock::new(
                std::collections::HashMap::new(),
            ))),
        }
    }

    #[tokio::test]
    async fn create_task_defaults_goal_id_from_active_harness_context() {
        let tempdir = TempDir::new().expect("tempdir");
        let services = harness_services_fixture(&tempdir).await;
        let current_goal_id = "harness:ceo:daily-checkin".to_string();
        let active_task = services
            .artifact_service
            .create_task(CreateTaskInput {
                principal: TEST_PRINCIPAL.to_string(),
                workspace: TEST_WORKSPACE.to_string(),
                title: "CEO cycle".to_string(),
                description: "Active harness cycle task".to_string(),
                agent_id: "ceo".to_string(),
                goal_id: Some(current_goal_id.clone()),
                ui_thread_id: default_ui_thread_id(),
                priority: None,
                due_date: None,
                tags: Vec::new(),
                created_by: "user".to_string(),
                depends_on: Vec::new(),
                approved: true,
                schedule: None,
                output_mode: TaskOutputMode::Accumulate,
                chat_session_id: None,
                lifecycle: crate::magician_v2::artifact_v2::models::TaskLifecycle::default(),
                sync_mode: crate::magician_v2::artifact_v2::models::TaskSyncMode::default(),
            })
            .await
            .expect("active task should persist");
        assert_eq!(
            active_task.manifest.goal_id,
            Some(current_goal_id.clone()),
            "fixture task should keep its goal_id in the immediate create response"
        );

        let reloaded_task = services
            .artifact_service
            .get_task_by_id(&active_task.manifest.task_id)
            .await
            .expect("task lookup should succeed")
            .expect("task should be discoverable by id");
        assert_eq!(
            reloaded_task.1.manifest.goal_id,
            Some(current_goal_id.clone()),
            "fixture task should keep its goal_id when reloaded from storage"
        );

        let mut context_params = HashMap::new();
        context_params.insert("__principal".to_string(), json!(TEST_PRINCIPAL));
        context_params.insert("__workspace".to_string(), json!(TEST_WORKSPACE));
        context_params.insert("__agent_id".to_string(), json!("ceo"));
        context_params.insert(
            "__task_id".to_string(),
            json!(active_task.manifest.task_id.clone()),
        );
        let context = HarnessExecutionContext::from_params(&services, &context_params)
            .await
            .expect("context should resolve");
        assert_eq!(
            context.goal_id,
            Some(current_goal_id.clone()),
            "harness context should inherit the active task goal_id"
        );

        let mut params = HashMap::new();
        params.insert("agent_id".to_string(), json!("worker-a"));
        params.insert("title".to_string(), json!("Investigate build failures"));
        params.insert(
            "description".to_string(),
            json!("Review the latest failing build and summarize next actions."),
        );
        params.insert("start_immediately".to_string(), json!(false));

        let result = execute_create_task(&services, &context, &params)
            .await
            .expect("task creation should succeed");
        assert_eq!(
            result["task"]["manifest"]["goal_id"],
            json!(current_goal_id),
            "harness-created tasks should inherit the active semantic goal_id by default"
        );
        assert_eq!(
            result["task"]["manifest"]["lifecycle"],
            json!("internal"),
            "autonomous harness work should stay out of the user's task list by default"
        );

        params.insert("title".to_string(), json!("Prepare customer follow-up"));
        params.insert("user_visible".to_string(), json!(true));
        let visible_result = execute_create_task(&services, &context, &params)
            .await
            .expect("explicit user-visible task creation should succeed");
        assert_eq!(
            visible_result["task"]["manifest"]["lifecycle"],
            json!("persistent"),
            "only an explicit user_visible opt-in should create a user commitment"
        );
    }

    #[test]
    fn harness_created_tasks_require_explicit_user_visible_opt_in() {
        assert_eq!(
            harness_created_task_lifecycle(&HashMap::new()),
            crate::magician_v2::artifact_v2::models::TaskLifecycle::Internal
        );

        let mut params = HashMap::new();
        params.insert("user_visible".to_string(), json!(true));
        assert_eq!(
            harness_created_task_lifecycle(&params),
            crate::magician_v2::artifact_v2::models::TaskLifecycle::Persistent
        );

        params.insert("user_visible".to_string(), json!(false));
        assert_eq!(
            harness_created_task_lifecycle(&params),
            crate::magician_v2::artifact_v2::models::TaskLifecycle::Internal
        );
    }

    #[tokio::test]
    async fn reassign_task_preserves_internal_lifecycle() {
        let tempdir = TempDir::new().expect("tempdir");
        let services = harness_services_fixture(&tempdir).await;
        let scoped_store = services
            .definition_store
            .for_scope(TEST_PRINCIPAL, TEST_WORKSPACE);
        let owner_record = scoped_store
            .get_definition("ceo")
            .await
            .expect("owner lookup should succeed")
            .expect("owner definition should exist");
        let owner_version = owner_record.version();
        let mut owner = owner_record.definition;
        owner.delegation_targets.push("worker-b".to_string());
        scoped_store
            .update_definition("ceo", owner, owner_version)
            .await
            .expect("fixture should admit the reassignment target into harness scope");
        let original_task = services
            .artifact_service
            .create_task(CreateTaskInput {
                principal: TEST_PRINCIPAL.to_string(),
                workspace: TEST_WORKSPACE.to_string(),
                title: "Investigate flaky test".to_string(),
                description: "Track the regression and propose a fix.".to_string(),
                agent_id: "worker-a".to_string(),
                goal_id: None,
                ui_thread_id: default_ui_thread_id(),
                priority: None,
                due_date: None,
                tags: Vec::new(),
                created_by: "system:harness:ceo".to_string(),
                depends_on: Vec::new(),
                approved: true,
                schedule: None,
                output_mode: TaskOutputMode::Accumulate,
                chat_session_id: None,
                lifecycle: crate::magician_v2::artifact_v2::models::TaskLifecycle::Internal,
                sync_mode: crate::magician_v2::artifact_v2::models::TaskSyncMode::default(),
            })
            .await
            .expect("internal source task should persist");

        let mut context_params = HashMap::new();
        context_params.insert("__principal".to_string(), json!(TEST_PRINCIPAL));
        context_params.insert("__workspace".to_string(), json!(TEST_WORKSPACE));
        context_params.insert("__agent_id".to_string(), json!("ceo"));
        let context = HarnessExecutionContext::from_params(&services, &context_params)
            .await
            .expect("context should resolve");

        let mut params = HashMap::new();
        params.insert(
            "task_id".to_string(),
            json!(original_task.manifest.task_id.clone()),
        );
        params.insert("agent_id".to_string(), json!("worker-b"));

        let result = execute_reassign_task(&services, &context, &params)
            .await
            .expect("internal task reassignment should succeed");
        assert_eq!(
            result["new_task"]["manifest"]["lifecycle"],
            json!("internal"),
            "reassignment must change ownership without changing audience"
        );
        assert_eq!(
            result["cancelled_task"]["state"]["status"],
            json!("cancelled")
        );
    }

    #[tokio::test]
    async fn work_ledger_reads_work_outcome_evidence_across_scope() {
        let tempdir = TempDir::new().expect("tempdir");
        let services = harness_services_fixture(&tempdir).await;

        let memory = services
            .memory_resolver
            .resolve_for_scope(TEST_PRINCIPAL, TEST_WORKSPACE)
            .expect("scoped memory service");

        // Two work_outcome ledger records on the in-scope worker + one non-ledger
        // record that must NOT surface through this tool. (The fixture owner only
        // delegates to `worker-a`, so that is the harness scope.)
        memory
            .append_native_evidence(
                "worker-a",
                work_outcome_record(
                    "run-a1",
                    "worker-a",
                    "failed",
                    &["retry deploy"],
                    &["pr:100"],
                    "2026-06-13T09:00:00+00:00",
                ),
            )
            .await
            .expect("append ledger record a1");
        memory
            .append_native_evidence(
                "worker-a",
                work_outcome_record(
                    "run-a2",
                    "worker-a",
                    "success",
                    &[],
                    &["pr:200"],
                    "2026-06-13T11:00:00+00:00",
                ),
            )
            .await
            .expect("append ledger record a2");
        let mut ambient = work_outcome_record(
            "noise",
            "worker-a",
            "success",
            &[],
            &[],
            "2026-06-13T10:00:00+00:00",
        );
        ambient.evidence_id = "evd:ambient".to_string();
        ambient.producer = "ambient_browser".to_string();
        memory
            .append_native_evidence("worker-a", ambient)
            .await
            .expect("append ambient record");

        // A ledger record on an OUT-OF-SCOPE agent must never surface.
        memory
            .append_native_evidence(
                "worker-b",
                work_outcome_record(
                    "run-b1",
                    "worker-b",
                    "success",
                    &[],
                    &["pr:999"],
                    "2026-06-13T12:00:00+00:00",
                ),
            )
            .await
            .expect("append out-of-scope ledger record");

        let mut context_params = HashMap::new();
        context_params.insert("__principal".to_string(), json!(TEST_PRINCIPAL));
        context_params.insert("__workspace".to_string(), json!(TEST_WORKSPACE));
        context_params.insert("__agent_id".to_string(), json!("ceo"));
        let context = HarnessExecutionContext::from_params(&services, &context_params)
            .await
            .expect("context should resolve");

        // No filters: both in-scope ledger records surface, newest-first; the
        // ambient record and the out-of-scope worker-b record are excluded.
        let result = execute_work_ledger(&services, &context, &HashMap::new())
            .await
            .expect("work ledger read should succeed");
        assert_eq!(result["total_count"], json!(2));
        let records = result["records"].as_array().expect("records array");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["evidence_id"], json!("evd:run:run-a2"));
        assert_eq!(records[0]["outcome"], json!("success"));
        assert_eq!(records[1]["evidence_id"], json!("evd:run:run-a1"));
        assert_eq!(records[1]["open_loops"], json!(["retry deploy"]));

        // kind filter keys off the recorded terminal outcome.
        let mut failed_filter = HashMap::new();
        failed_filter.insert("kind".to_string(), json!("failed"));
        let failed_result = execute_work_ledger(&services, &context, &failed_filter)
            .await
            .expect("filtered work ledger read should succeed");
        let failed_records = failed_result["records"].as_array().expect("records array");
        assert_eq!(failed_records.len(), 1);
        assert_eq!(failed_records[0]["evidence_id"], json!("evd:run:run-a1"));

        // Requesting an out-of-scope agent is rejected (scope gating preserved).
        let mut out_of_scope = HashMap::new();
        out_of_scope.insert("agent_id".to_string(), json!("stranger"));
        assert!(
            execute_work_ledger(&services, &context, &out_of_scope)
                .await
                .is_err(),
            "requesting an out-of-scope agent must be rejected"
        );
    }

    #[tokio::test]
    async fn create_task_preserves_completed_reference_task_ids() {
        let tempdir = TempDir::new().expect("tempdir");
        let services = harness_services_fixture(&tempdir).await;
        let source_task = services
            .artifact_service
            .create_task(CreateTaskInput {
                principal: TEST_PRINCIPAL.to_string(),
                workspace: TEST_WORKSPACE.to_string(),
                title: "Completed analysis".to_string(),
                description: "Prior result to continue from.".to_string(),
                agent_id: "worker-a".to_string(),
                goal_id: None,
                ui_thread_id: default_ui_thread_id(),
                priority: None,
                due_date: None,
                tags: Vec::new(),
                created_by: "user".to_string(),
                depends_on: Vec::new(),
                approved: true,
                schedule: None,
                output_mode: TaskOutputMode::Accumulate,
                chat_session_id: None,
                lifecycle: crate::magician_v2::artifact_v2::models::TaskLifecycle::default(),
                sync_mode: crate::magician_v2::artifact_v2::models::TaskSyncMode::default(),
            })
            .await
            .expect("source task should persist");
        let scope = ScopeRef::system_internal_unauthenticated(
            &TEST_PRINCIPAL.to_string(),
            &TEST_WORKSPACE.to_string(),
        );
        services
            .artifact_service
            .update_task_status(&scope, &source_task.manifest.task_id, "completed")
            .await
            .expect("source task should be completed");

        let mut context_params = HashMap::new();
        context_params.insert("__principal".to_string(), json!(TEST_PRINCIPAL));
        context_params.insert("__workspace".to_string(), json!(TEST_WORKSPACE));
        context_params.insert("__agent_id".to_string(), json!("ceo"));
        let context = HarnessExecutionContext::from_params(&services, &context_params)
            .await
            .expect("context should resolve");

        let mut params = HashMap::new();
        params.insert("agent_id".to_string(), json!("worker-a"));
        params.insert("title".to_string(), json!("Continue the analysis"));
        params.insert(
            "description".to_string(),
            json!("Use the previous completed analysis as read-only context."),
        );
        params.insert(
            "reference_task_ids".to_string(),
            json!([source_task.manifest.task_id.clone()]),
        );
        params.insert("start_immediately".to_string(), json!(false));

        let result = execute_create_task(&services, &context, &params)
            .await
            .expect("task creation should preserve completed references");
        assert_eq!(
            result["task"]["manifest"]["depends_on"],
            json!([source_task.manifest.task_id]),
            "harness-created continuation tasks should keep reference_task_ids"
        );
    }

    #[tokio::test]
    async fn create_task_rejects_unfinished_reference_task_ids() {
        let tempdir = TempDir::new().expect("tempdir");
        let services = harness_services_fixture(&tempdir).await;
        let source_task = services
            .artifact_service
            .create_task(CreateTaskInput {
                principal: TEST_PRINCIPAL.to_string(),
                workspace: TEST_WORKSPACE.to_string(),
                title: "Still running".to_string(),
                description: "This task is not complete yet.".to_string(),
                agent_id: "worker-a".to_string(),
                goal_id: None,
                ui_thread_id: default_ui_thread_id(),
                priority: None,
                due_date: None,
                tags: Vec::new(),
                created_by: "user".to_string(),
                depends_on: Vec::new(),
                approved: true,
                schedule: None,
                output_mode: TaskOutputMode::Accumulate,
                chat_session_id: None,
                lifecycle: crate::magician_v2::artifact_v2::models::TaskLifecycle::default(),
                sync_mode: crate::magician_v2::artifact_v2::models::TaskSyncMode::default(),
            })
            .await
            .expect("source task should persist");

        let mut context_params = HashMap::new();
        context_params.insert("__principal".to_string(), json!(TEST_PRINCIPAL));
        context_params.insert("__workspace".to_string(), json!(TEST_WORKSPACE));
        context_params.insert("__agent_id".to_string(), json!("ceo"));
        let context = HarnessExecutionContext::from_params(&services, &context_params)
            .await
            .expect("context should resolve");

        let mut params = HashMap::new();
        params.insert("agent_id".to_string(), json!("worker-a"));
        params.insert("title".to_string(), json!("Premature continuation"));
        params.insert(
            "description".to_string(),
            json!("This should not be linked until the source completes."),
        );
        params.insert(
            "reference_task_ids".to_string(),
            json!([source_task.manifest.task_id.clone()]),
        );

        let err = execute_create_task(&services, &context, &params)
            .await
            .expect_err("unfinished reference task should be rejected");
        assert!(
            err.to_string().contains("must refer to completed tasks"),
            "unexpected error: {err}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reassign_task_create_failure_does_not_cancel_original_task() {
        let tempdir = TempDir::new().expect("tempdir");
        let services = harness_services_fixture(&tempdir).await;
        let original_task = services
            .artifact_service
            .create_task(CreateTaskInput {
                principal: TEST_PRINCIPAL.to_string(),
                workspace: TEST_WORKSPACE.to_string(),
                title: "Investigate flaky test".to_string(),
                description: "Track the regression and propose a fix.".to_string(),
                agent_id: "worker-a".to_string(),
                goal_id: None,
                ui_thread_id: default_ui_thread_id(),
                priority: None,
                due_date: None,
                tags: Vec::new(),
                created_by: "user".to_string(),
                depends_on: Vec::new(),
                approved: true,
                schedule: None,
                output_mode: TaskOutputMode::Accumulate,
                chat_session_id: None,
                lifecycle: crate::magician_v2::artifact_v2::models::TaskLifecycle::default(),
                sync_mode: crate::magician_v2::artifact_v2::models::TaskSyncMode::default(),
            })
            .await
            .expect("original task should persist");

        let tasks_root = services
            .artifact_service
            .workspace()
            .tasks_root(TEST_PRINCIPAL, TEST_WORKSPACE);
        let original_permissions = fs::metadata(&tasks_root)
            .expect("tasks root metadata")
            .permissions();
        let mut readonly_permissions = original_permissions.clone();
        readonly_permissions.set_mode(0o555);
        fs::set_permissions(&tasks_root, readonly_permissions).expect("set readonly tasks root");

        let mut context_params = HashMap::new();
        context_params.insert("__principal".to_string(), json!(TEST_PRINCIPAL));
        context_params.insert("__workspace".to_string(), json!(TEST_WORKSPACE));
        context_params.insert("__agent_id".to_string(), json!("ceo"));
        let context = HarnessExecutionContext::from_params(&services, &context_params)
            .await
            .expect("context should resolve");

        let mut params = HashMap::new();
        params.insert(
            "task_id".to_string(),
            json!(original_task.manifest.task_id.clone()),
        );
        params.insert("agent_id".to_string(), json!("worker-b"));

        let result = execute_reassign_task(&services, &context, &params).await;

        fs::set_permissions(&tasks_root, original_permissions).expect("restore tasks root perms");

        let err = result.expect_err("reassign should fail when replacement task cannot persist");
        let err_text = err.to_string();
        assert!(
            err_text.contains("reassign_task"),
            "error should stay attributed to reassign_task: {err_text}"
        );

        let reloaded = services
            .artifact_service
            .get_task(
                &ScopeRef::system_internal_unauthenticated(
                    &TEST_PRINCIPAL.to_string(),
                    &TEST_WORKSPACE.to_string(),
                ),
                &original_task.manifest.task_id,
            )
            .await
            .expect("original task should remain readable");
        assert_eq!(
            reloaded.state.status, "pending",
            "original task should remain active when reassignment creation fails"
        );
    }

    #[tokio::test]
    async fn create_agent_rejects_out_of_scope_delegation_targets() {
        let tempdir = TempDir::new().expect("tempdir");
        let services = harness_services_fixture(&tempdir).await;
        let mut context_params = HashMap::new();
        context_params.insert("__principal".to_string(), json!(TEST_PRINCIPAL));
        context_params.insert("__workspace".to_string(), json!(TEST_WORKSPACE));
        context_params.insert("__agent_id".to_string(), json!("ceo"));
        let context = HarnessExecutionContext::from_params(&services, &context_params)
            .await
            .expect("context should resolve");

        let mut params = HashMap::new();
        params.insert("summary".to_string(), json!("Create a worker"));
        params.insert("evidence_refs".to_string(), json!(["episode:ep-1"]));
        params.insert(
            "definition".to_string(),
            json!({
                "agent_id": "worker-c",
                "name": "Worker C",
                "kind": "worker",
                "persona": "Worker",
                "tools": ["files"],
                "delegation_targets": ["worker-b"]
            }),
        );

        let err = execute_create_agent(&services, &context, &params)
            .await
            .expect_err("create_agent should reject out-of-scope delegation targets");
        assert!(
            err.to_string()
                .contains("delegation target `worker-b` is outside the active harness scope"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn create_agent_rejects_out_of_scope_readable_agents() {
        let tempdir = TempDir::new().expect("tempdir");
        let services = harness_services_fixture(&tempdir).await;
        let mut context_params = HashMap::new();
        context_params.insert("__principal".to_string(), json!(TEST_PRINCIPAL));
        context_params.insert("__workspace".to_string(), json!(TEST_WORKSPACE));
        context_params.insert("__agent_id".to_string(), json!("ceo"));
        let context = HarnessExecutionContext::from_params(&services, &context_params)
            .await
            .expect("context should resolve");

        let mut params = HashMap::new();
        params.insert("summary".to_string(), json!("Create an analyst"));
        params.insert(
            "evidence_refs".to_string(),
            json!(["episode:ep-readable-1"]),
        );
        params.insert(
            "definition".to_string(),
            json!({
                "agent_id": "analyst-c",
                "name": "Analyst C",
                "persona": "Analyst",
                "principal": TEST_PRINCIPAL,
                "workspace": TEST_WORKSPACE,
                "tools": [],
                "readable_agents": ["worker-b"]
            }),
        );

        let err = execute_create_agent(&services, &context, &params)
            .await
            .expect_err("create_agent should reject out-of-scope readable_agents");
        assert!(
            err.to_string()
                .contains("readable agent `worker-b` is outside the active harness scope"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn update_delegation_rejects_wildcard_target() {
        let tempdir = TempDir::new().expect("tempdir");
        let services = harness_services_fixture(&tempdir).await;
        let mut context_params = HashMap::new();
        context_params.insert("__principal".to_string(), json!(TEST_PRINCIPAL));
        context_params.insert("__workspace".to_string(), json!(TEST_WORKSPACE));
        context_params.insert("__agent_id".to_string(), json!("ceo"));
        let context = HarnessExecutionContext::from_params(&services, &context_params)
            .await
            .expect("context should resolve");

        let mut params = HashMap::new();
        params.insert("agent_id".to_string(), json!("worker-a"));
        params.insert("summary".to_string(), json!("Broaden delegation"));
        params.insert("evidence_refs".to_string(), json!(["episode:ep-2"]));
        params.insert("add".to_string(), json!(["*"]));

        let err = execute_update_delegation(&services, &context, &params)
            .await
            .expect_err("update_delegation should reject wildcard delegation");
        assert!(
            err.to_string().contains("wildcard `*` is not allowed"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn update_delegation_allows_owner_to_adopt_existing_workspace_agent() {
        let tempdir = TempDir::new().expect("tempdir");
        let services = harness_services_fixture(&tempdir).await;
        let mut context_params = HashMap::new();
        context_params.insert("__principal".to_string(), json!(TEST_PRINCIPAL));
        context_params.insert("__workspace".to_string(), json!(TEST_WORKSPACE));
        context_params.insert("__agent_id".to_string(), json!("ceo"));
        let context = HarnessExecutionContext::from_params(&services, &context_params)
            .await
            .expect("context should resolve");

        let mut params = HashMap::new();
        params.insert("agent_id".to_string(), json!("ceo"));
        params.insert(
            "summary".to_string(),
            json!("Adopt worker-b into the direct roster"),
        );
        params.insert("evidence_refs".to_string(), json!(["episode:ep-3"]));
        params.insert("add".to_string(), json!(["worker-b"]));

        let result = execute_update_delegation(&services, &context, &params)
            .await
            .expect("owner should be able to adopt an existing scoped-workspace agent");
        let yaml_after = result["proposal"]["yaml_after"]
            .as_str()
            .expect("yaml_after should be present");
        assert!(
            yaml_after.contains("worker-b"),
            "owner delegation proposal should include worker-b: {yaml_after}"
        );
    }

    #[tokio::test]
    async fn update_delegation_owner_rejects_missing_workspace_agent() {
        let tempdir = TempDir::new().expect("tempdir");
        let services = harness_services_fixture(&tempdir).await;
        let mut context_params = HashMap::new();
        context_params.insert("__principal".to_string(), json!(TEST_PRINCIPAL));
        context_params.insert("__workspace".to_string(), json!(TEST_WORKSPACE));
        context_params.insert("__agent_id".to_string(), json!("ceo"));
        let context = HarnessExecutionContext::from_params(&services, &context_params)
            .await
            .expect("context should resolve");

        let mut params = HashMap::new();
        params.insert("agent_id".to_string(), json!("ceo"));
        params.insert("summary".to_string(), json!("Adopt unknown worker"));
        params.insert("evidence_refs".to_string(), json!(["episode:ep-4"]));
        params.insert("add".to_string(), json!(["worker-z"]));

        let err = execute_update_delegation(&services, &context, &params)
            .await
            .expect_err("owner should not be able to adopt a missing agent");
        assert!(
            err.to_string()
                .contains("delegation target `worker-z` must already exist in the active harness scope workspace"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn list_proposals_includes_harness_owned_owner_and_create_agent_targets() {
        let tempdir = TempDir::new().expect("tempdir");
        let services = harness_services_fixture(&tempdir).await;
        let mut context_params = HashMap::new();
        context_params.insert("__principal".to_string(), json!(TEST_PRINCIPAL));
        context_params.insert("__workspace".to_string(), json!(TEST_WORKSPACE));
        context_params.insert("__agent_id".to_string(), json!("ceo"));
        let context = HarnessExecutionContext::from_params(&services, &context_params)
            .await
            .expect("context should resolve");
        let scoped_store = context.scoped_definition_store(&services);
        let proposal_store = ProposalStore::new(scoped_store.storage().clone());

        let owner_before = owner_definition()
            .to_yaml_string()
            .expect("owner yaml before");
        let mut owner_after = owner_definition();
        owner_after.delegation_targets.push("worker-b".to_string());
        owner_after.delegation_targets.sort();
        owner_after.delegation_targets.dedup();
        let owner_after = owner_after.to_yaml_string().expect("owner yaml after");
        proposal_store
            .create_proposal(NewDefinitionProposal {
                agent_id: "ceo".to_string(),
                source: "harness:ceo".to_string(),
                payload: json!({
                    "action_kind": UPDATE_DELEGATION_TOOL_NAME,
                    "owner_agent_id": "ceo",
                    "summary": "Adopt worker-b into the direct roster"
                }),
                yaml_before: owner_before,
                yaml_after: owner_after,
            })
            .await
            .expect("owner-targeted proposal should persist");

        let worker_c = AgentDefinition::from_yaml_str(
            r#"
agent_id: "worker-c"
name: "Worker C"
kind: worker
persona: "Worker"
principal: "owner"
workspace: "default"
tools:
  - "files"
"#,
        )
        .expect("worker-c definition");
        proposal_store
            .create_proposal(NewDefinitionProposal {
                agent_id: "worker-c".to_string(),
                source: "harness:ceo".to_string(),
                payload: json!({
                    "action_kind": CREATE_AGENT_TOOL_NAME,
                    "owner_agent_id": "ceo",
                    "summary": "Create worker-c"
                }),
                yaml_before: "# agent does not exist yet\n".to_string(),
                yaml_after: worker_c.to_yaml_string().expect("worker-c yaml"),
            })
            .await
            .expect("create-agent proposal should persist");

        let result = execute_list_proposals(&services, &context, &HashMap::new())
            .await
            .expect("list_proposals should succeed");
        let proposals = result["proposals"]
            .as_array()
            .expect("proposal list should be an array");
        let proposal_targets = proposals
            .iter()
            .filter_map(|proposal| proposal["agent_id"].as_str())
            .collect::<Vec<_>>();
        assert!(
            proposal_targets.contains(&"ceo"),
            "owner-targeted harness proposals should remain visible to the harness: {proposal_targets:?}"
        );
        assert!(
            proposal_targets.contains(&"worker-c"),
            "create-agent harness proposals should remain visible to the harness: {proposal_targets:?}"
        );

        let mut owner_only_params = HashMap::new();
        owner_only_params.insert("agent_id".to_string(), json!("ceo"));
        let owner_only = execute_list_proposals(&services, &context, &owner_only_params)
            .await
            .expect("owner-filtered proposal list should succeed");
        let owner_only_targets = owner_only["proposals"]
            .as_array()
            .expect("owner-only proposal list should be an array")
            .iter()
            .filter_map(|proposal| proposal["agent_id"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(owner_only_targets, vec!["ceo"]);
    }

    #[tokio::test]
    async fn system_status_marks_paused_agents_as_paused() {
        let tempdir = TempDir::new().expect("tempdir");
        let services = harness_services_fixture(&tempdir).await;
        let paused_index = services
            .scoped_paused_agents
            .clone()
            .expect("paused agent index should be present");
        let paused_agents = Arc::new(tokio::sync::RwLock::new(std::collections::HashSet::from([
            "worker-a".to_string(),
        ])));
        paused_index.write().await.insert(
            (TEST_PRINCIPAL.to_string(), TEST_WORKSPACE.to_string()),
            paused_agents,
        );

        let mut context_params = HashMap::new();
        context_params.insert("__principal".to_string(), json!(TEST_PRINCIPAL));
        context_params.insert("__workspace".to_string(), json!(TEST_WORKSPACE));
        context_params.insert("__agent_id".to_string(), json!("ceo"));
        let context = HarnessExecutionContext::from_params(&services, &context_params)
            .await
            .expect("context should resolve");

        let result = execute_system_status(&services, &context, &HashMap::new())
            .await
            .expect("system_status should succeed");
        let worker_a = result["agents"]
            .as_array()
            .expect("system status agents should be an array")
            .iter()
            .find(|agent| agent["agent_id"].as_str() == Some("worker-a"))
            .expect("worker-a should be present in system status");
        assert_eq!(worker_a["status"], "paused");
    }

    #[test]
    fn derive_agent_status_treats_task_backed_pauses_as_paused() {
        let task_stats = TaskStats {
            paused: 1,
            ..TaskStats::default()
        };
        assert_eq!(derive_agent_status(false, 1, &task_stats), "paused");
    }

    #[test]
    fn derive_agent_status_treats_planning_tasks_as_working_without_cycle_tracking() {
        let task_stats = TaskStats {
            planning: 1,
            ..TaskStats::default()
        };
        assert_eq!(derive_agent_status(false, 0, &task_stats), "working");
    }

    #[tokio::test]
    async fn create_proposal_rejects_out_of_scope_delegation_targets() {
        let tempdir = TempDir::new().expect("tempdir");
        let services = harness_services_fixture(&tempdir).await;
        let mut context_params = HashMap::new();
        context_params.insert("__principal".to_string(), json!(TEST_PRINCIPAL));
        context_params.insert("__workspace".to_string(), json!(TEST_WORKSPACE));
        context_params.insert("__agent_id".to_string(), json!("ceo"));
        let context = HarnessExecutionContext::from_params(&services, &context_params)
            .await
            .expect("context should resolve");

        let mut params = HashMap::new();
        params.insert("agent_id".to_string(), json!("worker-a"));
        params.insert("summary".to_string(), json!("Expand delegation"));
        params.insert(
            "yaml_after".to_string(),
            json!(
                r#"agent_id: "worker-a"
name: "Worker A"
kind: worker
persona: "Worker"
principal: "owner"
workspace: "default"
tools: []
delegation_targets:
  - "worker-b"
"#
            ),
        );

        let err = execute_create_proposal(&services, &context, &params)
            .await
            .expect_err("create_proposal should reject out-of-scope delegation targets");
        assert!(
            err.to_string()
                .contains("delegation target `worker-b` is outside the active harness scope"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn create_proposal_rejects_out_of_scope_readable_agents() {
        let tempdir = TempDir::new().expect("tempdir");
        let services = harness_services_fixture(&tempdir).await;
        let mut context_params = HashMap::new();
        context_params.insert("__principal".to_string(), json!(TEST_PRINCIPAL));
        context_params.insert("__workspace".to_string(), json!(TEST_WORKSPACE));
        context_params.insert("__agent_id".to_string(), json!("ceo"));
        let context = HarnessExecutionContext::from_params(&services, &context_params)
            .await
            .expect("context should resolve");

        let mut params = HashMap::new();
        params.insert("agent_id".to_string(), json!("worker-a"));
        params.insert("summary".to_string(), json!("Expand read access"));
        params.insert(
            "yaml_after".to_string(),
            json!(
                r#"agent_id: "worker-a"
name: "Worker A"
persona: "Worker A"
principal: "owner"
workspace: "default"
tools: []
readable_agents:
  - "worker-b"
"#
            ),
        );

        let err = execute_create_proposal(&services, &context, &params)
            .await
            .expect_err("create_proposal should reject out-of-scope readable_agents");
        assert!(
            err.to_string()
                .contains("readable agent `worker-b` is outside the active harness scope"),
            "unexpected error: {err}"
        );
    }

    /// A compiled-pack-def whose provider is a `HarnessCapabilityProvider`.
    /// This mirrors what a per-scope registry carries for a harness read tool:
    /// the pack-def is embedded in the scope, but `build_compiled_registry`
    /// never wires a provider for it (it does not carry `HarnessServices`).
    fn harness_read_pack_def(name: &str) -> CapabilityPackDefinition {
        CapabilityPackDefinition {
            name: name.to_string(),
            description: Some(format!("test harness read pack {name}")),
            version: None,
            guide: None,
            native_action_schemas: HashMap::new(),
            parameters: Vec::new(),
            implementation: ImplementationType::Compiled {
                provider_name: name.to_string(),
            },
            execution: None,
            auth: None,
            reliability: None,
            result_projection: None,
        }
    }

    /// Regression for the systemic gap where harness READ tools were auto-granted
    /// to harness agents but had no provider in the per-scope dispatch registry
    /// (their `HarnessCapabilityProvider` needs `HarnessServices`, which
    /// `build_compiled_registry` does not carry). `ScopedCapabilityResolver::
    /// registry_for_scope` now calls `register_harness_read_providers` on the
    /// freshly-built registry when harness services are present — this test
    /// pins the binding contract that call relies on: given the harness read
    /// pack-defs are in the registry (as they are per-scope), the read providers
    /// bind and become dispatchable.
    #[tokio::test]
    async fn register_harness_read_providers_binds_read_tools_when_pack_defs_present() {
        let tempdir = TempDir::new().expect("tempdir");
        let services = harness_services_fixture(&tempdir).await;

        let registry = Arc::new(super::super::capability::CapabilityRegistry::new());
        // The scope embeds the harness read pack-defs — but with no provider
        // (this is exactly the state `build_compiled_registry` leaves them in).
        registry.set_pack_definition(
            WORK_LEDGER_TOOL_NAME,
            harness_read_pack_def(WORK_LEDGER_TOOL_NAME),
        );
        registry.set_pack_definition(
            LIST_EPISODES_TOOL_NAME,
            harness_read_pack_def(LIST_EPISODES_TOOL_NAME),
        );

        // Before the re-bind: pack-defs exist, providers do NOT — this is the
        // dispatch gap ("is not a compiled pack").
        assert!(
            registry.get(WORK_LEDGER_TOOL_NAME).is_none(),
            "harness read tool must have no provider before the resolver re-bind"
        );
        assert!(registry.get(LIST_EPISODES_TOOL_NAME).is_none());

        // This is the exact call `registry_for_scope` now performs.
        register_harness_read_providers(&registry, services.clone());

        // After: every harness read tool whose pack-def exists is now bound and
        // dispatchable. `magician_work_ledger` (the work-ledger read tool) is
        // included — so it now dispatches in real harness runs.
        assert!(
            registry.get(WORK_LEDGER_TOOL_NAME).is_some(),
            "magician_work_ledger must bind a provider after the resolver re-bind"
        );
        assert!(
            registry.get(LIST_EPISODES_TOOL_NAME).is_some(),
            "list_episodes must bind a provider after the resolver re-bind"
        );

        // Guard: the re-bind is pack-def-gated, so a harness read tool whose
        // pack-def is NOT embedded in the scope stays unbound (no spurious
        // provider), and a non-harness tool is never touched.
        assert!(
            registry.get(READ_TRACE_TOOL_NAME).is_none(),
            "harness read tool with no pack-def in scope must stay unbound"
        );
        assert!(
            registry.get("edit_file").is_none(),
            "non-harness tool must be untouched by the harness read re-bind"
        );
    }

    /// A distinct provider type standing in for the universal handler-backed
    /// `list_agents` provider that `build_compiled_registry` installs in every
    /// scope registry (`compiled_handlers/list_agents.rs`, full roster). Its
    /// concrete type differs from `HarnessCapabilityProvider`, so a clobber by
    /// the harness re-bind is detectable both by `Arc::ptr_eq` and by the Debug
    /// type prefix.
    #[derive(Debug)]
    struct SentinelHandlerProvider;

    #[async_trait]
    impl CapabilityProvider for SentinelHandlerProvider {
        fn tool_name(&self) -> &str {
            LIST_AGENTS_TOOL_NAME
        }

        fn lower(&self, _step: &PlanStep) -> Result<MaybeGatedAction, ExecutionError> {
            Err(ExecutionError::Step("sentinel".to_string()))
        }

        async fn execute(
            &self,
            _action: &ExecutableAction,
            _session_id: Option<String>,
            _timeout_secs: u64,
        ) -> Result<ActionResult, ExecutionError> {
            Err(ExecutionError::Step("sentinel".to_string()))
        }
    }

    /// Regression for the review-found clobber: `list_agents` is BOTH in
    /// `HARNESS_READ_TOOL_NAMES` AND already bound in every per-scope registry
    /// by `build_compiled_registry` as a universal handler-backed provider (the
    /// full-roster `list_agents` handler). The scope-path re-bind uses
    /// `register_harness_read_providers_if_absent`, which must NOT clobber a
    /// provider the scope already has — otherwise a universal read tool's result
    /// silently narrows to the harness `execute_list_agents` (harness delegation
    /// targets only) across all scopes. This test pins that: after the scope-safe
    /// re-bind, (1) the pre-bound `list_agents` provider is UNCHANGED (same `Arc`,
    /// still the sentinel/handler type — not `HarnessCapabilityProvider`), and
    /// (2) the genuinely-unbound `magician_work_ledger` / `list_episodes` ARE
    /// newly bound. The base path (`register_harness_read_providers`,
    /// `skip_if_present = false`) is unaffected — its `list_agents` override is
    /// intended.
    #[tokio::test]
    async fn register_harness_read_providers_if_absent_preserves_bound_list_agents() {
        let tempdir = TempDir::new().expect("tempdir");
        let services = harness_services_fixture(&tempdir).await;

        let registry = Arc::new(super::super::capability::CapabilityRegistry::new());

        // The scope embeds pack-defs for all three harness read tools (as it
        // does per-scope), plus — for `list_agents` — the universal
        // handler-backed provider that `build_compiled_registry` installs. The
        // other two arrive with a pack-def but NO provider (the dispatch gap).
        registry.set_pack_definition(
            LIST_AGENTS_TOOL_NAME,
            harness_read_pack_def(LIST_AGENTS_TOOL_NAME),
        );
        registry.set_pack_definition(
            WORK_LEDGER_TOOL_NAME,
            harness_read_pack_def(WORK_LEDGER_TOOL_NAME),
        );
        registry.set_pack_definition(
            LIST_EPISODES_TOOL_NAME,
            harness_read_pack_def(LIST_EPISODES_TOOL_NAME),
        );
        let sentinel: Arc<dyn CapabilityProvider> = Arc::new(SentinelHandlerProvider);
        registry.register_override(Arc::clone(&sentinel));

        // Precondition: `list_agents` is bound (to the sentinel); the other two
        // are unbound.
        let before = registry
            .get(LIST_AGENTS_TOOL_NAME)
            .expect("list_agents must be pre-bound");
        assert!(
            Arc::ptr_eq(&before, &sentinel),
            "list_agents must start as the handler-backed sentinel provider"
        );
        assert!(registry.get(WORK_LEDGER_TOOL_NAME).is_none());
        assert!(registry.get(LIST_EPISODES_TOOL_NAME).is_none());

        // The exact call `registry_for_scope` now performs on the scope path.
        register_harness_read_providers_if_absent(&registry, services.clone());

        // (1) `list_agents` is UNCHANGED — same `Arc`, still the handler-backed
        // sentinel (NOT clobbered by the harness `HarnessCapabilityProvider`).
        let after = registry
            .get(LIST_AGENTS_TOOL_NAME)
            .expect("list_agents must still be bound");
        assert!(
            Arc::ptr_eq(&after, &sentinel),
            "list_agents provider must be the same Arc after the scope-safe re-bind \
             (the if-absent guard must not clobber the handler-backed provider)"
        );
        let after_debug = format!("{after:?}");
        assert!(
            after_debug.contains("SentinelHandlerProvider"),
            "list_agents must remain the handler-backed provider, not the harness one; \
             got {after_debug}"
        );
        assert!(
            !after_debug.contains("HarnessCapabilityProvider"),
            "list_agents must NOT be replaced by HarnessCapabilityProvider; got {after_debug}"
        );

        // (2) The genuinely-unbound harness read tools ARE newly bound (the
        // if-absent path still fills the real gap).
        let ledger = registry
            .get(WORK_LEDGER_TOOL_NAME)
            .expect("magician_work_ledger must bind after the scope-safe re-bind");
        assert!(
            format!("{ledger:?}").contains("HarnessCapabilityProvider"),
            "magician_work_ledger must bind the harness provider"
        );
        assert!(
            registry.get(LIST_EPISODES_TOOL_NAME).is_some(),
            "list_episodes must bind after the scope-safe re-bind"
        );
    }

    #[test]
    fn harness_task_cap_prefers_execution_provenance_and_falls_back_to_task() {
        assert_eq!(
            harness_task_source_tag(Some("task-1"), Some("exec-1")).as_deref(),
            Some("agentic-source-execution:exec-1")
        );
        assert_eq!(
            harness_task_source_tag(Some("task-1"), None).as_deref(),
            Some("agentic-source-task:task-1")
        );
        assert_eq!(harness_task_source_tag(Some("  "), Some("")), None);
    }

    #[test]
    fn harness_task_provenance_merge_is_idempotent() {
        let mut tags = harness_task_provenance_tags(Some("task-1"), Some("exec-1"));
        let original = tags.clone();

        merge_task_tags(
            &mut tags,
            harness_task_provenance_tags(Some("task-1"), Some("exec-1")),
        );

        assert_eq!(tags, original);
    }

    #[test]
    fn owner_approval_required_result_is_nonfatal_and_names_the_owner() {
        let mut extra = serde_json::Map::new();
        extra.insert(
            "backlog_item_id".to_string(),
            serde_json::Value::from("bk_abc"),
        );
        let result = super::owner_approval_required_result(
            crate::magician_v2::harness::PROMOTE_BACKLOG_ITEM_TOOL_NAME,
            "cto-agent",
            "cro",
            extra,
        );
        // A successful (Ok) tool result, NOT an error → the caller does not dead-pause.
        assert_eq!(result["status"], "owner_approval_required");
        assert_eq!(result["requested_owner_agent"], "cto-agent");
        assert_eq!(result["requesting_officer"], "cro");
        assert_eq!(result["backlog_item_id"], "bk_abc"); // extra merged
        let guidance = result["guidance"].as_str().expect("guidance is a string");
        assert!(guidance.contains("cto-agent"));
        assert!(guidance.contains("Do NOT pause"));
    }

    #[test]
    fn backlog_delivery_evidence_excludes_control_artifacts() {
        use crate::magician_v2::artifact_v2::models::PersistedExecutionArtifactRecord;

        let record = |artifact_type: &str| PersistedExecutionArtifactRecord {
            artifact_id: format!("artifact-{artifact_type}"),
            artifact_type: artifact_type.to_string(),
            content_type: "application/json".to_string(),
            payload: json!({}),
            produced_at: chrono::Utc::now().to_rfc3339(),
            source_execution_id: None,
            source_artifact_id: None,
        };
        assert!(!persisted_artifact_is_material_delivery(&record(
            "tool_inline_result"
        )));
        assert!(!persisted_artifact_is_material_delivery(&record(
            "tool_call_evidence"
        )));
        assert!(persisted_artifact_is_material_delivery(&record(
            "task_deliverable"
        )));
    }

    #[tokio::test]
    async fn promoted_backlog_delivery_can_return_to_proposed_for_rework() {
        let tempdir = TempDir::new().expect("tempdir");
        let services = harness_services_fixture(&tempdir).await;
        let mut context_params = HashMap::new();
        context_params.insert("__principal".to_string(), json!(TEST_PRINCIPAL));
        context_params.insert("__workspace".to_string(), json!(TEST_WORKSPACE));
        context_params.insert("__agent_id".to_string(), json!("ceo"));
        let context = HarnessExecutionContext::from_params(&services, &context_params)
            .await
            .expect("context should resolve");
        let store = BacklogStore::new(services.artifact_service.workspace().clone());
        let item = BacklogItem::new(
            TEST_PRINCIPAL,
            TEST_WORKSPACE,
            "cpo",
            "Stand up weekly product digest",
            "Create the scheduled digest and its durable output.",
            BacklogPriority::High,
        );
        store.upsert(&item).expect("backlog item should persist");

        let promoted = execute_promote_backlog_item(
            &services,
            &context,
            &HashMap::from([
                ("backlog_item_id".to_string(), json!(item.id.clone())),
                ("agent_id".to_string(), json!("worker-a")),
                ("start_immediately".to_string(), json!(false)),
            ]),
        )
        .await
        .expect("promotion should create a parked task");
        assert!(promoted["promoted_task_id"].as_str().is_some());

        let inspection = execute_inspect_backlog_delivery(
            &services,
            &context,
            &HashMap::from([("backlog_item_id".to_string(), json!(item.id.clone()))]),
        )
        .await
        .expect("promoted task should be inspectable before review");
        assert_eq!(inspection["task_status"], json!("pending"));
        assert_eq!(inspection["material_evidence_refs"], json!([]));

        let reviewed = execute_review_backlog_delivery(
            &services,
            &context,
            &HashMap::from([
                ("backlog_item_id".to_string(), json!(item.id.clone())),
                ("disposition".to_string(), json!("rework")),
                (
                    "summary".to_string(),
                    json!("The parked task did not execute or produce the requested digest."),
                ),
                (
                    "revised_description".to_string(),
                    json!("Execute and persist the weekly digest before completing."),
                ),
            ]),
        )
        .await
        .expect("terminal ready task should be returnable for rework");
        assert_eq!(reviewed["backlog_status"], json!("proposed"));
        let stored = store
            .get(TEST_PRINCIPAL, TEST_WORKSPACE, &item.id)
            .expect("reviewed item should persist");
        assert_eq!(stored.status, BacklogStatus::Proposed);
        assert_eq!(
            stored.description,
            "Execute and persist the weekly digest before completing."
        );
    }
}
