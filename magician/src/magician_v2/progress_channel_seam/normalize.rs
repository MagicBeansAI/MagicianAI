use serde_json::Value;
use tracing::warn;
use uuid::Uuid;

use crate::magician_v2::{
    agents::NotificationSeverity,
    artifact_v2::{models::TaskRecord, ArtifactV2Service, ScopeRef, V3ReadApi},
    orchestrator::v2_orchestrator::MagicianV2Orchestrator,
    realtime_events::{AgentEventEnvelope, RuntimeAgentEventType, RuntimeTransportEvent},
};

use crate::magician_v2::progress_channel_seam::{
    lineage::ExecutionLineageIndex,
    types::{ProgressMessage, ProgressMessageKind, ProgressSeverity, ProgressSource, Subscription},
};

fn execution_status_summary(new_status: &str) -> (String, Option<String>, ProgressSeverity) {
    match new_status {
        "Executing" | "Runnable" => (
            "running".to_string(),
            Some("Execution started".to_string()),
            ProgressSeverity::Info,
        ),
        "Paused" => (
            "paused".to_string(),
            Some("Execution paused".to_string()),
            ProgressSeverity::Warning,
        ),
        "WaitingUser" => (
            "paused".to_string(),
            Some("Waiting for user input".to_string()),
            ProgressSeverity::Warning,
        ),
        "WaitingChildren" => (
            "paused".to_string(),
            Some("Waiting on delegated child work".to_string()),
            ProgressSeverity::Warning,
        ),
        "Completed" => ("completed".to_string(), None, ProgressSeverity::Info),
        "Failed" => ("failed".to_string(), None, ProgressSeverity::Error),
        "Cancelled" => (
            "cancelled".to_string(),
            Some("Execution cancelled".to_string()),
            ProgressSeverity::Error,
        ),
        other => (
            "running".to_string(),
            Some(format!("Execution state changed: {other}")),
            ProgressSeverity::Info,
        ),
    }
}

fn child_status_summary(new_status: &str) -> Option<(String, Option<String>, ProgressSeverity)> {
    match new_status {
        "Executing" | "Runnable" => Some((
            "running".to_string(),
            Some("Delegated agent started working".to_string()),
            ProgressSeverity::Info,
        )),
        "Completed" => Some((
            "completed".to_string(),
            Some("Delegated agent finished".to_string()),
            ProgressSeverity::Info,
        )),
        "Failed" => Some((
            "failed".to_string(),
            Some("Delegated agent failed".to_string()),
            ProgressSeverity::Error,
        )),
        "Cancelled" => Some((
            "cancelled".to_string(),
            Some("Delegated agent cancelled".to_string()),
            ProgressSeverity::Error,
        )),
        _ => None,
    }
}

fn truncate_text(value: &str, max_chars: usize) -> String {
    let mut truncated = value.chars().take(max_chars).collect::<String>();
    if value.chars().count() > max_chars {
        truncated.push_str("...");
    }
    truncated
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

#[allow(clippy::too_many_arguments)]
fn base_message(
    source: ProgressSource,
    principal: String,
    workspace: String,
    execution_id: Option<String>,
    task_id: Option<String>,
    root_task_id: Option<String>,
    root_execution_id: Option<String>,
    parent_execution_id: Option<String>,
    agent_id: Option<String>,
    ui_thread_id: Option<String>,
    step_id: Option<String>,
    severity: ProgressSeverity,
    kind: ProgressMessageKind,
    timestamp: i64,
) -> ProgressMessage {
    // event_type is derived from `kind` below — keeps callers from
    // having to pass it explicitly while still tagging every normalized
    // ProgressMessage with the canonical taxonomy key for downstream
    // surface routing.
    let event_type = canonical_event_type_from_kind(&kind);
    let mut message = ProgressMessage {
        id: Uuid::new_v4().to_string(),
        seq: 0,
        log_key: String::new(),
        source,
        event_type,
        metadata: Default::default(),
        execution_id,
        task_id,
        root_task_id,
        root_execution_id,
        parent_execution_id,
        agent_id,
        ui_thread_id,
        step_id,
        routing_keys: Vec::new(),
        principal,
        workspace,
        severity,
        kind,
        timestamp,
    };
    message.routing_keys = derive_routing_keys(&message);
    message.log_key = default_log_key(&message);
    message
}

/// Derive the canonical `event_type` for a `ProgressMessageKind` so
/// per-surface routing can look up its render hint in
/// `realtime_events::GAUI_EVENT_TAXONOMY`.
///
/// For `AgentNotification`, mirror the inner `event_type`. For other
/// variants, return the synthetic taxonomy key registered for that
/// kind in the master table (`task.status_changed`,
/// `task.action_progress`, `task.child.status_changed`,
/// `execution.handed_over`). Returns `None` for variants without a
/// canonical taxonomy row; surface routing treats those as "drop
/// silently" the same as unknown event_types.
fn canonical_event_type_from_kind(kind: &ProgressMessageKind) -> Option<String> {
    Some(match kind {
        ProgressMessageKind::AgentNotification { event_type, .. } => event_type.clone(),
        ProgressMessageKind::StatusChanged { .. } => RuntimeAgentEventType::TaskStatusChanged
            .as_str()
            .to_string(),
        ProgressMessageKind::ChildStatusChanged { .. } => {
            RuntimeAgentEventType::TaskChildStatusChanged
                .as_str()
                .to_string()
        },
        ProgressMessageKind::ActionProgress { .. } => RuntimeAgentEventType::TaskActionProgress
            .as_str()
            .to_string(),
        ProgressMessageKind::HandedOver { .. } => RuntimeAgentEventType::ExecutionHandedOver
            .as_str()
            .to_string(),
    })
}

async fn load_task_record(
    v3_service: &std::sync::Arc<ArtifactV2Service>,
    orchestrator: &std::sync::Arc<MagicianV2Orchestrator>,
    task_id: &str,
    execution_id: Option<&str>,
) -> Option<(ScopeRef, TaskRecord)> {
    if let Some(execution_id) = execution_id {
        if let Ok(execution) = orchestrator.get_execution(execution_id).await {
            if execution.task_id.as_deref() == Some(task_id) {
                let scope = ScopeRef::system_internal_unauthenticated(
                    &execution.principal,
                    &execution.workspace,
                );
                if let Ok(task) = v3_service.get_task(&scope, task_id).await {
                    return Some((scope, task));
                }
            }
        }
    }

    v3_service.get_task_by_id(task_id).await.ok().flatten()
}

pub async fn normalize_execution_event(
    event: &RuntimeTransportEvent,
    lineage_index: &ExecutionLineageIndex,
    v3_service: &std::sync::Arc<ArtifactV2Service>,
    orchestrator: &std::sync::Arc<MagicianV2Orchestrator>,
) -> Option<ProgressMessage> {
    // App owner notifications are not execution progress. Projecting their
    // prompt would append a second durable body to the progress EventLog and
    // could fan it out to arbitrary progress channels beyond the sealed TTL.
    // Their dedicated resolution source is suppressed for the same ownership
    // reason; UserRequest/Attention is the only sanctioned surface.
    if crate::magician_v2::realtime_events::is_app_owner_notification_transport_event(event) {
        return None;
    }
    match event {
        RuntimeTransportEvent::ExecutionStatusChanged {
            execution_id,
            task_id,
            root_execution_id,
            new_status,
            timestamp,
            ..
        } => {
            let lineage = if task_id.is_some() {
                let (scope, task) = load_task_record(
                    v3_service,
                    orchestrator,
                    task_id.as_deref()?,
                    Some(execution_id),
                )
                .await?;
                let existing = lineage_index.get(execution_id).await;
                let execution = orchestrator.get_execution(execution_id).await.ok();
                let lineage = super::types::ExecutionLineage {
                    task_id: task_id.clone(),
                    root_execution_id: root_execution_id
                        .clone()
                        .or_else(|| Some(execution_id.clone())),
                    parent_execution_id: execution
                        .as_ref()
                        .and_then(|execution| execution.parent_execution_id.clone()),
                    agent_id: execution.map(|execution| execution.active_owner_agent_id),
                    ui_thread_id: Some(task.manifest.ui_thread_id.clone()),
                    step_id: existing.and_then(|lineage| lineage.step_id),
                    principal: scope.principal().to_string(),
                    workspace: scope.workspace().to_string(),
                    updated_at: chrono::Utc::now().timestamp_millis(),
                };
                let _ = lineage_index
                    .upsert(execution_id.clone(), lineage.clone())
                    .await;
                lineage
            } else {
                lineage_index
                    .resolve_or_load(execution_id, orchestrator, v3_service)
                    .await?
            };

            if task_id.is_some() {
                let (status, summary, severity) = execution_status_summary(new_status);
                Some(base_message(
                    ProgressSource::Execution,
                    lineage.principal,
                    lineage.workspace,
                    Some(execution_id.clone()),
                    task_id.clone(),
                    lineage.task_id.clone(),
                    lineage.root_execution_id.clone(),
                    lineage.parent_execution_id.clone(),
                    lineage.agent_id.clone(),
                    lineage.ui_thread_id.clone(),
                    lineage.step_id.clone(),
                    severity,
                    ProgressMessageKind::StatusChanged { status, summary },
                    *timestamp,
                ))
            } else {
                let (status, summary, severity) = child_status_summary(new_status)?;
                Some(base_message(
                    ProgressSource::Execution,
                    lineage.principal,
                    lineage.workspace,
                    Some(execution_id.clone()),
                    lineage.task_id.clone(),
                    lineage.task_id.clone(),
                    lineage.root_execution_id.clone(),
                    lineage.parent_execution_id.clone(),
                    lineage.agent_id.clone(),
                    lineage.ui_thread_id.clone(),
                    lineage.step_id.clone(),
                    severity,
                    ProgressMessageKind::ChildStatusChanged {
                        child_execution_id: execution_id.clone(),
                        status,
                        summary,
                    },
                    *timestamp,
                ))
            }
        },
        RuntimeTransportEvent::ExecutionResponsibilityChanged {
            execution_id,
            task_id,
            root_execution_id,
            parent_execution_id,
            active_owner_agent_id,
            owner_stack,
            timestamp,
            ..
        } => {
            let existing = lineage_index
                .resolve_or_load(execution_id, orchestrator, v3_service)
                .await;
            let previous_owner = owner_stack.last().cloned().or_else(|| {
                existing
                    .as_ref()
                    .and_then(|lineage| lineage.agent_id.clone())
            })?;
            if previous_owner == *active_owner_agent_id {
                return None;
            }

            let task: Option<TaskRecord> = match task_id.as_ref() {
                Some(task_id) => {
                    load_task_record(v3_service, orchestrator, task_id, Some(execution_id))
                        .await
                        .map(|(_, task)| task)
                },
                None => None,
            };
            let lineage = super::types::ExecutionLineage {
                task_id: task_id.clone().or_else(|| {
                    existing
                        .as_ref()
                        .and_then(|lineage| lineage.task_id.clone())
                }),
                root_execution_id: root_execution_id
                    .clone()
                    .or_else(|| {
                        existing
                            .as_ref()
                            .and_then(|lineage| lineage.root_execution_id.clone())
                    })
                    .or_else(|| Some(execution_id.clone())),
                parent_execution_id: parent_execution_id.clone().or_else(|| {
                    existing
                        .as_ref()
                        .and_then(|lineage| lineage.parent_execution_id.clone())
                }),
                agent_id: Some(active_owner_agent_id.clone()),
                ui_thread_id: task
                    .as_ref()
                    .map(|task| task.manifest.ui_thread_id.clone())
                    .or_else(|| {
                        existing
                            .as_ref()
                            .and_then(|lineage| lineage.ui_thread_id.clone())
                    }),
                step_id: existing
                    .as_ref()
                    .and_then(|lineage| lineage.step_id.clone()),
                principal: task
                    .as_ref()
                    .map(|task| task.manifest.principal.clone())
                    .or_else(|| existing.as_ref().map(|lineage| lineage.principal.clone()))?,
                workspace: task
                    .as_ref()
                    .map(|task| task.manifest.workspace.clone())
                    .or_else(|| existing.as_ref().map(|lineage| lineage.workspace.clone()))?,
                updated_at: chrono::Utc::now().timestamp_millis(),
            };
            let _ = lineage_index
                .upsert(execution_id.clone(), lineage.clone())
                .await;

            Some(base_message(
                ProgressSource::Execution,
                lineage.principal,
                lineage.workspace,
                Some(execution_id.clone()),
                lineage.task_id.clone(),
                lineage.task_id.clone(),
                lineage.root_execution_id.clone(),
                lineage.parent_execution_id.clone(),
                lineage.agent_id.clone(),
                lineage.ui_thread_id.clone(),
                lineage.step_id.clone(),
                ProgressSeverity::Info,
                ProgressMessageKind::HandedOver {
                    from_agent: previous_owner,
                    to_agent: active_owner_agent_id.clone(),
                },
                *timestamp,
            ))
        },
        RuntimeTransportEvent::AgenticActionExecuted {
            execution_id,
            iteration,
            action_type,
            target,
            success,
            error,
            timestamp,
            ..
        } => {
            let lineage = lineage_index
                .resolve_or_load(execution_id, orchestrator, v3_service)
                .await?;
            Some(base_message(
                ProgressSource::Execution,
                lineage.principal,
                lineage.workspace,
                Some(execution_id.clone()),
                lineage.task_id.clone(),
                lineage.task_id.clone(),
                lineage.root_execution_id.clone(),
                lineage.parent_execution_id.clone(),
                lineage.agent_id.clone(),
                lineage.ui_thread_id.clone(),
                lineage.step_id.clone(),
                ProgressSeverity::Info,
                ProgressMessageKind::ActionProgress {
                    iteration: *iteration as u32,
                    action_type: action_type.trim().to_lowercase(),
                    target: truncate_text(target.trim(), 90),
                    success: *success,
                    error: error
                        .as_ref()
                        .map(|value| truncate_text(value.trim(), 80))
                        .filter(|value| !value.is_empty()),
                },
                *timestamp,
            ))
        },
        // Canonical HITL — rebuild the chat/feed AgentNotification rows
        // that used to come from the retired legacy emits (H6.1–H6.4).
        // Render text depends on `source`; `attention_kind` is mapped to
        // the legacy taxonomy strings so existing surface-routing rules
        // and the `feed_api` Requests count badge keep working.
        RuntimeTransportEvent::HitlRequested {
            correlation_id,
            source,
            prompt,
            execution_id,
            task_id,
            agent_id,
            principal,
            workspace,
            timestamp,
            ..
        } => {
            // Canonical: every HITL request, regardless of `source`, surfaces
            // as event_type=`hitl.requested` with `source` carried in metadata
            // for downstream consumers to disambiguate render shape. Replaces
            // the prior per-source projection (`approval.requested`,
            // `user_request.pending`, etc.) — one canonical taxonomy row per
            // canonical lifecycle event, source becomes a payload field.
            //
            // `attention_kind` is a downstream filter discriminator (NOT a
            // taxonomy event_type) — `feed_api.rs::requests` badge count,
            // `artifact_v2/service.rs::build_attention_summaries`, and
            // attention-bucket logic key on these specific strings. The
            // per-source mapping predates this refactor and stays —
            // bumping the event_type to canonical doesn't change the
            // attention-bucket vocabulary.
            let (message_text, severity, attention_kind) = match source.as_str() {
                "approval" => (
                    "Approval requested before the agent can continue.".to_string(),
                    ProgressSeverity::Warning,
                    "approval.requested",
                ),
                "user_request" => (
                    if prompt.trim().is_empty() {
                        "User input required.".to_string()
                    } else {
                        prompt.clone()
                    },
                    ProgressSeverity::Warning,
                    "user_request.pending",
                ),
                "agentic" => (
                    if prompt.trim().is_empty() {
                        "Agent is waiting for input.".to_string()
                    } else {
                        prompt.clone()
                    },
                    ProgressSeverity::Warning,
                    "input.requested",
                ),
                _ => (
                    if prompt.trim().is_empty() {
                        "Human input required.".to_string()
                    } else {
                        prompt.clone()
                    },
                    ProgressSeverity::Info,
                    "hitl.requested",
                ),
            };
            let event_type_str = "hitl.requested";
            let lineage = match execution_id.as_deref() {
                Some(execution_id) => {
                    lineage_index
                        .resolve_or_load(execution_id, orchestrator, v3_service)
                        .await
                },
                None => None,
            };
            let resolved_principal = principal
                .clone()
                .or_else(|| lineage.as_ref().map(|lineage| lineage.principal.clone()))?;
            let resolved_workspace = workspace
                .clone()
                .or_else(|| lineage.as_ref().map(|lineage| lineage.workspace.clone()))?;
            let resolved_task_id = task_id
                .clone()
                .or_else(|| lineage.as_ref().and_then(|lineage| lineage.task_id.clone()));
            let resolved_agent_id = agent_id.clone().or_else(|| {
                lineage
                    .as_ref()
                    .and_then(|lineage| lineage.agent_id.clone())
            });
            Some(base_message(
                ProgressSource::Execution,
                resolved_principal,
                resolved_workspace,
                execution_id.clone(),
                resolved_task_id.clone(),
                resolved_task_id,
                lineage
                    .as_ref()
                    .and_then(|lineage| lineage.root_execution_id.clone()),
                lineage
                    .as_ref()
                    .and_then(|lineage| lineage.parent_execution_id.clone()),
                resolved_agent_id,
                lineage
                    .as_ref()
                    .and_then(|lineage| lineage.ui_thread_id.clone()),
                lineage.as_ref().and_then(|lineage| lineage.step_id.clone()),
                severity,
                ProgressMessageKind::AgentNotification {
                    event_type: event_type_str.to_string(),
                    message: message_text,
                    entity_key: Some(format!("{source}:{correlation_id}")),
                    // `source` is the new discriminator consumers read
                    // to pick per-source rendering (Approval feed item
                    // vs Escalation, etc.). `attention_kind` retains
                    // its pre-refactor per-source vocabulary so the
                    // `feed_api.rs::requests` badge filter and
                    // attention-bar consumers keep matching.
                    metadata: serde_json::json!({
                        "attention_kind": attention_kind,
                        "source": source,
                        "correlation_id": correlation_id,
                    }),
                },
                *timestamp,
            ))
        },
        RuntimeTransportEvent::HitlResolved {
            correlation_id,
            source,
            outcome,
            decision,
            execution_id,
            task_id,
            agent_id,
            principal,
            workspace,
            timestamp,
        } => {
            // Canonical: every HITL resolution surfaces as
            // event_type=`hitl.resolved` with `source` and `outcome`
            // (`approve` / `reject` / `expired` / …) carried in
            // metadata. Replaces the prior per-source projection
            // (`approval.resolved` / `approval.expired` /
            // `user_request.resolved`) — outcome is now a payload
            // field, not an event_type discriminator.
            let event_type_str = "hitl.resolved";
            let message_text = match source.as_str() {
                "approval" => match decision.as_deref() {
                    Some("approve") | Some("approved") => {
                        "Approval granted. The agent can continue.".to_string()
                    },
                    Some("reject") | Some("rejected") => {
                        "Approval rejected. The agent cannot continue.".to_string()
                    },
                    _ if outcome == "expired" => {
                        "Approval request expired before a decision was made.".to_string()
                    },
                    _ => "Approval resolved.".to_string(),
                },
                "user_request" => format!("User request resolved ({outcome})."),
                _ => format!("Human input received ({outcome})."),
            };
            let lineage = match execution_id.as_deref() {
                Some(execution_id) => {
                    lineage_index
                        .resolve_or_load(execution_id, orchestrator, v3_service)
                        .await
                },
                None => None,
            };
            let resolved_principal = principal
                .clone()
                .or_else(|| lineage.as_ref().map(|lineage| lineage.principal.clone()))?;
            let resolved_workspace = workspace
                .clone()
                .or_else(|| lineage.as_ref().map(|lineage| lineage.workspace.clone()))?;
            let resolved_task_id = task_id
                .clone()
                .or_else(|| lineage.as_ref().and_then(|lineage| lineage.task_id.clone()));
            let resolved_agent_id = agent_id.clone().or_else(|| {
                lineage
                    .as_ref()
                    .and_then(|lineage| lineage.agent_id.clone())
            });
            Some(base_message(
                ProgressSource::Execution,
                resolved_principal,
                resolved_workspace,
                execution_id.clone(),
                resolved_task_id.clone(),
                resolved_task_id,
                lineage
                    .as_ref()
                    .and_then(|lineage| lineage.root_execution_id.clone()),
                lineage
                    .as_ref()
                    .and_then(|lineage| lineage.parent_execution_id.clone()),
                resolved_agent_id,
                lineage
                    .as_ref()
                    .and_then(|lineage| lineage.ui_thread_id.clone()),
                lineage.as_ref().and_then(|lineage| lineage.step_id.clone()),
                ProgressSeverity::Info,
                ProgressMessageKind::AgentNotification {
                    event_type: event_type_str.to_string(),
                    message: message_text,
                    entity_key: Some(format!("{source}:{correlation_id}")),
                    metadata: serde_json::json!({
                        "source": source,
                        "correlation_id": correlation_id,
                        "outcome": outcome,
                        "decision": decision,
                    }),
                },
                *timestamp,
            ))
        },
        _ => None,
    }
}

fn default_agent_event_severity(event_type: &str) -> ProgressSeverity {
    match RuntimeAgentEventType::from_str(event_type) {
        Some(RuntimeAgentEventType::AgentCircuitOpened) => ProgressSeverity::Critical,
        Some(RuntimeAgentEventType::AgentCycleFailed | RuntimeAgentEventType::AgentGoalFailed) => {
            ProgressSeverity::Error
        },
        _ => {
            // Canonical `hitl.requested` is a Warning (every source
            // pauses the run waiting on a human). Approval, user_request,
            // clarification all collapse into this single canonical row
            // now — source disambiguation lives in the event payload.
            if event_type == "hitl.requested" {
                ProgressSeverity::Warning
            } else {
                ProgressSeverity::Info
            }
        },
    }
}

/// Returns the human-readable message string stored on every
/// `AgentNotification` ProgressMessage's `message` field.
///
/// This function is **only** responsible for rendering text — it does
/// NOT decide whether the event is shown on any particular surface.
/// Visibility is owned by `progress_channel_seam::surface_routing`, which
/// derives per-surface predicates from the master event taxonomy in
/// `realtime_events::GAUI_EVENT_TAXONOMY`. User-facing surfaces that
/// pass the routing predicate render this text; surfaces that don't
/// (e.g. the chat timeline for `tool.call.*`) drop the event before
/// reading the message field at all.
///
/// The fallback `"Agent event: <type>"` is a debug courtesy for
/// internal consumers (e.g. `events.jsonl` rows, the Internals drawer
/// raw stream); user-facing surfaces should never reach it because
/// their routing predicates filter the event out first.
fn default_agent_event_message(event_type: &str, payload: &Value) -> String {
    // Canonical `hitl.requested` / `hitl.resolved` paths read `source`
    // and `outcome` from the payload to pick per-source human-readable
    // text. Replaces the per-source projection rows that used to keep
    // text tied to event_type strings.
    if event_type == "hitl.requested" {
        let source = payload.get("source").and_then(|v| v.as_str()).unwrap_or("");
        return match source {
            "approval" => "Approval requested before the agent can continue.".to_string(),
            "user_request" => payload
                .get("prompt")
                .and_then(|v| v.as_str())
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| "User input required.".to_string()),
            _ => "Human input required.".to_string(),
        };
    }
    if event_type == "hitl.resolved" {
        let source = payload.get("source").and_then(|v| v.as_str()).unwrap_or("");
        let outcome = payload
            .get("outcome")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let decision = payload.get("decision").and_then(|v| v.as_str());
        return match (source, decision, outcome) {
            ("approval", Some("approve" | "approved"), _) => {
                "Approval granted. The agent can continue.".to_string()
            },
            ("approval", Some("reject" | "rejected"), _) => {
                "Approval rejected. The agent cannot continue.".to_string()
            },
            ("approval", _, "expired") => {
                "Approval request expired before a decision was made.".to_string()
            },
            ("approval", _, _) => "Approval resolved.".to_string(),
            ("user_request", _, _) => format!("User request resolved ({outcome})."),
            _ => format!("Human input received ({outcome})."),
        };
    }
    match RuntimeAgentEventType::from_str(event_type) {
        Some(RuntimeAgentEventType::AgentCircuitOpened) => {
            "Agent circuit breaker opened after repeated failures.".to_string()
        },
        Some(RuntimeAgentEventType::AgentCycleFailed | RuntimeAgentEventType::AgentGoalFailed) => {
            "Agent cycle failed.".to_string()
        },
        _ => format!("Agent event: {event_type}"),
    }
}

pub fn map_notification_severity(value: NotificationSeverity) -> ProgressSeverity {
    match value {
        NotificationSeverity::Low => ProgressSeverity::Info,
        NotificationSeverity::Medium => ProgressSeverity::Warning,
        NotificationSeverity::High => ProgressSeverity::Critical,
    }
}

pub fn render_template(template: &str, payload: &Value) -> String {
    if let Some(object) = payload.as_object() {
        let mut result = String::with_capacity(template.len());
        let mut rest = template;
        while let Some(start) = rest.find('{') {
            result.push_str(&rest[..start]);
            if let Some(end) = rest[start..].find('}') {
                let key = &rest[start + 1..start + end];
                if let Some(value) = object.get(key) {
                    match value {
                        Value::String(value) => result.push_str(value),
                        other => result.push_str(&other.to_string()),
                    }
                } else {
                    result.push_str(&rest[start..start + end + 1]);
                }
                rest = &rest[start + end + 1..];
            } else {
                result.push_str(&rest[start..]);
                rest = "";
            }
        }
        result.push_str(rest);
        result
    } else {
        template.to_string()
    }
}

pub fn apply_subscription_overrides(
    subscription: &Subscription,
    mut message: ProgressMessage,
) -> ProgressMessage {
    if let Some(output_severity) = subscription.output_severity {
        message.severity = output_severity;
    }

    if let (
        Some(template),
        ProgressMessageKind::AgentNotification {
            message: rendered,
            metadata,
            ..
        },
    ) = (subscription.message_template.as_ref(), &mut message.kind)
    {
        *rendered = render_template(template, metadata);
    }

    message
}

pub async fn normalize_agent_event(
    envelope: &AgentEventEnvelope,
    lineage_index: &ExecutionLineageIndex,
    v3_service: &std::sync::Arc<ArtifactV2Service>,
    orchestrator: &std::sync::Arc<MagicianV2Orchestrator>,
) -> Option<ProgressMessage> {
    // MUIJ/UI delta envelopes already have their own websocket delivery path and should
    // not be re-materialized as chat/feed/system progress notifications.
    if envelope.event_type.starts_with("agent.ui.") {
        return None;
    }

    // Engine and model residency belong to the process, not to any user
    // scope. They remain available on the realtime transport and through the
    // media status API, but must not be materialized into a user's progress
    // feed or produce an unscoped-event warning.
    if is_global_media_audio_lifecycle_event(&envelope.event_type) {
        return None;
    }

    if envelope.event_type == "agent.execution.mapping" {
        let execution_id = envelope.payload.get("execution_id")?.as_str()?;
        if let Some(mut lineage) = lineage_index
            .resolve_or_load(execution_id, orchestrator, v3_service)
            .await
        {
            lineage.agent_id = Some(envelope.agent_id.clone());
            let _ = lineage_index
                .upsert(execution_id.to_string(), lineage)
                .await;
        }
        return None;
    }

    let execution_id = envelope
        .payload
        .get("execution_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);

    let lineage = match execution_id.as_deref() {
        Some(execution_id) => {
            lineage_index
                .resolve_or_load(execution_id, orchestrator, v3_service)
                .await
        },
        None => None,
    };

    let principal = lineage
        .as_ref()
        .map(|lineage| lineage.principal.clone())
        .or_else(|| envelope.principal.clone())
        .or_else(|| {
            envelope
                .payload
                .get("principal")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let workspace = lineage
        .as_ref()
        .map(|lineage| lineage.workspace.clone())
        .or_else(|| envelope.workspace.clone())
        .or_else(|| {
            envelope
                .payload
                .get("workspace")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let (principal, workspace) = match (principal, workspace) {
        (Some(principal), Some(workspace)) => (principal, workspace),
        _ => {
            warn!(
                event_type = %envelope.event_type,
                agent_id = %envelope.agent_id,
                "dropping unscoped agent event because neither lineage nor explicit scope is available"
            );
            return None;
        },
    };

    Some(base_message(
        ProgressSource::AgentLifecycle,
        principal,
        workspace,
        execution_id,
        lineage.as_ref().and_then(|lineage| lineage.task_id.clone()),
        lineage.as_ref().and_then(|lineage| lineage.task_id.clone()),
        lineage
            .as_ref()
            .and_then(|lineage| lineage.root_execution_id.clone()),
        lineage
            .as_ref()
            .and_then(|lineage| lineage.parent_execution_id.clone()),
        Some(envelope.agent_id.clone()),
        lineage
            .as_ref()
            .and_then(|lineage| lineage.ui_thread_id.clone())
            .or_else(|| {
                envelope
                    .payload
                    .get("ui_thread_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            }),
        lineage.as_ref().and_then(|lineage| lineage.step_id.clone()),
        default_agent_event_severity(&envelope.event_type),
        ProgressMessageKind::AgentNotification {
            event_type: envelope.event_type.clone(),
            message: envelope
                .payload
                .get("message")
                .and_then(|value| value.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    default_agent_event_message(&envelope.event_type, &envelope.payload)
                }),
            entity_key: envelope
                .payload
                .get("entity_key")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            metadata: envelope.payload.clone(),
        },
        envelope.timestamp,
    ))
}

pub fn is_global_media_audio_lifecycle_event(event_type: &str) -> bool {
    event_type.starts_with("media.audio.engine.") || event_type.starts_with("media.audio.model.")
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::is_global_media_audio_lifecycle_event;

    #[test]
    fn only_process_global_audio_lifecycle_events_skip_progress_materialization() {
        assert!(is_global_media_audio_lifecycle_event(
            "media.audio.engine.started"
        ));
        assert!(is_global_media_audio_lifecycle_event(
            "media.audio.model.loaded"
        ));
        assert!(!is_global_media_audio_lifecycle_event(
            "media.audio.profile.degraded"
        ));
        assert!(!is_global_media_audio_lifecycle_event(
            "media.audio.vad.speech_started"
        ));
    }
}
