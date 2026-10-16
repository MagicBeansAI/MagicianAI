use std::{collections::HashMap, sync::Arc, time::Duration};

use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::execution_panel::V3ExecutionPanelAdapter;
use magician::magician_v2::{
    artifact_v2::{ArtifactV2Error, ScopeRef},
    progress_channel_seam::{
        surface_routing::execution_panel_surface_refreshes_on_agent_event, ProgressMessage,
    },
    realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent},
};

#[derive(Debug, Clone)]
enum PanelRefreshTarget {
    Task {
        principal: String,
        workspace: String,
        task_id: String,
        execution_id: Option<String>,
    },
    Execution {
        execution_id: String,
    },
}

impl PanelRefreshTarget {
    fn key(&self) -> String {
        match self {
            Self::Task {
                principal,
                workspace,
                task_id,
                execution_id,
            } => format!(
                "task:{principal}:{workspace}:{task_id}:{}",
                execution_id.clone().unwrap_or_default()
            ),
            Self::Execution { execution_id } => format!("execution:{execution_id}"),
        }
    }
}

pub struct ExecutionPanelProjector {
    service: V3ExecutionPanelAdapter,
    event_broadcaster: Arc<RuntimeTransportBroadcaster>,
    event_rx: broadcast::Receiver<RuntimeTransportEvent>,
    progress_rx: broadcast::Receiver<ProgressMessage>,
}

/// A `TaskNotFound` / `ExecutionNotFound` miss is expected while a plan restart
/// or execution lifecycle transition is re-creating the record (its state file
/// is briefly absent). The best-effort projector should skip quietly and retry
/// on the next event rather than warn. Any other error (IO, decode, corruption)
/// is a genuine failure and stays at WARN. Mirrors the benign-not-found pairing
/// used in `artifact_v2::service`.
fn is_transient_missing_record(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<ArtifactV2Error>(),
        Some(ArtifactV2Error::TaskNotFound(_) | ArtifactV2Error::ExecutionNotFound(_))
    )
}

impl ExecutionPanelProjector {
    pub fn new(
        service: V3ExecutionPanelAdapter,
        event_broadcaster: Arc<RuntimeTransportBroadcaster>,
        progress_rx: broadcast::Receiver<ProgressMessage>,
    ) -> Self {
        let event_rx = event_broadcaster.subscribe();
        Self {
            service,
            event_broadcaster,
            event_rx,
            progress_rx,
        }
    }

    pub fn start(self) {
        tokio::spawn(async move {
            self.run().await;
        });
    }

    async fn run(mut self) {
        let mut debounce = tokio::time::interval(Duration::from_millis(350));
        let mut pending: HashMap<String, PanelRefreshTarget> = HashMap::new();

        loop {
            tokio::select! {
                _ = debounce.tick() => {
                    let targets = pending.drain().map(|(_, target)| target).collect::<Vec<_>>();
                    for target in targets {
                        if let Err(error) = self.emit_delta(target).await {
                            if is_transient_missing_record(&error) {
                                // Expected while a plan restart / execution lifecycle
                                // transition is re-creating the record — the task or its
                                // selected execution's state file is momentarily absent.
                                // The projector is best-effort + debounced: the next event
                                // re-queues this target and emits once the record lands, and
                                // the panel loads fresh on open. Not a failure → DEBUG.
                                debug!(
                                    error = %error,
                                    "execution panel projector skipped delta — task/execution record not available yet (transient); will retry on next event"
                                );
                            } else {
                                warn!(error = %error, "execution panel projector failed to emit delta");
                            }
                        }
                    }
                }
                recv = self.event_rx.recv() => {
                    match recv {
                        Ok(event) => {
                            for target in refresh_targets_for_event(&event) {
                                pending.insert(target.key(), target);
                            }
                            if let Some(execution_id) = execution_scope_for_event(&event) {
                                if let Ok(Some((principal, workspace, task_id, root_execution_id))) =
                                    self.service.resolve_task_refresh_scope_for_execution(execution_id).await
                                {
                                    let task_target = PanelRefreshTarget::Task {
                                        principal,
                                        workspace,
                                        task_id,
                                        execution_id: Some(root_execution_id),
                                    };
                                    pending.insert(task_target.key(), task_target);
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            warn!(skipped, "execution panel projector lagged on realtime stream");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                recv = self.progress_rx.recv() => {
                    match recv {
                        Ok(message) => {
                            for target in refresh_targets_for_progress(&message) {
                                pending.insert(target.key(), target);
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            warn!(skipped, "execution panel projector lagged on progress stream");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    }

    async fn emit_delta(&self, target: PanelRefreshTarget) -> anyhow::Result<()> {
        let next_state = match &target {
            PanelRefreshTarget::Task {
                principal,
                workspace,
                task_id,
                execution_id,
            } => {
                let scope = ScopeRef::system_internal_unauthenticated(
                    &principal.clone(),
                    &workspace.clone(),
                );
                self.service
                    .get_task_panel_state(&scope, task_id, execution_id.as_deref())
                    .await?
            },
            PanelRefreshTarget::Execution { execution_id } => {
                let Some((principal, workspace, task_id, _root_execution_id)) = self
                    .service
                    .resolve_task_refresh_scope_for_execution(execution_id)
                    .await?
                else {
                    return Ok(());
                };
                let scope = ScopeRef::system_internal_unauthenticated(&principal, &workspace);
                self.service
                    .get_task_panel_state(&scope, &task_id, Some(execution_id))
                    .await?
            },
        };
        let Some(state) = next_state else {
            return Ok(());
        };

        self.event_broadcaster
            .emit_transport_only(RuntimeTransportEvent::ExecutionPanelDelta {
                principal: state.overview.principal.clone(),
                workspace: state.overview.workspace.clone(),
                task_id: (!state.overview.task_id.is_empty())
                    .then(|| state.overview.task_id.clone()),
                execution_id: state.overview.execution_id.clone(),
                state,
                timestamp: chrono::Utc::now().timestamp_millis(),
            });

        Ok(())
    }
}

fn refresh_targets_for_progress(message: &ProgressMessage) -> Vec<PanelRefreshTarget> {
    // Taxonomy-driven gate: only refresh the execution panel when the
    // event actually changes the projected state (lifecycle / task /
    // agent / Hitl / clarification categories). Internal stream events
    // (tool.call.*, reasoning.*, plan.step.*) leave the projection
    // unchanged — skipping the refresh trims panel-update load.
    if let Some(event_type) = message.event_type.as_deref() {
        if !execution_panel_surface_refreshes_on_agent_event(event_type) {
            return Vec::new();
        }
    }

    if let Some(task_id) = message.root_task_id.as_ref().or(message.task_id.as_ref()) {
        return vec![PanelRefreshTarget::Task {
            principal: message.principal.clone(),
            workspace: message.workspace.clone(),
            task_id: task_id.clone(),
            execution_id: message.execution_id.clone(),
        }];
    }

    message
        .execution_id
        .as_ref()
        .map(|execution_id| {
            vec![PanelRefreshTarget::Execution {
                execution_id: execution_id.clone(),
            }]
        })
        .unwrap_or_default()
}

fn refresh_targets_for_event(event: &RuntimeTransportEvent) -> Vec<PanelRefreshTarget> {
    match event {
        RuntimeTransportEvent::FeedItemCreated { item, .. } => item
            .task_id
            .as_ref()
            .map(|task_id| {
                vec![PanelRefreshTarget::Task {
                    principal: item.principal.clone(),
                    workspace: item.workspace.clone(),
                    task_id: task_id.clone(),
                    execution_id: metadata_string(&item.metadata, "execution_id"),
                }]
            })
            .unwrap_or_default(),
        RuntimeTransportEvent::FeedItemUpdated {
            principal,
            workspace,
            task_id,
            execution_id,
            ..
        }
        | RuntimeTransportEvent::FeedItemRemoved {
            principal,
            workspace,
            task_id,
            execution_id,
            ..
        } => task_id
            .as_ref()
            .map(|task_id| {
                vec![PanelRefreshTarget::Task {
                    principal: principal.clone(),
                    workspace: workspace.clone(),
                    task_id: task_id.clone(),
                    execution_id: execution_id.clone(),
                }]
            })
            .unwrap_or_default(),
        RuntimeTransportEvent::ExecutionRestoreFailed { execution_id, .. }
        | RuntimeTransportEvent::ExecutionInflightResent { execution_id, .. }
        | RuntimeTransportEvent::ExecutionInflightDropped { execution_id, .. }
        | RuntimeTransportEvent::ClarificationSessionSnapshot { execution_id, .. }
        | RuntimeTransportEvent::WorkflowResumed { execution_id, .. }
        | RuntimeTransportEvent::WorkflowResumeFailed { execution_id, .. }
        | RuntimeTransportEvent::SubGoalRequested { execution_id, .. }
        | RuntimeTransportEvent::SubGoalOutcome { execution_id, .. }
        | RuntimeTransportEvent::AtomicPlanGenerated { execution_id, .. }
        | RuntimeTransportEvent::ShellOutputChunk { execution_id, .. }
        | RuntimeTransportEvent::AgenticExecutionCompleted { execution_id, .. }
        | RuntimeTransportEvent::AgenticWaitingForUser { execution_id, .. }
        | RuntimeTransportEvent::AgenticWaitingForConfirmation { execution_id, .. }
        | RuntimeTransportEvent::AgenticResumed { execution_id, .. }
        | RuntimeTransportEvent::AgenticMaxIterationsReached { execution_id, .. } => {
            vec![PanelRefreshTarget::Execution {
                execution_id: execution_id.clone(),
            }]
        },
        // Canonical HITL refresh — any pause that carries an
        // `execution_id` triggers a refresh on that execution's panel.
        // Covers the post-H7.1 replacement of the legacy
        // `UserRequestPending` / `UserRequestResolved` arms plus any
        // future source whose canonical envelope arrives here.
        RuntimeTransportEvent::HitlRequested {
            execution_id: Some(execution_id),
            ..
        }
        | RuntimeTransportEvent::HitlResolved {
            execution_id: Some(execution_id),
            ..
        } => vec![PanelRefreshTarget::Execution {
            execution_id: execution_id.clone(),
        }],
        _ => Vec::new(),
    }
}

fn execution_scope_for_event(event: &RuntimeTransportEvent) -> Option<&str> {
    match event {
        RuntimeTransportEvent::ExecutionRestoreFailed { execution_id, .. }
        | RuntimeTransportEvent::ExecutionInflightResent { execution_id, .. }
        | RuntimeTransportEvent::ExecutionInflightDropped { execution_id, .. }
        | RuntimeTransportEvent::ClarificationSessionSnapshot { execution_id, .. }
        | RuntimeTransportEvent::WorkflowResumed { execution_id, .. }
        | RuntimeTransportEvent::WorkflowResumeFailed { execution_id, .. }
        | RuntimeTransportEvent::SubGoalRequested { execution_id, .. }
        | RuntimeTransportEvent::SubGoalOutcome { execution_id, .. }
        | RuntimeTransportEvent::AtomicPlanGenerated { execution_id, .. }
        | RuntimeTransportEvent::ShellOutputChunk { execution_id, .. }
        | RuntimeTransportEvent::AgenticExecutionCompleted { execution_id, .. }
        | RuntimeTransportEvent::AgenticWaitingForUser { execution_id, .. }
        | RuntimeTransportEvent::AgenticWaitingForConfirmation { execution_id, .. }
        | RuntimeTransportEvent::AgenticResumed { execution_id, .. }
        | RuntimeTransportEvent::AgenticMaxIterationsReached { execution_id, .. } => {
            Some(execution_id)
        },
        // Canonical HITL scope — `execution_id` is the canonical key.
        RuntimeTransportEvent::HitlRequested {
            execution_id: Some(execution_id),
            ..
        }
        | RuntimeTransportEvent::HitlResolved {
            execution_id: Some(execution_id),
            ..
        } => Some(execution_id),
        _ => None,
    }
}

fn metadata_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .as_object()
        .and_then(|record| record.get(key))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use magician::magician_v2::{
        feed::{FeedItem, FeedItemStatus, FeedItemType},
        progress_channel_seam::{
            ProgressMessage, ProgressMessageKind, ProgressSeverity, ProgressSource,
        },
        realtime_events::RuntimeTransportEvent,
    };

    #[test]
    fn refresh_targets_uses_scoped_feed_task_identity() {
        let targets = refresh_targets_for_event(&RuntimeTransportEvent::FeedItemCreated {
            item: FeedItem {
                id: "task:1".to_string(),
                principal: "alice".to_string(),
                workspace: "default".to_string(),
                item_type: FeedItemType::Task,
                task_id: Some("task-1".to_string()),
                ui_thread_id: Some("general".to_string()),
                agent_id: Some("atlas".to_string()),
                title: "Task".to_string(),
                summary: Some("summary".to_string()),
                status: FeedItemStatus::Info,
                created_at: 1,
                updated_at: 1,
                actions: Vec::new(),
                metadata: serde_json::json!({
                    "execution_id": "exec-1"
                }),
            },
            timestamp: 0,
        });

        assert_eq!(targets.len(), 1);
        match &targets[0] {
            PanelRefreshTarget::Task {
                principal,
                workspace,
                task_id,
                execution_id,
            } => {
                assert_eq!(principal, "alice");
                assert_eq!(workspace, "default");
                assert_eq!(task_id, "task-1");
                assert_eq!(execution_id.as_deref(), Some("exec-1"));
            },
            other => panic!("unexpected target: {other:?}"),
        }
    }

    #[test]
    fn refresh_targets_track_progress_messages_as_canonical_input() {
        let targets = refresh_targets_for_progress(&ProgressMessage {
            id: "msg-1".to_string(),
            seq: 7,
            log_key: "task:task-1".to_string(),
            source: ProgressSource::Execution,
            event_type: None,
            metadata: Default::default(),
            execution_id: Some("exec-7".to_string()),
            task_id: Some("task-1".to_string()),
            root_task_id: Some("task-1".to_string()),
            root_execution_id: Some("exec-7".to_string()),
            parent_execution_id: None,
            agent_id: Some("atlas".to_string()),
            ui_thread_id: Some("general".to_string()),
            step_id: None,
            routing_keys: vec!["task/task-1".to_string()],
            principal: "alice".to_string(),
            workspace: "default".to_string(),
            severity: ProgressSeverity::Info,
            kind: ProgressMessageKind::StatusChanged {
                status: "running".to_string(),
                summary: Some("Executing".to_string()),
            },
            timestamp: 1,
        });

        assert_eq!(targets.len(), 1);
        match &targets[0] {
            PanelRefreshTarget::Task {
                principal,
                workspace,
                task_id,
                execution_id,
            } => {
                assert_eq!(principal, "alice");
                assert_eq!(workspace, "default");
                assert_eq!(task_id, "task-1");
                assert_eq!(execution_id.as_deref(), Some("exec-7"));
            },
            other => panic!("unexpected target: {other:?}"),
        }
    }

    #[test]
    fn refresh_targets_tracks_shell_output_events() {
        let targets = refresh_targets_for_event(&RuntimeTransportEvent::ShellOutputChunk {
            execution_id: "exec-9".to_string(),
            step_id: "step-1".to_string(),
            step_index: 0,
            command: "echo hi".to_string(),
            stream: "stdout".to_string(),
            data: "hello".to_string(),
            sequence: 0,
            is_final: false,
            exit_code: None,
            principal: None,
            workspace: None,
            timestamp: 1,
        });

        assert_eq!(targets.len(), 1);
        match &targets[0] {
            PanelRefreshTarget::Execution { execution_id } => {
                assert_eq!(execution_id, "exec-9");
            },
            other => panic!("unexpected target: {other:?}"),
        }
    }

    #[test]
    fn execution_scope_for_event_returns_execution_id_for_shell_output() {
        let event = RuntimeTransportEvent::ShellOutputChunk {
            execution_id: "exec-9".to_string(),
            step_id: "step-1".to_string(),
            step_index: 0,
            command: "echo hi".to_string(),
            stream: "stdout".to_string(),
            data: "hello".to_string(),
            sequence: 0,
            is_final: false,
            exit_code: None,
            principal: None,
            workspace: None,
            timestamp: 1,
        };

        assert_eq!(execution_scope_for_event(&event), Some("exec-9"));
    }
}
