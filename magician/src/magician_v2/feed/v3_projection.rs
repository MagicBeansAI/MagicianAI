use std::sync::Arc;

use chrono::{DateTime, Utc};

use super::{
    store::{FeedQuery, FeedStore},
    types::{FeedItem, FeedItemPatch, FeedItemStatus, FeedItemType},
};
use crate::magician_v2::{
    artifact_v2::{models::TaskListItemV3, ScopeRef},
    realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent},
};

#[derive(Clone)]
pub struct V3FeedProjectionAdapter {
    feed_store: FeedStore,
    event_broadcaster: Arc<RuntimeTransportBroadcaster>,
}

impl V3FeedProjectionAdapter {
    pub fn new(feed_store: FeedStore, event_broadcaster: Arc<RuntimeTransportBroadcaster>) -> Self {
        Self {
            feed_store,
            event_broadcaster,
        }
    }

    pub async fn upsert_task_summary(
        &self,
        scope: &ScopeRef,
        task: TaskListItemV3,
    ) -> anyhow::Result<()> {
        let mut item = task_summary_to_feed_item(scope, task);
        let previous = self.feed_store.upsert_item(item.clone()).await?;
        if let Some(existing) = previous.as_ref() {
            item.created_at = existing.created_at;
        }
        self.emit_delta(previous, item).await;
        Ok(())
    }

    pub async fn list_task_items(
        &self,
        scope: &ScopeRef,
        ui_thread_id: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<FeedItem>> {
        self.feed_store
            .list_items(FeedQuery {
                principal: scope.principal().to_string(),
                workspace: scope.workspace().to_string(),
                before: None,
                after: None,
                limit,
                task_id: None,
                ui_thread_id: ui_thread_id.map(str::to_string),
                item_type: Some(FeedItemType::Task),
                status: None,
                agent_id: None,
            })
            .await
    }

    /// Recurring Monitors Phase 3 — upsert one monitor attention item (the
    /// `monitor_access_problem:*` Needs-You escalation). Same store + delta
    /// emission as task summaries; `created_at` sticks to the first write so
    /// repeated reprojects don't churn ordering.
    pub async fn upsert_monitor_attention_item(&self, item: FeedItem) -> anyhow::Result<()> {
        let mut item = item;
        let previous = self.feed_store.upsert_item(item.clone()).await?;
        if let Some(existing) = previous.as_ref() {
            item.created_at = existing.created_at;
        }
        self.emit_delta(previous, item).await;
        Ok(())
    }

    /// Recurring Monitors Phase 3 — auto-resolve: remove one monitor
    /// attention item by id (no-op `false` when absent) and emit the
    /// removal event so live surfaces clear immediately.
    pub async fn remove_monitor_attention_item(
        &self,
        scope: &ScopeRef,
        item_id: &str,
    ) -> anyhow::Result<bool> {
        let removed = self
            .feed_store
            .remove_item(&scope.principal(), &scope.workspace(), item_id)
            .await?;
        if removed {
            self.event_broadcaster
                .emit_transport_only(RuntimeTransportEvent::FeedItemRemoved {
                    principal: scope.principal().to_string(),
                    workspace: scope.workspace().to_string(),
                    id: item_id.to_string(),
                    task_id: None,
                    ui_thread_id: None,
                    execution_id: None,
                    timestamp: chrono::Utc::now().timestamp_millis(),
                });
        }
        Ok(removed)
    }

    /// Recurring Monitors Phase 3 — funnel-supported dismissal record
    /// (`feed_attention_dismissals`). `dismissed: true` when an access
    /// problem auto-resolves (suppresses any projected attention row too);
    /// `dismissed: false` clears a stale tombstone when a NEW failure streak
    /// re-crosses the threshold, so the fresh problem can resurface.
    pub async fn record_monitor_attention_dismissal(
        &self,
        scope: &ScopeRef,
        item_id: &str,
        dismissed: bool,
    ) -> anyhow::Result<()> {
        self.feed_store
            .set_attention_dismissed(
                &scope.principal(),
                &scope.workspace(),
                item_id,
                dismissed,
                chrono::Utc::now().timestamp_millis(),
            )
            .await
    }

    pub async fn remove_task_summary(&self, scope: &ScopeRef, task_id: &str) -> anyhow::Result<()> {
        let removed = self
            .feed_store
            .remove_task_items(&scope.principal(), &scope.workspace(), task_id)
            .await?;
        for item in removed {
            self.event_broadcaster
                .emit_transport_only(RuntimeTransportEvent::FeedItemRemoved {
                    principal: item.principal.clone(),
                    workspace: item.workspace.clone(),
                    id: item.id.clone(),
                    task_id: item.task_id.clone(),
                    ui_thread_id: item.ui_thread_id.clone(),
                    execution_id: metadata_string(&item.metadata, "execution_id"),
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

pub fn task_summary_to_feed_item(scope: &ScopeRef, task: TaskListItemV3) -> FeedItem {
    // A run holding a staged diff is not "running" from the user's side — it
    // is stopped, waiting on them, and the card has to say so or the only
    // signal is a spinner that never resolves. `awaiting_diff_approval` is
    // already false for every terminal task and for every task with no
    // pending proposal, so nothing that does not hold a diff moves.
    let status = if task.awaiting_diff_approval {
        FeedItemStatus::NeedsAction
    } else {
        feed_status_from_v3_task_status(&task.status)
    };
    let updated_at = parse_timestamp_millis(&task.updated_at);
    let created_at = parse_timestamp_millis(&task.created_at);
    let summary = task
        .completion_summary
        .clone()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            (!task.description.trim().is_empty()).then(|| task.description.trim().to_string())
        });
    let mut metadata = serde_json::json!({
        "storage_backend": "v3",
        "task_status": task.status,
        "agent_id": task.agent_id,
        "execution_id": task
            .active_root_execution_id
            .clone()
            .or(task.latest_root_execution_id.clone()),
        "root_execution_id": task
            .active_root_execution_id
            .clone()
            .or(task.latest_root_execution_id.clone()),
        "active_root_execution_id": task.active_root_execution_id,
        "latest_root_execution_id": task.latest_root_execution_id,
        "last_completed_root_execution_id": task.last_completed_root_execution_id,
        "completion_outcome": task.completion_outcome,
        "completion_artifact_names": task.completion_artifact_names,
    });
    // Inserted only when true, so a card for a task that holds no diff stays
    // byte-identical to what it was before this key existed. `status` above
    // already carries the same fact for clients that only read it; this is
    // what lets a client tell "waiting on a diff" from the other
    // `NeedsAction` reasons.
    if task.awaiting_diff_approval {
        if let Some(object) = metadata.as_object_mut() {
            object.insert(
                "awaiting_diff_approval".to_string(),
                serde_json::Value::Bool(true),
            );
        }
    }

    FeedItem {
        id: format!("v3:task:{}", task.id),
        principal: scope.principal().to_string(),
        workspace: scope.workspace().to_string(),
        item_type: FeedItemType::Task,
        task_id: Some(task.id),
        ui_thread_id: Some(task.ui_thread_id),
        agent_id: Some(task.agent_id),
        title: task.title,
        summary,
        status,
        created_at,
        updated_at,
        actions: Vec::new(),
        metadata,
    }
}

pub fn feed_status_from_v3_task_status(status: &str) -> FeedItemStatus {
    match status {
        "running" | "planning" | "waiting_for_children" => FeedItemStatus::Running,
        "waiting_for_user" | "waiting_for_confirmation" | "paused" | "paused_by_user" => {
            FeedItemStatus::NeedsAction
        },
        "failed" => FeedItemStatus::Failed,
        "completed" => FeedItemStatus::Done,
        _ => FeedItemStatus::Info,
    }
}

fn parse_timestamp_millis(raw: &str) -> i64 {
    DateTime::parse_from_rfc3339(raw)
        .map(|value| value.timestamp_millis())
        .unwrap_or_else(|_| Utc::now().timestamp_millis())
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
