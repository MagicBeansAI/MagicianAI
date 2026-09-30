use std::sync::Arc;

use serde_json::json;

use super::{
    store::FeedStore,
    types::{FeedItem, FeedItemPatch, FeedItemStatus, FeedItemType},
};
use crate::magician_v2::{
    realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent},
    storage::{Task, TaskSummary},
};

#[derive(Clone)]
pub struct TaskCrudFeedProjectionAdapter {
    feed_store: FeedStore,
    event_broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
}

impl TaskCrudFeedProjectionAdapter {
    pub fn new(
        feed_store: FeedStore,
        event_broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
    ) -> Self {
        Self {
            feed_store,
            event_broadcaster,
        }
    }

    pub async fn upsert_task(&self, task: &Task) -> anyhow::Result<()> {
        let mut item = task_feed_item_from_task(task);
        let previous = self.feed_store.upsert_item(item.clone()).await?;
        if let Some(existing) = previous.as_ref() {
            item.created_at = existing.created_at;
        }
        self.emit_delta(previous, item);
        Ok(())
    }

    pub async fn refresh_task_metadata(&self, task: &Task) -> anyhow::Result<()> {
        let feed_id = format!("task:{}", task.id);
        let Some(existing) = self
            .feed_store
            .get_item(&task.principal, &task.workspace, &feed_id)
            .await?
        else {
            return self.upsert_task(task).await;
        };

        let summary = TaskSummary::from(task);
        let mut metadata = existing.metadata.as_object().cloned().unwrap_or_default();
        metadata.insert("task_status".to_string(), json!(summary.status));
        metadata.insert("priority".to_string(), json!(summary.priority));
        metadata.insert(
            "execution_id".to_string(),
            json!(task
                .active_root_execution_id
                .clone()
                .or(task.latest_root_execution_id.clone())),
        );
        metadata.insert(
            "root_execution_id".to_string(),
            json!(task
                .active_root_execution_id
                .clone()
                .or(task.latest_root_execution_id.clone())),
        );
        metadata.insert(
            "agent_id".to_string(),
            json!((!summary.agent_id.is_empty()).then(|| summary.agent_id.clone())),
        );

        let item = FeedItem {
            id: existing.id.clone(),
            principal: summary.principal.clone(),
            workspace: summary.workspace.clone(),
            item_type: existing.item_type.clone(),
            task_id: Some(summary.id.clone()),
            ui_thread_id: Some(summary.ui_thread_id.clone()),
            agent_id: (!summary.agent_id.is_empty()).then(|| summary.agent_id.clone()),
            title: summary.title.clone(),
            summary: existing.summary.clone(),
            status: feed_status_from_task(task),
            created_at: existing.created_at,
            updated_at: summary.updated_at.max(existing.updated_at),
            actions: existing.actions.clone(),
            metadata: serde_json::Value::Object(metadata),
        };
        let previous = self.feed_store.upsert_item(item.clone()).await?;
        self.emit_delta(previous, item);
        Ok(())
    }

    pub async fn remove_task_items(&self, task: &Task) -> anyhow::Result<()> {
        let removed = self
            .feed_store
            .remove_task_items(&task.principal, &task.workspace, &task.id)
            .await?;
        if let Some(broadcaster) = &self.event_broadcaster {
            for item in removed {
                broadcaster.emit_transport_only(RuntimeTransportEvent::FeedItemRemoved {
                    principal: item.principal.clone(),
                    workspace: item.workspace.clone(),
                    id: item.id.clone(),
                    task_id: item.task_id.clone(),
                    ui_thread_id: item.ui_thread_id.clone(),
                    execution_id: metadata_string(&item.metadata, "execution_id"),
                    timestamp: chrono::Utc::now().timestamp_millis(),
                });
            }
        }
        Ok(())
    }

    fn emit_delta(&self, previous: Option<FeedItem>, item: FeedItem) {
        let Some(broadcaster) = &self.event_broadcaster else {
            return;
        };

        match previous {
            None => {
                let timestamp = chrono::Utc::now().timestamp_millis();
                broadcaster.emit_transport_only(RuntimeTransportEvent::FeedItemCreated {
                    item,
                    timestamp,
                });
            },
            Some(previous) => {
                let patch = FeedItemPatch::between(&previous, &item);
                if patch.is_empty() {
                    return;
                }
                broadcaster.emit_transport_only(RuntimeTransportEvent::FeedItemUpdated {
                    principal: item.principal.clone(),
                    workspace: item.workspace.clone(),
                    id: item.id.clone(),
                    task_id: item.task_id.clone(),
                    ui_thread_id: item.ui_thread_id.clone(),
                    execution_id: metadata_string(&item.metadata, "execution_id"),
                    patch,
                    timestamp: chrono::Utc::now().timestamp_millis(),
                });
            },
        }
    }
}

pub fn task_feed_item_from_task(task: &Task) -> FeedItem {
    let summary = TaskSummary::from(task);
    FeedItem {
        id: format!("task:{}", task.id),
        principal: task.principal.clone(),
        workspace: task.workspace.clone(),
        item_type: FeedItemType::Task,
        task_id: Some(task.id.clone()),
        ui_thread_id: Some(summary.ui_thread_id.clone()),
        agent_id: (!summary.agent_id.is_empty()).then(|| summary.agent_id.clone()),
        title: summary.title.clone(),
        summary: task_summary_from_task(task),
        status: feed_status_from_task(task),
        created_at: summary.created_at,
        updated_at: summary.updated_at,
        actions: Vec::new(),
        metadata: json!({
            "task_status": summary.status,
            "priority": summary.priority,
            "execution_id": task.active_root_execution_id.clone().or(task.latest_root_execution_id.clone()),
            "root_execution_id": task.active_root_execution_id.clone().or(task.latest_root_execution_id.clone()),
            "agent_id": (!summary.agent_id.is_empty()).then(|| summary.agent_id.clone()),
        }),
    }
}

pub fn task_summary_from_task(task: &Task) -> Option<String> {
    if let Some(summary) = task.completion_summary.clone() {
        return Some(summary);
    }

    let description = task.description.trim();
    if !description.is_empty() {
        const MAX_LEN: usize = 220;
        let mut clipped = description.chars().take(MAX_LEN + 1).collect::<String>();
        if clipped.chars().count() > MAX_LEN {
            clipped = clipped.chars().take(MAX_LEN).collect::<String>();
            clipped.push_str("...");
        }
        return Some(clipped);
    }

    match task.status {
        crate::magician_v2::storage::task_models::TaskStatus::Pending => {
            Some("Task created".to_string())
        },
        crate::magician_v2::storage::task_models::TaskStatus::Planning => {
            Some("Planning in progress".to_string())
        },
        crate::magician_v2::storage::task_models::TaskStatus::Ready => {
            Some("Ready to run".to_string())
        },
        crate::magician_v2::storage::task_models::TaskStatus::Running => {
            Some("Execution started".to_string())
        },
        crate::magician_v2::storage::task_models::TaskStatus::Paused => {
            Some("Execution paused".to_string())
        },
        crate::magician_v2::storage::task_models::TaskStatus::Completed => {
            Some("Task completed".to_string())
        },
        crate::magician_v2::storage::task_models::TaskStatus::Failed => {
            Some("Task failed".to_string())
        },
        crate::magician_v2::storage::task_models::TaskStatus::Cancelled => {
            Some("Task cancelled".to_string())
        },
        crate::magician_v2::storage::task_models::TaskStatus::Deferred => {
            Some("Execution deferred".to_string())
        },
    }
}

pub fn feed_status_from_task(task: &Task) -> FeedItemStatus {
    match task.status {
        crate::magician_v2::storage::task_models::TaskStatus::Running
        | crate::magician_v2::storage::task_models::TaskStatus::Planning => FeedItemStatus::Running,
        crate::magician_v2::storage::task_models::TaskStatus::Paused => FeedItemStatus::NeedsAction,
        crate::magician_v2::storage::task_models::TaskStatus::Completed => FeedItemStatus::Done,
        crate::magician_v2::storage::task_models::TaskStatus::Failed
        | crate::magician_v2::storage::task_models::TaskStatus::Cancelled => FeedItemStatus::Failed,
        crate::magician_v2::storage::task_models::TaskStatus::Pending
        | crate::magician_v2::storage::task_models::TaskStatus::Ready
        | crate::magician_v2::storage::task_models::TaskStatus::Deferred => FeedItemStatus::Info,
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
