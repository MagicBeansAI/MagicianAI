//! Server-side registry for outbound Magician Edge sessions.
//!
//! Socket admission authenticates a desktop before calling [`EdgeSessionRegistry::connect`].
//! The registry then owns the session generation, capability snapshot, lease,
//! request correlation, and replacement fencing. It intentionally knows
//! nothing about Actix or Tauri so the socket adapter and fake-Edge tests share
//! exactly the same lifecycle.

use parking_lot::{Mutex, RwLock};
use runtime_core::edge::{
    EdgeCallResult, EdgeCancel, EdgeCapabilityDescriptor, EdgeCapabilityUpdate, EdgeClientMessage,
    EdgeExecutionGrant, EdgeHeartbeat, EdgeHello, EdgeInvoke, EdgeServerMessage,
    EdgeSessionAccepted, EDGE_MAX_IN_FLIGHT_CALLS, EDGE_PROTOCOL_VERSION,
};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use uuid::Uuid;

const EDGE_LEASE_DURATION: Duration = Duration::from_secs(90);
const EDGE_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const MAX_EDGE_SESSIONS_GLOBAL: usize = 256;
const MAX_EDGE_SESSIONS_PER_SCOPE: usize = 32;
pub const EDGE_MIN_DISPATCH_TIMEOUT_MS: u64 = 100;
pub const EDGE_MAX_DISPATCH_TIMEOUT_MS: u64 = 120_000;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EdgeSessionKey {
    pub principal: String,
    pub workspace: String,
    pub device_id: String,
}

impl EdgeSessionKey {
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

    fn validate(&self) -> Result<(), EdgeSessionError> {
        for (name, value) in [
            ("principal", self.principal.as_str()),
            ("workspace", self.workspace.as_str()),
            ("device_id", self.device_id.as_str()),
        ] {
            if value.trim().is_empty() || value.len() > 160 || value.chars().any(char::is_control) {
                return Err(EdgeSessionError::InvalidProtocol(format!(
                    "invalid Edge {name}"
                )));
            }
        }
        Ok(())
    }
}

pub trait EdgeSessionSink: Send + Sync {
    /// Queue one complete JSON text frame. False means the socket is gone or
    /// its bounded mailbox is full.
    fn send_text(&self, payload: String) -> bool;

    /// Terminate a replaced, revoked, or expired socket.
    fn close(&self);
}

#[derive(Debug, thiserror::Error)]
pub enum EdgeSessionError {
    #[error("invalid Edge protocol message: {0}")]
    InvalidProtocol(String),
    #[error("the authenticated Edge scope does not match its hello")]
    ScopeMismatch,
    #[error("no Edge device `{0}` is connected for this scope")]
    NotConnected(String),
    #[error("Edge connection capacity reached")]
    CapacityReached,
    #[error("Edge capability `{capability}` operation `{operation}` is unavailable")]
    CapabilityUnavailable {
        capability: String,
        operation: String,
    },
    #[error("Edge capability generation changed")]
    CapabilityGenerationChanged,
    #[error("Edge execution grant does not name the selected device and workspace")]
    GrantScopeMismatch,
    #[error("Edge execution grant exceeds the advertised capability limits")]
    GrantLimitExceeded,
    #[error("Edge session reached its in-flight call limit")]
    InFlightLimit,
    #[error("duplicate Edge request id `{0}`")]
    DuplicateRequest(String),
    #[error("unknown or already completed Edge request id `{0}`")]
    UnknownRequest(String),
    #[error("the Edge session generation is stale")]
    StaleSession,
    #[error("the Edge socket cannot accept another message")]
    SocketUnavailable,
    #[error("Edge request timed out after {0:?}")]
    Timeout(Duration),
    #[error("Edge session disconnected: {0}")]
    Disconnected(String),
}

#[derive(Debug, Clone)]
pub struct EdgeInvocation {
    pub request_id: String,
    pub idempotency_key: String,
    pub deadline_at_ms: i64,
    pub grant: EdgeExecutionGrant,
    pub payload: Value,
}

/// Server-side request to one exact enrolled desktop. The registry mints the
/// wire grant from the live capability descriptor so callers never choose a
/// generation or increase byte limits themselves.
#[derive(Debug, Clone)]
pub struct EdgeDispatchRequest {
    pub execution_id: String,
    pub execution_epoch: u64,
    pub idempotency_key: String,
    pub capability: String,
    pub operation: String,
    pub payload: Value,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeSessionSnapshot {
    pub key: EdgeSessionKey,
    pub session_id: String,
    pub session_generation: u64,
    pub client_version: String,
    pub client_instance_id: String,
    pub capabilities: Vec<EdgeCapabilityDescriptor>,
}

struct PendingCall {
    capability: String,
    capability_generation: u64,
    max_response_bytes: u64,
    sender: oneshot::Sender<Result<EdgeCallResult, EdgeSessionError>>,
}

struct EdgeConnection {
    key: EdgeSessionKey,
    session_id: String,
    generation: u64,
    client_version: String,
    client_instance_id: String,
    capabilities: RwLock<HashMap<String, EdgeCapabilityDescriptor>>,
    lease_deadline: Mutex<Instant>,
    pending: Mutex<HashMap<String, PendingCall>>,
    sink: Arc<dyn EdgeSessionSink>,
}

impl EdgeConnection {
    fn matches(&self, session_id: &str, generation: u64) -> bool {
        self.session_id == session_id && self.generation == generation
    }

    fn snapshot(&self) -> EdgeSessionSnapshot {
        let mut capabilities = self
            .capabilities
            .read()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        capabilities.sort_by(|left, right| left.capability.cmp(&right.capability));
        EdgeSessionSnapshot {
            key: self.key.clone(),
            session_id: self.session_id.clone(),
            session_generation: self.generation,
            client_version: self.client_version.clone(),
            client_instance_id: self.client_instance_id.clone(),
            capabilities,
        }
    }

    fn retire(&self, reason: &str, close_socket: bool) {
        let pending = self.pending.lock().drain().collect::<Vec<_>>();
        for (_, call) in pending {
            let _ = call
                .sender
                .send(Err(EdgeSessionError::Disconnected(reason.to_owned())));
        }
        if close_socket {
            self.sink.close();
        }
    }
}

struct EdgeSessionRegistryInner {
    sessions: HashMap<EdgeSessionKey, Arc<EdgeConnection>>,
}

pub struct EdgeSessionRegistry {
    inner: RwLock<EdgeSessionRegistryInner>,
    next_generation: AtomicU64,
}

impl Default for EdgeSessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl EdgeSessionRegistry {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(EdgeSessionRegistryInner {
                sessions: HashMap::new(),
            }),
            next_generation: AtomicU64::new(1),
        }
    }

    /// Admit an already-authenticated outbound socket. The authenticated key,
    /// rather than hello fields, supplies principal/workspace authority.
    pub fn connect(
        &self,
        key: EdgeSessionKey,
        hello: EdgeHello,
        sink: Arc<dyn EdgeSessionSink>,
        now_ms: i64,
        now: Instant,
    ) -> Result<EdgeSessionAccepted, EdgeSessionError> {
        key.validate()?;
        hello
            .validate()
            .map_err(|error| EdgeSessionError::InvalidProtocol(error.to_string()))?;
        if hello.workspace_id != key.workspace || hello.device_id != key.device_id {
            return Err(EdgeSessionError::ScopeMismatch);
        }
        let capabilities = hello
            .capabilities
            .iter()
            .cloned()
            .map(|descriptor| (descriptor.capability.clone(), descriptor))
            .collect::<HashMap<_, _>>();
        let generation = self
            .next_generation
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                current.checked_add(1)
            })
            .map_err(|_| EdgeSessionError::CapacityReached)?;
        let accepted = EdgeSessionAccepted {
            protocol_version: EDGE_PROTOCOL_VERSION,
            session_id: Uuid::new_v4().to_string(),
            session_generation: generation,
            lease_duration_ms: EDGE_LEASE_DURATION.as_millis() as u64,
            heartbeat_interval_ms: EDGE_HEARTBEAT_INTERVAL.as_millis() as u64,
            max_in_flight_calls: EDGE_MAX_IN_FLIGHT_CALLS,
            server_time_ms: now_ms,
        };
        accepted
            .validate()
            .map_err(|error| EdgeSessionError::InvalidProtocol(error.to_string()))?;
        let frame = serde_json::to_string(&EdgeServerMessage::SessionAccepted(accepted.clone()))
            .map_err(|error| EdgeSessionError::InvalidProtocol(error.to_string()))?;

        let connection = Arc::new(EdgeConnection {
            key: key.clone(),
            session_id: accepted.session_id.clone(),
            generation,
            client_version: hello.client_version,
            client_instance_id: hello.client_instance_id,
            capabilities: RwLock::new(capabilities),
            lease_deadline: Mutex::new(now + EDGE_LEASE_DURATION),
            pending: Mutex::new(HashMap::new()),
            sink: Arc::clone(&sink),
        });

        let previous = {
            let mut inner = self.inner.write();
            let replacing = inner.sessions.contains_key(&key);
            let scope_count = inner
                .sessions
                .keys()
                .filter(|candidate| {
                    candidate.principal == key.principal && candidate.workspace == key.workspace
                })
                .count();
            if !replacing
                && (inner.sessions.len() >= MAX_EDGE_SESSIONS_GLOBAL
                    || scope_count >= MAX_EDGE_SESSIONS_PER_SCOPE)
            {
                return Err(EdgeSessionError::CapacityReached);
            }
            if !sink.send_text(frame) {
                return Err(EdgeSessionError::SocketUnavailable);
            }
            inner.sessions.insert(key, connection)
        };
        if let Some(previous) = previous {
            previous.retire("replaced by a newer Edge connection", true);
        }
        Ok(accepted)
    }

    /// Remove only the exact socket generation that stopped. A stale actor
    /// cannot deregister its replacement.
    pub fn disconnect(&self, key: &EdgeSessionKey, session_id: &str, generation: u64) -> bool {
        let removed = {
            let mut inner = self.inner.write();
            if inner
                .sessions
                .get(key)
                .is_some_and(|connection| connection.matches(session_id, generation))
            {
                inner.sessions.remove(key)
            } else {
                None
            }
        };
        if let Some(connection) = removed {
            connection.retire("socket disconnected", false);
            true
        } else {
            false
        }
    }

    pub fn revoke(&self, key: &EdgeSessionKey) -> bool {
        let removed = self.inner.write().sessions.remove(key);
        if let Some(connection) = removed {
            connection.retire("device credential revoked", true);
            true
        } else {
            false
        }
    }

    pub fn snapshot(&self, key: &EdgeSessionKey, now: Instant) -> Option<EdgeSessionSnapshot> {
        let connection = self.inner.read().sessions.get(key).cloned()?;
        if *connection.lease_deadline.lock() <= now {
            return None;
        }
        Some(connection.snapshot())
    }

    pub fn snapshots_for_scope(
        &self,
        principal: &str,
        workspace: &str,
        now: Instant,
    ) -> Vec<EdgeSessionSnapshot> {
        let mut snapshots = self
            .inner
            .read()
            .sessions
            .values()
            .filter(|connection| {
                connection.key.principal == principal
                    && connection.key.workspace == workspace
                    && *connection.lease_deadline.lock() > now
            })
            .map(|connection| connection.snapshot())
            .collect::<Vec<_>>();
        snapshots.sort_by(|left, right| left.key.device_id.cmp(&right.key.device_id));
        snapshots
    }

    pub fn eligible(
        &self,
        key: &EdgeSessionKey,
        capability: &str,
        operation: &str,
        now: Instant,
    ) -> Option<EdgeCapabilityDescriptor> {
        let connection = self.inner.read().sessions.get(key).cloned()?;
        if *connection.lease_deadline.lock() <= now {
            return None;
        }
        let descriptor = connection
            .capabilities
            .read()
            .get(capability)
            .filter(|descriptor| descriptor.supports(capability, operation))
            .cloned();
        descriptor
    }

    pub fn expire_leases(&self, now: Instant) -> usize {
        let expired = {
            let inner = self.inner.read();
            inner
                .sessions
                .iter()
                .filter(|(_, connection)| *connection.lease_deadline.lock() <= now)
                .map(|(key, connection)| (key.clone(), Arc::clone(connection)))
                .collect::<Vec<_>>()
        };
        let mut retired = Vec::new();
        {
            let mut inner = self.inner.write();
            for (key, expected) in expired {
                if inner
                    .sessions
                    .get(&key)
                    .is_some_and(|current| Arc::ptr_eq(current, &expected))
                {
                    inner.sessions.remove(&key);
                    retired.push(expected);
                }
            }
        }
        for connection in &retired {
            connection.retire("Edge lease expired", true);
        }
        retired.len()
    }

    pub fn receive_client_message(
        &self,
        key: &EdgeSessionKey,
        message: EdgeClientMessage,
        now: Instant,
    ) -> Result<(), EdgeSessionError> {
        let connection = self.current(key)?;
        match message {
            EdgeClientMessage::Hello(_) => Err(EdgeSessionError::InvalidProtocol(
                "hello is valid only as the first socket message".to_owned(),
            )),
            EdgeClientMessage::Heartbeat(heartbeat) => {
                self.receive_heartbeat(&connection, heartbeat, now)
            },
            EdgeClientMessage::CapabilityUpdate(update) => {
                self.receive_capability_update(&connection, update)
            },
            EdgeClientMessage::Result(result) => self.receive_result(&connection, result),
        }
    }

    pub async fn invoke(
        &self,
        key: &EdgeSessionKey,
        invocation: EdgeInvocation,
        now_ms: i64,
    ) -> Result<EdgeCallResult, EdgeSessionError> {
        let connection = self.current(key)?;
        if *connection.lease_deadline.lock() <= Instant::now() {
            return Err(EdgeSessionError::NotConnected(key.device_id.clone()));
        }
        let descriptor = connection
            .capabilities
            .read()
            .get(&invocation.grant.capability)
            .filter(|descriptor| {
                descriptor.supports(&invocation.grant.capability, &invocation.grant.operation)
            })
            .cloned()
            .ok_or_else(|| EdgeSessionError::CapabilityUnavailable {
                capability: invocation.grant.capability.clone(),
                operation: invocation.grant.operation.clone(),
            })?;
        self.validate_grant(key, &descriptor, &invocation.grant, now_ms)?;

        let request = EdgeInvoke {
            session_id: connection.session_id.clone(),
            session_generation: connection.generation,
            request_id: invocation.request_id.clone(),
            idempotency_key: invocation.idempotency_key,
            deadline_at_ms: invocation.deadline_at_ms,
            capability: invocation.grant.capability.clone(),
            operation: invocation.grant.operation.clone(),
            grant: invocation.grant,
            payload: invocation.payload,
        };
        request
            .validate(now_ms)
            .map_err(|error| EdgeSessionError::InvalidProtocol(error.to_string()))?;
        let frame = serde_json::to_string(&EdgeServerMessage::Invoke(request.clone()))
            .map_err(|error| EdgeSessionError::InvalidProtocol(error.to_string()))?;
        let (sender, receiver) = oneshot::channel();
        {
            let capabilities = connection.capabilities.read();
            if capabilities
                .get(&request.capability)
                .is_none_or(|current| current.generation != descriptor.generation)
            {
                return Err(EdgeSessionError::CapabilityGenerationChanged);
            }
            let mut pending = connection.pending.lock();
            if pending.contains_key(&request.request_id) {
                return Err(EdgeSessionError::DuplicateRequest(request.request_id));
            }
            let capability_in_flight = pending
                .values()
                .filter(|call| call.capability == request.capability)
                .count();
            if pending.len() >= EDGE_MAX_IN_FLIGHT_CALLS as usize
                || capability_in_flight >= descriptor.max_in_flight as usize
            {
                return Err(EdgeSessionError::InFlightLimit);
            }
            pending.insert(
                request.request_id.clone(),
                PendingCall {
                    capability: request.capability.clone(),
                    capability_generation: descriptor.generation,
                    max_response_bytes: request.grant.max_response_bytes,
                    sender,
                },
            );
        }
        if !self.is_current(key, &connection) || !connection.sink.send_text(frame) {
            connection.pending.lock().remove(&request.request_id);
            return Err(EdgeSessionError::SocketUnavailable);
        }

        let wait = Duration::from_millis(
            request
                .deadline_at_ms
                .saturating_sub(now_ms)
                .try_into()
                .unwrap_or(u64::MAX),
        );
        match tokio::time::timeout(wait, receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(EdgeSessionError::Disconnected(
                "result channel closed".to_owned(),
            )),
            Err(_) => {
                connection.pending.lock().remove(&request.request_id);
                let cancel = EdgeCancel {
                    session_id: connection.session_id.clone(),
                    session_generation: connection.generation,
                    request_id: request.request_id,
                    reason: "deadline_expired".to_owned(),
                };
                if let Ok(frame) = serde_json::to_string(&EdgeServerMessage::Cancel(cancel)) {
                    let _ = connection.sink.send_text(frame);
                }
                Err(EdgeSessionError::Timeout(wait))
            },
        }
    }

    /// Mint a short-lived grant from the current advertised descriptor and
    /// invoke one operation on an exact scoped device. This is the production
    /// dispatcher entry point used by API/tool adapters; it deliberately does
    /// not accept caller-supplied grants or capability limits.
    pub async fn dispatch(
        &self,
        key: &EdgeSessionKey,
        request: EdgeDispatchRequest,
        now_ms: i64,
    ) -> Result<EdgeCallResult, EdgeSessionError> {
        if !(EDGE_MIN_DISPATCH_TIMEOUT_MS..=EDGE_MAX_DISPATCH_TIMEOUT_MS)
            .contains(&request.timeout_ms)
        {
            return Err(EdgeSessionError::InvalidProtocol(format!(
                "Edge dispatch timeout must be in {EDGE_MIN_DISPATCH_TIMEOUT_MS}..={EDGE_MAX_DISPATCH_TIMEOUT_MS} ms"
            )));
        }
        let descriptor = self
            .eligible(key, &request.capability, &request.operation, Instant::now())
            .ok_or_else(|| EdgeSessionError::CapabilityUnavailable {
                capability: request.capability.clone(),
                operation: request.operation.clone(),
            })?;
        let timeout_ms = i64::try_from(request.timeout_ms).map_err(|_| {
            EdgeSessionError::InvalidProtocol("Edge dispatch timeout is outside range".into())
        })?;
        let deadline_at_ms = now_ms.checked_add(timeout_ms).ok_or_else(|| {
            EdgeSessionError::InvalidProtocol("Edge dispatch deadline overflowed".into())
        })?;
        let request_id = Uuid::new_v4().to_string();
        let grant = EdgeExecutionGrant {
            grant_id: Uuid::new_v4().to_string(),
            workspace_id: key.workspace.clone(),
            device_id: key.device_id.clone(),
            execution_id: request.execution_id,
            execution_epoch: request.execution_epoch,
            capability: descriptor.capability.clone(),
            capability_generation: descriptor.generation,
            operation: request.operation,
            issued_at_ms: now_ms,
            expires_at_ms: deadline_at_ms,
            max_request_bytes: descriptor.max_request_bytes,
            max_response_bytes: descriptor.max_response_bytes,
        };
        self.invoke(
            key,
            EdgeInvocation {
                request_id,
                idempotency_key: request.idempotency_key,
                deadline_at_ms,
                grant,
                payload: request.payload,
            },
            now_ms,
        )
        .await
    }

    fn current(&self, key: &EdgeSessionKey) -> Result<Arc<EdgeConnection>, EdgeSessionError> {
        self.inner
            .read()
            .sessions
            .get(key)
            .cloned()
            .ok_or_else(|| EdgeSessionError::NotConnected(key.device_id.clone()))
    }

    fn is_current(&self, key: &EdgeSessionKey, expected: &Arc<EdgeConnection>) -> bool {
        self.inner
            .read()
            .sessions
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, expected))
    }

    fn validate_grant(
        &self,
        key: &EdgeSessionKey,
        descriptor: &EdgeCapabilityDescriptor,
        grant: &EdgeExecutionGrant,
        now_ms: i64,
    ) -> Result<(), EdgeSessionError> {
        grant
            .validate(now_ms)
            .map_err(|error| EdgeSessionError::InvalidProtocol(error.to_string()))?;
        if grant.workspace_id != key.workspace || grant.device_id != key.device_id {
            return Err(EdgeSessionError::GrantScopeMismatch);
        }
        if grant.capability_generation != descriptor.generation {
            return Err(EdgeSessionError::CapabilityGenerationChanged);
        }
        if grant.max_request_bytes > descriptor.max_request_bytes
            || grant.max_response_bytes > descriptor.max_response_bytes
        {
            return Err(EdgeSessionError::GrantLimitExceeded);
        }
        Ok(())
    }

    fn receive_heartbeat(
        &self,
        connection: &Arc<EdgeConnection>,
        heartbeat: EdgeHeartbeat,
        now: Instant,
    ) -> Result<(), EdgeSessionError> {
        heartbeat
            .validate()
            .map_err(|error| EdgeSessionError::InvalidProtocol(error.to_string()))?;
        if !connection.matches(&heartbeat.session_id, heartbeat.session_generation) {
            return Err(EdgeSessionError::StaleSession);
        }
        *connection.lease_deadline.lock() = now + EDGE_LEASE_DURATION;
        Ok(())
    }

    fn receive_capability_update(
        &self,
        connection: &Arc<EdgeConnection>,
        update: EdgeCapabilityUpdate,
    ) -> Result<(), EdgeSessionError> {
        update
            .validate()
            .map_err(|error| EdgeSessionError::InvalidProtocol(error.to_string()))?;
        if !connection.matches(&update.session_id, update.session_generation) {
            return Err(EdgeSessionError::StaleSession);
        }
        let next = update
            .capabilities
            .into_iter()
            .map(|descriptor| (descriptor.capability.clone(), descriptor))
            .collect::<HashMap<_, _>>();
        let invalidated = {
            let mut capabilities = connection.capabilities.write();
            let mut pending = connection.pending.lock();
            let request_ids = pending
                .iter()
                .filter(|(_, call)| {
                    next.get(&call.capability).is_none_or(|descriptor| {
                        descriptor.generation != call.capability_generation
                    })
                })
                .map(|(request_id, _)| request_id.clone())
                .collect::<Vec<_>>();
            let invalidated = request_ids
                .into_iter()
                .filter_map(|request_id| pending.remove(&request_id))
                .collect::<Vec<_>>();
            *capabilities = next;
            invalidated
        };
        for call in invalidated {
            let _ = call
                .sender
                .send(Err(EdgeSessionError::CapabilityGenerationChanged));
        }
        Ok(())
    }

    fn receive_result(
        &self,
        connection: &Arc<EdgeConnection>,
        result: EdgeCallResult,
    ) -> Result<(), EdgeSessionError> {
        if !connection.matches(&result.session_id, result.session_generation) {
            return Err(EdgeSessionError::StaleSession);
        }
        let mut pending = connection.pending.lock();
        let call = pending
            .get(&result.request_id)
            .ok_or_else(|| EdgeSessionError::UnknownRequest(result.request_id.clone()))?;
        result
            .validate(call.max_response_bytes)
            .map_err(|error| EdgeSessionError::InvalidProtocol(error.to_string()))?;
        let call = pending
            .remove(&result.request_id)
            .ok_or_else(|| EdgeSessionError::UnknownRequest(result.request_id.clone()))?;
        let _ = call.sender.send(Ok(result));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime_core::edge::{EdgeCallError, EdgeCallStatus};
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
    use tokio::sync::Notify;

    const NOW_MS: i64 = 1_800_000_000_000;

    #[derive(Default)]
    struct FakeSink {
        frames: Mutex<Vec<String>>,
        closed: AtomicBool,
        sent: Notify,
    }

    impl EdgeSessionSink for FakeSink {
        fn send_text(&self, payload: String) -> bool {
            if self.closed.load(AtomicOrdering::SeqCst) {
                return false;
            }
            self.frames.lock().push(payload);
            self.sent.notify_one();
            true
        }

        fn close(&self) {
            self.closed.store(true, AtomicOrdering::SeqCst);
            self.sent.notify_one();
        }
    }

    impl FakeSink {
        fn take_frames(&self) -> Vec<String> {
            self.frames.lock().drain(..).collect()
        }

        async fn next_frame(&self) -> String {
            loop {
                let notified = self.sent.notified();
                if let Some(frame) = self.frames.lock().pop() {
                    return frame;
                }
                notified.await;
            }
        }
    }

    fn key() -> EdgeSessionKey {
        EdgeSessionKey::new("owner", "default", "desktop-1")
    }

    fn capability(generation: u64) -> EdgeCapabilityDescriptor {
        EdgeCapabilityDescriptor {
            capability: "screen.capture".into(),
            generation,
            operations: vec!["capture".into()],
            max_request_bytes: 8 * 1024,
            max_response_bytes: 8 * 1024 * 1024,
            max_in_flight: 2,
        }
    }

    fn hello(instance: &str, generation: u64) -> EdgeHello {
        EdgeHello {
            protocol_version: EDGE_PROTOCOL_VERSION,
            device_id: "desktop-1".into(),
            workspace_id: "default".into(),
            client_version: "0.1.0".into(),
            client_instance_id: instance.into(),
            capabilities: vec![capability(generation)],
        }
    }

    fn invocation(
        request_id: &str,
        capability_generation: u64,
        deadline_at_ms: i64,
    ) -> EdgeInvocation {
        EdgeInvocation {
            request_id: request_id.into(),
            idempotency_key: format!("execution-1:{request_id}"),
            deadline_at_ms,
            grant: EdgeExecutionGrant {
                grant_id: format!("grant-{request_id}"),
                workspace_id: "default".into(),
                device_id: "desktop-1".into(),
                execution_id: "execution-1".into(),
                execution_epoch: 2,
                capability: "screen.capture".into(),
                capability_generation,
                operation: "capture".into(),
                issued_at_ms: NOW_MS - 1_000,
                expires_at_ms: deadline_at_ms + 1_000,
                max_request_bytes: 8 * 1024,
                max_response_bytes: 8 * 1024 * 1024,
            },
            payload: json!({"display_id": 1}),
        }
    }

    #[test]
    fn server_assigns_generation_and_replacement_fences_old_socket() {
        let registry = EdgeSessionRegistry::new();
        let first_sink = Arc::new(FakeSink::default());
        let first = registry
            .connect(
                key(),
                hello("first", 1),
                first_sink.clone(),
                NOW_MS,
                Instant::now(),
            )
            .unwrap();
        let second_sink = Arc::new(FakeSink::default());
        let second = registry
            .connect(
                key(),
                hello("second", 1),
                second_sink,
                NOW_MS + 1,
                Instant::now(),
            )
            .unwrap();

        assert!(second.session_generation > first.session_generation);
        assert!(first_sink.closed.load(AtomicOrdering::SeqCst));
        assert!(!registry.disconnect(&key(), &first.session_id, first.session_generation));
        assert_eq!(
            registry
                .snapshot(&key(), Instant::now())
                .unwrap()
                .session_id,
            second.session_id
        );
    }

    #[test]
    fn live_inventory_is_scope_bound_and_stably_sorted() {
        let registry = EdgeSessionRegistry::new();
        for device_id in ["desktop-z", "desktop-a"] {
            let scoped_key = EdgeSessionKey::new("owner", "default", device_id);
            let mut scoped_hello = hello(device_id, 1);
            scoped_hello.device_id = device_id.into();
            registry
                .connect(
                    scoped_key,
                    scoped_hello,
                    Arc::new(FakeSink::default()),
                    NOW_MS,
                    Instant::now(),
                )
                .unwrap();
        }
        let foreign_key = EdgeSessionKey::new("other", "default", "desktop-other");
        let mut foreign_hello = hello("other", 1);
        foreign_hello.device_id = "desktop-other".into();
        registry
            .connect(
                foreign_key,
                foreign_hello,
                Arc::new(FakeSink::default()),
                NOW_MS,
                Instant::now(),
            )
            .unwrap();

        let devices = registry.snapshots_for_scope("owner", "default", Instant::now());
        assert_eq!(
            devices
                .iter()
                .map(|snapshot| snapshot.key.device_id.as_str())
                .collect::<Vec<_>>(),
            vec!["desktop-a", "desktop-z"]
        );
    }

    #[tokio::test]
    async fn invocation_round_trip_is_correlated_to_exact_session() {
        let registry = Arc::new(EdgeSessionRegistry::new());
        let sink = Arc::new(FakeSink::default());
        let accepted = registry
            .connect(key(), hello("one", 4), sink.clone(), NOW_MS, Instant::now())
            .unwrap();
        sink.take_frames();

        let pending_registry = Arc::clone(&registry);
        let pending = tokio::spawn(async move {
            pending_registry
                .invoke(&key(), invocation("request-1", 4, NOW_MS + 30_000), NOW_MS)
                .await
        });
        let frame = sink.next_frame().await;
        let EdgeServerMessage::Invoke(request) = serde_json::from_str(&frame).unwrap() else {
            panic!("expected invoke frame");
        };
        assert_eq!(request.session_id, accepted.session_id);
        assert_eq!(request.grant.device_id, "desktop-1");

        registry
            .receive_client_message(
                &key(),
                EdgeClientMessage::Result(EdgeCallResult {
                    session_id: accepted.session_id,
                    session_generation: accepted.session_generation,
                    request_id: request.request_id,
                    status: EdgeCallStatus::Succeeded,
                    payload: Some(json!({"artifact_ref": "screen:1"})),
                    error: None,
                }),
                Instant::now(),
            )
            .unwrap();
        let result = pending.await.unwrap().unwrap();
        assert_eq!(result.status, EdgeCallStatus::Succeeded);
    }

    #[tokio::test]
    async fn dispatcher_mints_limits_from_the_live_capability() {
        let registry = Arc::new(EdgeSessionRegistry::new());
        let sink = Arc::new(FakeSink::default());
        let accepted = registry
            .connect(key(), hello("one", 4), sink.clone(), NOW_MS, Instant::now())
            .unwrap();
        sink.take_frames();

        let pending_registry = Arc::clone(&registry);
        let pending = tokio::spawn(async move {
            pending_registry
                .dispatch(
                    &key(),
                    EdgeDispatchRequest {
                        execution_id: "execution-typed".into(),
                        execution_epoch: 3,
                        idempotency_key: "execution-typed:screen".into(),
                        capability: "screen.capture".into(),
                        operation: "capture".into(),
                        payload: json!({"display_id": 1}),
                        timeout_ms: 30_000,
                    },
                    NOW_MS,
                )
                .await
        });
        let frame = sink.next_frame().await;
        let EdgeServerMessage::Invoke(request) = serde_json::from_str(&frame).unwrap() else {
            panic!("expected invoke frame");
        };
        assert_eq!(request.grant.capability_generation, 4);
        assert_eq!(request.grant.max_request_bytes, 8 * 1024);
        assert_eq!(request.grant.max_response_bytes, 8 * 1024 * 1024);
        assert_eq!(request.grant.execution_id, "execution-typed");

        registry
            .receive_client_message(
                &key(),
                EdgeClientMessage::Result(EdgeCallResult {
                    session_id: accepted.session_id,
                    session_generation: accepted.session_generation,
                    request_id: request.request_id,
                    status: EdgeCallStatus::Succeeded,
                    payload: Some(json!({"ok": true})),
                    error: None,
                }),
                Instant::now(),
            )
            .unwrap();
        assert_eq!(
            pending.await.unwrap().unwrap().status,
            EdgeCallStatus::Succeeded
        );
    }

    #[tokio::test]
    async fn dispatcher_rejects_unbounded_timeouts_before_sending() {
        let registry = EdgeSessionRegistry::new();
        let sink = Arc::new(FakeSink::default());
        registry
            .connect(key(), hello("one", 4), sink.clone(), NOW_MS, Instant::now())
            .unwrap();
        sink.take_frames();
        let result = registry
            .dispatch(
                &key(),
                EdgeDispatchRequest {
                    execution_id: "execution-typed".into(),
                    execution_epoch: 3,
                    idempotency_key: "execution-typed:screen".into(),
                    capability: "screen.capture".into(),
                    operation: "capture".into(),
                    payload: json!({}),
                    timeout_ms: EDGE_MAX_DISPATCH_TIMEOUT_MS + 1,
                },
                NOW_MS,
            )
            .await;
        assert!(matches!(result, Err(EdgeSessionError::InvalidProtocol(_))));
        assert!(sink.take_frames().is_empty());
    }

    #[tokio::test]
    async fn grant_must_match_device_capability_generation_and_limits() {
        let registry = EdgeSessionRegistry::new();
        let sink = Arc::new(FakeSink::default());
        registry
            .connect(key(), hello("one", 4), sink.clone(), NOW_MS, Instant::now())
            .unwrap();
        sink.take_frames();

        let error = registry
            .invoke(&key(), invocation("stale", 3, NOW_MS + 1_000), NOW_MS)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            EdgeSessionError::CapabilityGenerationChanged
        ));

        let mut wrong_device = invocation("wrong-device", 4, NOW_MS + 1_000);
        wrong_device.grant.device_id = "somebody-else".into();
        assert!(matches!(
            registry.invoke(&key(), wrong_device, NOW_MS).await,
            Err(EdgeSessionError::GrantScopeMismatch)
        ));
        assert!(sink.take_frames().is_empty());
    }

    #[tokio::test]
    async fn capability_rotation_fails_calls_bound_to_old_generation() {
        let registry = Arc::new(EdgeSessionRegistry::new());
        let sink = Arc::new(FakeSink::default());
        let accepted = registry
            .connect(key(), hello("one", 4), sink.clone(), NOW_MS, Instant::now())
            .unwrap();
        sink.take_frames();

        let pending_registry = Arc::clone(&registry);
        let pending = tokio::spawn(async move {
            pending_registry
                .invoke(&key(), invocation("request-1", 4, NOW_MS + 30_000), NOW_MS)
                .await
        });
        let _ = sink.next_frame().await;
        registry
            .receive_client_message(
                &key(),
                EdgeClientMessage::CapabilityUpdate(EdgeCapabilityUpdate {
                    session_id: accepted.session_id,
                    session_generation: accepted.session_generation,
                    capabilities: vec![capability(5)],
                }),
                Instant::now(),
            )
            .unwrap();
        assert!(matches!(
            pending.await.unwrap(),
            Err(EdgeSessionError::CapabilityGenerationChanged)
        ));
    }

    #[test]
    fn lease_expiry_removes_and_closes_session() {
        let registry = EdgeSessionRegistry::new();
        let sink = Arc::new(FakeSink::default());
        let start = Instant::now();
        registry
            .connect(key(), hello("one", 1), sink.clone(), NOW_MS, start)
            .unwrap();
        assert_eq!(
            registry.expire_leases(start + EDGE_LEASE_DURATION + Duration::from_millis(1)),
            1
        );
        assert!(sink.closed.load(AtomicOrdering::SeqCst));
        assert!(registry.snapshot(&key(), Instant::now()).is_none());
    }

    #[test]
    fn failed_result_requires_typed_error() {
        let result = EdgeCallResult {
            session_id: "session".into(),
            session_generation: 1,
            request_id: "request".into(),
            status: EdgeCallStatus::Failed,
            payload: None,
            error: Some(EdgeCallError {
                code: "host_permission_denied".into(),
                message: "Screen Recording permission is disabled".into(),
                retryable: false,
            }),
        };
        result.validate(1024).unwrap();
    }
}
