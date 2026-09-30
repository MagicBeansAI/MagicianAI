//! Canonical, scope-bound raw tool-result materialization.
//!
//! This module is the production storage half of the unified tool-result
//! contract. Projection and surface adaptation remain separate: callers hand
//! it an already-redacted JSON value and authenticated invocation binding, and
//! the service atomically materializes that value under its existing chat,
//! task, or ephemeral owner before returning an opaque reference.
//!
//! References are locators, never bearer credentials. Every read supplies the
//! original owner and agent binding, resolves the current authority revision,
//! validates retention, and verifies the content hash before returning data.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock, Weak},
};

use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use crate::magician_v2::agents::{AgentStorage, FileLockGuard};
use crate::magician_v2::artifact_v2::{
    service::{ArtifactV2Error, ScopeRef},
    workspace::ArtifactV2Workspace,
};
use crate::magician_v2::chat::storage::{
    acquire_chat_session_lifecycle_guard_for_existing_scope,
    chat_session_lifecycle_error_is_not_found,
};
use crate::magician_v2::json_traversal::{
    canonicalize_json_owned, clone_json_iteratively, discard_json_iteratively, inspect_json,
    inspect_json_bounded, json_encoded_len, write_canonical_json, write_json,
    MAX_RETAINED_JSON_DEPTH,
};

pub const RAW_RESULT_SCHEMA_VERSION: u16 = 1;
pub const DEFAULT_INLINE_RESULT_BYTES: usize = 16 * 1024;
const MAX_INLINE_RESULT_BYTES: usize = 1024 * 1024;
pub const DEFAULT_RESULT_PAGE_BYTES: usize = 256 * 1024;
pub const MAX_RESULT_PAGE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_RESULT_PAGE_RECORDS: usize = 1_000;
pub const MAX_RESULT_FIELD_PATHS: usize = 32;
pub const RESULT_CURSOR_TTL_MS: i64 = 15 * 60 * 1_000;
const MAX_RESULT_FIELD_PATH_BYTES: usize = 1_024;
const MAX_RESULT_CURSOR_ENCODED_BYTES: usize = 16 * 1024;

/// Stable complete-value admission limit for the lossless read stream. Values
/// above this bound are decomposed into typed container/value fragments before
/// request pagination is applied. Keeping this independent of the caller's
/// page budget makes cursor offsets stable when a client changes page size.
const MAX_COMPLETE_READ_ENTRY_BYTES: usize = 64 * 1024;
/// Source-byte target for one large-string fragment. JSON escaping can expand
/// this value, so it is deliberately well below the complete-entry bound.
const STRING_READ_FRAGMENT_SOURCE_BYTES: usize = 8 * 1024;
pub const RAW_RESULT_RECONSTRUCTION_VERSION: u16 = 1;

const RESULT_REF_PREFIX: &str = "result_ref_v1_";
const RESULT_CURSOR_PREFIX: &str = "result_cursor_v1_";
const EXPIRED_CLEANUP_INTERVAL_MS: i64 = 5 * 60 * 1_000;
/// Complete canonical results above this size require a future streaming
/// ingestion contract. Refusing them before a payload-sized serialization
/// allocation keeps the current Value-based API honest and bounded.
const MAX_MATERIALIZED_RESULT_BYTES: u64 = 128 * 1024 * 1024;
const MAX_RESULT_LOCATOR_BYTES: u64 = 64 * 1024;
const MAX_RESULT_MANIFEST_OVERHEAD_BYTES: u64 = 1024 * 1024;
const MAX_RESULT_MANIFEST_BYTES: u64 =
    MAX_INLINE_RESULT_BYTES as u64 + MAX_RESULT_MANIFEST_OVERHEAD_BYTES;
/// A complete retained result cannot create an unbounded number of individually
/// allocated `Value` nodes even when its encoded bytes fit the 128 MiB ceiling.
const MAX_MATERIALIZED_RESULT_NODES: usize = 1_000_000;
const MAX_RESULT_LOCATOR_NODES: usize = 128;
const MAX_RESULT_MANIFEST_NODES: usize = MAX_MATERIALIZED_RESULT_NODES + 256;

struct CanonicalDigestWriter {
    hasher: blake3::Hasher,
    bytes: usize,
}

impl std::io::Write for CanonicalDigestWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.hasher.update(buffer);
        self.bytes = self.bytes.saturating_add(buffer.len());
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn canonical_content_digest(value: &Value) -> Result<(String, usize), CanonicalResultError> {
    let mut writer = CanonicalDigestWriter {
        hasher: blake3::Hasher::new(),
        bytes: 0,
    };
    write_canonical_json(value, &mut writer).map_err(|_| CanonicalResultError::InvalidRequest {
        code: "result_not_serializable",
    })?;
    Ok((
        format!("blake3:{}", writer.hasher.finalize().to_hex()),
        writer.bytes,
    ))
}

fn expired_cleanup_schedule() -> &'static Mutex<BTreeMap<String, i64>> {
    static LAST_SWEEP: OnceLock<Mutex<BTreeMap<String, i64>>> = OnceLock::new();
    LAST_SWEEP.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn process_materialization_lock(reference_digest: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Mutex<BTreeMap<String, Weak<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Weak entries keep the registry proportional to active identities. A
    // previous completed key is removed on the next acquisition instead of
    // accumulating one mutex per result for the life of the process.
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(reference_digest).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(reference_digest.to_string(), Arc::downgrade(&lock));
    lock
}

/// Public opaque reference to one canonical raw result.
///
/// The value intentionally carries no filesystem path, owner id, principal,
/// workspace, or authority material.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct ScopedResultRef(String);

impl ScopedResultRef {
    pub fn parse(value: impl Into<String>) -> Result<Self, CanonicalResultError> {
        let reference = Self(value.into());
        reference.id()?;
        Ok(reference)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn from_digest(digest: &str) -> Self {
        Self(format!("{RESULT_REF_PREFIX}{digest}"))
    }

    fn id(&self) -> Result<&str, CanonicalResultError> {
        let Some(id) = self.0.strip_prefix(RESULT_REF_PREFIX) else {
            return Err(CanonicalResultError::NotFound);
        };
        if id.len() != 64
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(CanonicalResultError::NotFound);
        }
        Ok(id)
    }
}

/// Existing lifecycle owner selected by the invocation surface.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RawResultOwner {
    Chat {
        session_id: String,
    },
    Task {
        task_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        execution_id: Option<String>,
    },
    EphemeralVoice {
        voice_session_id: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ResultRetentionClass {
    ChatLifecycle,
    TaskLifecycle,
    EphemeralVoice,
}

/// Lineage and authority lookup identity. Values are persisted server-side,
/// not encoded into the public reference.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RawResultIdentity {
    pub agent_id: String,
    pub tool_name: String,
    pub tool_call_id: String,
    /// Canonical trust-policy coordinates captured from the actual dispatched
    /// arguments. They let a continuation recheck an action-specific deny
    /// without trusting the model-visible tool name or reconstructing params.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_action: Option<String>,
}

/// Binding passed to the current-authority resolver on every materialization
/// and read. The resolver owns policy interpretation; this storage module only
/// enforces exact revision equality.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultAuthorityBinding {
    pub agent_id: String,
    pub owner: RawResultOwner,
    pub tool_name: String,
    pub tool_call_id: String,
    pub trust_tool: Option<String>,
    pub trust_action: Option<String>,
}

impl ResultAuthorityBinding {
    pub fn for_result(owner: RawResultOwner, identity: &RawResultIdentity) -> Self {
        Self {
            agent_id: identity.agent_id.clone(),
            owner,
            tool_name: identity.tool_name.clone(),
            tool_call_id: identity.tool_call_id.clone(),
            trust_tool: identity.trust_tool.clone(),
            trust_action: identity.trust_action.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, Error, PartialEq, Eq)]
#[error("authority revision lookup failed")]
pub struct ResultAuthorityLookupError;

#[async_trait]
pub trait CurrentResultAuthority: Send + Sync {
    async fn current_authority_revision(
        &self,
        scope: &ScopeRef,
        binding: &ResultAuthorityBinding,
    ) -> Result<Option<String>, ResultAuthorityLookupError>;

    /// Semantic authorization hook used by reads. Snapshot materialization
    /// retains exact revision equality by default; a fresh read resolver may
    /// additionally prove that the original tool/action remains authorized
    /// even when an unrelated policy edit produced a new global revision.
    async fn is_currently_authorized(
        &self,
        scope: &ScopeRef,
        binding: &ResultAuthorityBinding,
        materialized_revision: &str,
    ) -> Result<bool, ResultAuthorityLookupError> {
        Ok(self
            .current_authority_revision(scope, binding)
            .await?
            .as_deref()
            == Some(materialized_revision))
    }
}

/// Optional current-policy check for action-specific trust/deny rules. The
/// originating tool/action coordinates are persisted server-side and never
/// accepted from a continuation request.
pub trait ResultReadPolicyGuard: Send + Sync {
    fn permits(&self, tool: &str, action: &str) -> bool;
}

#[derive(Debug, Clone, Copy)]
struct CleanupOnlyAuthority;

#[async_trait]
impl CurrentResultAuthority for CleanupOnlyAuthority {
    async fn current_authority_revision(
        &self,
        _scope: &ScopeRef,
        _binding: &ResultAuthorityBinding,
    ) -> Result<Option<String>, ResultAuthorityLookupError> {
        Ok(None)
    }
}

/// Request-scoped authority resolver for a caller that has just resolved the
/// current effective policy revision for one exact invocation binding.
///
/// This snapshot must be reconstructed after every policy refresh and for every
/// independent read/materialization request. It is intentionally unsuitable as
/// a process-wide cache: only the exact scope + owner + agent + tool-call tuple
/// supplied at construction resolves successfully.
#[derive(Debug, Clone)]
pub struct SnapshotResultAuthority {
    scope: ScopeRef,
    binding: ResultAuthorityBinding,
    current_revision: String,
}

impl SnapshotResultAuthority {
    pub fn for_current_binding(
        scope: ScopeRef,
        binding: ResultAuthorityBinding,
        current_revision: impl Into<String>,
    ) -> Result<Self, CanonicalResultError> {
        validate_owner(&binding.owner)?;
        validate_binding_label(&binding.agent_id, "invalid_agent_id")?;
        validate_binding_label(&binding.tool_name, "invalid_tool_name")?;
        validate_binding_label(&binding.tool_call_id, "invalid_tool_call_id")?;
        if let Some(tool) = binding.trust_tool.as_deref() {
            validate_binding_label(tool, "invalid_trust_tool")?;
        }
        if let Some(action) = binding.trust_action.as_deref() {
            validate_binding_label(action, "invalid_trust_action")?;
        }
        let current_revision = current_revision.into();
        if current_revision.trim().is_empty() || current_revision.len() > 512 {
            return Err(CanonicalResultError::InvalidRequest {
                code: "invalid_authority_revision",
            });
        }
        Ok(Self {
            scope,
            binding,
            current_revision,
        })
    }
}

#[async_trait]
impl CurrentResultAuthority for SnapshotResultAuthority {
    async fn current_authority_revision(
        &self,
        scope: &ScopeRef,
        binding: &ResultAuthorityBinding,
    ) -> Result<Option<String>, ResultAuthorityLookupError> {
        let exact_scope = self.scope.principal() == scope.principal()
            && self.scope.workspace() == scope.workspace();
        Ok((exact_scope && &self.binding == binding).then(|| self.current_revision.clone()))
    }
}

/// Request-scoped resolver used by the common `read_result` continuation.
///
/// Unlike [`SnapshotResultAuthority`], the caller does not know the original
/// tool-call identity until the opaque reference has been resolved. This
/// resolver therefore binds the immutable scope/owner/agent tuple up front and
/// admits only manifest identities whose originating tool is still present in
/// the caller's freshly resolved effective-tool policy. The store then compares
/// the returned current revision with the revision persisted at materialization
/// time, so possession of a reference cannot survive a policy revision or
/// resurrect a removed tool grant.
#[derive(Clone)]
pub struct ScopedResultReadAuthority {
    scope: ScopeRef,
    owner: RawResultOwner,
    agent_id: String,
    authorized_tool_names: BTreeSet<String>,
    current_revision: String,
    policy_guard: Option<Arc<dyn ResultReadPolicyGuard>>,
}

impl ScopedResultReadAuthority {
    pub fn for_current_policy(
        scope: ScopeRef,
        owner: RawResultOwner,
        agent_id: impl Into<String>,
        authorized_tool_names: impl IntoIterator<Item = String>,
        current_revision: impl Into<String>,
    ) -> Result<Self, CanonicalResultError> {
        validate_owner(&owner)?;
        let agent_id = agent_id.into();
        validate_binding_label(&agent_id, "invalid_agent_id")?;
        let authorized_tool_names = authorized_tool_names
            .into_iter()
            .filter(|name| !name.trim().is_empty() && validate_binding_label(name, "tool").is_ok())
            .collect::<BTreeSet<_>>();
        let current_revision = current_revision.into();
        if current_revision.trim().is_empty() || current_revision.len() > 512 {
            return Err(CanonicalResultError::InvalidRequest {
                code: "invalid_authority_revision",
            });
        }
        Ok(Self {
            scope,
            owner,
            agent_id,
            authorized_tool_names,
            current_revision,
            policy_guard: None,
        })
    }

    pub fn with_policy_guard(mut self, policy_guard: Arc<dyn ResultReadPolicyGuard>) -> Self {
        self.policy_guard = Some(policy_guard);
        self
    }
}

#[async_trait]
impl CurrentResultAuthority for ScopedResultReadAuthority {
    async fn current_authority_revision(
        &self,
        scope: &ScopeRef,
        binding: &ResultAuthorityBinding,
    ) -> Result<Option<String>, ResultAuthorityLookupError> {
        let exact_scope = self.scope.principal() == scope.principal()
            && self.scope.workspace() == scope.workspace();
        let authorized = exact_scope
            && self.owner == binding.owner
            && self.agent_id == binding.agent_id
            && self.authorized_tool_names.contains(&binding.tool_name);
        Ok(authorized.then(|| self.current_revision.clone()))
    }

    async fn is_currently_authorized(
        &self,
        scope: &ScopeRef,
        binding: &ResultAuthorityBinding,
        _materialized_revision: &str,
    ) -> Result<bool, ResultAuthorityLookupError> {
        let exact_scope = self.scope.principal() == scope.principal()
            && self.scope.workspace() == scope.workspace();
        let exact_binding = exact_scope
            && self.owner == binding.owner
            && self.agent_id == binding.agent_id
            && self.authorized_tool_names.contains(&binding.tool_name);
        if !exact_binding {
            return Ok(false);
        }
        let policy_allowed = match (
            self.policy_guard.as_ref(),
            binding.trust_tool.as_deref(),
            binding.trust_action.as_deref(),
        ) {
            (Some(guard), Some(tool), Some(action)) => guard.permits(tool, action),
            // Legacy v1 manifests did not persist action coordinates. They may
            // still be read only through the current exact tool allowlist; a
            // caller with an action-aware guard fails closed for such records.
            (Some(_), _, _) => false,
            (None, _, _) => true,
        };
        Ok(policy_allowed)
    }
}

#[derive(Debug, Clone)]
pub struct MaterializeRawResultRequest {
    pub scope: ScopeRef,
    pub owner: RawResultOwner,
    pub identity: RawResultIdentity,
    pub authority_revision: String,
    /// Complete payload after execution-owned secret/restricted-field
    /// sanitization. This service never accepts an unredacted companion value.
    pub safe_value: Value,
    pub media_type: String,
    pub retention_class: ResultRetentionClass,
    pub expires_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RawResultDescriptor {
    pub schema_version: u16,
    pub content_ref: ScopedResultRef,
    pub content_hash: String,
    pub media_type: String,
    pub size_bytes: u64,
    pub retention_class: ResultRetentionClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
}

impl RawResultDescriptor {
    /// The sole supported bridge into the pure projection contract. Storage
    /// expiry/schema details remain storage-owned; the projection receives only
    /// the opaque locator, verified hash, media metadata, size, and mapped
    /// lifecycle class.
    pub fn to_projection_descriptor(
        &self,
    ) -> crate::magician_v2::tool_result_projection::RawResultDescriptor {
        use crate::magician_v2::tool_result_projection::{
            RawResultDescriptor as ProjectionRawResultDescriptor,
            ResultRetentionClass as ProjectionRetentionClass,
            ScopedResultRef as ProjectionScopedResultRef,
        };

        let retention_class = match self.retention_class {
            ResultRetentionClass::ChatLifecycle => ProjectionRetentionClass::ChatSession,
            ResultRetentionClass::TaskLifecycle => ProjectionRetentionClass::TaskExecution,
            // Unlinked voice results are deliberately bounded by an expiry and
            // therefore map to the projection contract's ephemeral class. A
            // voice call linked to Chat is stored as ChatLifecycle instead.
            ResultRetentionClass::EphemeralVoice => ProjectionRetentionClass::Ephemeral,
        };
        ProjectionRawResultDescriptor {
            content_ref: ProjectionScopedResultRef {
                result_ref: self.content_ref.as_str().to_string(),
                cursor: None,
            },
            content_hash: self.content_hash.clone(),
            media_type: self.media_type.clone(),
            size_bytes: self.size_bytes,
            retention_class,
        }
    }
}

/// Authenticated invocation binding required in addition to the opaque ref.
/// Supplying a reference from another session/run is intentionally insufficient.
#[derive(Debug, Clone)]
pub struct RawResultReadContext {
    pub scope: ScopeRef,
    pub owner: RawResultOwner,
    pub agent_id: String,
}

#[derive(Debug, Clone)]
pub struct RawResultReadRequest {
    pub content_ref: ScopedResultRef,
    pub cursor: Option<String>,
    /// RFC 6901 JSON pointers. Empty means the root value.
    pub field_paths: Vec<String>,
    pub max_records: usize,
    /// Bound on serialized entry bytes, not model tokens or envelope overhead.
    /// Model projection applies its own stricter final measurement later.
    pub max_serialized_bytes: usize,
}

impl RawResultReadRequest {
    pub fn first_page(content_ref: ScopedResultRef, max_records: usize) -> Self {
        Self {
            content_ref,
            cursor: None,
            field_paths: Vec::new(),
            max_records,
            max_serialized_bytes: DEFAULT_RESULT_PAGE_BYTES,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RawResultReadEntry {
    /// The requested selection root that produced this entry.
    pub field_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_index: Option<usize>,
    /// Exact RFC 6901 location at which `value` (or the reassembled string)
    /// belongs. This is explicit because numeric object keys cannot safely be
    /// inferred as array indexes without the typed container entries.
    #[serde(default)]
    pub reconstruction_path: String,
    #[serde(default)]
    pub kind: RawResultReadEntryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub string_fragment: Option<RawResultStringFragment>,
    pub value: Value,
}

fn discard_read_entry(mut entry: RawResultReadEntry) {
    discard_json_iteratively(std::mem::replace(&mut entry.value, Value::Null));
}

fn discard_read_entries(entries: &mut Vec<RawResultReadEntry>) {
    for entry in entries.drain(..) {
        discard_read_entry(entry);
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RawResultReadEntryKind {
    /// `value` is the complete value at `reconstruction_path`.
    CompleteValue,
    /// `value` is an empty object/array that establishes the exact container
    /// type before subsequent descendant entries are applied.
    Container,
    /// `value` is a UTF-8-boundary-aligned substring. Concatenate fragments in
    /// byte-offset order at the same path to reconstruct the complete scalar.
    StringFragment,
}

impl Default for RawResultReadEntryKind {
    fn default() -> Self {
        Self::CompleteValue
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RawResultStringFragment {
    pub byte_start: usize,
    pub byte_end: usize,
    pub total_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RawResultReadPage {
    pub schema_version: u16,
    #[serde(default)]
    pub reconstruction_version: u16,
    pub content_ref: ScopedResultRef,
    pub content_hash: String,
    /// Canonicalized selection roots bound into the cursor.
    #[serde(default)]
    pub selection_paths: Vec<String>,
    pub entries: Vec<RawResultReadEntry>,
    pub page_start: usize,
    /// Legacy wire name retained for clients. For decomposed large values this
    /// is the total number of lossless read units, including typed fragments.
    pub total_records: usize,
    #[serde(default)]
    pub total_entries: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

impl RawResultReadPage {
    /// True only when every returned unit is already a complete JSON value.
    /// A lossless page can still return `false` when a very large value is
    /// represented by explicit container/string fragments.
    pub fn complete_values_only(&self) -> bool {
        self.entries
            .iter()
            .all(|entry| entry.kind == RawResultReadEntryKind::CompleteValue)
    }

    pub fn serialized_entry_bytes(&self) -> usize {
        self.entries
            .iter()
            .filter_map(|entry| compact_read_entry_len(entry).ok())
            .sum()
    }
}

fn read_entry_into_value(entry: RawResultReadEntry) -> Value {
    let RawResultReadEntry {
        field_path,
        source_index,
        reconstruction_path,
        kind,
        string_fragment,
        value,
    } = entry;
    let mut object = serde_json::Map::new();
    object.insert("field_path".to_string(), Value::String(field_path));
    if let Some(source_index) = source_index {
        object.insert("source_index".to_string(), Value::from(source_index));
    }
    object.insert(
        "reconstruction_path".to_string(),
        Value::String(reconstruction_path),
    );
    object.insert(
        "kind".to_string(),
        Value::String(
            match kind {
                RawResultReadEntryKind::CompleteValue => "complete_value",
                RawResultReadEntryKind::Container => "container",
                RawResultReadEntryKind::StringFragment => "string_fragment",
            }
            .to_string(),
        ),
    );
    if let Some(fragment) = string_fragment {
        let mut fragment_value = serde_json::Map::new();
        fragment_value.insert("byte_start".to_string(), Value::from(fragment.byte_start));
        fragment_value.insert("byte_end".to_string(), Value::from(fragment.byte_end));
        fragment_value.insert("total_bytes".to_string(), Value::from(fragment.total_bytes));
        object.insert("string_fragment".to_string(), Value::Object(fragment_value));
    }
    object.insert("value".to_string(), value);
    Value::Object(object)
}

fn read_page_into_value(page: RawResultReadPage, include_content_hash: bool) -> Value {
    let RawResultReadPage {
        schema_version,
        reconstruction_version,
        content_ref,
        content_hash,
        selection_paths,
        entries,
        page_start,
        total_records,
        total_entries,
        next_cursor,
    } = page;
    let mut object = serde_json::Map::new();
    object.insert("schema_version".to_string(), Value::from(schema_version));
    object.insert(
        "reconstruction_version".to_string(),
        Value::from(reconstruction_version),
    );
    object.insert("content_ref".to_string(), Value::String(content_ref.0));
    if include_content_hash {
        object.insert("content_hash".to_string(), Value::String(content_hash));
    }
    object.insert(
        "selection_paths".to_string(),
        Value::Array(selection_paths.into_iter().map(Value::String).collect()),
    );
    object.insert(
        "entries".to_string(),
        Value::Array(entries.into_iter().map(read_entry_into_value).collect()),
    );
    object.insert("page_start".to_string(), Value::from(page_start));
    object.insert("total_records".to_string(), Value::from(total_records));
    object.insert("total_entries".to_string(), Value::from(total_entries));
    if let Some(next_cursor) = next_cursor {
        object.insert("next_cursor".to_string(), Value::String(next_cursor));
    }
    Value::Object(object)
}

fn read_success_envelope(page: RawResultReadPage, include_content_hash: bool) -> Value {
    let complete_values_only = page.complete_values_only();
    let mut envelope = serde_json::Map::new();
    envelope.insert("status".to_string(), Value::String("ok".to_string()));
    envelope.insert(
        "page".to_string(),
        read_page_into_value(page, include_content_hash),
    );
    envelope.insert("lossless_reconstruction".to_string(), Value::Bool(true));
    envelope.insert(
        "complete_records_only".to_string(),
        Value::Bool(complete_values_only),
    );
    Value::Object(envelope)
}

/// Authenticated Chat/task display envelope. Keeping the truthfulness flags
/// here prevents one surface from claiming fragment pages contain only
/// complete records; model continuations use the hash-free sibling below.
pub fn lossless_read_success_payload(page: RawResultReadPage) -> Value {
    read_success_envelope(page, true)
}

/// Provider-facing continuation envelope. Integrity hashes are intentionally
/// retained only by authenticated display/API clients; exposing an unkeyed
/// digest to a model creates a low-entropy value oracle without adding any
/// reconstruction capability.
pub fn model_lossless_read_success_payload(page: RawResultReadPage) -> Value {
    read_success_envelope(page, false)
}

/// Build the common telemetry fields for a complete-result read without ever
/// accepting or serializing result values, selected paths, references, hashes,
/// cursors, tool arguments, or URLs. Surface adapters may add authenticated
/// owner correlation such as a chat session or task id.
pub fn content_free_read_telemetry(
    outcome: &Result<RawResultReadPage, CanonicalResultError>,
    requested_field_count: usize,
    duration_ms: f64,
) -> Value {
    let state = match outcome {
        Ok(_) => "used",
        Err(CanonicalResultError::Expired | CanonicalResultError::CursorExpired) => "expired",
        Err(CanonicalResultError::Revoked) => "revoked",
        Err(CanonicalResultError::NotFound) => "not_found",
        Err(_) => "failed",
    };
    serde_json::json!({
        "result_reference_state": state,
        "requested_field_count": requested_field_count,
        "page_start": outcome.as_ref().ok().map(|page| page.page_start),
        "returned_records": outcome.as_ref().ok().map(|page| page.entries.len()),
        "returned_bytes": outcome.as_ref().ok().map(RawResultReadPage::serialized_entry_bytes),
        "returned_fragments": outcome.as_ref().ok().map(|page| {
            page.entries.iter().filter(|entry| {
                entry.kind != RawResultReadEntryKind::CompleteValue
            }).count()
        }),
        "has_next_page": outcome.as_ref().ok().is_some_and(|page| page.next_cursor.is_some()),
        "duration_ms": duration_ms,
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawResultCleanupReport {
    pub removed: usize,
    pub corrupt_manifests: usize,
    pub failed: usize,
}

/// Safe public failures. Filesystem paths, resolver diagnostics, stored
/// identities, and neighboring-scope existence are never included.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CanonicalResultError {
    #[error("result not found")]
    NotFound,
    #[error("result authority was revoked")]
    Revoked,
    #[error("result expired")]
    Expired,
    #[error("result cursor expired")]
    CursorExpired,
    #[error("result content is corrupt")]
    Corrupt,
    #[error("a different result is already bound to this tool call")]
    IdentityConflict,
    #[error("result cursor is invalid")]
    InvalidCursor,
    #[error("result request is invalid: {code}")]
    InvalidRequest { code: &'static str },
    #[error("one selected result record exceeds the requested page byte limit")]
    RecordTooLarge,
    #[error("result authority is temporarily unavailable")]
    AuthorityUnavailable,
    #[error("result storage is temporarily unavailable")]
    StorageUnavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RawResultManifestV1 {
    schema_version: u16,
    descriptor: RawResultDescriptor,
    scope: ScopeRef,
    scope_digest: String,
    owner: RawResultOwner,
    identity: RawResultIdentity,
    authority_revision: String,
    created_at_ms: i64,
    cursor_key: String,
    storage: RawResultStorageV1,
}

/// Minimal scope-local locator. The complete manifest (and inline payload, when
/// used) remains physically beneath its lifecycle owner; this record only maps
/// an opaque reference to that owner.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RawResultLocatorV1 {
    schema_version: u16,
    content_ref: ScopedResultRef,
    scope_digest: String,
    owner: RawResultOwner,
    manifest_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RawResultStorageV1 {
    Inline { value: Value },
    File,
}

fn discard_manifest_inline_payload(manifest: &mut RawResultManifestV1) {
    if let RawResultStorageV1::Inline { value } = &mut manifest.storage {
        discard_json_iteratively(std::mem::replace(value, Value::Null));
    }
}

fn take_manifest_wire_value(
    manifest: &mut RawResultManifestV1,
) -> Result<Value, CanonicalResultError> {
    // Convert only shallow metadata through Serde. The potentially deep inline
    // payload is moved into the wire tree after every fallible conversion, so
    // an error still leaves it attached to `manifest` for iterative cleanup.
    let descriptor =
        serde_json::to_value(&manifest.descriptor).map_err(|_| CanonicalResultError::Corrupt)?;
    let scope = serde_json::to_value(&manifest.scope).map_err(|_| CanonicalResultError::Corrupt)?;
    let owner = serde_json::to_value(&manifest.owner).map_err(|_| CanonicalResultError::Corrupt)?;
    let identity =
        serde_json::to_value(&manifest.identity).map_err(|_| CanonicalResultError::Corrupt)?;
    let storage = match std::mem::replace(&mut manifest.storage, RawResultStorageV1::File) {
        RawResultStorageV1::Inline { value } => {
            let mut storage = serde_json::Map::new();
            storage.insert("kind".to_string(), Value::String("inline".to_string()));
            storage.insert("value".to_string(), value);
            Value::Object(storage)
        },
        RawResultStorageV1::File => {
            let mut storage = serde_json::Map::new();
            storage.insert("kind".to_string(), Value::String("file".to_string()));
            Value::Object(storage)
        },
    };
    let mut wire = serde_json::Map::new();
    wire.insert(
        "schema_version".to_string(),
        Value::from(manifest.schema_version),
    );
    wire.insert("descriptor".to_string(), descriptor);
    wire.insert("scope".to_string(), scope);
    wire.insert(
        "scope_digest".to_string(),
        Value::String(manifest.scope_digest.clone()),
    );
    wire.insert("owner".to_string(), owner);
    wire.insert("identity".to_string(), identity);
    wire.insert(
        "authority_revision".to_string(),
        Value::String(manifest.authority_revision.clone()),
    );
    wire.insert(
        "created_at_ms".to_string(),
        Value::from(manifest.created_at_ms),
    );
    wire.insert(
        "cursor_key".to_string(),
        Value::String(manifest.cursor_key.clone()),
    );
    wire.insert("storage".to_string(), storage);
    Ok(Value::Object(wire))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ResultCursorClaimsV1 {
    schema_version: u16,
    result_id: String,
    content_hash: String,
    selection_digest: String,
    next_offset: usize,
    expires_at_ms: i64,
}

#[derive(Clone)]
pub struct CanonicalRawResultStore {
    workspace: ArtifactV2Workspace,
    authority: Arc<dyn CurrentResultAuthority>,
    inline_result_bytes: usize,
}

impl CanonicalRawResultStore {
    pub fn new(workspace: ArtifactV2Workspace, authority: Arc<dyn CurrentResultAuthority>) -> Self {
        Self {
            workspace,
            authority,
            inline_result_bytes: DEFAULT_INLINE_RESULT_BYTES,
        }
    }

    /// Construct a store for system lifecycle cleanup only. Reads,
    /// materialization and explicit deletes remain denied because this
    /// authority resolver never authorizes a binding.
    pub fn for_lifecycle_cleanup(workspace: ArtifactV2Workspace) -> Self {
        Self::new(workspace, Arc::new(CleanupOnlyAuthority))
    }

    pub fn with_inline_result_bytes(mut self, inline_result_bytes: usize) -> Self {
        self.inline_result_bytes = inline_result_bytes.min(MAX_INLINE_RESULT_BYTES);
        self
    }

    async fn acquire_result_mutation_lock(
        &self,
        scope: &ScopeRef,
        content_ref: &ScopedResultRef,
    ) -> Result<(tokio::sync::OwnedMutexGuard<()>, FileLockGuard), CanonicalResultError> {
        let locator_path = self.locator_path(scope, content_ref)?;
        let process_guard = process_materialization_lock(content_ref.as_str())
            .lock_owned()
            .await;
        let file_guard = AgentStorage::acquire_file_lock_exclusive(&locator_path)
            .await
            .map_err(|_| CanonicalResultError::StorageUnavailable)?;
        Ok((process_guard, file_guard))
    }

    /// Persist one complete safe result. The deterministic opaque identity makes
    /// retry/replay idempotent; no shared read-modify-write index is involved.
    pub async fn materialize(
        &self,
        mut request: MaterializeRawResultRequest,
    ) -> Result<RawResultDescriptor, CanonicalResultError> {
        let now_ms = Utc::now().timestamp_millis();
        let Some(payload_shape) =
            inspect_json_bounded(&request.safe_value, MAX_MATERIALIZED_RESULT_NODES)
        else {
            let rejected = std::mem::replace(&mut request.safe_value, Value::Null);
            discard_json_iteratively(rejected);
            return Err(CanonicalResultError::InvalidRequest {
                code: "result_nodes_exceeded",
            });
        };
        if payload_shape.max_depth > MAX_RETAINED_JSON_DEPTH {
            // The rejected payload can itself be adversarially deep. Drain it
            // explicitly so returning this error does not invoke recursive
            // `serde_json::Value` drop glue on the current runtime worker.
            let rejected = std::mem::replace(&mut request.safe_value, Value::Null);
            discard_json_iteratively(rejected);
            return Err(CanonicalResultError::InvalidRequest {
                code: "result_depth_exceeded",
            });
        }
        if let Err(error) = validate_materialize_request(&request, now_ms) {
            let rejected = std::mem::replace(&mut request.safe_value, Value::Null);
            discard_json_iteratively(rejected);
            return Err(error);
        }
        let measured_bytes = match json_encoded_len(&request.safe_value) {
            Ok(measured_bytes) => measured_bytes,
            Err(_) => {
                let rejected = std::mem::replace(&mut request.safe_value, Value::Null);
                discard_json_iteratively(rejected);
                return Err(CanonicalResultError::InvalidRequest {
                    code: "result_not_serializable",
                });
            },
        };
        if u64::try_from(measured_bytes).unwrap_or(u64::MAX) > MAX_MATERIALIZED_RESULT_BYTES {
            let rejected = std::mem::replace(&mut request.safe_value, Value::Null);
            discard_json_iteratively(rejected);
            return Err(CanonicalResultError::InvalidRequest {
                code: "result_too_large",
            });
        }
        let binding = authority_binding(&request.owner, &request.identity);
        if let Err(error) = self
            .require_current_authority(&request.scope, &binding, &request.authority_revision)
            .await
        {
            let rejected = std::mem::replace(&mut request.safe_value, Value::Null);
            discard_json_iteratively(rejected);
            return Err(error);
        }
        let _chat_lifecycle_guard = match &request.owner {
            RawResultOwner::Chat { session_id } => {
                match acquire_chat_session_lifecycle_guard_for_existing_scope(
                    &self.workspace,
                    &request.scope.principal(),
                    &request.scope.workspace(),
                    session_id,
                )
                .await
                {
                    Ok(guard) => Some(guard),
                    Err(error) => {
                        let rejected = std::mem::replace(&mut request.safe_value, Value::Null);
                        discard_json_iteratively(rejected);
                        return Err(if chat_session_lifecycle_error_is_not_found(&error) {
                            CanonicalResultError::NotFound
                        } else {
                            CanonicalResultError::StorageUnavailable
                        });
                    },
                }
            },
            _ => None,
        };
        // Expiry is an enforced read boundary, but without a production sweep
        // the payload would remain on disk forever. Schedule one best-effort
        // scope-local sweep after an authenticated result operation, rate
        // limited process-wide so normal tool latency never includes a full
        // locator scan. Chat/task lifecycle deletion still performs its exact
        // owner cleanup synchronously.
        self.schedule_expired_cleanup(&request.scope, now_ms);
        if let Err(error) = self
            .require_or_create_owner(&request.scope, &request.owner)
            .await
        {
            let rejected = std::mem::replace(&mut request.safe_value, Value::Null);
            discard_json_iteratively(rejected);
            return Err(error);
        }

        let safe_value =
            canonicalize_json_owned(std::mem::replace(&mut request.safe_value, Value::Null));
        let (content_digest, canonical_size) = match canonical_content_digest(&safe_value) {
            Ok(digest) => digest,
            Err(error) => {
                discard_json_iteratively(safe_value);
                return Err(error);
            },
        };
        if canonical_size != measured_bytes {
            discard_json_iteratively(safe_value);
            return Err(CanonicalResultError::InvalidRequest {
                code: "result_not_serializable",
            });
        }
        let reference_digest = match materialization_digest(
            &request.scope,
            &request.owner,
            &request.identity,
            &request.authority_revision,
        ) {
            Ok(digest) => digest,
            Err(error) => {
                discard_json_iteratively(safe_value);
                return Err(error);
            },
        };
        let content_ref = ScopedResultRef::from_digest(&reference_digest);
        let descriptor = RawResultDescriptor {
            schema_version: RAW_RESULT_SCHEMA_VERSION,
            content_ref: content_ref.clone(),
            content_hash: content_digest,
            media_type: request.media_type.trim().to_string(),
            size_bytes: u64::try_from(canonical_size).unwrap_or(u64::MAX),
            retention_class: request.retention_class,
            expires_at_ms: request.expires_at_ms,
        };
        let locator_path = match self.locator_path(&request.scope, &content_ref) {
            Ok(path) => path,
            Err(error) => {
                discard_json_iteratively(safe_value);
                return Err(error);
            },
        };
        // Serialize the complete same-identity read/verify-or-publish window.
        // The process mutex is exact-keyed (no unrelated shard head-of-line
        // blocking), async, and retains only weak references. The adjacent
        // bounded file lock gives independent processes the same transaction
        // order; atomic provider writes remain each file's crash boundary.
        let _materialization_guards = match self
            .acquire_result_mutation_lock(&request.scope, &content_ref)
            .await
        {
            Ok(guards) => guards,
            Err(error) => {
                discard_json_iteratively(safe_value);
                return Err(error);
            },
        };

        let locator_exists = match self.workspace.exists_path(&locator_path).await {
            Ok(exists) => exists,
            Err(_) => {
                discard_json_iteratively(safe_value);
                return Err(CanonicalResultError::StorageUnavailable);
            },
        };
        if locator_exists {
            // The persisted descriptor is authoritative for an idempotent
            // replay. Release this request's complete tree before
            // reading/verifying the existing payload, otherwise a large replay
            // transiently retains both complete trees.
            discard_json_iteratively(safe_value);
            let mut manifest = self.load_manifest(&request.scope, &content_ref).await?;
            if manifest.owner != request.owner
                || manifest.identity != request.identity
                || manifest.authority_revision != request.authority_revision
            {
                discard_manifest_inline_payload(&mut manifest);
                return Err(CanonicalResultError::Corrupt);
            }
            if manifest.descriptor != descriptor {
                discard_manifest_inline_payload(&mut manifest);
                return Err(CanonicalResultError::IdentityConflict);
            }
            let verified = self.load_and_verify_value(&mut manifest).await?;
            discard_json_iteratively(verified);
            return Ok(descriptor);
        }

        let storage = if canonical_size <= self.inline_result_bytes {
            RawResultStorageV1::Inline { value: safe_value }
        } else {
            let payload_path = match self.payload_path(&request.scope, &request.owner, &content_ref)
            {
                Ok(path) => path,
                Err(error) => {
                    discard_json_iteratively(safe_value);
                    return Err(error);
                },
            };
            let Some(payload_parent) = payload_path.parent() else {
                discard_json_iteratively(safe_value);
                return Err(CanonicalResultError::StorageUnavailable);
            };
            if self
                .workspace
                .create_dir_all_path(payload_parent)
                .await
                .is_err()
            {
                discard_json_iteratively(safe_value);
                return Err(CanonicalResultError::StorageUnavailable);
            }
            if self
                .workspace
                .write_canonical_json_value_atomic_stream_path(&payload_path, safe_value)
                .await
                .is_err()
            {
                return Err(CanonicalResultError::StorageUnavailable);
            }
            RawResultStorageV1::File
        };

        let mut manifest = RawResultManifestV1 {
            schema_version: RAW_RESULT_SCHEMA_VERSION,
            descriptor: descriptor.clone(),
            scope: request.scope.clone(),
            scope_digest: scope_digest(&request.scope),
            owner: request.owner,
            identity: request.identity,
            authority_revision: request.authority_revision,
            created_at_ms: now_ms,
            cursor_key: new_cursor_key(),
            storage,
        };
        let manifest_path = self.owner_manifest_path(
            &request.scope,
            &manifest.owner,
            &manifest.descriptor.content_ref,
        )?;
        let unpublished_payload_path = if matches!(&manifest.storage, RawResultStorageV1::File) {
            Some(self.payload_path(
                &request.scope,
                &manifest.owner,
                &manifest.descriptor.content_ref,
            )?)
        } else {
            None
        };
        let publication: Result<(), CanonicalResultError> = async {
            self.workspace
                .create_dir_all_path(
                    manifest_path
                        .parent()
                        .ok_or(CanonicalResultError::StorageUnavailable)?,
                )
                .await
                .map_err(|_| CanonicalResultError::StorageUnavailable)?;
            // Hash the established typed wire before moving the optional deep
            // inline payload into the stack-safe canonical writer. Loading the
            // manifest later recreates the same typed wire for verification;
            // whitespace and object-key order in the file are non-authoritative.
            let locator = RawResultLocatorV1 {
                schema_version: RAW_RESULT_SCHEMA_VERSION,
                content_ref: content_ref.clone(),
                scope_digest: scope_digest(&request.scope),
                owner: manifest.owner.clone(),
                manifest_hash: persisted_manifest_hash(&manifest)?,
            };
            let manifest_wire = take_manifest_wire_value(&mut manifest)?;
            self.workspace
                .write_canonical_json_value_atomic_stream_path(&manifest_path, manifest_wire)
                .await
                .map_err(|_| CanonicalResultError::StorageUnavailable)?;
            self.workspace
                .create_dir_all_path(
                    locator_path
                        .parent()
                        .ok_or(CanonicalResultError::StorageUnavailable)?,
                )
                .await
                .map_err(|_| CanonicalResultError::StorageUnavailable)?;
            // The scope-local locator is the commit record and is always written
            // last. A crash before this point can leave only an unreachable owner
            // artifact, never an advertised partial result.
            self.workspace
                .write_json_atomic_path(&locator_path, &locator)
                .await
                .map_err(|_| CanonicalResultError::StorageUnavailable)
        }
        .await;
        discard_manifest_inline_payload(&mut manifest);
        if let Err(error) = publication {
            // The locator is the commit record. A failed publication must not
            // leave payload/manifest files that no locator-based lifecycle
            // sweep can ever discover. This exact-identity lock is still held,
            // so best-effort rollback cannot race a successful publisher.
            self.rollback_unpublished_result(locator_path, manifest_path, unpublished_payload_path)
                .await;
            return Err(error);
        }

        Ok(descriptor)
    }

    fn schedule_expired_cleanup(&self, scope: &ScopeRef, now_ms: i64) {
        let scope_key = format!(
            "{}\0{}",
            self.workspace.base_root().display(),
            scope_digest(scope)
        );
        let should_schedule = expired_cleanup_schedule()
            .lock()
            .ok()
            .map(|mut schedule| {
                let due = schedule
                    .get(&scope_key)
                    .is_none_or(|last| now_ms.saturating_sub(*last) >= EXPIRED_CLEANUP_INTERVAL_MS);
                if due {
                    schedule.insert(scope_key, now_ms);
                }
                due
            })
            .unwrap_or(false);
        if !should_schedule {
            return;
        }

        let store = Self::for_lifecycle_cleanup(self.workspace.clone());
        let scope = scope.clone();
        tokio::spawn(async move {
            match store
                .cleanup_expired(&scope, Utc::now().timestamp_millis())
                .await
            {
                Ok(report)
                    if report.removed > 0 || report.failed > 0 || report.corrupt_manifests > 0 =>
                {
                    tracing::info!(
                        principal = %scope.principal(),
                        workspace = %scope.workspace(),
                        removed = report.removed,
                        failed = report.failed,
                        corrupt_manifests = report.corrupt_manifests,
                        "canonical tool-result expiry sweep completed"
                    );
                },
                Ok(_) => {},
                Err(error) => tracing::warn!(
                    principal = %scope.principal(),
                    workspace = %scope.workspace(),
                    error = %error,
                    "canonical tool-result expiry sweep failed"
                ),
            }
        });
    }

    pub async fn read(
        &self,
        context: &RawResultReadContext,
        request: &RawResultReadRequest,
    ) -> Result<RawResultReadPage, CanonicalResultError> {
        self.read_at(context, request, Utc::now().timestamp_millis())
            .await
    }

    async fn read_at(
        &self,
        context: &RawResultReadContext,
        request: &RawResultReadRequest,
        now_ms: i64,
    ) -> Result<RawResultReadPage, CanonicalResultError> {
        validate_read_request(request)?;
        let mut manifest = self
            .load_authorized_manifest(context, &request.content_ref, now_ms)
            .await?;
        let mut raw = self.load_and_verify_value(&mut manifest).await?;
        let outcome: Result<RawResultReadPage, CanonicalResultError> = (|| {
            let paths = normalize_field_paths(&request.field_paths)?;
            let selection_digest = field_selection_digest(&paths);
            let mut total_records = 0usize;
            visit_selected_entries(&raw, &paths, |_| {
                total_records = total_records.saturating_add(1);
                Ok(true)
            })?;
            let start = match request.cursor.as_deref() {
                Some(cursor) => {
                    self.decode_cursor(&manifest, cursor, &selection_digest, total_records, now_ms)?
                },
                None => 0,
            };

            let mut page = Vec::new();
            let mut page_bytes = 0usize;
            let mut entry_offset = 0usize;
            let visited = visit_selected_entries(&raw, &paths, |entry| {
                if entry_offset < start {
                    entry_offset = entry_offset.saturating_add(1);
                    return Ok(true);
                }
                if page.len() >= request.max_records {
                    return Ok(false);
                }
                let mut entry = entry.into_owned();
                let entry_bytes = match compact_read_entry_len(&entry) {
                    Ok(entry_bytes) => entry_bytes,
                    Err(error) => {
                        discard_json_iteratively(std::mem::replace(&mut entry.value, Value::Null));
                        return Err(error);
                    },
                };
                if page.is_empty() && entry_bytes > request.max_serialized_bytes {
                    discard_read_entry(entry);
                    return Err(CanonicalResultError::RecordTooLarge);
                }
                if page_bytes.saturating_add(entry_bytes) > request.max_serialized_bytes {
                    discard_read_entry(entry);
                    return Ok(false);
                }
                page_bytes = page_bytes.saturating_add(entry_bytes);
                page.push(entry);
                entry_offset = entry_offset.saturating_add(1);
                Ok(true)
            });
            if let Err(error) = visited {
                discard_read_entries(&mut page);
                return Err(error);
            }
            let next_offset = start.saturating_add(page.len());
            let next_cursor = if next_offset < total_records {
                match self.encode_cursor(&manifest, &selection_digest, next_offset, now_ms) {
                    Ok(cursor) => Some(cursor),
                    Err(error) => {
                        discard_read_entries(&mut page);
                        return Err(error);
                    },
                }
            } else {
                None
            };

            Ok(RawResultReadPage {
                schema_version: RAW_RESULT_SCHEMA_VERSION,
                reconstruction_version: RAW_RESULT_RECONSTRUCTION_VERSION,
                content_ref: request.content_ref.clone(),
                content_hash: manifest.descriptor.content_hash.clone(),
                selection_paths: paths,
                entries: page,
                page_start: start,
                total_records,
                total_entries: total_records,
                next_cursor,
            })
        })();
        discard_json_iteratively(std::mem::replace(&mut raw, Value::Null));
        outcome
    }

    /// Authorized explicit deletion. Lifecycle sweepers may instead use
    /// `cleanup_owner` or `cleanup_expired`, which do not require a now-revoked
    /// invocation to remain authorized merely to erase data.
    pub async fn delete(
        &self,
        context: &RawResultReadContext,
        content_ref: &ScopedResultRef,
    ) -> Result<(), CanonicalResultError> {
        let _mutation_guards = self
            .acquire_result_mutation_lock(&context.scope, content_ref)
            .await?;
        let mut manifest = self
            .load_authorized_manifest(context, content_ref, Utc::now().timestamp_millis())
            .await?;
        let removed = self.remove_manifest_and_payload(&manifest).await;
        discard_manifest_inline_payload(&mut manifest);
        removed
    }

    /// System lifecycle cleanup for every reference owned by one deleted chat,
    /// task/run, or ephemeral voice session.
    pub async fn cleanup_owner(
        &self,
        scope: &ScopeRef,
        owner: &RawResultOwner,
    ) -> Result<RawResultCleanupReport, CanonicalResultError> {
        validate_owner(owner)?;
        self.cleanup_owner_matching(scope, |candidate| candidate == owner)
            .await
    }

    /// Remove all execution-scoped and task-root results for a task whose
    /// files are being permanently removed.
    pub async fn cleanup_task(
        &self,
        scope: &ScopeRef,
        task_id: &str,
    ) -> Result<RawResultCleanupReport, CanonicalResultError> {
        validate_identifier(task_id, "invalid_task_id")?;
        self.cleanup_owner_matching(scope, |candidate| {
            matches!(candidate, RawResultOwner::Task { task_id: candidate_id, .. } if candidate_id == task_id)
        })
        .await
    }

    async fn cleanup_owner_matching<F>(
        &self,
        scope: &ScopeRef,
        predicate: F,
    ) -> Result<RawResultCleanupReport, CanonicalResultError>
    where
        F: Fn(&RawResultOwner) -> bool,
    {
        let refs_dir = self.refs_dir(scope);
        let entries = self
            .workspace
            .read_dir_path_or_empty(&refs_dir)
            .await
            .map_err(|_| CanonicalResultError::StorageUnavailable)?;
        let mut report = RawResultCleanupReport::default();
        for entry in entries {
            if entry.is_dir || !entry.file_name.ends_with(".json") {
                continue;
            }
            let Some(reference_id) = entry.file_name.strip_suffix(".json") else {
                continue;
            };
            let content_ref =
                match ScopedResultRef::parse(format!("{RESULT_REF_PREFIX}{reference_id}")) {
                    Ok(content_ref) => content_ref,
                    Err(_) => {
                        report.corrupt_manifests += 1;
                        continue;
                    },
                };
            let locator_path = self.locator_path(scope, &content_ref)?;
            let _mutation_guards =
                match self.acquire_result_mutation_lock(scope, &content_ref).await {
                    Ok(guards) => guards,
                    Err(_) => {
                        report.failed += 1;
                        continue;
                    },
                };
            let locator = match self
                .read_bounded_json::<RawResultLocatorV1>(
                    locator_path.clone(),
                    MAX_RESULT_LOCATOR_BYTES,
                    MAX_RESULT_LOCATOR_NODES,
                    map_manifest_read_error,
                )
                .await
            {
                Ok(locator)
                    if locator.schema_version == RAW_RESULT_SCHEMA_VERSION
                        && locator.content_ref.as_str() == content_ref.as_str()
                        && locator.scope_digest == scope_digest(scope)
                        && is_blake3_digest(&locator.manifest_hash)
                        && validate_owner(&locator.owner).is_ok() =>
                {
                    locator
                },
                Ok(_) | Err(CanonicalResultError::Corrupt) => {
                    report.corrupt_manifests += 1;
                    continue;
                },
                Err(_) => {
                    report.failed += 1;
                    continue;
                },
            };
            if !predicate(&locator.owner) {
                continue;
            }
            match self.load_manifest(scope, &content_ref).await {
                Ok(mut manifest) => {
                    match self.remove_manifest_and_payload(&manifest).await {
                        Ok(()) => report.removed += 1,
                        Err(_) => report.failed += 1,
                    }
                    discard_manifest_inline_payload(&mut manifest);
                },
                Err(CanonicalResultError::Corrupt | CanonicalResultError::NotFound) => {
                    // The lifecycle owner may already have been deleted. Remove
                    // every path derivable from the validated locator without
                    // following data-controlled paths, then invalidate the ref.
                    report.corrupt_manifests += 1;
                    let payload_path = self.payload_path(scope, &locator.owner, &content_ref)?;
                    let manifest_path =
                        self.owner_manifest_path(scope, &locator.owner, &content_ref)?;
                    let payload_removed = self.remove_file_if_present(payload_path).await;
                    let manifest_removed = self.remove_file_if_present(manifest_path).await;
                    let locator_removed = self.remove_file_if_present(locator_path).await;
                    if payload_removed.is_ok()
                        && manifest_removed.is_ok()
                        && locator_removed.is_ok()
                    {
                        report.removed += 1;
                    } else {
                        report.failed += 1;
                    }
                },
                Err(_) => report.failed += 1,
            }
        }
        Ok(report)
    }

    /// System retention sweep. Corrupt manifests are reported and retained for
    /// operator repair rather than guessed at or followed as paths.
    pub async fn cleanup_expired(
        &self,
        scope: &ScopeRef,
        now_ms: i64,
    ) -> Result<RawResultCleanupReport, CanonicalResultError> {
        self.cleanup_matching(scope, |manifest| {
            manifest
                .descriptor
                .expires_at_ms
                .is_some_and(|expires_at| now_ms >= expires_at)
        })
        .await
    }

    async fn cleanup_matching<F>(
        &self,
        scope: &ScopeRef,
        predicate: F,
    ) -> Result<RawResultCleanupReport, CanonicalResultError>
    where
        F: Fn(&RawResultManifestV1) -> bool,
    {
        let refs_dir = self.refs_dir(scope);
        let entries = self
            .workspace
            .read_dir_path_or_empty(&refs_dir)
            .await
            .map_err(|_| CanonicalResultError::StorageUnavailable)?;
        let mut report = RawResultCleanupReport::default();
        for entry in entries {
            if entry.is_dir || !entry.file_name.ends_with(".json") {
                continue;
            }
            let Some(reference_id) = entry.file_name.strip_suffix(".json") else {
                continue;
            };
            let content_ref =
                match ScopedResultRef::parse(format!("{RESULT_REF_PREFIX}{reference_id}")) {
                    Ok(content_ref) => content_ref,
                    Err(_) => {
                        report.corrupt_manifests += 1;
                        continue;
                    },
                };
            let _mutation_guards =
                match self.acquire_result_mutation_lock(scope, &content_ref).await {
                    Ok(guards) => guards,
                    Err(_) => {
                        report.failed += 1;
                        continue;
                    },
                };
            let mut manifest = match self.load_manifest(scope, &content_ref).await {
                Ok(manifest) => manifest,
                Err(CanonicalResultError::Corrupt | CanonicalResultError::NotFound) => {
                    report.corrupt_manifests += 1;
                    continue;
                },
                Err(_) => {
                    report.failed += 1;
                    continue;
                },
            };
            if !predicate(&manifest) {
                discard_manifest_inline_payload(&mut manifest);
                continue;
            }
            match self.remove_manifest_and_payload(&manifest).await {
                Ok(()) => report.removed += 1,
                Err(_) => report.failed += 1,
            }
            discard_manifest_inline_payload(&mut manifest);
        }
        Ok(report)
    }

    async fn load_authorized_manifest(
        &self,
        context: &RawResultReadContext,
        content_ref: &ScopedResultRef,
        now_ms: i64,
    ) -> Result<RawResultManifestV1, CanonicalResultError> {
        let mut manifest = self.load_manifest(&context.scope, content_ref).await?;
        // Reference possession alone grants nothing. Mismatched bindings collapse
        // to NotFound so callers cannot probe neighboring sessions or agents.
        if manifest.owner != context.owner || manifest.identity.agent_id != context.agent_id {
            discard_manifest_inline_payload(&mut manifest);
            return Err(CanonicalResultError::NotFound);
        }
        if manifest
            .descriptor
            .expires_at_ms
            .is_some_and(|expires_at| now_ms >= expires_at)
        {
            discard_manifest_inline_payload(&mut manifest);
            return Err(CanonicalResultError::Expired);
        }
        let owner_exists = match self.owner_exists(&context.scope, &manifest.owner).await {
            Ok(owner_exists) => owner_exists,
            Err(error) => {
                discard_manifest_inline_payload(&mut manifest);
                return Err(error);
            },
        };
        if !owner_exists {
            discard_manifest_inline_payload(&mut manifest);
            return Err(CanonicalResultError::NotFound);
        }
        let authority = self
            .require_current_authority(
                &context.scope,
                &authority_binding(&manifest.owner, &manifest.identity),
                &manifest.authority_revision,
            )
            .await;
        if let Err(error) = authority {
            discard_manifest_inline_payload(&mut manifest);
            return Err(error);
        }
        Ok(manifest)
    }

    async fn require_current_authority(
        &self,
        scope: &ScopeRef,
        binding: &ResultAuthorityBinding,
        expected_revision: &str,
    ) -> Result<(), CanonicalResultError> {
        let authorized = self
            .authority
            .is_currently_authorized(scope, binding, expected_revision)
            .await
            .map_err(|_| CanonicalResultError::AuthorityUnavailable)?;
        if !authorized {
            return Err(CanonicalResultError::Revoked);
        }
        Ok(())
    }

    async fn require_or_create_owner(
        &self,
        scope: &ScopeRef,
        owner: &RawResultOwner,
    ) -> Result<(), CanonicalResultError> {
        if matches!(owner, RawResultOwner::EphemeralVoice { .. }) {
            self.workspace
                .create_dir_all_path(self.owner_anchor(scope, owner)?)
                .await
                .map_err(|_| CanonicalResultError::StorageUnavailable)?;
            return Ok(());
        }
        if !self.owner_exists(scope, owner).await? {
            return Err(CanonicalResultError::NotFound);
        }
        Ok(())
    }

    async fn owner_exists(
        &self,
        scope: &ScopeRef,
        owner: &RawResultOwner,
    ) -> Result<bool, CanonicalResultError> {
        self.workspace
            .exists_path(self.owner_anchor(scope, owner)?)
            .await
            .map_err(|_| CanonicalResultError::StorageUnavailable)
    }

    async fn load_manifest(
        &self,
        scope: &ScopeRef,
        content_ref: &ScopedResultRef,
    ) -> Result<RawResultManifestV1, CanonicalResultError> {
        let locator_path = self.locator_path(scope, content_ref)?;
        let locator = self
            .read_bounded_json::<RawResultLocatorV1>(
                locator_path,
                MAX_RESULT_LOCATOR_BYTES,
                MAX_RESULT_LOCATOR_NODES,
                map_manifest_read_error,
            )
            .await?;
        if locator.scope_digest != scope_digest(scope) {
            return Err(CanonicalResultError::NotFound);
        }
        if locator.schema_version != RAW_RESULT_SCHEMA_VERSION
            || locator.content_ref.as_str() != content_ref.as_str()
            || validate_owner(&locator.owner).is_err()
            || !is_blake3_digest(&locator.manifest_hash)
        {
            return Err(CanonicalResultError::Corrupt);
        }
        let path = self.owner_manifest_path(scope, &locator.owner, content_ref)?;
        let mut manifest = self
            .workspace
            .read_json_bounded_stream_path_with_cleanup_on_error::<RawResultManifestV1, _, _>(
                path,
                MAX_RESULT_MANIFEST_BYTES,
                MAX_RETAINED_JSON_DEPTH,
                MAX_RESULT_MANIFEST_NODES,
                discard_manifest_inline_payload,
            )
            .await
            .map_err(map_owned_manifest_read_error)?;
        let validation: Result<(), CanonicalResultError> = (|| {
            if persisted_manifest_hash(&manifest)? != locator.manifest_hash {
                return Err(CanonicalResultError::Corrupt);
            }
            if manifest.scope.principal() != scope.principal()
                || manifest.scope.workspace() != scope.workspace()
                || manifest.scope_digest != scope_digest(scope)
            {
                return Err(CanonicalResultError::NotFound);
            }
            validate_manifest_shape(&manifest, scope)?;
            if manifest.descriptor.content_ref.as_str() != content_ref.as_str()
                || manifest.owner != locator.owner
                || materialization_digest(
                    scope,
                    &manifest.owner,
                    &manifest.identity,
                    &manifest.authority_revision,
                )? != content_ref.id()?
            {
                return Err(CanonicalResultError::Corrupt);
            }
            Ok(())
        })();
        if let Err(error) = validation {
            discard_manifest_inline_payload(&mut manifest);
            return Err(error);
        }
        Ok(manifest)
    }

    async fn load_and_verify_value(
        &self,
        manifest: &mut RawResultManifestV1,
    ) -> Result<Value, CanonicalResultError> {
        match &mut manifest.storage {
            RawResultStorageV1::Inline { value } => {
                let value = canonicalize_json_owned(std::mem::replace(value, Value::Null));
                if inspect_json(&value).max_depth > MAX_RETAINED_JSON_DEPTH {
                    discard_json_iteratively(value);
                    return Err(CanonicalResultError::Corrupt);
                }
                let (digest, encoded_len) = match canonical_content_digest(&value) {
                    Ok(digest) => digest,
                    Err(_) => {
                        discard_json_iteratively(value);
                        return Err(CanonicalResultError::Corrupt);
                    },
                };
                if digest != manifest.descriptor.content_hash
                    || u64::try_from(encoded_len).unwrap_or(u64::MAX)
                        != manifest.descriptor.size_bytes
                {
                    discard_json_iteratively(value);
                    return Err(CanonicalResultError::Corrupt);
                }
                Ok(value)
            },
            RawResultStorageV1::File => {
                let path = self.payload_path(
                    &manifest.scope,
                    &manifest.owner,
                    &manifest.descriptor.content_ref,
                )?;
                let expected_size = manifest.descriptor.size_bytes;
                if expected_size > MAX_MATERIALIZED_RESULT_BYTES {
                    return Err(CanonicalResultError::Corrupt);
                }
                let value = self
                    .workspace
                    .read_verified_json_value_path(
                        &path,
                        expected_size,
                        &manifest.descriptor.content_hash,
                        MAX_MATERIALIZED_RESULT_BYTES,
                        MAX_RETAINED_JSON_DEPTH,
                        MAX_MATERIALIZED_RESULT_NODES,
                    )
                    .await
                    .map_err(map_payload_read_error)?;
                let retained = inspect_json_bounded(&value, MAX_MATERIALIZED_RESULT_NODES);
                if retained.is_none_or(|shape| shape.max_depth > MAX_RETAINED_JSON_DEPTH) {
                    let rejected = value;
                    discard_json_iteratively(rejected);
                    return Err(CanonicalResultError::Corrupt);
                }
                // File payloads are canonical when written and their exact
                // canonical byte hash was checked above; rebuilding a second
                // payload-sized canonical byte vector here adds no integrity.
                Ok(value)
            },
        }
    }

    /// Admit persisted JSON before invoking Serde. Size alone is insufficient:
    /// a small adversarial document can exhaust the runtime worker stack while
    /// Serde is recursively constructing its value/struct tree.
    async fn read_bounded_json<T>(
        &self,
        path: PathBuf,
        max_bytes: u64,
        max_nodes: usize,
        map_read_error: fn(ArtifactV2Error) -> CanonicalResultError,
    ) -> Result<T, CanonicalResultError>
    where
        T: serde::de::DeserializeOwned + Send + 'static,
    {
        self.workspace
            .read_json_bounded_stream_path::<T, _>(
                path,
                max_bytes,
                MAX_RETAINED_JSON_DEPTH,
                max_nodes,
            )
            .await
            .map_err(map_read_error)
    }

    async fn remove_manifest_and_payload(
        &self,
        manifest: &RawResultManifestV1,
    ) -> Result<(), CanonicalResultError> {
        if matches!(&manifest.storage, RawResultStorageV1::File) {
            let payload = self.payload_path(
                &manifest.scope,
                &manifest.owner,
                &manifest.descriptor.content_ref,
            )?;
            match self.workspace.remove_file_path(payload).await {
                Ok(()) => {},
                Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                },
                Err(_) => return Err(CanonicalResultError::StorageUnavailable),
            }
        }
        let manifest_path = self.owner_manifest_path(
            &manifest.scope,
            &manifest.owner,
            &manifest.descriptor.content_ref,
        )?;
        self.workspace
            .remove_file_path(manifest_path)
            .await
            .map_err(|error| match error {
                ArtifactV2Error::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    CanonicalResultError::NotFound
                },
                _ => CanonicalResultError::StorageUnavailable,
            })?;
        let locator_path = self.locator_path(&manifest.scope, &manifest.descriptor.content_ref)?;
        self.workspace
            .remove_file_path(locator_path)
            .await
            .map_err(|error| match error {
                ArtifactV2Error::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    CanonicalResultError::NotFound
                },
                _ => CanonicalResultError::StorageUnavailable,
            })
    }

    async fn remove_file_if_present(&self, path: PathBuf) -> Result<(), CanonicalResultError> {
        match self.workspace.remove_file_path(path).await {
            Ok(()) => Ok(()),
            Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(())
            },
            Err(_) => Err(CanonicalResultError::StorageUnavailable),
        }
    }

    async fn rollback_unpublished_result(
        &self,
        locator_path: PathBuf,
        manifest_path: PathBuf,
        payload_path: Option<PathBuf>,
    ) {
        // Invalidate a possibly visible commit record first, then remove the
        // unreachable owner files. Every path is derived before publication
        // from the validated scope/owner/reference identity.
        // If invalidation is indeterminate, retain the complete transaction:
        // deleting its manifest/payload could turn a locator that remained
        // visible after an ambiguous post-rename error into durable corruption.
        let locator_display = locator_path.display().to_string();
        if let Err(error) = self.remove_file_if_present(locator_path).await {
            tracing::warn!(
                locator_path = %locator_display,
                error = %error,
                "retaining complete tool-result transaction after indeterminate locator invalidation"
            );
            return;
        }
        if let Err(error) = self.remove_file_if_present(manifest_path.clone()).await {
            tracing::warn!(
                manifest_path = %manifest_path.display(),
                error = %error,
                "uncommitted tool-result manifest remains for retry or orphan repair"
            );
        }
        if let Some(payload_path) = payload_path {
            if let Err(error) = self.remove_file_if_present(payload_path.clone()).await {
                tracing::warn!(
                    payload_path = %payload_path.display(),
                    error = %error,
                    "uncommitted tool-result payload remains for retry or orphan repair"
                );
            }
        }
    }

    fn refs_dir(&self, scope: &ScopeRef) -> PathBuf {
        self.workspace
            .scope_root(&scope.principal(), &scope.workspace())
            .join("runtime")
            .join("tool_results")
            .join("refs")
            .join("v1")
    }

    fn locator_path(
        &self,
        scope: &ScopeRef,
        content_ref: &ScopedResultRef,
    ) -> Result<PathBuf, CanonicalResultError> {
        Ok(self
            .refs_dir(scope)
            .join(format!("{}.json", content_ref.id()?)))
    }

    fn owner_manifest_path(
        &self,
        scope: &ScopeRef,
        owner: &RawResultOwner,
        content_ref: &ScopedResultRef,
    ) -> Result<PathBuf, CanonicalResultError> {
        Ok(self
            .owner_result_root(scope, owner)?
            .join("manifests")
            .join(format!("{}.json", content_ref.id()?)))
    }

    fn owner_anchor(
        &self,
        scope: &ScopeRef,
        owner: &RawResultOwner,
    ) -> Result<PathBuf, CanonicalResultError> {
        validate_owner(owner)?;
        Ok(match owner {
            RawResultOwner::Chat { session_id } => {
                self.workspace
                    .chat_session_dir(&scope.principal(), &scope.workspace(), session_id)
            },
            RawResultOwner::Task {
                task_id,
                execution_id: Some(execution_id),
            } => self.workspace.execution_dir(
                &scope.principal(),
                &scope.workspace(),
                task_id,
                execution_id,
            ),
            RawResultOwner::Task {
                task_id,
                execution_id: None,
            } => self
                .workspace
                .task_dir(&scope.principal(), &scope.workspace(), task_id),
            RawResultOwner::EphemeralVoice { voice_session_id } => self
                .workspace
                .scope_root(&scope.principal(), &scope.workspace())
                .join("runtime")
                .join("tool_results")
                .join("ephemeral_voice")
                .join(stable_owner_segment(voice_session_id)),
        })
    }

    fn payload_path(
        &self,
        scope: &ScopeRef,
        owner: &RawResultOwner,
        content_ref: &ScopedResultRef,
    ) -> Result<PathBuf, CanonicalResultError> {
        Ok(self
            .owner_result_root(scope, owner)?
            .join("payloads")
            .join(format!("{}.json", content_ref.id()?)))
    }

    fn owner_result_root(
        &self,
        scope: &ScopeRef,
        owner: &RawResultOwner,
    ) -> Result<PathBuf, CanonicalResultError> {
        let root = match owner {
            RawResultOwner::Chat { session_id } => self.workspace.chat_session_outputs_dir(
                &scope.principal(),
                &scope.workspace(),
                session_id,
            ),
            RawResultOwner::Task {
                task_id,
                execution_id: Some(execution_id),
            } => self.workspace.execution_outputs_dir(
                &scope.principal(),
                &scope.workspace(),
                task_id,
                execution_id,
            ),
            RawResultOwner::Task {
                task_id,
                execution_id: None,
            } => self
                .workspace
                .task_outputs_dir(&scope.principal(), &scope.workspace(), task_id),
            RawResultOwner::EphemeralVoice { .. } => self.owner_anchor(scope, owner)?,
        };
        Ok(root.join("tool_results").join("v1"))
    }

    fn encode_cursor(
        &self,
        manifest: &RawResultManifestV1,
        selection_digest: &str,
        next_offset: usize,
        now_ms: i64,
    ) -> Result<String, CanonicalResultError> {
        let result_expiry = manifest.descriptor.expires_at_ms.unwrap_or(i64::MAX);
        let expires_at_ms = now_ms
            .saturating_add(RESULT_CURSOR_TTL_MS)
            .min(result_expiry);
        let claims = ResultCursorClaimsV1 {
            schema_version: RAW_RESULT_SCHEMA_VERSION,
            result_id: manifest.descriptor.content_ref.id()?.to_string(),
            content_hash: manifest.descriptor.content_hash.clone(),
            selection_digest: selection_digest.to_string(),
            next_offset,
            expires_at_ms,
        };
        let body = serde_json::to_vec(&claims).map_err(|_| CanonicalResultError::Corrupt)?;
        let key = cursor_key_bytes(&manifest.cursor_key)?;
        let mac = blake3::keyed_hash(&key, &body);
        Ok(format!(
            "{RESULT_CURSOR_PREFIX}{}.{}",
            URL_SAFE_NO_PAD.encode(body),
            URL_SAFE_NO_PAD.encode(mac.as_bytes())
        ))
    }

    fn decode_cursor(
        &self,
        manifest: &RawResultManifestV1,
        cursor: &str,
        selection_digest: &str,
        total_records: usize,
        now_ms: i64,
    ) -> Result<usize, CanonicalResultError> {
        if cursor.len() > MAX_RESULT_CURSOR_ENCODED_BYTES {
            return Err(CanonicalResultError::InvalidCursor);
        }
        let Some(encoded) = cursor.strip_prefix(RESULT_CURSOR_PREFIX) else {
            return Err(CanonicalResultError::InvalidCursor);
        };
        let Some((body_encoded, mac_encoded)) = encoded.split_once('.') else {
            return Err(CanonicalResultError::InvalidCursor);
        };
        let body = URL_SAFE_NO_PAD
            .decode(body_encoded)
            .map_err(|_| CanonicalResultError::InvalidCursor)?;
        let supplied_mac = URL_SAFE_NO_PAD
            .decode(mac_encoded)
            .map_err(|_| CanonicalResultError::InvalidCursor)?;
        let key = cursor_key_bytes(&manifest.cursor_key)?;
        let expected_mac = blake3::keyed_hash(&key, &body);
        if !constant_time_eq(&supplied_mac, expected_mac.as_bytes()) {
            return Err(CanonicalResultError::InvalidCursor);
        }
        let claims: ResultCursorClaimsV1 =
            serde_json::from_slice(&body).map_err(|_| CanonicalResultError::InvalidCursor)?;
        if claims.schema_version != RAW_RESULT_SCHEMA_VERSION
            || claims.result_id != manifest.descriptor.content_ref.id()?
            || claims.content_hash != manifest.descriptor.content_hash
            || claims.selection_digest != selection_digest
            || claims.next_offset >= total_records
        {
            return Err(CanonicalResultError::InvalidCursor);
        }
        if now_ms >= claims.expires_at_ms {
            return Err(CanonicalResultError::CursorExpired);
        }
        Ok(claims.next_offset)
    }
}

fn validate_materialize_request(
    request: &MaterializeRawResultRequest,
    now_ms: i64,
) -> Result<(), CanonicalResultError> {
    validate_owner(&request.owner)?;
    validate_binding_label(&request.identity.agent_id, "invalid_agent_id")?;
    validate_binding_label(&request.identity.tool_name, "invalid_tool_name")?;
    validate_binding_label(&request.identity.tool_call_id, "invalid_tool_call_id")?;
    match (
        request.identity.trust_tool.as_deref(),
        request.identity.trust_action.as_deref(),
    ) {
        (Some(tool), Some(action)) => {
            validate_binding_label(tool, "invalid_trust_tool")?;
            validate_binding_label(action, "invalid_trust_action")?;
        },
        (None, None) => {},
        _ => {
            return Err(CanonicalResultError::InvalidRequest {
                code: "incomplete_trust_coordinates",
            });
        },
    }
    if request.authority_revision.trim().is_empty() || request.authority_revision.len() > 512 {
        return Err(CanonicalResultError::InvalidRequest {
            code: "invalid_authority_revision",
        });
    }
    if request.media_type.trim().is_empty() || request.media_type.len() > 255 {
        return Err(CanonicalResultError::InvalidRequest {
            code: "invalid_media_type",
        });
    }
    if !retention_matches_owner(&request.owner, request.retention_class) {
        return Err(CanonicalResultError::InvalidRequest {
            code: "owner_retention_mismatch",
        });
    }
    if matches!(&request.owner, RawResultOwner::EphemeralVoice { .. })
        && request.expires_at_ms.is_none()
    {
        return Err(CanonicalResultError::InvalidRequest {
            code: "ephemeral_expiry_required",
        });
    }
    if request
        .expires_at_ms
        .is_some_and(|expires_at| now_ms >= expires_at)
    {
        return Err(CanonicalResultError::Expired);
    }
    Ok(())
}

fn validate_read_request(request: &RawResultReadRequest) -> Result<(), CanonicalResultError> {
    request.content_ref.id()?;
    if request.max_records == 0 || request.max_records > MAX_RESULT_PAGE_RECORDS {
        return Err(CanonicalResultError::InvalidRequest {
            code: "invalid_max_records",
        });
    }
    if request.max_serialized_bytes == 0 || request.max_serialized_bytes > MAX_RESULT_PAGE_BYTES {
        return Err(CanonicalResultError::InvalidRequest {
            code: "invalid_max_serialized_bytes",
        });
    }
    if request.field_paths.len() > MAX_RESULT_FIELD_PATHS {
        return Err(CanonicalResultError::InvalidRequest {
            code: "too_many_field_paths",
        });
    }
    if request
        .cursor
        .as_deref()
        .is_some_and(|cursor| cursor.len() > MAX_RESULT_CURSOR_ENCODED_BYTES)
    {
        return Err(CanonicalResultError::InvalidCursor);
    }
    let contains_root = request.field_paths.iter().any(String::is_empty);
    if contains_root && request.field_paths.iter().any(|path| !path.is_empty()) {
        return Err(CanonicalResultError::InvalidRequest {
            code: "root_field_path_overlap",
        });
    }
    for path in &request.field_paths {
        let bytes = path.as_bytes();
        let has_invalid_escape = bytes.iter().enumerate().any(|(index, byte)| {
            *byte == b'~'
                && !matches!(bytes.get(index + 1), Some(next) if *next == b'0' || *next == b'1')
        });
        if path.len() > MAX_RESULT_FIELD_PATH_BYTES
            || (!path.is_empty() && !path.starts_with('/'))
            || has_invalid_escape
        {
            return Err(CanonicalResultError::InvalidRequest {
                code: "invalid_field_path",
            });
        }
    }
    Ok(())
}

fn validate_manifest_shape(
    manifest: &RawResultManifestV1,
    expected_scope: &ScopeRef,
) -> Result<(), CanonicalResultError> {
    if manifest.schema_version != RAW_RESULT_SCHEMA_VERSION
        || manifest.descriptor.schema_version != RAW_RESULT_SCHEMA_VERSION
        || manifest.scope.principal() != expected_scope.principal()
        || manifest.scope.workspace() != expected_scope.workspace()
        || manifest.scope_digest != scope_digest(expected_scope)
        || manifest.descriptor.content_ref.id().is_err()
        || !is_blake3_digest(&manifest.descriptor.content_hash)
        || manifest.descriptor.size_bytes > MAX_MATERIALIZED_RESULT_BYTES
        || (matches!(&manifest.storage, RawResultStorageV1::Inline { .. })
            && manifest.descriptor.size_bytes > MAX_INLINE_RESULT_BYTES as u64)
        || manifest.descriptor.media_type.trim().is_empty()
        || cursor_key_bytes(&manifest.cursor_key).is_err()
        || !retention_matches_owner(&manifest.owner, manifest.descriptor.retention_class)
        || (matches!(&manifest.owner, RawResultOwner::EphemeralVoice { .. })
            && manifest.descriptor.expires_at_ms.is_none())
    {
        return Err(CanonicalResultError::Corrupt);
    }
    validate_owner(&manifest.owner).map_err(|_| CanonicalResultError::Corrupt)?;
    Ok(())
}

fn retention_matches_owner(owner: &RawResultOwner, retention: ResultRetentionClass) -> bool {
    matches!(
        (owner, retention),
        (
            RawResultOwner::Chat { .. },
            ResultRetentionClass::ChatLifecycle
        ) | (
            RawResultOwner::Task { .. },
            ResultRetentionClass::TaskLifecycle
        ) | (
            RawResultOwner::EphemeralVoice { .. },
            ResultRetentionClass::EphemeralVoice
        )
    )
}

fn validate_owner(owner: &RawResultOwner) -> Result<(), CanonicalResultError> {
    match owner {
        RawResultOwner::Chat { session_id } => {
            validate_identifier(session_id, "invalid_chat_session_id")
        },
        RawResultOwner::Task {
            task_id,
            execution_id,
        } => {
            ArtifactV2Workspace::validate_task_id(task_id).map_err(|_| {
                CanonicalResultError::InvalidRequest {
                    code: "invalid_task_id",
                }
            })?;
            if let Some(execution_id) = execution_id {
                validate_identifier(execution_id, "invalid_execution_id")?;
            }
            Ok(())
        },
        RawResultOwner::EphemeralVoice { voice_session_id } => {
            validate_identifier(voice_session_id, "invalid_voice_session_id")
        },
    }
}

fn validate_identifier(value: &str, code: &'static str) -> Result<(), CanonicalResultError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > 512
        || value == "."
        || value == ".."
        || value
            .chars()
            .any(|ch| ch == '/' || ch == '\\' || ch == ':' || ch.is_control())
    {
        return Err(CanonicalResultError::InvalidRequest { code });
    }
    Ok(())
}

fn validate_binding_label(value: &str, code: &'static str) -> Result<(), CanonicalResultError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > 1_024
        || value.chars().any(char::is_control)
    {
        return Err(CanonicalResultError::InvalidRequest { code });
    }
    Ok(())
}

fn authority_binding(
    owner: &RawResultOwner,
    identity: &RawResultIdentity,
) -> ResultAuthorityBinding {
    ResultAuthorityBinding::for_result(owner.clone(), identity)
}

fn materialization_digest(
    scope: &ScopeRef,
    owner: &RawResultOwner,
    identity: &RawResultIdentity,
    authority_revision: &str,
) -> Result<String, CanonicalResultError> {
    let owner = serde_json::to_vec(owner).map_err(|_| CanonicalResultError::StorageUnavailable)?;
    let identity =
        serde_json::to_vec(identity).map_err(|_| CanonicalResultError::StorageUnavailable)?;
    Ok(tagged_hash(&[
        b"canonical_raw_result_v1",
        scope.principal().as_bytes(),
        scope.workspace().as_bytes(),
        &owner,
        &identity,
        authority_revision.as_bytes(),
    ]))
}

fn scope_digest(scope: &ScopeRef) -> String {
    format!(
        "blake3:{}",
        tagged_hash(&[
            b"canonical_result_scope_v1",
            scope.principal().as_bytes(),
            scope.workspace().as_bytes(),
        ])
    )
}

fn stable_owner_segment(owner_id: &str) -> String {
    tagged_hash(&[b"canonical_result_owner_v1", owner_id.as_bytes()])
}

#[cfg(any(test, feature = "test-fixtures"))]
fn content_hash(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

fn persisted_manifest_hash(manifest: &RawResultManifestV1) -> Result<String, CanonicalResultError> {
    struct ManifestHashWriter(blake3::Hasher);

    impl std::io::Write for ManifestHashWriter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.0.update(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn write_literal(writer: &mut ManifestHashWriter, bytes: &[u8]) {
        writer.0.update(bytes);
    }

    let mut writer = ManifestHashWriter(blake3::Hasher::new());
    write_literal(&mut writer, b"{\"schema_version\":");
    serde_json::to_writer(&mut writer, &manifest.schema_version)
        .map_err(|_| CanonicalResultError::Corrupt)?;
    write_literal(&mut writer, b",\"descriptor\":");
    serde_json::to_writer(&mut writer, &manifest.descriptor)
        .map_err(|_| CanonicalResultError::Corrupt)?;
    write_literal(&mut writer, b",\"scope\":");
    serde_json::to_writer(&mut writer, &manifest.scope)
        .map_err(|_| CanonicalResultError::Corrupt)?;
    write_literal(&mut writer, b",\"scope_digest\":");
    serde_json::to_writer(&mut writer, &manifest.scope_digest)
        .map_err(|_| CanonicalResultError::Corrupt)?;
    write_literal(&mut writer, b",\"owner\":");
    serde_json::to_writer(&mut writer, &manifest.owner)
        .map_err(|_| CanonicalResultError::Corrupt)?;
    write_literal(&mut writer, b",\"identity\":");
    serde_json::to_writer(&mut writer, &manifest.identity)
        .map_err(|_| CanonicalResultError::Corrupt)?;
    write_literal(&mut writer, b",\"authority_revision\":");
    serde_json::to_writer(&mut writer, &manifest.authority_revision)
        .map_err(|_| CanonicalResultError::Corrupt)?;
    write_literal(&mut writer, b",\"created_at_ms\":");
    serde_json::to_writer(&mut writer, &manifest.created_at_ms)
        .map_err(|_| CanonicalResultError::Corrupt)?;
    write_literal(&mut writer, b",\"cursor_key\":");
    serde_json::to_writer(&mut writer, &manifest.cursor_key)
        .map_err(|_| CanonicalResultError::Corrupt)?;
    write_literal(&mut writer, b",\"storage\":");
    match &manifest.storage {
        RawResultStorageV1::Inline { value } => {
            write_literal(&mut writer, b"{\"kind\":\"inline\",\"value\":");
            write_canonical_json(value, &mut writer).map_err(|_| CanonicalResultError::Corrupt)?;
            write_literal(&mut writer, b"}");
        },
        RawResultStorageV1::File => write_literal(&mut writer, b"{\"kind\":\"file\"}"),
    }
    write_literal(&mut writer, b"}");
    Ok(format!("blake3:{}", writer.0.finalize().to_hex()))
}

fn is_blake3_digest(value: &str) -> bool {
    value.strip_prefix("blake3:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn tagged_hash(parts: &[&[u8]]) -> String {
    let mut hasher = blake3::Hasher::new();
    for part in parts {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    hasher.finalize().to_hex().to_string()
}

fn new_cursor_key() -> String {
    let mut key = [0u8; 32];
    let first = *Uuid::new_v4().as_bytes();
    let second = *Uuid::new_v4().as_bytes();
    key[..16].copy_from_slice(&first);
    key[16..].copy_from_slice(&second);
    URL_SAFE_NO_PAD.encode(key)
}

fn cursor_key_bytes(encoded: &str) -> Result<[u8; 32], CanonicalResultError> {
    let decoded = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| CanonicalResultError::Corrupt)?;
    decoded
        .try_into()
        .map_err(|_| CanonicalResultError::Corrupt)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

fn normalize_field_paths(paths: &[String]) -> Result<Vec<String>, CanonicalResultError> {
    if paths.is_empty() {
        return Ok(vec![String::new()]);
    }
    let mut normalized = BTreeSet::new();
    for path in paths {
        let bytes = path.as_bytes();
        let has_invalid_escape = bytes.iter().enumerate().any(|(index, byte)| {
            *byte == b'~'
                && !matches!(bytes.get(index + 1), Some(next) if *next == b'0' || *next == b'1')
        });
        if path.len() > MAX_RESULT_FIELD_PATH_BYTES
            || (!path.is_empty() && !path.starts_with('/'))
            || has_invalid_escape
        {
            return Err(CanonicalResultError::InvalidRequest {
                code: "invalid_field_path",
            });
        }
        normalized.insert(path.clone());
    }
    if normalized.contains("") && normalized.len() > 1 {
        return Err(CanonicalResultError::InvalidRequest {
            code: "root_field_path_overlap",
        });
    }
    Ok(normalized.into_iter().collect())
}

fn field_selection_digest(paths: &[String]) -> String {
    let parts = paths.iter().map(String::as_bytes).collect::<Vec<_>>();
    // Version the cursor selection with the reconstruction stream. A cursor
    // minted for the old one-entry root representation must never be applied
    // to the lossless fragment offsets below.
    let mut all = vec![b"canonical_result_selection_lossless_v1".as_slice()];
    all.extend(parts);
    tagged_hash(&all)
}

#[cfg(any(test, feature = "test-fixtures"))]
#[derive(Serialize)]
struct BorrowedCompleteReadEntry<'a> {
    field_path: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_index: Option<usize>,
    reconstruction_path: &'a str,
    kind: RawResultReadEntryKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    string_fragment: Option<RawResultStringFragment>,
    value: &'a Value,
}

struct SerializedLenWriter {
    bytes: usize,
}

impl std::io::Write for SerializedLenWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buffer.len());
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn write_read_entry_wire(
    writer: &mut impl std::io::Write,
    field_path: &str,
    source_index: Option<usize>,
    reconstruction_path: &str,
    kind: RawResultReadEntryKind,
    string_fragment: Option<&RawResultStringFragment>,
    value: &Value,
) -> Result<(), CanonicalResultError> {
    writer
        .write_all(b"{\"field_path\":")
        .map_err(|_| CanonicalResultError::Corrupt)?;
    serde_json::to_writer(&mut *writer, field_path).map_err(|_| CanonicalResultError::Corrupt)?;
    if let Some(source_index) = source_index {
        writer
            .write_all(b",\"source_index\":")
            .map_err(|_| CanonicalResultError::Corrupt)?;
        serde_json::to_writer(&mut *writer, &source_index)
            .map_err(|_| CanonicalResultError::Corrupt)?;
    }
    writer
        .write_all(b",\"reconstruction_path\":")
        .map_err(|_| CanonicalResultError::Corrupt)?;
    serde_json::to_writer(&mut *writer, reconstruction_path)
        .map_err(|_| CanonicalResultError::Corrupt)?;
    writer
        .write_all(b",\"kind\":")
        .map_err(|_| CanonicalResultError::Corrupt)?;
    serde_json::to_writer(&mut *writer, &kind).map_err(|_| CanonicalResultError::Corrupt)?;
    if let Some(string_fragment) = string_fragment {
        writer
            .write_all(b",\"string_fragment\":")
            .map_err(|_| CanonicalResultError::Corrupt)?;
        serde_json::to_writer(&mut *writer, string_fragment)
            .map_err(|_| CanonicalResultError::Corrupt)?;
    }
    writer
        .write_all(b",\"value\":")
        .map_err(|_| CanonicalResultError::Corrupt)?;
    write_json(value, &mut *writer).map_err(|_| CanonicalResultError::Corrupt)?;
    writer
        .write_all(b"}")
        .map_err(|_| CanonicalResultError::Corrupt)
}

fn compact_read_entry_len(value: &RawResultReadEntry) -> Result<usize, CanonicalResultError> {
    let mut writer = SerializedLenWriter { bytes: 0 };
    write_read_entry_wire(
        &mut writer,
        &value.field_path,
        value.source_index,
        &value.reconstruction_path,
        value.kind,
        value.string_fragment.as_ref(),
        &value.value,
    )?;
    Ok(writer.bytes)
}

struct BoundedSerializedLenWriter {
    bytes: usize,
    limit: usize,
    exceeded: bool,
}

impl std::io::Write for BoundedSerializedLenWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let admission = self.limit.saturating_add(1);
        if self.bytes.saturating_add(buffer.len()) >= admission {
            self.bytes = admission;
            self.exceeded = true;
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                "serialized value exceeded bounded length probe",
            ));
        }
        self.bytes = self.bytes.saturating_add(buffer.len());
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
std::thread_local! {
    static BOUNDED_LENGTH_PROBE_METRICS: std::cell::Cell<(usize, usize, usize)> =
        const { std::cell::Cell::new((0, 0, 0)) };
}

fn compact_read_entry_len_bounded(
    field_path: &str,
    source_index: Option<usize>,
    reconstruction_path: &str,
    kind: RawResultReadEntryKind,
    string_fragment: Option<&RawResultStringFragment>,
    value: &Value,
    limit: usize,
) -> Result<Option<usize>, CanonicalResultError> {
    let mut writer = BoundedSerializedLenWriter {
        bytes: 0,
        limit,
        exceeded: false,
    };
    let result = write_read_entry_wire(
        &mut writer,
        field_path,
        source_index,
        reconstruction_path,
        kind,
        string_fragment,
        value,
    );
    #[cfg(any(test, feature = "test-fixtures"))]
    BOUNDED_LENGTH_PROBE_METRICS.with(|metrics| {
        let (calls, total, maximum) = metrics.get();
        metrics.set((
            calls.saturating_add(1),
            total.saturating_add(writer.bytes),
            maximum.max(writer.bytes),
        ));
    });
    if writer.exceeded {
        return Ok(None);
    }
    result?;
    Ok(Some(writer.bytes))
}

enum SelectedReadUnit<'a> {
    Complete {
        selection_path: &'a str,
        reconstruction_path: String,
        source_index: Option<usize>,
        value: &'a Value,
    },
    Container {
        selection_path: &'a str,
        reconstruction_path: String,
        source_index: Option<usize>,
        object: bool,
    },
    StringFragment {
        selection_path: &'a str,
        reconstruction_path: String,
        source_index: Option<usize>,
        byte_start: usize,
        byte_end: usize,
        total_bytes: usize,
        value: &'a str,
    },
}

impl SelectedReadUnit<'_> {
    fn into_owned(self) -> RawResultReadEntry {
        match self {
            Self::Complete {
                selection_path,
                reconstruction_path,
                source_index,
                value,
            } => RawResultReadEntry {
                field_path: selection_path.to_string(),
                source_index,
                reconstruction_path,
                kind: RawResultReadEntryKind::CompleteValue,
                string_fragment: None,
                value: clone_json_iteratively(value),
            },
            Self::Container {
                selection_path,
                reconstruction_path,
                source_index,
                object,
            } => RawResultReadEntry {
                field_path: selection_path.to_string(),
                source_index,
                reconstruction_path,
                kind: RawResultReadEntryKind::Container,
                string_fragment: None,
                value: if object {
                    Value::Object(serde_json::Map::new())
                } else {
                    Value::Array(Vec::new())
                },
            },
            Self::StringFragment {
                selection_path,
                reconstruction_path,
                source_index,
                byte_start,
                byte_end,
                total_bytes,
                value,
            } => RawResultReadEntry {
                field_path: selection_path.to_string(),
                source_index,
                reconstruction_path,
                kind: RawResultReadEntryKind::StringFragment,
                string_fragment: Some(RawResultStringFragment {
                    byte_start,
                    byte_end,
                    total_bytes,
                }),
                value: Value::String(value.to_string()),
            },
        }
    }
}

enum ReadTraversalFrame<'a> {
    Array {
        selection_path: &'a str,
        reconstruction_path: String,
        source_index: Option<usize>,
        remaining: std::iter::Enumerate<std::slice::Iter<'a, Value>>,
    },
    Object {
        selection_path: &'a str,
        reconstruction_path: String,
        source_index: Option<usize>,
        remaining: serde_json::map::Iter<'a>,
    },
}

fn visit_selected_entries<'a>(
    raw: &'a Value,
    paths: &'a [String],
    mut visitor: impl FnMut(SelectedReadUnit<'a>) -> Result<bool, CanonicalResultError>,
) -> Result<(), CanonicalResultError> {
    for path in paths {
        let selected = if path.is_empty() {
            raw
        } else {
            raw.pointer(path)
                .ok_or(CanonicalResultError::InvalidRequest {
                    code: "field_path_not_found",
                })?
        };
        match selected {
            Value::Array(records) => {
                if records.is_empty() {
                    if !visitor(SelectedReadUnit::Complete {
                        selection_path: path,
                        reconstruction_path: path.clone(),
                        source_index: None,
                        value: selected,
                    })? {
                        return Ok(());
                    }
                } else {
                    for (source_index, value) in records.iter().enumerate() {
                        if !visit_lossless_read_entries(
                            path,
                            append_json_pointer(path, &source_index.to_string()),
                            Some(source_index),
                            value,
                            &mut visitor,
                        )? {
                            return Ok(());
                        }
                    }
                }
            },
            value => {
                if !visit_lossless_read_entries(path, path.clone(), None, value, &mut visitor)? {
                    return Ok(());
                }
            },
        }
    }
    Ok(())
}

fn visit_lossless_read_entries<'a>(
    selection_path: &'a str,
    reconstruction_path: String,
    source_index: Option<usize>,
    value: &'a Value,
    visitor: &mut impl FnMut(SelectedReadUnit<'a>) -> Result<bool, CanonicalResultError>,
) -> Result<bool, CanonicalResultError> {
    let mut pending = Some((reconstruction_path, value));
    let mut frames = Vec::<ReadTraversalFrame<'a>>::new();
    loop {
        if let Some((reconstruction_path, value)) = pending.take() {
            let complete_bytes = compact_read_entry_len_bounded(
                selection_path,
                source_index,
                &reconstruction_path,
                RawResultReadEntryKind::CompleteValue,
                None,
                value,
                MAX_COMPLETE_READ_ENTRY_BYTES,
            )?;
            if complete_bytes.is_some() {
                if !visitor(SelectedReadUnit::Complete {
                    selection_path,
                    reconstruction_path,
                    source_index,
                    value,
                })? {
                    return Ok(false);
                }
            } else {
                match value {
                    Value::Object(object) => {
                        if !visitor(SelectedReadUnit::Container {
                            selection_path,
                            reconstruction_path: reconstruction_path.clone(),
                            source_index,
                            object: true,
                        })? {
                            return Ok(false);
                        }
                        frames.push(ReadTraversalFrame::Object {
                            selection_path,
                            reconstruction_path,
                            source_index,
                            remaining: object.iter(),
                        });
                    },
                    Value::Array(array) => {
                        if !visitor(SelectedReadUnit::Container {
                            selection_path,
                            reconstruction_path: reconstruction_path.clone(),
                            source_index,
                            object: false,
                        })? {
                            return Ok(false);
                        }
                        frames.push(ReadTraversalFrame::Array {
                            selection_path,
                            reconstruction_path,
                            source_index,
                            remaining: array.iter().enumerate(),
                        });
                    },
                    Value::String(text) => {
                        let total_bytes = text.len();
                        let mut byte_start = 0usize;
                        while byte_start < total_bytes {
                            let mut byte_end = byte_start
                                .saturating_add(STRING_READ_FRAGMENT_SOURCE_BYTES)
                                .min(total_bytes);
                            while byte_end > byte_start && !text.is_char_boundary(byte_end) {
                                byte_end -= 1;
                            }
                            if byte_end == byte_start {
                                byte_end = text[byte_start..]
                                    .char_indices()
                                    .nth(1)
                                    .map(|(offset, _)| byte_start + offset)
                                    .unwrap_or(total_bytes);
                            }
                            if !visitor(SelectedReadUnit::StringFragment {
                                selection_path,
                                reconstruction_path: reconstruction_path.clone(),
                                source_index,
                                byte_start,
                                byte_end,
                                total_bytes,
                                value: &text[byte_start..byte_end],
                            })? {
                                return Ok(false);
                            }
                            byte_start = byte_end;
                        }
                    },
                    Value::Null | Value::Bool(_) | Value::Number(_) => {
                        return Err(CanonicalResultError::Corrupt);
                    },
                }
            }
        }

        loop {
            let Some(frame) = frames.last_mut() else {
                return Ok(true);
            };
            match frame {
                ReadTraversalFrame::Array {
                    selection_path: frame_selection_path,
                    reconstruction_path,
                    source_index: frame_source_index,
                    remaining,
                } => {
                    if let Some((index, child)) = remaining.next() {
                        debug_assert_eq!(*frame_selection_path, selection_path);
                        pending = Some((
                            append_json_pointer(reconstruction_path, &index.to_string()),
                            child,
                        ));
                        debug_assert_eq!(*frame_source_index, source_index);
                        break;
                    }
                    frames.pop();
                },
                ReadTraversalFrame::Object {
                    selection_path: frame_selection_path,
                    reconstruction_path,
                    source_index: frame_source_index,
                    remaining,
                } => {
                    if let Some((key, child)) = remaining.next() {
                        debug_assert_eq!(*frame_selection_path, selection_path);
                        pending = Some((append_json_pointer(reconstruction_path, key), child));
                        debug_assert_eq!(*frame_source_index, source_index);
                        break;
                    }
                    frames.pop();
                },
            }
        }
    }
}

fn append_json_pointer(base: &str, token: &str) -> String {
    let escaped = token.replace('~', "~0").replace('/', "~1");
    if base.is_empty() {
        format!("/{escaped}")
    } else {
        format!("{base}/{escaped}")
    }
}

fn map_manifest_read_error(error: ArtifactV2Error) -> CanonicalResultError {
    match error {
        ArtifactV2Error::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
            CanonicalResultError::NotFound
        },
        ArtifactV2Error::InvalidRequest(_) | ArtifactV2Error::Serde(_) => {
            CanonicalResultError::Corrupt
        },
        _ => CanonicalResultError::StorageUnavailable,
    }
}

fn map_payload_read_error(error: ArtifactV2Error) -> CanonicalResultError {
    match error {
        ArtifactV2Error::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
            CanonicalResultError::Corrupt
        },
        ArtifactV2Error::InvalidRequest(_) | ArtifactV2Error::Serde(_) => {
            CanonicalResultError::Corrupt
        },
        _ => CanonicalResultError::StorageUnavailable,
    }
}

fn map_owned_manifest_read_error(error: ArtifactV2Error) -> CanonicalResultError {
    match error {
        ArtifactV2Error::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
            CanonicalResultError::Corrupt
        },
        ArtifactV2Error::InvalidRequest(_) | ArtifactV2Error::Serde(_) => {
            CanonicalResultError::Corrupt
        },
        _ => CanonicalResultError::StorageUnavailable,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;
    use tokio::sync::RwLock;

    use super::*;

    #[test]
    fn read_request_rejects_oversized_cursor_and_paths_before_storage_access() {
        let content_ref = ScopedResultRef::from_digest(&"a".repeat(64));
        let mut request = RawResultReadRequest::first_page(content_ref, 1);
        request.cursor = Some("x".repeat(MAX_RESULT_CURSOR_ENCODED_BYTES + 1));
        assert_eq!(
            validate_read_request(&request),
            Err(CanonicalResultError::InvalidCursor),
        );

        request.cursor = None;
        request.field_paths = vec![format!("/{}", "x".repeat(MAX_RESULT_FIELD_PATH_BYTES))];
        assert_eq!(
            validate_read_request(&request),
            Err(CanonicalResultError::InvalidRequest {
                code: "invalid_field_path"
            }),
        );

        request.field_paths = vec![String::new(), String::new()];
        assert!(
            validate_read_request(&request).is_ok(),
            "duplicate root selections retain the normalization contract",
        );
    }

    #[derive(Default)]
    struct TestAuthority {
        revisions: RwLock<HashMap<String, String>>,
    }

    impl TestAuthority {
        async fn set(&self, agent_id: &str, revision: &str) {
            self.revisions
                .write()
                .await
                .insert(agent_id.to_string(), revision.to_string());
        }
    }

    #[async_trait]
    impl CurrentResultAuthority for TestAuthority {
        async fn current_authority_revision(
            &self,
            _scope: &ScopeRef,
            binding: &ResultAuthorityBinding,
        ) -> Result<Option<String>, ResultAuthorityLookupError> {
            Ok(self.revisions.read().await.get(&binding.agent_id).cloned())
        }
    }

    struct TestPolicyGuard(bool);

    impl ResultReadPolicyGuard for TestPolicyGuard {
        fn permits(&self, _tool: &str, _action: &str) -> bool {
            self.0
        }
    }

    fn scope(principal: &str) -> ScopeRef {
        ScopeRef::system_internal_unauthenticated(&principal.to_string(), &"default".to_string())
    }

    fn chat_owner(session_id: &str) -> RawResultOwner {
        RawResultOwner::Chat {
            session_id: session_id.to_string(),
        }
    }

    fn identity(call_id: &str) -> RawResultIdentity {
        RawResultIdentity {
            agent_id: "personal-assistant".to_string(),
            tool_name: "fixture_tool".to_string(),
            tool_call_id: call_id.to_string(),
            trust_tool: Some("fixture_tool".to_string()),
            trust_action: Some("execute".to_string()),
        }
    }

    async fn create_chat_owner(workspace: &ArtifactV2Workspace, session_id: &str) {
        use crate::magician_v2::chat::models::{
            ChatChannel, ChatSession, ChatSessionDocument, ChatSessionStatus,
        };

        let now = Utc::now().timestamp_millis();
        let document = ChatSessionDocument {
            format_version: 2,
            session: ChatSession {
                internal_voice: None,
                id: session_id.to_string(),
                principal: "owner".to_string(),
                workspace: "default".to_string(),
                agent_id: "personal-assistant".to_string(),
                ui_thread_id: "tool-result-tests".to_string(),
                title: None,
                origin_channel: ChatChannel::web(),
                status: ChatSessionStatus::Active,
                history_lane: crate::magician_v2::history::HistoryLane::Personal,
                is_default_session: false,
                created_at: now,
                updated_at: now,
            },
            messages: Vec::new(),
            llm_history: Vec::new(),
        };
        workspace
            .ensure_chat_session_workspace("owner", "default", session_id)
            .await
            .unwrap();
        workspace
            .write_json_atomic_path(
                workspace.chat_session_path("owner", "default", session_id),
                &document,
            )
            .await
            .unwrap();
    }

    async fn chat_store(
        inline_bytes: usize,
    ) -> (
        tempfile::TempDir,
        ArtifactV2Workspace,
        Arc<TestAuthority>,
        CanonicalRawResultStore,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let authority = Arc::new(TestAuthority::default());
        authority.set("personal-assistant", "authority-1").await;
        create_chat_owner(&workspace, "chat-1").await;
        let store = CanonicalRawResultStore::new(workspace.clone(), authority.clone())
            .with_inline_result_bytes(inline_bytes);
        (temp, workspace, authority, store)
    }

    fn materialize_request(value: Value, call_id: &str) -> MaterializeRawResultRequest {
        MaterializeRawResultRequest {
            scope: scope("owner"),
            owner: chat_owner("chat-1"),
            identity: identity(call_id),
            authority_revision: "authority-1".to_string(),
            safe_value: value,
            media_type: "application/json".to_string(),
            retention_class: ResultRetentionClass::ChatLifecycle,
            expires_at_ms: None,
        }
    }

    fn read_context() -> RawResultReadContext {
        RawResultReadContext {
            scope: scope("owner"),
            owner: chat_owner("chat-1"),
            agent_id: "personal-assistant".to_string(),
        }
    }

    #[tokio::test]
    async fn request_scoped_authority_snapshot_matches_only_the_exact_binding() {
        let binding = authority_binding(&chat_owner("chat-1"), &identity("snapshot-call"));
        let snapshot = SnapshotResultAuthority::for_current_binding(
            scope("owner"),
            binding.clone(),
            "authority-1",
        )
        .unwrap();
        assert_eq!(
            snapshot
                .current_authority_revision(&scope("owner"), &binding)
                .await
                .unwrap()
                .as_deref(),
            Some("authority-1")
        );

        let other_binding = authority_binding(&chat_owner("chat-2"), &identity("snapshot-call"));
        assert_eq!(
            snapshot
                .current_authority_revision(&scope("owner"), &other_binding)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            snapshot
                .current_authority_revision(&scope("other-owner"), &binding)
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn current_read_policy_rechecks_tool_and_action_without_bearer_authority() {
        let (_temp, workspace, _authority, store) = chat_store(usize::MAX).await;
        let descriptor = store
            .materialize(materialize_request(json!({"answer": 42}), "guarded-call"))
            .await
            .unwrap();
        let owner = chat_owner("chat-1");
        let allowed = ScopedResultReadAuthority::for_current_policy(
            scope("owner"),
            owner.clone(),
            "personal-assistant",
            ["fixture_tool".to_string()],
            "unrelated-new-policy-revision",
        )
        .unwrap()
        .with_policy_guard(Arc::new(TestPolicyGuard(true)));
        let page = CanonicalRawResultStore::new(workspace.clone(), Arc::new(allowed))
            .read(
                &read_context(),
                &RawResultReadRequest::first_page(descriptor.content_ref.clone(), 10),
            )
            .await
            .unwrap();
        assert_eq!(page.entries[0].value["answer"], 42);

        let denied = ScopedResultReadAuthority::for_current_policy(
            scope("owner"),
            owner,
            "personal-assistant",
            ["fixture_tool".to_string()],
            "unrelated-new-policy-revision",
        )
        .unwrap()
        .with_policy_guard(Arc::new(TestPolicyGuard(false)));
        assert_eq!(
            CanonicalRawResultStore::new(workspace, Arc::new(denied))
                .read(
                    &read_context(),
                    &RawResultReadRequest::first_page(descriptor.content_ref.clone(), 10),
                )
                .await
                .unwrap_err(),
            CanonicalResultError::Revoked
        );

        let removed_tool = ScopedResultReadAuthority::for_current_policy(
            scope("owner"),
            chat_owner("chat-1"),
            "personal-assistant",
            ["another_tool".to_string()],
            "current-policy-revision",
        )
        .unwrap()
        .with_policy_guard(Arc::new(TestPolicyGuard(true)));
        assert_eq!(
            CanonicalRawResultStore::new(store.workspace.clone(), Arc::new(removed_tool),)
                .read(
                    &read_context(),
                    &RawResultReadRequest::first_page(descriptor.content_ref, 10),
                )
                .await
                .unwrap_err(),
            CanonicalResultError::Revoked
        );
    }

    #[tokio::test]
    async fn failed_publication_rollback_removes_every_uncommitted_result_file() {
        let (temp, workspace, _authority, store) = chat_store(0).await;
        let locator = temp.path().join("rollback-locator.json");
        let manifest = temp.path().join("rollback-manifest.json");
        let payload = temp.path().join("rollback-payload.json");
        for path in [&locator, &manifest, &payload] {
            workspace
                .write_atomic_path(path, b"uncommitted")
                .await
                .expect("rollback fixture");
        }

        store
            .rollback_unpublished_result(locator.clone(), manifest.clone(), Some(payload.clone()))
            .await;

        assert!(!locator.exists());
        assert!(!manifest.exists());
        assert!(!payload.exists());
    }

    #[tokio::test]
    async fn failed_publication_rollback_retains_transaction_when_commit_invalidation_fails() {
        let (temp, workspace, _authority, store) = chat_store(0).await;
        let locator = temp.path().join("rollback-locator-directory");
        let manifest = temp.path().join("rollback-manifest-retained.json");
        let payload = temp.path().join("rollback-payload-retained.json");
        std::fs::create_dir(&locator).expect("locator removal failure fixture");
        for path in [&manifest, &payload] {
            workspace
                .write_atomic_path(path, b"uncommitted")
                .await
                .expect("rollback fixture");
        }

        store
            .rollback_unpublished_result(locator.clone(), manifest.clone(), Some(payload.clone()))
            .await;

        assert!(locator.is_dir());
        assert!(manifest.exists());
        assert!(payload.exists());
    }

    #[tokio::test]
    async fn atomic_materialization_is_hashed_and_idempotent() {
        let (_temp, _workspace, _authority, store) = chat_store(0).await;
        let value = json!({"z": 2, "a": [{"value": "exact"}]});
        let first = store
            .materialize(materialize_request(value.clone(), "call-1"))
            .await
            .unwrap();
        let second = store
            .materialize(materialize_request(value, "call-1"))
            .await
            .unwrap();
        assert_eq!(first, second);
        assert!(first.content_hash.starts_with("blake3:"));
        assert_eq!(
            store
                .materialize(materialize_request(
                    json!({"different": "second result for the same call"}),
                    "call-1",
                ))
                .await
                .unwrap_err(),
            CanonicalResultError::IdentityConflict
        );

        let page = store
            .read(
                &read_context(),
                &RawResultReadRequest::first_page(first.content_ref.clone(), 10),
            )
            .await
            .unwrap();
        assert_eq!(page.content_hash, first.content_hash);
        assert_eq!(
            page.entries[0].value,
            json!({"a": [{"value": "exact"}], "z": 2})
        );
    }

    #[tokio::test]
    async fn default_stack_materialization_rejects_and_drains_adversarial_json_depth() {
        let (_temp, _workspace, _authority, store) = chat_store(0).await;
        let mut value = Value::String("deep evidence".to_string());
        for _ in 0..2_048 {
            value = Value::Array(vec![value]);
        }

        assert_eq!(
            store
                .materialize(materialize_request(value, "deep-call"))
                .await
                .unwrap_err(),
            CanonicalResultError::InvalidRequest {
                code: "result_depth_exceeded",
            }
        );
    }

    #[tokio::test]
    async fn concurrent_conflicting_results_cannot_replace_one_call_identity() {
        let (_temp, workspace, authority, _store) = chat_store(0).await;
        let first_store = CanonicalRawResultStore::new(workspace.clone(), authority.clone())
            .with_inline_result_bytes(0);
        let second_store =
            CanonicalRawResultStore::new(workspace, authority).with_inline_result_bytes(0);
        let (first, second) = tokio::join!(
            first_store.materialize(materialize_request(json!({"winner": 1}), "race-call")),
            second_store.materialize(materialize_request(json!({"winner": 2}), "race-call")),
        );
        let results = [first, second];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(CanonicalResultError::IdentityConflict)))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn inline_and_file_storage_share_the_same_public_read_contract() {
        let (_temp, _workspace, _authority, file_store) = chat_store(0).await;
        let inline_store = file_store.clone().with_inline_result_bytes(usize::MAX);
        let inline = inline_store
            .materialize(materialize_request(json!([1, 2, 3]), "inline-call"))
            .await
            .unwrap();
        let file = file_store
            .materialize(materialize_request(json!([1, 2, 3]), "file-call"))
            .await
            .unwrap();

        for descriptor in [inline, file] {
            let page = file_store
                .read(
                    &read_context(),
                    &RawResultReadRequest::first_page(descriptor.content_ref, 10),
                )
                .await
                .unwrap();
            assert_eq!(
                page.entries
                    .into_iter()
                    .map(|entry| entry.value)
                    .collect::<Vec<_>>(),
                vec![json!(1), json!(2), json!(3)]
            );
        }
    }

    #[tokio::test]
    async fn task_execution_owner_uses_task_lifecycle_and_owner_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let authority = Arc::new(TestAuthority::default());
        authority.set("personal-assistant", "authority-1").await;
        let owner = RawResultOwner::Task {
            task_id: "task-1".to_string(),
            execution_id: Some("execution-1".to_string()),
        };
        workspace
            .create_dir_all_path(workspace.execution_dir(
                "owner",
                "default",
                "task-1",
                "execution-1",
            ))
            .await
            .unwrap();
        let store =
            CanonicalRawResultStore::new(workspace.clone(), authority).with_inline_result_bytes(0);
        let descriptor = store
            .materialize(MaterializeRawResultRequest {
                scope: scope("owner"),
                owner: owner.clone(),
                identity: identity("task-call"),
                authority_revision: "authority-1".to_string(),
                safe_value: json!({"rows": [{"id": 1}, {"id": 2}]}),
                media_type: "application/json".to_string(),
                retention_class: ResultRetentionClass::TaskLifecycle,
                expires_at_ms: None,
            })
            .await
            .unwrap();
        assert_eq!(
            descriptor.to_projection_descriptor().retention_class,
            crate::magician_v2::tool_result_projection::ResultRetentionClass::TaskExecution
        );
        let context = RawResultReadContext {
            scope: scope("owner"),
            owner: owner.clone(),
            agent_id: "personal-assistant".to_string(),
        };
        let page = store
            .read(
                &context,
                &RawResultReadRequest {
                    content_ref: descriptor.content_ref.clone(),
                    cursor: None,
                    field_paths: vec!["/rows".to_string()],
                    max_records: 10,
                    max_serialized_bytes: DEFAULT_RESULT_PAGE_BYTES,
                },
            )
            .await
            .unwrap();
        assert_eq!(page.entries.len(), 2);

        workspace
            .remove_dir_all_path(workspace.execution_dir(
                "owner",
                "default",
                "task-1",
                "execution-1",
            ))
            .await
            .unwrap();
        let report = store.cleanup_owner(&scope("owner"), &owner).await.unwrap();
        assert_eq!(report.removed, 1);
        assert_eq!(
            store
                .read(
                    &context,
                    &RawResultReadRequest::first_page(descriptor.content_ref, 10),
                )
                .await
                .unwrap_err(),
            CanonicalResultError::NotFound
        );
    }

    #[tokio::test]
    async fn descriptor_conversion_is_explicit_and_retention_safe() {
        use crate::magician_v2::tool_result_projection::ResultRetentionClass as ProjectionRetention;

        let (_temp, _workspace, _authority, store) = chat_store(usize::MAX).await;
        let descriptor = store
            .materialize(materialize_request(
                json!({"answer": 42}),
                "projection-call",
            ))
            .await
            .unwrap();
        let projected = descriptor.to_projection_descriptor();
        assert_eq!(
            projected.content_ref.result_ref,
            descriptor.content_ref.as_str()
        );
        assert!(projected.content_ref.cursor.is_none());
        assert_eq!(projected.content_hash, descriptor.content_hash);
        assert_eq!(projected.retention_class, ProjectionRetention::ChatSession);

        let mut ephemeral = descriptor;
        ephemeral.retention_class = ResultRetentionClass::EphemeralVoice;
        assert_eq!(
            ephemeral.to_projection_descriptor().retention_class,
            ProjectionRetention::Ephemeral
        );
    }

    #[tokio::test]
    async fn deleted_chat_owner_rejects_canonical_result_publication_without_recreating_tree() {
        use crate::magician_v2::chat::storage::{ChatStore, FileChatStore};

        let (_temp, workspace, _authority, result_store) = chat_store(0).await;
        let chat_store = FileChatStore::with_workspace_layout_index(workspace.clone())
            .await
            .expect("chat index");
        chat_store
            .delete_session("chat-1")
            .await
            .expect("delete chat owner");

        assert_eq!(
            result_store
                .materialize(materialize_request(
                    json!({"late": "canonical result"}),
                    "late-after-delete",
                ))
                .await
                .expect_err("deleted chat generation is fenced"),
            CanonicalResultError::NotFound,
        );
        assert!(!workspace
            .exists_path(workspace.chat_session_dir("owner", "default", "chat-1"))
            .await
            .expect("session tree state"));
    }

    #[tokio::test]
    async fn scope_owner_and_agent_bindings_are_not_bearer_access() {
        let (_temp, workspace, authority, store) = chat_store(usize::MAX).await;
        authority.set("other-agent", "authority-1").await;
        workspace
            .create_dir_all_path(workspace.chat_session_dir("owner", "default", "chat-2"))
            .await
            .unwrap();
        let descriptor = store
            .materialize(materialize_request(json!({"private": true}), "call-scope"))
            .await
            .unwrap();

        let mut wrong_scope = read_context();
        wrong_scope.scope = scope("other-owner");
        let mut wrong_owner = read_context();
        wrong_owner.owner = chat_owner("chat-2");
        let mut wrong_agent = read_context();
        wrong_agent.agent_id = "other-agent".to_string();
        for context in [wrong_scope, wrong_owner, wrong_agent] {
            let error = store
                .read(
                    &context,
                    &RawResultReadRequest::first_page(descriptor.content_ref.clone(), 10),
                )
                .await
                .unwrap_err();
            assert_eq!(error, CanonicalResultError::NotFound);
        }
    }

    #[tokio::test]
    async fn current_authority_revision_revokes_an_existing_reference() {
        let (_temp, _workspace, authority, store) = chat_store(usize::MAX).await;
        let descriptor = store
            .materialize(materialize_request(json!({"answer": 42}), "call-revoke"))
            .await
            .unwrap();
        authority.set("personal-assistant", "authority-2").await;
        let error = store
            .read(
                &read_context(),
                &RawResultReadRequest::first_page(descriptor.content_ref, 10),
            )
            .await
            .unwrap_err();
        assert_eq!(error, CanonicalResultError::Revoked);
    }

    #[tokio::test]
    async fn field_path_pagination_is_stable_and_cursor_is_result_bound() {
        let (_temp, _workspace, _authority, store) = chat_store(usize::MAX).await;
        let first = store
            .materialize(materialize_request(
                json!({"results": [{"id": 1}, {"id": 2}, {"id": 3}], "status": "ok"}),
                "call-page-1",
            ))
            .await
            .unwrap();
        let second = store
            .materialize(materialize_request(
                json!({"results": ["other"]}),
                "call-page-2",
            ))
            .await
            .unwrap();
        let request = RawResultReadRequest {
            content_ref: first.content_ref.clone(),
            cursor: None,
            field_paths: vec!["/results".to_string()],
            max_records: 2,
            max_serialized_bytes: DEFAULT_RESULT_PAGE_BYTES,
        };
        let page_one = store.read(&read_context(), &request).await.unwrap();
        assert_eq!(page_one.page_start, 0);
        assert_eq!(page_one.total_records, 3);
        assert_eq!(page_one.entries[0].source_index, Some(0));
        assert_eq!(page_one.entries[1].source_index, Some(1));

        let page_two = store
            .read(
                &read_context(),
                &RawResultReadRequest {
                    cursor: page_one.next_cursor.clone(),
                    ..request.clone()
                },
            )
            .await
            .unwrap();
        assert_eq!(page_two.page_start, 2);
        assert_eq!(page_two.entries[0].value, json!({"id": 3}));
        assert!(page_two.next_cursor.is_none());

        let cross_result_error = store
            .read(
                &read_context(),
                &RawResultReadRequest {
                    content_ref: second.content_ref,
                    cursor: page_one.next_cursor,
                    field_paths: vec!["/results".to_string()],
                    max_records: 2,
                    max_serialized_bytes: DEFAULT_RESULT_PAGE_BYTES,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(cross_result_error, CanonicalResultError::InvalidCursor);
    }

    #[tokio::test]
    async fn large_root_string_pages_on_utf8_boundaries_and_reconstructs_exactly() {
        let (_temp, _workspace, _authority, store) = chat_store(0).await;
        let original = "नमस्ते-🧭-مرحبا-".repeat(8_000);
        let descriptor = store
            .materialize(materialize_request(
                json!(original.clone()),
                "large-scalar-call",
            ))
            .await
            .unwrap();
        let mut request = RawResultReadRequest {
            content_ref: descriptor.content_ref,
            cursor: None,
            field_paths: Vec::new(),
            max_records: 3,
            max_serialized_bytes: DEFAULT_RESULT_PAGE_BYTES,
        };
        let mut reconstructed = String::new();
        let mut expected_page_start = 0usize;
        let mut expected_byte_start = 0usize;
        loop {
            let page = store.read(&read_context(), &request).await.unwrap();
            assert_eq!(page.page_start, expected_page_start);
            assert_eq!(
                page.reconstruction_version,
                RAW_RESULT_RECONSTRUCTION_VERSION
            );
            assert_eq!(page.selection_paths, vec![String::new()]);
            assert!(!page.complete_values_only());
            for entry in &page.entries {
                assert_eq!(entry.kind, RawResultReadEntryKind::StringFragment);
                assert_eq!(entry.reconstruction_path, "");
                let fragment = entry.string_fragment.as_ref().unwrap();
                assert_eq!(fragment.byte_start, expected_byte_start);
                assert!(original.is_char_boundary(fragment.byte_start));
                assert!(original.is_char_boundary(fragment.byte_end));
                assert_eq!(fragment.total_bytes, original.len());
                let text = entry.value.as_str().unwrap();
                assert_eq!(text, &original[fragment.byte_start..fragment.byte_end]);
                reconstructed.push_str(text);
                expected_byte_start = fragment.byte_end;
            }
            expected_page_start += page.entries.len();
            match page.next_cursor {
                Some(cursor) => request.cursor = Some(cursor),
                None => {
                    assert_eq!(expected_page_start, page.total_records);
                    break;
                },
            }
        }
        assert_eq!(reconstructed, original);
    }

    #[tokio::test]
    async fn large_root_object_has_typed_preorder_fragments_with_stable_paging() {
        let (_temp, _workspace, _authority, store) = chat_store(0).await;
        let first = "α/β~🧪".repeat(12_000);
        let second = "second-value".repeat(10_000);
        let raw = json!({
            "empty": [],
            "nested": {
                "0": first.clone(),
                "slash/key~": second.clone(),
            },
            "status": "ok"
        });
        let descriptor = store
            .materialize(materialize_request(raw, "large-object-call"))
            .await
            .unwrap();
        let mut request = RawResultReadRequest {
            content_ref: descriptor.content_ref,
            cursor: None,
            field_paths: Vec::new(),
            max_records: 2,
            max_serialized_bytes: DEFAULT_RESULT_PAGE_BYTES,
        };
        let mut all = Vec::new();
        loop {
            let page = store.read(&read_context(), &request).await.unwrap();
            all.extend(page.entries);
            match page.next_cursor {
                Some(cursor) => request.cursor = Some(cursor),
                None => break,
            }
        }
        assert_eq!(all[0].kind, RawResultReadEntryKind::Container);
        assert_eq!(all[0].reconstruction_path, "");
        assert_eq!(all[0].value, json!({}));
        assert!(all.iter().any(|entry| {
            entry.reconstruction_path == "/empty"
                && entry.kind == RawResultReadEntryKind::CompleteValue
                && entry.value == json!([])
        }));
        let first_fragments = all
            .iter()
            .filter(|entry| entry.reconstruction_path == "/nested/0")
            .map(|entry| entry.value.as_str().unwrap())
            .collect::<String>();
        let second_fragments = all
            .iter()
            .filter(|entry| entry.reconstruction_path == "/nested/slash~1key~0")
            .map(|entry| entry.value.as_str().unwrap())
            .collect::<String>();
        assert_eq!(first_fragments, first);
        assert_eq!(second_fragments, second);
        assert!(all.iter().any(|entry| {
            entry.reconstruction_path == "/status"
                && entry.kind == RawResultReadEntryKind::CompleteValue
                && entry.value == json!("ok")
        }));
    }

    #[test]
    fn oversized_nested_selection_probes_only_one_threshold_per_ancestor() {
        const NESTED_CONTAINERS: usize = 24;
        let mut raw = Value::String("wide-leaf".repeat(MAX_COMPLETE_READ_ENTRY_BYTES));
        for index in (0..NESTED_CONTAINERS).rev() {
            raw = json!({(format!("level-{index}")): raw});
        }
        let paths = vec![String::new()];
        BOUNDED_LENGTH_PROBE_METRICS.with(|metrics| metrics.set((0, 0, 0)));

        let mut emitted = Vec::new();
        visit_selected_entries(&raw, &paths, |unit| {
            emitted.push(unit.into_owned());
            Ok(true)
        })
        .expect("nested selection visits");

        let (calls, total_bytes, max_bytes) =
            BOUNDED_LENGTH_PROBE_METRICS.with(|metrics| metrics.get());
        assert_eq!(calls, NESTED_CONTAINERS + 1);
        assert!(max_bytes <= MAX_COMPLETE_READ_ENTRY_BYTES + 1);
        assert!(total_bytes <= calls * (MAX_COMPLETE_READ_ENTRY_BYTES + 1));
        assert_eq!(
            emitted
                .iter()
                .filter(|entry| entry.kind == RawResultReadEntryKind::Container)
                .count(),
            NESTED_CONTAINERS
        );

        let borrowed_value = json!({"escaped": "α/β~🧪"});
        let borrowed_wire = {
            let borrowed = BorrowedCompleteReadEntry {
                field_path: "/rows/~0~1",
                source_index: Some(7),
                reconstruction_path: "/rows/7",
                kind: RawResultReadEntryKind::CompleteValue,
                string_fragment: None,
                value: &borrowed_value,
            };
            serde_json::to_vec(&borrowed).expect("borrowed entry wire")
        };
        let owned = RawResultReadEntry {
            field_path: "/rows/~0~1".to_string(),
            source_index: Some(7),
            reconstruction_path: "/rows/7".to_string(),
            kind: RawResultReadEntryKind::CompleteValue,
            string_fragment: None,
            value: borrowed_value,
        };
        assert_eq!(
            borrowed_wire,
            serde_json::to_vec(&owned).expect("owned entry wire"),
            "bounded admission must preserve the established entry wire shape",
        );
        assert_eq!(
            compact_read_entry_len(&owned).expect("iterative entry wire length"),
            serde_json::to_vec(&owned)
                .expect("legacy owned entry wire")
                .len(),
            "iterative entry accounting must preserve the exact serde wire length",
        );
    }

    #[tokio::test]
    async fn read_telemetry_is_content_free_even_when_page_contains_sensitive_values() {
        let (_temp, _workspace, _authority, store) = chat_store(usize::MAX).await;
        let descriptor = store
            .materialize(materialize_request(
                json!({"secret_field": "do-not-emit-this-value"}),
                "telemetry-content-call",
            ))
            .await
            .unwrap();
        let outcome = store
            .read(
                &read_context(),
                &RawResultReadRequest {
                    content_ref: descriptor.content_ref.clone(),
                    cursor: None,
                    field_paths: vec!["/secret_field".to_string()],
                    max_records: 10,
                    max_serialized_bytes: DEFAULT_RESULT_PAGE_BYTES,
                },
            )
            .await;
        let telemetry = content_free_read_telemetry(&outcome, 1, 12.5);
        let encoded = telemetry.to_string();
        assert!(!encoded.contains("do-not-emit-this-value"));
        assert!(!encoded.contains("secret_field"));
        assert!(!encoded.contains(descriptor.content_ref.as_str()));
        assert!(!encoded.contains(&descriptor.content_hash));
        assert_eq!(telemetry["result_reference_state"], "used");
        assert_eq!(telemetry["requested_field_count"], 1);
        assert_eq!(telemetry["returned_records"], 1);

        let page = outcome.unwrap();
        let mut expected_model_page = serde_json::to_value(&page).expect("page wire");
        let expected_authenticated_page = expected_model_page.clone();
        expected_model_page
            .as_object_mut()
            .expect("page object")
            .remove("content_hash");
        let model_envelope = model_lossless_read_success_payload(page.clone());
        assert!(model_envelope["page"].get("content_hash").is_none());
        assert_eq!(model_envelope["page"], expected_model_page);
        assert!(!model_envelope
            .to_string()
            .contains(&descriptor.content_hash));

        let envelope = lossless_read_success_payload(page);
        assert_eq!(envelope["page"], expected_authenticated_page);
        assert_eq!(envelope["status"], "ok");
        assert_eq!(envelope["lossless_reconstruction"], true);
        assert_eq!(envelope["complete_records_only"], true);
        assert_eq!(
            envelope["page"]["reconstruction_version"],
            json!(RAW_RESULT_RECONSTRUCTION_VERSION)
        );
    }

    #[tokio::test]
    async fn task_result_reference_is_isolated_to_exact_execution_owner() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let authority = Arc::new(TestAuthority::default());
        authority.set("personal-assistant", "authority-1").await;
        let owner = RawResultOwner::Task {
            task_id: "task-isolation".to_string(),
            execution_id: Some("execution-a".to_string()),
        };
        workspace
            .create_dir_all_path(workspace.execution_dir(
                "owner",
                "default",
                "task-isolation",
                "execution-a",
            ))
            .await
            .unwrap();
        workspace
            .create_dir_all_path(workspace.execution_dir(
                "owner",
                "default",
                "task-isolation",
                "execution-b",
            ))
            .await
            .unwrap();
        let store = CanonicalRawResultStore::new(workspace, authority);
        let descriptor = store
            .materialize(MaterializeRawResultRequest {
                scope: scope("owner"),
                owner,
                identity: identity("task-isolation-call"),
                authority_revision: "authority-1".to_string(),
                safe_value: json!({"private": "execution-a"}),
                media_type: "application/json".to_string(),
                retention_class: ResultRetentionClass::TaskLifecycle,
                expires_at_ms: None,
            })
            .await
            .unwrap();
        let wrong_execution = RawResultReadContext {
            scope: scope("owner"),
            owner: RawResultOwner::Task {
                task_id: "task-isolation".to_string(),
                execution_id: Some("execution-b".to_string()),
            },
            agent_id: "personal-assistant".to_string(),
        };
        assert_eq!(
            store
                .read(
                    &wrong_execution,
                    &RawResultReadRequest::first_page(descriptor.content_ref, 10),
                )
                .await
                .unwrap_err(),
            CanonicalResultError::NotFound
        );
    }

    #[test]
    fn lifecycle_retention_matrix_is_fail_closed() {
        assert!(retention_matches_owner(
            &chat_owner("chat-1"),
            ResultRetentionClass::ChatLifecycle
        ));
        assert!(!retention_matches_owner(
            &chat_owner("chat-1"),
            ResultRetentionClass::TaskLifecycle
        ));
        assert!(!retention_matches_owner(
            &RawResultOwner::Task {
                task_id: "task-1".to_string(),
                execution_id: None,
            },
            ResultRetentionClass::EphemeralVoice
        ));
        assert!(retention_matches_owner(
            &RawResultOwner::EphemeralVoice {
                voice_session_id: "voice-1".to_string(),
            },
            ResultRetentionClass::EphemeralVoice
        ));
    }

    #[test]
    fn raw_result_owner_wire_contract_is_explicit_and_surface_neutral() {
        assert_eq!(
            serde_json::to_value(chat_owner("chat-parent")).unwrap(),
            json!({"kind": "chat", "session_id": "chat-parent"})
        );
        assert_eq!(
            serde_json::to_value(RawResultOwner::Task {
                task_id: "task-child".to_string(),
                execution_id: Some("exec-child".to_string()),
            })
            .unwrap(),
            json!({
                "kind": "task",
                "task_id": "task-child",
                "execution_id": "exec-child"
            })
        );
        assert_eq!(
            serde_json::to_value(RawResultOwner::EphemeralVoice {
                voice_session_id: "voice-transient".to_string(),
            })
            .unwrap(),
            json!({
                "kind": "ephemeral_voice",
                "voice_session_id": "voice-transient"
            })
        );
    }

    #[test]
    fn manifest_admission_rejects_oversized_payload_before_file_allocation() {
        let expected_scope = scope("owner");
        let manifest = RawResultManifestV1 {
            schema_version: RAW_RESULT_SCHEMA_VERSION,
            descriptor: RawResultDescriptor {
                schema_version: RAW_RESULT_SCHEMA_VERSION,
                content_ref: ScopedResultRef::from_digest(&"a".repeat(64)),
                content_hash: content_hash(b"oversized"),
                media_type: "application/json".to_string(),
                size_bytes: MAX_MATERIALIZED_RESULT_BYTES.saturating_add(1),
                retention_class: ResultRetentionClass::ChatLifecycle,
                expires_at_ms: None,
            },
            scope: expected_scope.clone(),
            scope_digest: scope_digest(&expected_scope),
            owner: chat_owner("chat-1"),
            identity: identity("oversized-manifest"),
            authority_revision: "authority-1".to_string(),
            created_at_ms: 1,
            cursor_key: new_cursor_key(),
            storage: RawResultStorageV1::File,
        };

        assert_eq!(
            validate_manifest_shape(&manifest, &expected_scope),
            Err(CanonicalResultError::Corrupt)
        );

        let mut oversized_inline = manifest;
        oversized_inline.descriptor.size_bytes = MAX_INLINE_RESULT_BYTES as u64 + 1;
        oversized_inline.storage = RawResultStorageV1::Inline { value: Value::Null };
        assert_eq!(
            validate_manifest_shape(&oversized_inline, &expected_scope),
            Err(CanonicalResultError::Corrupt),
            "large results must use the bounded external payload path",
        );
    }

    #[test]
    fn stack_safe_manifest_hash_and_wire_preserve_the_legacy_contract() {
        let expected_scope = scope("owner");
        let manifest = RawResultManifestV1 {
            schema_version: RAW_RESULT_SCHEMA_VERSION,
            descriptor: RawResultDescriptor {
                schema_version: RAW_RESULT_SCHEMA_VERSION,
                content_ref: ScopedResultRef::from_digest(&"b".repeat(64)),
                content_hash: content_hash(br#"{"a":1,"z":2}"#),
                media_type: "application/json".to_string(),
                size_bytes: 13,
                retention_class: ResultRetentionClass::ChatLifecycle,
                expires_at_ms: Some(123_456),
            },
            scope: expected_scope.clone(),
            scope_digest: scope_digest(&expected_scope),
            owner: chat_owner("chat-wire"),
            identity: identity("manifest-wire"),
            authority_revision: "authority-wire".to_string(),
            created_at_ms: 123,
            cursor_key: new_cursor_key(),
            storage: RawResultStorageV1::Inline {
                value: canonicalize_json_owned(json!({"z": 2, "a": 1})),
            },
        };
        let legacy_hash = content_hash(&serde_json::to_vec(&manifest).expect("legacy wire"));
        assert_eq!(persisted_manifest_hash(&manifest).unwrap(), legacy_hash);

        let expected_wire = serde_json::to_value(&manifest).expect("typed manifest wire");
        let mut moved = manifest.clone();
        let moved_wire = take_manifest_wire_value(&mut moved).expect("moved manifest wire");
        assert_eq!(moved_wire, expected_wire);
        assert!(matches!(moved.storage, RawResultStorageV1::File));

        let mut file_manifest = manifest;
        discard_manifest_inline_payload(&mut file_manifest);
        file_manifest.storage = RawResultStorageV1::File;
        let legacy_file_hash =
            content_hash(&serde_json::to_vec(&file_manifest).expect("legacy file wire"));
        assert_eq!(
            persisted_manifest_hash(&file_manifest).unwrap(),
            legacy_file_hash
        );
    }

    #[test]
    fn inline_deep_result_materialization_and_read_complete_on_a_small_stack() {
        std::thread::Builder::new()
            .name("inline-result-small-stack".to_string())
            .stack_size(512 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("small-stack runtime");
                runtime.block_on(async {
                    let (_temp, _workspace, _authority, store) = chat_store(usize::MAX).await;
                    let mut payload = Value::String("leaf".to_string());
                    let retained_depth = MAX_RETAINED_JSON_DEPTH.saturating_sub(24);
                    for _ in 0..retained_depth {
                        payload = Value::Array(vec![payload]);
                    }
                    let descriptor = store
                        .materialize(materialize_request(payload, "small-stack-inline"))
                        .await
                        .expect("inline materialization");
                    let mut page = store
                        .read(
                            &read_context(),
                            &RawResultReadRequest::first_page(descriptor.content_ref, 1),
                        )
                        .await
                        .expect("inline result read");
                    assert_eq!(page.entries.len(), 1);
                    assert_eq!(
                        page.entries[0].kind,
                        RawResultReadEntryKind::CompleteValue
                    );
                    assert_eq!(page.entries[0].source_index, Some(0));
                    assert_eq!(page.entries[0].reconstruction_path, "/0");
                    assert_eq!(
                        inspect_json(&page.entries[0].value).max_depth,
                        retained_depth.saturating_sub(1),
                        "root arrays are record collections, so the first returned record has one fewer container level",
                    );
                    discard_read_entries(&mut page.entries);
                });
            })
            .expect("spawn small-stack inline result thread")
            .join()
            .expect("small-stack inline result thread");
    }

    #[tokio::test]
    async fn persisted_result_metadata_rejects_deep_json_before_serde() {
        let (temp, _workspace, _authority, store) = chat_store(0).await;
        let path = temp.path().join("deep-result-metadata.json");
        let mut body = vec![b'['; 10_000];
        body.extend(std::iter::repeat_n(b']', 10_000));
        store
            .workspace
            .write_atomic_path(&path, &body)
            .await
            .expect("adversarial metadata persists");

        let result = store
            .read_bounded_json::<RawResultLocatorV1>(
                path,
                MAX_RESULT_LOCATOR_BYTES,
                MAX_RESULT_LOCATOR_NODES,
                map_manifest_read_error,
            )
            .await;
        assert!(matches!(result, Err(CanonicalResultError::Corrupt)));
    }

    #[tokio::test]
    async fn forged_expired_corrupt_and_deleted_references_fail_safely() {
        let (_temp, workspace, _authority, store) = chat_store(0).await;
        let descriptor = store
            .materialize(materialize_request(json!([1, 2]), "call-safety"))
            .await
            .unwrap();
        let mut request = RawResultReadRequest::first_page(descriptor.content_ref.clone(), 1);
        let first = store
            .read_at(&read_context(), &request, 1_000)
            .await
            .unwrap();
        let cursor = first.next_cursor.unwrap();
        request.cursor = Some(format!("{cursor}forged"));
        assert_eq!(
            store
                .read_at(&read_context(), &request, 1_001)
                .await
                .unwrap_err(),
            CanonicalResultError::InvalidCursor
        );
        request.cursor = Some(cursor);
        assert_eq!(
            store
                .read_at(&read_context(), &request, 1_000 + RESULT_CURSOR_TTL_MS)
                .await
                .unwrap_err(),
            CanonicalResultError::CursorExpired
        );

        let manifest = store
            .load_manifest(&scope("owner"), &descriptor.content_ref)
            .await
            .unwrap();
        let payload = store
            .payload_path(&scope("owner"), &manifest.owner, &descriptor.content_ref)
            .unwrap();
        workspace
            .write_atomic_path(payload, br#"{"tampered":true}"#)
            .await
            .unwrap();
        let clean_request = RawResultReadRequest::first_page(descriptor.content_ref.clone(), 1);
        assert_eq!(
            store
                .read(&read_context(), &clean_request)
                .await
                .unwrap_err(),
            CanonicalResultError::Corrupt
        );

        store
            .delete(&read_context(), &descriptor.content_ref)
            .await
            .unwrap();
        assert_eq!(
            store
                .read(&read_context(), &clean_request)
                .await
                .unwrap_err(),
            CanonicalResultError::NotFound
        );
    }

    #[tokio::test]
    async fn ephemeral_expiry_cleanup_removes_payload_and_reference() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(temp.path());
        let authority = Arc::new(TestAuthority::default());
        authority.set("personal-assistant", "authority-1").await;
        let store = CanonicalRawResultStore::new(workspace, authority).with_inline_result_bytes(0);
        let owner = RawResultOwner::EphemeralVoice {
            voice_session_id: "voice-1".to_string(),
        };
        let descriptor = store
            .materialize(MaterializeRawResultRequest {
                scope: scope("owner"),
                owner: owner.clone(),
                identity: identity("voice-call"),
                authority_revision: "authority-1".to_string(),
                safe_value: json!({"answer": "bounded"}),
                media_type: "application/json".to_string(),
                retention_class: ResultRetentionClass::EphemeralVoice,
                expires_at_ms: Some(Utc::now().timestamp_millis() + 10_000),
            })
            .await
            .unwrap();
        let context = RawResultReadContext {
            scope: scope("owner"),
            owner: owner.clone(),
            agent_id: "personal-assistant".to_string(),
        };
        assert_eq!(
            store
                .read_at(
                    &context,
                    &RawResultReadRequest::first_page(descriptor.content_ref.clone(), 10),
                    Utc::now().timestamp_millis() + 20_000,
                )
                .await
                .unwrap_err(),
            CanonicalResultError::Expired
        );
        let report = store
            .cleanup_expired(&scope("owner"), Utc::now().timestamp_millis() + 20_000)
            .await
            .unwrap();
        assert_eq!(report.removed, 1);
        assert_eq!(
            store
                .read(
                    &context,
                    &RawResultReadRequest::first_page(descriptor.content_ref, 10)
                )
                .await
                .unwrap_err(),
            CanonicalResultError::NotFound
        );
    }
}
