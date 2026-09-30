use std::{collections::HashMap, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{Mutex as AsyncMutex, RwLock};

use super::workspace::ArtifactV2Workspace;
use crate::magician_v2::{
    feed::{FeedAttentionLane, FeedStore},
    progress_channel_seam::{
        ProgressMessage, ProgressMessageKind, ProgressSeverity, ProgressSource,
    },
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V3TaskProgressSummary {
    pub task_id: String,
    pub principal: String,
    pub workspace: String,
    pub ui_thread_id: String,
    pub agent_id: String,
    pub status: String,
    pub summary: Option<String>,
    pub active_root_execution_id: Option<String>,
    pub latest_root_execution_id: Option<String>,
    pub last_completed_root_execution_id: Option<String>,
    pub updated_at: String,
    /// `true` while a terminal execution under this task has flipped
    /// status to "completed" / "failed" / "cancelled" but the output-
    /// synthesis pipeline (Step 2 of `reduce_execution_terminal*`) has
    /// not yet committed output refs.
    ///
    /// Consumers use this to render a "finalizing…" pill instead of a
    /// final-state badge, and the projection adapter uses it to hold
    /// back the terminal `task.status_changed` event so chat fan-outs
    /// don't tear down before the eventual `output.available` events
    /// arrive. Defaults to `false` on older payloads.
    #[serde(default)]
    pub synthesis_pending: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V3ExecutionProgressSummary {
    pub execution_id: String,
    pub task_id: String,
    pub root_execution_id: Option<String>,
    pub parent_execution_id: Option<String>,
    pub agent_id: String,
    pub relationship_type: String,
    pub status: String,
    pub summary: Option<String>,
    pub waiting_for_children: bool,
    pub active_child_execution_ids: Vec<String>,
    pub ready_child_output_ids: Vec<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V3AttentionSummary {
    pub attention_id: String,
    pub attention_kind: String,
    pub task_id: String,
    pub execution_id: String,
    pub summary: String,
    pub detail: Option<String>,
    #[serde(default)]
    pub entries: Vec<String>,
    pub item_count: usize,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_state_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<serde_json::Value>,
    /// HITL source (`"diff_approval"`, `"agentic"`, `"user_request"`,
    /// `"clarification"`, `"plan_approval"`, …) taken from the underlying
    /// `hitl.requested { source }` payload. Lets the frontend adapter route
    /// the user's response to the correct resume endpoint without inferring
    /// it from `attention_kind`. `None` for non-HITL groups
    /// (running / failed / max-iterations / confirmation / agent-issue).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V3ActivitySummary {
    pub activity_id: String,
    pub activity_kind: String,
    pub task_id: String,
    pub execution_id: String,
    pub summary: String,
    pub detail: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V3OutputAvailableSummary {
    pub output_id: String,
    pub task_id: String,
    pub execution_id: Option<String>,
    pub scope: String,
    pub audience: String,
    pub role: String,
    pub media_type: String,
    pub summary: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V3ProgressProjectionRecord {
    pub task: V3TaskProgressSummary,
    pub execution: Option<V3ExecutionProgressSummary>,
    #[serde(default)]
    pub children: Vec<V3ExecutionProgressSummary>,
    #[serde(default)]
    pub attention: Vec<V3AttentionSummary>,
    #[serde(default)]
    pub activities: Vec<V3ActivitySummary>,
    #[serde(default)]
    pub outputs: Vec<V3OutputAvailableSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct V3TaskAttentionRecord {
    pub task: V3TaskProgressSummary,
    #[serde(default)]
    pub attention: Vec<V3AttentionSummary>,
}

impl V3ProgressProjectionRecord {
    pub fn to_progress_messages(&self) -> Vec<ProgressMessage> {
        let mut messages = Vec::new();
        messages.push(task_status_message(&self.task));
        if let Some(execution) = self.execution.as_ref() {
            messages.push(execution_progress_message(&self.task, execution));
        }
        messages.extend(
            self.children
                .iter()
                .map(|child| child_status_message(&self.task, child)),
        );
        messages.extend(self.attention.iter().map(|attention| {
            attention_message(
                &self.task,
                self.execution.as_ref(),
                &self.children,
                attention,
            )
        }));
        messages.extend(self.activities.iter().map(|activity| {
            activity_message(
                &self.task,
                self.execution.as_ref(),
                &self.children,
                activity,
            )
        }));
        messages.extend(self.outputs.iter().map(|output| {
            output_available_message(&self.task, self.execution.as_ref(), &self.children, output)
        }));
        messages
    }
}

#[derive(Clone)]
pub struct V3TaskProgressProjectionAdapter {
    /// Direct handle on the transport bus — replaces the previous
    /// `progress_router.publish_message(message)` path. Producers
    /// emit `ProgressEvent` straight to the bus; the router is just
    /// a downstream consumer (one of many).
    event_broadcaster: Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>,
    workspace: ArtifactV2Workspace,
    /// The exact store instance used by `FeedApi`. DuckDB connections opened
    /// independently against the same file do not provide the in-process
    /// visibility contract this projection requires: a row can be committed to
    /// the WAL by one database instance while the API's already-open instance
    /// continues to serve its older snapshot. Injecting the startup store keeps
    /// publication and Attention reads on one database handle.
    attention_store: FeedStore,
    last_published: Arc<RwLock<HashMap<String, V3ProgressProjectionRecord>>>,
    publish_locks: Arc<AsyncMutex<HashMap<String, Arc<AsyncMutex<()>>>>>,
    pending_attention_recovery: Arc<AsyncMutex<HashMap<String, V3ProgressProjectionRecord>>>,
}

impl V3TaskProgressProjectionAdapter {
    pub fn new(
        event_broadcaster: Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>,
        workspace: ArtifactV2Workspace,
        attention_store: FeedStore,
    ) -> Self {
        Self {
            event_broadcaster,
            workspace,
            attention_store,
            last_published: Arc::new(RwLock::new(HashMap::new())),
            publish_locks: Arc::new(AsyncMutex::new(HashMap::new())),
            pending_attention_recovery: Arc::new(AsyncMutex::new(HashMap::new())),
        }
    }

    pub async fn publish_projection(
        &self,
        mut projection: V3ProgressProjectionRecord,
    ) -> anyhow::Result<usize> {
        let key = projection_key(&projection.task);
        let publish_lock = self.publish_lock(&key).await;
        let _publish_guard = publish_lock.lock().await;
        let previous = if let Some(cached) = self.last_published.read().await.get(&key).cloned() {
            Some(cached)
        } else {
            self.load_persisted_projection(&projection).await?
        };
        if let Some(previous) = previous.as_ref() {
            if projection_is_older(&projection, previous) {
                tracing::warn!(
                    task_id = %projection.task.task_id,
                    incoming_generation = projection_generation(&projection),
                    current_generation = projection_generation(previous),
                    "ignored stale V3 progress projection publish"
                );
                return Ok(0);
            }
        }

        apply_synthesis_pending_status_guard(&mut projection, previous.as_ref());

        if completed_without_outputs(previous.as_ref(), &projection) {
            // App workflows settle a governed typed result, not a generic
            // synthesized chat output. Use the same durable owner lookup as
            // terminal settlement; task IDs and UI labels are not evidence.
            let scope = crate::magician_v2::auth::ScopeRef::system_internal_unauthenticated(
                &projection.task.principal,
                &projection.task.workspace,
            );
            let app_result_owned =
                match crate::magician_v2::apps::workflows::AppWorkflowService::new(
                    self.workspace.clone(),
                )
                .is_workflow_task(&scope, &projection.task.task_id)
                .await
                {
                    Ok(owned) => owned,
                    Err(error) => {
                        tracing::warn!(
                            task_id = %projection.task.task_id,
                            %error,
                            "[PROJECTION] could not determine completed task result owner"
                        );
                        false
                    },
                };
            if !app_result_owned {
                tracing::warn!(
                    task_id = %projection.task.task_id,
                    agent_id = %projection.task.agent_id,
                    principal = %projection.task.principal,
                    workspace = %projection.task.workspace,
                    prior_status = %previous.as_ref()
                        .map(|value| value.task.status.as_str())
                        .unwrap_or("(no prior projection)"),
                    outputs_count = projection.outputs.len(),
                    children_count = projection.children.len(),
                    has_summary = projection.task.summary.is_some(),
                    backtrace = ?std::backtrace::Backtrace::force_capture(),
                    "[PROJECTION] completed task has no generic outputs or App result owner; \
                     chat fan-outs may close before subsequent output.available events"
                );
            }
        }

        let messages = diff_projection(previous.as_ref(), &projection);
        if messages.is_empty() {
            self.persist_projection(&projection).await?;
            self.persist_attention_snapshot(&projection).await?;
            self.persist_attention_feed_projection_fail_open(&key, &projection)
                .await;
            self.last_published.write().await.insert(key, projection);
            return Ok(0);
        }

        let published = messages.len();
        for message in messages {
            self.event_broadcaster.emit_progress(message);
        }
        self.persist_projection(&projection).await?;
        self.persist_attention_snapshot(&projection).await?;
        self.persist_attention_feed_projection_fail_open(&key, &projection)
            .await;
        self.last_published.write().await.insert(key, projection);
        Ok(published)
    }

    async fn load_persisted_projection(
        &self,
        projection: &V3ProgressProjectionRecord,
    ) -> anyhow::Result<Option<V3ProgressProjectionRecord>> {
        let path = self.projection_snapshot_path(&projection.task);
        match self.workspace.read_json_path(&path).await {
            Ok(record) => Ok(Some(record)),
            Err(super::service::ArtifactV2Error::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(None)
            },
            Err(error) => Err(error.into()),
        }
    }

    async fn persist_projection(
        &self,
        projection: &V3ProgressProjectionRecord,
    ) -> anyhow::Result<()> {
        let path = self.projection_snapshot_path(&projection.task);
        self.workspace
            .write_json_atomic_path(&path, projection)
            .await?;
        Ok(())
    }

    async fn persist_attention_snapshot(
        &self,
        projection: &V3ProgressProjectionRecord,
    ) -> anyhow::Result<()> {
        let path = self.attention_snapshot_path(&projection.task);
        self.workspace
            .write_json_atomic_path(
                &path,
                &V3TaskAttentionRecord {
                    task: projection.task.clone(),
                    attention: projection.attention.clone(),
                },
            )
            .await?;
        Ok(())
    }

    async fn persist_attention_feed_projection(
        &self,
        projection: &V3ProgressProjectionRecord,
    ) -> anyhow::Result<()> {
        let items = projection
            .attention
            .iter()
            .filter(|attention| attention_kind_surfaces_in_feed(&attention.attention_kind))
            .map(|attention| super::service::build_attention_feed_item(&projection.task, attention))
            .collect();
        self.attention_store
            .reconcile_attention_projection(
                &projection.task.principal,
                &projection.task.workspace,
                FeedAttentionLane::Requests,
                "v3_task_attention",
                &projection.task.task_id,
                projection_generation(projection),
                None,
                items,
            )
            .await?;
        Ok(())
    }

    async fn persist_attention_feed_projection_fail_open(
        &self,
        key: &str,
        projection: &V3ProgressProjectionRecord,
    ) {
        match self.persist_attention_feed_projection(projection).await {
            Ok(()) => {
                let generation = projection_generation(projection);
                let mut pending = self.pending_attention_recovery.lock().await;
                if pending
                    .get(key)
                    .is_some_and(|queued| projection_generation(queued) <= generation)
                {
                    pending.remove(key);
                }
            },
            Err(error) => {
                tracing::warn!(
                    %error,
                    task_id = %projection.task.task_id,
                    "V3 task mutation committed but attention feed projection failed; queued recovery"
                );
                let mut pending = self.pending_attention_recovery.lock().await;
                let replace = pending
                    .get(key)
                    .map(|queued| {
                        projection_generation(projection) >= projection_generation(queued)
                    })
                    .unwrap_or(true);
                if replace {
                    pending.insert(key.to_string(), projection.clone());
                }
                drop(pending);
                let adapter = self.clone();
                let key = key.to_string();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    adapter.retry_pending_attention_projection(&key).await;
                });
            },
        }
    }

    async fn retry_pending_attention_projection(&self, key: &str) {
        let publish_lock = self.publish_lock(key).await;
        let _publish_guard = publish_lock.lock().await;
        let projection = self
            .pending_attention_recovery
            .lock()
            .await
            .get(key)
            .cloned();
        let Some(projection) = projection else {
            return;
        };
        match self.persist_attention_feed_projection(&projection).await {
            Ok(()) => {
                let generation = projection_generation(&projection);
                let mut pending = self.pending_attention_recovery.lock().await;
                if pending
                    .get(key)
                    .is_some_and(|queued| projection_generation(queued) <= generation)
                {
                    pending.remove(key);
                }
            },
            Err(error) => {
                tracing::warn!(
                    %error,
                    task_id = %projection.task.task_id,
                    "queued V3 attention feed projection retry failed; startup recovery remains available"
                );
            },
        }
    }

    async fn publish_lock(&self, key: &str) -> Arc<AsyncMutex<()>> {
        self.publish_locks
            .lock()
            .await
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    fn projection_snapshot_path(&self, task: &V3TaskProgressSummary) -> std::path::PathBuf {
        self.workspace.task_progress_projection_path(
            &task.principal,
            &task.workspace,
            &task.task_id,
        )
    }

    fn attention_snapshot_path(&self, task: &V3TaskProgressSummary) -> std::path::PathBuf {
        self.workspace.task_attention_projection_path(
            &task.principal,
            &task.workspace,
            &task.task_id,
        )
    }
}

fn attention_kind_surfaces_in_feed(kind: &str) -> bool {
    matches!(
        kind,
        "input.requested"
            | "waiting_for_confirmation"
            | "max_iterations_reached"
            | "user_request.pending"
            | "hitl.requested"
            | "agent.circuit.opened"
            | "agent.cycle.failed"
            | "agent.goal.failed"
    )
}

fn task_status_message(task: &V3TaskProgressSummary) -> ProgressMessage {
    base_message(
        ProgressSeverity::from_task_status(&task.status),
        ProgressMessageKind::StatusChanged {
            status: task.status.clone(),
            summary: task.summary.clone(),
        },
        task.principal.clone(),
        task.workspace.clone(),
        task.active_root_execution_id
            .clone()
            .or(task.latest_root_execution_id.clone())
            .or(task.last_completed_root_execution_id.clone()),
        Some(task.task_id.clone()),
        Some(task.task_id.clone()),
        task.active_root_execution_id
            .clone()
            .or(task.latest_root_execution_id.clone())
            .or(task.last_completed_root_execution_id.clone()),
        None,
        Some(task.agent_id.clone()),
        Some(task.ui_thread_id.clone()),
        &task.updated_at,
    )
}

fn child_status_message(
    task: &V3TaskProgressSummary,
    child: &V3ExecutionProgressSummary,
) -> ProgressMessage {
    base_message(
        ProgressSeverity::from_execution_status(&child.status),
        ProgressMessageKind::ChildStatusChanged {
            child_execution_id: child.execution_id.clone(),
            status: child.status.clone(),
            summary: child.summary.clone(),
        },
        task.principal.clone(),
        task.workspace.clone(),
        Some(child.execution_id.clone()),
        Some(child.task_id.clone()),
        Some(child.task_id.clone()),
        child.root_execution_id.clone(),
        child.parent_execution_id.clone(),
        Some(child.agent_id.clone()),
        Some(task.ui_thread_id.clone()),
        &child.updated_at,
    )
}

fn execution_progress_message(
    task: &V3TaskProgressSummary,
    execution: &V3ExecutionProgressSummary,
) -> ProgressMessage {
    base_message(
        ProgressSeverity::from_execution_status(&execution.status),
        ProgressMessageKind::AgentNotification {
            event_type: "execution.progress".to_string(),
            message: execution
                .summary
                .clone()
                .unwrap_or_else(|| format!("Execution is {}.", execution.status.replace('_', " "))),
            entity_key: Some(format!("execution:{}", execution.execution_id)),
            metadata: json!({
                "execution_id": execution.execution_id,
                "task_id": execution.task_id,
                "status": execution.status,
                "relationship_type": execution.relationship_type,
                "waiting_for_children": execution.waiting_for_children,
                "active_child_execution_ids": execution.active_child_execution_ids,
                "ready_child_output_ids": execution.ready_child_output_ids,
            }),
        },
        task.principal.clone(),
        task.workspace.clone(),
        Some(execution.execution_id.clone()),
        Some(task.task_id.clone()),
        Some(task.task_id.clone()),
        execution.root_execution_id.clone(),
        execution.parent_execution_id.clone(),
        Some(execution.agent_id.clone()),
        Some(task.ui_thread_id.clone()),
        &execution.updated_at,
    )
}

fn attention_message(
    task: &V3TaskProgressSummary,
    execution: Option<&V3ExecutionProgressSummary>,
    children: &[V3ExecutionProgressSummary],
    attention: &V3AttentionSummary,
) -> ProgressMessage {
    let context = execution_progress_context(
        execution,
        children,
        Some(attention.execution_id.as_str()),
        task,
    );
    // HITL attention summaries (approval, user_request, clarification)
    // map to the canonical `hitl.requested` event_type so they share a
    // single taxonomy row with the wire-side canonical emission. The
    // legacy `attention_kind` strings stay on the metadata field for
    // downstream consumers (`feed_api.rs::requests` badge filter,
    // attention-bar buckets) that key on those vocabulary values.
    // Non-HITL attention kinds (input.requested, waiting_for_confirmation,
    // max_iterations_reached, execution.failed, running) keep their
    // attention_kind string as the event_type — they have their own
    // taxonomy rows in `ArtifactV2EventType`.
    let event_type = match attention.attention_kind.as_str() {
        // Pending side maps to canonical `hitl.requested`.
        "approval.requested" | "user_request.pending" | "clarification.queued" => {
            "hitl.requested".to_string()
        },
        // Resolved / expired / response-received side maps to canonical
        // `hitl.resolved`. Outcome lives in payload elsewhere; here the
        // projection emits a single canonical resolution event_type.
        "approval.resolved"
        | "approval.expired"
        | "user_request.resolved"
        | "clarification.response_received" => "hitl.resolved".to_string(),
        other => other.to_string(),
    };
    // Derive `source` so consumers that dispatch on payload.source
    // (feed materializer, chat surface, AttentionBar respond router)
    // route HITL projections correctly. The legacy paths key on the
    // `attention_kind` prefix; the canonical `hitl.requested` paths
    // (V3 clarification + plan_approval) all share the same
    // `attention_kind: "hitl.requested"`, so we disambiguate via the
    // `attention_id` prefix that each grouping site stamps.
    let source = match attention.attention_kind.as_str() {
        s if s.starts_with("approval.") => Some("approval"),
        s if s.starts_with("user_request.") => Some("user_request"),
        s if s.starts_with("clarification.") => Some("clarification"),
        "hitl.requested" => {
            if attention
                .attention_id
                .starts_with("attention:clarification:")
            {
                Some("clarification")
            } else if attention
                .attention_id
                .starts_with("attention:plan_approval:")
            {
                Some("plan_approval")
            } else {
                None
            }
        },
        _ => None,
    };
    base_message(
        ProgressSeverity::from_attention_kind(&attention.attention_kind),
        ProgressMessageKind::AgentNotification {
            event_type,
            message: render_attention_message(attention),
            entity_key: Some(attention.attention_id.clone()),
            metadata: json!({
                "attention_id": attention.attention_id,
                "attention_kind": attention.attention_kind,
                "source": source,
                "execution_id": attention.execution_id,
                "detail": attention.detail,
                "entries": attention.entries,
                "item_count": attention.item_count,
                "pause_state_id": attention.pause_state_id,
                "input_type": attention.input_type,
                "hint": attention.hint,
                "options": attention.options,
                "input_schema": attention.input_schema,
            }),
        },
        task.principal.clone(),
        task.workspace.clone(),
        Some(attention.execution_id.clone()),
        Some(task.task_id.clone()),
        Some(task.task_id.clone()),
        context.root_execution_id,
        context.parent_execution_id,
        context.agent_id,
        Some(task.ui_thread_id.clone()),
        &attention.updated_at,
    )
}

fn activity_message(
    task: &V3TaskProgressSummary,
    execution: Option<&V3ExecutionProgressSummary>,
    children: &[V3ExecutionProgressSummary],
    activity: &V3ActivitySummary,
) -> ProgressMessage {
    let context = execution_progress_context(
        execution,
        children,
        Some(activity.execution_id.as_str()),
        task,
    );
    base_message(
        ProgressSeverity::Info,
        ProgressMessageKind::AgentNotification {
            event_type: activity.activity_kind.clone(),
            message: activity.summary.clone(),
            entity_key: Some(activity.activity_id.clone()),
            metadata: json!({
                "activity_id": activity.activity_id,
                "activity_kind": activity.activity_kind,
                "execution_id": activity.execution_id,
                "detail": activity.detail,
            }),
        },
        task.principal.clone(),
        task.workspace.clone(),
        Some(activity.execution_id.clone()),
        Some(task.task_id.clone()),
        Some(task.task_id.clone()),
        context.root_execution_id,
        context.parent_execution_id,
        context.agent_id,
        Some(task.ui_thread_id.clone()),
        &activity.updated_at,
    )
}

fn output_available_message(
    task: &V3TaskProgressSummary,
    execution: Option<&V3ExecutionProgressSummary>,
    children: &[V3ExecutionProgressSummary],
    output: &V3OutputAvailableSummary,
) -> ProgressMessage {
    let context =
        execution_progress_context(execution, children, output.execution_id.as_deref(), task);
    base_message(
        ProgressSeverity::Info,
        ProgressMessageKind::AgentNotification {
            event_type: "output.available".to_string(),
            message: output.summary.clone().unwrap_or_else(|| {
                format!(
                    "{} output available ({})",
                    output.audience.replace('_', " "),
                    output.role
                )
            }),
            entity_key: Some(format!("output:{}", output.output_id)),
            metadata: json!({
                "output_id": output.output_id,
                "execution_id": output.execution_id,
                "scope": output.scope,
                "audience": output.audience,
                "role": output.role,
                "media_type": output.media_type,
            }),
        },
        task.principal.clone(),
        task.workspace.clone(),
        output
            .execution_id
            .clone()
            .or_else(|| execution.map(|value| value.execution_id.clone())),
        Some(task.task_id.clone()),
        Some(task.task_id.clone()),
        context.root_execution_id,
        context.parent_execution_id,
        context.agent_id,
        Some(task.ui_thread_id.clone()),
        &output.created_at,
    )
}

struct ExecutionProgressContext {
    root_execution_id: Option<String>,
    parent_execution_id: Option<String>,
    agent_id: Option<String>,
}

fn execution_progress_context(
    execution: Option<&V3ExecutionProgressSummary>,
    children: &[V3ExecutionProgressSummary],
    execution_id: Option<&str>,
    task: &V3TaskProgressSummary,
) -> ExecutionProgressContext {
    let matched = execution_id.and_then(|target| {
        children
            .iter()
            .find(|child| child.execution_id == target)
            .or_else(|| execution.filter(|current| current.execution_id == target))
    });
    let resolved = matched.or(execution);
    ExecutionProgressContext {
        root_execution_id: resolved.and_then(|value| value.root_execution_id.clone()),
        parent_execution_id: resolved.and_then(|value| value.parent_execution_id.clone()),
        agent_id: resolved
            .map(|value| value.agent_id.clone())
            .or_else(|| Some(task.agent_id.clone())),
    }
}

fn base_message(
    severity: ProgressSeverity,
    kind: ProgressMessageKind,
    principal: String,
    workspace: String,
    execution_id: Option<String>,
    task_id: Option<String>,
    root_task_id: Option<String>,
    root_execution_id: Option<String>,
    parent_execution_id: Option<String>,
    agent_id: Option<String>,
    ui_thread_id: Option<String>,
    timestamp: &str,
) -> ProgressMessage {
    // Mirror the inner event_type when this is an AgentNotification —
    // surface routing reads it to look up render hints in the master
    // taxonomy. Other ProgressMessageKind variants get a synthetic
    // canonical key (see `task.*` / `execution.*` rows in
    // GAUI_EVENT_TAXONOMY).
    let event_type = match &kind {
        ProgressMessageKind::AgentNotification { event_type, .. } => Some(event_type.clone()),
        ProgressMessageKind::StatusChanged { .. } => Some("task.status_changed".to_string()),
        ProgressMessageKind::ChildStatusChanged { .. } => {
            Some("task.child.status_changed".to_string())
        },
        ProgressMessageKind::ActionProgress { .. } => Some("task.action_progress".to_string()),
        ProgressMessageKind::HandedOver { .. } => Some("execution.handed_over".to_string()),
    };
    let mut message = ProgressMessage {
        id: uuid::Uuid::new_v4().to_string(),
        seq: 0,
        log_key: String::new(),
        source: ProgressSource::Projection,
        event_type,
        metadata: Default::default(),
        execution_id,
        task_id,
        root_task_id,
        root_execution_id,
        parent_execution_id,
        agent_id,
        ui_thread_id,
        step_id: None,
        routing_keys: Vec::new(),
        principal,
        workspace,
        severity,
        kind,
        timestamp: parse_timestamp_millis(timestamp),
    };
    message.routing_keys = derive_routing_keys(&message);
    message.log_key = default_log_key(&message);
    message
}

fn parse_timestamp_millis(raw: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|value| value.timestamp_millis())
        .unwrap_or_else(|_| chrono::Utc::now().timestamp_millis())
}

fn derive_routing_keys(message: &ProgressMessage) -> Vec<String> {
    let mut keys = Vec::new();
    if let Some(root_task_id) = message.root_task_id.as_deref() {
        keys.push(format!("task/{root_task_id}"));
        if let Some(agent_id) = message.agent_id.as_deref() {
            keys.push(format!("task/{root_task_id}/agent/{agent_id}"));
        }
        if let Some(execution_id) = message.execution_id.as_deref() {
            keys.push(format!("task/{root_task_id}/execution/{execution_id}"));
        }
        if let Some(root_execution_id) = message.root_execution_id.as_deref() {
            keys.push(format!(
                "task/{root_task_id}/root-execution/{root_execution_id}"
            ));
        }
        if let Some(parent_execution_id) = message.parent_execution_id.as_deref() {
            keys.push(format!(
                "task/{root_task_id}/parent-execution/{parent_execution_id}"
            ));
        }
    }
    if let Some(thread_id) = message.ui_thread_id.as_deref() {
        keys.push(format!("thread/{thread_id}"));
        if let Some(root_task_id) = message.root_task_id.as_deref() {
            keys.push(format!("thread/{thread_id}/task/{root_task_id}"));
        }
        if let Some(agent_id) = message.agent_id.as_deref() {
            keys.push(format!("thread/{thread_id}/agent/{agent_id}"));
        }
    }
    if let Some(agent_id) = message.agent_id.as_deref() {
        keys.push(format!("agent/{agent_id}"));
    }
    keys
}

fn default_log_key(message: &ProgressMessage) -> String {
    if let Some(task_id) = message.root_task_id.as_ref() {
        format!("task:{task_id}")
    } else if let Some(agent_id) = message.agent_id.as_ref() {
        format!("agent:{agent_id}")
    } else if let Some(execution_id) = message.execution_id.as_ref() {
        format!("execution:{execution_id}")
    } else {
        "misc:unscoped".to_string()
    }
}

trait ProgressSeverityExt {
    fn from_task_status(status: &str) -> Self;
    fn from_execution_status(status: &str) -> Self;
    fn from_attention_kind(kind: &str) -> Self;
}

impl ProgressSeverityExt for ProgressSeverity {
    fn from_task_status(status: &str) -> Self {
        match status {
            "failed" => ProgressSeverity::Error,
            "waiting_for_user" | "waiting_for_confirmation" | "paused" | "paused_by_user" => {
                ProgressSeverity::Warning
            },
            "completed" => ProgressSeverity::Info,
            _ => ProgressSeverity::Info,
        }
    }

    fn from_execution_status(status: &str) -> Self {
        match status {
            "failed" => ProgressSeverity::Error,
            "cancelled" => ProgressSeverity::Error,
            "waiting_for_user" | "waiting_for_confirmation" | "waiting_for_children" | "paused" => {
                ProgressSeverity::Warning
            },
            _ => ProgressSeverity::Info,
        }
    }

    fn from_attention_kind(kind: &str) -> Self {
        match kind {
            "execution.failed" | "task.failed" | "max_iterations_reached" => {
                ProgressSeverity::Error
            },
            _ => ProgressSeverity::Warning,
        }
    }
}

fn projection_key(task: &V3TaskProgressSummary) -> String {
    format!("{}:{}:{}", task.principal, task.workspace, task.task_id)
}

fn projection_generation(projection: &V3ProgressProjectionRecord) -> i64 {
    let mut generation = parse_projection_timestamp(&projection.task.updated_at).unwrap_or(0);
    let mut include = |timestamp: &str| {
        if let Some(value) = parse_projection_timestamp(timestamp) {
            generation = generation.max(value);
        }
    };
    if let Some(execution) = projection.execution.as_ref() {
        include(&execution.updated_at);
    }
    for child in &projection.children {
        include(&child.updated_at);
    }
    for attention in &projection.attention {
        include(&attention.updated_at);
    }
    for activity in &projection.activities {
        include(&activity.updated_at);
    }
    for output in &projection.outputs {
        include(&output.created_at);
    }
    generation
}

fn projection_is_older(
    incoming: &V3ProgressProjectionRecord,
    current: &V3ProgressProjectionRecord,
) -> bool {
    projection_generation(incoming) < projection_generation(current)
}

fn parse_projection_timestamp(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.timestamp_millis())
}

/// Returns `true` for task statuses that represent a completed
/// lifecycle. Used by the synthesis-pending guard to detect the window
/// between Step 1 (`reduce_execution_terminal_status_only`) and Step 2
/// (`reduce_execution_terminal`) of the two-step terminal write — that
/// window is where the projection must hold back the terminal
/// `task.status_changed` event so chat fan-outs don't tear down before
/// outputs land. Keep aligned with the values
/// `persist_execution_outcome` and the reducer trait recognise.
fn is_terminal_task_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled")
}

/// Synthesis-pending guard for the two-step terminal write.
///
/// Step 1 (`reduce_execution_terminal_status_only`) flips
/// `task.state.status` to "completed" / "failed" / "cancelled" AND sets
/// `synthesis_pending = true`, then spawns the synthesis pipeline in
/// the background. The orchestrator then calls
/// `project_task_progress` on the way out — which lands here while
/// synthesis is still in flight. If we let the diff emit
/// `task.status_changed: completed` now, chat fan-outs tear down on
/// that signal and miss the subsequent `output.available` events that
/// arrive once Step 2 commits the output refs.
///
/// Hold the visible status at the previously-published (non-terminal)
/// value while `synthesis_pending = true`. The next publish — fired by
/// Step 2 after synthesis lands and `synthesis_pending` is cleared —
/// sees the real running → completed transition and emits a clean
/// status change with outputs already registered.
///
/// `synthesis_pending` remains `true` in the cached projection so
/// downstream consumers (UI list endpoints) can render "finalizing…".
/// If synthesis fails (the rare path), the reducer clears
/// `synthesis_pending` while leaving the terminal status in place —
/// the subsequent publish will then emit the terminal event normally
/// because `synthesis_pending = false` and the guard is silent.
fn apply_synthesis_pending_status_guard(
    projection: &mut V3ProgressProjectionRecord,
    previous: Option<&V3ProgressProjectionRecord>,
) {
    if !projection.task.synthesis_pending {
        return;
    }
    if !is_terminal_task_status(&projection.task.status) {
        return;
    }
    let prior_status = previous
        .map(|cached| cached.task.status.clone())
        .filter(|status| !is_terminal_task_status(status))
        .unwrap_or_else(|| "running".to_string());
    tracing::debug!(
        target: "artifact_v2.progress",
        task_id = %projection.task.task_id,
        terminal_status = %projection.task.status,
        held_at = %prior_status,
        "holding terminal task.status_changed until synthesis lands"
    );
    projection.task.status = prior_status;
}

// Diagnose only a new completed transition. Synthesis-pending transitions
// have already been held by apply_synthesis_pending_status_guard.
fn completed_without_outputs(
    previous: Option<&V3ProgressProjectionRecord>,
    current: &V3ProgressProjectionRecord,
) -> bool {
    current.task.status == "completed"
        && previous.is_none_or(|value| value.task.status != "completed")
        && current.outputs.is_empty()
}

fn diff_projection(
    previous: Option<&V3ProgressProjectionRecord>,
    current: &V3ProgressProjectionRecord,
) -> Vec<ProgressMessage> {
    let mut messages = Vec::new();

    if previous
        .map(|value| task_progress_changed(&value.task, &current.task))
        .unwrap_or(true)
    {
        messages.push(task_status_message(&current.task));
    }

    if current
        .execution
        .as_ref()
        .map(|execution| {
            previous
                .and_then(|value| value.execution.as_ref())
                .map(|previous_execution| execution_progress_changed(previous_execution, execution))
                .unwrap_or(true)
        })
        .unwrap_or(false)
    {
        if let Some(execution) = current.execution.as_ref() {
            messages.push(execution_progress_message(&current.task, execution));
        }
    }

    let previous_children = previous
        .map(|value| {
            value
                .children
                .iter()
                .map(|child| (child.execution_id.as_str(), child))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    for child in &current.children {
        let changed = previous_children
            .get(child.execution_id.as_str())
            .map(|previous_child| child_progress_changed(previous_child, child))
            .unwrap_or(true);
        if changed {
            messages.push(child_status_message(&current.task, child));
        }
    }

    let previous_attention = previous
        .map(|value| {
            value
                .attention
                .iter()
                .map(|attention| (attention.attention_id.as_str(), attention))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    for attention in &current.attention {
        let changed = previous_attention
            .get(attention.attention_id.as_str())
            .map(|previous_attention| attention_changed(previous_attention, attention))
            .unwrap_or(true);
        if changed {
            let context = lookup_execution_context(current, Some(attention.execution_id.as_str()));
            messages.push(attention_message(
                &current.task,
                context,
                &current.children,
                attention,
            ));
        }
    }

    let previous_activities = previous
        .map(|value| {
            value
                .activities
                .iter()
                .map(|activity| (activity.activity_id.as_str(), activity))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    for activity in &current.activities {
        let changed = previous_activities
            .get(activity.activity_id.as_str())
            .map(|previous_activity| activity_changed(previous_activity, activity))
            .unwrap_or(true);
        if changed {
            let context = lookup_execution_context(current, Some(activity.execution_id.as_str()));
            messages.push(activity_message(
                &current.task,
                context,
                &current.children,
                activity,
            ));
        }
    }

    let previous_output_ids = previous
        .map(|value| {
            value
                .outputs
                .iter()
                .map(|output| output.output_id.as_str())
                .collect::<std::collections::HashSet<_>>()
        })
        .unwrap_or_default();
    for output in &current.outputs {
        if !previous_output_ids.contains(output.output_id.as_str()) {
            let context = output
                .execution_id
                .as_deref()
                .and_then(|execution_id| lookup_execution_context(current, Some(execution_id)))
                .or(current.execution.as_ref());
            messages.push(output_available_message(
                &current.task,
                context,
                &current.children,
                output,
            ));
        }
    }

    messages
}

fn lookup_execution_context<'a>(
    projection: &'a V3ProgressProjectionRecord,
    execution_id: Option<&str>,
) -> Option<&'a V3ExecutionProgressSummary> {
    let execution_id = execution_id?;
    projection
        .execution
        .as_ref()
        .filter(|execution| execution.execution_id == execution_id)
        .or_else(|| {
            projection
                .children
                .iter()
                .find(|child| child.execution_id == execution_id)
        })
}

fn task_progress_changed(
    previous: &V3TaskProgressSummary,
    current: &V3TaskProgressSummary,
) -> bool {
    previous.status != current.status
        || previous.summary != current.summary
        // Surface the synthesis window: when a task flips to terminal with
        // `synthesis_pending = true`, the status guard holds the visible status
        // at "running", so without this the diff wouldn't fire and the card
        // couldn't show "Preparing final result…". Comparing the flag makes the
        // false→true (enter synthesis) and true→false (synthesis landed)
        // transitions each emit a status event.
        || previous.synthesis_pending != current.synthesis_pending
        || previous.active_root_execution_id != current.active_root_execution_id
        || previous.latest_root_execution_id != current.latest_root_execution_id
        || previous.last_completed_root_execution_id != current.last_completed_root_execution_id
}

fn child_progress_changed(
    previous: &V3ExecutionProgressSummary,
    current: &V3ExecutionProgressSummary,
) -> bool {
    previous.status != current.status
        || previous.summary != current.summary
        || previous.waiting_for_children != current.waiting_for_children
        || previous.active_child_execution_ids != current.active_child_execution_ids
        || previous.ready_child_output_ids != current.ready_child_output_ids
}

fn execution_progress_changed(
    previous: &V3ExecutionProgressSummary,
    current: &V3ExecutionProgressSummary,
) -> bool {
    child_progress_changed(previous, current)
        || previous.relationship_type != current.relationship_type
}

fn attention_changed(previous: &V3AttentionSummary, current: &V3AttentionSummary) -> bool {
    previous.attention_kind != current.attention_kind
        || previous.summary != current.summary
        || previous.detail != current.detail
        || previous.entries != current.entries
        || previous.item_count != current.item_count
        || previous.pause_state_id != current.pause_state_id
        || previous.input_type != current.input_type
        || previous.hint != current.hint
        || previous.options != current.options
        || previous.input_schema != current.input_schema
}

fn activity_changed(previous: &V3ActivitySummary, current: &V3ActivitySummary) -> bool {
    previous.activity_kind != current.activity_kind
        || previous.summary != current.summary
        || previous.detail != current.detail
}

fn render_attention_message(attention: &V3AttentionSummary) -> String {
    if attention.entries.is_empty() {
        if let Some(detail) = attention
            .detail
            .as_ref()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
        {
            return format!("{}\n\n{}", attention.summary, detail);
        }
        return attention.summary.clone();
    }

    let rendered_entries = attention
        .entries
        .iter()
        .map(|entry| format!("- {entry}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{}\n\n{}", attention.summary, rendered_entries)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::{
        V3AttentionSummary, V3ExecutionProgressSummary, V3OutputAvailableSummary,
        V3ProgressProjectionRecord, V3TaskProgressProjectionAdapter, V3TaskProgressSummary,
    };
    use crate::magician_v2::{
        artifact_v2::workspace::ArtifactV2Workspace,
        feed::{FeedAttentionLane, FeedAttentionPageQuery, FeedStore},
        progress_channel_seam::{ProgressMessageKind, ProgressSource},
        realtime_events::RuntimeTransportBroadcaster,
    };

    fn sample_task() -> V3TaskProgressSummary {
        V3TaskProgressSummary {
            task_id: "task-1".to_string(),
            principal: "principal-a".to_string(),
            workspace: "workspace-a".to_string(),
            ui_thread_id: "thread-1".to_string(),
            agent_id: "personal-agent".to_string(),
            status: "running".to_string(),
            summary: Some("Task is running.".to_string()),
            active_root_execution_id: Some("exec-root".to_string()),
            latest_root_execution_id: Some("exec-root".to_string()),
            last_completed_root_execution_id: None,
            updated_at: "2026-03-28T10:00:00Z".to_string(),
            synthesis_pending: false,
        }
    }

    fn sample_execution(status: &str, waiting_for_children: bool) -> V3ExecutionProgressSummary {
        V3ExecutionProgressSummary {
            execution_id: "exec-root".to_string(),
            task_id: "task-1".to_string(),
            root_execution_id: Some("exec-root".to_string()),
            parent_execution_id: None,
            agent_id: "personal-agent".to_string(),
            relationship_type: "root".to_string(),
            status: status.to_string(),
            summary: Some("Waiting on 2 delegated execution(s).".to_string()),
            waiting_for_children,
            active_child_execution_ids: vec![
                "exec-child-1".to_string(),
                "exec-child-2".to_string(),
            ],
            ready_child_output_ids: vec!["out-child-1".to_string()],
            updated_at: "2026-03-28T10:00:01Z".to_string(),
        }
    }

    fn sample_child_execution() -> V3ExecutionProgressSummary {
        V3ExecutionProgressSummary {
            execution_id: "exec-child-1".to_string(),
            task_id: "task-1".to_string(),
            root_execution_id: Some("exec-root".to_string()),
            parent_execution_id: Some("exec-root".to_string()),
            agent_id: "delegate-agent".to_string(),
            relationship_type: "delegate".to_string(),
            status: "waiting_for_user".to_string(),
            summary: Some("Waiting for input.".to_string()),
            waiting_for_children: false,
            active_child_execution_ids: Vec::new(),
            ready_child_output_ids: Vec::new(),
            updated_at: "2026-03-28T10:00:02Z".to_string(),
        }
    }

    fn empty_projection_with_task(task: V3TaskProgressSummary) -> V3ProgressProjectionRecord {
        V3ProgressProjectionRecord {
            task,
            execution: None,
            children: Vec::new(),
            attention: Vec::new(),
            activities: Vec::new(),
            outputs: Vec::new(),
        }
    }

    fn task_with(status: &str, synthesis_pending: bool) -> V3TaskProgressSummary {
        let mut task = sample_task();
        task.status = status.to_string();
        task.synthesis_pending = synthesis_pending;
        task
    }

    #[tokio::test]
    async fn published_hitl_is_immediately_visible_through_shared_attention_store() {
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let feed_store = FeedStore::open_workspace(workspace.clone()).unwrap();
        // Materialize first, as the production Feed API does at startup. The
        // regression was specifically a second independently-opened DuckDB
        // instance writing after this reader-side store was already live.
        feed_store
            .materialize_scope("principal-a", "workspace-a")
            .await
            .unwrap();
        let adapter = V3TaskProgressProjectionAdapter::new(
            Arc::new(RuntimeTransportBroadcaster::new(16)),
            workspace,
            feed_store.clone(),
        );
        let mut projection = empty_projection_with_task(sample_task());
        projection.attention.push(V3AttentionSummary {
            attention_id: "attention:clarification:task-1:planexec-1:question-1".to_string(),
            attention_kind: "hitl.requested".to_string(),
            task_id: "task-1".to_string(),
            execution_id: "planexec-1".to_string(),
            summary: "Planning clarification".to_string(),
            detail: Some("Which publication date should be used?".to_string()),
            entries: vec!["Which publication date should be used?".to_string()],
            item_count: 1,
            updated_at: "2026-03-28T10:00:03Z".to_string(),
            pause_state_id: Some("question-1".to_string()),
            input_type: Some("text".to_string()),
            hint: None,
            options: None,
            input_schema: Some(json!({"type": "text"})),
            source: Some("clarification".to_string()),
        });

        adapter.publish_projection(projection).await.unwrap();

        let page = feed_store
            .list_attention_lane_page(FeedAttentionPageQuery {
                exclude_task_ids: Vec::new(),
                principal: "principal-a".to_string(),
                workspace: "workspace-a".to_string(),
                lane: FeedAttentionLane::Requests,
                ui_thread_id: None,
                cursor: None,
                offset: 0,
                limit: 20,
            })
            .await
            .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.request_hitl_total, Some(1));
        assert_eq!(page.items[0].task_id.as_deref(), Some("task-1"));
        assert_eq!(
            page.items[0]
                .metadata
                .get("source")
                .and_then(serde_json::Value::as_str),
            Some("clarification")
        );
    }

    #[test]
    fn projection_ordering_rejects_an_older_per_task_publish() {
        let mut current = empty_projection_with_task(sample_task());
        current.task.updated_at = "2026-03-28T10:00:02Z".to_string();
        let mut incoming = empty_projection_with_task(sample_task());
        incoming.task.updated_at = "2026-03-28T10:00:01Z".to_string();

        assert!(super::projection_is_older(&incoming, &current));
        assert!(!super::projection_is_older(&current, &incoming));
    }

    #[test]
    fn missing_output_diagnostic_only_checks_new_completed_transitions() {
        let running = empty_projection_with_task(task_with("running", false));
        let completed = empty_projection_with_task(task_with("completed", false));
        let failed = empty_projection_with_task(task_with("failed", false));
        assert!(super::completed_without_outputs(Some(&running), &completed));
        assert!(super::completed_without_outputs(None, &completed));
        assert!(!super::completed_without_outputs(
            Some(&completed),
            &completed
        ));
        assert!(!super::completed_without_outputs(Some(&running), &failed));
        let mut pending = empty_projection_with_task(task_with("completed", true));
        super::apply_synthesis_pending_status_guard(&mut pending, Some(&running));
        assert!(!super::completed_without_outputs(Some(&running), &pending));
    }

    #[test]
    fn synthesis_guard_holds_completed_until_synthesis_lands() {
        // Previous publish: running.
        let previous = empty_projection_with_task(task_with("running", false));
        // Current publish: status=completed but synthesis_pending=true
        // (Step 1 of two-step terminal write, before outputs land).
        let mut current = empty_projection_with_task(task_with("completed", true));

        super::apply_synthesis_pending_status_guard(&mut current, Some(&previous));

        // Guard coerces visible status back to "running" so the diff
        // does not emit task.status_changed:completed yet.
        assert_eq!(current.task.status, "running");
        // synthesis_pending is preserved so the UI can render the
        // "finalizing…" pill.
        assert!(current.task.synthesis_pending);

        let messages = super::diff_projection(Some(&previous), &current);
        assert!(
            !messages
                .iter()
                .any(|m| matches!(&m.kind, ProgressMessageKind::StatusChanged { status, .. } if status == "completed")),
            "must not emit status_changed:completed while synthesis is pending"
        );
    }

    #[test]
    fn synthesis_guard_passes_through_after_synthesis_clears() {
        // Previous publish: running (held by Step 1 guard).
        let previous = empty_projection_with_task(task_with("running", true));
        // Current publish: synthesis landed — status=completed and
        // synthesis_pending=false.
        let mut current = empty_projection_with_task(task_with("completed", false));

        super::apply_synthesis_pending_status_guard(&mut current, Some(&previous));

        // Guard is silent — synthesis_pending is false.
        assert_eq!(current.task.status, "completed");

        let messages = super::diff_projection(Some(&previous), &current);
        assert!(
            messages
                .iter()
                .any(|m| matches!(&m.kind, ProgressMessageKind::StatusChanged { status, .. } if status == "completed")),
            "expected the clean running→completed transition to emit"
        );
    }

    #[test]
    fn synthesis_guard_uses_running_when_no_previous_publish() {
        // Cold start: no previous projection cached. Step 1 publishes
        // status=completed,synthesis_pending=true. Coerce to "running"
        // since there's no prior non-terminal status to fall back on.
        let mut current = empty_projection_with_task(task_with("completed", true));

        super::apply_synthesis_pending_status_guard(&mut current, None);

        assert_eq!(current.task.status, "running");
        assert!(current.task.synthesis_pending);
    }

    #[test]
    fn synthesis_guard_ignored_for_non_terminal_status() {
        // synthesis_pending should never be true while non-terminal,
        // but defend against the impossible state — guard must not
        // touch non-terminal status values.
        let previous = empty_projection_with_task(task_with("running", false));
        let mut current = empty_projection_with_task(task_with("waiting_for_user", true));

        super::apply_synthesis_pending_status_guard(&mut current, Some(&previous));

        assert_eq!(current.task.status, "waiting_for_user");
    }

    #[test]
    fn synthesis_guard_fires_for_failed_status_too() {
        // Same hazard for "failed" — Step 1 sets task.state.status =
        // "failed" + synthesis_pending = true. Guard must hold the
        // failed projection too so output.available events (e.g. an
        // error report artefact synthesised by the failure pipeline)
        // can reach fan-outs.
        let previous = empty_projection_with_task(task_with("running", false));
        let mut current = empty_projection_with_task(task_with("failed", true));

        super::apply_synthesis_pending_status_guard(&mut current, Some(&previous));

        assert_eq!(current.task.status, "running");
    }

    #[test]
    fn v3_projection_messages_are_marked_as_projection_source() {
        let record = V3ProgressProjectionRecord {
            task: sample_task(),
            execution: Some(sample_execution("waiting_for_children", true)),
            children: Vec::new(),
            attention: Vec::new(),
            activities: Vec::new(),
            outputs: Vec::new(),
        };

        let messages = record.to_progress_messages();
        assert!(!messages.is_empty());
        assert!(messages
            .iter()
            .all(|message| message.source == ProgressSource::Projection));
    }

    #[test]
    fn v3_projection_emits_execution_progress_message_for_root_execution() {
        let record = V3ProgressProjectionRecord {
            task: sample_task(),
            execution: Some(sample_execution("waiting_for_children", true)),
            children: Vec::new(),
            attention: Vec::new(),
            activities: Vec::new(),
            outputs: Vec::new(),
        };

        let messages = record.to_progress_messages();
        let execution_message = messages
            .iter()
            .find(|message| {
                matches!(
                    &message.kind,
                    ProgressMessageKind::AgentNotification { event_type, .. }
                        if event_type == "execution.progress"
                )
            })
            .expect("execution progress message");

        assert_eq!(execution_message.execution_id.as_deref(), Some("exec-root"));
        assert_eq!(execution_message.task_id.as_deref(), Some("task-1"));
    }

    #[test]
    fn child_attention_messages_keep_child_execution_context() {
        let record = V3ProgressProjectionRecord {
            task: sample_task(),
            execution: Some(sample_execution("waiting_for_children", true)),
            children: vec![sample_child_execution()],
            attention: vec![V3AttentionSummary {
                attention_id: "attention:input_requested:task-1:exec-child-1".to_string(),
                attention_kind: "input.requested".to_string(),
                task_id: "task-1".to_string(),
                execution_id: "exec-child-1".to_string(),
                summary: "1 pending input request.".to_string(),
                detail: Some("Need clarification.".to_string()),
                entries: vec!["What should I do next?".to_string()],
                item_count: 1,
                updated_at: "2026-03-28T10:00:03Z".to_string(),
                pause_state_id: Some("pause-state-1".to_string()),
                input_type: Some("text".to_string()),
                hint: Some("Add more detail.".to_string()),
                options: None,
                input_schema: Some(
                    json!({"type": "text", "placeholder": "Say more", "multiline": false}),
                ),
                source: None,
            }],
            activities: Vec::new(),
            outputs: vec![V3OutputAvailableSummary {
                output_id: "out-child-1".to_string(),
                task_id: "task-1".to_string(),
                execution_id: Some("exec-child-1".to_string()),
                scope: "execution".to_string(),
                audience: "agent".to_string(),
                role: "result".to_string(),
                media_type: "application/json".to_string(),
                summary: Some("Child result ready.".to_string()),
                created_at: "2026-03-28T10:00:04Z".to_string(),
            }],
        };

        let messages = record.to_progress_messages();

        let attention_message = messages
            .iter()
            .find(|message| {
                matches!(
                    &message.kind,
                    ProgressMessageKind::AgentNotification { event_type, .. }
                        if event_type == "input.requested"
                )
            })
            .expect("attention message");
        assert_eq!(
            attention_message.execution_id.as_deref(),
            Some("exec-child-1")
        );
        assert_eq!(
            attention_message.parent_execution_id.as_deref(),
            Some("exec-root")
        );
        assert_eq!(
            attention_message.agent_id.as_deref(),
            Some("delegate-agent")
        );

        let output_message = messages
            .iter()
            .find(|message| {
                matches!(
                    &message.kind,
                    ProgressMessageKind::AgentNotification { event_type, .. }
                        if event_type == "output.available"
                )
            })
            .expect("output available message");
        assert_eq!(output_message.execution_id.as_deref(), Some("exec-child-1"));
        assert_eq!(
            output_message.parent_execution_id.as_deref(),
            Some("exec-root")
        );
        assert_eq!(output_message.agent_id.as_deref(), Some("delegate-agent"));
    }
}
