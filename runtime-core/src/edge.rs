//! Shared Magician Edge wire vocabulary.
//!
//! The desktop connects out to a selected Magician engine. The engine can then
//! invoke only capabilities advertised by that particular session and covered
//! by a short-lived execution grant. This module deliberately contains no
//! socket, credential-store, or operating-system code so the cloud service,
//! desktop, and protocol conformance tests can all use the same contract.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fmt;

pub const EDGE_PROTOCOL_VERSION: u16 = 1;
pub const EDGE_MAX_CAPABILITIES: usize = 64;
pub const EDGE_MAX_OPERATIONS_PER_CAPABILITY: usize = 64;
pub const EDGE_MAX_IDENTIFIER_BYTES: usize = 160;
pub const EDGE_MAX_ERROR_MESSAGE_BYTES: usize = 4 * 1024;
pub const EDGE_ABSOLUTE_MAX_PAYLOAD_BYTES: u64 = 48 * 1024 * 1024;
pub const EDGE_MAX_IN_FLIGHT_CALLS: u16 = 64;
pub const EDGE_MIN_LEASE_MS: u64 = 5_000;
pub const EDGE_MAX_LEASE_MS: u64 = 5 * 60 * 1_000;

/// Stable capability names shared by the Edge server and desktop dispatcher.
/// Individual CUA tools and CDP verbs remain bounded operations beneath these
/// names rather than becoming ambient remote endpoints.
pub const EDGE_CAPABILITY_CUA: &str = "host.cua";
pub const EDGE_CAPABILITY_BROWSER_CDP: &str = "browser.cdp";
pub const EDGE_CAPABILITY_IMESSAGE: &str = "host.imessage";
pub const EDGE_CAPABILITY_ANDROID_OBSERVATION: &str = "host.android-observation";

pub const EDGE_BROWSER_OPERATIONS: &[&str] = &["health", "version", "targets", "command"];
pub const EDGE_IMESSAGE_OPERATIONS: &[&str] = &["query"];
pub const EDGE_ANDROID_OBSERVATION_OPERATIONS: &[&str] = &["open-settings"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeProtocolError(String);

impl EdgeProtocolError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for EdgeProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for EdgeProtocolError {}

pub type EdgeProtocolResult<T = ()> = Result<T, EdgeProtocolError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeCapabilityDescriptor {
    pub capability: String,
    pub generation: u64,
    pub operations: Vec<String>,
    pub max_request_bytes: u64,
    pub max_response_bytes: u64,
    pub max_in_flight: u16,
}

impl EdgeCapabilityDescriptor {
    pub fn validate(&self) -> EdgeProtocolResult {
        validate_token("capability", &self.capability)?;
        if self.generation == 0 {
            return Err(EdgeProtocolError::new(
                "capability generation must be greater than zero",
            ));
        }
        if self.operations.is_empty() || self.operations.len() > EDGE_MAX_OPERATIONS_PER_CAPABILITY
        {
            return Err(EdgeProtocolError::new(format!(
                "capability operations must contain 1..={EDGE_MAX_OPERATIONS_PER_CAPABILITY} entries"
            )));
        }
        let mut operations = BTreeSet::new();
        for operation in &self.operations {
            validate_token("operation", operation)?;
            if !operations.insert(operation) {
                return Err(EdgeProtocolError::new(format!(
                    "duplicate capability operation: {operation}"
                )));
            }
        }
        validate_payload_limit("max_request_bytes", self.max_request_bytes)?;
        validate_payload_limit("max_response_bytes", self.max_response_bytes)?;
        if !(1..=EDGE_MAX_IN_FLIGHT_CALLS).contains(&self.max_in_flight) {
            return Err(EdgeProtocolError::new(format!(
                "max_in_flight must be in 1..={EDGE_MAX_IN_FLIGHT_CALLS}"
            )));
        }
        Ok(())
    }

    pub fn supports(&self, capability: &str, operation: &str) -> bool {
        self.capability == capability
            && self
                .operations
                .iter()
                .any(|candidate| candidate == operation)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeHello {
    pub protocol_version: u16,
    pub device_id: String,
    pub workspace_id: String,
    pub client_version: String,
    pub client_instance_id: String,
    pub capabilities: Vec<EdgeCapabilityDescriptor>,
}

impl EdgeHello {
    pub fn validate(&self) -> EdgeProtocolResult {
        validate_protocol_version(self.protocol_version)?;
        validate_identifier("device_id", &self.device_id)?;
        validate_identifier("workspace_id", &self.workspace_id)?;
        validate_identifier("client_version", &self.client_version)?;
        validate_identifier("client_instance_id", &self.client_instance_id)?;
        if self.capabilities.len() > EDGE_MAX_CAPABILITIES {
            return Err(EdgeProtocolError::new(format!(
                "capabilities exceed the {EDGE_MAX_CAPABILITIES}-entry limit"
            )));
        }
        let mut names = BTreeSet::new();
        for capability in &self.capabilities {
            capability.validate()?;
            if !names.insert(&capability.capability) {
                return Err(EdgeProtocolError::new(format!(
                    "duplicate capability: {}",
                    capability.capability
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeSessionAccepted {
    pub protocol_version: u16,
    pub session_id: String,
    pub session_generation: u64,
    pub lease_duration_ms: u64,
    pub heartbeat_interval_ms: u64,
    pub max_in_flight_calls: u16,
    pub server_time_ms: i64,
}

impl EdgeSessionAccepted {
    pub fn validate(&self) -> EdgeProtocolResult {
        validate_protocol_version(self.protocol_version)?;
        validate_identifier("session_id", &self.session_id)?;
        if self.session_generation == 0 {
            return Err(EdgeProtocolError::new(
                "session_generation must be greater than zero",
            ));
        }
        if !(EDGE_MIN_LEASE_MS..=EDGE_MAX_LEASE_MS).contains(&self.lease_duration_ms) {
            return Err(EdgeProtocolError::new(format!(
                "lease_duration_ms must be in {EDGE_MIN_LEASE_MS}..={EDGE_MAX_LEASE_MS}"
            )));
        }
        if self.heartbeat_interval_ms == 0
            || self.heartbeat_interval_ms > self.lease_duration_ms / 2
        {
            return Err(EdgeProtocolError::new(
                "heartbeat_interval_ms must be positive and no more than half the lease",
            ));
        }
        if !(1..=EDGE_MAX_IN_FLIGHT_CALLS).contains(&self.max_in_flight_calls) {
            return Err(EdgeProtocolError::new(format!(
                "max_in_flight_calls must be in 1..={EDGE_MAX_IN_FLIGHT_CALLS}"
            )));
        }
        if self.server_time_ms <= 0 {
            return Err(EdgeProtocolError::new(
                "server_time_ms must be greater than zero",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeHeartbeat {
    pub session_id: String,
    pub session_generation: u64,
    pub sent_at_ms: i64,
}

impl EdgeHeartbeat {
    pub fn validate(&self) -> EdgeProtocolResult {
        validate_identifier("session_id", &self.session_id)?;
        if self.session_generation == 0 {
            return Err(EdgeProtocolError::new(
                "session_generation must be greater than zero",
            ));
        }
        if self.sent_at_ms <= 0 {
            return Err(EdgeProtocolError::new(
                "heartbeat sent_at_ms must be greater than zero",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeCapabilityUpdate {
    pub session_id: String,
    pub session_generation: u64,
    pub capabilities: Vec<EdgeCapabilityDescriptor>,
}

impl EdgeCapabilityUpdate {
    pub fn validate(&self) -> EdgeProtocolResult {
        EdgeHeartbeat {
            session_id: self.session_id.clone(),
            session_generation: self.session_generation,
            sent_at_ms: 1,
        }
        .validate()?;
        if self.capabilities.len() > EDGE_MAX_CAPABILITIES {
            return Err(EdgeProtocolError::new(format!(
                "capabilities exceed the {EDGE_MAX_CAPABILITIES}-entry limit"
            )));
        }
        let mut names = BTreeSet::new();
        for capability in &self.capabilities {
            capability.validate()?;
            if !names.insert(&capability.capability) {
                return Err(EdgeProtocolError::new(format!(
                    "duplicate capability: {}",
                    capability.capability
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeExecutionGrant {
    pub grant_id: String,
    pub workspace_id: String,
    pub device_id: String,
    pub execution_id: String,
    pub execution_epoch: u64,
    pub capability: String,
    pub capability_generation: u64,
    pub operation: String,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    pub max_request_bytes: u64,
    pub max_response_bytes: u64,
}

impl EdgeExecutionGrant {
    pub fn validate(&self, now_ms: i64) -> EdgeProtocolResult {
        validate_identifier("grant_id", &self.grant_id)?;
        validate_identifier("workspace_id", &self.workspace_id)?;
        validate_identifier("device_id", &self.device_id)?;
        validate_identifier("execution_id", &self.execution_id)?;
        validate_token("capability", &self.capability)?;
        validate_token("operation", &self.operation)?;
        if self.execution_epoch == 0 {
            return Err(EdgeProtocolError::new(
                "execution_epoch must be greater than zero",
            ));
        }
        if self.capability_generation == 0 {
            return Err(EdgeProtocolError::new(
                "capability_generation must be greater than zero",
            ));
        }
        if self.issued_at_ms <= 0 || self.expires_at_ms <= self.issued_at_ms {
            return Err(EdgeProtocolError::new(
                "execution grant expiry must be after its positive issue time",
            ));
        }
        if self.expires_at_ms <= now_ms {
            return Err(EdgeProtocolError::new("execution grant has expired"));
        }
        validate_payload_limit("max_request_bytes", self.max_request_bytes)?;
        validate_payload_limit("max_response_bytes", self.max_response_bytes)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeInvoke {
    pub session_id: String,
    pub session_generation: u64,
    pub request_id: String,
    pub idempotency_key: String,
    pub deadline_at_ms: i64,
    pub grant: EdgeExecutionGrant,
    pub capability: String,
    pub operation: String,
    pub payload: Value,
}

impl EdgeInvoke {
    pub fn validate(&self, now_ms: i64) -> EdgeProtocolResult {
        validate_identifier("session_id", &self.session_id)?;
        validate_identifier("request_id", &self.request_id)?;
        validate_identifier("idempotency_key", &self.idempotency_key)?;
        if self.session_generation == 0 {
            return Err(EdgeProtocolError::new(
                "session_generation must be greater than zero",
            ));
        }
        self.grant.validate(now_ms)?;
        validate_token("capability", &self.capability)?;
        validate_token("operation", &self.operation)?;
        if self.capability != self.grant.capability || self.operation != self.grant.operation {
            return Err(EdgeProtocolError::new(
                "invocation capability and operation must match its execution grant",
            ));
        }
        if self.deadline_at_ms <= now_ms || self.deadline_at_ms > self.grant.expires_at_ms {
            return Err(EdgeProtocolError::new(
                "invocation deadline must be in the future and within its execution grant",
            ));
        }
        validate_json_size(
            "invocation payload",
            &self.payload,
            self.grant.max_request_bytes,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeCallStatus {
    Succeeded,
    Failed,
    Unavailable,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeCallError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl EdgeCallError {
    pub fn validate(&self) -> EdgeProtocolResult {
        validate_token("error code", &self.code)?;
        if self.message.trim().is_empty() || self.message.len() > EDGE_MAX_ERROR_MESSAGE_BYTES {
            return Err(EdgeProtocolError::new(format!(
                "error message must contain 1..={EDGE_MAX_ERROR_MESSAGE_BYTES} bytes"
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeCallResult {
    pub session_id: String,
    pub session_generation: u64,
    pub request_id: String,
    pub status: EdgeCallStatus,
    pub payload: Option<Value>,
    pub error: Option<EdgeCallError>,
}

impl EdgeCallResult {
    pub fn validate(&self, max_response_bytes: u64) -> EdgeProtocolResult {
        validate_identifier("session_id", &self.session_id)?;
        validate_identifier("request_id", &self.request_id)?;
        if self.session_generation == 0 {
            return Err(EdgeProtocolError::new(
                "session_generation must be greater than zero",
            ));
        }
        validate_payload_limit("max_response_bytes", max_response_bytes)?;
        match self.status {
            EdgeCallStatus::Succeeded if self.error.is_some() => {
                return Err(EdgeProtocolError::new(
                    "a successful capability result cannot contain an error",
                ));
            },
            EdgeCallStatus::Failed | EdgeCallStatus::Unavailable | EdgeCallStatus::Cancelled
                if self.error.is_none() =>
            {
                return Err(EdgeProtocolError::new(
                    "a non-success capability result must contain an error",
                ));
            },
            _ => {},
        }
        if let Some(error) = &self.error {
            error.validate()?;
        }
        if let Some(payload) = &self.payload {
            validate_json_size("capability result payload", payload, max_response_bytes)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeCancel {
    pub session_id: String,
    pub session_generation: u64,
    pub request_id: String,
    pub reason: String,
}

impl EdgeCancel {
    pub fn validate(&self) -> EdgeProtocolResult {
        validate_identifier("session_id", &self.session_id)?;
        validate_identifier("request_id", &self.request_id)?;
        if self.session_generation == 0 {
            return Err(EdgeProtocolError::new(
                "session_generation must be greater than zero",
            ));
        }
        if self.reason.trim().is_empty() || self.reason.len() > EDGE_MAX_ERROR_MESSAGE_BYTES {
            return Err(EdgeProtocolError::new(format!(
                "cancellation reason must contain 1..={EDGE_MAX_ERROR_MESSAGE_BYTES} bytes"
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "body", rename_all = "snake_case")]
pub enum EdgeClientMessage {
    Hello(EdgeHello),
    Heartbeat(EdgeHeartbeat),
    CapabilityUpdate(EdgeCapabilityUpdate),
    Result(EdgeCallResult),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "body", rename_all = "snake_case")]
pub enum EdgeServerMessage {
    SessionAccepted(EdgeSessionAccepted),
    Invoke(EdgeInvoke),
    Cancel(EdgeCancel),
}

fn validate_protocol_version(version: u16) -> EdgeProtocolResult {
    if version == EDGE_PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(EdgeProtocolError::new(format!(
            "unsupported Edge protocol version {version}; expected {EDGE_PROTOCOL_VERSION}"
        )))
    }
}

fn validate_identifier(field: &str, value: &str) -> EdgeProtocolResult {
    if value.trim().is_empty() || value.len() > EDGE_MAX_IDENTIFIER_BYTES {
        return Err(EdgeProtocolError::new(format!(
            "{field} must contain 1..={EDGE_MAX_IDENTIFIER_BYTES} bytes"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(EdgeProtocolError::new(format!(
            "{field} cannot contain control characters"
        )));
    }
    Ok(())
}

fn validate_token(field: &str, value: &str) -> EdgeProtocolResult {
    validate_identifier(field, value)?;
    if !value.bytes().all(|byte| {
        byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || matches!(byte, b'.' | b'_' | b'-' | b':')
    }) {
        return Err(EdgeProtocolError::new(format!(
            "{field} must use lowercase ASCII letters, digits, '.', '_', '-', or ':'"
        )));
    }
    Ok(())
}

fn validate_payload_limit(field: &str, bytes: u64) -> EdgeProtocolResult {
    if bytes == 0 || bytes > EDGE_ABSOLUTE_MAX_PAYLOAD_BYTES {
        return Err(EdgeProtocolError::new(format!(
            "{field} must be in 1..={EDGE_ABSOLUTE_MAX_PAYLOAD_BYTES}"
        )));
    }
    Ok(())
}

fn validate_json_size(field: &str, value: &Value, limit: u64) -> EdgeProtocolResult {
    validate_payload_limit("payload limit", limit)?;
    let bytes = serde_json::to_vec(value)
        .map_err(|_| EdgeProtocolError::new(format!("{field} cannot be encoded as JSON")))?;
    if bytes.len() as u64 > limit {
        return Err(EdgeProtocolError::new(format!(
            "{field} is {} bytes and exceeds its {limit}-byte grant",
            bytes.len()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW_MS: i64 = 1_800_000_000_000;

    fn screen_capability() -> EdgeCapabilityDescriptor {
        EdgeCapabilityDescriptor {
            capability: "screen.capture".into(),
            generation: 4,
            operations: vec!["capture".into(), "status".into()],
            max_request_bytes: 8 * 1024,
            max_response_bytes: 8 * 1024 * 1024,
            max_in_flight: 2,
        }
    }

    fn grant() -> EdgeExecutionGrant {
        EdgeExecutionGrant {
            grant_id: "grant-1".into(),
            workspace_id: "default".into(),
            device_id: "desktop-1".into(),
            execution_id: "execution-1".into(),
            execution_epoch: 9,
            capability: "screen.capture".into(),
            capability_generation: 4,
            operation: "capture".into(),
            issued_at_ms: NOW_MS - 1_000,
            expires_at_ms: NOW_MS + 30_000,
            max_request_bytes: 8 * 1024,
            max_response_bytes: 8 * 1024 * 1024,
        }
    }

    fn invocation() -> EdgeInvoke {
        EdgeInvoke {
            session_id: "session-1".into(),
            session_generation: 7,
            request_id: "request-1".into(),
            idempotency_key: "execution-1:screen-1".into(),
            deadline_at_ms: NOW_MS + 10_000,
            grant: grant(),
            capability: "screen.capture".into(),
            operation: "capture".into(),
            payload: json!({"display_id": 1}),
        }
    }

    #[test]
    fn client_hello_round_trips_with_explicit_wire_tag() {
        let message = EdgeClientMessage::Hello(EdgeHello {
            protocol_version: EDGE_PROTOCOL_VERSION,
            device_id: "desktop-1".into(),
            workspace_id: "default".into(),
            client_version: "0.1.0".into(),
            client_instance_id: "instance-7".into(),
            capabilities: vec![screen_capability()],
        });
        let encoded = serde_json::to_value(&message).unwrap();
        assert_eq!(encoded["type"], "hello");
        let EdgeClientMessage::Hello(decoded) =
            serde_json::from_value::<EdgeClientMessage>(encoded).unwrap()
        else {
            panic!("wire message changed variants");
        };
        decoded.validate().unwrap();
        assert!(decoded.capabilities[0].supports("screen.capture", "capture"));
    }

    #[test]
    fn hello_rejects_version_skew_and_duplicate_capabilities() {
        let mut hello = EdgeHello {
            protocol_version: EDGE_PROTOCOL_VERSION + 1,
            device_id: "desktop-1".into(),
            workspace_id: "default".into(),
            client_version: "0.1.0".into(),
            client_instance_id: "instance-1".into(),
            capabilities: vec![screen_capability()],
        };
        assert!(hello
            .validate()
            .unwrap_err()
            .to_string()
            .contains("version"));
        hello.protocol_version = EDGE_PROTOCOL_VERSION;
        hello.capabilities.push(screen_capability());
        assert!(hello
            .validate()
            .unwrap_err()
            .to_string()
            .contains("duplicate"));
    }

    #[test]
    fn invocation_is_bound_to_grant_capability_deadline_and_size() {
        let mut invoke = invocation();
        invoke.validate(NOW_MS).unwrap();

        invoke.operation = "status".into();
        assert!(invoke
            .validate(NOW_MS)
            .unwrap_err()
            .to_string()
            .contains("match"));
        invoke.operation = "capture".into();

        invoke.deadline_at_ms = invoke.grant.expires_at_ms + 1;
        assert!(invoke
            .validate(NOW_MS)
            .unwrap_err()
            .to_string()
            .contains("deadline"));
        invoke.deadline_at_ms = NOW_MS + 10_000;

        invoke.grant.max_request_bytes = 8;
        assert!(invoke
            .validate(NOW_MS)
            .unwrap_err()
            .to_string()
            .contains("exceeds"));
    }

    #[test]
    fn expired_grant_and_stale_session_generation_are_rejected() {
        let mut invoke = invocation();
        invoke.grant.expires_at_ms = NOW_MS;
        assert!(invoke
            .validate(NOW_MS)
            .unwrap_err()
            .to_string()
            .contains("expired"));

        invoke.grant.expires_at_ms = NOW_MS + 30_000;
        invoke.session_generation = 0;
        assert!(invoke
            .validate(NOW_MS)
            .unwrap_err()
            .to_string()
            .contains("session_generation"));
    }

    #[test]
    fn result_shape_distinguishes_success_from_failure() {
        let mut result = EdgeCallResult {
            session_id: "session-1".into(),
            session_generation: 7,
            request_id: "request-1".into(),
            status: EdgeCallStatus::Succeeded,
            payload: Some(json!({"image_ref": "artifact:screen-1"})),
            error: None,
        };
        result.validate(4 * 1024).unwrap();

        result.status = EdgeCallStatus::Unavailable;
        assert!(result.validate(4 * 1024).is_err());
        result.error = Some(EdgeCallError {
            code: "edge_offline".into(),
            message: "The selected desktop is offline".into(),
            retryable: true,
        });
        result.validate(4 * 1024).unwrap();
    }

    #[test]
    fn unknown_struct_fields_fail_closed() {
        let value = json!({
            "protocol_version": EDGE_PROTOCOL_VERSION,
            "device_id": "desktop-1",
            "workspace_id": "default",
            "client_version": "0.1.0",
            "client_instance_id": "instance-1",
            "capabilities": [],
            "ambient_filesystem_access": true
        });
        assert!(serde_json::from_value::<EdgeHello>(value).is_err());
    }
}
