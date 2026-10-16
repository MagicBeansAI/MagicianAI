use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::magician_v2::agents::condition::condition_allows;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ProgressSource {
    Execution,
    AgentLifecycle,
    Projection,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ProgressSeverity {
    Trace,
    Info,
    Warning,
    Error,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProgressMessageKind {
    StatusChanged {
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    ActionProgress {
        iteration: u32,
        action_type: String,
        target: String,
        success: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    ChildStatusChanged {
        child_execution_id: String,
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    HandedOver {
        from_agent: String,
        to_agent: String,
    },
    AgentNotification {
        event_type: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        entity_key: Option<String>,
        #[serde(default)]
        metadata: serde_json::Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProgressMessage {
    pub id: String,
    pub seq: u64,
    pub log_key: String,
    pub source: ProgressSource,
    /// Canonical event_type string identifying *what kind* of progress
    /// event this is. Surfaces (chat, whatsapp, telegram, push) look
    /// this up in `realtime_events::GAUI_EVENT_TAXONOMY` to get the
    /// render hint + coalesce key.
    ///
    /// For `AgentNotification` ProgressMessages, mirror the inner
    /// `event_type` (e.g. `approval.requested`, `agent.cycle.failed`).
    /// For `StatusChanged` / `ActionProgress` / `HandedOver` etc.,
    /// the emit site supplies a synthetic value from the
    /// `chat.pack.*` / `task.*` / `execution.*` namespace registered
    /// in `GAUI_EVENT_TAXONOMY`.
    ///
    /// `None` falls back to the legacy subscription-metadata routing
    /// in `ChatChannel::deliver` for backward compatibility during
    /// the migration; new emit sites should always set this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_type: Option<String>,
    /// Optional metadata bag set by the emitter for use by the
    /// coalesce-key resolver (e.g. `pause_state_id` for escalation
    /// events). Channel-agnostic.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub metadata: std::collections::HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routing_keys: Vec<String>,
    pub principal: String,
    pub workspace: String,
    pub severity: ProgressSeverity,
    pub kind: ProgressMessageKind,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct EventLocator {
    pub principal: String,
    pub workspace: String,
    pub log_key: String,
    pub seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionSource {
    Dynamic,
    Declarative { agent_id: String, rule_name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum SubscriptionFilter {
    TaskId(String),
    ExecutionId(String),
    AgentId(String),
    UiThreadId(String),
    RoutingKeyPrefix(String),
    RoutingKeyGlob(String),
    AgentLifecycleEvent {
        agent_id: String,
        event_pattern: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        condition: Option<String>,
    },
}

impl SubscriptionFilter {
    pub fn matches(&self, message: &ProgressMessage) -> bool {
        match self {
            Self::TaskId(task_id) => message.routing_keys.iter().any(|key| {
                key == &format!("task/{task_id}") || key.starts_with(&format!("task/{task_id}/"))
            }),
            Self::ExecutionId(execution_id) => {
                message.execution_id.as_deref() == Some(execution_id.as_str())
                    || message.root_execution_id.as_deref() == Some(execution_id.as_str())
                    || message.parent_execution_id.as_deref() == Some(execution_id.as_str())
            },
            Self::AgentId(agent_id) => message.agent_id.as_deref() == Some(agent_id.as_str()),
            Self::UiThreadId(thread_id) => {
                message.ui_thread_id.as_deref() == Some(thread_id.as_str())
            },
            Self::RoutingKeyPrefix(prefix) => message
                .routing_keys
                .iter()
                .any(|key| key == prefix || key.starts_with(&format!("{prefix}/"))),
            Self::RoutingKeyGlob(pattern) => message
                .routing_keys
                .iter()
                .any(|key| routing_key_glob_matches(pattern, key)),
            Self::AgentLifecycleEvent {
                agent_id,
                event_pattern,
                condition,
            } => match &message.kind {
                ProgressMessageKind::AgentNotification { event_type, .. } => {
                    message.agent_id.as_deref() == Some(agent_id.as_str())
                        && event_type.starts_with(event_pattern)
                        && condition_allows(condition.as_deref(), "progress_channels")
                },
                _ => false,
            },
        }
    }
}

fn routing_key_glob_matches(pattern: &str, key: &str) -> bool {
    let mut pattern_segments = pattern.split('/');
    let mut key_segments = key.split('/');

    loop {
        match (pattern_segments.next(), key_segments.next()) {
            (Some(pattern_segment), Some(key_segment)) => {
                if !segment_glob_matches(pattern_segment, key_segment) {
                    return false;
                }
            },
            (None, None) => return true,
            _ => return false,
        }
    }
}

fn segment_glob_matches(pattern: &str, candidate: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == candidate;
    }

    let pattern_chars = pattern.chars().collect::<Vec<_>>();
    let candidate_chars = candidate.chars().collect::<Vec<_>>();

    let mut pattern_index = 0usize;
    let mut candidate_index = 0usize;
    let mut star_index: Option<usize> = None;
    let mut candidate_backtrack = 0usize;

    while candidate_index < candidate_chars.len() {
        if pattern_index < pattern_chars.len()
            && pattern_chars[pattern_index] == candidate_chars[candidate_index]
        {
            pattern_index += 1;
            candidate_index += 1;
            continue;
        }

        if pattern_index < pattern_chars.len() && pattern_chars[pattern_index] == '*' {
            star_index = Some(pattern_index);
            pattern_index += 1;
            candidate_backtrack = candidate_index;
            continue;
        }

        if let Some(star) = star_index {
            pattern_index = star + 1;
            candidate_backtrack += 1;
            candidate_index = candidate_backtrack;
            continue;
        }

        return false;
    }

    while pattern_index < pattern_chars.len() && pattern_chars[pattern_index] == '*' {
        pattern_index += 1;
    }

    pattern_index == pattern_chars.len()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Subscription {
    pub id: String,
    pub channel_id: String,
    pub filter: SubscriptionFilter,
    pub principal: String,
    pub workspace: String,
    pub min_severity: ProgressSeverity,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
    pub source: SubscriptionSource,
    pub retention_secs: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_template: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_severity: Option<ProgressSeverity>,
    #[serde(default)]
    pub watermark: u64,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub pending_retry: BTreeSet<EventLocator>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionLineage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    pub principal: String,
    pub workspace: String,
    pub updated_at: i64,
}

/// Agent-lifecycle event types that are broadcast but never journaled.
///
/// A liveness ping is a statement about *now*. It carries no history, nothing
/// reads it back, and the next one supersedes it — so paying a durable append
/// per ping buys nothing. Left journaled, the media heartbeat grew the
/// `agent:__media__` shard to 123 MB, of which 86,148 records (70%, 91 MB)
/// were heartbeats, and made an idle voice session the single largest source
/// of fsyncs in the process.
///
/// Membership is a deliberate, narrow judgement, not a severity rule. Session
/// registration, disconnection and permission changes are real lifecycle facts
/// and stay on disk. Only add an event type here when losing every instance of
/// it on restart costs nothing.
pub const EPHEMERAL_PROGRESS_EVENT_TYPES: &[&str] =
    &[crate::magician_v2::media_seam::MEDIA_SESSION_HEARTBEAT];

impl ProgressMessage {
    pub fn is_terminal(&self) -> bool {
        match &self.kind {
            ProgressMessageKind::StatusChanged { status, .. } => {
                matches!(status.as_str(), "completed" | "failed" | "cancelled")
            },
            ProgressMessageKind::ChildStatusChanged { status, .. } => {
                matches!(status.as_str(), "completed" | "failed" | "cancelled")
            },
            _ => false,
        }
    }

    /// Whether this message is broadcast-only: delivered live to subscribers
    /// and to the progress stream, but never appended to its event shard.
    ///
    /// Because it is never journaled it is also not replayable, so the router
    /// must not record a pending retry for it — there would be nothing to load
    /// the locator back from.
    pub fn is_ephemeral(&self) -> bool {
        match &self.kind {
            ProgressMessageKind::AgentNotification { event_type, .. } => {
                EPHEMERAL_PROGRESS_EVENT_TYPES.contains(&event_type.as_str())
            },
            _ => false,
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::{
        ProgressMessage, ProgressMessageKind, ProgressSeverity, ProgressSource, SubscriptionFilter,
    };

    fn message_with_keys(keys: &[&str]) -> ProgressMessage {
        ProgressMessage {
            id: "msg-1".to_string(),
            seq: 1,
            log_key: "task:task-1".to_string(),
            source: ProgressSource::Execution,
            event_type: None,
            metadata: Default::default(),
            execution_id: Some("exec-1".to_string()),
            task_id: Some("task-1".to_string()),
            root_task_id: Some("task-1".to_string()),
            root_execution_id: Some("exec-1".to_string()),
            parent_execution_id: None,
            agent_id: Some("agent-a".to_string()),
            ui_thread_id: Some("thread-a".to_string()),
            step_id: None,
            routing_keys: keys.iter().map(|key| (*key).to_string()).collect(),
            principal: "default".to_string(),
            workspace: "default".to_string(),
            severity: ProgressSeverity::Info,
            kind: ProgressMessageKind::StatusChanged {
                status: "running".to_string(),
                summary: None,
            },
            timestamp: 0,
        }
    }

    #[test]
    fn routing_key_prefix_matches_descendants_only_on_path_boundaries() {
        let message = message_with_keys(&["name/name2/name3", "name/name2_extra"]);
        assert!(SubscriptionFilter::RoutingKeyPrefix("name/name2".to_string()).matches(&message));
        assert!(
            !SubscriptionFilter::RoutingKeyPrefix("name/name2_*".to_string()).matches(&message)
        );
    }

    #[test]
    fn routing_key_glob_matches_within_a_single_segment() {
        let message =
            message_with_keys(&["name/name2_alpha", "thread/travel/agent/web-researcher"]);
        assert!(SubscriptionFilter::RoutingKeyGlob("name/name2_*".to_string()).matches(&message));
        assert!(
            SubscriptionFilter::RoutingKeyGlob("thread/*/agent/web-researcher".to_string())
                .matches(&message)
        );
    }

    #[test]
    fn routing_key_glob_does_not_cross_path_boundaries() {
        let message = message_with_keys(&[
            "name/name2/alpha",
            "thread/travel/agent/web-researcher/detail",
        ]);
        assert!(!SubscriptionFilter::RoutingKeyGlob("name/name2_*".to_string()).matches(&message));
        assert!(
            !SubscriptionFilter::RoutingKeyGlob("thread/*/agent/web-researcher".to_string())
                .matches(&message)
        );
    }

    fn notification(event_type: &str) -> ProgressMessage {
        let mut message = message_with_keys(&[]);
        message.kind = ProgressMessageKind::AgentNotification {
            event_type: event_type.to_string(),
            message: "event".to_string(),
            entity_key: None,
            metadata: serde_json::Value::Null,
        };
        message
    }

    #[test]
    fn only_listed_notification_event_types_are_ephemeral() {
        assert!(
            notification(crate::magician_v2::media_seam::MEDIA_SESSION_HEARTBEAT).is_ephemeral()
        );
        assert!(
            !notification(crate::magician_v2::media_seam::MEDIA_SESSION_REGISTERED).is_ephemeral()
        );
        assert!(!notification("media.session.disconnected").is_ephemeral());
    }

    #[test]
    fn a_status_change_is_never_ephemeral() {
        // Ephemerality is a per-event-type judgement about notifications, not
        // a severity or kind rule. A lifecycle transition is always journaled.
        assert!(!message_with_keys(&[]).is_ephemeral());
    }
}
