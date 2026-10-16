//! Shared protocol types for the Magicutor WebSocket bridge.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Request sent by the Magicutor server to the extension over the bridge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionRequest {
    /// Unique request ID for matching responses.
    #[serde(rename = "requestId")]
    pub request_id: String,

    /// Bridge action to execute in the extension background worker.
    pub action: String,

    /// Action parameters.
    #[serde(default)]
    pub params: Value,
}

/// Response returned by the extension over the bridge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionResponse {
    /// Request ID this response corresponds to.
    #[serde(rename = "requestId")]
    pub request_id: String,

    /// Success status.
    pub success: bool,

    /// Result data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,

    /// Error message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ExtensionResponse {
    pub fn success(request_id: String, result: Value) -> Self {
        Self {
            request_id,
            success: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(request_id: String, error: String) -> Self {
        Self {
            request_id,
            success: false,
            result: None,
            error: Some(error),
        }
    }
}
