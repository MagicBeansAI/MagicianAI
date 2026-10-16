//! Durable, payload-minimal reconstruction contract for remote MCP Tasks.
//!
//! Only official remote Tasks are restart-recoverable. MRTR request state and ordinary
//! in-flight calls are bound to the SDK session that dispatched them and deliberately
//! never enter this document. The product owns durable storage and must atomically claim
//! a checkpoint before allowing a recovered task to perform side effects.

use std::{fmt, time::Duration};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tool_runtime_core::credential_profiles::CredentialProfileKey;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::{McpClientError, McpToolId};

pub const MCP_TASK_RECOVERY_CONTRACT_V1: &str = "magician.mcp-task-recovery.v1";

const TASK_RECOVERY_SCHEMA_VERSION: u32 = 1;
const MAX_RECOVERY_RESOURCE_BYTES: usize = 8 * 1024;
const MAX_RECOVERY_SLOT_BYTES: usize = 1_024;
const MAX_RECOVERY_DOCUMENT_BYTES: usize = 128 * 1024;
pub(crate) const MAX_RECOVERY_CLOCK_SKEW: Duration = Duration::from_secs(60);

/// Exact, non-secret product binding for a restart-recoverable task.
///
/// The digest binds principal, workspace, provider, profile alias, credential binding,
/// a caller-supplied canonical MCP resource identity, and one product-owned durable
/// continuation slot. It is safe to persist but is redacted from diagnostics so scoped
/// identities do not become log metadata.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct McpTaskRecoveryBinding {
    pub(crate) digest: [u8; 32],
}

impl McpTaskRecoveryBinding {
    pub fn new(
        profile: &CredentialProfileKey,
        canonical_resource_identity: &str,
        continuation_slot_identity: &str,
    ) -> Result<Self, McpClientError> {
        if canonical_resource_identity.is_empty()
            || canonical_resource_identity.len() > MAX_RECOVERY_RESOURCE_BYTES
            || canonical_resource_identity.trim() != canonical_resource_identity
            || canonical_resource_identity.chars().any(char::is_control)
        {
            return Err(McpClientError::TaskRecoveryBindingRejected);
        }
        if continuation_slot_identity.is_empty()
            || continuation_slot_identity.len() > MAX_RECOVERY_SLOT_BYTES
            || continuation_slot_identity.trim() != continuation_slot_identity
            || continuation_slot_identity.chars().any(char::is_control)
        {
            return Err(McpClientError::TaskRecoveryBindingRejected);
        }
        let profile =
            serde_json::to_vec(profile).map_err(|_| McpClientError::TaskRecoveryBindingRejected)?;
        let mut hasher = Sha256::new();
        hash_component(&mut hasher, b"magician-mcp-task-recovery-binding-v1");
        hash_component(&mut hasher, &profile);
        hash_component(&mut hasher, canonical_resource_identity.as_bytes());
        hash_component(&mut hasher, continuation_slot_identity.as_bytes());
        Ok(Self {
            digest: hasher.finalize().into(),
        })
    }
}

impl fmt::Debug for McpTaskRecoveryBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("McpTaskRecoveryBinding([REDACTED])")
    }
}

/// Restart classification for one currently active process-local continuation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum McpContinuationRecoveryDisposition {
    /// The remote task can produce a durable checkpoint and be revalidated after reconnect.
    RecoverableRemoteTask,
    /// The continuation contains SDK-session authority and must fail closed on session loss.
    SessionBound,
}

/// Move-only, redacted checkpoint suitable for product-owned durable storage.
///
/// It contains no credential, bearer token, OAuth state, original tool arguments,
/// provider result, MRTR request/response, schema, status message, or input value. The
/// remote task id and immutable creation marker remain private inside the document.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct McpTaskRecoveryCheckpoint(Vec<u8>);

impl McpTaskRecoveryCheckpoint {
    /// Reconstitute a checkpoint loaded from a trusted product-owned record.
    /// Full schema, checksum, binding, expiry, and server checks occur during recovery.
    pub fn from_bytes(mut bytes: Vec<u8>) -> Result<Self, McpClientError> {
        if bytes.is_empty() || bytes.len() > MAX_RECOVERY_DOCUMENT_BYTES {
            bytes.zeroize();
            return Err(McpClientError::TaskRecoveryRecordRejected);
        }
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub(crate) fn encode(record: &McpTaskRecoveryRecord) -> Result<Self, McpClientError> {
        let record_bytes =
            serde_json::to_vec(&record).map_err(|_| McpClientError::TaskRecoveryRecordRejected)?;
        let envelope = McpTaskRecoveryEnvelopeRef {
            checksum: Sha256::digest(&record_bytes).into(),
            record,
        };
        let bytes = serde_json::to_vec(&envelope)
            .map_err(|_| McpClientError::TaskRecoveryRecordRejected)?;
        Self::from_bytes(bytes)
    }

    pub(crate) fn decode(
        &self,
        binding: McpTaskRecoveryBinding,
    ) -> Result<McpTaskRecoveryRecord, McpClientError> {
        let envelope: McpTaskRecoveryEnvelope = serde_json::from_slice(&self.0)
            .map_err(|_| McpClientError::TaskRecoveryRecordRejected)?;
        if envelope.record.schema_version != TASK_RECOVERY_SCHEMA_VERSION
            || envelope.record.binding_digest != binding.digest
        {
            return Err(McpClientError::TaskRecoveryRecordRejected);
        }
        let record_bytes = serde_json::to_vec(&envelope.record)
            .map_err(|_| McpClientError::TaskRecoveryRecordRejected)?;
        let expected: [u8; 32] = Sha256::digest(&record_bytes).into();
        if !constant_time_eq(&expected, &envelope.checksum) {
            return Err(McpClientError::TaskRecoveryRecordRejected);
        }
        Ok(envelope.record)
    }
}

/// Move-only recovery attempt prepared before any network dispatch.
///
/// The replacement checkpoint already consumes this attempt's operation count and
/// revision. A product coordinator must durably replace the prior checkpoint with this
/// one before passing the attempt to [`McpClient::recover_task`](crate::McpClient::recover_task).
pub struct McpPreparedTaskRecovery {
    pub(crate) record: McpTaskRecoveryRecord,
    pub(crate) tool: McpToolId,
    checkpoint: McpTaskRecoveryCheckpoint,
}

impl McpPreparedTaskRecovery {
    pub(crate) fn new(
        record: McpTaskRecoveryRecord,
        tool: McpToolId,
    ) -> Result<Self, McpClientError> {
        let checkpoint = McpTaskRecoveryCheckpoint::encode(&record)?;
        Ok(Self {
            record,
            tool,
            checkpoint,
        })
    }

    /// Checkpoint that must atomically replace the input record before dispatch.
    pub fn checkpoint(&self) -> &McpTaskRecoveryCheckpoint {
        &self.checkpoint
    }
}

impl fmt::Debug for McpPreparedTaskRecovery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpPreparedTaskRecovery")
            .field("contract", &MCP_TASK_RECOVERY_CONTRACT_V1)
            .field("checkpoint", &self.checkpoint)
            .field("private_state", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Debug for McpTaskRecoveryCheckpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpTaskRecoveryCheckpoint")
            .field("contract", &MCP_TASK_RECOVERY_CONTRACT_V1)
            .field("bytes", &self.0.len())
            .field("contents", &"[REDACTED]")
            .finish()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpTaskRecoveryEnvelope {
    checksum: [u8; 32],
    record: McpTaskRecoveryRecord,
}

#[derive(Serialize)]
struct McpTaskRecoveryEnvelopeRef<'a> {
    checksum: [u8; 32],
    record: &'a McpTaskRecoveryRecord,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpTaskRecoveryRecord {
    pub(crate) schema_version: u32,
    pub(crate) binding_digest: [u8; 32],
    pub(crate) transport: String,
    pub(crate) protocol_version: String,
    pub(crate) server_name: Option<String>,
    pub(crate) server_version: Option<String>,
    pub(crate) tool_name: String,
    pub(crate) task_id: String,
    pub(crate) created_at: String,
    pub(crate) started_at_epoch_millis: u64,
    pub(crate) lifetime_expires_at_epoch_millis: u64,
    pub(crate) expires_at_epoch_millis: u64,
    pub(crate) operation_count: u64,
    pub(crate) cancel_requested: bool,
    pub(crate) revision: u64,
}

impl McpTaskRecoveryRecord {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        binding: McpTaskRecoveryBinding,
        transport: &str,
        protocol_version: &str,
        server_name: Option<&str>,
        server_version: Option<&str>,
        tool_name: &str,
        task_id: &str,
        created_at: &str,
        started_at_epoch_millis: u64,
        lifetime_expires_at_epoch_millis: u64,
        expires_at_epoch_millis: u64,
        operation_count: usize,
        cancel_requested: bool,
        revision: u64,
    ) -> Result<Self, McpClientError> {
        let operation_count = u64::try_from(operation_count)
            .map_err(|_| McpClientError::TaskRecoveryRecordRejected)?;
        Ok(Self {
            schema_version: TASK_RECOVERY_SCHEMA_VERSION,
            binding_digest: binding.digest,
            transport: transport.to_owned(),
            protocol_version: protocol_version.to_owned(),
            server_name: server_name.map(str::to_owned),
            server_version: server_version.map(str::to_owned),
            tool_name: tool_name.to_owned(),
            task_id: task_id.to_owned(),
            created_at: created_at.to_owned(),
            started_at_epoch_millis,
            lifetime_expires_at_epoch_millis,
            expires_at_epoch_millis,
            operation_count,
            cancel_requested,
            revision,
        })
    }
}

fn hash_component(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right.iter())
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

#[cfg(test)]
mod tests {
    use static_assertions::{assert_impl_all, assert_not_impl_any};
    use tool_runtime_core::credential_profiles::{
        CredentialProfileBinding, CredentialProfileKey, CredentialScope,
    };

    use super::*;

    fn binding(resource: &str) -> McpTaskRecoveryBinding {
        let profile = CredentialProfileKey::new(
            CredentialScope::new("person", "space").unwrap(),
            "mcp",
            "primary",
            CredentialProfileBinding::Provider,
        )
        .unwrap();
        McpTaskRecoveryBinding::new(&profile, resource, "execution/task/one").unwrap()
    }

    fn record(binding: McpTaskRecoveryBinding) -> McpTaskRecoveryRecord {
        McpTaskRecoveryRecord::new(
            binding,
            "streamable_http",
            "2026-07-28",
            Some("server"),
            Some("1"),
            "tool",
            "task-private",
            "2026-08-07T00:00:00Z",
            10,
            100,
            90,
            3,
            false,
            4,
        )
        .unwrap()
    }

    #[test]
    fn recovery_capabilities_are_non_serializable_and_checkpoint_is_move_only() {
        assert_impl_all!(McpTaskRecoveryBinding: Send, Sync, Copy);
        assert_not_impl_any!(McpTaskRecoveryBinding: Serialize, Deserialize<'static>);
        assert_impl_all!(McpTaskRecoveryCheckpoint: Send, Sync);
        assert_not_impl_any!(McpTaskRecoveryCheckpoint: Clone, Serialize, Deserialize<'static>);
        assert_impl_all!(McpPreparedTaskRecovery: Send, Sync);
        assert_not_impl_any!(McpPreparedTaskRecovery: Clone, Serialize, Deserialize<'static>);
    }

    #[test]
    fn binding_is_exact_and_debug_is_redacted() {
        let first = binding("mcp://server/one");
        let second = binding("mcp://server/two");
        assert_ne!(first, second);
        let profile = CredentialProfileKey::new(
            CredentialScope::new("person", "space").unwrap(),
            "mcp",
            "primary",
            CredentialProfileBinding::Provider,
        )
        .unwrap();
        assert_ne!(
            first,
            McpTaskRecoveryBinding::new(&profile, "mcp://server/one", "execution/task/two")
                .unwrap()
        );
        assert_eq!(format!("{first:?}"), "McpTaskRecoveryBinding([REDACTED])");
        assert!(McpTaskRecoveryBinding::new(&profile, " resource ", "execution/task/one").is_err());
        assert!(McpTaskRecoveryBinding::new(&profile, "mcp://server/one", " bad slot ").is_err());
    }

    #[test]
    fn checkpoint_rejects_wrong_binding_corruption_and_unknown_fields() {
        let first = binding("mcp://server/one");
        let checkpoint = McpTaskRecoveryCheckpoint::encode(&record(first)).unwrap();
        assert!(checkpoint.decode(first).is_ok());
        assert!(checkpoint.decode(binding("mcp://server/two")).is_err());

        let mut corrupt = checkpoint.as_bytes().to_vec();
        let index = corrupt.len() / 2;
        corrupt[index] ^= 1;
        let corrupt = McpTaskRecoveryCheckpoint::from_bytes(corrupt).unwrap();
        assert!(corrupt.decode(first).is_err());

        let mut value: serde_json::Value = serde_json::from_slice(checkpoint.as_bytes()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("unexpected".to_owned(), serde_json::Value::Bool(true));
        let unknown =
            McpTaskRecoveryCheckpoint::from_bytes(serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(unknown.decode(first).is_err());
    }

    #[test]
    fn checkpoint_debug_never_exposes_private_task_fields() {
        let binding = binding("mcp://server/one");
        let checkpoint = McpTaskRecoveryCheckpoint::encode(&record(binding)).unwrap();
        let debug = format!("{checkpoint:?}");
        assert!(!debug.contains("task-private"));
        assert!(!debug.contains("tool"));
        assert!(debug.contains("[REDACTED]"));
    }
}
