use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

use anyhow::Context;
use chrono::{DateTime, Utc};

use crate::execution_panel::ExecutionPanelRuntimeStore;
use magician::magician_v2::{
    artifact_v2::{
        execution_artifacts::FilesystemExecutionArtifactIndexStore,
        models::{
            CanonicalEvent, ExecutionTreeNode, ExecutionTreeRecord, OutputRef,
            PersistedExecutionArtifactRecord, TaskOutputsRecord,
        },
        ArtifactV2Error, ArtifactV2Service, ScopeRef, V3ReadApi,
    },
    ask_loop::{AskLoopApi, SessionQuestion, SessionQuestionStatus},
    execution::agentic::{FullPauseStore, PendingPauseInfo, UserInputType},
    execution::{ScreenshotMetadata, ScreenshotStorage},
    execution_panel::types::{
        ExecutionPanelArtifactRef, ExecutionPanelAttentionItem, ExecutionPanelClarificationOption,
        ExecutionPanelClarificationQuestion, ExecutionPanelClarificationSubmission,
        ExecutionPanelDebugState, ExecutionPanelDelegationGroup, ExecutionPanelExecutionContext,
        ExecutionPanelObservation, ExecutionPanelOutputResult, ExecutionPanelOutputState,
        ExecutionPanelOverview, ExecutionPanelRecentRun, ExecutionPanelResponsibilityChild,
        ExecutionPanelResponsibilityState, ExecutionPanelRunState, ExecutionPanelShellEntry,
        ExecutionPanelState, ExecutionPanelTab, ExecutionPanelTaskplanDocument,
        ExecutionPanelTimelineEntry,
    },
    feed::{FeedItem, FeedItemStatus, FeedItemType},
    hitl::{HitlOpenIdentifiers, HitlOpenScope, HitlOpenTarget},
    progress_channel_seam::{event_log::EventLog, ProgressMessage, ProgressMessageKind},
    storage::{TaskStatus, WaitingState},
};
use tracing::warn;

#[derive(Clone)]
pub struct V3ExecutionPanelAdapter {
    service: Arc<ArtifactV2Service>,
    screenshot_storage: Option<Arc<ScreenshotStorage>>,
    ask_loop_api: Option<Arc<AskLoopApi>>,
    pause_store: Option<Arc<FullPauseStore>>,
    runtime_store: Option<ExecutionPanelRuntimeStore>,
    event_log: Option<EventLog>,
}

impl V3ExecutionPanelAdapter {
    pub fn new(service: Arc<ArtifactV2Service>) -> Self {
        Self {
            service,
            screenshot_storage: None,
            ask_loop_api: None,
            pause_store: None,
            runtime_store: None,
            event_log: None,
        }
    }

    pub fn with_runtime_support(
        mut self,
        screenshot_storage: Arc<ScreenshotStorage>,
        ask_loop_api: Arc<AskLoopApi>,
        runtime_store: ExecutionPanelRuntimeStore,
    ) -> Self {
        self.screenshot_storage = Some(screenshot_storage);
        self.ask_loop_api = Some(ask_loop_api);
        self.runtime_store = Some(runtime_store);
        self
    }

    pub fn with_pause_store(mut self, pause_store: Arc<FullPauseStore>) -> Self {
        self.pause_store = Some(pause_store);
        self
    }

    pub fn with_progress_event_log(mut self, event_log: EventLog) -> Self {
        self.event_log = Some(event_log);
        self
    }

    pub async fn get_task_panel_state(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        requested_execution_id: Option<&str>,
    ) -> anyhow::Result<Option<ExecutionPanelState>> {
        let task = match self.service.get_task(scope, task_id).await {
            Ok(task) => task,
            Err(ArtifactV2Error::TaskNotFound(_)) => return Ok(None),
            Err(error) => return Err(anyhow::Error::new(error)),
        };
        let tree = self
            .service
            .get_execution_tree(scope, task_id)
            .await
            .with_context(|| format!("loading V3 execution tree for task {task_id}"))?;
        let task_outputs = self
            .service
            .get_task_outputs(scope, task_id)
            .await
            .with_context(|| format!("loading V3 task outputs for task {task_id}"))?;

        let selected_execution_id = requested_execution_id
            .map(str::to_string)
            .or_else(|| task.state.active_root_execution_id.clone())
            .or_else(|| task.state.latest_root_execution_id.clone())
            .or_else(|| task.state.last_completed_root_execution_id.clone())
            .or_else(|| tree.root_execution_id.clone());
        let selected_node = selected_execution_id
            .as_deref()
            .and_then(|execution_id| find_node(&tree, execution_id));
        let selected_execution = if let Some(execution_id) = selected_execution_id.as_deref() {
            Some(
                self.service
                    .get_execution(scope, task_id, execution_id)
                    .await
                    .with_context(|| {
                        format!("loading V3 execution record {execution_id} for task {task_id}")
                    })?,
            )
        } else {
            None
        };

        let output_result = self
            .build_output_result(
                scope,
                task_id,
                &task_outputs,
                selected_node,
                selected_execution.as_ref(),
            )
            .await?;
        let responsibility = selected_node.map(|node| self.build_responsibility_state(node, &tree));
        // Delegated children are collected BEFORE the events load, because
        // their events have to be loaded too — see below.
        let related_execution_ids = selected_node
            .map(|node| collect_related_execution_ids(&tree, &node.execution_id))
            .unwrap_or_default();
        // **A delegated child's events are this run's events.** They live in
        // the child's own log, so loading only the selected execution left
        // nothing for the activity-log filter to admit: the panel showed the
        // parent's rows, stopped at the delegation, and resumed only when the
        // child was already terminal. Filtering could not fix that — the rows
        // were never read.
        //
        // Merged on `timestamp` (then `seq`, which is per-execution and so not
        // a global order) to give one chronological feed across the tree.
        // Per-child failures are swallowed deliberately: a child whose log is
        // unreadable costs its own rows, never the whole panel.
        let recent_events = if let Some(execution) = selected_execution.as_ref() {
            let mut events = self
                .load_recent_events(scope, task_id, &execution.state.execution_id)
                .await?;
            for child_execution_id in &related_execution_ids {
                match self
                    .load_recent_events(scope, task_id, child_execution_id)
                    .await
                {
                    Ok(mut child_events) => events.append(&mut child_events),
                    Err(error) => tracing::debug!(
                        task_id = %task_id,
                        child_execution_id = %child_execution_id,
                        %error,
                        "delegated child events unavailable for the execution panel"
                    ),
                }
            }
            events.sort_by(|left, right| {
                left.timestamp
                    .cmp(&right.timestamp)
                    .then_with(|| left.seq.cmp(&right.seq))
            });
            events
        } else {
            Vec::new()
        };
        let taskplan = if let Some(execution) = selected_execution.as_ref() {
            self.load_taskplan(scope, task_id, execution).await?
        } else {
            None
        };
        let pending_questions = self
            .load_pending_questions(scope, task_id, selected_execution_id.as_deref())
            .await?;
        let needs_attention = self
            .load_attention_items(scope, &task.manifest.task_id, &task.manifest.ui_thread_id)
            .await?;
        let timeline = self
            .load_timeline(
                scope,
                &task.manifest.task_id,
                selected_execution_id.as_deref(),
                &related_execution_ids,
            )
            .await?;
        let observations = self
            .load_observations(selected_execution_id.as_deref(), &related_execution_ids)
            .await?;
        let shell_entries = self
            .load_shell_entries(selected_execution_id.as_deref(), &related_execution_ids)
            .await;

        let selected_context =
            if let (Some(node), Some(execution)) = (selected_node, selected_execution.as_ref()) {
                Some(
                    self.build_execution_context(
                        scope,
                        task_id,
                        node,
                        execution,
                        output_result.as_ref(),
                    )
                    .await?,
                )
            } else {
                None
            };

        let recent_runs = self
            .build_recent_runs(scope, task_id, &tree, &task_outputs)
            .await?;
        let (selected_execution_outputs, selected_child_outputs) = selected_execution
            .as_ref()
            .map(|execution| {
                (
                    execution.refs.output_refs.clone(),
                    execution.refs.child_output_refs.clone(),
                )
            })
            .unwrap_or_default();
        let selected_execution_artifacts =
            if let Some(execution_id) = selected_execution_id.as_deref() {
                let store =
                    FilesystemExecutionArtifactIndexStore::new(self.service.workspace().clone());
                match store.list_artifacts(scope, task_id, execution_id).await {
                    Ok(records) => Some(
                        records
                            .into_iter()
                            .map(|record| project_execution_artifact(record, execution_id))
                            .collect(),
                    ),
                    Err(error) => {
                        warn!(
                            task_id,
                            execution_id,
                            error = %error,
                            "execution panel could not read selected execution artifacts"
                        );
                        None
                    },
                }
            } else {
                None
            };
        let overview = ExecutionPanelOverview {
            task_id: task.manifest.task_id.clone(),
            execution_id: selected_execution_id.clone(),
            principal: task.manifest.principal.clone(),
            workspace: task.manifest.workspace.clone(),
            ui_thread_id: task.manifest.ui_thread_id.clone(),
            title: task.manifest.title.clone(),
            description: task.manifest.description.clone(),
            status: map_task_status(&task.state.status),
            priority: None,
            assigned_agent_id: task.manifest.agent_id.clone(),
            active_agent_id: selected_node.map(|node| node.agent_id.clone()),
            has_plan: selected_node
                .and_then(|node| node.plan_id.clone())
                .is_some(),
            progress: None,
            current_step: None,
            created_at: parse_rfc3339_millis(&task.manifest.created_at),
            updated_at: parse_rfc3339_millis(&task.state.updated_at),
        };

        let summary = output_result
            .as_ref()
            .and_then(|result| result.summary.clone())
            .or_else(|| {
                responsibility
                    .as_ref()
                    .map(|value| value.responsibility_summary.clone())
            });
        let recent_activity = self.build_recent_activity_feed_items(
            &task,
            selected_execution_id.as_deref(),
            &recent_events,
        );
        let agent_by_execution: HashMap<String, String> = tree
            .nodes
            .iter()
            .map(|node| (node.execution_id.clone(), node.agent_id.clone()))
            .collect();
        let activity_log = build_activity_log(
            &task.manifest.principal,
            &task.manifest.workspace,
            &task.manifest.task_id,
            &task.manifest.ui_thread_id,
            &task.manifest.agent_id,
            selected_execution_id.as_deref(),
            &related_execution_ids,
            &agent_by_execution,
            &recent_events,
        );
        let delegations = build_delegation_groups(&tree, &related_execution_ids, &activity_log);

        Ok(Some(ExecutionPanelState {
            default_tab: if output_result.is_some() {
                ExecutionPanelTab::Output
            } else if selected_execution_id.is_some() {
                ExecutionPanelTab::Run
            } else {
                ExecutionPanelTab::Plan
            },
            overview,
            run: ExecutionPanelRunState {
                summary,
                responsibility,
                pending_questions,
                needs_attention,
                recent_activity,
                activity_log,
                delegations,
            },
            output: ExecutionPanelOutputState {
                result: output_result,
                deliveries: Vec::new(),
                recent_runs,
                selected_execution_outputs,
                selected_child_outputs,
                selected_execution_artifacts,
            },
            debug: ExecutionPanelDebugState {
                selected_execution: selected_context,
                taskplan,
                timeline,
                observations,
                shell_entries,
                latest_error_message: selected_node
                    .filter(|node| map_task_status(&node.status) == TaskStatus::Failed)
                    .map(|node| format!("Execution {} failed", node.execution_id)),
                history_count: tree
                    .nodes
                    .iter()
                    .filter(|node| node.relationship_type == "root")
                    .count(),
                tags: Vec::new(),
            },
        }))
    }

    pub async fn get_execution_panel_state(
        &self,
        scope: &ScopeRef,
        execution_id: &str,
    ) -> anyhow::Result<Option<ExecutionPanelState>> {
        let Some(task_id) = self.find_task_id_for_execution(scope, execution_id).await? else {
            return Ok(None);
        };
        self.get_task_panel_state(scope, &task_id, Some(execution_id))
            .await
    }

    pub async fn resolve_task_refresh_scope_for_execution(
        &self,
        execution_id: &str,
    ) -> anyhow::Result<Option<(String, String, String, String)>> {
        let Some((scope, task_id, root_execution_id)) =
            self.service.find_execution_scope(execution_id).await?
        else {
            return Ok(None);
        };
        Ok(Some((
            scope.principal().to_string(),
            scope.workspace().to_string(),
            task_id,
            root_execution_id,
        )))
    }

    async fn find_task_id_for_execution(
        &self,
        scope: &ScopeRef,
        execution_id: &str,
    ) -> anyhow::Result<Option<String>> {
        let tasks_root = self
            .service
            .workspace()
            .tasks_root(&scope.principal(), &scope.workspace());
        let entries = self
            .service
            .workspace()
            .read_dir_path_or_empty(&tasks_root)
            .await
            .with_context(|| format!("reading V3 tasks root {}", tasks_root.display()))?;
        for entry in entries {
            if !entry.is_dir {
                continue;
            }
            let task_id = entry.file_name;
            let state_path = self.service.workspace().execution_state_path(
                &scope.principal(),
                &scope.workspace(),
                &task_id,
                execution_id,
            );
            if self
                .service
                .workspace()
                .exists_path(&state_path)
                .await
                .unwrap_or(false)
            {
                return Ok(Some(task_id));
            }
        }
        Ok(None)
    }

    async fn build_output_result(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        task_outputs: &TaskOutputsRecord,
        selected_node: Option<&ExecutionTreeNode>,
        selected_execution: Option<&magician::magician_v2::artifact_v2::models::ExecutionRecord>,
    ) -> anyhow::Result<Option<ExecutionPanelOutputResult>> {
        let selected_execution_id = selected_node.map(|node| node.execution_id.as_str());
        let maybe_output = if let (Some(execution_id), Some(primary_user_output_id)) = (
            selected_execution_id,
            task_outputs.primary_user_output_id.as_ref(),
        ) {
            task_outputs
                .outputs
                .iter()
                .find(|output| {
                    output.output_id == *primary_user_output_id
                        && output.source_execution_id.as_deref() == Some(execution_id)
                })
                .cloned()
                .or_else(|| {
                    task_outputs
                        .outputs
                        .iter()
                        .find(|output| {
                            output.audience == "user"
                                && output.source_execution_id.as_deref() == Some(execution_id)
                        })
                        .cloned()
                })
        } else {
            None
        }
        .or_else(|| {
            task_outputs
                .primary_user_output_id
                .as_ref()
                .and_then(|output_id| {
                    task_outputs
                        .outputs
                        .iter()
                        .find(|output| output.output_id == *output_id)
                        .cloned()
                })
        });

        if let Some(output) = maybe_output {
            let summary = self.read_output_summary(scope, task_id, &output).await?;
            return Ok(Some(ExecutionPanelOutputResult {
                summary,
                outcome: selected_node.map(|node| node.status.clone()),
                artifact_names: vec![output_file_name(&output.relative_path)],
            }));
        }

        if let (Some(node), Some(execution)) = (selected_node, selected_execution) {
            if let Some(output_id) = execution.state.primary_execution_output_id.as_ref() {
                if let Some(output) = execution
                    .refs
                    .output_refs
                    .iter()
                    .find(|output| output.output_id == *output_id)
                    .cloned()
                {
                    let summary = self.read_output_summary(scope, task_id, &output).await?;
                    return Ok(Some(ExecutionPanelOutputResult {
                        summary,
                        outcome: Some(node.status.clone()),
                        artifact_names: vec![output_file_name(&output.relative_path)],
                    }));
                }
            }
        }

        Ok(None)
    }

    async fn build_execution_context(
        &self,
        _scope: &ScopeRef,
        _task_id: &str,
        node: &ExecutionTreeNode,
        execution: &magician::magician_v2::artifact_v2::models::ExecutionRecord,
        result: Option<&ExecutionPanelOutputResult>,
    ) -> anyhow::Result<ExecutionPanelExecutionContext> {
        let plan_ref = execution
            .refs
            .plan_refs
            .last()
            .map(|plan| plan.relative_path.clone());
        Ok(ExecutionPanelExecutionContext {
            execution_id: node.execution_id.clone(),
            status: map_task_status(&node.status),
            started_at: Some(parse_rfc3339_millis(&node.started_at)),
            ended_at: node.completed_at.as_deref().map(parse_rfc3339_millis),
            error_message: (map_task_status(&node.status) == TaskStatus::Failed)
                .then(|| format!("Execution {} failed", node.execution_id)),
            summary: result.and_then(|value| value.summary.clone()),
            outcome: result.and_then(|value| value.outcome.clone()),
            artifact_names: result
                .map(|value| value.artifact_names.clone())
                .unwrap_or_default(),
            current_step: None,
            progress: None,
            plan_ref,
            plan_id: node.plan_id.clone(),
            artifact_chain_id: None,
            linked_inputs: Vec::new(),
            step_statuses: Vec::new(),
        })
    }

    async fn build_recent_runs(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        tree: &ExecutionTreeRecord,
        task_outputs: &TaskOutputsRecord,
    ) -> anyhow::Result<Vec<ExecutionPanelRecentRun>> {
        let mut runs = Vec::new();
        let mut root_nodes: Vec<_> = tree
            .nodes
            .iter()
            .filter(|node| node.relationship_type == "root")
            .cloned()
            .collect();
        root_nodes.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        for node in root_nodes {
            let task_user_output = task_outputs.outputs.iter().find(|output| {
                output.audience == "user"
                    && output.source_execution_id.as_deref() == Some(node.execution_id.as_str())
            });
            let completion_summary = if let Some(output) = task_user_output {
                self.read_output_summary(scope, task_id, output).await?
            } else {
                None
            };
            runs.push(ExecutionPanelRecentRun {
                execution_id: node.execution_id.clone(),
                started_at: parse_rfc3339_millis(&node.started_at),
                ended_at: node.completed_at.as_deref().map(parse_rfc3339_millis),
                status: map_task_status(&node.status),
                completion_summary,
                completion_outcome: Some(node.status.clone()),
                completion_artifact_names: task_user_output
                    .map(|output| vec![output_file_name(&output.relative_path)])
                    .unwrap_or_default(),
                current_step: None,
                progress: None,
                error_message: (map_task_status(&node.status) == TaskStatus::Failed)
                    .then(|| format!("Execution {} failed", node.execution_id)),
            });
        }
        Ok(runs)
    }

    async fn load_taskplan(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution: &magician::magician_v2::artifact_v2::models::ExecutionRecord,
    ) -> anyhow::Result<Option<ExecutionPanelTaskplanDocument>> {
        let Some(plan_ref) = execution.refs.plan_refs.last() else {
            return Ok(None);
        };
        let path = self
            .service
            .workspace()
            .task_dir(&scope.principal(), &scope.workspace(), task_id)
            .join(&plan_ref.relative_path);
        let markdown = match self.service.workspace().read_to_string_path(&path).await {
            Ok(markdown) => markdown,
            Err(_) => return Ok(None),
        };
        Ok(Some(ExecutionPanelTaskplanDocument {
            execution_id: execution.state.execution_id.clone(),
            markdown,
        }))
    }

    async fn read_output_summary(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        output: &OutputRef,
    ) -> anyhow::Result<Option<String>> {
        let path = self
            .service
            .workspace()
            .task_dir(&scope.principal(), &scope.workspace(), task_id)
            .join(&output.relative_path);
        let media_type = base_media_type(&output.media_type);
        let text = match media_type {
            "text/markdown" | "text/plain" | "application/xml" | "text/xml" => self
                .service
                .workspace()
                .read_to_string_path(&path)
                .await
                .ok(),
            "application/json" => match self.service.workspace().read_to_string_path(&path).await {
                Ok(raw) => summarize_json_output(&raw),
                Err(_) => None,
            },
            "text/html" => Some(format!(
                "HTML output available at `{}`.",
                output.relative_path
            )),
            _ => Some(format!(
                "Output available at `{}` ({media_type}).",
                output.relative_path
            )),
        };
        Ok(text.map(|value| truncate_text(value.trim().to_string(), 4000)))
    }

    async fn load_recent_events(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
    ) -> anyhow::Result<Vec<CanonicalEvent>> {
        let events_path = self.service.workspace().execution_events_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
            execution_id,
        );
        let commit_path = self.service.workspace().execution_events_commit_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
            execution_id,
        );
        let mut events = self
            .service
            .workspace()
            .read_committed_jsonl_path::<CanonicalEvent, _, _>(&events_path, &commit_path)
            .await
            .with_context(|| {
                format!("loading V3 events for task {task_id} execution {execution_id}")
            })?;
        events.sort_by(|left, right| left.seq.cmp(&right.seq));
        // NOTE: previously truncated to the last 24 events. The deep-work
        // feed (`run.activity_log`) needs the full seq-ordered log, so we
        // keep every event. `build_recent_activity_feed_items` still caps
        // its own summary subset at 6, so the legacy Run tab is unchanged.
        Ok(events)
    }

    async fn load_pending_questions(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: Option<&str>,
    ) -> anyhow::Result<Vec<ExecutionPanelClarificationQuestion>> {
        let Some(execution_id) = execution_id else {
            return Ok(Vec::new());
        };
        let mut pending_questions = Vec::new();
        let mut seen_ids = HashSet::new();

        if let Some(ask_loop_api) = &self.ask_loop_api {
            let questions = ask_loop_api
                .get_pending_session_questions_for_execution(execution_id)
                .await
                .unwrap_or_default();
            for question in questions.iter().map(|question| {
                build_clarification_question(question, scope, task_id, execution_id)
            }) {
                if seen_ids.insert(question.id.clone()) {
                    pending_questions.push(question);
                }
            }
        }

        if let Some(pause_store) = &self.pause_store {
            for pause in pause_store
                .get_pending_for_execution(execution_id)
                .iter()
                .map(build_pause_clarification_question)
            {
                if seen_ids.insert(pause.id.clone()) {
                    pending_questions.push(pause);
                }
            }
        }

        Ok(pending_questions)
    }

    async fn load_attention_items(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        ui_thread_id: &str,
    ) -> anyhow::Result<Vec<ExecutionPanelAttentionItem>> {
        let items = self
            .service
            .list_attention_items(scope, Some(ui_thread_id))
            .await
            .with_context(|| format!("loading V3 attention items for task {task_id}"))?;
        Ok(items
            .into_iter()
            .filter(|item| item.task_id.as_deref() == Some(task_id))
            .map(|item| ExecutionPanelAttentionItem {
                hitl_request: embedded_hitl_target(&item),
                item,
            })
            .collect())
    }

    async fn load_timeline(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        selected_execution_id: Option<&str>,
        related_execution_ids: &[String],
    ) -> anyhow::Result<Vec<ExecutionPanelTimelineEntry>> {
        let Some(event_log) = &self.event_log else {
            return Ok(Vec::new());
        };
        let mut messages = event_log
            .list_recent(
                &scope.principal(),
                &scope.workspace(),
                &format!("task:{task_id}"),
                60,
            )
            .await?;
        messages.extend(
            self.load_child_execution_timeline_messages(scope, related_execution_ids)
                .await?,
        );
        Ok(filter_and_map_timeline(
            messages,
            selected_execution_id,
            related_execution_ids,
        ))
    }

    async fn load_observations(
        &self,
        execution_id: Option<&str>,
        related_execution_ids: &[String],
    ) -> anyhow::Result<Vec<ExecutionPanelObservation>> {
        let Some(execution_id) = execution_id else {
            return Ok(Vec::new());
        };
        let Some(storage) = &self.screenshot_storage else {
            return Ok(Vec::new());
        };

        let mut observations = storage
            .list_observations(execution_id)
            .await
            .with_context(|| format!("listing observations for execution {execution_id}"))?;
        for child_execution_id in related_execution_ids {
            if child_execution_id == execution_id {
                continue;
            }
            let mut child_observations = storage
                .list_observations(child_execution_id)
                .await
                .with_context(|| {
                    format!("listing observations for child execution {child_execution_id}")
                })?;
            observations.append(&mut child_observations);
        }
        observations.sort_by(|left, right| right.captured_at.cmp(&left.captured_at));
        let mut seen = HashSet::new();
        observations.retain(|metadata| seen.insert(metadata.observation_id.clone()));
        observations.truncate(24);
        Ok(observations
            .into_iter()
            .map(map_observation)
            .collect::<Vec<_>>())
    }

    async fn load_shell_entries(
        &self,
        execution_id: Option<&str>,
        related_execution_ids: &[String],
    ) -> Vec<ExecutionPanelShellEntry> {
        let Some(execution_id) = execution_id else {
            return Vec::new();
        };
        let Some(runtime_store) = &self.runtime_store else {
            return Vec::new();
        };
        let mut entries = runtime_store
            .shell_entries_for_execution(execution_id)
            .await;
        for child_execution_id in related_execution_ids {
            if child_execution_id == execution_id {
                continue;
            }
            entries.extend(
                runtime_store
                    .shell_entries_for_execution(child_execution_id)
                    .await,
            );
        }
        entries.sort_by(|left, right| right.started_at.cmp(&left.started_at));
        entries
    }

    async fn load_child_execution_timeline_messages(
        &self,
        scope: &ScopeRef,
        related_execution_ids: &[String],
    ) -> anyhow::Result<Vec<ProgressMessage>> {
        let Some(event_log) = &self.event_log else {
            return Ok(Vec::new());
        };
        let mut merged = Vec::new();
        for execution_id in related_execution_ids {
            merged.extend(
                event_log
                    .list_recent(
                        &scope.principal(),
                        &scope.workspace(),
                        &format!("execution:{execution_id}"),
                        40,
                    )
                    .await?,
            );
        }
        Ok(merged)
    }

    fn build_recent_activity_feed_items(
        &self,
        task: &magician::magician_v2::artifact_v2::models::TaskRecord,
        selected_execution_id: Option<&str>,
        recent_events: &[CanonicalEvent],
    ) -> Vec<FeedItem> {
        // Capped, newest-first summary subset for the legacy Run tab.
        recent_events
            .iter()
            .rev()
            .take(6)
            .filter(|event| {
                selected_execution_id.is_none_or(|execution_id| event.execution_id == execution_id)
            })
            .map(|event| {
                event_to_feed_item(
                    &task.manifest.principal,
                    &task.manifest.workspace,
                    &task.manifest.task_id,
                    &task.manifest.ui_thread_id,
                    &task.manifest.agent_id,
                    event,
                    "activity",
                )
            })
            .collect()
    }

    fn build_responsibility_state(
        &self,
        node: &ExecutionTreeNode,
        tree: &ExecutionTreeRecord,
    ) -> ExecutionPanelResponsibilityState {
        let child_nodes_by_id: HashMap<_, _> = tree
            .nodes
            .iter()
            .map(|candidate| (candidate.execution_id.as_str(), candidate))
            .collect();
        let active_children = node
            .child_execution_ids
            .iter()
            .filter_map(|child_execution_id| {
                let child = child_nodes_by_id.get(child_execution_id.as_str())?;
                let delegation = node
                    .delegation_summary
                    .delegations
                    .iter()
                    .find(|delegation| {
                        delegation.child_execution_id.as_deref()
                            == Some(child.execution_id.as_str())
                    });
                Some(ExecutionPanelResponsibilityChild {
                    execution_id: child.execution_id.clone(),
                    title: delegation.map(|delegation| delegation.sub_goal.clone()),
                    waiting_state: map_waiting_state(&child.status, child.waiting_for_children),
                    active_owner_agent_id: child.agent_id.clone(),
                    delegation_chain: build_execution_chain(tree, &child.execution_id),
                    current_stage: None,
                    current_provider: None,
                    latest_summary: None,
                    is_blocking: node
                        .active_child_execution_ids
                        .iter()
                        .any(|active_id| active_id == &child.execution_id),
                })
            })
            .collect::<Vec<_>>();

        ExecutionPanelResponsibilityState {
            execution_id: node.execution_id.clone(),
            parent_execution_id: node.parent_execution_id.clone(),
            waiting_state: map_waiting_state(&node.status, node.waiting_for_children),
            active_owner_agent_id: node.agent_id.clone(),
            owner_stack: build_owner_chain(tree, &node.execution_id),
            owner_chain: build_owner_chain(tree, &node.execution_id),
            handover_active: false,
            waiting_on_children: node.waiting_for_children,
            active_child_count: node.active_child_execution_ids.len(),
            historical_child_count: node.child_execution_ids.len(),
            responsibility_summary: build_responsibility_summary(node),
            current_stage: None,
            current_provider: None,
            paused_from_state: None,
            latest_summary: None,
            active_children,
        }
    }
}

fn project_execution_artifact(
    record: PersistedExecutionArtifactRecord,
    selected_execution_id: &str,
) -> ExecutionPanelArtifactRef {
    let payload = record.payload.as_object();
    let task_path = payload
        .and_then(|value| value.get("task_relative_path"))
        .and_then(serde_json::Value::as_str)
        .and_then(safe_task_relative_path);
    // Older records use `relative_path` for several incompatible roots. It is
    // task-addressable only when it already names one of the two roots accepted
    // by the task-output route; a bare runtime-download path must not be exposed
    // as a working task file merely because it is syntactically relative.
    let legacy_task_path = payload
        .and_then(|value| value.get("relative_path"))
        .and_then(serde_json::Value::as_str)
        .and_then(safe_task_relative_path)
        .filter(|path| path.starts_with("outputs/") || path.starts_with("executions/"));
    let execution_path = payload
        .and_then(|value| value.get("execution_relative_path"))
        .and_then(serde_json::Value::as_str)
        .and_then(safe_task_relative_path)
        .map(|path| format!("executions/{selected_execution_id}/{path}"));
    let relative_path = task_path.or(legacy_task_path).or(execution_path);
    let display_name = payload
        .and_then(|value| value.get("display_name").or_else(|| value.get("file_name")))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            relative_path
                .as_deref()
                .and_then(|path| path.rsplit('/').next())
                .map(ToOwned::to_owned)
        });
    let size_bytes = payload
        .and_then(|value| value.get("size_bytes"))
        .and_then(serde_json::Value::as_u64);
    let content_type = payload
        .and_then(|value| value.get("content_type").or_else(|| value.get("mime_type")))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or(record.content_type);

    ExecutionPanelArtifactRef {
        artifact_id: record.artifact_id,
        artifact_type: record.artifact_type,
        content_type,
        produced_at: record.produced_at,
        source_execution_id: record.source_execution_id,
        display_name,
        relative_path,
        size_bytes,
    }
}

/// Accept only paths the task-output route can resolve beneath the task root.
/// Absolute paths and traversal components remain metadata-only artifacts.
fn safe_task_relative_path(value: &str) -> Option<String> {
    let value = value.trim().trim_start_matches("./");
    if value.is_empty()
        || value.starts_with('/')
        || value.contains('\\')
        || value.split('/').any(|component| component == "..")
    {
        return None;
    }
    Some(value.to_string())
}

fn find_node<'a>(
    tree: &'a ExecutionTreeRecord,
    execution_id: &str,
) -> Option<&'a ExecutionTreeNode> {
    tree.nodes
        .iter()
        .find(|node| node.execution_id == execution_id)
}

fn build_execution_chain(tree: &ExecutionTreeRecord, execution_id: &str) -> Vec<String> {
    let nodes_by_id: HashMap<_, _> = tree
        .nodes
        .iter()
        .map(|node| (node.execution_id.as_str(), node))
        .collect();
    let mut chain = Vec::new();
    let mut current = nodes_by_id.get(execution_id).copied();
    while let Some(node) = current {
        chain.push(node.execution_id.clone());
        current = node
            .parent_execution_id
            .as_deref()
            .and_then(|parent_id| nodes_by_id.get(parent_id).copied());
    }
    chain.reverse();
    chain
}

/// Every execution whose work belongs under `execution_id` in the panel —
/// the delegated children at any depth, not just the ones still running.
///
/// **`child_execution_ids`, not `active_child_execution_ids`.** The active list
/// is the in-flight delegation group and the parent CLEARS it on settlement, so
/// keying off it made a child's whole history vanish from the panel the moment
/// the run finished — which is exactly when someone opens the drawer to read
/// it. A finished delegation is the normal case, not the edge case.
///
/// Recursive, so nested and repeated delegations are all collected; `visited`
/// keeps a cycle in the tree record from looping.
fn collect_related_execution_ids(tree: &ExecutionTreeRecord, execution_id: &str) -> Vec<String> {
    let nodes_by_id: HashMap<_, _> = tree
        .nodes
        .iter()
        .map(|node| (node.execution_id.as_str(), node))
        .collect();
    let mut pending = vec![execution_id.to_string()];
    let mut visited = HashSet::new();
    let mut related = Vec::new();

    while let Some(current_execution_id) = pending.pop() {
        if !visited.insert(current_execution_id.clone()) {
            continue;
        }
        let Some(node) = nodes_by_id.get(current_execution_id.as_str()) else {
            continue;
        };
        // Union of both lists: the complete history is authoritative, but a
        // child that has been linked and not yet folded into it is still this
        // run's work and must not wait for bookkeeping to appear.
        for child_execution_id in node
            .child_execution_ids
            .iter()
            .chain(node.active_child_execution_ids.iter())
        {
            if visited.contains(child_execution_id) || related.contains(child_execution_id) {
                continue;
            }
            related.push(child_execution_id.clone());
            pending.push(child_execution_id.clone());
        }
    }

    related
}

fn build_owner_chain(tree: &ExecutionTreeRecord, execution_id: &str) -> Vec<String> {
    let nodes_by_id: HashMap<_, _> = tree
        .nodes
        .iter()
        .map(|node| (node.execution_id.as_str(), node))
        .collect();
    let mut chain = Vec::new();
    let mut current = nodes_by_id.get(execution_id).copied();
    while let Some(node) = current {
        chain.push(node.agent_id.clone());
        current = node
            .parent_execution_id
            .as_deref()
            .and_then(|parent_id| nodes_by_id.get(parent_id).copied());
    }
    chain.reverse();
    chain
}

fn filter_and_map_timeline(
    messages: Vec<ProgressMessage>,
    selected_execution_id: Option<&str>,
    related_execution_ids: &[String],
) -> Vec<ExecutionPanelTimelineEntry> {
    let related_execution_ids = related_execution_ids
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    let mut filtered = messages
        .into_iter()
        .filter(|message| seen.insert(message.id.clone()))
        .filter(|message| {
            timeline_matches_execution(message, selected_execution_id, &related_execution_ids)
        })
        .collect::<Vec<_>>();
    filtered.sort_by(|left, right| right.timestamp.cmp(&left.timestamp));
    filtered
        .into_iter()
        .map(build_timeline_entry)
        .collect::<Vec<_>>()
}

fn timeline_matches_execution(
    message: &ProgressMessage,
    selected_execution_id: Option<&str>,
    related_execution_ids: &HashSet<String>,
) -> bool {
    let Some(selected_execution_id) = selected_execution_id else {
        return true;
    };
    let matches_selected = message.execution_id.as_deref() == Some(selected_execution_id)
        || message.root_execution_id.as_deref() == Some(selected_execution_id)
        || message.parent_execution_id.as_deref() == Some(selected_execution_id);
    let matches_related = [
        message.execution_id.as_ref(),
        message.root_execution_id.as_ref(),
        message.parent_execution_id.as_ref(),
    ]
    .into_iter()
    .flatten()
    .any(|execution_id| related_execution_ids.contains(execution_id));

    matches_selected || matches_related
}

fn build_timeline_entry(message: ProgressMessage) -> ExecutionPanelTimelineEntry {
    let (title, detail) = match &message.kind {
        ProgressMessageKind::StatusChanged { status, summary } => (
            format!("Status changed to {}", title_case(status)),
            summary.clone().unwrap_or_else(|| {
                if let Some(execution_id) = message.execution_id.as_deref() {
                    format!("Execution {execution_id} entered {status}")
                } else {
                    format!("Execution entered {status}")
                }
            }),
        ),
        ProgressMessageKind::ActionProgress {
            iteration: _,
            action_type,
            target,
            success,
            error,
        } => (
            if *success {
                title_case(action_type)
            } else {
                format!("Failed {}", action_type)
            },
            error.clone().unwrap_or_else(|| compact_target(target)),
        ),
        ProgressMessageKind::ChildStatusChanged {
            child_execution_id: _,
            status,
            summary,
        } => (
            format!("Delegated Task {}", title_case(status)),
            summary
                .clone()
                .unwrap_or_else(|| format!("Status: {}", status)),
        ),
        ProgressMessageKind::HandedOver {
            from_agent,
            to_agent,
        } => (
            "Agent Handover".to_string(),
            format!("{} -> {}", from_agent, to_agent),
        ),
        ProgressMessageKind::AgentNotification {
            event_type,
            message,
            entity_key: _,
            ..
        } => {
            let title = if event_type == "execution.progress" {
                "Progress".to_string()
            } else {
                title_case(&event_type.replace('.', " "))
            };
            (title, message.clone())
        },
    };

    ExecutionPanelTimelineEntry {
        id: message.id,
        timestamp: message.timestamp,
        severity: message.severity,
        title,
        message: detail,
        execution_id: message.execution_id,
        agent_id: message.agent_id,
        step_id: message.step_id,
    }
}

fn map_observation(metadata: ScreenshotMetadata) -> ExecutionPanelObservation {
    ExecutionPanelObservation {
        observation_id: metadata.observation_id,
        captured_at: metadata.captured_at.timestamp_millis(),
        step_id: metadata.step_id,
        page_stage: metadata.page_stage,
        url: metadata.url,
        has_screenshot: metadata.has_screenshot,
    }
}

fn build_clarification_question(
    question: &SessionQuestion,
    scope: &ScopeRef,
    task_id: &str,
    execution_id: &str,
) -> ExecutionPanelClarificationQuestion {
    let options = question
        .options
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|option| ExecutionPanelClarificationOption {
            value: option.value,
            label: option.label,
            description: option.description,
        })
        .collect::<Vec<_>>();
    let input_type = if options.is_empty() { "text" } else { "choice" };
    let mut input_schema = serde_json::Map::from_iter([(
        "stage".to_string(),
        serde_json::json!(question.stage.as_str()),
    )]);
    if let Some(source_slot_id) = question.source_slot_id.as_deref() {
        input_schema.insert(
            "source_slot_id".to_string(),
            serde_json::json!(source_slot_id),
        );
    }
    if !options.is_empty() {
        input_schema.insert(
            "options".to_string(),
            serde_json::Value::Array(
                options
                    .iter()
                    .map(|option| {
                        serde_json::json!({
                            "id": option.value,
                            "label": option.label,
                            "description": option.description,
                        })
                    })
                    .collect(),
            ),
        );
    }
    ExecutionPanelClarificationQuestion {
        id: question.id.clone(),
        question_text: question.question_text.clone(),
        status: session_question_status(question.status).to_string(),
        context_snippets: question.context_snippets.clone(),
        related_slots: question.related_slots.clone(),
        source_slot_id: question.source_slot_id.clone(),
        slot_confidence: question.slot_confidence,
        options,
        submission: None,
        hitl_request: Some(HitlOpenTarget {
            id: question.id.clone(),
            source: "clarification".to_string(),
            input_type: input_type.to_string(),
            prompt: question.question_text.clone(),
            hint: (!question.context_snippets.is_empty())
                .then(|| question.context_snippets.join("\n")),
            input_schema: serde_json::Value::Object(input_schema),
            identifiers: HitlOpenIdentifiers {
                correlation_id: Some(question.id.clone()),
                ..Default::default()
            },
            scope: HitlOpenScope {
                principal: Some(scope.principal().to_string()),
                workspace: Some(scope.workspace().to_string()),
                workflow_id: Some(execution_id.to_string()),
                task_id: Some(task_id.to_string()),
                execution_id: Some(execution_id.to_string()),
                ..Default::default()
            },
            at: Some(question.created_at.timestamp_millis()),
        }),
    }
}

fn build_pause_clarification_question(
    pause: &PendingPauseInfo,
) -> ExecutionPanelClarificationQuestion {
    let question_text = pause
        .question
        .clone()
        .unwrap_or_else(|| default_pause_question_text(&pause.input_type));
    let context_snippets = build_pause_context_snippets(pause);
    let options = build_pause_options(pause);
    let input_schema = build_pause_input_schema(pause, &options);
    let hitl_identity = match &pause.input_type {
        UserInputType::DiffApproval {
            proposal_id,
            transaction_id,
            ..
        } => [proposal_id.as_deref(), transaction_id.as_deref()]
            .into_iter()
            .flatten()
            .map(str::trim)
            .find(|id| !id.is_empty())
            .map(|id| (id.to_string(), "diff_approval")),
        _ => Some((pause.key.clone(), "agentic")),
    };
    ExecutionPanelClarificationQuestion {
        id: pause.key.clone(),
        question_text: question_text.clone(),
        status: "waiting_on_user".to_string(),
        context_snippets: context_snippets.clone(),
        related_slots: Vec::new(),
        source_slot_id: None,
        slot_confidence: None,
        options,
        submission: Some(ExecutionPanelClarificationSubmission {
            mode: "agentic_resume".to_string(),
            input_type: Some(pause.input_type.type_name().to_string()),
            pause_state_id: Some(pause.key.clone()),
            plan_id: pause.plan_id.clone(),
            step_id: pause.step_id.clone(),
            agent_id: pause.agent_id.clone(),
            goal_id: pause.goal_id.clone(),
            cycle_id: pause.cycle_id.clone(),
        }),
        hitl_request: hitl_identity.map(|(id, source)| HitlOpenTarget {
            id: id.clone(),
            source: source.to_string(),
            input_type: pause.input_type.type_name().to_string(),
            prompt: question_text,
            hint: (!context_snippets.is_empty()).then(|| context_snippets.join("\n")),
            input_schema,
            identifiers: HitlOpenIdentifiers {
                pause_state_id: Some(pause.key.clone()),
                correlation_id: Some(id),
                ..Default::default()
            },
            scope: HitlOpenScope {
                principal: pause.principal.clone(),
                workspace: pause.workspace.clone(),
                workflow_id: None,
                task_id: pause.task_id.clone(),
                execution_id: Some(pause.execution_id.clone()),
                agent_id: pause.agent_id.clone(),
                thread_id: None,
            },
            at: None,
        }),
    }
}

fn embedded_hitl_target(item: &FeedItem) -> Option<HitlOpenTarget> {
    item.metadata
        .get("hitl_request")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn build_pause_context_snippets(pause: &PendingPauseInfo) -> Vec<String> {
    let mut snippets = Vec::new();
    if let Some(hint) = pause
        .hint
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        snippets.push(hint.trim().to_string());
    }
    if let Some(summary) = pause
        .confirmation_action_summary
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        snippets.push(summary.trim().to_string());
    }
    if let Some(reason) = pause
        .confirmation_reason
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        snippets.push(format!("Reason: {}", reason.trim()));
    }
    if pause.is_retry {
        snippets.push(format!("Retry attempt {}.", pause.retry_count.max(1)));
        if let Some(reason) = pause
            .retry_reason
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            snippets.push(format!("Retry reason: {}", reason.trim()));
        }
        if let Some(answer) = pause
            .previous_answer
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            snippets.push(format!("Previous answer: {}", answer.trim()));
        }
    }
    snippets
}

fn build_pause_options(pause: &PendingPauseInfo) -> Vec<ExecutionPanelClarificationOption> {
    match &pause.input_type {
        UserInputType::Choice { options, .. } | UserInputType::MultiChoice { options, .. } => {
            options
                .iter()
                .map(|option| ExecutionPanelClarificationOption {
                    value: option.id.clone(),
                    label: option.label.clone(),
                    description: option.description.clone(),
                })
                .collect()
        },
        UserInputType::Confirmation {
            confirm_label,
            deny_label,
            ..
        } => vec![
            ExecutionPanelClarificationOption {
                value: "confirm".to_string(),
                label: confirm_label
                    .clone()
                    .unwrap_or_else(|| "Confirm".to_string()),
                description: None,
            },
            ExecutionPanelClarificationOption {
                value: "deny".to_string(),
                label: deny_label.clone().unwrap_or_else(|| "Deny".to_string()),
                description: None,
            },
        ],
        UserInputType::ExternalAction { done_label, .. } => {
            vec![ExecutionPanelClarificationOption {
                value: "completed".to_string(),
                label: done_label
                    .clone()
                    .unwrap_or_else(|| "I've completed this".to_string()),
                description: None,
            }]
        },
        UserInputType::ToolAuthorization { .. }
        | UserInputType::SandboxOverride { .. }
        | UserInputType::DiffApproval { .. } => parse_pause_options_json(pause.options.as_deref()),
        _ => Vec::new(),
    }
}

fn build_pause_input_schema(
    pause: &PendingPauseInfo,
    options: &[ExecutionPanelClarificationOption],
) -> serde_json::Value {
    let mut input_schema = pause
        .input_type
        .schema_json_value()
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    if !options.is_empty() {
        input_schema
            .as_object_mut()
            .expect("schema is an object")
            .insert(
                "options".to_string(),
                serde_json::Value::Array(
                    options
                        .iter()
                        .map(|option| {
                            serde_json::json!({
                                "id": option.value,
                                "label": option.label,
                                "description": option.description,
                            })
                        })
                        .collect(),
                ),
            );
    }
    input_schema
}

fn parse_pause_options_json(options_json: Option<&str>) -> Vec<ExecutionPanelClarificationOption> {
    #[derive(serde::Deserialize)]
    struct PauseOptionRecord {
        #[serde(alias = "id")]
        value: String,
        label: String,
        #[serde(default)]
        description: Option<String>,
    }

    options_json
        .and_then(|raw| {
            serde_json::from_str::<Vec<PauseOptionRecord>>(raw)
                .map_err(|err| {
                    tracing::warn!(
                        "[EXECUTION-PANEL] Failed to parse pause options JSON: {err}; raw={raw}"
                    );
                    err
                })
                .ok()
        })
        .unwrap_or_default()
        .into_iter()
        .map(|option| ExecutionPanelClarificationOption {
            value: option.value,
            label: option.label,
            description: option.description,
        })
        .collect()
}

fn default_pause_question_text(input_type: &UserInputType) -> String {
    match input_type {
        UserInputType::Confirmation { .. } => "Confirmation required to continue.".to_string(),
        UserInputType::ExternalAction { .. } => {
            "Complete the requested action, then confirm to continue.".to_string()
        },
        UserInputType::ToolAuthorization { tool_name, .. } => {
            format!("Approve use of `{tool_name}` to continue execution.")
        },
        UserInputType::SandboxOverride { command, .. } => {
            format!("Approve sandbox override for `{command}` to continue execution.")
        },
        _ => "Input required to continue execution.".to_string(),
    }
}

fn session_question_status(status: SessionQuestionStatus) -> &'static str {
    match status {
        SessionQuestionStatus::Queued => "queued",
        SessionQuestionStatus::WaitingOnUser => "waiting_on_user",
        SessionQuestionStatus::Answered => "answered",
        SessionQuestionStatus::Cancelled => "cancelled",
        SessionQuestionStatus::HandedOff => "handed_off",
    }
}

fn compact_target(target: &str) -> String {
    let trimmed = target.trim();
    if trimmed.len() <= 120 {
        trimmed.to_string()
    } else {
        format!("{}...", &trimmed[..117])
    }
}

fn title_case(value: &str) -> String {
    value
        .replace('_', " ")
        .split_whitespace()
        .map(|segment| {
            let mut chars = segment.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn build_responsibility_summary(node: &ExecutionTreeNode) -> String {
    if node.waiting_for_children {
        return format!(
            "Waiting on {} delegated execution(s).",
            node.active_child_execution_ids.len()
        );
    }
    if node.delegation_summary.result_ready_count > 0 {
        return format!(
            "{} child result(s) ready.",
            node.delegation_summary.result_ready_count
        );
    }
    format!("Execution is {}.", node.status.replace('_', " "))
}

fn map_task_status(status: &str) -> TaskStatus {
    match status {
        "planning" => TaskStatus::Planning,
        "ready" => TaskStatus::Ready,
        "running" | "waiting_for_children" | "waiting_for_user" | "waiting_for_confirmation" => {
            TaskStatus::Running
        },
        "paused" => TaskStatus::Paused,
        "completed" => TaskStatus::Completed,
        "failed" => TaskStatus::Failed,
        "cancelled" => TaskStatus::Cancelled,
        "deferred" | "sleeping" => TaskStatus::Deferred,
        _ => TaskStatus::Pending,
    }
}

fn map_waiting_state(status: &str, waiting_for_children: bool) -> WaitingState {
    if waiting_for_children || status == "waiting_for_children" {
        return WaitingState::WaitingChildren;
    }
    match status {
        "planning" => WaitingState::Planning,
        "ready" => WaitingState::PlanningComplete,
        "running" => WaitingState::Executing,
        "waiting_for_user" | "waiting_for_confirmation" => WaitingState::WaitingUser,
        "completed" => WaitingState::Completed,
        "failed" => WaitingState::Failed,
        "cancelled" => WaitingState::Cancelled,
        "paused" => WaitingState::Paused,
        _ => WaitingState::Runnable,
    }
}

fn parse_rfc3339_millis(value: &str) -> i64 {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc).timestamp_millis())
        .unwrap_or_else(|_| Utc::now().timestamp_millis())
}

fn output_file_name(relative_path: &str) -> String {
    PathBuf::from(relative_path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| relative_path.to_string())
}

fn base_media_type(media_type: &str) -> &str {
    media_type
        .split(';')
        .next()
        .map(str::trim)
        .unwrap_or(media_type)
}

fn summarize_json_output(raw: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    if let Some(summary) = value
        .get("summary")
        .and_then(|value| value.as_str())
        .or_else(|| {
            value
                .get("outcome_summary")
                .and_then(|value| value.as_str())
        })
        .or_else(|| value.get("title").and_then(|value| value.as_str()))
    {
        return Some(summary.to_string());
    }
    serde_json::to_string_pretty(&value)
        .ok()
        .map(|text| format!("```json\n{text}\n```"))
}

/// Map one canonical event to a humanized `FeedItem` row. Shared by the
/// capped Run-tab summary (`build_recent_activity_feed_items`) and the
/// full deep-work `build_activity_log`; `id_prefix` distinguishes the two
/// (`activity` vs `log`) so their ids never collide.
fn event_to_feed_item(
    principal: &str,
    workspace: &str,
    task_id: &str,
    ui_thread_id: &str,
    agent_id: &str,
    event: &CanonicalEvent,
    id_prefix: &str,
) -> FeedItem {
    FeedItem {
        id: format!("v3:{}:{}:{}", id_prefix, event.execution_id, event.seq),
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        item_type: FeedItemType::Task,
        task_id: Some(task_id.to_string()),
        ui_thread_id: Some(ui_thread_id.to_string()),
        agent_id: Some(agent_id.to_string()),
        title: humanize_event_type(&event.event_type),
        summary: activity_summary(event),
        status: activity_status(event),
        created_at: parse_rfc3339_millis(&event.timestamp),
        updated_at: parse_rfc3339_millis(&event.timestamp),
        actions: Vec::new(),
        metadata: activity_metadata(event),
    }
}

/// Build the FeedItem `metadata` for an activity-log entry. Always carries
/// `storage_backend` / `execution_id` / `event_type`; additionally surfaces
/// the per-event telemetry the deep-work feed renders inline — `latency_ms`
/// for any timed call, token counts + `model` / `provider` / `cost` for
/// `llm.*` events, and the tool name (`action_type`) for `tool.*` events.
/// LLM price is exposed as unit-bearing `cost_usd`; the legacy `cost` alias is
/// retained while older clients and persisted fixtures still read it.
/// The data already lives on `event.payload`; it was previously dropped here.
fn activity_metadata(event: &CanonicalEvent) -> serde_json::Value {
    let mut meta = serde_json::Map::new();
    meta.insert("storage_backend".to_string(), serde_json::json!("v3"));
    meta.insert(
        "execution_id".to_string(),
        serde_json::json!(event.execution_id),
    );
    meta.insert(
        "event_type".to_string(),
        serde_json::json!(event.event_type),
    );

    let event_type = event.event_type.as_str();
    let keys: &[&str] = if event_type.starts_with("llm.") {
        &[
            "latency_ms",
            "input_tokens",
            "output_tokens",
            "reasoning_tokens",
            "cache_read_tokens",
            "cache_creation_tokens",
            "model",
            "provider",
            // `capability` lets the UI title LLM rows "Thinking with <cap>",
            // matching the chat activity card's vocabulary.
            "capability",
        ]
    } else if event_type.starts_with("tool.") {
        &["latency_ms", "action_type", "tool_name", "target"]
    } else if event_type == "recipe.replay" {
        &[
            "kind",
            "recipe_id",
            "version",
            "duration_ms",
            "step_id",
            "class",
            "replayed_steps",
            "origin",
            "to",
            "decision",
            "status",
            "transport",
        ]
    } else {
        &["latency_ms"]
    };
    if let Some(payload) = event.payload.as_object() {
        for &key in keys {
            match payload.get(key) {
                Some(value) if !value.is_null() => {
                    meta.insert(key.to_string(), value.clone());
                },
                _ => {},
            }
        }
        if event_type.starts_with("llm.") {
            if let Some(cost) = payload
                .get("cost_usd")
                .or_else(|| payload.get("cost"))
                .filter(|value| !value.is_null())
            {
                meta.insert("cost_usd".to_string(), cost.clone());
                meta.insert("cost".to_string(), cost.clone());
            }
        }
    }
    serde_json::Value::Object(meta)
}

/// Full seq-ordered humanized event log (oldest→newest) for the selected
/// execution, powering the tab-less deep-work feed. Unlike
/// `build_recent_activity_feed_items` this is NOT reversed and NOT capped
/// — it returns every event for the execution.
fn build_activity_log(
    principal: &str,
    workspace: &str,
    task_id: &str,
    ui_thread_id: &str,
    agent_id: &str,
    selected_execution_id: Option<&str>,
    related_execution_ids: &[String],
    agent_by_execution: &HashMap<String, String>,
    recent_events: &[CanonicalEvent],
) -> Vec<FeedItem> {
    let related: HashSet<&str> = related_execution_ids
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    recent_events
        .iter()
        .filter(|event| match selected_execution_id {
            // A delegated child's work IS this run's work. Filtering to the
            // selected id alone is what made the panel stop at the parent's
            // last event: the child runs under its own execution id, so every
            // one of its events was dropped even though they were loaded.
            Some(execution_id) => {
                event.execution_id == execution_id || related.contains(event.execution_id.as_str())
            },
            None => true,
        })
        .map(|event| {
            event_to_feed_item(
                principal,
                workspace,
                task_id,
                ui_thread_id,
                // Attribute each entry to the agent that actually produced it.
                // One scalar for the whole log stamped every child event with
                // the parent's agent, which no grouping could undo.
                agent_by_execution
                    .get(event.execution_id.as_str())
                    .map(String::as_str)
                    .unwrap_or(agent_id),
                event,
                "log",
            )
        })
        .collect()
}

/// Describe each delegated child contributing to `activity_log` so the panel
/// can collapse it into its own group with a rollup header.
///
/// Ordered by start time, and children with no entries are still listed: a
/// delegation that produced nothing is exactly the case a reader needs to see.
fn build_delegation_groups(
    tree: &ExecutionTreeRecord,
    related_execution_ids: &[String],
    activity_log: &[FeedItem],
) -> Vec<ExecutionPanelDelegationGroup> {
    let mut entry_counts: HashMap<&str, u32> = HashMap::new();
    for item in activity_log {
        if let Some(execution_id) = item
            .metadata
            .get("execution_id")
            .and_then(serde_json::Value::as_str)
        {
            *entry_counts.entry(execution_id).or_insert(0) += 1;
        }
    }
    let nodes_by_id: HashMap<_, _> = tree
        .nodes
        .iter()
        .map(|node| (node.execution_id.as_str(), node))
        .collect();
    let mut groups: Vec<ExecutionPanelDelegationGroup> = related_execution_ids
        .iter()
        .filter_map(|execution_id| nodes_by_id.get(execution_id.as_str()).copied())
        .map(|node| ExecutionPanelDelegationGroup {
            execution_id: node.execution_id.clone(),
            agent_id: node.agent_id.clone(),
            status: node.status.clone(),
            entry_count: entry_counts
                .get(node.execution_id.as_str())
                .copied()
                .unwrap_or(0),
            parent_execution_id: node.parent_execution_id.clone(),
            started_at: node.started_at.clone(),
            completed_at: node.completed_at.clone(),
        })
        .collect();
    groups.sort_by(|left, right| {
        left.started_at
            .cmp(&right.started_at)
            .then_with(|| left.execution_id.cmp(&right.execution_id))
    });
    groups
}

fn activity_status(event: &CanonicalEvent) -> FeedItemStatus {
    match event.event_type.as_str() {
        "input.requested" | "waiting_for_confirmation" | "max_iterations_reached" => {
            FeedItemStatus::NeedsAction
        },
        "tool.failed" | "llm.failed" => FeedItemStatus::Failed,
        "agentic.execution_completed" => FeedItemStatus::Done,
        _ => FeedItemStatus::Info,
    }
}

fn activity_summary(event: &CanonicalEvent) -> Option<String> {
    match event.event_type.as_str() {
        "input.requested" => event
            .payload
            .get("question")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        "waiting_for_confirmation" => event
            .payload
            .get("action_summary")
            .and_then(|value| value.as_str())
            .map(|value| format!("Waiting for confirmation: {value}")),
        "delegation.requested" => event
            .payload
            .get("sub_goal")
            .and_then(|value| value.as_str())
            .map(|value| format!("Delegated: {value}")),
        "delegation.results_ready" => event
            .payload
            .get("child_execution_id")
            .and_then(|value| value.as_str())
            .map(|value| format!("Child result ready from {value}")),
        "execution.status_changed" => event
            .payload
            .get("new_status")
            .and_then(|value| value.as_str())
            .map(|value| format!("Execution status changed to {}.", value.replace('_', " "))),
        "agentic.execution_completed" => event
            .payload
            .get("summary")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        // Surface what the LLM produced at each decision so the deep-work
        // feed shows substance, not a bare "llm succeeded" / "agentic
        // decision made" label. `decision_summary` is the model's stated
        // decision ("Execute: pack:tool_search(query=…)").
        "llm.succeeded" | "llm.failed" => event
            .payload
            .get("decision_summary")
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string),
        // Prefer the model's surfaced reasoning / thinking when it is REAL
        // (some providers don't return chain-of-thought, in which case the
        // flat loop stamps a synthetic "Native pack tool call: …"
        // placeholder we skip); otherwise fall back to the concrete action
        // the LLM chose (the resolved tool call + arguments).
        "agentic.decision_made" => {
            let payload = &event.payload;
            let real_reasoning = payload
                .get("thinking")
                .or_else(|| payload.get("reasoning"))
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty() && !value.starts_with("Native "));
            real_reasoning.map(str::to_string).or_else(|| {
                payload
                    .get("action_summary")
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.trim().is_empty())
                    .map(str::to_string)
            })
        },
        _ => event
            .payload
            .get("summary")
            .and_then(|value| value.as_str())
            .map(str::to_string),
    }
}

fn humanize_event_type(event_type: &str) -> String {
    match event_type {
        "input.requested" => "Input requested".to_string(),
        "waiting_for_confirmation" => "Waiting for confirmation".to_string(),
        "delegation.requested" => "Delegation requested".to_string(),
        "delegation.results_ready" => "Delegation results ready".to_string(),
        "execution.status_changed" => "Execution status changed".to_string(),
        "agentic.execution_completed" => "Execution completed".to_string(),
        "agentic.decision_made" => "Decision".to_string(),
        "llm.requested" => "LLM call".to_string(),
        "llm.succeeded" => "LLM response".to_string(),
        "llm.failed" => "LLM error".to_string(),
        other => other.replace(['.', '_'], " "),
    }
}

fn truncate_text(value: String, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value;
    }
    let truncated: String = value.chars().take(max_chars).collect();
    format!("{truncated}\n\n...[truncated]")
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::{
        activity_metadata, build_activity_log, build_clarification_question,
        build_delegation_groups, build_pause_clarification_question, collect_related_execution_ids,
        parse_pause_options_json, project_execution_artifact, safe_task_relative_path,
    };
    use std::collections::HashMap;

    use magician::magician_v2::artifact_v2::{
        models::{
            CanonicalEvent, DelegationReadinessRecord, ExecutionTreeNode, ExecutionTreeRecord,
            PersistedExecutionArtifactRecord,
        },
        ScopeRef,
    };
    use magician::magician_v2::ask_loop::{SessionQuestion, SessionQuestionStatus};
    use magician::magician_v2::execution::agentic::{PendingPauseInfo, UserInputType};
    use magician::magician_v2::execution_panel::types::ExecutionPanelOutputState;

    fn tree_node(
        execution_id: &str,
        parent_execution_id: Option<&str>,
        agent_id: &str,
        status: &str,
    ) -> ExecutionTreeNode {
        ExecutionTreeNode {
            execution_id: execution_id.to_string(),
            parent_execution_id: parent_execution_id.map(str::to_string),
            root_execution_id: Some(parent_execution_id.unwrap_or(execution_id).to_string()),
            agent_id: agent_id.to_string(),
            relationship_type: if parent_execution_id.is_some() {
                "delegate".to_string()
            } else {
                "root".to_string()
            },
            status: status.to_string(),
            plan_id: None,
            primary_execution_output_id: None,
            active_child_execution_ids: Vec::new(),
            child_execution_ids: Vec::new(),
            ready_child_output_ids: Vec::new(),
            waiting_for_children: false,
            delegation_summary: DelegationReadinessRecord {
                execution_id: execution_id.to_string(),
                task_id: "task_x".to_string(),
                execution_status: status.to_string(),
                waiting_for_children: false,
                active_child_execution_ids: Vec::new(),
                ready_child_output_ids: Vec::new(),
                result_ready_count: 0,
                waiting_count: 0,
                blocked_count: 0,
                terminal_without_output_count: 0,
                delegations: Vec::new(),
            },
            started_at: "2026-05-30T00:00:00Z".to_string(),
            completed_at: None,
            updated_at: "2026-05-30T00:00:00Z".to_string(),
        }
    }

    fn sample_event(seq: u64, event_type: &str) -> CanonicalEvent {
        CanonicalEvent {
            event_id: format!("e{seq}"),
            seq,
            timestamp: format!("2026-05-30T00:00:0{seq}Z"),
            principal: "alpha".to_string(),
            workspace: "default".to_string(),
            task_id: "task_x".to_string(),
            execution_id: "exec_1".to_string(),
            plan_id: None,
            step_id: None,
            event_type: event_type.to_string(),
            ref_ids: Default::default(),
            payload: serde_json::json!({}),
        }
    }

    #[test]
    fn execution_artifact_projection_exposes_only_bounded_file_metadata() {
        let projected = project_execution_artifact(
            PersistedExecutionArtifactRecord {
                artifact_id: "artifact-1".to_string(),
                artifact_type: "screen_capture".to_string(),
                content_type: "application/json".to_string(),
                payload: serde_json::json!({
                    "execution_relative_path": "outputs/capture.png",
                    "display_name": "Annotated capture.png",
                    "content_type": "image/png",
                    "size_bytes": 4096,
                    "large_private_payload": "this must not reach the panel"
                }),
                produced_at: "2026-07-31T00:00:00Z".to_string(),
                source_execution_id: Some("exec-1".to_string()),
                source_artifact_id: None,
            },
            "exec-1",
        );

        assert_eq!(projected.artifact_id, "artifact-1");
        assert_eq!(
            projected.display_name.as_deref(),
            Some("Annotated capture.png")
        );
        assert_eq!(
            projected.relative_path.as_deref(),
            Some("executions/exec-1/outputs/capture.png")
        );
        assert_eq!(projected.size_bytes, Some(4096));
        assert_eq!(projected.content_type, "image/png");
    }

    #[test]
    fn execution_artifact_projection_keeps_unsafe_paths_metadata_only() {
        let projected = project_execution_artifact(
            PersistedExecutionArtifactRecord {
                artifact_id: "artifact-unsafe".to_string(),
                artifact_type: "tool_result".to_string(),
                content_type: "application/json".to_string(),
                payload: serde_json::json!({
                    "task_relative_path": "../../outside.json",
                    "display_name": "Structured tool result"
                }),
                produced_at: "2026-07-31T00:00:00Z".to_string(),
                source_execution_id: Some("exec-1".to_string()),
                source_artifact_id: None,
            },
            "exec-1",
        );

        assert_eq!(
            projected.display_name.as_deref(),
            Some("Structured tool result")
        );
        assert_eq!(projected.relative_path, None);
        assert_eq!(safe_task_relative_path("/absolute/file"), None);
        assert_eq!(safe_task_relative_path("nested\\file"), None);
    }

    #[test]
    fn execution_output_projection_serializes_known_empty_run_lists() {
        let unknown_artifacts = serde_json::to_value(ExecutionPanelOutputState::default())
            .expect("output state must serialize");
        assert_eq!(
            unknown_artifacts["selected_execution_outputs"],
            serde_json::json!([])
        );
        assert_eq!(
            unknown_artifacts["selected_child_outputs"],
            serde_json::json!([])
        );
        assert!(unknown_artifacts
            .get("selected_execution_artifacts")
            .is_none());

        let known_empty_artifacts = serde_json::to_value(ExecutionPanelOutputState {
            selected_execution_artifacts: Some(Vec::new()),
            ..ExecutionPanelOutputState::default()
        })
        .expect("output state must serialize");
        assert_eq!(
            known_empty_artifacts["selected_execution_artifacts"],
            serde_json::json!([])
        );
    }

    #[test]
    fn build_activity_log_returns_full_chronological_log() {
        // 8 events (seq 1..=8), mixed types incl. reasoning + tool.
        let events: Vec<CanonicalEvent> = (1..=8u64)
            .map(|i| {
                let event_type = match i {
                    3 => "reasoning.delta",
                    5 => "tool.call.started",
                    _ => "lifecycle.iteration.started",
                };
                sample_event(i, event_type)
            })
            .collect();

        let log = build_activity_log(
            "alpha",
            "default",
            "task_x",
            "general",
            "agent_a",
            Some("exec_1"),
            &[],
            &HashMap::new(),
            &events,
        );

        // Every event is included (no 24-cap, no take(6)).
        assert_eq!(log.len(), 8, "activity_log must include every event");
        // Oldest→newest order (NOT reversed like recent_activity).
        assert_eq!(log.first().map(|f| f.id.as_str()), Some("v3:log:exec_1:1"));
        assert_eq!(log.last().map(|f| f.id.as_str()), Some("v3:log:exec_1:8"));
        assert!(
            log.windows(2).all(|w| w[0].created_at <= w[1].created_at),
            "activity_log must be ascending by timestamp"
        );
    }

    /// A foreign run stays out; a delegated child of this run comes in.
    ///
    /// The narrower rule — selected execution only — is why the deep panel
    /// stopped at the parent's last event: a delegated child runs under its own
    /// execution id, so every event it produced was dropped from the log even
    /// though the adapter had already loaded them.
    #[test]
    fn build_activity_log_keeps_related_children_and_drops_foreign_executions() {
        let mut events = vec![sample_event(1, "lifecycle.iteration.started")];
        let mut child = sample_event(2, "lifecycle.iteration.started");
        child.execution_id = "exec_child".to_string();
        events.push(child);
        let mut foreign = sample_event(3, "lifecycle.iteration.started");
        foreign.execution_id = "exec_unrelated".to_string();
        events.push(foreign);

        let agent_by_execution = HashMap::from([
            ("exec_1".to_string(), "personal-assistant".to_string()),
            ("exec_child".to_string(), "web-researcher".to_string()),
        ]);
        let log = build_activity_log(
            "alpha",
            "default",
            "task_x",
            "general",
            "agent_a",
            Some("exec_1"),
            &["exec_child".to_string()],
            &agent_by_execution,
            &events,
        );

        assert_eq!(
            log.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(),
            vec!["v3:log:exec_1:1", "v3:log:exec_child:2"],
            "the delegated child belongs in its parent's log; an unrelated run does not"
        );
        // Attribution has to be per event, or a grouped panel labels every
        // child step with the parent's agent.
        assert_eq!(log[0].agent_id.as_deref(), Some("personal-assistant"));
        assert_eq!(log[1].agent_id.as_deref(), Some("web-researcher"));
        assert_eq!(log[1].metadata["execution_id"], "exec_child");
    }

    /// A FINISHED delegation must still be in the panel.
    ///
    /// The collector used to walk `active_child_execution_ids`, which the
    /// parent clears on settlement — so a child's entire history disappeared
    /// from the drawer the moment the run ended, which is precisely when it is
    /// read. Terminal is the normal case here, not the edge case.
    #[test]
    fn related_ids_include_a_child_the_parent_has_already_settled() {
        let mut parent = tree_node("exec_1", None, "personal-assistant", "failed");
        // Settled: the active group is empty, the history is not.
        parent.active_child_execution_ids = Vec::new();
        parent.child_execution_ids = vec!["exec_child".to_string()];
        let child = tree_node("exec_child", Some("exec_1"), "web-researcher", "failed");
        let tree = ExecutionTreeRecord {
            task_id: "task_x".to_string(),
            task_status: "failed".to_string(),
            active_root_execution_id: None,
            latest_root_execution_id: Some("exec_1".to_string()),
            last_completed_root_execution_id: None,
            root_execution_id: Some("exec_1".to_string()),
            nodes: vec![parent, child],
        };

        assert_eq!(
            collect_related_execution_ids(&tree, "exec_1"),
            vec!["exec_child".to_string()],
            "a settled child is still this run's work"
        );
    }

    /// The rollup a collapsed delegation header renders.
    #[test]
    fn delegation_groups_describe_each_child_with_its_own_status_and_count() {
        let parent = tree_node("exec_1", None, "personal-assistant", "running");
        let child = tree_node("exec_child", Some("exec_1"), "web-researcher", "failed");
        let tree = ExecutionTreeRecord {
            task_id: "task_x".to_string(),
            task_status: "running".to_string(),
            active_root_execution_id: Some("exec_1".to_string()),
            latest_root_execution_id: Some("exec_1".to_string()),
            last_completed_root_execution_id: None,
            root_execution_id: Some("exec_1".to_string()),
            nodes: vec![parent, child],
        };
        let agent_by_execution = HashMap::from([
            ("exec_1".to_string(), "personal-assistant".to_string()),
            ("exec_child".to_string(), "web-researcher".to_string()),
        ]);
        let mut events = vec![sample_event(1, "lifecycle.iteration.started")];
        for seq in 2..=4u64 {
            let mut child_event = sample_event(seq, "lifecycle.iteration.started");
            child_event.execution_id = "exec_child".to_string();
            events.push(child_event);
        }
        let related = vec!["exec_child".to_string()];
        let log = build_activity_log(
            "alpha",
            "default",
            "task_x",
            "general",
            "agent_a",
            Some("exec_1"),
            &related,
            &agent_by_execution,
            &events,
        );

        let groups = build_delegation_groups(&tree, &related, &log);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].execution_id, "exec_child");
        assert_eq!(groups[0].agent_id, "web-researcher");
        assert_eq!(
            groups[0].status, "failed",
            "the header shows the child's own status, never the parent's"
        );
        assert_eq!(groups[0].entry_count, 3);
        assert_eq!(groups[0].parent_execution_id.as_deref(), Some("exec_1"));
    }

    #[test]
    fn llm_activity_metadata_names_recorded_price_in_us_dollars() {
        let mut event = sample_event(1, "llm.succeeded");
        event.payload = serde_json::json!({
            "cost": 0.00610875,
            "model": "gpt-5.6-terra",
            "input_tokens": 12_345,
            "output_tokens": 321
        });

        let metadata = activity_metadata(&event);
        assert_eq!(metadata["cost_usd"], serde_json::json!(0.00610875));
        assert_eq!(
            metadata["cost"],
            serde_json::json!(0.00610875),
            "the compatibility alias remains until every persisted client has migrated"
        );
    }

    #[test]
    fn non_llm_activity_does_not_promote_an_unrelated_cost_field() {
        let mut event = sample_event(1, "tool.succeeded");
        event.payload = serde_json::json!({"cost": 4.25, "tool_name": "search"});

        let metadata = activity_metadata(&event);
        assert!(metadata.get("cost_usd").is_none());
        assert!(metadata.get("cost").is_none());
    }

    #[test]
    fn recipe_activity_metadata_preserves_only_the_bounded_lifecycle_contract() {
        let mut event = sample_event(1, "recipe.replay");
        event.payload = serde_json::json!({
            "kind": "recipe.replay.fallback.handoff",
            "recipe_id": "recipe_1",
            "template": "first story about {query}",
            "step_id": "step_3",
            "class": "schema_drift",
            "replayed_steps": 2,
            "secret": "must-not-reach-the-panel"
        });

        let metadata = activity_metadata(&event);
        assert_eq!(
            metadata["kind"],
            serde_json::json!("recipe.replay.fallback.handoff")
        );
        assert_eq!(metadata["step_id"], serde_json::json!("step_3"));
        assert_eq!(metadata["replayed_steps"], serde_json::json!(2));
        assert!(metadata.get("template").is_none());
        assert!(metadata.get("secret").is_none());
    }

    #[test]
    fn unit_bearing_llm_price_wins_over_a_conflicting_legacy_alias() {
        let mut event = sample_event(1, "llm.succeeded");
        event.payload = serde_json::json!({"cost_usd": 0.0042, "cost": 99.0});

        let metadata = activity_metadata(&event);
        assert_eq!(metadata["cost_usd"], serde_json::json!(0.0042));
        assert_eq!(metadata["cost"], serde_json::json!(0.0042));
    }

    fn sample_pending_pause(input_type: UserInputType) -> PendingPauseInfo {
        let mut pending = PendingPauseInfo::for_test("pause-1", "exec-1", input_type);
        pending.task_id = Some("task-1".to_string());
        pending.plan_id = Some("plan-1".to_string());
        pending.step_id = Some("step-1".to_string());
        pending.question = Some("Need your input".to_string());
        pending.hint = Some("Pick one option to continue.".to_string());
        pending.iteration = 3;
        pending.principal = Some("alpha".to_string());
        pending.workspace = Some("default".to_string());
        pending
    }

    #[test]
    fn build_session_clarification_question_carries_real_question_contract() {
        let question = SessionQuestion {
            id: "question-3".to_string(),
            source_slot_id: Some("release_channel".to_string()),
            blocker_type: magician::magician_v2::ask_loop::clarifier::BlockerType::MissingEntity,
            stage: magician::magician_v2::state_tracker::StageContext::PlanningIteration,
            question_text: "Which release channel?".to_string(),
            context_snippets: vec!["The task did not specify one.".to_string()],
            options: Some(vec![
                magician::magician_v2::ask_loop::clarifier::QuestionOption {
                    value: "stable".to_string(),
                    label: "Stable".to_string(),
                    description: None,
                },
            ]),
            slot_confidence: Some(0.4),
            related_slots: vec!["deployment".to_string()],
            status: SessionQuestionStatus::WaitingOnUser,
            created_at: chrono::Utc::now(),
            answered_at: None,
        };

        let scope =
            ScopeRef::system_internal_unauthenticated(&"alpha".to_string(), &"default".to_string());
        let target = build_clarification_question(&question, &scope, "task-4", "exec-4")
            .hitl_request
            .expect("session question should carry direct-open payload");
        assert_eq!(target.id, "question-3");
        assert_eq!(target.scope.principal.as_deref(), Some("alpha"));
        assert_eq!(target.scope.workspace.as_deref(), Some("default"));
        assert_eq!(target.scope.workflow_id.as_deref(), Some("exec-4"));
        assert_eq!(target.scope.task_id.as_deref(), Some("task-4"));
        assert_eq!(target.source, "clarification");
        assert_eq!(target.scope.execution_id.as_deref(), Some("exec-4"));
        assert_eq!(target.input_schema["source_slot_id"], "release_channel");
        assert_eq!(target.input_schema["stage"], "planning_iteration");
        assert_eq!(target.input_schema["options"][0]["id"], "stable");
    }

    #[test]
    fn build_pause_clarification_question_adds_resume_submission_metadata() {
        let question = build_pause_clarification_question(&sample_pending_pause(
            UserInputType::Confirmation {
                confirm_label: Some("Approve".to_string()),
                deny_label: Some("Reject".to_string()),
                destructive: false,
            },
        ));

        assert_eq!(question.id, "pause-1");
        assert_eq!(question.status, "waiting_on_user");
        assert_eq!(question.options.len(), 2);
        assert_eq!(question.options[0].value, "confirm");
        assert_eq!(question.options[0].label, "Approve");
        let submission = question
            .submission
            .expect("pause-backed question should carry resume metadata");
        assert_eq!(submission.mode, "agentic_resume");
        assert_eq!(submission.pause_state_id.as_deref(), Some("pause-1"));
        assert_eq!(submission.input_type.as_deref(), Some("confirmation"));
        let target = question
            .hitl_request
            .expect("pause-backed question should carry direct-open payload");
        assert_eq!(target.id, "pause-1");
        assert_eq!(target.source, "agentic");
        assert_eq!(target.input_type, "confirmation");
        assert_eq!(target.input_schema["confirm_label"], "Approve");
        assert_eq!(target.input_schema["deny_label"], "Reject");
        assert_eq!(target.input_schema["options"][0]["id"], "confirm");
        assert_eq!(
            target.identifiers.pause_state_id.as_deref(),
            Some("pause-1")
        );
        assert_eq!(target.scope.execution_id.as_deref(), Some("exec-1"));
        assert_eq!(target.scope.principal.as_deref(), Some("alpha"));
        assert_eq!(target.scope.workspace.as_deref(), Some("default"));
    }

    #[test]
    fn build_pause_clarification_question_preserves_diff_schema() {
        let question = build_pause_clarification_question(&sample_pending_pause(
            UserInputType::DiffApproval {
                transaction_id: Some("txn-1".to_string()),
                proposal_id: None,
                approval_source: Some("transaction".to_string()),
                rationale: "Update the generated client".to_string(),
                files: vec![
                    magician::magician_v2::execution::agentic::DiffApprovalFile {
                        path: "src/client.rs".to_string(),
                        status: "M".to_string(),
                        additions: 4,
                        deletions: 1,
                        unified_diff: "@@ -1 +1 @@".to_string(),
                    },
                ],
            },
        ));

        let target = question
            .hitl_request
            .expect("diff pause should carry direct-open payload");
        assert_eq!(target.id, "txn-1");
        assert_eq!(target.source, "diff_approval");
        assert_eq!(target.input_type, "diff_approval");
        assert_eq!(
            target.identifiers.pause_state_id.as_deref(),
            Some("pause-1")
        );
        assert_eq!(target.identifiers.correlation_id.as_deref(), Some("txn-1"));
        assert_eq!(target.scope.principal.as_deref(), Some("alpha"));
        assert_eq!(target.scope.workspace.as_deref(), Some("default"));
        assert_eq!(target.scope.execution_id.as_deref(), Some("exec-1"));
        assert_eq!(target.input_schema["transaction_id"], "txn-1");
        assert_eq!(target.input_schema["files"][0]["path"], "src/client.rs");
        assert_eq!(target.input_schema["options"][0]["id"], "apply");
        assert_eq!(target.input_schema["options"][1]["id"], "reject");
    }

    #[test]
    fn parse_pause_options_json_accepts_pause_store_option_shape() {
        let options = parse_pause_options_json(Some(
            r#"[{"id":"allow_once","label":"Allow Once"},{"id":"deny","label":"Deny"}]"#,
        ));

        assert_eq!(options.len(), 2);
        assert_eq!(options[0].value, "allow_once");
        assert_eq!(options[1].label, "Deny");
    }
}
