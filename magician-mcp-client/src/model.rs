use std::fmt;

use serde::Serialize;
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

/// Cloneable, payload-free cancellation authority for one or more MCP call futures.
///
/// Cancellation is sticky. Passing an already-cancelled token prevents dispatch; cancelling
/// it after dispatch asks the official SDK transport to cancel the exact request.
#[derive(Clone, Default)]
pub struct McpCallCancellation {
    inner: CancellationToken,
}

impl McpCallCancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.inner.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    pub(crate) async fn cancelled(&self) {
        self.inner.cancelled().await;
    }
}

impl fmt::Debug for McpCallCancellation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpCallCancellation")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

/// Opaque identifier minted from one validated discovery snapshot.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct McpToolId {
    client_instance_id: u64,
    discovery_generation: u64,
    remote_name: String,
}

impl McpToolId {
    pub fn remote_name(&self) -> &str {
        &self.remote_name
    }

    pub(crate) fn new(
        remote_name: String,
        client_instance_id: u64,
        discovery_generation: u64,
    ) -> Self {
        Self {
            client_instance_id,
            discovery_generation,
            remote_name,
        }
    }
}

impl fmt::Debug for McpToolId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpToolId")
            .field("client_instance_id", &self.client_instance_id)
            .field("discovery_generation", &self.discovery_generation)
            .field("remote_name", &"<redacted>")
            .finish()
    }
}

/// Untrusted MCP annotations preserved as display hints only.
///
/// Authorization and approval code must not treat these fields as policy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct McpToolHints {
    pub read_only: Option<bool>,
    pub destructive: Option<bool>,
    pub idempotent: Option<bool>,
    pub open_world: Option<bool>,
}

#[derive(Clone, PartialEq)]
pub struct McpToolDescriptor {
    pub(crate) id: McpToolId,
    pub(crate) title: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) input_schema: Map<String, Value>,
    pub(crate) output_schema: Option<Map<String, Value>>,
    pub(crate) hints: McpToolHints,
}

impl fmt::Debug for McpToolDescriptor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpToolDescriptor")
            .field("id", &self.id)
            .field("has_title", &self.title.is_some())
            .field("has_description", &self.description.is_some())
            .field("input_schema_keys", &self.input_schema.len())
            .field("has_output_schema", &self.output_schema.is_some())
            .finish()
    }
}

impl McpToolDescriptor {
    pub fn id(&self) -> &McpToolId {
        &self.id
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    pub fn input_schema(&self) -> &Map<String, Value> {
        &self.input_schema
    }

    pub fn output_schema(&self) -> Option<&Map<String, Value>> {
        self.output_schema.as_ref()
    }

    pub fn hints(&self) -> &McpToolHints {
        &self.hints
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct McpConnectionInfo {
    pub transport: &'static str,
    pub protocol_version: String,
    pub server_name: Option<String>,
    pub server_version: Option<String>,
    pub instructions: Option<String>,
    pub capabilities: Value,
    pub sdk_version: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct McpToolCallResult {
    pub result_type: String,
    pub content: Vec<Value>,
    pub structured_content: Option<Value>,
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum McpToolCallOutcome {
    Complete(McpToolCallResult),
    InputRequired(crate::McpPendingCall),
    Task(crate::McpPendingCall),
}
