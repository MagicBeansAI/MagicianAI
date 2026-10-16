//! MCP client over the outbound socket opened by an Android companion.
//!
//! The phone dials Magician because it may be behind CGNAT, asleep, or moving
//! between networks. Connection direction does not change MCP roles: Magician
//! is the governed MCP client and the phone is a bounded tools-only MCP server.

use std::{collections::HashMap, sync::Arc, time::Duration};

use magician_mcp_client::{
    DuplexJsonTransportConfig, McpClient, McpClientConfig, McpNotificationSubscription,
    McpSubscriptionCapabilities, McpSubscriptionRequest, McpToolCallOutcome, McpToolId,
    McpTransportConfig,
};
use parking_lot::RwLock;
use serde_json::{Map, Value};
use tokio::sync::{mpsc, Notify, OwnedSemaphorePermit, RwLock as AsyncRwLock, Semaphore};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::magician_v2::device_pairing::DeviceAutomationSocketAuthority;

/// Longest a device action may run before the public Android verb returns.
pub const DEFAULT_DEVICE_ACTION_TIMEOUT: Duration = Duration::from_secs(30);

const DEVICE_MCP_CHANNEL_CAPACITY: usize = 64;
const MAX_DEVICE_CONNECTIONS_GLOBAL: usize = 256;
const MAX_DEVICE_CONNECTIONS_PER_SCOPE: usize = 64;

/// Which device a request is for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeviceKey {
    pub principal: String,
    pub workspace: String,
    pub device_id: String,
}

impl DeviceKey {
    pub fn new(
        principal: impl Into<String>,
        workspace: impl Into<String>,
        device_id: impl Into<String>,
    ) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
            device_id: device_id.into(),
        }
    }
}

/// Exact socket generation. A stale actor may never deregister its replacement.
pub type DeviceConnectionId = Uuid;

#[derive(Debug, thiserror::Error)]
pub enum DeviceBridgeError {
    #[error("no device `{0}` is connected for this scope")]
    NotConnected(String),
    #[error("device MCP session is unavailable: {0}")]
    SessionUnavailable(String),
    #[error("device did not answer within {0:?}")]
    Timeout(Duration),
    #[error("device disconnected before answering")]
    Disconnected,
    #[error("device reported: {0}")]
    DeviceError(String),
    #[error("device protected foreground application `{0}` from automation")]
    AppProtected(String),
    #[error("device answered without a result")]
    EmptyResult,
    #[error("the bound device connection changed before the action completed")]
    ConnectionChanged,
    #[error("device result exceeded the admitted {0}-byte ceiling")]
    ResultTooLarge(usize),
}

/// Exact authenticated socket generation selected by an Apps physical owner.
///
/// This is availability, not app authority: it deliberately implements
/// neither Serde nor a public raw-device getter. The interactive owner still
/// has to bind this generation into its reviewed target and consume the common
/// one-shot effect permit before calling [`DeviceBridgeHub::dispatch_bound`].
pub(crate) struct DeviceOwnerConnection {
    key: DeviceKey,
    id: DeviceConnectionId,
    protocol_version: String,
    server_name: String,
    server_version: String,
    automation_target_ref: String,
    automation_identity_digest: String,
    automation_review_generation: u64,
    play_integrity_verdict_digest: String,
    owner_io: Arc<Semaphore>,
}

impl DeviceOwnerConnection {
    pub(crate) fn key(&self) -> &DeviceKey {
        &self.key
    }

    pub(crate) fn connection_id(&self) -> DeviceConnectionId {
        self.id
    }

    pub(crate) fn protocol_version(&self) -> &str {
        &self.protocol_version
    }

    pub(crate) fn server_name(&self) -> &str {
        &self.server_name
    }

    pub(crate) fn server_version(&self) -> &str {
        &self.server_version
    }

    pub(crate) fn automation_target_ref(&self) -> &str {
        &self.automation_target_ref
    }

    pub(crate) fn automation_identity_digest(&self) -> &str {
        &self.automation_identity_digest
    }

    pub(crate) fn automation_review_generation(&self) -> u64 {
        self.automation_review_generation
    }

    pub(crate) fn play_integrity_verdict_digest(&self) -> &str {
        &self.play_integrity_verdict_digest
    }

    /// Reserve the sole Apps physical-owner lane for this socket generation.
    /// The caller acquires it before durable dispatch-start; no action waits
    /// behind unrelated handset I/O after consuming its one-shot effect token.
    pub(crate) async fn reserve_owner_io(&self) -> Result<OwnedSemaphorePermit, DeviceBridgeError> {
        Arc::clone(&self.owner_io)
            .acquire_owned()
            .await
            .map_err(|_| DeviceBridgeError::ConnectionChanged)
    }
}

/// How MCP frames reach the WebSocket actor that owns the accepted connection.
pub trait DeviceSink: Send + Sync {
    /// False means the actor mailbox has already closed.
    fn send_text(&self, payload: String) -> bool;

    /// Close the owning socket after an unrecoverable MCP session failure.
    fn close(&self);
}

struct DeviceMcpSession {
    client: McpClient,
    tools: HashMap<String, DeviceTool>,
}

/// One advertised tool: its id for calls and the argument names its schema
/// requires, so a caller can tell an eligible companion from an older one
/// before dispatching.
#[derive(Clone)]
struct DeviceTool {
    id: McpToolId,
    required: Vec<String>,
}

enum DeviceConnectionState {
    Connecting,
    Ready(Arc<AsyncRwLock<DeviceMcpSession>>),
    Failed(String),
}

struct DeviceConnection {
    id: DeviceConnectionId,
    inbound: mpsc::Sender<String>,
    state: RwLock<DeviceConnectionState>,
    changed: Notify,
    cancelled: CancellationToken,
    sink: Arc<dyn DeviceSink>,
    authority: DeviceAutomationSocketAuthority,
    owner_io: Arc<Semaphore>,
}

impl DeviceConnection {
    fn fail(&self, reason: impl Into<String>) {
        *self.state.write() = DeviceConnectionState::Failed(reason.into());
        self.changed.notify_waiters();
    }

    fn terminate(&self, reason: impl Into<String>) {
        self.fail(reason);
        self.cancelled.cancel();
        self.sink.close();
    }

    fn ready(&self, session: Arc<AsyncRwLock<DeviceMcpSession>>) -> bool {
        let mut state = self.state.write();
        if self.cancelled.is_cancelled() {
            return false;
        }
        *state = DeviceConnectionState::Ready(session);
        self.changed.notify_waiters();
        true
    }

    async fn wait_until_ready(
        &self,
        deadline: tokio::time::Instant,
        timeout: Duration,
    ) -> Result<Arc<AsyncRwLock<DeviceMcpSession>>, DeviceBridgeError> {
        loop {
            // Register before inspecting state, so a transition between the
            // check and the await cannot become a missed wake-up.
            let changed = self.changed.notified();
            match &*self.state.read() {
                DeviceConnectionState::Ready(session) => return Ok(Arc::clone(session)),
                DeviceConnectionState::Failed(reason) => {
                    return Err(DeviceBridgeError::SessionUnavailable(reason.clone()));
                },
                DeviceConnectionState::Connecting => {},
            }
            if tokio::time::timeout_at(deadline, changed).await.is_err() {
                return Err(DeviceBridgeError::Timeout(timeout));
            }
        }
    }
}

struct HubInner {
    devices: HashMap<DeviceKey, Arc<DeviceConnection>>,
}

/// The process-wide registry of authenticated, scoped Android MCP connections.
static GLOBAL_HUB: std::sync::OnceLock<Arc<DeviceBridgeHub>> = std::sync::OnceLock::new();

pub fn install_global_hub(hub: Arc<DeviceBridgeHub>) {
    let _ = GLOBAL_HUB.set(hub);
}

pub fn global_hub() -> Option<Arc<DeviceBridgeHub>> {
    GLOBAL_HUB.get().cloned()
}

pub struct DeviceBridgeHub {
    inner: RwLock<HubInner>,
}

impl Default for DeviceBridgeHub {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceBridgeHub {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HubInner {
                devices: HashMap::new(),
            }),
        }
    }

    /// Attach one accepted WebSocket and start explicit MCP discovery on it.
    pub fn connect(
        &self,
        key: DeviceKey,
        id: DeviceConnectionId,
        authority: DeviceAutomationSocketAuthority,
        sink: Arc<dyn DeviceSink>,
    ) -> bool {
        let (to_client, from_socket) = mpsc::channel(DEVICE_MCP_CHANNEL_CAPACITY);
        let (to_socket, mut from_client) = mpsc::channel(DEVICE_MCP_CHANNEL_CAPACITY);
        let connection = Arc::new(DeviceConnection {
            id,
            inbound: to_client,
            state: RwLock::new(DeviceConnectionState::Connecting),
            changed: Notify::new(),
            cancelled: CancellationToken::new(),
            sink: Arc::clone(&sink),
            authority,
            owner_io: Arc::new(Semaphore::new(1)),
        });

        let previous = {
            let mut inner = self.inner.write();
            let replacing = inner.devices.contains_key(&key);
            let scope_count = inner
                .devices
                .keys()
                .filter(|candidate| {
                    candidate.principal == key.principal && candidate.workspace == key.workspace
                })
                .count();
            if !replacing
                && (inner.devices.len() >= MAX_DEVICE_CONNECTIONS_GLOBAL
                    || scope_count >= MAX_DEVICE_CONNECTIONS_PER_SCOPE)
            {
                drop(inner);
                connection.terminate("device connection capacity reached");
                return false;
            }
            inner.devices.insert(key.clone(), Arc::clone(&connection))
        };
        if let Some(previous) = previous {
            previous.terminate("device reconnected");
        }

        let outbound_connection = Arc::clone(&connection);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = outbound_connection.cancelled.cancelled() => break,
                    payload = from_client.recv() => {
                        let Some(payload) = payload else { break };
                        if !sink.send_text(payload) {
                            outbound_connection.terminate("device socket closed");
                            break;
                        }
                    }
                }
            }
        });

        tokio::spawn(establish_mcp_session(
            Arc::clone(&connection),
            from_socket,
            to_socket,
        ));
        true
    }

    /// Deregister only the socket generation that actually stopped.
    pub fn disconnect(&self, key: &DeviceKey, id: DeviceConnectionId) {
        let removed = {
            let mut inner = self.inner.write();
            if inner.devices.get(key).is_some_and(|entry| entry.id == id) {
                inner.devices.remove(key)
            } else {
                None
            }
        };
        if let Some(connection) = removed {
            connection.fail("device disconnected");
            connection.cancelled.cancel();
        }
    }

    /// Revoke the currently attached generation regardless of its id.
    ///
    /// Owner-initiated unpairing is stronger than an actor cleanup callback:
    /// it must close a live socket immediately, not merely refuse the next
    /// reconnect. Removing before termination also fences the actor's eventual
    /// stale `disconnect` callback from disturbing a replacement.
    pub fn revoke(&self, key: &DeviceKey) -> bool {
        let removed = self.inner.write().devices.remove(key);
        if let Some(connection) = removed {
            connection.terminate("device unpaired");
            true
        } else {
            false
        }
    }

    pub fn is_connected(&self, key: &DeviceKey) -> bool {
        self.inner.read().devices.contains_key(key)
    }

    /// Check that a move-only Apps binding still names the exact currently
    /// authenticated socket generation. This is used by the final pre-start
    /// fence so a reconnect is rejected before durable effect start; the same
    /// identity is checked again by `dispatch_bound` at physical I/O.
    pub(crate) fn owner_connection_is_current(&self, binding: &DeviceOwnerConnection) -> bool {
        self.owner_connection_identity_is_current(&binding.key, binding.id)
    }

    pub(crate) fn owner_connection_identity_is_current(
        &self,
        key: &DeviceKey,
        connection_id: DeviceConnectionId,
    ) -> bool {
        self.inner
            .read()
            .devices
            .get(key)
            .is_some_and(|current| current.id == connection_id && !current.cancelled.is_cancelled())
    }

    pub fn connected_devices(&self) -> Vec<DeviceKey> {
        self.inner.read().devices.keys().cloned().collect()
    }

    /// Bind an Apps owner to the exact currently authenticated socket.
    /// Reconnection creates a different UUID and invalidates this value.
    pub(crate) async fn bind_owner_connection(
        &self,
        key: &DeviceKey,
        timeout: Duration,
    ) -> Result<DeviceOwnerConnection, DeviceBridgeError> {
        let connection = self
            .inner
            .read()
            .devices
            .get(key)
            .cloned()
            .ok_or_else(|| DeviceBridgeError::NotConnected(key.device_id.clone()))?;
        let id = connection.id;
        let deadline = tokio::time::Instant::now() + timeout;
        let session = connection.wait_until_ready(deadline, timeout).await?;
        let session = session.read().await;
        let info = session.client.connection_info();
        let server_name = info.server_name.clone().ok_or_else(|| {
            DeviceBridgeError::SessionUnavailable("device server identity is absent".to_owned())
        })?;
        let server_version = info.server_version.clone().ok_or_else(|| {
            DeviceBridgeError::SessionUnavailable("device server version is absent".to_owned())
        })?;
        let current_matches = self
            .inner
            .read()
            .devices
            .get(key)
            .is_some_and(|current| current.id == id);
        if !current_matches || connection.cancelled.is_cancelled() {
            return Err(DeviceBridgeError::ConnectionChanged);
        }
        Ok(DeviceOwnerConnection {
            key: key.clone(),
            id,
            protocol_version: info.protocol_version.clone(),
            server_name,
            server_version,
            automation_target_ref: connection.authority.target_ref().to_owned(),
            automation_identity_digest: connection.authority.identity_digest().to_owned(),
            automation_review_generation: connection.authority.review_generation(),
            play_integrity_verdict_digest: connection
                .authority
                .play_integrity_verdict_digest()
                .expect("hub admits only fresh Play Integrity authorities")
                .to_owned(),
            owner_io: Arc::clone(&connection.owner_io),
        })
    }

    /// Deliver one complete WebSocket text frame into the rmcp transport.
    ///
    /// This is synchronous so the actor preserves WebSocket ordering. A full
    /// bounded channel fails closed instead of queueing unbounded paired-device
    /// input in process memory.
    pub fn receive_text(
        &self,
        key: &DeviceKey,
        id: DeviceConnectionId,
        payload: String,
    ) -> Result<(), DeviceBridgeError> {
        let connection = self
            .inner
            .read()
            .devices
            .get(key)
            .filter(|connection| connection.id == id)
            .cloned()
            .ok_or(DeviceBridgeError::Disconnected)?;
        connection.inbound.try_send(payload).map_err(|error| {
            connection.terminate("device MCP inbound channel closed or full");
            DeviceBridgeError::SessionUnavailable(error.to_string())
        })
    }

    /// Call one tool name from the latest authoritative device discovery.
    pub async fn dispatch(
        &self,
        key: &DeviceKey,
        action: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, DeviceBridgeError> {
        let connection = self
            .inner
            .read()
            .devices
            .get(key)
            .cloned()
            .ok_or_else(|| DeviceBridgeError::NotConnected(key.device_id.clone()))?;
        let deadline = tokio::time::Instant::now() + timeout;
        self.dispatch_connection(key, connection, action, params, timeout, deadline, None)
            .await
    }

    /// Whether the connected device's `action` requires the named argument
    /// by its own schema — a caller's way to tell an eligible companion from
    /// an older build before dispatching anything to it.
    pub async fn tool_requires(
        &self,
        key: &DeviceKey,
        action: &str,
        argument: &str,
        timeout: Duration,
    ) -> Result<bool, DeviceBridgeError> {
        let connection = self
            .inner
            .read()
            .devices
            .get(key)
            .cloned()
            .ok_or_else(|| DeviceBridgeError::NotConnected(key.device_id.clone()))?;
        let deadline = tokio::time::Instant::now() + timeout;
        let session = connection.wait_until_ready(deadline, timeout).await?;
        let session = session.read().await;
        let tool = session.tools.get(action).ok_or_else(|| {
            DeviceBridgeError::DeviceError(format!(
                "this device does not currently advertise `{action}`"
            ))
        })?;
        Ok(tool.required.iter().any(|name| name == argument))
    }

    /// Dispatch through one exact socket generation and enforce the Apps
    /// result ceiling before returning bytes to the workflow owner.
    ///
    /// Dropping this future at a stop/deadline fence drops the MCP call future;
    /// the client emits the protocol cancellation notification for the exact
    /// request. The handset tracks that request and cancels its coroutine.
    pub(crate) async fn dispatch_bound(
        &self,
        binding: &DeviceOwnerConnection,
        action: &str,
        params: Value,
        timeout: Duration,
        max_result_bytes: usize,
    ) -> Result<Value, DeviceBridgeError> {
        if max_result_bytes == 0 {
            return Err(DeviceBridgeError::ResultTooLarge(0));
        }
        let connection = self
            .inner
            .read()
            .devices
            .get(&binding.key)
            .filter(|connection| connection.id == binding.id)
            .cloned()
            .ok_or(DeviceBridgeError::ConnectionChanged)?;
        let deadline = tokio::time::Instant::now() + timeout;
        self.dispatch_connection(
            &binding.key,
            connection,
            action,
            params,
            timeout,
            deadline,
            Some((binding.id, max_result_bytes)),
        )
        .await
    }

    async fn dispatch_connection(
        &self,
        key: &DeviceKey,
        connection: Arc<DeviceConnection>,
        action: &str,
        params: Value,
        timeout: Duration,
        deadline: tokio::time::Instant,
        bound: Option<(DeviceConnectionId, usize)>,
    ) -> Result<Value, DeviceBridgeError> {
        let session = connection.wait_until_ready(deadline, timeout).await?;

        loop {
            let needs_refresh = {
                let session = session.read().await;
                session
                    .client
                    .invalidation_state()
                    .map_err(|error| DeviceBridgeError::SessionUnavailable(error.to_string()))?
                    .tools_changed()
            };
            if needs_refresh {
                let mut session = session.write().await;
                if let Some(tools) = session
                    .client
                    .refresh_tools_if_invalidated()
                    .await
                    .map_err(|error| DeviceBridgeError::SessionUnavailable(error.to_string()))?
                {
                    session.tools = tool_index(tools);
                }
                continue;
            }

            let session = session.read().await;
            let tool = session
                .tools
                .get(action)
                .map(|tool| tool.id.clone())
                .ok_or_else(|| {
                    DeviceBridgeError::DeviceError(format!(
                        "this device does not currently advertise `{action}`"
                    ))
                })?;
            let arguments = params.as_object().cloned().unwrap_or_else(Map::new);
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(DeviceBridgeError::Timeout(timeout));
            }
            let outcome =
                tokio::time::timeout(remaining, session.client.call_tool(&tool, arguments))
                    .await
                    .map_err(|_| DeviceBridgeError::Timeout(timeout))?
                    .map_err(|error| {
                        if connection.cancelled.is_cancelled() {
                            DeviceBridgeError::Disconnected
                        } else {
                            DeviceBridgeError::SessionUnavailable(error.to_string())
                        }
                    })?;
            let projected = project_tool_outcome(outcome)?;
            if let Some((expected_id, max_result_bytes)) = bound {
                let current_matches = self
                    .inner
                    .read()
                    .devices
                    .get(key)
                    .is_some_and(|current| current.id == expected_id);
                if !current_matches || connection.cancelled.is_cancelled() {
                    return Err(DeviceBridgeError::ConnectionChanged);
                }
                let encoded = serde_json::to_vec(&projected)
                    .map_err(|error| DeviceBridgeError::DeviceError(error.to_string()))?;
                if encoded.len() > max_result_bytes {
                    return Err(DeviceBridgeError::ResultTooLarge(max_result_bytes));
                }
            }
            return Ok(projected);
        }
    }
}

async fn establish_mcp_session(
    connection: Arc<DeviceConnection>,
    inbound: mpsc::Receiver<String>,
    outbound: mpsc::Sender<String>,
) {
    let mut config = McpClientConfig::new(McpTransportConfig::DuplexJson(
        DuplexJsonTransportConfig::new(inbound, outbound),
    ));
    config.connect_timeout = Duration::from_secs(20);
    config.request_timeout = DEFAULT_DEVICE_ACTION_TIMEOUT;
    config.subscription_capabilities =
        McpSubscriptionCapabilities::new().enable_tools_list_changed();

    let mut client = match McpClient::connect(config).await {
        Ok(client) => client,
        Err(error) => {
            connection.terminate(error.to_string());
            return;
        },
    };
    // Subscribe before taking the initial snapshot. Activating a subscription
    // intentionally dirties every accepted catalog to close the race between
    // an earlier list and the listen acknowledgement; listing afterwards both
    // acknowledges that epoch and avoids fetching the same roster twice.
    let subscription = match client
        .open_notification_subscription(McpSubscriptionRequest::new().with_tools_list_changed())
        .await
    {
        Ok(subscription) => subscription,
        Err(error) => {
            connection.terminate(error.to_string());
            return;
        },
    };
    let tools = match client.discover_tools().await {
        Ok(tools) => tool_index(tools),
        Err(error) => {
            connection.terminate(error.to_string());
            return;
        },
    };

    let session = Arc::new(AsyncRwLock::new(DeviceMcpSession { client, tools }));
    if !connection.ready(session) {
        return;
    }
    tokio::spawn(drain_tool_invalidations(
        Arc::clone(&connection),
        subscription,
    ));
}

async fn drain_tool_invalidations(
    connection: Arc<DeviceConnection>,
    mut subscription: McpNotificationSubscription,
) {
    loop {
        tokio::select! {
            _ = connection.cancelled.cancelled() => {
                let _ = subscription.cancel().await;
                break;
            },
            update = subscription.next() => {
                if update.is_err() {
                    connection.terminate("device MCP tool subscription ended");
                    break;
                }
            },
        }
    }
}

fn tool_index(tools: Vec<magician_mcp_client::McpToolDescriptor>) -> HashMap<String, DeviceTool> {
    tools
        .into_iter()
        .map(|tool| {
            let required = tool
                .input_schema()
                .get("required")
                .and_then(Value::as_array)
                .map(|names| {
                    names
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            (
                tool.id().remote_name().to_owned(),
                DeviceTool {
                    id: tool.id().clone(),
                    required,
                },
            )
        })
        .collect()
}

fn project_tool_outcome(outcome: McpToolCallOutcome) -> Result<Value, DeviceBridgeError> {
    let McpToolCallOutcome::Complete(result) = outcome else {
        return Err(DeviceBridgeError::DeviceError(
            "Android companion returned an unsupported MCP continuation".to_owned(),
        ));
    };
    if result.is_error {
        if let Some(package) = result
            .structured_content
            .as_ref()
            .filter(|content| {
                content
                    .get("app_protected")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            })
            .and_then(|content| content.get("foreground_package"))
            .and_then(Value::as_str)
            .filter(|package| package.len() <= 255)
        {
            return Err(DeviceBridgeError::AppProtected(package.to_owned()));
        }
        let detail = result
            .content
            .iter()
            .find_map(|block| block.get("text").and_then(Value::as_str))
            .unwrap_or("tool reported an error")
            .to_owned();
        return Err(DeviceBridgeError::DeviceError(detail));
    }
    let mut projected = Map::new();
    projected.insert("isError".to_owned(), Value::Bool(false));
    projected.insert("content".to_owned(), Value::Array(result.content));
    if let Some(structured) = result.structured_content {
        projected.insert("structuredContent".to_owned(), structured);
    }
    Ok(Value::Object(projected))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    };

    struct FakePhone {
        hub: Arc<DeviceBridgeHub>,
        key: DeviceKey,
        connection_id: DeviceConnectionId,
        sent: Mutex<Vec<Value>>,
        subscription_id: Mutex<Option<Value>>,
        list_calls: AtomicUsize,
        fail_tools: AtomicBool,
        answer_tools: AtomicBool,
        closed: AtomicBool,
    }

    impl FakePhone {
        fn new(
            hub: Arc<DeviceBridgeHub>,
            key: DeviceKey,
            connection_id: DeviceConnectionId,
        ) -> Arc<Self> {
            Arc::new(Self {
                hub,
                key,
                connection_id,
                sent: Mutex::new(Vec::new()),
                subscription_id: Mutex::new(None),
                list_calls: AtomicUsize::new(0),
                fail_tools: AtomicBool::new(false),
                answer_tools: AtomicBool::new(true),
                closed: AtomicBool::new(false),
            })
        }

        fn respond(&self, frame: &Value) -> Option<Value> {
            let method = frame.get("method")?.as_str()?;
            let id = frame.get("id").cloned().unwrap_or(Value::Null);
            match method {
                "server/discover" => Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "resultType": "complete",
                        "supportedVersions": ["2026-07-28"],
                        "capabilities": {"tools": {"listChanged": true}},
                        "ttlMs": 300000,
                        "cacheScope": "private",
                        "_meta": {"io.modelcontextprotocol/serverInfo": {
                            "name": "fake-phone", "version": "1.0.0"
                        }}
                    }
                })),
                "tools/list" => {
                    self.list_calls.fetch_add(1, Ordering::SeqCst);
                    Some(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {"tools": [{
                            "name": "android_tap",
                            "description": "tap",
                            "inputSchema": {"type": "object"},
                            "annotations": {
                                "readOnlyHint": false,
                                "destructiveHint": true,
                                "idempotentHint": false,
                                "openWorldHint": true
                            }
                        }]}
                    }))
                },
                "subscriptions/listen" => {
                    *self.subscription_id.lock().unwrap() = Some(id.clone());
                    Some(json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/subscriptions/acknowledged",
                        "params": {
                            "_meta": {"io.modelcontextprotocol/subscriptionId": id},
                            "notifications": {"toolsListChanged": true}
                        }
                    }))
                },
                "tools/call" if self.answer_tools.load(Ordering::SeqCst) => {
                    let failed = self.fail_tools.load(Ordering::SeqCst);
                    Some(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "resultType": "complete",
                            "content": [{"type": "text", "text": if failed {
                                "no such node"
                            } else {
                                "{\"tapped\":true}"
                            }}],
                            "isError": failed
                        }
                    }))
                },
                _ => None,
            }
        }

        fn notify_tools_changed(&self) {
            let id = self.subscription_id.lock().unwrap().clone().unwrap();
            self.hub
                .receive_text(
                    &self.key,
                    self.connection_id,
                    json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/tools/list_changed",
                        "params": {"_meta": {
                            "io.modelcontextprotocol/subscriptionId": id
                        }}
                    })
                    .to_string(),
                )
                .unwrap();
        }
    }

    impl DeviceSink for FakePhone {
        fn send_text(&self, payload: String) -> bool {
            let frame: Value = serde_json::from_str(&payload).unwrap();
            self.sent.lock().unwrap().push(frame.clone());
            if let Some(response) = self.respond(&frame) {
                if self
                    .hub
                    .receive_text(&self.key, self.connection_id, response.to_string())
                    .is_err()
                {
                    return false;
                }
            }
            true
        }

        fn close(&self) {
            self.closed.store(true, Ordering::SeqCst);
        }
    }

    fn key() -> DeviceKey {
        DeviceKey::new("owner", "default", "pixel-9")
    }

    fn connect_phone(hub: &Arc<DeviceBridgeHub>, key: DeviceKey) -> Arc<FakePhone> {
        let connection_id = Uuid::new_v4();
        let phone = FakePhone::new(Arc::clone(hub), key.clone(), connection_id);
        assert!(hub.connect(
            key,
            connection_id,
            crate::magician_v2::device_pairing::test_device_automation_socket_authority(),
            phone.clone(),
        ));
        phone
    }

    #[tokio::test]
    async fn dispatch_uses_discover_list_subscribe_and_tools_call_over_mcp() {
        let hub = Arc::new(DeviceBridgeHub::new());
        let phone = connect_phone(&hub, key());
        let result = hub
            .dispatch(
                &key(),
                "android_tap",
                json!({"x": 10, "y": 20}),
                Duration::from_secs(2),
            )
            .await
            .unwrap();

        assert_eq!(result["isError"], false);
        let sent = phone.sent.lock().unwrap();
        let methods = sent
            .iter()
            .filter_map(|frame| frame.get("method").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert!(methods.starts_with(&["server/discover", "subscriptions/listen", "tools/list",]));
        assert!(methods.contains(&"tools/call"));
        assert!(sent.iter().all(|frame| frame.get("type").is_none()));
    }

    #[tokio::test]
    async fn per_scope_socket_capacity_rejects_new_connection_without_evicting_live_owners() {
        let hub = Arc::new(DeviceBridgeHub::new());
        let mut phones = Vec::new();
        for index in 0..MAX_DEVICE_CONNECTIONS_PER_SCOPE {
            phones.push(connect_phone(
                &hub,
                DeviceKey::new("owner", "default", format!("phone-{index}")),
            ));
        }
        let overflow_key = DeviceKey::new("owner", "default", "phone-overflow");
        let overflow_connection_id = Uuid::new_v4();
        let overflow = FakePhone::new(
            Arc::clone(&hub),
            overflow_key.clone(),
            overflow_connection_id,
        );
        assert!(!hub.connect(
            overflow_key.clone(),
            overflow_connection_id,
            crate::magician_v2::device_pairing::test_device_automation_socket_authority(),
            overflow.clone(),
        ));
        assert!(overflow.closed.load(Ordering::SeqCst));
        assert!(!hub.is_connected(&overflow_key));
        assert_eq!(
            hub.connected_devices().len(),
            MAX_DEVICE_CONNECTIONS_PER_SCOPE
        );
        assert!(hub.is_connected(&DeviceKey::new("owner", "default", "phone-0")));
        drop(phones);
    }

    #[tokio::test]
    async fn device_tool_failure_remains_distinct_from_transport_failure() {
        let hub = Arc::new(DeviceBridgeHub::new());
        let phone = connect_phone(&hub, key());
        phone.fail_tools.store(true, Ordering::SeqCst);
        let result = hub
            .dispatch(&key(), "android_tap", json!({}), Duration::from_secs(2))
            .await;
        assert!(matches!(
            result,
            Err(DeviceBridgeError::DeviceError(message)) if message == "no such node"
        ));
    }

    #[tokio::test]
    async fn tool_list_notification_causes_authoritative_refresh_before_next_call() {
        let hub = Arc::new(DeviceBridgeHub::new());
        let phone = connect_phone(&hub, key());
        hub.dispatch(&key(), "android_tap", json!({}), Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(phone.list_calls.load(Ordering::SeqCst), 1);
        phone.notify_tools_changed();
        tokio::task::yield_now().await;
        hub.dispatch(&key(), "android_tap", json!({}), Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(phone.list_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn stale_disconnect_cannot_remove_replacement_socket() {
        let hub = Arc::new(DeviceBridgeHub::new());
        let first = connect_phone(&hub, key());
        let second = connect_phone(&hub, key());
        assert!(first.closed.load(Ordering::SeqCst));
        hub.disconnect(&key(), first.connection_id);
        assert!(hub.is_connected(&key()));
        hub.dispatch(&key(), "android_tap", json!({}), Duration::from_secs(2))
            .await
            .unwrap();
        assert!(second
            .sent
            .lock()
            .unwrap()
            .iter()
            .any(|frame| frame.get("method") == Some(&Value::String("tools/call".into()))));
    }

    #[tokio::test]
    async fn owner_connection_currentness_changes_at_reconnect() {
        let hub = Arc::new(DeviceBridgeHub::new());
        connect_phone(&hub, key());
        let binding = hub
            .bind_owner_connection(&key(), Duration::from_secs(2))
            .await
            .unwrap();
        assert!(hub.owner_connection_is_current(&binding));

        connect_phone(&hub, key());
        assert!(!hub.owner_connection_is_current(&binding));
    }

    #[tokio::test]
    async fn revocation_closes_and_removes_the_live_socket_immediately() {
        let hub = Arc::new(DeviceBridgeHub::new());
        let phone = connect_phone(&hub, key());

        assert!(hub.revoke(&key()));
        assert!(phone.closed.load(Ordering::SeqCst));
        assert!(!hub.is_connected(&key()));
        assert!(!hub.revoke(&key()));
    }

    #[tokio::test]
    async fn dispatching_to_an_absent_device_fails_without_waiting() {
        let hub = DeviceBridgeHub::new();
        let result = hub
            .dispatch(&key(), "android_tap", json!({}), Duration::from_secs(30))
            .await;
        assert!(matches!(result, Err(DeviceBridgeError::NotConnected(_))));
    }

    #[tokio::test]
    async fn an_unanswered_tool_call_obeys_the_public_action_timeout() {
        let hub = Arc::new(DeviceBridgeHub::new());
        let phone = connect_phone(&hub, key());
        phone.answer_tools.store(false, Ordering::SeqCst);
        let result = hub
            .dispatch(&key(), "android_tap", json!({}), Duration::from_millis(25))
            .await;
        assert!(matches!(result, Err(DeviceBridgeError::Timeout(_))));
    }

    #[tokio::test]
    async fn an_invalid_mcp_stream_closes_the_socket_so_the_phone_reconnects() {
        let hub = Arc::new(DeviceBridgeHub::new());
        let phone = connect_phone(&hub, key());
        hub.receive_text(&key(), phone.connection_id, "not-json".to_owned())
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !phone.closed.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn apps_owner_binding_rejects_reconnected_socket_and_oversized_result() {
        let hub = Arc::new(DeviceBridgeHub::new());
        connect_phone(&hub, key());
        let bound = hub
            .bind_owner_connection(&key(), Duration::from_secs(2))
            .await
            .unwrap();
        assert!(matches!(
            hub.dispatch_bound(&bound, "android_tap", json!({}), Duration::from_secs(2), 1,)
                .await,
            Err(DeviceBridgeError::ResultTooLarge(1))
        ));

        connect_phone(&hub, key());
        assert!(matches!(
            hub.dispatch_bound(
                &bound,
                "android_tap",
                json!({}),
                Duration::from_secs(2),
                1024,
            )
            .await,
            Err(DeviceBridgeError::ConnectionChanged)
        ));
    }

    #[tokio::test]
    async fn apps_owner_socket_lane_is_single_in_flight() {
        let hub = Arc::new(DeviceBridgeHub::new());
        connect_phone(&hub, key());
        let bound = hub
            .bind_owner_connection(&key(), Duration::from_secs(2))
            .await
            .unwrap();
        let first = bound.reserve_owner_io().await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(10), bound.reserve_owner_io())
                .await
                .is_err()
        );
        drop(first);
        assert!(
            tokio::time::timeout(Duration::from_secs(1), bound.reserve_owner_io())
                .await
                .is_ok()
        );
    }
}
