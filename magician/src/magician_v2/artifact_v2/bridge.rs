use std::sync::Arc;

use tokio::sync::{broadcast, oneshot};
use tracing::debug;

use crate::magician_v2::realtime_events::RuntimeTransportBroadcaster;

use super::{
    events::{map_v2_realtime_event, ArtifactV2EventType},
    reducer::ArtifactV2Reducer,
    service::{ExecutionContext, ScopeRef, TaskFeedProjector, TaskProgressProjector},
};

pub struct V3RuntimeEventBridge {
    reducer: Arc<dyn ArtifactV2Reducer>,
    task_feed_projector: Option<Arc<dyn TaskFeedProjector>>,
    task_progress_projector: Option<Arc<dyn TaskProgressProjector>>,
}

impl V3RuntimeEventBridge {
    pub fn new(
        reducer: Arc<dyn ArtifactV2Reducer>,
        task_feed_projector: Option<Arc<dyn TaskFeedProjector>>,
        task_progress_projector: Option<Arc<dyn TaskProgressProjector>>,
    ) -> Self {
        Self {
            reducer,
            task_feed_projector,
            task_progress_projector,
        }
    }

    pub fn start(
        &self,
        broadcaster: Arc<RuntimeTransportBroadcaster>,
        ctx: ExecutionContext,
    ) -> oneshot::Sender<()> {
        let mut rx = broadcaster.subscribe();
        let reducer = Arc::clone(&self.reducer);
        let task_feed_projector = self.task_feed_projector.clone();
        let task_progress_projector = self.task_progress_projector.clone();
        let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let scope = ScopeRef::system_internal_unauthenticated(
                &ctx.principal.clone(),
                &ctx.workspace.clone(),
            );
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    event = rx.recv() => {
                        match event {
                            Ok(event) => {
                                if let Some(mirrored) = map_v2_realtime_event(&event)
                                    .filter(|mapped| mapped.execution_id == ctx.execution_id)
                                {
                                    let _ = reducer
                                        .reduce_runtime_signal(
                                            &scope,
                                            &ctx,
                                            mirrored.execution_status_hint.as_deref(),
                                            mirrored.task_status_hint,
                                            mirrored.active_child_execution_ids.as_deref(),
                                            mirrored.current_step_id.as_deref(),
                                            &chrono::Utc::now().to_rfc3339(),
                                        )
                                        .await;
                                    // Handle step completion/failure events — update execution state
                                    // A tool call that settled — with a result or a
                                    // failure — is the flat loop advancing. A direct
                                    // run has no taskplan steps to report otherwise,
                                    // and read as stalled after five minutes of work.
                                    if matches!(
                                        mirrored.event_type,
                                        ArtifactV2EventType::ToolSucceeded | ArtifactV2EventType::ToolFailed
                                    ) {
                                        let _ = reducer
                                            .reduce_action_settled(
                                                &scope,
                                                &ctx,
                                                &chrono::Utc::now().to_rfc3339(),
                                            )
                                            .await;
                                    }
                                    if matches!(
                                        mirrored.event_type,
                                        ArtifactV2EventType::StepCompleted | ArtifactV2EventType::StepFailed
                                    ) {
                                        if let Some(step_id) = mirrored.payload.get("step_id").and_then(|v| v.as_str()) {
                                            let _ = reducer
                                                .reduce_step_event(
                                                    &scope,
                                                    &ctx,
                                                    mirrored.event_type,
                                                    step_id,
                                                    &chrono::Utc::now().to_rfc3339(),
                                                )
                                                .await;
                                        }
                                    }
                                    if should_project_feed_item(mirrored.event_type) {
                                        if let Some(projector) = &task_feed_projector {
                                            let _ = projector
                                                .project_task_feed_item(&scope, &ctx.task_id)
                                                .await;
                                        }
                                    }
                                    if should_project_progress_item(mirrored.event_type) {
                                        if let Some(projector) = &task_progress_projector {
                                            let _ = projector
                                                .project_task_progress(&scope, &ctx.task_id)
                                                .await;
                                        }
                                    }
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                                debug!(
                                    skipped = skipped,
                                    execution_id = %ctx.execution_id,
                                    "[ARTIFACT-V2] Runtime event bridge lagged"
                                );
                            }
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
        });
        stop_tx
    }
}

fn should_project_feed_item(event_type: ArtifactV2EventType) -> bool {
    matches!(
        event_type,
        ArtifactV2EventType::InputRequested
            | ArtifactV2EventType::WaitingForConfirmation
            | ArtifactV2EventType::MaxIterationsReached
            | ArtifactV2EventType::AgenticExecutionCompleted
            | ArtifactV2EventType::ExecutionStatusChanged
            | ArtifactV2EventType::ExecutionResponsibilityChanged
    )
}

fn should_project_progress_item(event_type: ArtifactV2EventType) -> bool {
    matches!(
        event_type,
        ArtifactV2EventType::StepCompleted
            | ArtifactV2EventType::StepFailed
            | ArtifactV2EventType::LlmRequested
            | ArtifactV2EventType::LlmSucceeded
            | ArtifactV2EventType::LlmFailed
            | ArtifactV2EventType::AgenticIterationStarted
            | ArtifactV2EventType::AgenticDecisionMade
            | ArtifactV2EventType::ToolSucceeded
            | ArtifactV2EventType::ToolFailed
            | ArtifactV2EventType::InputRequested
            | ArtifactV2EventType::WaitingForConfirmation
            | ArtifactV2EventType::MaxIterationsReached
            | ArtifactV2EventType::AgenticExecutionCompleted
            | ArtifactV2EventType::ExecutionStatusChanged
            | ArtifactV2EventType::ExecutionResponsibilityChanged
            | ArtifactV2EventType::ArtifactCreated
    )
}
