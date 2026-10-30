//! The latest chat turn's status per session, for clients that reconnect.
//!
//! A turn whose client went away (`continue_on_disconnect`) keeps running. A
//! page or app that comes back reads this state to show the turn as it is:
//! running, completed, stopped, or failed. The state lives in memory, because
//! a running turn exists only in this process. Every change is also emitted
//! as [`CHAT_TURN_STATE_EVENT`]. That event carries the turn's
//! `chat_turn_id`, so the per-turn event file keeps the final state after a
//! restart.

use serde::{Deserialize, Serialize};

/// Named agent event for every turn state change. The payload is a
/// [`ChatTurnState`] plus `chat_session_id`.
pub const CHAT_TURN_STATE_EVENT: &str = "chat.turn.state_changed";

/// `reason` of a turn the owner stopped (`DELETE /chat/sessions/{id}/run`).
pub const USER_STOP_REASON: &str = "user_stop";
/// `reason` of a failed turn whose error ended it.
pub const ERROR_REASON: &str = "error";
/// `reason` of a turn whose future was dropped before it settled.
pub const INTERRUPTED_REASON: &str = "interrupted";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatTurnStatus {
    Running,
    Completed,
    Stopped,
    Failed,
}

/// How a turn's future ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatTurnOutcome {
    /// It returned a reply.
    Answered,
    /// It returned the cancellation sentinel (Stop or the watchdog).
    Cancelled,
    /// It returned an error.
    Failed,
    /// It was dropped before it returned.
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatTurnState {
    pub chat_turn_id: String,
    pub status: ChatTurnStatus,
    /// Unix ms.
    pub started_at: i64,
    /// Unix ms; set once the turn is no longer running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<i64>,
    /// Why a turn stopped or failed: [`USER_STOP_REASON`], a watchdog reason
    /// (`turn_stalled`, `turn_wall_clock_exceeded`), [`ERROR_REASON`] or
    /// [`INTERRUPTED_REASON`]. Unset for running and completed turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl ChatTurnState {
    pub fn running(chat_turn_id: &str, started_at: i64) -> Self {
        Self {
            chat_turn_id: chat_turn_id.to_string(),
            status: ChatTurnStatus::Running,
            started_at,
            ended_at: None,
            reason: None,
        }
    }

    /// The state after the turn ended with `outcome`. A cancelled turn keeps
    /// a watchdog reason noted while it ran, so only a cancel with no noted
    /// reason reads as the owner's Stop.
    pub fn settled(&self, outcome: ChatTurnOutcome, ended_at: i64) -> Self {
        let (status, reason) = match outcome {
            ChatTurnOutcome::Answered => (ChatTurnStatus::Completed, None),
            ChatTurnOutcome::Cancelled => (
                ChatTurnStatus::Stopped,
                Some(
                    self.reason
                        .clone()
                        .unwrap_or_else(|| USER_STOP_REASON.to_string()),
                ),
            ),
            ChatTurnOutcome::Failed => (ChatTurnStatus::Failed, Some(ERROR_REASON.to_string())),
            ChatTurnOutcome::Interrupted => {
                (ChatTurnStatus::Failed, Some(INTERRUPTED_REASON.to_string()))
            },
        };
        Self {
            chat_turn_id: self.chat_turn_id.clone(),
            status,
            started_at: self.started_at,
            ended_at: Some(ended_at),
            reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turn_settles_to_the_status_clients_show() {
        let running = ChatTurnState::running("chat-turn-1", 10);
        assert_eq!(running.status, ChatTurnStatus::Running);

        let done = running.settled(ChatTurnOutcome::Answered, 20);
        assert_eq!(done.status, ChatTurnStatus::Completed);
        assert_eq!((done.started_at, done.ended_at), (10, Some(20)));
        assert!(done.reason.is_none());

        let stopped = running.settled(ChatTurnOutcome::Cancelled, 20);
        assert_eq!(stopped.status, ChatTurnStatus::Stopped);
        assert_eq!(stopped.reason.as_deref(), Some(USER_STOP_REASON));

        let mut stalled = running.clone();
        stalled.reason = Some("turn_stalled".to_string());
        let watchdog = stalled.settled(ChatTurnOutcome::Cancelled, 20);
        assert_eq!(watchdog.status, ChatTurnStatus::Stopped);
        assert_eq!(watchdog.reason.as_deref(), Some("turn_stalled"));

        let failed = running.settled(ChatTurnOutcome::Failed, 20);
        assert_eq!(failed.status, ChatTurnStatus::Failed);
        assert_eq!(failed.reason.as_deref(), Some(ERROR_REASON));

        let dropped = running.settled(ChatTurnOutcome::Interrupted, 20);
        assert_eq!(dropped.status, ChatTurnStatus::Failed);
        assert_eq!(dropped.reason.as_deref(), Some(INTERRUPTED_REASON));
    }

    #[test]
    fn the_wire_shape_is_snake_case_and_omits_unset_fields() {
        let running = serde_json::to_value(ChatTurnState::running("t", 1)).unwrap();
        assert_eq!(
            running,
            serde_json::json!({"chat_turn_id": "t", "status": "running", "started_at": 1})
        );
        let stopped = serde_json::to_value(
            ChatTurnState::running("t", 1).settled(ChatTurnOutcome::Cancelled, 2),
        )
        .unwrap();
        assert_eq!(stopped["status"], "stopped");
        assert_eq!(stopped["ended_at"], 2);
        assert_eq!(stopped["reason"], "user_stop");
    }
}
