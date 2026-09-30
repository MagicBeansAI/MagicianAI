use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Response shape retained for legacy Magician call sites that still return
/// sidecar observation errors through the old result envelope. Magicutor no
/// longer accepts browser action execution requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResponse {
    /// Execution ID for tracking.
    pub execution_id: String,

    /// Current status.
    pub status: ExecutionStatus,

    /// Result data, when available.
    pub result: Option<Value>,

    /// Error message, when failed.
    pub error: Option<String>,

    /// Session ID, when a browser session is associated with the response.
    pub session_id: Option<String>,
}

/// Status of a retained execution response envelope.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// Session identifier.
pub type SessionId = String;

/// Generate a new session ID.
pub fn new_session_id() -> SessionId {
    Uuid::new_v4().to_string()
}

/// Generate a new execution ID.
pub fn new_execution_id() -> String {
    Uuid::new_v4().to_string()
}

// =============================================================================
// Passive ambient page metadata
// =============================================================================

/// Passive page signal emitted by the extension for a thread-owned tab.
///
/// This is not a browser action result and must not be used as task-completion
/// verification. It is the ambient observation substrate for page identity and
/// coarse change detection.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AmbientPageSignal {
    pub event_id: String,
    pub thread_id: String,
    pub tab_id: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub event_kind: String,
    pub timestamp: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structural_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<AmbientPageSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<AmbientPageChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture: Option<AmbientCaptureStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AmbientPageSummary {
    #[serde(default)]
    pub form_count: u32,
    #[serde(default)]
    pub input_count: u32,
    #[serde(default)]
    pub button_count: u32,
    #[serde(default)]
    pub link_count: u32,
    #[serde(default)]
    pub heading_count: u32,
    #[serde(default)]
    pub iframe_count: u32,
    #[serde(default)]
    pub canvas_count: u32,
    #[serde(default)]
    pub password_input_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AmbientPageChange {
    #[serde(default)]
    pub url_changed: bool,
    #[serde(default)]
    pub title_changed: bool,
    #[serde(default)]
    pub structural_changed: bool,
    #[serde(default)]
    pub content_changed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_structural_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_content_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AmbientCaptureStatus {
    #[serde(default)]
    pub degraded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
}
