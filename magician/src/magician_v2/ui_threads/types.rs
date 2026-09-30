use serde::{Deserialize, Serialize};

use crate::magician_v2::history::HistoryLane;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UiThreadRecord {
    pub principal: String,
    pub workspace: String,
    pub id: String,
    pub name: String,
    pub archived: bool,
    pub sort_order: i64,
    pub memory_summary: Option<String>,
    pub memory_updated_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    /// Separates user-created history from product-generated activity.
    #[serde(default = "default_history_lane")]
    pub history_lane: HistoryLane,
    /// UI display layout for this thread. `"chat"` (default) renders
    /// the conversation-first chat surface; `"dev"` renders the
    /// Developer Mode workbench (xterm.js terminal pane, diff strip,
    /// plan-mode review) over the same underlying chat data. See
    /// `docs/plans/2026-05-13-developer-mode-workbench.md`.
    #[serde(default = "default_display_mode")]
    pub display_mode: String,
    /// Developer Mode "plan-mode gate" — when `true`, the autonomous
    /// executor pauses for human approval before each non-read-only
    /// action via the existing escalation envelope. Off by default;
    /// flipped per-thread by the user in Developer Mode. See
    /// docs/plans/2026-05-13-developer-mode-workbench.md Phase 5.
    #[serde(default)]
    pub plan_mode: bool,
}

fn default_display_mode() -> String {
    "chat".to_string()
}

fn default_history_lane() -> HistoryLane {
    HistoryLane::Personal
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UiThreadDetail {
    #[serde(flatten)]
    pub record: UiThreadRecord,
    pub memory_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiThreadPage {
    pub threads: Vec<UiThreadRecord>,
    pub total: usize,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiThreadSearchCandidate {
    pub id: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default)]
pub struct UiThreadUpdate {
    pub name: Option<String>,
    pub archived: Option<bool>,
    pub sort_order: Option<i64>,
    pub memory_summary: Option<Option<String>>,
    pub memory_updated_at: Option<Option<i64>>,
    /// Update the display mode. Only "chat" and "dev" are accepted by
    /// the service layer (`UiThreadService::update_thread`).
    pub display_mode: Option<String>,
    /// Toggle Developer-Mode plan-mode gate (Phase 5).
    pub plan_mode: Option<bool>,
    pub history_lane: Option<HistoryLane>,
}
