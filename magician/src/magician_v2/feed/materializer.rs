use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::magician_v2::{
    artifact_v2::{
        models::{PublishedSurfaceRecord, TaskListItemV3},
        ArtifactV2Service, ScopeRef, V3ReadApi, PUBLISHED_SURFACE_CHANGED_EVENT_TYPE,
    },
    chat::storage::ChatStore,
    feed::{
        store::{FeedAttentionLane, FeedStore},
        types::{FeedItem, FeedItemPatch, FeedItemStatus, FeedItemType},
        v3_projection::{feed_status_from_v3_task_status, task_summary_to_feed_item},
        ROUTINE_RESULT_PUBLISHED_EVENT_TYPE,
    },
    progress_channel_seam::{
        surface_routing::feed_surface_renders_agent_event, ProgressMessage, ProgressMessageKind,
    },
    realtime_events::{AgentEventEnvelope, RuntimeTransportBroadcaster, RuntimeTransportEvent},
};

pub struct FeedMaterializer {
    feed_store: FeedStore,
    v3_service: Arc<ArtifactV2Service>,
    _chat_store: Arc<dyn ChatStore>,
    progress_rx: broadcast::Receiver<ProgressMessage>,
    event_rx: broadcast::Receiver<RuntimeTransportEvent>,
    event_broadcaster: Arc<RuntimeTransportBroadcaster>,
}

#[derive(Debug, Deserialize, Default)]
struct LegacyAttentionDismissedState {
    #[serde(default)]
    items: HashMap<String, LegacyAttentionDismissedRecord>,
}

#[derive(Debug, Deserialize)]
struct LegacyAttentionDismissedRecord {
    dismissed_at: i64,
}

impl FeedMaterializer {
    pub fn new(
        feed_store: FeedStore,
        v3_service: Arc<ArtifactV2Service>,
        chat_store: Arc<dyn ChatStore>,
        progress_rx: broadcast::Receiver<ProgressMessage>,
        event_rx: broadcast::Receiver<RuntimeTransportEvent>,
        event_broadcaster: Arc<RuntimeTransportBroadcaster>,
    ) -> Self {
        Self {
            feed_store,
            v3_service,
            _chat_store: chat_store,
            progress_rx,
            event_rx,
            event_broadcaster,
        }
    }

    pub fn start(self) {
        tokio::spawn(async move {
            self.run().await;
        });
    }

    async fn run(mut self) {
        self.reconcile_existing_attention_projections().await;
        let mut debounce = tokio::time::interval(Duration::from_millis(500));
        let mut pending_task_updates: HashMap<(String, String, String), ProgressMessage> =
            HashMap::new();

        loop {
            tokio::select! {
                _ = debounce.tick() => {
                    let pending = pending_task_updates.drain().map(|(_, message)| message).collect::<Vec<_>>();
                    for message in pending {
                        if let Err(error) = self.apply_task_message(&message).await {
                            warn!(error = %error, message_id = %message.id, "feed materializer failed to flush debounced task update");
                        }
                    }
                }
                recv = self.progress_rx.recv() => {
                    match recv {
                        Ok(message) => {
                            if should_debounce_task_message(&message) {
                                if let Some(feed_id) = task_feed_id(&message) {
                                    pending_task_updates.insert(
                                        (
                                            message.principal.clone(),
                                            message.workspace.clone(),
                                            feed_id,
                                        ),
                                        message,
                                    );
                                }
                                continue;
                            }

                            if let Some(feed_id) = task_feed_id(&message) {
                                pending_task_updates.remove(&(
                                    message.principal.clone(),
                                    message.workspace.clone(),
                                    feed_id,
                                ));
                            }

                            if let Err(error) = self.apply_message(&message).await {
                                warn!(error = %error, message_id = %message.id, "feed materializer failed to apply progress message");
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            warn!(skipped, "feed materializer lagged on progress stream");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                recv = self.event_rx.recv() => {
                    match recv {
                        Ok(event) => {
                            if let Err(error) = self.apply_realtime_event(&event).await {
                                warn!(error = %error, "feed materializer failed to apply supplemental realtime event");
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            warn!(skipped, "feed materializer lagged on supplemental realtime stream");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }
    }

    async fn reconcile_existing_attention_projections(&self) {
        let scopes = match self
            .v3_service
            .workspace()
            .list_tenant_scope_segments()
            .await
        {
            Ok(scopes) => scopes,
            Err(error) => {
                warn!(%error, "feed materializer could not enumerate scopes for attention recovery");
                return;
            },
        };
        for (principal, workspace) in scopes {
            let recovery_started_at = chrono::Utc::now().timestamp_millis();
            let scope =
                ScopeRef::system_internal_unauthenticated(&principal.clone(), &workspace.clone());
            self.import_legacy_attention_dismissals(&principal, &workspace)
                .await;
            let items = match self.v3_service.list_attention_items(&scope, None).await {
                Ok(items) => items,
                Err(error) => {
                    warn!(%error, %principal, %workspace, "feed materializer attention recovery source failed");
                    continue;
                },
            };
            let user_tasks = match self.v3_service.list_tasks(&scope).await {
                Ok(tasks) => tasks,
                Err(error) => {
                    warn!(%error, %principal, %workspace, "feed materializer user-task recovery source failed");
                    continue;
                },
            };
            let internal_tasks = match self.v3_service.list_internal_tasks(&scope).await {
                Ok(tasks) => tasks,
                Err(error) => {
                    warn!(%error, %principal, %workspace, "feed materializer internal-task recovery source failed");
                    continue;
                },
            };
            let valid_groups = user_tasks
                .iter()
                .chain(internal_tasks.iter())
                .map(|task| task.id.clone())
                .collect::<HashSet<_>>();
            let mut items_by_task = HashMap::<String, Vec<FeedItem>>::new();
            for item in items {
                if let Some(task_id) = item.task_id.as_ref() {
                    items_by_task.entry(task_id.clone()).or_default().push(item);
                }
            }
            for task_id in &valid_groups {
                let task_items = items_by_task.remove(task_id).unwrap_or_default();
                let generation = task_items
                    .iter()
                    .map(|item| item.updated_at)
                    .max()
                    .unwrap_or(recovery_started_at);
                if let Err(error) = self
                    .feed_store
                    .reconcile_attention_projection(
                        &principal,
                        &workspace,
                        FeedAttentionLane::Requests,
                        "v3_task_attention",
                        task_id,
                        generation,
                        Some(recovery_started_at),
                        task_items,
                    )
                    .await
                {
                    warn!(%error, %principal, %workspace, %task_id, "feed materializer attention group recovery write failed");
                }
            }
            if let Err(error) = self
                .feed_store
                .purge_attention_projection_groups_not_in(
                    &principal,
                    &workspace,
                    "v3_task_attention",
                    &valid_groups,
                    recovery_started_at,
                )
                .await
            {
                warn!(%error, %principal, %workspace, "feed materializer stale attention group recovery failed");
            }
        }
    }

    async fn import_legacy_attention_dismissals(&self, principal: &str, workspace: &str) {
        let path = self
            .v3_service
            .workspace()
            .ui_root(principal, workspace)
            .join("attention_dismissed_state.json");
        if !path.exists() {
            return;
        }
        let state = match self
            .v3_service
            .workspace()
            .read_json_path::<LegacyAttentionDismissedState, _>(&path)
            .await
        {
            Ok(state) => state,
            Err(error) => {
                warn!(%error, %principal, %workspace, "feed materializer legacy dismissal import failed");
                return;
            },
        };
        let records = state
            .items
            .into_iter()
            .map(|(id, record)| (id, true, record.dismissed_at))
            .collect();
        if let Err(error) = self
            .feed_store
            .merge_attention_dismissals(principal, workspace, records)
            .await
        {
            warn!(%error, %principal, %workspace, "feed materializer indexed dismissal import failed");
        }
    }

    async fn apply_message(&self, message: &ProgressMessage) -> anyhow::Result<()> {
        // Taxonomy-driven visibility gate. The feed surfaces lifecycle
        // milestones, escalations, and agent-level events — not raw
        // agentic-stream / planning / LLM internals. The decision is
        // sourced from `surface_routing::feed_surface_renders_agent_event`
        // (over `realtime_events::GAUI_EVENT_TAXONOMY`) so new events
        // route correctly without per-call-site updates here.
        if let Some(event_type) = message.event_type.as_deref() {
            if !feed_surface_renders_agent_event(event_type) {
                return Ok(());
            }
        }
        match &message.kind {
            ProgressMessageKind::StatusChanged { .. }
            | ProgressMessageKind::ActionProgress { .. }
            | ProgressMessageKind::ChildStatusChanged { .. }
            | ProgressMessageKind::HandedOver { .. } => {
                self.apply_task_message(message).await?;
            },
            ProgressMessageKind::AgentNotification {
                event_type,
                entity_key,
                ..
            } => {
                if event_type == "hitl.requested" {
                    // HITL belongs to the attention projection, not the
                    // durable Activity feed. V3 task attention snapshots now
                    // power `/feed/attention`; writing approval/escalation
                    // rows here made historical feed/debug views noisy.
                    return Ok(());
                }
                if event_type == "hitl.resolved" {
                    if let Some(id) = entity_key.as_deref() {
                        self.remove_item(&message.principal, &message.workspace, id)
                            .await?;
                    }
                    return Ok(());
                }
                // Other AgentNotification event_types don't materialize
                // a feed item today. (The 7 legacy projection arms used
                // to live here; canonical `hitl.requested` / `hitl.resolved`
                // above now handle every HITL source.)
            },
        }
        Ok(())
    }

    async fn apply_realtime_event(&self, event: &RuntimeTransportEvent) -> anyhow::Result<()> {
        match event {
            RuntimeTransportEvent::ChatMessageReceived { .. } => {
                // Chat already owns assistant/user conversation rendering.
                // Mirroring every assistant message into the Activity feed
                // turns the feed into a duplicate chat
                // transcript, so Activity feed admission is
                // intentionally limited to task state,
                // deliveries, routines, and learning surfaces.
            },
            RuntimeTransportEvent::AgentEvent { event }
                if event.event_type == ROUTINE_RESULT_PUBLISHED_EVENT_TYPE =>
            {
                self.apply_routine_result_published(event).await?;
            },
            RuntimeTransportEvent::AgentEvent { event }
                if event.event_type == PUBLISHED_SURFACE_CHANGED_EVENT_TYPE =>
            {
                self.apply_published_surface_changed(event).await?;
            },
            RuntimeTransportEvent::HitlRequested { .. } => {},
            RuntimeTransportEvent::HitlResolved {
                correlation_id,
                source,
                principal,
                workspace,
                ..
            } => {
                self.apply_hitl_resolved(
                    correlation_id,
                    source,
                    principal.as_deref(),
                    workspace.as_deref(),
                )
                .await?;
            },
            _ => {},
        }
        Ok(())
    }

    /// Remove old approval/escalation feed rows if a previous runtime
    /// version materialized them. New HITL requests are served by V3
    /// attention snapshots and are not written to the durable feed.
    async fn apply_hitl_resolved(
        &self,
        correlation_id: &str,
        source: &str,
        principal: Option<&str>,
        workspace: Option<&str>,
    ) -> anyhow::Result<()> {
        let Some(principal) = principal.filter(|s| !s.is_empty()) else {
            return Ok(());
        };
        let Some(workspace) = workspace.filter(|s| !s.is_empty()) else {
            return Ok(());
        };
        let entity_keys = match source {
            "approval" => vec![format!("approval:{correlation_id}")],
            "user_request" => vec![format!("escalation:{correlation_id}")],
            _ => return Ok(()),
        };
        for entity_key in entity_keys {
            self.remove_item(principal, workspace, &entity_key).await?;
        }
        Ok(())
    }

    async fn apply_task_message(&self, message: &ProgressMessage) -> anyhow::Result<()> {
        let Some(task_id) = message
            .root_task_id
            .as_deref()
            .or(message.task_id.as_deref())
        else {
            return Ok(());
        };

        let scope = ScopeRef::system_internal_unauthenticated(
            &message.principal.clone(),
            &message.workspace.clone(),
        );
        let task = match self.v3_service.get_task_list_item(&scope, task_id).await {
            Ok(task) => task,
            Err(error) => {
                debug!(task_id, error = %error, "feed materializer could not load task for progress update");
                return Ok(());
            },
        };
        let mut item = task_summary_to_feed_item(&scope, task.clone());
        item.summary = task_summary_from_message(message, &task);
        item.status = task_status_from_message_for_task(message, &task);
        item.updated_at = message.timestamp.max(item.updated_at);
        item.metadata = json!({
            "storage_backend": "v3",
            "task_status": task.status,
            "priority": task.priority,
            "execution_id": message.execution_id,
            "root_execution_id": message.root_execution_id,
            "active_root_execution_id": task.active_root_execution_id,
            "latest_root_execution_id": task.latest_root_execution_id,
            "last_completed_root_execution_id": task.last_completed_root_execution_id,
            "current_step_title": task.current_step_title,
            "current_substep_title": task.current_substep_title,
            "agent_id": item_agent_id_value(message, &task),
        });
        // The metadata above is a full replacement, so anything
        // `task_summary_to_feed_item` added has to be re-added here or a
        // progress message silently drops it. That is exactly what happened to
        // `awaiting_diff_approval`: a run staged a diff, the card said
        // Needs-You, and the next heartbeat rebuilt the item without the key
        // and with `Running` — a spinner over a run that is stopped waiting on
        // the user.
        if task.awaiting_diff_approval {
            if let Some(object) = item.metadata.as_object_mut() {
                object.insert(
                    "awaiting_diff_approval".to_string(),
                    serde_json::Value::Bool(true),
                );
            }
        }
        item.agent_id = task_agent_id(message, &task);

        let previous = self.feed_store.upsert_item(item.clone()).await?;
        if let Some(existing) = previous.as_ref() {
            item.created_at = existing.created_at;
        }
        self.emit_delta(previous, item).await;
        Ok(())
    }

    async fn apply_routine_result_published(
        &self,
        event: &AgentEventEnvelope,
    ) -> anyhow::Result<()> {
        let payload = &event.payload;
        let Some(routine_id) = payload
            .get("routine_id")
            .and_then(|value| value.as_str())
            .or_else(|| payload.get("task_id").and_then(|value| value.as_str()))
        else {
            debug!(
                event_type = %event.event_type,
                agent_id = %event.agent_id,
                "feed materializer skipped routine result without routine id"
            );
            return Ok(());
        };

        let task_summary =
            if let Some(task_id) = payload.get("task_id").and_then(|value| value.as_str()) {
                match self.v3_service.get_task_by_id(task_id).await.ok().flatten() {
                    Some((scope, _)) => self
                        .v3_service
                        .get_task_list_item(&scope, task_id)
                        .await
                        .ok()
                        .map(|summary| (scope, summary)),
                    None => None,
                }
            } else {
                None
            };

        let principal = payload
            .get("principal")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .or_else(|| {
                task_summary
                    .as_ref()
                    .map(|(scope, _)| scope.principal().to_string())
            });
        let workspace = payload
            .get("workspace")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .or_else(|| {
                task_summary
                    .as_ref()
                    .map(|(scope, _)| scope.workspace().to_string())
            });
        let (Some(principal), Some(workspace)) = (principal, workspace) else {
            debug!(
                event_type = %event.event_type,
                agent_id = %event.agent_id,
                "feed materializer skipped routine result without scope"
            );
            return Ok(());
        };

        let completed_at = payload
            .get("completed_at")
            .and_then(|value| value.as_i64())
            .unwrap_or(event.timestamp);
        let feed_status = payload
            .get("feed_status")
            .and_then(|value| value.as_str())
            .and_then(|value| FeedItemStatus::from_db_str(value).ok())
            .unwrap_or(FeedItemStatus::Done);
        let title = payload
            .get("title")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .or_else(|| {
                task_summary
                    .as_ref()
                    .map(|(_, summary)| summary.title.clone())
            })
            .unwrap_or_else(|| "Routine result".to_string());
        let summary = payload
            .get("summary")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let task_id = payload
            .get("task_id")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .or_else(|| task_summary.as_ref().map(|(_, summary)| summary.id.clone()));
        let ui_thread_id = payload
            .get("ui_thread_id")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .or_else(|| {
                task_summary
                    .as_ref()
                    .map(|(_, summary)| summary.ui_thread_id.clone())
            });
        let ui_thread_id = user_visible_feed_thread_id(ui_thread_id);

        let item = FeedItem {
            id: format!("routine:{routine_id}"),
            principal,
            workspace,
            item_type: FeedItemType::RoutineResult,
            task_id,
            ui_thread_id,
            agent_id: Some(event.agent_id.clone()),
            title,
            summary,
            status: feed_status,
            created_at: completed_at,
            updated_at: completed_at,
            actions: Vec::new(),
            metadata: payload.clone(),
        };
        self.upsert_item(item).await
    }

    async fn apply_published_surface_changed(
        &self,
        event: &AgentEventEnvelope,
    ) -> anyhow::Result<()> {
        let payload = &event.payload;
        let Some(surface_id) = payload.get("surface_id").and_then(|value| value.as_str()) else {
            debug!(
                event_type = %event.event_type,
                agent_id = %event.agent_id,
                "feed materializer skipped published-surface event without surface_id"
            );
            return Ok(());
        };
        let Some(principal) = payload.get("principal").and_then(|value| value.as_str()) else {
            debug!(
                event_type = %event.event_type,
                surface_id,
                "feed materializer skipped published-surface event without principal"
            );
            return Ok(());
        };
        let Some(workspace) = payload.get("workspace").and_then(|value| value.as_str()) else {
            debug!(
                event_type = %event.event_type,
                surface_id,
                "feed materializer skipped published-surface event without workspace"
            );
            return Ok(());
        };

        let feed_id = published_surface_delivery_feed_id(surface_id);
        if matches!(
            payload.get("status").and_then(|value| value.as_str()),
            Some("unpublished" | "superseded")
        ) {
            self.remove_item(principal, workspace, &feed_id).await?;
            return Ok(());
        }

        let scope = ScopeRef::system_internal_unauthenticated(
            &principal.to_string(),
            &workspace.to_string(),
        );
        let record = match self
            .v3_service
            .get_published_surface(&scope, surface_id)
            .await
        {
            Ok(record) => record,
            Err(error) => {
                warn!(
                    surface_id,
                    principal,
                    workspace,
                    %error,
                    "feed materializer could not resolve published surface for delivery card"
                );
                return Ok(());
            },
        };
        if matches!(record.status.as_str(), "unpublished" | "superseded") {
            self.remove_item(principal, workspace, &feed_id).await?;
            return Ok(());
        }

        self.upsert_item(published_surface_delivery_feed_item(
            &record,
            Some(event.agent_id.as_str()),
        )?)
        .await
    }

    async fn upsert_item(&self, mut item: FeedItem) -> anyhow::Result<()> {
        let previous = self.feed_store.upsert_item(item.clone()).await?;
        if let Some(existing) = previous.as_ref() {
            item.created_at = existing.created_at;
        }
        self.emit_delta(previous, item).await;
        Ok(())
    }

    async fn remove_item(&self, principal: &str, workspace: &str, id: &str) -> anyhow::Result<()> {
        let removed = self.feed_store.get_item(principal, workspace, id).await?;
        if self
            .feed_store
            .remove_item(principal, workspace, id)
            .await?
        {
            self.event_broadcaster
                .emit_transport_only(RuntimeTransportEvent::FeedItemRemoved {
                    principal: principal.to_string(),
                    workspace: workspace.to_string(),
                    id: id.to_string(),
                    task_id: removed.as_ref().and_then(|item| item.task_id.clone()),
                    ui_thread_id: removed.as_ref().and_then(|item| item.ui_thread_id.clone()),
                    execution_id: removed
                        .as_ref()
                        .and_then(|item| metadata_string(&item.metadata, "execution_id")),
                    timestamp: chrono::Utc::now().timestamp_millis(),
                });
        }
        Ok(())
    }

    async fn emit_delta(&self, previous: Option<FeedItem>, item: FeedItem) {
        match previous {
            None => {
                let timestamp = chrono::Utc::now().timestamp_millis();
                self.event_broadcaster.emit_transport_only(
                    RuntimeTransportEvent::FeedItemCreated { item, timestamp },
                );
            },
            Some(previous) => {
                let patch = FeedItemPatch::between(&previous, &item);
                if patch.is_empty() {
                    return;
                }
                self.event_broadcaster.emit_transport_only(
                    RuntimeTransportEvent::FeedItemUpdated {
                        principal: item.principal.clone(),
                        workspace: item.workspace.clone(),
                        id: item.id.clone(),
                        task_id: item.task_id.clone(),
                        ui_thread_id: item.ui_thread_id.clone(),
                        execution_id: metadata_string(&item.metadata, "execution_id"),
                        patch,
                        timestamp: chrono::Utc::now().timestamp_millis(),
                    },
                );
            },
        }
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

fn should_debounce_task_message(message: &ProgressMessage) -> bool {
    matches!(
        message.kind,
        ProgressMessageKind::ActionProgress { .. }
            | ProgressMessageKind::ChildStatusChanged { .. }
            | ProgressMessageKind::HandedOver { .. }
    )
}

fn task_feed_id(message: &ProgressMessage) -> Option<String> {
    message
        .root_task_id
        .as_ref()
        .or(message.task_id.as_ref())
        .map(|task_id| format!("v3:task:{task_id}"))
}

fn task_status_from_message(message: &ProgressMessage) -> FeedItemStatus {
    match &message.kind {
        ProgressMessageKind::StatusChanged { status, .. } => match status.as_str() {
            "completed" => FeedItemStatus::Done,
            "failed" | "cancelled" => FeedItemStatus::Failed,
            _ => FeedItemStatus::Running,
        },
        ProgressMessageKind::ActionProgress { .. }
        | ProgressMessageKind::ChildStatusChanged { .. }
        | ProgressMessageKind::HandedOver { .. } => FeedItemStatus::Running,
        ProgressMessageKind::AgentNotification { .. } => FeedItemStatus::Info,
    }
}

fn task_status_from_message_for_task(
    message: &ProgressMessage,
    task: &TaskListItemV3,
) -> FeedItemStatus {
    let message_status = task_status_from_message(message);

    // A run holding a staged diff is stopped, waiting on the user — whatever
    // a *progress* message says it is doing. Same precedence as
    // `task_summary_to_feed_item`, and it has to be, because this function
    // *overwrites* that one's answer on every progress message. Without it a
    // single heartbeat after a diff was staged turned Needs-You back into a
    // spinner that never resolves.
    //
    // Scoped to the spinner it exists to suppress, and to nothing else. The
    // guard used to run before the message was looked at, which quietly
    // overrode two answers it was never meant to touch:
    //
    // * an `AgentNotification`, which is `Info` — a note from the agent is not
    //   the thing the user has to act on;
    // * a terminal `StatusChanged`. The comment here used to claim that could
    //   not happen because `awaiting_diff_approval` is false for every
    //   terminal task — but the flag comes from the *task record read at
    //   message time*, not from the message. A completion processed while the
    //   record still reads non-terminal has a terminal message and a true
    //   flag, and the card said Needs-You over a finished run. Bounded in
    //   practice (the mainline emitter builds the message from the record, and
    //   the next projection corrects the card) but it was not the invariant
    //   the comment asserted.
    //
    // What *is* true, and is all this needs: a message that carries no verdict
    // of its own — `Running` — must not outrank a staged diff.
    //
    // That deliberately still covers a task whose own status maps to `Info`
    // (`pending`, `queued`) while a diff is staged. `task_summary_to_feed_item`
    // makes the same call on the same record, and two surfaces disagreeing
    // about whether the same diff is actionable is worse than either answer.
    if task.awaiting_diff_approval && message_status == FeedItemStatus::Running {
        return FeedItemStatus::NeedsAction;
    }

    if message_status != FeedItemStatus::Running {
        return message_status;
    }

    let task_status = feed_status_from_v3_task_status(&task.status);
    if task_status == FeedItemStatus::Running {
        return message_status;
    }
    if matches!(&message.kind, ProgressMessageKind::StatusChanged { .. })
        && !task_status_is_terminal(&task.status)
    {
        return message_status;
    }

    task_status
}

fn task_status_is_terminal(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "completed" | "failed" | "cancelled" | "canceled"
    )
}

fn task_summary_from_message(message: &ProgressMessage, task: &TaskListItemV3) -> Option<String> {
    match &message.kind {
        ProgressMessageKind::StatusChanged { summary, status } => summary
            .clone()
            .or_else(|| task.completion_summary.clone())
            .or_else(|| match status.as_str() {
                "running" | "started" => Some("Execution started".to_string()),
                "completed" => Some("Task completed".to_string()),
                "failed" => Some("Task failed".to_string()),
                "cancelled" => Some("Task cancelled".to_string()),
                _ => None,
            }),
        ProgressMessageKind::ActionProgress {
            action_type,
            target,
            success,
            error,
            ..
        } => {
            if *success {
                Some(format!("{action_type}: {target}"))
            } else if let Some(error) = error {
                Some(format!("{action_type} failed: {error}"))
            } else {
                Some(format!("{action_type} failed"))
            }
        },
        ProgressMessageKind::ChildStatusChanged {
            summary, status, ..
        } => summary
            .clone()
            .or_else(|| Some(format!("Delegated work {status}"))),
        ProgressMessageKind::HandedOver {
            from_agent,
            to_agent,
        } => Some(format!("Handed over from {from_agent} to {to_agent}")),
        ProgressMessageKind::AgentNotification { .. } => None,
    }
}

fn item_agent_id_value(message: &ProgressMessage, task: &TaskListItemV3) -> Option<String> {
    task_agent_id(message, task)
}

fn task_agent_id(message: &ProgressMessage, task: &TaskListItemV3) -> Option<String> {
    match &message.kind {
        ProgressMessageKind::HandedOver { to_agent, .. } => Some(to_agent.clone()),
        _ => message
            .agent_id
            .clone()
            .or_else(|| (!task.agent_id.is_empty()).then(|| task.agent_id.clone())),
    }
}

fn published_surface_delivery_feed_id(surface_id: &str) -> String {
    format!("data_delivery:{surface_id}")
}

fn published_surface_delivery_feed_item(
    record: &PublishedSurfaceRecord,
    agent_id: Option<&str>,
) -> anyhow::Result<FeedItem> {
    let created_at = parse_surface_timestamp_millis(&record.published_at);
    let updated_at = parse_surface_timestamp_millis(&record.updated_at).max(created_at);
    Ok(FeedItem {
        id: published_surface_delivery_feed_id(&record.surface_id),
        principal: record.principal.clone(),
        workspace: record.workspace.clone(),
        item_type: FeedItemType::DataDelivery,
        task_id: record.task_id.clone(),
        ui_thread_id: user_visible_feed_thread_id(record.ui_thread_id.clone().or_else(|| {
            (record.placement.placement_kind == "thread")
                .then(|| record.placement.placement_id.clone())
                .flatten()
        })),
        agent_id: agent_id
            .filter(|value| !value.trim().is_empty() && *value != "__system__")
            .map(str::to_string),
        title: record.title.clone(),
        summary: record.summary.clone(),
        status: FeedItemStatus::Done,
        created_at,
        updated_at,
        actions: Vec::new(),
        metadata: json!({
            "surface_id": record.surface_id,
            "surface_kind": record.surface_kind,
            "surface_status": record.status,
            "logical_surface_id": record.logical_surface_id,
            "route": record.route,
            "document_key": record.document_key,
            "source_output_id": record.source_output_id,
            "execution_id": record.source_execution_id,
            "media_type": record.media_type,
            "materialized_render_kind": record.materialized_render_kind,
            "materialized_document_key": record.materialized_document_key,
            "published_at": record.published_at,
            "updated_at": record.updated_at,
            "placement": record.placement,
        }),
    })
}

fn user_visible_feed_thread_id(value: Option<String>) -> Option<String> {
    value
        .map(|thread_id| thread_id.trim().to_string())
        .filter(|thread_id| !thread_id.is_empty() && !thread_id.starts_with("system:"))
}

fn parse_surface_timestamp_millis(raw: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|value| value.timestamp_millis())
        .unwrap_or_else(|_| chrono::Utc::now().timestamp_millis())
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::magician_v2::{
        artifact_v2::{
            models::{PublishedSurfacePlacement, PublishedSurfaceRecord},
            published_surface_changed_payload, FilesystemPublishedSurfaceStore, V3ReadApi,
        },
        chat::{
            models::{ChatChannel, ChatMessage, ChatMessageContent, ChatMessageDirection},
            storage::{ChatStore, FileChatStore},
        },
        feed::FeedQuery,
        progress_channel_seam::{
            ProgressMessage, ProgressMessageKind, ProgressSeverity, ProgressSource,
        },
        realtime_events::AgentEventEnvelope,
        test_support::build_test_artifact_v2_service,
    };

    async fn make_v3_service(
        tmp: &TempDir,
    ) -> Arc<crate::magician_v2::artifact_v2::ArtifactV2Service> {
        let service = build_test_artifact_v2_service(tmp.path());
        let task = service
            .create_task(crate::magician_v2::artifact_v2::CreateTaskInput {
                principal: "alpha".to_string(),
                workspace: "prod".to_string(),
                title: "Feed task".to_string(),
                description: "desc".to_string(),
                agent_id: "atlas".to_string(),
                goal_id: None,
                ui_thread_id: "travel".to_string(),
                priority: Some("p2".to_string()),
                due_date: None,
                tags: Vec::new(),
                created_by: "user".to_string(),
                depends_on: Vec::new(),
                approved: true,
                schedule: None,
                output_mode: crate::magician_v2::artifact_v2::models::TaskOutputMode::Accumulate,
                chat_session_id: None,
                lifecycle: crate::magician_v2::artifact_v2::models::TaskLifecycle::default(),
                sync_mode: crate::magician_v2::artifact_v2::models::TaskSyncMode::default(),
            })
            .await
            .unwrap();
        assert!(!task.manifest.task_id.is_empty());
        service
    }

    async fn make_chat_store(tmp: &TempDir) -> Arc<dyn ChatStore> {
        let store = Arc::new(FileChatStore::new(tmp.path()));
        store.initialize().await.unwrap();
        let chat_store: Arc<dyn ChatStore> = store;
        chat_store
    }

    fn sample_message(task_id: &str, kind: ProgressMessageKind) -> ProgressMessage {
        ProgressMessage {
            id: "msg-1".to_string(),
            seq: 1,
            log_key: format!("task:{task_id}"),
            source: ProgressSource::Execution,
            event_type: None,
            metadata: Default::default(),
            execution_id: Some("exec-1".to_string()),
            task_id: Some(task_id.to_string()),
            root_task_id: Some(task_id.to_string()),
            root_execution_id: Some("exec-1".to_string()),
            parent_execution_id: None,
            agent_id: Some("atlas".to_string()),
            ui_thread_id: Some("travel".to_string()),
            step_id: None,
            routing_keys: vec![format!("task/{task_id}")],
            principal: "alpha".to_string(),
            workspace: "prod".to_string(),
            severity: ProgressSeverity::Info,
            kind,
            timestamp: 1234,
        }
    }

    fn sample_published_surface(surface_id: &str) -> PublishedSurfaceRecord {
        PublishedSurfaceRecord {
            surface_id: surface_id.to_string(),
            principal: "alpha".to_string(),
            workspace: "prod".to_string(),
            surface_kind: "dashboard".to_string(),
            status: "active".to_string(),
            logical_surface_id: Some("weekly-briefing".to_string()),
            route: "/briefing/weekly-briefing".to_string(),
            document_key: format!("doc-{surface_id}"),
            task_id: Some("task-1".to_string()),
            ui_thread_id: None,
            source_output_id: Some("out-1".to_string()),
            source_execution_id: Some("exec-1".to_string()),
            media_type: Some("text/markdown".to_string()),
            materialized_render_kind: Some("muij_surface".to_string()),
            materialized_document_key: Some(format!("rendered-{surface_id}")),
            materialized_at: Some("2026-05-16T04:30:00Z".to_string()),
            title: "Weekly briefing".to_string(),
            summary: Some("A delivered briefing for review.".to_string()),
            placement: PublishedSurfacePlacement {
                placement_kind: "thread".to_string(),
                placement_id: Some("travel".to_string()),
                pinned: false,
            },
            manifest_artifact_uid: None,
            manifest_name: None,
            input_artifact_ids: vec!["artifact-1".to_string()],
            published_at: "2026-05-16T04:30:00Z".to_string(),
            unpublished_at: None,
            updated_at: "2026-05-16T04:31:00Z".to_string(),
        }
    }

    #[tokio::test]
    async fn apply_task_message_upserts_task_card() {
        let tmp = TempDir::new().unwrap();
        let v3_service = make_v3_service(&tmp).await;
        let task_id = v3_service
            .list_tasks(
                &crate::magician_v2::artifact_v2::ScopeRef::system_internal_unauthenticated(
                    &"alpha".to_string(),
                    &"prod".to_string(),
                ),
            )
            .await
            .unwrap()[0]
            .id
            .clone();

        let feed_store = FeedStore::open(tmp.path()).unwrap();
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let (_progress_tx, progress_rx) = broadcast::channel(16);
        let chat_store = make_chat_store(&tmp).await;
        let materializer = FeedMaterializer::new(
            feed_store.clone(),
            v3_service,
            chat_store,
            progress_rx,
            broadcaster.subscribe(),
            broadcaster.clone(),
        );
        materializer
            .apply_task_message(&sample_message(
                &task_id,
                ProgressMessageKind::StatusChanged {
                    status: "running".to_string(),
                    summary: Some("started".to_string()),
                },
            ))
            .await
            .unwrap();

        let item = feed_store
            .get_item("alpha", "prod", &format!("v3:task:{task_id}"))
            .await
            .unwrap()
            .expect("feed item");
        assert_eq!(item.item_type, FeedItemType::Task);
        assert_eq!(item.status, FeedItemStatus::Running);
        assert_eq!(item.ui_thread_id.as_deref(), Some("travel"));
    }

    /// A `TaskListItemV3` for the task `make_v3_service` created, with the two
    /// fields these cases vary set by the caller.
    ///
    /// Built through the service rather than by hand so the other forty fields
    /// are whatever the real card carries; only what is under test is forced.
    async fn diff_holding_item(
        service: &Arc<ArtifactV2Service>,
        scope: &ScopeRef,
        status: &str,
        awaiting_diff_approval: bool,
    ) -> TaskListItemV3 {
        let task_id = service.list_tasks(scope).await.unwrap()[0].id.clone();
        let mut item = service.get_task_list_item(scope, &task_id).await.unwrap();
        item.status = status.to_string();
        item.awaiting_diff_approval = awaiting_diff_approval;
        item
    }

    /// The guard exists to stop a *heartbeat* overwriting Needs-You. It ran
    /// before the message was looked at, so it also overwrote the answer of
    /// every message that had one of its own — including a notification, whose
    /// answer is `Info`. A note from the agent is not a thing to act on.
    #[tokio::test]
    async fn a_notification_for_a_diff_holding_task_keeps_its_own_status() {
        let tmp = TempDir::new().unwrap();
        let v3_service = make_v3_service(&tmp).await;
        let scope =
            ScopeRef::system_internal_unauthenticated(&"alpha".to_string(), &"prod".to_string());
        let task = diff_holding_item(&v3_service, &scope, "running", true).await;

        let message = sample_message(
            &task.id,
            ProgressMessageKind::AgentNotification {
                event_type: "note".to_string(),
                message: "found the flaky test".to_string(),
                entity_key: None,
                metadata: json!({}),
            },
        );

        assert_eq!(
            task_status_from_message_for_task(&message, &task),
            FeedItemStatus::Info,
            "a notification carries its own answer; the diff guard must not overwrite it"
        );
    }

    /// The guard's comment claimed a terminal message could never reach it,
    /// because `awaiting_diff_approval` is false for every terminal task. But
    /// the flag comes from the task *record read at message time*, not from
    /// the message — so a completion processed while the record still reads
    /// non-terminal arrives with a terminal message AND a true flag, and the
    /// card said Needs-You over a finished run.
    #[tokio::test]
    async fn a_terminal_message_is_not_masked_by_a_stale_diff_flag() {
        let tmp = TempDir::new().unwrap();
        let v3_service = make_v3_service(&tmp).await;
        let scope =
            ScopeRef::system_internal_unauthenticated(&"alpha".to_string(), &"prod".to_string());
        // The record still says running — that is the whole point.
        let task = diff_holding_item(&v3_service, &scope, "running", true).await;

        for (status, expected) in [
            ("completed", FeedItemStatus::Done),
            ("failed", FeedItemStatus::Failed),
            ("cancelled", FeedItemStatus::Failed),
        ] {
            let message = sample_message(
                &task.id,
                ProgressMessageKind::StatusChanged {
                    status: status.to_string(),
                    summary: None,
                },
            );
            assert_eq!(
                task_status_from_message_for_task(&message, &task),
                expected,
                "a terminal `{status}` message must not read as Needs-You"
            );
        }
    }

    /// And the case the guard is actually for still holds: a message with no
    /// verdict of its own does not outrank a staged diff.
    #[tokio::test]
    async fn a_heartbeat_for_a_diff_holding_task_still_needs_action() {
        let tmp = TempDir::new().unwrap();
        let v3_service = make_v3_service(&tmp).await;
        let scope =
            ScopeRef::system_internal_unauthenticated(&"alpha".to_string(), &"prod".to_string());
        let task = diff_holding_item(&v3_service, &scope, "running", true).await;

        for kind in [
            ProgressMessageKind::ActionProgress {
                iteration: 3,
                action_type: "tool_call".to_string(),
                target: "read_file".to_string(),
                success: true,
                error: None,
            },
            ProgressMessageKind::StatusChanged {
                status: "running".to_string(),
                summary: None,
            },
        ] {
            let message = sample_message(&task.id, kind);
            assert_eq!(
                task_status_from_message_for_task(&message, &task),
                FeedItemStatus::NeedsAction,
                "a message with no verdict must not turn Needs-You back into a spinner"
            );
        }
    }

    /// The materializer rebuilds the whole item on every progress message, so
    /// it is the one that has to preserve "waiting on you" — not the
    /// projection function the existing tests pin. Before this, a run staged a
    /// diff, the card said Needs-You, and the very next heartbeat reverted it
    /// to `Running` with the metadata key gone.
    #[tokio::test]
    async fn a_progress_message_after_a_staged_diff_keeps_the_card_needing_action() {
        let tmp = TempDir::new().unwrap();
        let v3_service = make_v3_service(&tmp).await;
        let scope =
            ScopeRef::system_internal_unauthenticated(&"alpha".to_string(), &"prod".to_string());
        let task_id = v3_service.list_tasks(&scope).await.unwrap()[0].id.clone();

        // Staged through the store both the card and the announcement read —
        // the durable state a restart sees, not a fixture struct.
        crate::magician_v2::execution::file_edit::proposal::CodeChangeProposalStore::new(
            &v3_service
                .workspace()
                .scope_root(&scope.principal(), &scope.workspace()),
        )
        .stage_patch_with_apply_root(
            crate::magician_v2::execution::file_edit::transaction::TransactionScope {
                principal: scope.principal().to_string(),
                workspace: scope.workspace().to_string(),
            },
            "the change",
            "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n-fn main() {}\n+fn main() { }\n",
            "session-1",
            Vec::new(),
            None,
            Some(task_id.clone()),
            Some("exec-1".to_string()),
        )
        .expect("stage the proposal");

        let feed_store = FeedStore::open(tmp.path()).unwrap();
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let (_progress_tx, progress_rx) = broadcast::channel(16);
        let chat_store = make_chat_store(&tmp).await;
        let materializer = FeedMaterializer::new(
            feed_store.clone(),
            v3_service,
            chat_store,
            progress_rx,
            broadcaster.subscribe(),
            broadcaster.clone(),
        );

        // An ordinary heartbeat, the kind that arrives constantly while a run
        // is parked on an approval.
        materializer
            .apply_task_message(&sample_message(
                &task_id,
                ProgressMessageKind::ActionProgress {
                    iteration: 3,
                    action_type: "tool_call".to_string(),
                    target: "read_file".to_string(),
                    success: true,
                    error: None,
                },
            ))
            .await
            .unwrap();

        let item = feed_store
            .get_item("alpha", "prod", &format!("v3:task:{task_id}"))
            .await
            .unwrap()
            .expect("feed item");
        assert_eq!(
            item.status,
            FeedItemStatus::NeedsAction,
            "a run waiting on a diff must not read as running"
        );
        assert_eq!(
            item.metadata.get("awaiting_diff_approval"),
            Some(&serde_json::Value::Bool(true)),
            "the key the projection sets must survive the rebuild"
        );
    }

    #[tokio::test]
    async fn late_running_progress_does_not_resurrect_inactive_task_card() {
        let tmp = TempDir::new().unwrap();
        let v3_service = make_v3_service(&tmp).await;
        let scope = crate::magician_v2::artifact_v2::ScopeRef::system_internal_unauthenticated(
            &"alpha".to_string(),
            &"prod".to_string(),
        );
        let task_id = v3_service.list_tasks(&scope).await.unwrap()[0].id.clone();
        v3_service
            .update_task_status(&scope, &task_id, "failed")
            .await
            .unwrap();

        let feed_store = FeedStore::open(tmp.path()).unwrap();
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let (_progress_tx, progress_rx) = broadcast::channel(16);
        let chat_store = make_chat_store(&tmp).await;
        let materializer = FeedMaterializer::new(
            feed_store.clone(),
            v3_service,
            chat_store,
            progress_rx,
            broadcaster.subscribe(),
            broadcaster.clone(),
        );

        materializer
            .apply_task_message(&sample_message(
                &task_id,
                ProgressMessageKind::ActionProgress {
                    iteration: 1,
                    action_type: "tool".to_string(),
                    target: "stale progress".to_string(),
                    success: true,
                    error: None,
                },
            ))
            .await
            .unwrap();

        let item = feed_store
            .get_item("alpha", "prod", &format!("v3:task:{task_id}"))
            .await
            .unwrap()
            .expect("feed item");
        assert_eq!(item.status, FeedItemStatus::Failed);
        assert_eq!(
            item.metadata
                .get("task_status")
                .and_then(serde_json::Value::as_str),
            Some("failed")
        );
    }

    #[tokio::test]
    async fn approval_notification_stays_out_of_durable_feed() {
        let tmp = TempDir::new().unwrap();
        let v3_service = make_v3_service(&tmp).await;
        let feed_store = FeedStore::open(tmp.path()).unwrap();
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let (_progress_tx, progress_rx) = broadcast::channel(16);
        let chat_store = make_chat_store(&tmp).await;
        let materializer = FeedMaterializer::new(
            feed_store.clone(),
            v3_service,
            chat_store,
            progress_rx,
            broadcaster.subscribe(),
            broadcaster.clone(),
        );

        let pending = sample_message(
            "task-1",
            ProgressMessageKind::AgentNotification {
                event_type: "hitl.requested".to_string(),
                message: "Approval requested before the agent can continue.".to_string(),
                entity_key: Some("approval:abc".to_string()),
                metadata: json!({"source":"approval","approval_id":"abc"}),
            },
        );
        materializer.apply_message(&pending).await.unwrap();
        assert!(feed_store
            .get_item("alpha", "prod", "approval:abc")
            .await
            .unwrap()
            .is_none());

        let resolved = sample_message(
            "task-1",
            ProgressMessageKind::AgentNotification {
                event_type: "hitl.resolved".to_string(),
                message: "resolved".to_string(),
                entity_key: Some("approval:abc".to_string()),
                metadata: json!({"source":"approval"}),
            },
        );
        materializer.apply_message(&resolved).await.unwrap();
        assert!(feed_store
            .get_item("alpha", "prod", "approval:abc")
            .await
            .unwrap()
            .is_none());
    }

    /// H5.1 — canonical `HitlRequested` envelopes are attention-only.
    /// They should not materialize approval rows in the durable Activity
    /// feed; resolution still cleans up legacy rows from older runtimes.
    #[tokio::test]
    async fn hitl_requested_approval_stays_out_of_durable_feed() {
        let tmp = TempDir::new().unwrap();
        let v3_service = make_v3_service(&tmp).await;
        let feed_store = FeedStore::open(tmp.path()).unwrap();
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let (_progress_tx, progress_rx) = broadcast::channel(16);
        let chat_store = make_chat_store(&tmp).await;
        let materializer = FeedMaterializer::new(
            feed_store.clone(),
            v3_service,
            chat_store,
            progress_rx,
            broadcaster.subscribe(),
            broadcaster.clone(),
        );

        let canonical = RuntimeTransportEvent::HitlRequested {
            correlation_id: "abc".to_string(),
            source: "approval".to_string(),
            input_type: "confirmation".to_string(),
            prompt: "Approve plan?".to_string(),
            hint: None,
            input_schema: None,
            task_id: Some("task-1".to_string()),
            execution_id: None,
            agent_id: Some("agent-1".to_string()),
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 1_700_000_000_000,
        };
        materializer.apply_realtime_event(&canonical).await.unwrap();
        assert!(feed_store
            .get_item("alpha", "prod", "approval:abc")
            .await
            .unwrap()
            .is_none());

        // Legacy progress-channel HITL requests are also attention-only.
        let second = sample_message(
            "task-1",
            ProgressMessageKind::AgentNotification {
                event_type: "hitl.requested".to_string(),
                message: "Approve plan?".to_string(),
                entity_key: Some("approval:abc".to_string()),
                metadata: json!({"source":"approval","approval_id":"abc"}),
            },
        );
        materializer.apply_message(&second).await.unwrap();
        let after_request = feed_store
            .list_items(FeedQuery {
                principal: "alpha".to_string(),
                workspace: "prod".to_string(),
                limit: 100,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            after_request
                .iter()
                .filter(|i| i.id == "approval:abc")
                .count(),
            0,
            "HitlRequested should not create durable approval feed rows"
        );

        // Canonical HitlResolved is still accepted so old rows can be
        // removed when upgrading from runtimes that materialized HITL.
        let resolved = RuntimeTransportEvent::HitlResolved {
            correlation_id: "abc".to_string(),
            source: "approval".to_string(),
            outcome: "responded".to_string(),
            decision: Some("approve".to_string()),
            task_id: Some("task-1".to_string()),
            execution_id: None,
            agent_id: Some("agent-1".to_string()),
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 1_700_000_001_000,
        };
        materializer.apply_realtime_event(&resolved).await.unwrap();
        assert!(feed_store
            .get_item("alpha", "prod", "approval:abc")
            .await
            .unwrap()
            .is_none());
    }

    /// H5.1 — user_request HITL is also attention-only.
    #[tokio::test]
    async fn hitl_requested_user_request_stays_out_of_durable_feed() {
        let tmp = TempDir::new().unwrap();
        let v3_service = make_v3_service(&tmp).await;
        let feed_store = FeedStore::open(tmp.path()).unwrap();
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let (_progress_tx, progress_rx) = broadcast::channel(16);
        let chat_store = make_chat_store(&tmp).await;
        let materializer = FeedMaterializer::new(
            feed_store.clone(),
            v3_service,
            chat_store,
            progress_rx,
            broadcaster.subscribe(),
            broadcaster.clone(),
        );

        let canonical = RuntimeTransportEvent::HitlRequested {
            correlation_id: "req-xyz".to_string(),
            source: "user_request".to_string(),
            input_type: "choice".to_string(),
            prompt: "Which calendar?".to_string(),
            hint: None,
            input_schema: None,
            task_id: Some("task-2".to_string()),
            execution_id: Some("exec-2".to_string()),
            agent_id: Some("agent-2".to_string()),
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 1_700_000_000_000,
        };
        materializer.apply_realtime_event(&canonical).await.unwrap();
        assert!(feed_store
            .get_item("alpha", "prod", "escalation:req-xyz")
            .await
            .unwrap()
            .is_none());
    }

    /// H5.1 — `source` values outside the materialized set (e.g.
    /// `clarification`, `agentic`) must NOT spam the feed. They surface
    /// through `pendingHitlStore` / attention markers, not via feed
    /// items.
    #[tokio::test]
    async fn hitl_requested_clarification_does_not_materialize() {
        let tmp = TempDir::new().unwrap();
        let v3_service = make_v3_service(&tmp).await;
        let feed_store = FeedStore::open(tmp.path()).unwrap();
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let (_progress_tx, progress_rx) = broadcast::channel(16);
        let chat_store = make_chat_store(&tmp).await;
        let materializer = FeedMaterializer::new(
            feed_store.clone(),
            v3_service,
            chat_store,
            progress_rx,
            broadcaster.subscribe(),
            broadcaster.clone(),
        );

        let canonical = RuntimeTransportEvent::HitlRequested {
            correlation_id: "clarif-q-1".to_string(),
            source: "clarification".to_string(),
            input_type: "text".to_string(),
            prompt: "Which week?".to_string(),
            hint: None,
            input_schema: None,
            task_id: Some("task-3".to_string()),
            execution_id: None,
            agent_id: Some("agent-3".to_string()),
            principal: Some("alpha".to_string()),
            workspace: Some("prod".to_string()),
            timestamp: 1_700_000_000_000,
        };
        materializer.apply_realtime_event(&canonical).await.unwrap();
        // No feed item should exist for clarification sources.
        let any = feed_store
            .list_items(FeedQuery {
                principal: "alpha".to_string(),
                workspace: "prod".to_string(),
                limit: 100,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(
            any.iter().all(|item| !item.id.contains("clarif")),
            "clarification source must not materialize a feed row"
        );
    }

    #[tokio::test]
    async fn assistant_chat_message_does_not_materialize_activity_feed_card() {
        let tmp = TempDir::new().unwrap();
        let v3_service = make_v3_service(&tmp).await;
        let chat_store = make_chat_store(&tmp).await;
        let session = chat_store
            .get_or_create_active_session("alpha", "prod", "general", &ChatChannel::web(), "atlas")
            .await
            .unwrap();
        let feed_store = FeedStore::open(tmp.path()).unwrap();
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let (_progress_tx, progress_rx) = broadcast::channel(16);
        let materializer = FeedMaterializer::new(
            feed_store.clone(),
            v3_service,
            chat_store,
            progress_rx,
            broadcaster.subscribe(),
            broadcaster.clone(),
        );

        let message = ChatMessage {
            id: "msg-42".to_string(),
            session_id: session.id.clone(),
            direction: ChatMessageDirection::Assistant,
            content: ChatMessageContent::Text {
                text: "All set.".to_string(),
                plan_reply: None,
            },
            created_at: 2345,
            chat_turn_id: None,
            voice_origin: None,
            context_origin: None,
            speech_segments: None,
            source_surface: None,
            presence_session_id: None,
            presentation: None,
        };
        materializer
            .apply_realtime_event(&RuntimeTransportEvent::ChatMessageReceived {
                session_id: session.id.clone(),
                message,
                principal: Some(session.principal.clone()),
                workspace: Some(session.workspace.clone()),
                origin_channel: Some(session.origin_channel.clone()),
                timestamp: chrono::Utc::now().timestamp_millis(),
            })
            .await
            .unwrap();

        assert!(feed_store
            .get_item("alpha", "prod", "agent_message:msg-42")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn routine_result_published_upserts_routine_card() {
        let tmp = TempDir::new().unwrap();
        let v3_service = make_v3_service(&tmp).await;
        let task_id = v3_service
            .list_tasks(
                &crate::magician_v2::artifact_v2::ScopeRef::system_internal_unauthenticated(
                    &"alpha".to_string(),
                    &"prod".to_string(),
                ),
            )
            .await
            .unwrap()[0]
            .id
            .clone();
        let chat_store = make_chat_store(&tmp).await;
        let feed_store = FeedStore::open(tmp.path()).unwrap();
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let (_progress_tx, progress_rx) = broadcast::channel(16);
        let materializer = FeedMaterializer::new(
            feed_store.clone(),
            v3_service,
            chat_store,
            progress_rx,
            broadcaster.subscribe(),
            broadcaster.clone(),
        );

        materializer
            .apply_realtime_event(&RuntimeTransportEvent::AgentEvent {
                event: AgentEventEnvelope::new(
                    ROUTINE_RESULT_PUBLISHED_EVENT_TYPE,
                    "atlas",
                    json!({
                        "routine_id": task_id.clone(),
                        "task_id": task_id.clone(),
                        "principal": "alpha",
                        "workspace": "prod",
                        "ui_thread_id": "travel",
                        "title": "Morning briefing",
                        "summary": "Inbox triaged and briefing published.",
                        "feed_status": "done",
                        "outcome": "goal_achieved",
                        "completed_at": 1711368000000i64,
                    }),
                ),
            })
            .await
            .unwrap();

        let item = feed_store
            .get_item("alpha", "prod", &format!("routine:{task_id}"))
            .await
            .unwrap()
            .expect("routine result feed item");
        assert_eq!(item.item_type, FeedItemType::RoutineResult);
        assert_eq!(item.status, FeedItemStatus::Done);
        assert_eq!(item.ui_thread_id.as_deref(), Some("travel"));
        assert_eq!(
            item.summary.as_deref(),
            Some("Inbox triaged and briefing published.")
        );
    }

    #[tokio::test]
    async fn published_surface_changed_upserts_and_removes_delivery_card() {
        let tmp = TempDir::new().unwrap();
        let v3_service = make_v3_service(&tmp).await;
        let store = FilesystemPublishedSurfaceStore::new(v3_service.workspace().clone());
        let record = sample_published_surface("surface-1");
        store.upsert_surface(&record).await.unwrap();

        let chat_store = make_chat_store(&tmp).await;
        let feed_store = FeedStore::open(tmp.path()).unwrap();
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
        let (_progress_tx, progress_rx) = broadcast::channel(16);
        let materializer = FeedMaterializer::new(
            feed_store.clone(),
            v3_service,
            chat_store,
            progress_rx,
            broadcaster.subscribe(),
            broadcaster.clone(),
        );

        materializer
            .apply_realtime_event(&RuntimeTransportEvent::AgentEvent {
                event: AgentEventEnvelope::new(
                    PUBLISHED_SURFACE_CHANGED_EVENT_TYPE,
                    "atlas",
                    published_surface_changed_payload(&record),
                ),
            })
            .await
            .unwrap();

        let item = feed_store
            .get_item("alpha", "prod", "data_delivery:surface-1")
            .await
            .unwrap()
            .expect("published surface should materialize as data delivery");
        assert_eq!(item.item_type, FeedItemType::DataDelivery);
        assert_eq!(item.status, FeedItemStatus::Done);
        assert_eq!(item.title, "Weekly briefing");
        assert_eq!(item.ui_thread_id.as_deref(), Some("travel"));
        assert_eq!(
            item.metadata.get("route").and_then(|value| value.as_str()),
            Some("/briefing/weekly-briefing")
        );

        let mut superseded_payload = published_surface_changed_payload(&record);
        superseded_payload["status"] = json!("superseded");
        materializer
            .apply_realtime_event(&RuntimeTransportEvent::AgentEvent {
                event: AgentEventEnvelope::new(
                    PUBLISHED_SURFACE_CHANGED_EVENT_TYPE,
                    "atlas",
                    superseded_payload,
                ),
            })
            .await
            .unwrap();

        assert!(feed_store
            .get_item("alpha", "prod", "data_delivery:surface-1")
            .await
            .unwrap()
            .is_none());
    }
}
