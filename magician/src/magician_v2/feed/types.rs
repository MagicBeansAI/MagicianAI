use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeedItemType {
    Task,
    Approval,
    AgentMessage,
    DataDelivery,
    RoutineResult,
    Escalation,
    LearningCandidate,
    LearningInsight,
    /// User-facing distilled knowledge the agent has accumulated —
    /// research findings, contacts learned, routines observed, user
    /// preferences detected, agent heuristics distilled. Surfaces on
    /// `/today` as a knowledge digest. Distinct from
    /// `LearningInsight` / `LearningCandidate` which are internal
    /// process telemetry (reflection completions, evaluation runs,
    /// memory-promotion candidates needing review) that surface on
    /// `/feed` for diagnostics.
    AgentLearning,
}

impl FeedItemType {
    pub fn as_db_str(&self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Approval => "approval",
            Self::AgentMessage => "agent_message",
            Self::DataDelivery => "data_delivery",
            Self::RoutineResult => "routine",
            Self::Escalation => "escalation",
            Self::LearningCandidate => "learning_candidate",
            Self::LearningInsight => "learning_insight",
            Self::AgentLearning => "agent_learning",
        }
    }

    pub fn from_db_str(value: &str) -> anyhow::Result<Self> {
        match value {
            "task" => Ok(Self::Task),
            "approval" => Ok(Self::Approval),
            "agent_message" => Ok(Self::AgentMessage),
            "data_delivery" => Ok(Self::DataDelivery),
            "routine" => Ok(Self::RoutineResult),
            "escalation" => Ok(Self::Escalation),
            "learning_candidate" => Ok(Self::LearningCandidate),
            "learning_insight" => Ok(Self::LearningInsight),
            "agent_learning" => Ok(Self::AgentLearning),
            other => anyhow::bail!("unknown feed item type: {other}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FeedItemStatus {
    Running,
    Done,
    NeedsAction,
    Failed,
    Info,
}

impl FeedItemStatus {
    pub fn as_db_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::NeedsAction => "needs_action",
            Self::Failed => "failed",
            Self::Info => "info",
        }
    }

    pub fn from_db_str(value: &str) -> anyhow::Result<Self> {
        match value {
            "running" => Ok(Self::Running),
            "done" => Ok(Self::Done),
            "needs_action" => Ok(Self::NeedsAction),
            "failed" => Ok(Self::Failed),
            "info" => Ok(Self::Info),
            other => anyhow::bail!("unknown feed item status: {other}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedAction {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_type: Option<String>,
    #[serde(default)]
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FeedItem {
    pub id: String,
    pub principal: String,
    pub workspace: String,
    pub item_type: FeedItemType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub status: FeedItemStatus,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<FeedAction>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct FeedItemPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<FeedItemStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_thread_id: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

impl FeedItemPatch {
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.summary.is_none()
            && self.status.is_none()
            && self.task_id.is_none()
            && self.ui_thread_id.is_none()
            && self.agent_id.is_none()
            && self.updated_at.is_none()
            && self.metadata.is_none()
    }

    pub fn between(previous: &FeedItem, current: &FeedItem) -> Self {
        let mut patch = Self::default();
        if previous.title != current.title {
            patch.title = Some(current.title.clone());
        }
        if previous.summary != current.summary {
            patch.summary = Some(current.summary.clone());
        }
        if previous.status != current.status {
            patch.status = Some(current.status.clone());
        }
        if previous.task_id != current.task_id {
            patch.task_id = Some(current.task_id.clone());
        }
        if previous.ui_thread_id != current.ui_thread_id {
            patch.ui_thread_id = Some(current.ui_thread_id.clone());
        }
        if previous.agent_id != current.agent_id {
            patch.agent_id = Some(current.agent_id.clone());
        }
        if previous.updated_at != current.updated_at {
            patch.updated_at = Some(current.updated_at);
        }
        if previous.metadata != current.metadata {
            patch.metadata = Some(current.metadata.clone());
        }
        patch
    }
}

use crate::magician_v2::attention_lane_facade::{AttentionLanePriorityRecord, AttentionLaneRecord};
use crate::magician_v2::today_projection_cache::TodayItem;

impl AttentionLaneRecord for TodayItem {
    fn attention_lane_record_id(&self) -> &str {
        &self.id
    }

    fn attention_lane_record_updated_at(&self) -> i64 {
        self.updated_at
    }
}
impl AttentionLanePriorityRecord for TodayItem {
    fn attention_lane_record_priority(&self) -> i64 {
        self.priority
    }
}
impl AttentionLaneRecord for FeedItem {
    fn attention_lane_record_id(&self) -> &str {
        &self.id
    }

    fn attention_lane_record_updated_at(&self) -> i64 {
        self.updated_at
    }
}
