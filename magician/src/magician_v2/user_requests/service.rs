//! Core `UserRequestService` — durably submit a request, either returning after
//! acceptance or blocking until the user responds / a timeout fires, with
//! first-response-wins semantics.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, RwLock};
use tracing::{debug, warn};
use zeroize::Zeroizing;

#[path = "classify.rs"]
mod classify;
/// The one-time collection clamp, shared with the agentic pause-time
/// classifier so a pause and a request agree on the window.
pub(crate) use classify::ONE_TIME_COLLECTION_MAX_SECS;
#[path = "custody.rs"]
mod custody;
#[cfg(test)]
#[path = "custody_tests.rs"]
mod custody_tests;
#[path = "sensitive.rs"]
mod sensitive;
use custody::{
    custody_hold_deadline_ms, sensitive_value_is_acceptable, PendingDeposit, SensitiveCustody,
};
use sensitive::{recorded_sensitive_channel, SECURE_CONFIRM_REQUEST, SECURE_INPUT_REQUEST};
pub use sensitive::{
    SensitiveAnswer, SensitiveAnswerStatus, SensitiveField, SensitiveInputSpec, SensitiveKind,
    SensitiveProvenance, ANDROID_NOTIFICATION_CHANNEL, SECURE_ANSWER_CHANNEL,
    VERIFICATION_CODE_RESOLVER_CHANNEL,
};

use crate::magician_v2::agents::{AgentStorage, FileLockGuard};
use crate::magician_v2::artifact_v2::io::{write_bytes_durably, write_bytes_durably_sync};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::json_traversal::{
    inspect_json_bounded, json_bytes_nesting_is_bounded, json_bytes_nodes_are_bounded,
    json_encoded_len, MAX_RETAINED_JSON_DEPTH,
};
use crate::magician_v2::realtime_events::{
    AppOwnerNotificationPublicationGeneration, HitlLifecycleState, RuntimeTransportBroadcaster,
    RuntimeTransportEvent,
};

// ---------------------------------------------------------------------------
// Public data types
// ---------------------------------------------------------------------------

/// A pending request waiting for a human response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UserRequest {
    /// Unique ID. `ask()` always replaces it with a UUID v4;
    /// `submit_nonblocking()` preserves a non-empty deterministic ID or
    /// generates a UUID when empty.
    pub id: String,
    /// Discriminator — e.g. `"tool_authorization"`, `"sandbox_override"`,
    /// `"confirmation"`, `"user_input"`, `"cannot_proceed"`.
    pub request_type: String,
    /// Human-readable question shown to the user.
    pub question: String,
    /// Buttons / choices the user can pick from.
    pub options: Vec<RequestOption>,
    /// Who to ask (principal / user id).
    pub principal: String,
    /// Workspace context for scoped delivery.
    pub workspace: String,
    /// Type-specific payload (tool name, command text, violation details, etc.).
    pub context: serde_json::Value,
    /// Originating subsystem — `"executor"`, `"chat"`, `"scheduler"`, etc.
    pub source: String,
    /// Execution context (if applicable).
    #[serde(rename = "execution_id", skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    /// Task context (if applicable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// How long to wait for a response before applying `default_on_timeout`.
    pub timeout_secs: u64,
    /// The `option.id` that is auto-selected when the timeout fires.
    pub default_on_timeout: String,
    /// Epoch-millis timestamp set by the service during first acceptance.
    pub created_at: i64,
    /// Server-owned sensitivity contract, classified once at acceptance.
    /// `None` means an ordinary request. Never carries a value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensitive: Option<SensitiveInputSpec>,
}

/// One selectable option presented to the user.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RequestOption {
    /// Machine-readable identifier — e.g. `"allow_once"`, `"deny"`.
    pub id: String,
    /// Human-readable label — e.g. `"Allow Once"`.
    pub label: String,
    /// Whether the option also requires a free-text input field.
    pub requires_input: bool,
}

/// The user's (or timeout's) answer to a `UserRequest`.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct UserResponse {
    /// Matches `UserRequest.id`.
    pub request_id: String,
    /// The chosen `RequestOption.id`.
    pub decision: String,
    /// Optional free-text (guidance, edited arguments, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    /// Which channel answered — `"web"`, `"telegram"`, `"timeout"`, etc.
    pub channel: String,
    /// Value-free status of each sensitive answer. When this is non-empty,
    /// `input` carries no sensitive value — only the ordinary fields of a
    /// mixed form, rendered as `id: value` lines. Old shards load as empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sensitive: Vec<SensitiveAnswer>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UserRequestStatus {
    Pending,
    Resolved,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UserRequestRecord {
    #[serde(flatten)]
    pub request: UserRequest,
    pub status: UserRequestStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<UserResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<i64>,
    /// Durable retry bit for the scoped generic `HitlRequested` lifecycle
    /// fact. Absent legacy records are treated as already published.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_publication_pending: Option<bool>,
    /// Durable, content-free debt indicating that the canonical scoped
    /// `HitlResolved` lifecycle fact has not yet been acknowledged by its
    /// journal owner. Absent on legacy rows, which are treated as already
    /// published so an upgrade cannot replay arbitrary historical responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_publication_pending: Option<bool>,
}

impl UserRequestRecord {
    fn pending(request: UserRequest) -> Self {
        Self {
            request,
            status: UserRequestStatus::Pending,
            response: None,
            resolved_at: None,
            request_publication_pending: None,
            resolution_publication_pending: None,
        }
    }

    fn mark_resolved(&mut self, response: UserResponse, resolved_at: i64) {
        self.status = UserRequestStatus::Resolved;
        self.response = Some(response);
        self.resolved_at = Some(resolved_at);
        self.resolution_publication_pending = None;
    }

    fn latest_activity_at(&self) -> i64 {
        self.resolved_at.unwrap_or(self.request.created_at)
    }
}

// ---------------------------------------------------------------------------
// Internal bookkeeping
// ---------------------------------------------------------------------------

struct PendingRequest {
    request: UserRequest,
    /// Private durable owner generation for app notifications. This is stored
    /// beside the request in the pending shard, never in public `context`.
    app_owner_generation: Option<String>,
    response_tx: oneshot::Sender<UserResponse>,
    // Never serialized, published, or restored. Only trusted in-process callers own the receiver.
    sensitive_tx: Option<oneshot::Sender<Zeroizing<String>>>,
    /// `None` for restored-from-disk entries whose timeout has been
    /// re-spawned out-of-band, and briefly during shared request acceptance
    /// before the timeout task is attached. `Some(handle)` otherwise.
    /// `abort()` on a `None` is a no-op.
    timeout_handle: Option<tokio::task::JoinHandle<()>>,
}

/// Pending requests keyed globally by id with bounded scope-local membership
/// indexes. Provider hot paths must never discover one tenant's <=512 rows by
/// scanning every other tenant's live requests. The canonical-owner counts
/// also make Artifact V2 path-alias rejection O(1).
#[derive(Default)]
struct PendingRequests {
    by_id: HashMap<String, PendingRequest>,
    by_scope: HashMap<(String, String), HashSet<String>>,
    by_storage_scope: HashMap<(String, String), HashMap<(String, String), usize>>,
}

impl PendingRequests {
    fn insert(&mut self, request_id: String, entry: PendingRequest) -> Option<PendingRequest> {
        let replaced = self.remove(&request_id);
        let logical_scope = (
            entry.request.principal.clone(),
            entry.request.workspace.clone(),
        );
        self.by_scope
            .entry(logical_scope.clone())
            .or_default()
            .insert(request_id.clone());
        let storage_scope =
            ArtifactV2Workspace::scope_dir_segments(&logical_scope.0, &logical_scope.1);
        *self
            .by_storage_scope
            .entry(storage_scope)
            .or_default()
            .entry(logical_scope)
            .or_default() += 1;
        self.by_id.insert(request_id, entry);
        replaced
    }

    fn remove(&mut self, request_id: &str) -> Option<PendingRequest> {
        let entry = self.by_id.remove(request_id)?;
        let logical_scope = (
            entry.request.principal.clone(),
            entry.request.workspace.clone(),
        );
        if let Some(ids) = self.by_scope.get_mut(&logical_scope) {
            ids.remove(request_id);
            if ids.is_empty() {
                self.by_scope.remove(&logical_scope);
            }
        }
        let storage_scope =
            ArtifactV2Workspace::scope_dir_segments(&logical_scope.0, &logical_scope.1);
        if let Some(owners) = self.by_storage_scope.get_mut(&storage_scope) {
            if let Some(count) = owners.get_mut(&logical_scope) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    owners.remove(&logical_scope);
                }
            }
            if owners.is_empty() {
                self.by_storage_scope.remove(&storage_scope);
            }
        }
        Some(entry)
    }

    fn get(&self, request_id: &str) -> Option<&PendingRequest> {
        self.by_id.get(request_id)
    }

    fn get_mut(&mut self, request_id: &str) -> Option<&mut PendingRequest> {
        self.by_id.get_mut(request_id)
    }

    fn contains_key(&self, request_id: &str) -> bool {
        self.by_id.contains_key(request_id)
    }

    fn values(&self) -> impl Iterator<Item = &PendingRequest> {
        self.by_id.values()
    }

    fn iter(&self) -> impl Iterator<Item = (&String, &PendingRequest)> {
        self.by_id.iter()
    }

    fn scope_values<'a>(
        &'a self,
        principal: &str,
        workspace: &str,
    ) -> impl Iterator<Item = &'a PendingRequest> {
        self.by_scope
            .get(&(principal.to_owned(), workspace.to_owned()))
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|request_id| self.by_id.get(request_id))
    }

    fn has_storage_alias(&self, principal: &str, workspace: &str) -> bool {
        let logical_scope = (principal.to_owned(), workspace.to_owned());
        let storage_scope = ArtifactV2Workspace::scope_dir_segments(principal, workspace);
        self.by_storage_scope
            .get(&storage_scope)
            .is_some_and(|owners| owners.keys().any(|owner| owner != &logical_scope))
    }
}

/// History is physically and semantically scope-owned, so keep that ownership
/// in memory as well. Scope mutations remain O(the scope ceiling) instead of
/// retaining/splicing a deployment-wide vector under the history mutex.
#[derive(Default)]
struct HistoryRecords {
    by_scope: HashMap<(String, String), Vec<UserRequestRecord>>,
    by_storage_scope: HashMap<(String, String), HashSet<(String, String)>>,
    id_scope: HashMap<String, (String, String)>,
}

impl HistoryRecords {
    fn from_records(records: Vec<UserRequestRecord>) -> Self {
        let mut history = Self::default();
        for record in records {
            history.push(record);
        }
        history
    }

    fn push(&mut self, record: UserRequestRecord) {
        let logical_scope = (
            record.request.principal.clone(),
            record.request.workspace.clone(),
        );
        let storage_scope =
            ArtifactV2Workspace::scope_dir_segments(&logical_scope.0, &logical_scope.1);
        self.by_storage_scope
            .entry(storage_scope)
            .or_default()
            .insert(logical_scope.clone());
        self.id_scope
            .insert(record.request.id.clone(), logical_scope.clone());
        self.by_scope.entry(logical_scope).or_default().push(record);
    }

    fn iter(&self) -> impl Iterator<Item = &UserRequestRecord> {
        self.by_scope.values().flat_map(|records| records.iter())
    }

    fn retain(&mut self, mut keep: impl FnMut(&UserRequestRecord) -> bool) {
        self.by_scope.retain(|_, records| {
            records.retain(|record| keep(record));
            !records.is_empty()
        });
        self.rebuild_indexes();
    }

    fn records_for_scope(&self, principal: &str, workspace: &str) -> Vec<UserRequestRecord> {
        self.by_scope
            .get(&(principal.to_owned(), workspace.to_owned()))
            .cloned()
            .unwrap_or_default()
    }

    fn record_by_id(&self, request_id: &str) -> Option<&UserRequestRecord> {
        let scope = self.id_scope.get(request_id)?;
        self.by_scope
            .get(scope)?
            .iter()
            .find(|record| record.request.id == request_id)
    }

    fn upsert_pending(&mut self, request: &UserRequest) {
        let scope = (request.principal.clone(), request.workspace.clone());
        let records = self.by_scope.entry(scope.clone()).or_default();
        upsert_pending_in_history(records, request);
        self.reindex_scope(&scope);
    }

    fn upsert_resolved(
        &mut self,
        request: &UserRequest,
        response: &UserResponse,
        resolved_at: i64,
    ) {
        let scope = (request.principal.clone(), request.workspace.clone());
        let records = self.by_scope.entry(scope.clone()).or_default();
        upsert_resolved_in_history(records, request, response, resolved_at);
        self.reindex_scope(&scope);
    }

    fn replace_scope(
        &mut self,
        principal: &str,
        workspace: &str,
        scoped_records: Vec<UserRequestRecord>,
    ) {
        let scope = (principal.to_owned(), workspace.to_owned());
        if let Some(previous) = self.by_scope.get(&scope) {
            for record in previous {
                if self.id_scope.get(&record.request.id) == Some(&scope) {
                    self.id_scope.remove(&record.request.id);
                }
            }
        }
        if scoped_records.is_empty() {
            self.by_scope.remove(&scope);
            let storage_scope = ArtifactV2Workspace::scope_dir_segments(principal, workspace);
            if let Some(owners) = self.by_storage_scope.get_mut(&storage_scope) {
                owners.remove(&scope);
                if owners.is_empty() {
                    self.by_storage_scope.remove(&storage_scope);
                }
            }
        } else {
            debug_assert!(scoped_records.iter().all(|record| {
                record.request.principal == principal && record.request.workspace == workspace
            }));
            let storage_scope = ArtifactV2Workspace::scope_dir_segments(principal, workspace);
            self.by_storage_scope
                .entry(storage_scope)
                .or_default()
                .insert(scope.clone());
            for record in &scoped_records {
                self.id_scope
                    .insert(record.request.id.clone(), scope.clone());
            }
            self.by_scope.insert(scope, scoped_records);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.by_scope.values().map(Vec::len).sum()
    }

    fn to_vec(&self) -> Vec<UserRequestRecord> {
        self.iter().cloned().collect()
    }

    fn trim_all_scopes(&mut self, limit: usize) {
        for records in self.by_scope.values_mut() {
            trim_history_scope_records(records, limit);
        }
        self.retain(|_| true);
    }

    fn has_storage_alias(&self, principal: &str, workspace: &str) -> bool {
        let logical_scope = (principal.to_owned(), workspace.to_owned());
        let storage_scope = ArtifactV2Workspace::scope_dir_segments(principal, workspace);
        self.by_storage_scope
            .get(&storage_scope)
            .is_some_and(|owners| owners.iter().any(|owner| owner != &logical_scope))
    }

    fn reindex_scope(&mut self, scope: &(String, String)) {
        if let Some(records) = self.by_scope.get(scope) {
            for record in records {
                self.id_scope
                    .insert(record.request.id.clone(), scope.clone());
            }
            let storage_scope = ArtifactV2Workspace::scope_dir_segments(&scope.0, &scope.1);
            self.by_storage_scope
                .entry(storage_scope)
                .or_default()
                .insert(scope.clone());
        }
    }

    fn rebuild_indexes(&mut self) {
        self.by_storage_scope.clear();
        self.id_scope.clear();
        for (scope, records) in &self.by_scope {
            let storage_scope = ArtifactV2Workspace::scope_dir_segments(&scope.0, &scope.1);
            self.by_storage_scope
                .entry(storage_scope)
                .or_default()
                .insert(scope.clone());
            for record in records {
                self.id_scope
                    .insert(record.request.id.clone(), scope.clone());
            }
        }
    }
}

impl std::ops::Index<usize> for HistoryRecords {
    type Output = UserRequestRecord;

    fn index(&self, index: usize) -> &Self::Output {
        self.iter()
            .nth(index)
            .expect("user request history index out of bounds")
    }
}

/// Drop entries whose `created_at` is older than this on restore.
/// Belt-and-braces for I3 — the per-entry timeout check already removes
/// expired entries, but this cap prevents a runaway producer with
/// implausibly large `timeout_secs` from growing the snapshot forever.
const MAX_RESTORE_AGE_MS: i64 = 30 * 24 * 60 * 60 * 1000; // 30 days
const MAX_USER_REQUEST_CONTEXT_NODES: usize = 32 * 1024;
const MAX_USER_REQUEST_OPTIONS: usize = 128;
const MAX_USER_REQUEST_RECORD_BYTES: usize = 512 * 1024;
const MAX_USER_REQUEST_SCOPE_RECORDS: usize = 512;
const MAX_USER_REQUEST_SCOPE_SHARD_BYTES: usize = 16 * 1024 * 1024;
const MAX_USER_REQUEST_SHARD_JSON_NODES: usize = 1_000_000;
const MAX_USER_REQUEST_LEGACY_RECORDS: usize = 4_096;
const APP_OWNER_NOTIFICATION_REPLAY_TOMBSTONE_TYPE: &str =
    "__app_owner_notification_replay_tombstone.v1";
const APP_OWNER_NOTIFICATION_CLEANUP_BATCH: usize = 32;
const GENERIC_RESOLUTION_PUBLICATION_BATCH: usize = 32;
const USER_REQUEST_RETRY_QUEUE_SCAN_LIMIT: usize = 64;
const USER_REQUEST_STORE_WRITER_LEASE_NAME: &str = "user-request-store-writer";
static USER_REQUEST_STORE_WRITER_LEASES: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();

struct UserRequestStoreWriterLease {
    target: PathBuf,
    file_guard: Option<FileLockGuard>,
}

impl Drop for UserRequestStoreWriterLease {
    fn drop(&mut self) {
        let registry = USER_REQUEST_STORE_WRITER_LEASES.get_or_init(|| Mutex::new(HashSet::new()));
        match registry.lock() {
            Ok(mut held) => {
                // Release cross-process authority while same-process authority
                // is still registered. A racing constructor either observes
                // the registry entry or waits until both owners are gone.
                drop(self.file_guard.take());
                held.remove(&self.target);
            },
            Err(_) => {
                // A poisoned registry remains fail-closed for this process;
                // still release the OS owner during teardown.
                drop(self.file_guard.take());
            },
        }
    }
}

fn user_request_store_writer_lease_target(root: &Path) -> PathBuf {
    let root = if root.as_os_str().is_empty() {
        Path::new(".")
    } else {
        root
    };
    root.join(USER_REQUEST_STORE_WRITER_LEASE_NAME)
}

fn user_request_store_writer_lease_target_for_persist_path(path: &Path) -> PathBuf {
    user_request_store_writer_lease_target(path.parent().unwrap_or_else(|| Path::new(".")))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedResponseResult {
    Accepted,
    AlreadyResolved,
    ScopeMismatch,
    PersistenceUnavailable,
}

/// Receipt returned by [`UserRequestService::submit_nonblocking`].
///
/// A replay is successful only when every caller-controlled request field is
/// exactly equal to the first accepted submission. `created_at` is excluded
/// because it is owned by the service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserRequestSubmission {
    Accepted { request_id: String },
    IdempotentReplay { request_id: String },
}

impl UserRequestSubmission {
    pub fn request_id(&self) -> &str {
        match self {
            Self::Accepted { request_id } | Self::IdempotentReplay { request_id } => request_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UserRequestSubmissionError {
    #[error("caller-supplied user request id is invalid")]
    InvalidRequestId,
    #[error("user request id `{request_id}` was already used for a different request")]
    IdempotencyConflict { request_id: String },
    #[error("durable user request persistence is unavailable")]
    PersistenceUnavailable,
    #[error("user request scope is at its bounded durable capacity")]
    ScopeCapacityExceeded,
}

/// Backward-compatible pending-shard row. Legacy JSON-array entries contain
/// only the flattened `UserRequest` fields and therefore deserialize with no
/// generation. New app rows add one private sibling field; deserializing the
/// file as `Vec<UserRequest>` still ignores that internal field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct PersistedPendingRequest {
    #[serde(flatten)]
    request: UserRequest,
    #[serde(
        default,
        rename = "__app_owner_generation_v1",
        skip_serializing_if = "Option::is_none"
    )]
    app_owner_generation: Option<String>,
}

struct RequestShardLoad<T> {
    entries: Vec<T>,
    healthy: bool,
}

struct RequestStoreLoad<T> {
    entries: Vec<T>,
    unhealthy_scopes: HashSet<(String, String)>,
    legacy_healthy: bool,
}

struct RequestShardPath {
    scope: Option<(String, String)>,
    path: PathBuf,
}

struct PendingMigrationResult {
    unhealthy_scopes: HashSet<(String, String)>,
    legacy_healthy: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UserRequestIdentityMode {
    FreshRandom,
    PreserveCallerId,
}

#[derive(Debug)]
enum ResolvedRequestCommit {
    Committed,
    AppResolutionReady { redacted_request: UserRequest },
    RetryAppCleanup { redacted_request: UserRequest },
    Rejected,
}

#[derive(Default)]
struct AppNotificationCleanupQueue {
    request_ids: VecDeque<String>,
    members: HashSet<String>,
}

#[derive(Default)]
struct GenericResolutionPublicationQueue {
    request_ids: VecDeque<String>,
    members: HashSet<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum RetryQueuePop {
    Batch(Vec<String>),
    /// Live members remain, but this bounded scan encountered only stale
    /// deque nodes. The worker must yield and continue rather than relinquish
    /// ownership and strand debt behind those nodes.
    Pending,
    Empty,
}

fn pop_bounded_retry_queue_batch(
    request_ids: &mut VecDeque<String>,
    members: &HashSet<String>,
    batch_limit: usize,
) -> RetryQueuePop {
    let mut batch = Vec::with_capacity(batch_limit);
    let mut selected = HashSet::with_capacity(batch_limit);
    let mut scanned = 0usize;
    let scan_limit = request_ids.len().min(USER_REQUEST_RETRY_QUEUE_SCAN_LIMIT);
    while batch.len() < batch_limit && scanned < scan_limit {
        let Some(request_id) = request_ids.pop_front() else {
            break;
        };
        scanned += 1;
        if !members.contains(&request_id) {
            continue;
        }
        request_ids.push_back(request_id.clone());
        if selected.insert(request_id.clone()) {
            batch.push(request_id);
        }
    }
    if !batch.is_empty() {
        RetryQueuePop::Batch(batch)
    } else if members.is_empty() && request_ids.is_empty() {
        RetryQueuePop::Empty
    } else {
        // This includes an all-stale suffix after the last live member was
        // completed. Drain it through subsequent fixed-size scans; calling
        // `VecDeque::clear` here would move unbounded destructor work back
        // under the queue mutex.
        RetryQueuePop::Pending
    }
}

impl GenericResolutionPublicationQueue {
    fn enqueue(&mut self, request_id: String) {
        if self.members.insert(request_id.clone()) {
            self.request_ids.push_back(request_id);
        }
    }

    fn pop_batch(&mut self) -> RetryQueuePop {
        pop_bounded_retry_queue_batch(
            &mut self.request_ids,
            &self.members,
            GENERIC_RESOLUTION_PUBLICATION_BATCH,
        )
    }

    fn complete(&mut self, request_id: &str) {
        self.members.remove(request_id);
    }
}

impl AppNotificationCleanupQueue {
    fn enqueue(&mut self, request_id: String) {
        if self.members.insert(request_id.clone()) {
            self.request_ids.push_back(request_id);
        }
    }

    fn pop_batch(&mut self) -> RetryQueuePop {
        pop_bounded_retry_queue_batch(
            &mut self.request_ids,
            &self.members,
            APP_OWNER_NOTIFICATION_CLEANUP_BATCH,
        )
    }

    fn complete(&mut self, request_id: &str) {
        self.members.remove(request_id);
    }
}

struct AcceptedUserRequest {
    request: UserRequest,
    receipt: UserRequestSubmission,
    response_rx: Option<oneshot::Receiver<UserResponse>>,
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// Central request/response service.
///
/// Thread-safe (`Arc`-friendly) — clone-cheap via internal `Arc<RwLock<...>>`.
pub struct UserRequestService {
    pending: Arc<RwLock<PendingRequests>>,
    event_broadcaster: Arc<RuntimeTransportBroadcaster>,
    history: Arc<Mutex<HistoryRecords>>,
    history_limit: usize,
    history_persist_path: Option<PathBuf>,
    /// Where to persist the live `pending` map so it survives magician
    /// restarts. Without this, every restart drops every in-flight
    /// `ask()` — the operator still sees the rows in `/attention`
    /// (events.jsonl is durable), but `respond_scoped` returns
    /// `AlreadyResolved` because the in-memory map is empty.
    ///
    /// When set: `ask()` writes a full snapshot of pending entries
    /// (request data only — `response_tx` and `timeout_handle` are
    /// process-local and not serialisable). On startup, the file is
    /// replayed via `with_pending_persist_path` to repopulate the map
    /// with dummy senders. Resolution carries the original `context`
    /// (incl. memory-consolidator `memory_question_key`) into history
    /// so dedup keeps working across restart boundaries.
    pending_persist_path: Option<PathBuf>,
    workspace_layout: Option<ArtifactV2Workspace>,
    /// Stable data-path identity used to derive the process-wide advisory
    /// writer sentinel. Every durable history/pending owner in one workspace
    /// shares this target; the guard itself is retained by the service and by
    /// every detached task that can still mutate those owners.
    store_writer_lease_target: Option<PathBuf>,
    store_writer_lease: Option<Arc<UserRequestStoreWriterLease>>,
    /// False only when a legacy flat owner could not be boundedly recovered or
    /// separated. Scoped shard failures live in the adjacent quarantine set so
    /// unrelated owners remain available without overwriting hidden authority.
    legacy_persistence_recovery_healthy: Arc<AtomicBool>,
    persistence_recovery_unhealthy_scopes: Arc<Mutex<HashSet<(String, String)>>>,
    app_notification_cleanup_retry_started: Arc<AtomicBool>,
    app_notification_cleanup_queue: Arc<Mutex<AppNotificationCleanupQueue>>,
    app_notification_request_publication_retry_started: Arc<AtomicBool>,
    app_notification_request_publication_queue: Arc<Mutex<GenericResolutionPublicationQueue>>,
    generic_resolution_publication_retry_started: Arc<AtomicBool>,
    generic_resolution_publication_queue: Arc<Mutex<GenericResolutionPublicationQueue>>,
    generic_request_publication_retry_started: Arc<AtomicBool>,
    generic_request_publication_queue: Arc<Mutex<GenericResolutionPublicationQueue>>,
    /// Sensitive answers awaiting their one in-process take. Never persisted.
    sensitive_custody: Arc<SensitiveCustody>,
}

impl UserRequestService {
    /// Create a new service wired to the given event broadcaster.
    pub fn new(event_broadcaster: Arc<RuntimeTransportBroadcaster>) -> Self {
        Self {
            sensitive_custody: Arc::new(SensitiveCustody::new()),
            pending: Arc::new(RwLock::new(PendingRequests::default())),
            event_broadcaster,
            history: Arc::new(Mutex::new(HistoryRecords::default())),
            history_limit: 256,
            history_persist_path: None,
            pending_persist_path: None,
            workspace_layout: None,
            store_writer_lease_target: None,
            store_writer_lease: None,
            legacy_persistence_recovery_healthy: Arc::new(AtomicBool::new(true)),
            persistence_recovery_unhealthy_scopes: Arc::new(Mutex::new(HashSet::new())),
            app_notification_cleanup_retry_started: Arc::new(AtomicBool::new(false)),
            app_notification_cleanup_queue: Arc::new(Mutex::new(
                AppNotificationCleanupQueue::default(),
            )),
            app_notification_request_publication_retry_started: Arc::new(AtomicBool::new(false)),
            app_notification_request_publication_queue: Arc::new(Mutex::new(
                GenericResolutionPublicationQueue::default(),
            )),
            generic_resolution_publication_retry_started: Arc::new(AtomicBool::new(false)),
            generic_resolution_publication_queue: Arc::new(Mutex::new(
                GenericResolutionPublicationQueue::default(),
            )),
            generic_request_publication_retry_started: Arc::new(AtomicBool::new(false)),
            generic_request_publication_queue: Arc::new(Mutex::new(
                GenericResolutionPublicationQueue::default(),
            )),
        }
    }

    pub fn with_workspace_layout(mut self, workspace_layout: ArtifactV2Workspace) -> Self {
        let target = user_request_store_writer_lease_target(workspace_layout.base_root());
        if self.workspace_layout.is_none()
            && (self.history_persist_path.is_some() || self.pending_persist_path.is_some())
        {
            self.legacy_persistence_recovery_healthy
                .store(false, Ordering::SeqCst);
            warn!(
                configured_target = %target.display(),
                "[USER-REQUEST] Ignoring workspace layout configured after a persisted owner was loaded; fenced durable mutations"
            );
            return self;
        }
        if self
            .store_writer_lease_target
            .as_ref()
            .is_some_and(|held| held != &target)
        {
            self.legacy_persistence_recovery_healthy
                .store(false, Ordering::SeqCst);
            warn!(
                configured_target = %target.display(),
                held_target = %self.store_writer_lease_target.as_ref().expect("checked").display(),
                "[USER-REQUEST] Ignoring late workspace layout because persisted owners were already bound to a different writer lease; fenced durable mutations"
            );
            return self;
        }
        // A workspace layout alone is still an in-memory UserRequest service;
        // only configuring a durable history or pending owner activates the
        // disk lease. Both persistence builders derive this workspace target
        // and acquire it before their first read.
        self.workspace_layout = Some(workspace_layout);
        self
    }

    fn ensure_store_writer_lease(&mut self, target: PathBuf) -> bool {
        if let Some(held_target) = self.store_writer_lease_target.as_ref() {
            if held_target != &target {
                self.legacy_persistence_recovery_healthy
                    .store(false, Ordering::SeqCst);
                warn!(
                    configured_target = %target.display(),
                    held_target = %held_target.display(),
                    "[USER-REQUEST] Persisted owners resolve to different writer leases; fenced durable mutations"
                );
                return false;
            }
            return self.store_writer_lease.is_some();
        }

        self.store_writer_lease_target = Some(target.clone());
        let registry = USER_REQUEST_STORE_WRITER_LEASES.get_or_init(|| Mutex::new(HashSet::new()));
        let mut held = match registry.lock() {
            Ok(held) => held,
            Err(_) => {
                self.legacy_persistence_recovery_healthy
                    .store(false, Ordering::SeqCst);
                warn!(
                    target = %target.display(),
                    "[USER-REQUEST] Durable store writer registry is poisoned; skipped recovery and fenced mutations"
                );
                return false;
            },
        };
        if held.contains(&target) {
            self.legacy_persistence_recovery_healthy
                .store(false, Ordering::SeqCst);
            warn!(
                target = %target.display(),
                "[USER-REQUEST] Another in-process service owns the durable store writer lease; skipped recovery and fenced mutations"
            );
            return false;
        }
        match AgentStorage::try_create_file_lock_exclusive_sync(&target) {
            Ok(Some(guard)) => {
                held.insert(target.clone());
                self.store_writer_lease = Some(Arc::new(UserRequestStoreWriterLease {
                    target,
                    file_guard: Some(guard),
                }));
                true
            },
            Ok(None) => {
                self.legacy_persistence_recovery_healthy
                    .store(false, Ordering::SeqCst);
                warn!(
                    target = %target.display(),
                    "[USER-REQUEST] Another upgraded process owns the durable store writer lease; skipped recovery and fenced mutations"
                );
                false
            },
            Err(error) => {
                self.legacy_persistence_recovery_healthy
                    .store(false, Ordering::SeqCst);
                warn!(
                    target = %target.display(),
                    error = %error,
                    "[USER-REQUEST] Could not acquire durable store writer lease; skipped recovery and fenced mutations"
                );
                false
            },
        }
    }

    pub fn with_history_limit(mut self, limit: usize) -> Self {
        self.history_limit = limit.clamp(1, MAX_USER_REQUEST_SCOPE_RECORDS);
        self.prune_and_persist_history();
        self
    }

    pub fn with_history_persist_path(mut self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        if let Some(configured_path) = self.history_persist_path.as_ref() {
            // Recovery is not a replaceable builder option: the first load may
            // already have installed publication debt derived from that owner.
            // Re-entering here (even with the same pathname) would reload the
            // maps without retracting those owners; a different pathname would
            // additionally split subsequent RMW authority under one root-wide
            // lease. Preserve the first binding and fence every durable write.
            self.legacy_persistence_recovery_healthy
                .store(false, Ordering::SeqCst);
            warn!(
                configured_path = %configured_path.display(),
                rejected_path = %path.display(),
                "[USER-REQUEST] Ignoring repeated history persistence configuration; fenced durable mutations"
            );
            return self;
        }
        self.history_persist_path = Some(path.clone());
        let lease_target = self.workspace_layout.as_ref().map_or_else(
            || user_request_store_writer_lease_target_for_persist_path(&path),
            |layout| user_request_store_writer_lease_target(layout.base_root()),
        );
        if !self.ensure_store_writer_lease(lease_target) {
            return self;
        }
        if !self
            .legacy_persistence_recovery_healthy
            .load(Ordering::SeqCst)
        {
            warn!(
                path = %path.display(),
                "[USER-REQUEST] Skipping history recovery because durable store ownership is fenced"
            );
            return self;
        }
        let loaded =
            load_user_request_history(self.workspace_layout.as_ref(), &path, self.history_limit);
        if !loaded.legacy_healthy {
            self.legacy_persistence_recovery_healthy
                .store(false, Ordering::SeqCst);
        }
        if !loaded.unhealthy_scopes.is_empty() {
            let flat_owner_is_coupled = self.workspace_layout.as_ref().map_or(true, |layout| {
                !matches!(layout.metadata_path_sync(&path), Ok(None))
            });
            if flat_owner_is_coupled {
                // A flat history owner couples otherwise independent scopes.
                // Until it is fully migrated, quarantining one scoped shard
                // cannot safely permit partial flat rewrites.
                self.legacy_persistence_recovery_healthy
                    .store(false, Ordering::SeqCst);
            }
        }
        self.persistence_recovery_unhealthy_scopes
            .lock()
            .expect("user request recovery-scope mutex poisoned")
            .extend(loaded.unhealthy_scopes);
        {
            let mut history = self
                .history
                .lock()
                .expect("user request history mutex poisoned");
            *history = HistoryRecords::from_records(loaded.entries);
            let recovered_history = history.to_vec();
            enqueue_restored_generic_resolution_publication_debts(
                &self.generic_resolution_publication_queue,
                &recovered_history,
            );
            enqueue_restored_generic_request_publication_debts(
                &self.generic_request_publication_queue,
                &recovered_history,
            );
        }
        self
    }

    fn scope_persistence_recovery_healthy(&self, principal: &str, workspace: &str) -> bool {
        let storage_scope =
            request_storage_scope(self.workspace_layout.as_ref(), principal, workspace);
        self.legacy_persistence_recovery_healthy
            .load(Ordering::SeqCst)
            && !self
                .persistence_recovery_unhealthy_scopes
                .lock()
                .expect("user request recovery-scope mutex poisoned")
                .contains(&storage_scope)
    }

    fn has_unhealthy_recovery_scopes(&self) -> bool {
        !self
            .persistence_recovery_unhealthy_scopes
            .lock()
            .expect("user request recovery-scope mutex poisoned")
            .is_empty()
    }

    /// Replay the persisted `pending` snapshot from disk and repopulate
    /// the in-memory map so post-restart `respond_scoped` can resolve
    /// real entries (carrying their original `context` — needed for
    /// memory-consolidator dedup via `memory_question_key`).
    ///
    /// For each restored entry:
    /// 1. Drop entries older than `MAX_RESTORE_AGE_MS` (defensive cap
    ///    against unbounded snapshot growth), except app-owner notifications
    ///    whose host-sealed absolute deadline is still live.
    /// 2. Compute remaining timeout (`created_at + timeout_secs*1000 - now`).
    ///    If already expired, fire a synthetic timeout right now
    ///    (records the default-on-timeout response in history, emits
    ///    `HitlResolved`) and skip re-inserting — keeps semantics
    ///    aligned with what would have happened had the process not
    ///    crashed.
    /// 3. Otherwise insert with a throw-away `oneshot::Sender` (the
    ///    original caller's receiver died with the prior process), republish
    ///    `HitlRequested`, and spawn a timeout task with the remaining time.
    /// 4. Upsert the restored request back to `Pending` in history —
    ///    `with_history_persist_path` runs first and
    ///    `normalize_restored_history_records` would otherwise leave the row
    ///    marked as fake-resolved with `channel: "service_restart"` until the
    ///    operator answered.
    pub async fn with_pending_persist_path(mut self, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        if let Some(configured_path) = self.pending_persist_path.as_ref() {
            // The first recovery may already own detached timeout and
            // publication tasks. Re-entering cannot revoke those writers, so
            // never replace or reload their durable owner in-place.
            self.legacy_persistence_recovery_healthy
                .store(false, Ordering::SeqCst);
            warn!(
                configured_path = %configured_path.display(),
                rejected_path = %path.display(),
                "[USER-REQUEST] Ignoring repeated pending persistence configuration; fenced durable mutations"
            );
            return self;
        }
        self.pending_persist_path = Some(path.clone());
        let lease_target = self.workspace_layout.as_ref().map_or_else(
            || user_request_store_writer_lease_target_for_persist_path(&path),
            |layout| user_request_store_writer_lease_target(layout.base_root()),
        );
        if !self.ensure_store_writer_lease(lease_target) {
            return self;
        }

        // IM1 — order guard. The I1 revert below in-place rewrites
        // matching history rows back to `Pending`, but if
        // `with_history_persist_path` is called AFTER this method
        // it would `*history = records` and overwrite our work with
        // the pre-revert on-disk state. Fail closed before loading either
        // pending authority or spawning detached persistence owners.
        if self.history_persist_path.is_none() {
            self.legacy_persistence_recovery_healthy
                .store(false, Ordering::SeqCst);
            warn!(
                path = %path.display(),
                "[USER-REQUEST] with_pending_persist_path called before with_history_persist_path; \
                 skipped recovery and fenced durable mutations. Call \
                 with_history_persist_path first."
            );
            return self;
        }
        if !self
            .legacy_persistence_recovery_healthy
            .load(Ordering::SeqCst)
        {
            warn!(
                path = %path.display(),
                "[USER-REQUEST] Skipping pending recovery because legacy history recovery is incomplete"
            );
            return self;
        }

        let mut restored = load_pending_requests(self.workspace_layout.as_ref(), &path);
        if !restored.legacy_healthy {
            self.legacy_persistence_recovery_healthy
                .store(false, Ordering::SeqCst);
            warn!(
                path = %path.display(),
                max_shard_bytes = MAX_USER_REQUEST_SCOPE_SHARD_BYTES,
                "[USER-REQUEST] Legacy pending recovery is incomplete; preserving its unattributable disk authority"
            );
            return self;
        }
        let generation_failures = normalize_restored_pending_generations(
            self.workspace_layout.as_ref(),
            &mut restored.entries,
        );
        restored.unhealthy_scopes.extend(generation_failures);
        // Pending and history are independent durable owners joined by the
        // globally unique request id. A cross-store same-id mismatch must not
        // let a resolved row from one scope delete or rewrite a different
        // scope's pending body during startup reconciliation. Exact requests
        // and the digest-bound app tombstone produced from that request are
        // the only compatible pairs.
        {
            let recovery_now_ms = chrono::Utc::now().timestamp_millis();
            let history = self
                .history
                .lock()
                .expect("user request history mutex poisoned");
            for row in &restored.entries {
                if self.workspace_layout.is_some()
                    && history.has_storage_alias(&row.request.principal, &row.request.workspace)
                {
                    restored.unhealthy_scopes.insert(request_storage_scope(
                        self.workspace_layout.as_ref(),
                        &row.request.principal,
                        &row.request.workspace,
                    ));
                    continue;
                }
                if !is_app_owner_notification_cleanup_marker(&row.request)
                    && ((app_owner_notification_marker_present(&row.request)
                        && validated_app_owner_notification_deadline_ms(&row.request).is_err())
                        || (is_app_owner_notification(&row.request)
                            && request_deadline_ms(&row.request) <= recovery_now_ms))
                {
                    // These rows are independently authorized for fail-closed
                    // body cleanup below. Already-redacted markers still take
                    // the ordinary duplicate-id path: they have no private
                    // body whose expiry could justify bypassing a conflicting
                    // history owner.
                    continue;
                }
                let Some(record) = history.record_by_id(&row.request.id) else {
                    continue;
                };
                if !pending_request_matches_history_owner(&row.request, &record.request) {
                    restored.unhealthy_scopes.insert(request_storage_scope(
                        self.workspace_layout.as_ref(),
                        &row.request.principal,
                        &row.request.workspace,
                    ));
                    restored.unhealthy_scopes.insert(request_storage_scope(
                        self.workspace_layout.as_ref(),
                        &record.request.principal,
                        &record.request.workspace,
                    ));
                }
            }
        }
        let existing_unhealthy = self
            .persistence_recovery_unhealthy_scopes
            .lock()
            .expect("user request recovery-scope mutex poisoned")
            .clone();
        let restored_unhealthy = restored.unhealthy_scopes.clone();
        {
            restored.entries.retain(|row| {
                !request_scope_is_quarantined(
                    self.workspace_layout.as_ref(),
                    &existing_unhealthy,
                    &row.request.principal,
                    &row.request.workspace,
                ) && !request_scope_is_quarantined(
                    self.workspace_layout.as_ref(),
                    &restored_unhealthy,
                    &row.request.principal,
                    &row.request.workspace,
                )
            });
        }
        if !restored_unhealthy.is_empty() || !existing_unhealthy.is_empty() {
            // Quarantine is a scope-wide read and mutation boundary. Do not
            // leave a history projection from a newly conflicting pending
            // owner visible for replay/listing merely because history loaded
            // first; its durable shard remains untouched for recovery.
            self.history
                .lock()
                .expect("user request history mutex poisoned")
                .retain(|record| {
                    !request_scope_is_quarantined(
                        self.workspace_layout.as_ref(),
                        &existing_unhealthy,
                        &record.request.principal,
                        &record.request.workspace,
                    ) && !request_scope_is_quarantined(
                        self.workspace_layout.as_ref(),
                        &restored_unhealthy,
                        &record.request.principal,
                        &record.request.workspace,
                    )
                });
        }
        let has_quarantined_scope =
            !restored.unhealthy_scopes.is_empty() || !existing_unhealthy.is_empty();
        if has_quarantined_scope {
            if let Some(layout) = self.workspace_layout.as_ref() {
                let legacy_paths = [Some(path.as_path()), self.history_persist_path.as_deref()];
                let legacy_owner_is_coupled = legacy_paths
                    .into_iter()
                    .flatten()
                    .any(|legacy| !matches!(layout.metadata_path_sync(legacy), Ok(None)));
                if legacy_owner_is_coupled {
                    self.persistence_recovery_unhealthy_scopes
                        .lock()
                        .expect("user request recovery-scope mutex poisoned")
                        .extend(restored.unhealthy_scopes);
                    self.legacy_persistence_recovery_healthy
                        .store(false, Ordering::SeqCst);
                    warn!(
                        path = %path.display(),
                        "[USER-REQUEST] Quarantined scope is coupled to a legacy flat owner; preserving it and fencing mutations"
                    );
                    return self;
                }
            }
        }
        let migration = try_migrate_pending_request_rows_sync(
            self.workspace_layout.as_ref(),
            &path,
            &restored.entries,
            restored.unhealthy_scopes.is_empty() && existing_unhealthy.is_empty(),
        );
        if !migration.legacy_healthy {
            self.legacy_persistence_recovery_healthy
                .store(false, Ordering::SeqCst);
            warn!(
                path = %path.display(),
                "[USER-REQUEST] Could not durably migrate the legacy pending owner"
            );
            return self;
        }
        restored.unhealthy_scopes.extend(migration.unhealthy_scopes);
        let all_unhealthy = restored.unhealthy_scopes.clone();
        restored.entries.retain(|row| {
            !request_scope_is_quarantined(
                self.workspace_layout.as_ref(),
                &all_unhealthy,
                &row.request.principal,
                &row.request.workspace,
            )
        });
        if !all_unhealthy.is_empty() {
            self.history
                .lock()
                .expect("user request history mutex poisoned")
                .retain(|record| {
                    !request_scope_is_quarantined(
                        self.workspace_layout.as_ref(),
                        &all_unhealthy,
                        &record.request.principal,
                        &record.request.workspace,
                    )
                });
        }
        if !restored.unhealthy_scopes.is_empty() {
            warn!(
                path = %path.display(),
                scope_count = restored.unhealthy_scopes.len(),
                "[USER-REQUEST] Pending recovery quarantined malformed or unpersistable owner scopes"
            );
        }
        self.persistence_recovery_unhealthy_scopes
            .lock()
            .expect("user request recovery-scope mutex poisoned")
            .extend(restored.unhealthy_scopes);
        let recovered_pending_scopes = restored
            .entries
            .iter()
            .map(|row| (row.request.principal.clone(), row.request.workspace.clone()))
            .collect::<HashSet<_>>();
        if restored.entries.is_empty() {
            // History loading normalizes stale pending records and compacts or
            // expires app-notification bodies. Persist that cleanup even when
            // there is no pending shard to drive the reconciliation loop.
            if let Some(layout) = self.workspace_layout.as_ref() {
                let pending = self.pending.read().await;
                let mut history = self
                    .history
                    .lock()
                    .expect("user request history mutex poisoned");
                let scopes = history
                    .iter()
                    .map(|record| {
                        (
                            record.request.principal.clone(),
                            record.request.workspace.clone(),
                        )
                    })
                    .collect::<HashSet<_>>();
                let already_unhealthy = self
                    .persistence_recovery_unhealthy_scopes
                    .lock()
                    .expect("user request recovery-scope mutex poisoned")
                    .clone();
                let failed = try_persist_recovered_request_scopes_sync(
                    layout,
                    self.history_persist_path.as_deref(),
                    self.pending_persist_path.as_deref(),
                    &history,
                    &pending,
                    scopes,
                    &already_unhealthy,
                );
                if !failed.is_empty() {
                    // A failed startup rewrite is a read boundary as well as
                    // a write fence. Do not expose the typed projection while
                    // the durable shard remains unverified.
                    history.retain(|record| {
                        !request_scope_is_quarantined(
                            Some(layout),
                            &failed,
                            &record.request.principal,
                            &record.request.workspace,
                        )
                    });
                }
                drop(history);
                drop(pending);
                let scoped_recovery_complete = failed.is_empty() && already_unhealthy.is_empty();
                if !failed.is_empty()
                    && self.history_persist_path.as_deref().is_some_and(|legacy| {
                        !matches!(layout.metadata_path_sync(legacy), Ok(None))
                    })
                {
                    self.legacy_persistence_recovery_healthy
                        .store(false, Ordering::SeqCst);
                }
                self.persistence_recovery_unhealthy_scopes
                    .lock()
                    .expect("user request recovery-scope mutex poisoned")
                    .extend(failed);
                if scoped_recovery_complete {
                    if let Some(legacy_history_path) = self.history_persist_path.as_deref() {
                        let legacy_removed = match layout.metadata_path_sync(legacy_history_path) {
                            Ok(Some(_)) => {
                                layout.remove_file_path_sync(legacy_history_path).is_ok()
                            },
                            Ok(None) => true,
                            Err(_) => false,
                        };
                        if !legacy_removed {
                            self.legacy_persistence_recovery_healthy
                                .store(false, Ordering::SeqCst);
                        }
                    }
                }
            } else if !self.has_unhealthy_recovery_scopes() {
                self.prune_and_persist_history();
            }
            // An empty restore can also mean that the only persisted rows were
            // malformed or schema-incompatible. Rewrite every known pending
            // shard from the authoritative empty map so an unreadable app-owner
            // notification body cannot remain physically retained forever.
            // Failure leaves the legacy source in place and is surfaced loudly;
            // a later startup will retry the same repair.
            if self.workspace_layout.is_none() && !self.has_unhealthy_recovery_scopes() {
                let pending = self.pending.read().await;
                if let Err(error) = write_pending_snapshot(
                    self.workspace_layout.as_ref(),
                    self.pending_persist_path.as_deref(),
                    &pending,
                )
                .await
                {
                    warn!(
                        path = %path.display(),
                        error = %error,
                        "[USER-REQUEST] Failed to rewrite empty pending shards"
                    );
                }
                drop(pending);
            }
            self.ensure_generic_request_publication_retry();
            self.ensure_generic_resolution_publication_retry();
            return self;
        }

        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut restored_alive = 0usize;
        let mut restored_expired = 0usize;
        let mut dropped_too_old = 0usize;
        let mut app_notification_cleanup_debts = Vec::new();

        // IM2 — batch history mutations under ONE lock + ONE persist.
        // The previous design persisted pending / resolved history per entry,
        // with one sync file write each. For N restored entries that's N
        // writes; here it collapses to one.
        let mut pending = self.pending.write().await;
        let mut history_guard = self
            .history
            .lock()
            .expect("user request history mutex poisoned");

        let mut restored_generations = HashMap::new();
        for restored in restored.entries {
            let mut request = restored.request;
            // A row written before the sensitivity contract carries no spec;
            // classify it now so an old password prompt answered after the
            // upgrade is not treated as ordinary text.
            if request.sensitive.is_none() {
                request.sensitive = classify::classify_sensitive(&request, request.created_at);
                if request.sensitive.as_ref().is_some_and(|spec| spec.one_time) {
                    request.timeout_secs = request
                        .timeout_secs
                        .clamp(1, classify::ONE_TIME_COLLECTION_MAX_SECS);
                }
            }
            if let Some(generation) = restored.app_owner_generation {
                restored_generations.insert(request.id.clone(), generation);
            }
            // Cleanup debt is deliberately persisted in this compact form if a
            // prior process could not delete a full pending shard. It is never
            // a request to republish an empty notification body.
            if is_app_owner_notification_cleanup_marker(&request) {
                app_notification_cleanup_debts
                    .push((request.clone(), app_notification_cleanup_response(&request)));
                continue;
            }
            // Restore can process many durable rows. Re-sample time per entry
            // so neither the app body TTL nor the re-armed timeout is extended
            // by time spent reconciling earlier scopes.
            let now_ms = chrono::Utc::now().timestamp_millis();
            if app_owner_notification_marker_present(&request)
                && validated_app_owner_notification_deadline_ms(&request).is_err()
            {
                // Marker-bearing rows are a reserved host contract. A torn or
                // forged shape must never fall back to generic, non-expiring
                // UserRequest retention. Convert it immediately to content-free
                // cleanup debt and let the ordered two-owner cleanup remove the
                // physical body.
                restored_expired += 1;
                let response = UserResponse {
                    request_id: request.id.clone(),
                    decision: request.default_on_timeout.clone(),
                    input: None,
                    channel: "timeout".to_string(),
                    sensitive: Vec::new(),
                };
                app_notification_cleanup_debts.push((
                    redact_app_owner_notification_request(&request, &response, now_ms),
                    response,
                ));
                continue;
            }
            let deadline_ms = request_deadline_ms(&request);
            if is_app_owner_notification(&request) && deadline_ms <= now_ms {
                // The reviewed TTL independently authorizes content cleanup.
                // Handle it before consulting any same-id history record so a
                // conflicting owner cannot substitute its lifecycle marker or
                // keep this expired private body on disk.
                restored_expired += 1;
                let response = UserResponse {
                    request_id: request.id.clone(),
                    decision: request.default_on_timeout.clone(),
                    input: None,
                    channel: "timeout".to_string(),
                    sensitive: Vec::new(),
                };
                app_notification_cleanup_debts.push((
                    redact_app_owner_notification_request(&request, &response, now_ms),
                    response,
                ));
                continue;
            }
            // A failed pending-snapshot deletion must not resurrect a request
            // whose independently durable history already records a genuine
            // response/timeout. Synthetic service-restart markers are the one
            // exception: they describe a formerly-pending row and are reverted
            // below when its pending snapshot is present.
            let resolved_history_record =
                history_guard.record_by_id(&request.id).filter(|record| {
                    record.request.principal == request.principal
                        && record.request.workspace == request.workspace
                        && record.status == UserRequestStatus::Resolved
                        && (if is_app_owner_notification_cleanup_marker(&record.request) {
                            app_notification_tombstone_is_resolution_authority(record)
                        } else {
                            record
                                .response
                                .as_ref()
                                .map_or(true, |response| response.channel != "service_restart")
                        })
                });
            if let Some(record) = resolved_history_record {
                // A prior app-notification resolution may have committed its
                // digest-only replay marker but crashed (or lost IO) before the
                // content-bearing pending shard was deleted. Reconcile that
                // second owner and republish resolution only after both shards
                // are content-free. Never retain the stale body as retry debt.
                if is_app_owner_notification_cleanup_marker(&record.request) {
                    app_notification_cleanup_debts.push((
                        record.request.clone(),
                        app_notification_cleanup_response(&record.request),
                    ));
                }
                continue;
            }

            // I3 — defensive TTL cap. Entries older than the global
            // limit are dropped without ceremony. App owner notifications
            // carry a host-sealed absolute expiry and may be reviewed for 31
            // days, so their exact live deadline supersedes this generic cap.
            if now_ms.saturating_sub(request.created_at) > MAX_RESTORE_AGE_MS
                && !(is_app_owner_notification(&request) && deadline_ms > now_ms)
            {
                dropped_too_old += 1;
                warn!(
                    request_id = %request.id,
                    request_type = %request.request_type,
                    created_at = request.created_at,
                    age_ms = now_ms - request.created_at,
                    "[USER-REQUEST] Dropping restored pending entry older than MAX_RESTORE_AGE_MS"
                );
                continue;
            }

            let remaining_ms = deadline_ms.saturating_sub(now_ms);

            if remaining_ms <= 0 {
                // Expired during downtime. Publish the synthetic resolution
                // only after its scoped history marker is durable; that marker
                // suppresses the still-present pre-crash pending shard if the
                // process dies before the cleaned snapshot below is written.
                restored_expired += 1;
                let default_response = UserResponse {
                    request_id: request.id.clone(),
                    decision: request.default_on_timeout.clone(),
                    input: None,
                    channel: "timeout".to_string(),
                    sensitive: Vec::new(),
                };
                let mut resolved_history =
                    history_guard.records_for_scope(&request.principal, &request.workspace);
                // A crash can leave the pending owner committed before the
                // history/requested outbox row exists. Reconstruct the pending
                // fact first so HitlResolved can never be published (or trimmed)
                // ahead of its exact HitlRequested prerequisite.
                upsert_pending_in_history(&mut resolved_history, &request);
                upsert_resolved_in_history(
                    &mut resolved_history,
                    &request,
                    &default_response,
                    now_ms,
                );
                trim_history_records(&mut resolved_history, self.history_limit);
                let resolution_marker_retained = resolved_history.iter().any(|record| {
                    record.request.id == request.id && record.status == UserRequestStatus::Resolved
                });
                let publication_debt_retained = resolved_history.iter().any(|record| {
                    record.request.id == request.id
                        && generic_resolution_publication_is_pending(record)
                });
                let request_publication_debt_retained = resolved_history.iter().any(|record| {
                    record.request.id == request.id
                        && generic_request_publication_is_pending(record)
                });
                let history_result = try_persist_user_request_scope_candidate(
                    self.workspace_layout.as_ref(),
                    self.history_persist_path.as_deref(),
                    &history_guard,
                    &resolved_history,
                    &request.principal,
                    &request.workspace,
                );
                if self.history_persist_path.is_some()
                    && history_result.is_ok()
                    && resolution_marker_retained
                    && publication_debt_retained
                    && request_publication_debt_retained
                {
                    history_guard.replace_scope(
                        &request.principal,
                        &request.workspace,
                        resolved_history,
                    );
                    enqueue_generic_resolution_publication(
                        &self.generic_resolution_publication_queue,
                        &request.id,
                    );
                    enqueue_generic_resolution_publication(
                        &self.generic_request_publication_queue,
                        &request.id,
                    );
                    continue;
                }
                if let Err(error) = history_result {
                    warn!(
                        request_id = %request.id,
                        error = %error,
                        "[USER-REQUEST] Could not durably resolve restored expiry; retrying in-process"
                    );
                }
            }

            // Still within the original deadline — restore as live
            // pending with a fresh timeout task for the remaining
            // window. The sender's receiver is dropped on the spot:
            // the original caller is gone, so no value will ever flow
            // back through the channel. `respond_scoped` calling
            // `send()` on this is expected to fail silently.
            if remaining_ms > 0 {
                restored_alive += 1;
            }
            let (dummy_tx, dummy_rx) = oneshot::channel::<UserResponse>();
            drop(dummy_rx);

            let pending_clone = Arc::clone(&self.pending);
            let history_clone = Arc::clone(&self.history);
            let broadcaster_clone = Arc::clone(&self.event_broadcaster);
            let history_persist_path_clone = self.history_persist_path.clone();
            let pending_persist_path_clone = Some(path.clone());
            let workspace_layout_clone = self.workspace_layout.clone();
            let store_writer_lease_clone = self.store_writer_lease.clone();
            let history_limit = self.history_limit;
            let cleanup_retry_started = Arc::clone(&self.app_notification_cleanup_retry_started);
            let cleanup_queue = Arc::clone(&self.app_notification_cleanup_queue);
            let generic_publication_retry_started =
                Arc::clone(&self.generic_resolution_publication_retry_started);
            let generic_publication_queue = Arc::clone(&self.generic_resolution_publication_queue);
            let generic_request_publication_retry_started =
                Arc::clone(&self.generic_request_publication_retry_started);
            let generic_request_publication_queue =
                Arc::clone(&self.generic_request_publication_queue);
            let request_id_for_timeout = request.id.clone();
            let default_decision = request.default_on_timeout.clone();
            let remaining_timeout_ms = remaining_ms.max(1) as u64;

            // Restore the live owner before replay publication so an observer
            // can never receive HitlRequested for an entry that
            // respond_scoped cannot find. The pre-crash pending shard is
            // already the durable acceptance authority at this point.
            history_guard.upsert_pending(&request);
            pending.insert(
                request.id.clone(),
                PendingRequest {
                    request: request.clone(),
                    app_owner_generation: restored_generations.get(&request.id).cloned(),
                    response_tx: dummy_tx,
                    sensitive_tx: None,
                    timeout_handle: None,
                },
            );
            if generic_resolution_requires_publication_debt(&request) {
                enqueue_generic_resolution_publication(
                    &self.generic_request_publication_queue,
                    &request.id,
                );
            } else if is_app_owner_notification(&request) {
                enqueue_app_notification_request_publication(
                    &self.app_notification_request_publication_queue,
                    &request.id,
                );
            } else {
                emit_hitl_requested(&self.event_broadcaster, &request);
            }

            // App lifecycle publication is owned by a separate fixed-page
            // worker after startup releases the global pending/history guards.
            // The absolute deadline below remains the original durable one.
            let handle = tokio::spawn(async move {
                tokio::time::sleep(tokio::time::Duration::from_millis(remaining_timeout_ms)).await;
                let mut retry_delay_secs = 1u64;
                loop {
                    let mut pending = pending_clone.write().await;
                    let Some(entry) = pending.remove(&request_id_for_timeout) else {
                        return;
                    };
                    let default_response = UserResponse {
                        request_id: request_id_for_timeout.clone(),
                        decision: default_decision.clone(),
                        input: None,
                        channel: "timeout".to_string(),
                        sensitive: Vec::new(),
                    };
                    let resolved_at = chrono::Utc::now().timestamp_millis();
                    match try_commit_resolved_request(
                        &history_clone,
                        workspace_layout_clone.as_ref(),
                        history_persist_path_clone.as_deref(),
                        pending_persist_path_clone.as_deref(),
                        &pending,
                        history_limit,
                        &entry.request,
                        entry.app_owner_generation.as_deref(),
                        &default_response,
                        resolved_at,
                    ) {
                        ResolvedRequestCommit::Committed => {},
                        ResolvedRequestCommit::AppResolutionReady { redacted_request } => {
                            // The durable redacted marker is the outbox. Hand
                            // it to the single cleanup owner before yielding;
                            // that owner performs journal reduction on the
                            // admitted blocking pool and wakes this waiter only
                            // after exact lifecycle acceptance.
                            let mut entry = entry;
                            entry.request = redacted_request;
                            pending.insert(request_id_for_timeout.clone(), entry);
                            enqueue_app_notification_cleanup(
                                &cleanup_queue,
                                &request_id_for_timeout,
                            );
                            spawn_app_notification_cleanup_retry(
                                Arc::clone(&pending_clone),
                                Arc::clone(&history_clone),
                                Arc::clone(&broadcaster_clone),
                                workspace_layout_clone.clone(),
                                history_persist_path_clone.clone(),
                                pending_persist_path_clone.clone(),
                                history_limit,
                                store_writer_lease_clone.clone(),
                                Arc::clone(&cleanup_retry_started),
                                Arc::clone(&cleanup_queue),
                            );
                            return;
                        },
                        ResolvedRequestCommit::RetryAppCleanup { redacted_request } => {
                            let mut entry = entry;
                            entry.request = redacted_request;
                            pending.insert(request_id_for_timeout.clone(), entry);
                            enqueue_app_notification_cleanup(
                                &cleanup_queue,
                                &request_id_for_timeout,
                            );
                            spawn_app_notification_cleanup_retry(
                                Arc::clone(&pending_clone),
                                Arc::clone(&history_clone),
                                Arc::clone(&broadcaster_clone),
                                workspace_layout_clone.clone(),
                                history_persist_path_clone.clone(),
                                pending_persist_path_clone.clone(),
                                history_limit,
                                store_writer_lease_clone.clone(),
                                Arc::clone(&cleanup_retry_started),
                                Arc::clone(&cleanup_queue),
                            );
                            return;
                        },
                        ResolvedRequestCommit::Rejected => {
                            pending.insert(request_id_for_timeout.clone(), entry);
                            drop(pending);
                            tokio::time::sleep(tokio::time::Duration::from_secs(retry_delay_secs))
                                .await;
                            retry_delay_secs = retry_delay_secs.saturating_mul(2).min(60);
                            continue;
                        },
                    }
                    if !entry.response_tx.is_closed() {
                        let _ = entry.response_tx.send(default_response);
                    }
                    drop(pending);
                    publish_generic_request_or_enqueue_retry(
                        &history_clone,
                        &broadcaster_clone,
                        workspace_layout_clone.as_ref(),
                        history_persist_path_clone.as_deref(),
                        history_limit,
                        store_writer_lease_clone.as_ref(),
                        &generic_request_publication_retry_started,
                        &generic_request_publication_queue,
                        &request_id_for_timeout,
                    );
                    publish_generic_resolution_or_enqueue_retry(
                        &history_clone,
                        &broadcaster_clone,
                        workspace_layout_clone.as_ref(),
                        history_persist_path_clone.as_deref(),
                        history_limit,
                        store_writer_lease_clone.as_ref(),
                        &generic_publication_retry_started,
                        &generic_publication_queue,
                        &request_id_for_timeout,
                    );
                    return;
                }
            });

            pending
                .get_mut(&request.id)
                .expect("restored request remains owned while pending write lock is held")
                .timeout_handle = Some(handle);
        }

        // Reconcile app-notification cleanup while both in-memory owners are
        // exclusively held. Pre-install every compatible live redacted history
        // marker before deleting any pending shard in the same scope: a single
        // scoped pending rewrite can remove several stale bodies, so each must
        // first have independent deterministic-replay authority. Absolute
        // expiry may authorize cleanup without a marker, but never replacement
        // of a foreign same-id history owner. Grouping also bounds startup IO
        // to one ordered history/pending pair per scope.
        install_app_notification_cleanup_history_markers(
            &mut history_guard,
            &app_notification_cleanup_debts,
            now_ms,
        );
        history_guard.trim_all_scopes(self.history_limit);
        let mut cleanup_debts_by_scope: HashMap<
            (String, String),
            Vec<(UserRequest, UserResponse)>,
        > = HashMap::new();
        for (request, response) in app_notification_cleanup_debts {
            cleanup_debts_by_scope
                .entry((request.principal.clone(), request.workspace.clone()))
                .or_default()
                .push((request, response));
        }
        let mut cleanup_retry_needed = false;
        for ((_principal, _workspace), cleanup_debts) in cleanup_debts_by_scope {
            let Some((representative_request, _)) = cleanup_debts.first() else {
                continue;
            };
            for (request, response) in &cleanup_debts {
                let redacted_request = redact_app_owner_notification_request(
                    request,
                    response,
                    app_notification_resolution_timestamp_ms(request, now_ms),
                );
                let request_id = redacted_request.id.clone();
                let (dummy_tx, dummy_rx) = oneshot::channel::<UserResponse>();
                drop(dummy_rx);
                pending.insert(
                    request_id.clone(),
                    PendingRequest {
                        app_owner_generation: restored_generations.get(&request_id).cloned(),
                        request: redacted_request,
                        response_tx: dummy_tx,
                        sensitive_tx: None,
                        timeout_handle: None,
                    },
                );
            }
            let scoped_history = history_guard.records_for_scope(
                &representative_request.principal,
                &representative_request.workspace,
            );
            if try_commit_app_notification_cleanup_scope_with_history(
                &scoped_history,
                self.workspace_layout.as_ref(),
                self.history_persist_path.as_deref(),
                self.pending_persist_path.as_deref(),
                &pending,
                representative_request,
                &cleanup_debts,
            ) {
                for (request, _) in &cleanup_debts {
                    enqueue_app_notification_cleanup(
                        &self.app_notification_cleanup_queue,
                        &request.id,
                    );
                }
                // Lifecycle reconciliation may require a complete shared
                // journal reduction. Keep the durable, content-free markers
                // installed and defer that work until after the startup-held
                // pending/history guards are released.
                cleanup_retry_needed = true;
            } else {
                // Retain only compact retry debt. Persisting this redacted
                // owner is safe even when history IO failed: it preserves the
                // deterministic identity and resolution metadata without the
                // reviewed notification body.
                if let Some((request, _)) = cleanup_debts.first() {
                    if let Err(error) = try_write_pending_scope_snapshot_sync(
                        self.workspace_layout.as_ref(),
                        self.pending_persist_path.as_deref(),
                        &pending,
                        &request.principal,
                        &request.workspace,
                    ) {
                        warn!(
                            principal = %request.principal,
                            workspace = %request.workspace,
                            error = %error,
                            "[USER-REQUEST] Could not persist grouped redacted app cleanup debt"
                        );
                    }
                }
                for (request, _) in &cleanup_debts {
                    enqueue_app_notification_cleanup(
                        &self.app_notification_cleanup_queue,
                        &request.id,
                    );
                }
                cleanup_retry_needed = true;
            }
        }
        if cleanup_retry_needed {
            self.ensure_app_notification_cleanup_retry();
        }

        // Single batched history persist for ordinary restored records. If it
        // fails, do not run the global pending rewrite: that rewrite could
        // otherwise clean an app pending owner before its history owner.
        let (history_persisted, needs_global_pending_write) = if let Some(layout) =
            self.workspace_layout.as_ref()
        {
            let mut scopes = recovered_pending_scopes;
            scopes.extend(history_guard.iter().map(|record| {
                (
                    record.request.principal.clone(),
                    record.request.workspace.clone(),
                )
            }));
            scopes.extend(pending.values().map(|entry| {
                (
                    entry.request.principal.clone(),
                    entry.request.workspace.clone(),
                )
            }));
            let already_unhealthy = self
                .persistence_recovery_unhealthy_scopes
                .lock()
                .expect("user request recovery-scope mutex poisoned")
                .clone();
            let failed = try_persist_recovered_request_scopes_sync(
                layout,
                self.history_persist_path.as_deref(),
                self.pending_persist_path.as_deref(),
                &history_guard,
                &pending,
                scopes,
                &already_unhealthy,
            );
            let mut complete = failed.is_empty();
            if !complete {
                warn!(
                    scope_count = failed.len(),
                    "[USER-REQUEST] Startup reconciliation quarantined scopes whose bounded rewrite failed"
                );
                if self
                    .history_persist_path
                    .as_deref()
                    .is_some_and(|legacy| !matches!(layout.metadata_path_sync(legacy), Ok(None)))
                {
                    self.legacy_persistence_recovery_healthy
                        .store(false, Ordering::SeqCst);
                }
                remove_failed_startup_scope_projections(
                    layout,
                    &failed,
                    &mut history_guard,
                    &mut pending,
                );
            }
            self.persistence_recovery_unhealthy_scopes
                .lock()
                .expect("user request recovery-scope mutex poisoned")
                .extend(failed);
            if complete && already_unhealthy.is_empty() {
                if let Some(legacy_history_path) = self.history_persist_path.as_deref() {
                    let legacy_removed = match layout.metadata_path_sync(legacy_history_path) {
                        Ok(Some(_)) => layout.remove_file_path_sync(legacy_history_path).is_ok(),
                        Ok(None) => true,
                        Err(_) => false,
                    };
                    if !legacy_removed {
                        // The scoped rows are durable, but leaving an older
                        // flat copy would create conflicting authority on the
                        // next replay. Fence this process until migration can
                        // be retried safely.
                        self.legacy_persistence_recovery_healthy
                            .store(false, Ordering::SeqCst);
                        complete = false;
                    }
                }
            }
            (complete, false)
        } else if self.has_unhealthy_recovery_scopes() {
            // A legacy full-store rewrite would serialize the intentionally
            // omitted quarantined scopes as empty and erase disk authority.
            (false, false)
        } else {
            let complete_history = history_guard.to_vec();
            match try_persist_user_request_history(
                self.workspace_layout.as_ref(),
                self.history_persist_path.as_deref(),
                &complete_history,
            ) {
                Ok(()) => (true, true),
                Err(error) => {
                    warn!(
                        error = %error,
                        "[USER-REQUEST] Failed to persist restored request history"
                    );
                    (false, false)
                },
            }
        };
        drop(history_guard);

        // Persist the cleaned-up snapshot (expired + too-old entries
        // removed) so subsequent restarts don't keep re-processing
        // them.
        if history_persisted && needs_global_pending_write {
            if let Err(error) = write_pending_snapshot(
                self.workspace_layout.as_ref(),
                self.pending_persist_path.as_deref(),
                &pending,
            )
            .await
            {
                warn!(
                    path = %path.display(),
                    error = %error,
                    "[USER-REQUEST] Failed to persist reconciled pending shards"
                );
            }
        }
        drop(pending);

        // History loading seeded any pre-crash generic publication debts, and
        // restored expiries above may have added new ones. Start exactly one
        // bounded worker only after the startup-held pending/history guards are
        // gone so reconciliation cannot deadlock with ordinary resolution.
        self.ensure_app_notification_request_publication_retry();
        self.ensure_generic_request_publication_retry();
        self.ensure_generic_resolution_publication_retry();

        debug!(
            restored_alive,
            restored_expired,
            dropped_too_old,
            path = %path.display(),
            "[USER-REQUEST] Restored pending entries from disk"
        );
        self
    }

    /// Submit a request without waiting for a human response.
    ///
    /// An empty `request.id` requests a fresh UUID. A non-empty id is treated
    /// as a deterministic idempotency key: an exact replay returns
    /// [`UserRequestSubmission::IdempotentReplay`], while reusing the id for
    /// different caller-controlled content fails closed.
    ///
    /// A newly accepted receipt is returned only after the pending entry and
    /// history record have crossed their configured persistence boundary. The
    /// canonical `HitlRequested` is attempted synchronously; scoped generic
    /// admission failure leaves durable retry debt without changing the
    /// established acceptance result. The response channel is deliberately
    /// detached only after that boundary.
    pub async fn submit_nonblocking(
        &self,
        request: UserRequest,
    ) -> Result<UserRequestSubmission, UserRequestSubmissionError> {
        let identity_mode = if request.id.is_empty() {
            UserRequestIdentityMode::FreshRandom
        } else {
            UserRequestIdentityMode::PreserveCallerId
        };
        let mut accepted = self.accept_request(request, identity_mode, false).await?;
        drop(accepted.response_rx.take());
        Ok(accepted.receipt)
    }

    /// Submit only after the pending request is durably snapshotted. This is
    /// the crash-safe boundary for durable upstream outboxes: an unavailable
    /// persistence owner is an error, never an in-memory acceptance.
    pub async fn submit_nonblocking_durable(
        &self,
        request: UserRequest,
    ) -> Result<UserRequestSubmission, UserRequestSubmissionError> {
        let identity_mode = if request.id.is_empty() {
            UserRequestIdentityMode::FreshRandom
        } else {
            UserRequestIdentityMode::PreserveCallerId
        };
        let mut accepted = self.accept_request(request, identity_mode, true).await?;
        drop(accepted.response_rx.take());
        Ok(accepted.receipt)
    }

    /// Submit a request and **block** until the user responds or the timeout
    /// fires.
    ///
    /// This is the primary API surface.  Any subsystem (executor, chat,
    /// scheduler) calls this to pause and wait for human input.
    ///
    /// * `request.id` and `request.created_at` are overwritten with fresh
    ///   values.
    /// * On timeout the response has `channel = "timeout"` and `decision`
    ///   equal to `request.default_on_timeout`.
    pub async fn ask(&self, request: UserRequest) -> UserResponse {
        self.ask_with_acceptance_hook(request, |_| {}).await
    }

    /// Submit a request, report its service-owned identity after acceptance,
    /// and then block until the user responds or the timeout fires.
    ///
    /// The hook runs only after the request has crossed the same persistence
    /// and publication boundary as [`Self::ask`], but before this future waits
    /// for resolution. Callers use it to order subsystem-specific lifecycle
    /// events around the shared HITL request without predicting the UUID that
    /// the service owns.
    pub async fn ask_with_acceptance_hook<F>(
        &self,
        request: UserRequest,
        on_accepted: F,
    ) -> UserResponse
    where
        F: FnOnce(&UserRequest) + Send,
    {
        let default_on_setup_error = request.default_on_timeout.clone();
        let accepted = self
            .accept_request(request, UserRequestIdentityMode::FreshRandom, false)
            .await;
        let mut accepted = match accepted {
            Ok(accepted) => accepted,
            Err(error) => {
                // Fresh UUID admission retries collisions internally, so this
                // branch is defensive and preserves `ask()`'s response-only
                // public signature.
                warn!(error = %error, "[USER-REQUEST] failed to publish request");
                return UserResponse {
                    request_id: String::new(),
                    decision: default_on_setup_error,
                    input: None,
                    channel: "error".to_string(),
                    sensitive: Vec::new(),
                };
            },
        };
        let request = accepted.request;
        on_accepted(&request);
        let rx = accepted
            .response_rx
            .take()
            .expect("fresh user request acceptance always owns a response receiver");

        // The authenticated plane call carries an explicit scoped input route.
        // Keep this service as the acceptance/resolution owner, including its
        // durable publication and first-response-wins gate. Other runtime tasks
        // have no task-local route and retain their existing UI delivery.
        if let Ok(route) =
            crate::magician_v2::execution::plane::input::INPUT_ROUTE.try_with(Clone::clone)
        {
            if request.principal == route.principal && request.workspace == route.workspace {
                return self.await_plane_response(request, rx, route).await;
            }
        }

        rx.await.unwrap_or_else(|_| {
            warn!(
                "[USER-REQUEST] oneshot channel dropped for request {}; returning default",
                request.id
            );
            UserResponse {
                request_id: request.id,
                decision: request.default_on_timeout,
                input: None,
                channel: "error".to_string(),
                sensitive: Vec::new(),
            }
        })
    }

    // Erase the optional transport bridge's future at this boundary. The
    // generic ask path is embedded in many runtime futures; none should carry
    // the form/response race's concrete state through their own async types.
    fn await_plane_response(
        &self,
        request: UserRequest,
        mut rx: oneshot::Receiver<UserResponse>,
        route: crate::magician_v2::execution::plane::input::InputRoute,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = UserResponse> + Send + '_>> {
        Box::pin(async move {
            use crate::magician_v2::execution::agentic::types::UserInputValue;
            use crate::magician_v2::execution::plane::input::{
                service_input_type, service_response,
            };

            let specification = service_input_type(&request);
            let question = request.question.clone();
            let channel = route.channel.clone();
            let cancelled = || UserResponse {
                request_id: request.id.clone(),
                decision: "cancel".into(),
                input: None,
                channel: "error".into(),
                sensitive: Vec::new(),
            };
            // A request the service classified SENSITIVE cannot be collected
            // through an MCP form at all (`CREDENTIAL_REFUSAL`), so the plane
            // does not race for it: the refusal resolves INSTANTLY, which won
            // the race every time and answered the ask with a cancel — the card
            // vanished from Attention milliseconds after it appeared and the
            // asker was told "cancel" with no reason. The ask stays open for the
            // authenticated UI, exactly as it would for a caller with no plane
            // route at all.
            //
            // Only that case. `service_input_type` refuses for several other
            // reasons — a conditional option, an unreadable specification, an
            // authorization that needs the review UI — and those must keep
            // aborting immediately as they always did, or the caller waits out
            // the request's whole window for an answer the plane could never
            // give.
            let collects_a_credential = request.sensitive.is_some()
                || matches!(
                    request.request_type.as_str(),
                    SECURE_INPUT_REQUEST | SECURE_CONFIRM_REQUEST
                );
            if let Err(reason) = &specification {
                if collects_a_credential {
                    warn!(
                        request_id = %request.id,
                        %reason,
                        "[USER-REQUEST] the plane cannot collect a credential ask; it stays open for the authenticated UI"
                    );
                    return rx.await.unwrap_or_else(|_| cancelled());
                }
            }
            let answer = async move {
                match specification {
                    Ok(input_type) => channel.ask(&question, input_type).await,
                    Err(reason) => Err(reason),
                }
            };
            tokio::pin!(answer);
            tokio::select! {
                response = &mut rx => return response.unwrap_or_else(|_| cancelled()),
                value = &mut answer => {
                    let value = value.unwrap_or_else(|reason| UserInputValue::aborted(Some(reason)));
                    let response = service_response(&request, value);
                    if matches!(
                        self.respond_scoped(response, Some(&route.principal), Some(&route.workspace)).await,
                        ScopedResponseResult::PersistenceUnavailable | ScopedResponseResult::ScopeMismatch
                    ) {
                        return cancelled();
                    }
                }
            }
            rx.await.unwrap_or_else(|_| cancelled())
        })
    }

    async fn accept_request(
        &self,
        request: UserRequest,
        identity_mode: UserRequestIdentityMode,
        require_durable: bool,
    ) -> Result<AcceptedUserRequest, UserRequestSubmissionError> {
        self.accept_request_with_sensitive_receiver(request, identity_mode, require_durable, None)
            .await
    }

    async fn accept_request_with_sensitive_receiver(
        &self,
        mut request: UserRequest,
        identity_mode: UserRequestIdentityMode,
        require_durable: bool,
        sensitive_tx: Option<oneshot::Sender<Zeroizing<String>>>,
    ) -> Result<AcceptedUserRequest, UserRequestSubmissionError> {
        const MAX_CALLER_REQUEST_ID_BYTES: usize = 256;

        // Untaken material past its hold is gone at the next thing the service
        // does, not at the take that would have found it.
        self.sweep_expired_custody();

        if identity_mode == UserRequestIdentityMode::PreserveCallerId
            && (request.id.trim().is_empty()
                || request.id.trim() != request.id
                || request.id.len() > MAX_CALLER_REQUEST_ID_BYTES)
        {
            return Err(UserRequestSubmissionError::InvalidRequestId);
        }
        if require_durable && self.pending_persist_path.is_none() {
            return Err(UserRequestSubmissionError::PersistenceUnavailable);
        }
        let (tx, rx) = oneshot::channel();
        let mut pending = self.pending.write().await;
        let mut history = self
            .history
            .lock()
            .expect("user request history mutex poisoned");

        match identity_mode {
            UserRequestIdentityMode::FreshRandom => loop {
                let candidate = uuid::Uuid::new_v4().to_string();
                if !pending.contains_key(&candidate) && history.record_by_id(&candidate).is_none() {
                    request.id = candidate;
                    break;
                }
            },
            UserRequestIdentityMode::PreserveCallerId => {
                if let Some(existing) = pending.get(&request.id).map(|entry| &entry.request) {
                    return replay_or_conflict(existing, &request);
                }
                if let Some(existing) = history
                    .record_by_id(&request.id)
                    .map(|record| &record.request)
                {
                    return replay_or_conflict(existing, &request);
                }
            },
        }
        if (self.pending_persist_path.is_some()
            || self.history_persist_path.is_some()
            || self.store_writer_lease_target.is_some())
            && !self.scope_persistence_recovery_healthy(&request.principal, &request.workspace)
        {
            return Err(UserRequestSubmissionError::PersistenceUnavailable);
        }
        if self.workspace_layout.is_some()
            && (pending.has_storage_alias(&request.principal, &request.workspace)
                || history.has_storage_alias(&request.principal, &request.workspace))
        {
            // Artifact V2 normalizes raw scope segments. A second logical
            // alias would rewrite the first owner's physical shard, so reject
            // it before mutating either in-memory or durable authority.
            return Err(UserRequestSubmissionError::PersistenceUnavailable);
        }
        request.created_at = chrono::Utc::now().timestamp_millis();
        // Classified once, here; every later strip/redact/vault decision reads
        // the stored spec. A producer-supplied spec is kept as is.
        if request.sensitive.is_none() {
            request.sensitive = classify::classify_sensitive(&request, request.created_at);
        }
        // One-time material waits no longer than its collection window, so
        // the request and the spec expire together (as the browser one-time
        // prompt already arranges for itself).
        if request.sensitive.as_ref().is_some_and(|spec| spec.one_time) {
            request.timeout_secs = request
                .timeout_secs
                .clamp(1, classify::ONE_TIME_COLLECTION_MAX_SECS);
        }

        if app_owner_notification_marker_present(&request)
            && validated_app_owner_notification_deadline_ms(&request).is_err()
        {
            return Err(UserRequestSubmissionError::PersistenceUnavailable);
        }
        let deadline_ms = request_deadline_ms(&request);
        if is_app_owner_notification(&request) && deadline_ms <= request.created_at {
            return Err(UserRequestSubmissionError::PersistenceUnavailable);
        }
        if !user_request_is_admitted(&request) {
            return Err(UserRequestSubmissionError::ScopeCapacityExceeded);
        }
        let app_owner_generation =
            is_app_owner_notification(&request).then(new_app_owner_notification_generation);
        if !pending_scope_accepts(&pending, &request, app_owner_generation.as_deref()) {
            return Err(UserRequestSubmissionError::ScopeCapacityExceeded);
        }
        let mut accepted_scope = history.records_for_scope(&request.principal, &request.workspace);
        upsert_pending_in_history(&mut accepted_scope, &request);
        trim_history_scope_records(&mut accepted_scope, self.history_limit);
        if !history_scope_is_admitted(&accepted_scope) {
            return Err(UserRequestSubmissionError::ScopeCapacityExceeded);
        }
        let pending_clone = Arc::clone(&self.pending);
        let history_clone = Arc::clone(&self.history);
        let history_limit = self.history_limit;
        let history_persist_path = self.history_persist_path.clone();
        let pending_persist_path = self.pending_persist_path.clone();
        let workspace_layout = self.workspace_layout.clone();
        let store_writer_lease = self.store_writer_lease.clone();
        let request_id_for_timeout = request.id.clone();
        let default_decision = request.default_on_timeout.clone();
        let broadcaster_for_timeout = Arc::clone(&self.event_broadcaster);
        let cleanup_retry_started = Arc::clone(&self.app_notification_cleanup_retry_started);
        let cleanup_queue = Arc::clone(&self.app_notification_cleanup_queue);
        let generic_publication_retry_started =
            Arc::clone(&self.generic_resolution_publication_retry_started);
        let generic_publication_queue = Arc::clone(&self.generic_resolution_publication_queue);
        let generic_request_publication_retry_started =
            Arc::clone(&self.generic_request_publication_retry_started);
        let generic_request_publication_queue = Arc::clone(&self.generic_request_publication_queue);

        pending.insert(
            request.id.clone(),
            PendingRequest {
                request: request.clone(),
                app_owner_generation,
                response_tx: tx,
                sensitive_tx,
                timeout_handle: None,
            },
        );

        if self.pending_persist_path.is_some()
            && try_write_pending_scope_snapshot_sync(
                self.workspace_layout.as_ref(),
                self.pending_persist_path.as_deref(),
                &pending,
                &request.principal,
                &request.workspace,
            )
            .is_err()
        {
            pending.remove(&request.id);
            return Err(UserRequestSubmissionError::PersistenceUnavailable);
        }
        let history_result = try_persist_user_request_scope_candidate(
            self.workspace_layout.as_ref(),
            self.history_persist_path.as_deref(),
            &history,
            &accepted_scope,
            &request.principal,
            &request.workspace,
        );
        if require_durable && (self.history_persist_path.is_none() || history_result.is_err()) {
            let removed = pending
                .remove(&request.id)
                .expect("newly inserted durable request remains present during rollback");
            if try_write_pending_scope_snapshot_sync(
                self.workspace_layout.as_ref(),
                self.pending_persist_path.as_deref(),
                &pending,
                &request.principal,
                &request.workspace,
            )
            .is_ok()
            {
                return Err(UserRequestSubmissionError::PersistenceUnavailable);
            }

            // The pending commit could not be rolled back, so acceptance has
            // crossed its durable boundary. Keep the process state aligned
            // with disk and publish the request rather than returning a false
            // failure which would leave unowned, silent debt.
            pending.insert(request.id.clone(), removed);
            history.replace_scope(&request.principal, &request.workspace, accepted_scope);
            warn!(
                request_id = %request.id,
                "[USER-REQUEST] history persistence and pending rollback both failed; preserving durable acceptance"
            );
        } else {
            if !require_durable {
                if let Err(error) = history_result {
                    if let Some(path) = self.history_persist_path.as_deref() {
                        warn!(
                            path = %path.display(),
                            request_id = %request.id,
                            error = %error,
                            "[USER-REQUEST] Failed to persist accepted request history"
                        );
                    }
                }
            }
            history.replace_scope(&request.principal, &request.workspace, accepted_scope);
        }
        drop(history);

        let publish_unscoped_inline = !generic_resolution_requires_publication_debt(&request)
            && !is_app_owner_notification(&request);
        if publish_unscoped_inline {
            // Unscoped events do no shared-journal reduction. Publish before
            // arming the timeout so its resolution cannot overtake the request.
            emit_hitl_requested(&self.event_broadcaster, &request);
        }

        // Persistence and history work above may consume part of the sealed
        // window. Recompute from the original absolute deadline rather than
        // extending the request by sleeping the acceptance-time duration.
        let remaining_timeout_ms = deadline_ms
            .saturating_sub(chrono::Utc::now().timestamp_millis())
            .max(1) as u64;
        let timeout_handle = tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(remaining_timeout_ms)).await;
            let mut retry_delay_secs = 1u64;
            loop {
                let mut pending = pending_clone.write().await;
                let Some(entry) = pending.remove(&request_id_for_timeout) else {
                    return;
                };
                let default_response = UserResponse {
                    request_id: request_id_for_timeout.clone(),
                    decision: default_decision.clone(),
                    input: None,
                    channel: "timeout".to_string(),
                    sensitive: Vec::new(),
                };
                let resolved_at = chrono::Utc::now().timestamp_millis();
                match try_commit_resolved_request(
                    &history_clone,
                    workspace_layout.as_ref(),
                    history_persist_path.as_deref(),
                    pending_persist_path.as_deref(),
                    &pending,
                    history_limit,
                    &entry.request,
                    entry.app_owner_generation.as_deref(),
                    &default_response,
                    resolved_at,
                ) {
                    ResolvedRequestCommit::Committed => {},
                    ResolvedRequestCommit::AppResolutionReady { redacted_request } => {
                        let mut entry = entry;
                        entry.request = redacted_request;
                        pending.insert(request_id_for_timeout.clone(), entry);
                        enqueue_app_notification_cleanup(&cleanup_queue, &request_id_for_timeout);
                        spawn_app_notification_cleanup_retry(
                            Arc::clone(&pending_clone),
                            Arc::clone(&history_clone),
                            Arc::clone(&broadcaster_for_timeout),
                            workspace_layout.clone(),
                            history_persist_path.clone(),
                            pending_persist_path.clone(),
                            history_limit,
                            store_writer_lease.clone(),
                            Arc::clone(&cleanup_retry_started),
                            Arc::clone(&cleanup_queue),
                        );
                        return;
                    },
                    ResolvedRequestCommit::RetryAppCleanup { redacted_request } => {
                        let mut entry = entry;
                        entry.request = redacted_request;
                        pending.insert(request_id_for_timeout.clone(), entry);
                        enqueue_app_notification_cleanup(&cleanup_queue, &request_id_for_timeout);
                        spawn_app_notification_cleanup_retry(
                            Arc::clone(&pending_clone),
                            Arc::clone(&history_clone),
                            Arc::clone(&broadcaster_for_timeout),
                            workspace_layout.clone(),
                            history_persist_path.clone(),
                            pending_persist_path.clone(),
                            history_limit,
                            store_writer_lease.clone(),
                            Arc::clone(&cleanup_retry_started),
                            Arc::clone(&cleanup_queue),
                        );
                        return;
                    },
                    ResolvedRequestCommit::Rejected => {
                        pending.insert(request_id_for_timeout.clone(), entry);
                        drop(pending);
                        tokio::time::sleep(tokio::time::Duration::from_secs(retry_delay_secs))
                            .await;
                        retry_delay_secs = retry_delay_secs.saturating_mul(2).min(60);
                        continue;
                    },
                }
                if !entry.response_tx.is_closed() {
                    let _ = entry.response_tx.send(default_response);
                }
                drop(pending);
                publish_generic_request_or_enqueue_retry(
                    &history_clone,
                    &broadcaster_for_timeout,
                    workspace_layout.as_ref(),
                    history_persist_path.as_deref(),
                    history_limit,
                    store_writer_lease.as_ref(),
                    &generic_request_publication_retry_started,
                    &generic_request_publication_queue,
                    &request_id_for_timeout,
                );
                publish_generic_resolution_or_enqueue_retry(
                    &history_clone,
                    &broadcaster_for_timeout,
                    workspace_layout.as_ref(),
                    history_persist_path.as_deref(),
                    history_limit,
                    store_writer_lease.as_ref(),
                    &generic_publication_retry_started,
                    &generic_publication_queue,
                    &request_id_for_timeout,
                );
                return;
            }
        });

        pending
            .get_mut(&request.id)
            .expect("newly inserted user request remains locked during acceptance")
            .timeout_handle = Some(timeout_handle);
        drop(pending);

        // Publish only after the durable pending snapshot and original
        // absolute timeout are installed. App notifications use a distinct
        // fixed-page blocking owner so no journal reduction runs under either
        // global UserRequest guard; a racing response simply makes that debt
        // complete during the worker's exact revalidation.
        if generic_resolution_requires_publication_debt(&request) {
            self.publish_generic_request_or_retry(&request.id);
        } else if is_app_owner_notification(&request) {
            enqueue_app_notification_request_publication(
                &self.app_notification_request_publication_queue,
                &request.id,
            );
            self.ensure_app_notification_request_publication_retry();
        }

        debug!(
            "[USER-REQUEST] Accepted request id={} type={} principal={}",
            request.id, request.request_type, request.principal
        );
        Ok(AcceptedUserRequest {
            receipt: UserRequestSubmission::Accepted {
                request_id: request.id.clone(),
            },
            request,
            response_rx: Some(rx),
        })
    }

    /// Respond to a pending request.  **First response wins.**
    ///
    /// Returns `true` if this response crossed the durable resolution boundary.
    /// Returns `false` if the request was already resolved or persistence was
    /// unavailable and the request remains pending for retry.
    /// One-shot take of a sensitive answer by its reference. In-process only:
    /// never expose this as an agent tool or through the API. The caller must
    /// prove the request's scope; a mismatch returns `None` without consuming.
    ///
    /// The chat consumer takes here (P3, `dispatch_chat_need_user_input`);
    /// the agentic resume path vaults its answers directly.
    pub(crate) fn take_sensitive(
        &self,
        reference: &str,
        request_id: &str,
        principal: &str,
        workspace: &str,
    ) -> Option<Zeroizing<String>> {
        self.sensitive_custody.take(
            reference,
            request_id,
            principal,
            workspace,
            chrono::Utc::now().timestamp_millis(),
        )
    }

    /// Forget every custody deposit of a request whose owning execution is
    /// gone (cancelled, replaced, or lost) before its consumer took them, or
    /// whose consumer has taken what it needed.
    pub(crate) fn retire_sensitive_for_request(&self, request_id: &str) {
        let retired = self.sensitive_custody.retire_request(request_id);
        // The row must not keep claiming `provided` for material this call
        // discarded: `unavailable` is the status that exists for exactly it.
        self.record_sensitive_answer_status(retired, SensitiveAnswerStatus::Unavailable);
    }

    /// Drop untaken material past its hold and let the request's history row
    /// say so: each swept deposit's `sensitive[]` entry goes from `provided`
    /// to `expired`, persisted with its scope. Value-free throughout.
    fn sweep_expired_custody(&self) {
        self.sweep_expired_custody_at(chrono::Utc::now().timestamp_millis());
    }

    fn sweep_expired_custody_at(&self, now_ms: i64) {
        let swept = self.sensitive_custody.sweep_expired(now_ms);
        self.record_sensitive_answer_status(swept, SensitiveAnswerStatus::Expired);
    }

    /// Move each named deposit's `sensitive[]` entry from `provided` to
    /// `status`, persisted with its scope. Value-free throughout; shared by the
    /// expiry sweep and the retirement of a request whose consumer is gone.
    fn record_sensitive_answer_status(
        &self,
        swept: Vec<custody::SweptDeposit>,
        status: SensitiveAnswerStatus,
    ) {
        if swept.is_empty() {
            return;
        }
        let mut by_scope: HashMap<(String, String), Vec<(String, String)>> = HashMap::new();
        for deposit in swept {
            by_scope
                .entry((deposit.principal, deposit.workspace))
                .or_default()
                .push((deposit.request_id, deposit.reference));
        }
        let mut history = self
            .history
            .lock()
            .expect("user request history mutex poisoned");
        for ((principal, workspace), swept) in by_scope {
            let mut scoped = history.records_for_scope(&principal, &workspace);
            let mut changed = false;
            for (request_id, reference) in &swept {
                let Some(record) = scoped
                    .iter_mut()
                    .find(|record| record.request.id == *request_id)
                else {
                    continue;
                };
                let Some(response) = record.response.as_mut() else {
                    continue;
                };
                for answer in response.sensitive.iter_mut() {
                    if answer.reference == *reference
                        && answer.status == SensitiveAnswerStatus::Provided
                    {
                        answer.status = status;
                        changed = true;
                    }
                }
            }
            if !changed {
                continue;
            }
            if let Err(error) = try_persist_user_request_scope_candidate(
                self.workspace_layout.as_ref(),
                self.history_persist_path.as_deref(),
                &history,
                &scoped,
                &principal,
                &workspace,
            ) {
                warn!(
                    principal,
                    workspace,
                    error = %error,
                    "[USER-REQUEST] Could not persist the expiry of untaken sensitive material"
                );
                continue;
            }
            history.replace_scope(&principal, &workspace, scoped);
        }
    }

    pub async fn respond(&self, response: UserResponse) -> bool {
        matches!(
            self.respond_scoped(response, None, None).await,
            ScopedResponseResult::Accepted
        )
    }

    /// Respond to a pending request while enforcing optional principal/workspace scope.
    ///
    /// For post-restart recovery, a supplied scope must exactly match a durable
    /// history record. Unknown IDs, incomplete scopes, and ownership mismatches
    /// return [`ScopedResponseResult::ScopeMismatch`] without writing history or
    /// emitting a resolution. If neither scoped durable authority can commit,
    /// [`ScopedResponseResult::PersistenceUnavailable`] leaves the request live
    /// for retry. Calls with no scope retain the internal orphan fallback used
    /// by [`Self::respond`].
    pub async fn respond_scoped(
        &self,
        mut response: UserResponse,
        principal: Option<&str>,
        workspace: Option<&str>,
    ) -> ScopedResponseResult {
        if !user_response_is_admitted(&response) {
            return ScopedResponseResult::PersistenceUnavailable;
        }
        self.sweep_expired_custody();
        if (self.pending_persist_path.is_some()
            || self.history_persist_path.is_some()
            || self.store_writer_lease_target.is_some())
            && !self
                .legacy_persistence_recovery_healthy
                .load(Ordering::SeqCst)
        {
            return ScopedResponseResult::PersistenceUnavailable;
        }
        let mut pending = self.pending.write().await;

        // Recovery path: the in-memory `pending` map only holds requests
        // registered by THIS process. After a magician restart, agents
        // that were waiting on `oneshot::Receiver` are dead and the map
        // is empty — but the request data is durable in events.jsonl
        // (canonical `hitl.requested { source: "user_request" }` rows)
        // and the V3 attention surface still lists those rows from the
        // event log. Returning `AlreadyResolved` here would leave those
        // rows stuck forever, repeatedly failing with the same error
        // every time the operator clicks Respond.
        //
        // Instead: when the entry is missing, treat the response as a
        // post-restart resolution. We can't wake the original caller
        // (its `oneshot::Receiver` is gone with the old process), but we
        // CAN record the response in history and emit the canonical
        // `HitlResolved` event — which the attention summary builder
        // pairs with the original `HitlRequested` row and stops
        // listing. From the operator's perspective the row disappears,
        // which is the right outcome.
        if !pending.contains_key(&response.request_id) {
            drop(pending);
            return self
                .emit_orphan_resolution(response, principal, workspace)
                .await;
        }

        // Re-borrow now that we know the entry exists for the scope check.
        let entry = pending.get(&response.request_id).expect("just checked");
        match (principal, workspace) {
            (None, None) => {},
            (Some(expected_principal), Some(expected_workspace))
                if entry.request.principal == expected_principal
                    && entry.request.workspace == expected_workspace => {},
            (Some(expected_principal), Some(expected_workspace)) => {
                debug!(
                    request_id = %response.request_id,
                    expected_principal,
                    expected_workspace,
                    actual_principal = %entry.request.principal,
                    actual_workspace = %entry.request.workspace,
                    "[USER-REQUEST] Rejected scoped response due to scope mismatch"
                );
                return ScopedResponseResult::ScopeMismatch;
            },
            _ => {
                debug!(
                    request_id = %response.request_id,
                    "[USER-REQUEST] Rejected response with incomplete scope"
                );
                return ScopedResponseResult::ScopeMismatch;
            },
        }
        if !self
            .scope_persistence_recovery_healthy(&entry.request.principal, &entry.request.workspace)
        {
            return ScopedResponseResult::PersistenceUnavailable;
        }

        // Strip material BEFORE persistence, lifecycle publication or ordinary
        // oneshot delivery. A restored/abandoned secure request cannot receive it.
        let mut custody_deposits: Vec<PendingDeposit> = Vec::new();
        let sensitive_value = if entry.request.request_type == SECURE_INPUT_REQUEST {
            let value = response.input.take().map(Zeroizing::new);
            let may_accept = principal.is_some()
                && workspace.is_some()
                && request_deadline_ms(&entry.request) > chrono::Utc::now().timestamp_millis()
                && !entry.response_tx.is_closed()
                && entry
                    .sensitive_tx
                    .as_ref()
                    .is_some_and(|tx| !tx.is_closed())
                && response.decision == "provide_input"
                && value
                    .as_deref()
                    .is_some_and(|v| sensitive_value_is_acceptable(v));
            response.decision = if may_accept { "provided" } else { "cancel" }.into();
            response.channel = recorded_sensitive_channel(&response.channel);
            if may_accept {
                value
            } else {
                None
            }
        } else if entry.request.request_type == SECURE_CONFIRM_REQUEST {
            // The one-time fill confirmation is a decision, never material; it
            // is checked before any spec so wording can never turn it into a cancel.
            drop(response.input.take().map(Zeroizing::new));
            let approved = principal.is_some()
                && workspace.is_some()
                && request_deadline_ms(&entry.request) > chrono::Utc::now().timestamp_millis()
                && !entry.response_tx.is_closed()
                && response.decision == "allow_once";
            response.decision = if approved { "allow_once" } else { "cancel" }.into();
            response.channel = recorded_sensitive_channel(&response.channel);
            None
        } else if let Some(spec) = entry.request.sensitive.clone() {
            // Every other sensitive answer: the value goes to service custody
            // (after the durable commit below) and the public response carries
            // only references and statuses. Accepting requires a proved scope,
            // a live asker, and both the request and collection deadlines.
            let raw = response.input.take().map(Zeroizing::new);
            if response.decision != "provide_input" {
                // A decision answer (cancel, an option id) carries no material:
                // keep the decision as given, forget anything that rode along.
                drop(raw);
                None
            } else {
                let mut ordinary_text: Option<String> = None;
                let now_ms = chrono::Utc::now().timestamp_millis();
                let collection_open = spec.collection_deadline_ms > now_ms;
                let may_accept = principal.is_some()
                    && workspace.is_some()
                    && request_deadline_ms(&entry.request) > now_ms
                    && collection_open
                    && !entry.response_tx.is_closed();
                let refused_status = if collection_open {
                    SensitiveAnswerStatus::Cancelled
                } else {
                    SensitiveAnswerStatus::Expired
                };
                let mut answers = Vec::new();
                let mut offer = |kind: SensitiveKind,
                                 field: Option<String>,
                                 value: Option<Zeroizing<String>>| {
                    let reference = SensitiveCustody::new_reference();
                    let status = match value {
                        Some(value) if may_accept && sensitive_value_is_acceptable(&value) => {
                            custody_deposits.push(PendingDeposit {
                                reference: reference.clone(),
                                kind,
                                value,
                            });
                            SensitiveAnswerStatus::Provided
                        },
                        _ => refused_status,
                    };
                    answers.push(SensitiveAnswer {
                        reference,
                        kind,
                        status,
                        field,
                    });
                };
                if spec.fields.is_empty() {
                    offer(spec.kind.unwrap_or(SensitiveKind::Other), None, raw);
                } else {
                    // A form ships every answered field as one JSON object; the
                    // flagged fields are deposited, the rest stay usable as text.
                    let mut values: HashMap<String, Zeroizing<String>> = raw
                        .as_deref()
                        .and_then(|raw| {
                            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(raw)
                                .ok()
                        })
                        .map(|map| {
                            map.into_iter()
                                .filter_map(|(id, value)| match value {
                                    serde_json::Value::String(value) => {
                                        Some((id, Zeroizing::new(value)))
                                    },
                                    _ => None,
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    for field in &spec.fields {
                        offer(field.kind, Some(field.id.clone()), values.remove(&field.id));
                    }
                    drop(raw);
                    let mut ordinary: Vec<(String, Zeroizing<String>)> =
                        values.into_iter().collect();
                    ordinary.sort_by(|a, b| a.0.cmp(&b.0));
                    let rendered = ordinary
                        .iter()
                        .map(|(id, value)| format!("{id}: {}", value.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n");
                    ordinary_text = (!rendered.is_empty()).then_some(rendered);
                }
                let provided = answers
                    .iter()
                    .any(|a| a.status == SensitiveAnswerStatus::Provided);
                response.decision = if provided { "provided" } else { "cancel" }.into();
                response.channel = recorded_sensitive_channel(&response.channel);
                response.sensitive = answers;
                response.input = ordinary_text;
                None
            }
        } else {
            None
        };

        // We hold the write lock and confirmed the entry exists above,
        // so the remove is infallible. No retry / re-check needed.
        let mut entry = pending
            .remove(&response.request_id)
            .expect("entry verified above while holding write lock");
        let resolved_at = chrono::Utc::now().timestamp_millis();
        // A compact cleanup marker already seals the first resolution. A
        // retrying caller may help finish persistence, but must not substitute
        // a different outcome or decision. Likewise, a still-content-bearing
        // app notification that crossed its reviewed absolute deadline can
        // only expire; a late raw-ID response cannot revive it after read
        // surfaces have hidden the body.
        let response = if is_app_owner_notification_cleanup_marker(&entry.request) {
            let response = redact_app_notification_cleanup_response(
                &app_notification_cleanup_response(&entry.request),
            );
            if !is_app_owner_notification_replay_tombstone(&entry.request) {
                entry.request =
                    redact_app_owner_notification_request(&entry.request, &response, resolved_at);
            }
            response
        } else if is_app_owner_notification(&entry.request)
            && request_deadline_ms(&entry.request) <= resolved_at
        {
            UserResponse {
                request_id: entry.request.id.clone(),
                decision: entry.request.default_on_timeout.clone(),
                input: None,
                channel: "timeout".to_string(),
                sensitive: Vec::new(),
            }
        } else {
            response
        };
        match try_commit_resolved_request(
            &self.history,
            self.workspace_layout.as_ref(),
            self.history_persist_path.as_deref(),
            self.pending_persist_path.as_deref(),
            &pending,
            self.history_limit,
            &entry.request,
            entry.app_owner_generation.as_deref(),
            &response,
            resolved_at,
        ) {
            ResolvedRequestCommit::Committed => {},
            ResolvedRequestCommit::AppResolutionReady { redacted_request } => {
                if let Some(handle) = entry.timeout_handle.take() {
                    handle.abort();
                }
                let request_id = redacted_request.id.clone();
                entry.request = redacted_request;
                pending.insert(request_id.clone(), entry);
                enqueue_app_notification_cleanup(&self.app_notification_cleanup_queue, &request_id);
                self.ensure_app_notification_cleanup_retry();
                debug!(
                    "[USER-REQUEST] Accepted app notification resolution id={} decision={} channel={}",
                    response.request_id, response.decision, response.channel
                );
                return ScopedResponseResult::Accepted;
            },
            ResolvedRequestCommit::RetryAppCleanup { redacted_request } => {
                if let Some(handle) = entry.timeout_handle.take() {
                    handle.abort();
                }
                let cleanup_request_id = response.request_id.clone();
                entry.request = redacted_request;
                pending.insert(cleanup_request_id.clone(), entry);
                enqueue_app_notification_cleanup(
                    &self.app_notification_cleanup_queue,
                    &cleanup_request_id,
                );
                self.ensure_app_notification_cleanup_retry();
                return ScopedResponseResult::PersistenceUnavailable;
            },
            ResolvedRequestCommit::Rejected => {
                pending.insert(response.request_id.clone(), entry);
                return ScopedResponseResult::PersistenceUnavailable;
            },
        }

        if let (Some(tx), Some(value)) = (entry.sensitive_tx.take(), sensitive_value) {
            // Sending transfers the only owned value; a closed receiver drops and zeroizes it.
            let _ = tx.send(value);
        }
        if !custody_deposits.is_empty() {
            let now_ms = chrono::Utc::now().timestamp_millis();
            let collection_deadline_ms = entry
                .request
                .sensitive
                .as_ref()
                .map(|spec| spec.collection_deadline_ms)
                .unwrap_or(now_ms);
            let deadline_ms = custody_hold_deadline_ms(collection_deadline_ms, now_ms);
            for deposit in custody_deposits.drain(..) {
                self.sensitive_custody.deposit(
                    deposit.reference,
                    deposit.value,
                    deposit.kind,
                    &entry.request.id,
                    &entry.request.principal,
                    &entry.request.workspace,
                    deadline_ms,
                    now_ms,
                );
            }
        }
        if let Some(handle) = entry.timeout_handle {
            handle.abort(); // cancel timeout task (no-op for restored entries)
        }
        // MN2 — restored entries' receivers were dropped at restore
        // time, so `send()` would always Err and the `response.clone()`
        // would be wasted. `is_closed()` short-circuits both.
        if !entry.response_tx.is_closed() {
            let _ = entry.response_tx.send(response.clone());
        }

        // Generic resolutions use their durable history row as a content-free
        // outbox. The response has already crossed its first-response boundary;
        // publication failure therefore queues retry without changing the
        // caller-visible Accepted result.
        drop(pending);
        self.publish_generic_resolution_or_retry(&response.request_id);

        debug!(
            "[USER-REQUEST] Resolved request id={} decision={} channel={}",
            response.request_id, response.decision, response.channel
        );
        ScopedResponseResult::Accepted
    }

    /// Recovery emit for a request that has no entry in the in-memory
    /// `pending` map. Happens when magician restarts between `ask()`
    /// and the operator response, AND the pending snapshot was missing
    /// or corrupted (with the snapshot intact, `with_pending_persist_path`
    /// rehydrates the entry and this path doesn't fire).
    ///
    /// Look up the request in history first — the shared acceptance path saved
    /// the full `UserRequest` before returning, so the context (including
    /// `memory_question_key`) is recoverable in most cases. Scoped callers must
    /// have a matching authoritative history row; only deliberately unscoped
    /// internal callers may fall back to a stub when history also lost the row.
    async fn emit_orphan_resolution(
        &self,
        mut response: UserResponse,
        principal: Option<&str>,
        workspace: Option<&str>,
    ) -> ScopedResponseResult {
        let resolved_at = chrono::Utc::now().timestamp_millis();
        // Preserve the global lock order used by ordinary resolution: pending
        // first, then history. The caller released its earlier guard before
        // entering this recovery path.
        let mut pending = self.pending.write().await;
        if pending.contains_key(&response.request_id) {
            return ScopedResponseResult::PersistenceUnavailable;
        }

        // I2 — recover the original request from history if possible
        // so memory-consolidator dedup (and any other context-driven
        // consumer) keeps working.
        //
        // Also detect the "duplicate respond in the same process" case:
        // if history already has a *genuinely* Resolved record for
        // this request_id, the entry was removed from `pending` because
        // we already accepted a prior response. First-response-wins
        // semantics require us to reject the second response with
        // `AlreadyResolved` — falling through to emit a second
        // `HitlResolved` would double-resolve.
        //
        // EXCEPTION: `normalize_restored_history_records` marks every
        // persisted-Pending row as fake-Resolved with
        // `channel: "service_restart"` on startup (so stale Pending
        // entries don't survive a restart in a misleading state). For
        // legacy requests created BEFORE durable-pending shipped —
        // their pending snapshot is empty, so I1's revert doesn't
        // fire — the operator still has every right to answer them
        // through `/attention`. Treat the synthetic
        // `service_restart` resolution as recoverable, NOT as a real
        // prior response.
        let mut app_publication_marker = None;
        let committed_response;
        let request_for_history = {
            let mut history = self
                .history
                .lock()
                .expect("user request history mutex poisoned");
            let matching = history.record_by_id(&response.request_id);

            // A supplied scope is an ownership assertion, not data for
            // manufacturing an orphan record. Require both dimensions and
            // verify them against durable history before revealing whether a
            // matching request was already resolved. Unknown and cross-scope
            // IDs therefore have the same fail-closed result.
            if principal.is_some() || workspace.is_some() {
                let Some(record) = matching else {
                    debug!(
                        request_id = %response.request_id,
                        "[USER-REQUEST] Rejected scoped orphan response without authoritative history"
                    );
                    return ScopedResponseResult::ScopeMismatch;
                };
                let scope_matches = matches!(
                    (principal, workspace),
                    (Some(expected_principal), Some(expected_workspace))
                        if record.request.principal == expected_principal
                            && record.request.workspace == expected_workspace
                );
                if !scope_matches {
                    debug!(
                        request_id = %response.request_id,
                        expected_principal = principal.unwrap_or(""),
                        expected_workspace = workspace.unwrap_or(""),
                        actual_principal = %record.request.principal,
                        actual_workspace = %record.request.workspace,
                        "[USER-REQUEST] Rejected recovered response due to scope mismatch"
                    );
                    return ScopedResponseResult::ScopeMismatch;
                }
            }

            match matching {
                Some(record)
                    if !self.scope_persistence_recovery_healthy(
                        &record.request.principal,
                        &record.request.workspace,
                    ) =>
                {
                    return ScopedResponseResult::PersistenceUnavailable;
                },
                None if self.has_unhealthy_recovery_scopes() => {
                    // An unscoped recovery stub cannot prove it is unrelated
                    // to a quarantined owner's unreadable authority.
                    return ScopedResponseResult::PersistenceUnavailable;
                },
                _ => {},
            }

            let mut request_for_history = match matching {
                Some(record) => {
                    if record.status == UserRequestStatus::Resolved {
                        let synthetic_restart_marker = record
                            .response
                            .as_ref()
                            .map(|r| r.channel == "service_restart")
                            .unwrap_or(false)
                            || (is_app_owner_notification_cleanup_marker(&record.request)
                                && !app_notification_tombstone_is_resolution_authority(record));
                        if !synthetic_restart_marker {
                            return ScopedResponseResult::AlreadyResolved;
                        }
                    }
                    record.request.clone()
                },
                None => UserRequest {
                    id: response.request_id.clone(),
                    request_type: "recovered_after_restart".to_string(),
                    question: String::new(),
                    options: Vec::new(),
                    principal: String::new(),
                    workspace: String::new(),
                    context: serde_json::Value::Null,
                    source: "recovered_after_restart".to_string(),
                    execution_id: None,
                    task_id: None,
                    timeout_secs: 0,
                    default_on_timeout: String::new(),
                    created_at: resolved_at,
                    sensitive: None,
                },
            };

            if request_for_history.sensitive.is_none() {
                request_for_history.sensitive = classify::classify_sensitive(
                    &request_for_history,
                    request_for_history.created_at,
                );
            }
            if matches!(
                request_for_history.request_type.as_str(),
                SECURE_INPUT_REQUEST | SECURE_CONFIRM_REQUEST
            ) || request_for_history.sensitive.is_some()
            {
                // No live asker can take it; refuse and forget the material.
                // The operator's decision itself is kept (a choice on a
                // request that also collects a secret is still their choice);
                // only an answer that *was* the material becomes a cancel,
                // because there is nothing left to deliver.
                drop(response.input.take().map(Zeroizing::new));
                if response.decision == "provide_input" {
                    response.decision = "cancel".into();
                }
                response.channel = "service_restart".into();
                response.sensitive.clear();
            }
            // Apply the same sealed-outcome rules as the live pending path.
            // A compact cleanup marker already owns its response, and a raw app
            // notification at/past its absolute deadline can only time out.
            committed_response = if is_app_owner_notification_cleanup_marker(&request_for_history) {
                let response = redact_app_notification_cleanup_response(
                    &app_notification_cleanup_response(&request_for_history),
                );
                if !is_app_owner_notification_replay_tombstone(&request_for_history) {
                    request_for_history = redact_app_owner_notification_request(
                        &request_for_history,
                        &response,
                        resolved_at,
                    );
                }
                response
            } else if is_app_owner_notification(&request_for_history)
                && request_deadline_ms(&request_for_history) <= resolved_at
            {
                UserResponse {
                    request_id: request_for_history.id.clone(),
                    decision: request_for_history.default_on_timeout.clone(),
                    input: None,
                    channel: "timeout".to_string(),
                    sensitive: Vec::new(),
                }
            } else {
                response.clone()
            };

            if is_app_owner_notification(&request_for_history) {
                match try_commit_resolved_request_with_history(
                    &mut history,
                    self.workspace_layout.as_ref(),
                    self.history_persist_path.as_deref(),
                    self.pending_persist_path.as_deref(),
                    &pending,
                    self.history_limit,
                    &request_for_history,
                    None,
                    &committed_response,
                    resolved_at,
                ) {
                    ResolvedRequestCommit::Committed => {},
                    ResolvedRequestCommit::AppResolutionReady { redacted_request } => {
                        app_publication_marker = Some(redacted_request);
                    },
                    ResolvedRequestCommit::RetryAppCleanup { redacted_request } => {
                        let request_id = redacted_request.id.clone();
                        let (dummy_tx, dummy_rx) = oneshot::channel::<UserResponse>();
                        drop(dummy_rx);
                        pending.insert(
                            request_id.clone(),
                            PendingRequest {
                                request: redacted_request,
                                app_owner_generation: None,
                                response_tx: dummy_tx,
                                sensitive_tx: None,
                                timeout_handle: None,
                            },
                        );
                        enqueue_app_notification_cleanup(
                            &self.app_notification_cleanup_queue,
                            &request_id,
                        );
                        self.ensure_app_notification_cleanup_retry();
                        return ScopedResponseResult::PersistenceUnavailable;
                    },
                    ResolvedRequestCommit::Rejected => {
                        return ScopedResponseResult::PersistenceUnavailable;
                    },
                }
            } else {
                // Keep the first-response decision and winning write atomic for
                // recovered IDs. Otherwise two concurrent orphan responses can
                // both observe the synthetic restart marker and both emit.
                let mut resolved_scope = history.records_for_scope(
                    &request_for_history.principal,
                    &request_for_history.workspace,
                );
                upsert_resolved_in_history(
                    &mut resolved_scope,
                    &request_for_history,
                    &committed_response,
                    resolved_at,
                );
                trim_history_records(&mut resolved_scope, self.history_limit);
                if generic_resolution_requires_publication_debt(&request_for_history)
                    && !resolved_scope.iter().any(|record| {
                        record.request.id == request_for_history.id
                            && generic_resolution_publication_is_pending(record)
                    })
                {
                    warn!(
                        request_id = %response.request_id,
                        "[USER-REQUEST] Rejected orphan resolution because publication debt was not retained"
                    );
                    return ScopedResponseResult::PersistenceUnavailable;
                }
                if let Err(error) = try_persist_user_request_scope_candidate(
                    self.workspace_layout.as_ref(),
                    self.history_persist_path.as_deref(),
                    &history,
                    &resolved_scope,
                    &request_for_history.principal,
                    &request_for_history.workspace,
                ) {
                    warn!(
                        request_id = %response.request_id,
                        error = %error,
                        "[USER-REQUEST] Rejected orphan resolution because history persistence failed"
                    );
                    return ScopedResponseResult::PersistenceUnavailable;
                }
                history.replace_scope(
                    &request_for_history.principal,
                    &request_for_history.workspace,
                    resolved_scope,
                );
            }
            request_for_history
        };

        if app_publication_marker.is_some() {
            // An app owner generation exists only in the pending owner. If that
            // owner is missing, history alone cannot mint replacement lifecycle
            // authority for the same public correlation id.
            return ScopedResponseResult::PersistenceUnavailable;
        }

        drop(pending);
        if generic_resolution_requires_publication_debt(&request_for_history) {
            self.publish_generic_resolution_or_retry(&committed_response.request_id);
        } else {
            // Preserve the legacy best-effort behavior for deliberately
            // unscoped internal recovery stubs. The durable outbox is only for
            // scoped lifecycle facts accepted by the lifecycle journal.
            self.event_broadcaster
                .emit(RuntimeTransportEvent::HitlResolved {
                    correlation_id: committed_response.request_id.clone(),
                    source: "user_request".to_string(),
                    outcome: "responded".to_string(),
                    decision: Some(committed_response.decision.clone()),
                    task_id: request_for_history.task_id.clone(),
                    execution_id: request_for_history.execution_id.clone(),
                    agent_id: request_owner_agent_id(&request_for_history),
                    principal: (!request_for_history.principal.trim().is_empty())
                        .then(|| request_for_history.principal.clone()),
                    workspace: (!request_for_history.workspace.trim().is_empty())
                        .then(|| request_for_history.workspace.clone()),
                    timestamp: resolved_at,
                });
        }
        debug!(
            request_id = %committed_response.request_id,
            decision = %committed_response.decision,
            channel = %committed_response.channel,
            "[USER-REQUEST] Orphan-resolved request (in-memory entry missing — likely post-restart recovery)"
        );
        ScopedResponseResult::Accepted
    }

    /// Return snapshots of all currently pending requests (for UI listing).
    pub async fn list_pending(&self) -> Vec<UserRequest> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.pending
            .read()
            .await
            .values()
            .filter(|pending| request_is_visible_pending(&pending.request, now_ms))
            .map(|p| p.request.clone())
            .collect()
    }

    /// Return snapshots of all currently pending requests for a scoped caller.
    pub async fn list_pending_for_scope(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Vec<UserRequest> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        self.pending
            .read()
            .await
            .scope_values(principal, workspace)
            .filter(|pending| request_is_visible_pending(&pending.request, now_ms))
            .map(|pending| pending.request.clone())
            .collect()
    }

    /// Snapshot a single pending request's full body by id, scoped. Returns
    /// `None` if the id is unknown, already resolved (no longer pending), or out
    /// of the caller's scope. Pure read — no state change.
    ///
    /// The HITL resolve handler uses this to recover an envoy upward-request's
    /// routing `context` (channel/address/guest_thread_id/payload) BEFORE calling
    /// `respond_scoped` (which removes the entry). The tiny race between snapshot
    /// and `respond_scoped` is benign: `respond_scoped` is the atomic gate, so a
    /// concurrent double-submit relays exactly once. Restart-safe — the entry is
    /// restored into `pending` on startup, so the snapshot still finds it.
    pub async fn pending_request_snapshot(
        &self,
        request_id: &str,
        principal: Option<&str>,
        workspace: Option<&str>,
    ) -> Option<UserRequest> {
        let pending = self.pending.read().await;
        let entry = pending.get(request_id)?;
        match (principal, workspace) {
            (None, None) => {},
            (Some(expected_principal), Some(expected_workspace))
                if entry.request.principal == expected_principal
                    && entry.request.workspace == expected_workspace => {},
            _ => return None,
        }
        if !request_is_visible_pending(&entry.request, chrono::Utc::now().timestamp_millis()) {
            return None;
        }
        Some(entry.request.clone())
    }

    pub fn list_history_for_scope(
        &self,
        principal: &str,
        workspace: &str,
        limit: Option<usize>,
    ) -> Vec<UserRequestRecord> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let history = self
            .history
            .lock()
            .expect("user request history mutex poisoned");
        let mut records = history.records_for_scope(principal, workspace);
        records.retain(|record| {
            !is_app_owner_notification_cleanup_marker(&record.request)
                && !(is_app_owner_notification(&record.request)
                    && request_deadline_ms(&record.request) <= now_ms)
        });
        records.sort_by(|left, right| {
            right
                .latest_activity_at()
                .cmp(&left.latest_activity_at())
                .then_with(|| right.request.created_at.cmp(&left.request.created_at))
        });
        if let Some(limit) = limit {
            records.truncate(limit);
        }
        records
    }

    fn prune_and_persist_history(&self) {
        let mut history = self
            .history
            .lock()
            .expect("user request history mutex poisoned");
        history.trim_all_scopes(self.history_limit);
        if !self
            .legacy_persistence_recovery_healthy
            .load(Ordering::SeqCst)
            || self.has_unhealthy_recovery_scopes()
        {
            // Memory may discard only the typed projection it actually owns.
            // A provider-wide rewrite would interpret every quarantined or
            // unattributable scope omitted from that projection as empty and
            // erase its still-authoritative shard (and possibly the legacy
            // source). Leave all durable owners untouched until recovery has
            // proved every scope independently writable.
            warn!(
                "[USER-REQUEST] Skipped history prune persistence while recovery authority is quarantined"
            );
            return;
        }
        let complete_history = history.to_vec();
        if let Err(error) = try_persist_user_request_history(
            self.workspace_layout.as_ref(),
            self.history_persist_path.as_deref(),
            &complete_history,
        ) {
            // This provider-wide compatibility path cannot attribute a partial
            // rewrite or legacy-owner deletion failure to one scope. Fence all
            // subsequent durable mutations instead of allowing either copy to
            // become silently authoritative.
            self.legacy_persistence_recovery_healthy
                .store(false, Ordering::SeqCst);
            warn!(error = %error, "[USER-REQUEST] Failed to persist pruned request history; fenced durable mutations");
        }
    }

    fn ensure_app_notification_cleanup_retry(&self) {
        spawn_app_notification_cleanup_retry(
            Arc::clone(&self.pending),
            Arc::clone(&self.history),
            Arc::clone(&self.event_broadcaster),
            self.workspace_layout.clone(),
            self.history_persist_path.clone(),
            self.pending_persist_path.clone(),
            self.history_limit,
            self.store_writer_lease.clone(),
            Arc::clone(&self.app_notification_cleanup_retry_started),
            Arc::clone(&self.app_notification_cleanup_queue),
        );
    }

    fn ensure_app_notification_request_publication_retry(&self) {
        spawn_app_notification_request_publication_retry(
            Arc::clone(&self.pending),
            Arc::clone(&self.event_broadcaster),
            self.store_writer_lease.clone(),
            Arc::clone(&self.app_notification_request_publication_retry_started),
            Arc::clone(&self.app_notification_request_publication_queue),
        );
    }

    fn ensure_generic_resolution_publication_retry(&self) {
        spawn_generic_resolution_publication_retry(
            Arc::clone(&self.history),
            Arc::clone(&self.event_broadcaster),
            self.workspace_layout.clone(),
            self.history_persist_path.clone(),
            self.history_limit,
            self.store_writer_lease.clone(),
            Arc::clone(&self.generic_resolution_publication_retry_started),
            Arc::clone(&self.generic_resolution_publication_queue),
        );
    }

    fn ensure_generic_request_publication_retry(&self) {
        spawn_generic_request_publication_retry(
            Arc::clone(&self.history),
            Arc::clone(&self.event_broadcaster),
            self.workspace_layout.clone(),
            self.history_persist_path.clone(),
            self.history_limit,
            self.store_writer_lease.clone(),
            Arc::clone(&self.generic_request_publication_retry_started),
            Arc::clone(&self.generic_request_publication_queue),
        );
    }

    fn publish_generic_request_or_retry(&self, request_id: &str) {
        publish_generic_request_or_enqueue_retry(
            &self.history,
            &self.event_broadcaster,
            self.workspace_layout.as_ref(),
            self.history_persist_path.as_deref(),
            self.history_limit,
            self.store_writer_lease.as_ref(),
            &self.generic_request_publication_retry_started,
            &self.generic_request_publication_queue,
            request_id,
        );
    }

    fn publish_generic_resolution_or_retry(&self, request_id: &str) {
        // A response may win before a previously failed requested append has
        // recovered. Drive the requested debt first; the resolution publisher
        // also refuses admission while that durable bit remains set.
        self.publish_generic_request_or_retry(request_id);
        publish_generic_resolution_or_enqueue_retry(
            &self.history,
            &self.event_broadcaster,
            self.workspace_layout.as_ref(),
            self.history_persist_path.as_deref(),
            self.history_limit,
            self.store_writer_lease.as_ref(),
            &self.generic_resolution_publication_retry_started,
            &self.generic_resolution_publication_queue,
            request_id,
        );
    }
}

fn artifact_v2_error_to_io(error: crate::magician_v2::artifact_v2::ArtifactV2Error) -> io::Error {
    match error {
        crate::magician_v2::artifact_v2::ArtifactV2Error::Io(error) => error,
        error => io::Error::other(error.to_string()),
    }
}

fn read_persisted_bytes_sync(
    workspace_layout: Option<&ArtifactV2Workspace>,
    path: &Path,
    max_bytes: usize,
) -> io::Result<Vec<u8>> {
    if let Some(layout) = workspace_layout {
        let bytes = layout
            .read_prefix_path_sync(path, max_bytes.saturating_add(1) as u64)
            .map_err(artifact_v2_error_to_io)?;
        if bytes.len() > max_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "user request shard exceeds its byte ceiling",
            ));
        }
        return Ok(bytes);
    }
    let file = fs::File::open(path)?;
    let mut bytes = Vec::with_capacity(max_bytes.min(64 * 1024));
    file.take(max_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "user request shard exceeds its byte ceiling",
        ));
    }
    Ok(bytes)
}

fn write_persisted_bytes_atomic_sync(
    workspace_layout: Option<&ArtifactV2Workspace>,
    path: &Path,
    bytes: &[u8],
) -> io::Result<()> {
    if let Some(layout) = workspace_layout {
        return layout
            .write_atomic_path_sync(path, bytes)
            .map_err(artifact_v2_error_to_io);
    }
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    fs::create_dir_all(parent)?;
    // The shared durable writer, not a fixed `<file>.tmp` sibling. Every
    // writer of this store shares that one staging name — history is
    // rewritten on every prune and pending on every resolve — so two of them
    // interleaving publish a half-written record list. The writer also
    // `sync_all`s the file and the directory, and still removes its staging
    // file on failure.
    write_bytes_durably_sync(path, bytes)
}

async fn write_persisted_bytes_atomic(
    workspace_layout: Option<&ArtifactV2Workspace>,
    path: &Path,
    bytes: &[u8],
) -> io::Result<()> {
    if let Some(layout) = workspace_layout {
        return layout
            .write_atomic_path(path, bytes)
            .await
            .map_err(artifact_v2_error_to_io);
    }
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    tokio::fs::create_dir_all(parent).await?;
    // Async twin of `write_persisted_bytes_atomic_sync` above, and the same
    // reason: the fixed `<file>.tmp` staging name was shared by every writer
    // of this store, and neither the staging file nor the directory was
    // `sync_all`ed.
    write_bytes_durably(path, bytes).await
}

// ── Per-scope request sharding ──────────────────────────────────────────────
// Scope-owned flat-file state (user-request history/pending, bot-auth cache)
// lives under each scope at `scopes/<principal>/<workspace>/requests/<file>`
// rather than a single file at the store root. Reads merge every per-scope shard
// plus the legacy single-file location (pre-sharding installs) and dedup by id;
// writes group by scope, rewrite every scope that has (or had) a shard so a
// removed entry can't resurrect from a stale shard, and migrate the legacy file.

fn request_shard_file_name(legacy_path: &Path) -> String {
    legacy_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("user_requests.json")
        .to_string()
}

/// Canonical on-disk owner identity for a request scope. Artifact V2 scope
/// paths normalize unsafe characters, so quarantine and ownership checks must
/// use the same key; comparing a raw principal to a directory segment both
/// falsely rejects legitimate scopes and lets a raw alias bypass quarantine.
fn request_storage_scope(
    workspace_layout: Option<&ArtifactV2Workspace>,
    principal: &str,
    workspace: &str,
) -> (String, String) {
    workspace_layout.map_or_else(
        || (principal.to_owned(), workspace.to_owned()),
        |_| ArtifactV2Workspace::scope_dir_segments(principal, workspace),
    )
}

fn request_scope_is_quarantined(
    workspace_layout: Option<&ArtifactV2Workspace>,
    unhealthy_scopes: &HashSet<(String, String)>,
    principal: &str,
    workspace: &str,
) -> bool {
    unhealthy_scopes.contains(&request_storage_scope(
        workspace_layout,
        principal,
        workspace,
    ))
}

/// A failed post-restore rewrite is also a read boundary. Remove every typed
/// projection for the physical owner and abort its already-armed timeout before
/// startup releases the global guards; unrelated scopes remain live.
fn remove_failed_startup_scope_projections(
    layout: &ArtifactV2Workspace,
    failed: &HashSet<(String, String)>,
    history: &mut HistoryRecords,
    pending: &mut PendingRequests,
) {
    history.retain(|record| {
        !request_scope_is_quarantined(
            Some(layout),
            failed,
            &record.request.principal,
            &record.request.workspace,
        )
    });
    let failed_pending_ids = pending
        .iter()
        .filter(|(_, entry)| {
            request_scope_is_quarantined(
                Some(layout),
                failed,
                &entry.request.principal,
                &entry.request.workspace,
            )
        })
        .map(|(request_id, _)| request_id.clone())
        .collect::<Vec<_>>();
    for request_id in failed_pending_ids {
        if let Some(mut entry) = pending.remove(&request_id) {
            if let Some(timeout_handle) = entry.timeout_handle.take() {
                timeout_handle.abort();
            }
        }
    }
}

/// Per-scope shard paths for `legacy_path`'s file name, plus the legacy
/// single-file location itself (read last, for back-compat).
fn request_shard_paths(
    workspace_layout: Option<&ArtifactV2Workspace>,
    legacy_path: &Path,
) -> Vec<RequestShardPath> {
    let mut shards = Vec::new();
    if let Some(layout) = workspace_layout {
        let file_name = request_shard_file_name(legacy_path);
        for (principal, workspace) in layout.scopes_with_request_shard(&file_name) {
            shards.push(RequestShardPath {
                path: layout.scope_requests_path(&principal, &workspace, &file_name),
                scope: Some((principal, workspace)),
            });
        }
    }
    shards.push(RequestShardPath {
        scope: None,
        path: legacy_path.to_path_buf(),
    });
    shards
}

/// Read a JSON array shard with per-entry resilience (one schema-incompatible
/// entry never drops the whole shard). Missing file → empty.
fn read_request_shard<T: serde::de::DeserializeOwned>(
    workspace_layout: Option<&ArtifactV2Workspace>,
    path: &Path,
    max_records: usize,
) -> RequestShardLoad<T> {
    let bytes = match read_persisted_bytes_sync(
        workspace_layout,
        path,
        MAX_USER_REQUEST_SCOPE_SHARD_BYTES,
    ) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return RequestShardLoad {
                entries: Vec::new(),
                healthy: true,
            };
        },
        Err(error) => {
            warn!(path = %path.display(), error = %error, "[USER-REQUEST] Failed to read request shard");
            return RequestShardLoad {
                entries: Vec::new(),
                healthy: false,
            };
        },
    };
    if !json_bytes_nesting_is_bounded(&bytes, MAX_RETAINED_JSON_DEPTH)
        || !json_bytes_nodes_are_bounded(&bytes, MAX_USER_REQUEST_SHARD_JSON_NODES)
    {
        warn!(
            path = %path.display(),
            "[USER-REQUEST] Request shard exceeds JSON structure admission"
        );
        return RequestShardLoad {
            entries: Vec::new(),
            healthy: false,
        };
    }
    let values: Vec<serde_json::Value> = match serde_json::from_slice(&bytes) {
        Ok(values) => values,
        Err(error) => {
            warn!(path = %path.display(), error = %error, "[USER-REQUEST] Failed to parse request shard");
            return RequestShardLoad {
                entries: Vec::new(),
                healthy: false,
            };
        },
    };
    if values.len() > max_records {
        warn!(
            path = %path.display(),
            record_count = values.len(),
            max_records,
            "[USER-REQUEST] Request shard exceeds its record ceiling"
        );
        return RequestShardLoad {
            entries: Vec::new(),
            healthy: false,
        };
    }
    let mut entries = Vec::with_capacity(values.len().min(max_records));
    let mut healthy = true;
    for value in values {
        match serde_json::from_value(value) {
            Ok(entry) => entries.push(entry),
            Err(_) => healthy = false,
        }
    }
    RequestShardLoad { entries, healthy }
}

/// Write a JSON array shard (sync, atomic) through the workspace provider.
fn try_write_request_shard_sync<T: serde::Serialize>(
    workspace_layout: Option<&ArtifactV2Workspace>,
    path: &Path,
    items: &[&T],
) -> io::Result<()> {
    let max_records = if workspace_layout.is_some() {
        MAX_USER_REQUEST_SCOPE_RECORDS
    } else {
        MAX_USER_REQUEST_LEGACY_RECORDS
    };
    if items.len() > max_records {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "user request shard exceeds its record ceiling",
        ));
    }
    let bytes = serde_json::to_vec_pretty(items).map_err(io::Error::other)?;
    if bytes.len() > MAX_USER_REQUEST_SCOPE_SHARD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "user request shard exceeds its byte ceiling",
        ));
    }
    write_persisted_bytes_atomic_sync(workspace_layout, path, &bytes)
}

fn load_user_request_history(
    workspace_layout: Option<&ArtifactV2Workspace>,
    path: &Path,
    limit: usize,
) -> RequestStoreLoad<UserRequestRecord> {
    let mut by_id: HashMap<String, (UserRequestRecord, (String, String))> = HashMap::new();
    let mut poisoned_ids = HashSet::new();
    let mut unhealthy_scopes = HashSet::new();
    let mut storage_scope_owners: HashMap<(String, String), (String, String)> = HashMap::new();
    let mut legacy_healthy = true;
    for shard in request_shard_paths(workspace_layout, path) {
        let max_records = if shard.scope.is_none() {
            MAX_USER_REQUEST_LEGACY_RECORDS
        } else {
            MAX_USER_REQUEST_SCOPE_RECORDS
        };
        let loaded =
            read_request_shard::<UserRequestRecord>(workspace_layout, &shard.path, max_records);
        if !loaded.healthy {
            match shard.scope {
                Some(scope) => {
                    unhealthy_scopes.insert(scope);
                },
                None => legacy_healthy = false,
            }
            continue;
        }
        if let Some(owner_scope) = shard.scope.as_ref() {
            if loaded.entries.iter().any(|record| {
                request_storage_scope(
                    workspace_layout,
                    &record.request.principal,
                    &record.request.workspace,
                ) != *owner_scope
            }) {
                unhealthy_scopes.insert(owner_scope.clone());
                continue;
            }
        }
        for record in loaded.entries {
            let request_id = record.request.id.clone();
            let logical_scope = (
                record.request.principal.clone(),
                record.request.workspace.clone(),
            );
            let scope = request_storage_scope(
                workspace_layout,
                &record.request.principal,
                &record.request.workspace,
            );
            match storage_scope_owners.entry(scope.clone()) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(logical_scope);
                },
                std::collections::hash_map::Entry::Occupied(entry)
                    if entry.get() == &logical_scope => {},
                std::collections::hash_map::Entry::Occupied(_) => {
                    // Two logical owners that normalize to one directory can
                    // never share a shard safely. Quarantine the physical
                    // owner instead of allowing either alias to rewrite it.
                    unhealthy_scopes.insert(scope);
                    continue;
                },
            }
            if poisoned_ids.contains(&request_id) {
                unhealthy_scopes.insert(scope);
                continue;
            }
            match by_id.entry(request_id.clone()) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert((record, scope));
                },
                std::collections::hash_map::Entry::Occupied(entry) if entry.get().0 == record => {},
                std::collections::hash_map::Entry::Occupied(entry) => {
                    unhealthy_scopes.insert(entry.get().1.clone());
                    unhealthy_scopes.insert(scope);
                    entry.remove();
                    poisoned_ids.insert(request_id);
                },
            }
        }
    }
    let mut records = by_id
        .into_values()
        .map(|(record, _)| record)
        .collect::<Vec<_>>();
    for record in &records {
        if !user_request_is_admitted(&record.request) {
            unhealthy_scopes.insert(request_storage_scope(
                workspace_layout,
                &record.request.principal,
                &record.request.workspace,
            ));
        }
    }
    records.retain(|record| {
        !request_scope_is_quarantined(
            workspace_layout,
            &unhealthy_scopes,
            &record.request.principal,
            &record.request.workspace,
        )
    });
    normalize_restored_history_records(&mut records);
    trim_history_records(&mut records, limit);
    let scopes = records
        .iter()
        .map(|record| {
            (
                record.request.principal.clone(),
                record.request.workspace.clone(),
            )
        })
        .collect::<HashSet<_>>();
    for scope in scopes {
        let scoped = history_records_for_scope(&records, &scope.0, &scope.1);
        if !history_scope_is_admitted(&scoped) {
            unhealthy_scopes.insert(request_storage_scope(workspace_layout, &scope.0, &scope.1));
        }
    }
    records.retain(|record| {
        !request_scope_is_quarantined(
            workspace_layout,
            &unhealthy_scopes,
            &record.request.principal,
            &record.request.workspace,
        )
    });
    RequestStoreLoad {
        entries: records,
        unhealthy_scopes,
        legacy_healthy,
    }
}

fn try_persist_user_request_history(
    workspace_layout: Option<&ArtifactV2Workspace>,
    path: Option<&Path>,
    records: &[UserRequestRecord],
) -> io::Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    let Some(layout) = workspace_layout else {
        // No provider (legacy / in-memory tests): single-file fallback.
        let all: Vec<&UserRequestRecord> = records.iter().collect();
        return try_write_request_shard_sync::<UserRequestRecord>(None, path, &all);
    };
    let file_name = request_shard_file_name(path);
    let mut by_scope: std::collections::HashMap<(String, String), Vec<&UserRequestRecord>> =
        std::collections::HashMap::new();
    let mut logical_owners: HashMap<(String, String), (String, String)> = HashMap::new();
    for record in records {
        let storage_scope = request_storage_scope(
            Some(layout),
            &record.request.principal,
            &record.request.workspace,
        );
        let logical_scope = (
            record.request.principal.clone(),
            record.request.workspace.clone(),
        );
        if logical_owners
            .insert(storage_scope.clone(), logical_scope.clone())
            .is_some_and(|owner| owner != logical_scope)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "distinct request scopes resolve to one storage shard",
            ));
        }
        by_scope.entry(storage_scope).or_default().push(record);
    }
    let mut scopes: std::collections::HashSet<(String, String)> =
        by_scope.keys().cloned().collect();
    scopes.extend(layout.scopes_with_request_shard(&file_name));
    let empty: Vec<&UserRequestRecord> = Vec::new();
    for (principal, workspace) in scopes {
        let shard = layout.scope_requests_path(&principal, &workspace, &file_name);
        let group = by_scope.get(&(principal, workspace)).unwrap_or(&empty);
        try_write_request_shard_sync::<UserRequestRecord>(Some(layout), &shard, group)?;
    }
    // Migrate away the legacy single-file location only after every target
    // shard is durable, and report deletion failure as incomplete migration.
    // Returning success while the flat owner remains lets a later legacy
    // writer diverge from the scoped owners without the caller fencing it.
    if layout
        .metadata_path_sync(path)
        .map_err(artifact_v2_error_to_io)?
        .is_some()
    {
        layout
            .remove_file_path_sync(path)
            .map_err(artifact_v2_error_to_io)?;
    }
    Ok(())
}

/// Persist exactly one scope's history shard for a scoped mutation. Rewriting
/// unrelated shards turns a failure in another tenant into a partial commit of
/// the caller's acceptance, which cannot be rolled back atomically.
fn try_persist_user_request_scope_history(
    workspace_layout: Option<&ArtifactV2Workspace>,
    path: Option<&Path>,
    records: &[UserRequestRecord],
    principal: &str,
    workspace: &str,
) -> io::Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    let Some(layout) = workspace_layout else {
        let all = records.iter().collect::<Vec<_>>();
        return try_write_request_shard_sync::<UserRequestRecord>(None, path, &all);
    };
    let storage_scope = request_storage_scope(Some(layout), principal, workspace);
    if records.iter().any(|record| {
        (record.request.principal != principal || record.request.workspace != workspace)
            && request_storage_scope(
                Some(layout),
                &record.request.principal,
                &record.request.workspace,
            ) == storage_scope
    }) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "distinct request scopes resolve to one storage shard",
        ));
    }
    let shard = layout.scope_requests_path(
        &storage_scope.0,
        &storage_scope.1,
        &request_shard_file_name(path),
    );
    let scoped = records
        .iter()
        .filter(|record| {
            record.request.principal == principal && record.request.workspace == workspace
        })
        .collect::<Vec<_>>();
    try_write_request_shard_sync::<UserRequestRecord>(Some(layout), &shard, &scoped)
}

/// Persist a scope-local candidate without making the legacy single-file
/// fallback forget unrelated scopes. Production provider-backed storage writes
/// the candidate directly to its shard; the compatibility fallback rebuilds
/// the complete flat file only in that legacy mode.
fn try_persist_user_request_scope_candidate(
    workspace_layout: Option<&ArtifactV2Workspace>,
    path: Option<&Path>,
    current_history: &HistoryRecords,
    scoped_candidate: &[UserRequestRecord],
    principal: &str,
    workspace: &str,
) -> io::Result<()> {
    if workspace_layout.is_some() {
        if current_history.has_storage_alias(principal, workspace) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "distinct request scopes resolve to one storage shard",
            ));
        }
        return try_persist_user_request_scope_history(
            workspace_layout,
            path,
            scoped_candidate,
            principal,
            workspace,
        );
    }
    let mut complete_candidate = current_history.to_vec();
    replace_history_scope(
        &mut complete_candidate,
        principal,
        workspace,
        scoped_candidate.to_vec(),
    );
    try_persist_user_request_scope_history(None, path, &complete_candidate, principal, workspace)
}

/// Persist startup reconciliation one owner scope at a time. A failure leaves
/// that scope's pre-recovery shards untouched and does not stop independent
/// scopes from durably completing their bounded cleanup.
fn try_persist_recovered_request_scopes_sync(
    layout: &ArtifactV2Workspace,
    history_path: Option<&Path>,
    pending_path: Option<&Path>,
    history: &HistoryRecords,
    pending: &PendingRequests,
    scopes: HashSet<(String, String)>,
    already_unhealthy: &HashSet<(String, String)>,
) -> HashSet<(String, String)> {
    let mut failed = HashSet::new();
    for (principal, workspace) in scopes {
        let storage_scope = request_storage_scope(Some(layout), &principal, &workspace);
        if already_unhealthy.contains(&storage_scope) {
            continue;
        }
        let scoped_history = history.records_for_scope(&principal, &workspace);
        if try_persist_user_request_scope_history(
            Some(layout),
            history_path,
            &scoped_history,
            &principal,
            &workspace,
        )
        .is_err()
        {
            failed.insert(storage_scope);
            continue;
        }
        if try_write_pending_scope_snapshot_sync(
            Some(layout),
            pending_path,
            pending,
            &principal,
            &workspace,
        )
        .is_err()
        {
            failed.insert(storage_scope);
        }
    }
    failed
}

#[cfg(test)]
fn persist_user_request_history(
    workspace_layout: Option<&ArtifactV2Workspace>,
    path: Option<&Path>,
    records: &[UserRequestRecord],
) {
    if let Err(error) = try_persist_user_request_history(workspace_layout, path, records) {
        if let Some(path) = path {
            warn!(path = %path.display(), error = %error, "[USER-REQUEST] Failed to persist request history");
        }
    }
}

fn load_pending_requests(
    workspace_layout: Option<&ArtifactV2Workspace>,
    path: &Path,
) -> RequestStoreLoad<PersistedPendingRequest> {
    // Merge every per-scope shard plus the legacy single-file snapshot; dedup by
    // id. `read_request_shard` keeps the MN3 per-entry resilience (a single
    // schema-incompatible entry never drops the whole shard).
    let mut by_id: HashMap<String, (PersistedPendingRequest, (String, String))> = HashMap::new();
    let mut poisoned_ids = HashSet::new();
    let mut unhealthy_scopes = HashSet::new();
    let mut storage_scope_owners: HashMap<(String, String), (String, String)> = HashMap::new();
    let mut legacy_healthy = true;
    for shard in request_shard_paths(workspace_layout, path) {
        let max_records = if shard.scope.is_none() {
            MAX_USER_REQUEST_LEGACY_RECORDS
        } else {
            MAX_USER_REQUEST_SCOPE_RECORDS
        };
        let loaded = read_request_shard::<PersistedPendingRequest>(
            workspace_layout,
            &shard.path,
            max_records,
        );
        if !loaded.healthy {
            match shard.scope {
                Some(scope) => {
                    unhealthy_scopes.insert(scope);
                },
                None => legacy_healthy = false,
            }
            continue;
        }
        if let Some(owner_scope) = shard.scope.as_ref() {
            if loaded.entries.iter().any(|row| {
                request_storage_scope(
                    workspace_layout,
                    &row.request.principal,
                    &row.request.workspace,
                ) != *owner_scope
            }) {
                unhealthy_scopes.insert(owner_scope.clone());
                continue;
            }
        }
        for request in loaded.entries {
            let request_id = request.request.id.clone();
            let logical_scope = (
                request.request.principal.clone(),
                request.request.workspace.clone(),
            );
            let scope = request_storage_scope(
                workspace_layout,
                &request.request.principal,
                &request.request.workspace,
            );
            match storage_scope_owners.entry(scope.clone()) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(logical_scope);
                },
                std::collections::hash_map::Entry::Occupied(entry)
                    if entry.get() == &logical_scope => {},
                std::collections::hash_map::Entry::Occupied(_) => {
                    unhealthy_scopes.insert(scope);
                    continue;
                },
            }
            if poisoned_ids.contains(&request_id) {
                unhealthy_scopes.insert(scope);
                continue;
            }
            match by_id.entry(request_id.clone()) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert((request, scope));
                },
                std::collections::hash_map::Entry::Occupied(mut entry)
                    if entry.get().0.request == request.request =>
                {
                    let existing_generation = entry.get().0.app_owner_generation.clone();
                    let candidate_generation = request.app_owner_generation.clone();
                    match (existing_generation, candidate_generation) {
                        (None, Some(generation)) => {
                            entry.get_mut().0.app_owner_generation = Some(generation);
                        },
                        (Some(left), Some(right)) if left != right => {
                            unhealthy_scopes.insert(entry.get().1.clone());
                            unhealthy_scopes.insert(scope);
                            entry.remove();
                            poisoned_ids.insert(request_id);
                        },
                        _ => {},
                    }
                },
                std::collections::hash_map::Entry::Occupied(entry) => {
                    unhealthy_scopes.insert(entry.get().1.clone());
                    unhealthy_scopes.insert(scope);
                    entry.remove();
                    poisoned_ids.insert(request_id);
                },
            }
        }
    }
    let mut requests = by_id
        .into_values()
        .map(|(request, _)| request)
        .collect::<Vec<_>>();
    for request in &requests {
        if !user_request_is_admitted(&request.request) {
            unhealthy_scopes.insert(request_storage_scope(
                workspace_layout,
                &request.request.principal,
                &request.request.workspace,
            ));
        }
    }
    let mut scope_counts: HashMap<(String, String), usize> = HashMap::new();
    for request in &requests {
        let scope = (
            request.request.principal.clone(),
            request.request.workspace.clone(),
        );
        let count = scope_counts.entry(scope.clone()).or_default();
        *count = count.saturating_add(1);
        if *count > MAX_USER_REQUEST_SCOPE_RECORDS {
            unhealthy_scopes.insert(request_storage_scope(workspace_layout, &scope.0, &scope.1));
        }
    }
    requests.retain(|request| {
        !request_scope_is_quarantined(
            workspace_layout,
            &unhealthy_scopes,
            &request.request.principal,
            &request.request.workspace,
        )
    });
    RequestStoreLoad {
        entries: requests,
        unhealthy_scopes,
        legacy_healthy,
    }
}

fn new_app_owner_notification_generation() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn app_owner_notification_generation_is_valid(value: &str) -> bool {
    AppOwnerNotificationPublicationGeneration::parse(value).is_some()
}

fn normalize_restored_pending_generations(
    workspace_layout: Option<&ArtifactV2Workspace>,
    rows: &mut Vec<PersistedPendingRequest>,
) -> HashSet<(String, String)> {
    let mut unhealthy_scopes = HashSet::new();
    for row in rows.iter_mut() {
        let app_owned = app_owner_notification_marker_present(&row.request);
        match (&row.app_owner_generation, app_owned) {
            (Some(generation), true) if app_owner_notification_generation_is_valid(generation) => {
            },
            (None, true) => {
                row.app_owner_generation = Some(new_app_owner_notification_generation());
            },
            (None, false) => {},
            (Some(_), _) => {
                unhealthy_scopes.insert(request_storage_scope(
                    workspace_layout,
                    &row.request.principal,
                    &row.request.workspace,
                ));
            },
        }
    }
    rows.retain(|row| {
        !request_scope_is_quarantined(
            workspace_layout,
            &unhealthy_scopes,
            &row.request.principal,
            &row.request.workspace,
        )
    });
    unhealthy_scopes
}

/// Rewrite legacy flattened JSON-array rows with bounded per-scope shards and
/// private app-owner generations before any recovered app row is republished.
/// The legacy file is removed only after every target shard is durable.
fn try_migrate_pending_request_rows_sync(
    workspace_layout: Option<&ArtifactV2Workspace>,
    legacy_path: &Path,
    rows: &[PersistedPendingRequest],
    remove_legacy_when_complete: bool,
) -> PendingMigrationResult {
    let mut result = PendingMigrationResult {
        unhealthy_scopes: HashSet::new(),
        legacy_healthy: true,
    };
    let Some(layout) = workspace_layout else {
        if !remove_legacy_when_complete {
            // A malformed flat-file row has no independently writable owner.
            // Preserve the bytes verbatim and fence every durable mutation;
            // rewriting only the typed healthy subset would erase authority.
            result.legacy_healthy = false;
            return result;
        }
        result.legacy_healthy = write_pending_rows_sync(None, legacy_path, rows).is_ok();
        return result;
    };
    let file_name = request_shard_file_name(legacy_path);
    let mut by_scope: HashMap<(String, String), Vec<PersistedPendingRequest>> = HashMap::new();
    let mut logical_owners: HashMap<(String, String), (String, String)> = HashMap::new();
    for row in rows {
        let storage_scope =
            request_storage_scope(Some(layout), &row.request.principal, &row.request.workspace);
        let logical_scope = (row.request.principal.clone(), row.request.workspace.clone());
        if logical_owners
            .insert(storage_scope.clone(), logical_scope.clone())
            .is_some_and(|owner| owner != logical_scope)
        {
            result.unhealthy_scopes.insert(storage_scope);
            continue;
        }
        if !user_request_is_admitted(&row.request) {
            result.unhealthy_scopes.insert(storage_scope);
            continue;
        }
        let scoped = by_scope.entry(storage_scope.clone()).or_default();
        if scoped.len() >= MAX_USER_REQUEST_SCOPE_RECORDS {
            result.unhealthy_scopes.insert(storage_scope);
            continue;
        }
        scoped.push(row.clone());
    }
    for ((principal, workspace), scoped) in &by_scope {
        if result
            .unhealthy_scopes
            .contains(&(principal.clone(), workspace.clone()))
        {
            continue;
        }
        let shard = layout.scope_requests_path(principal, workspace, &file_name);
        if write_pending_rows_sync(Some(layout), &shard, scoped).is_err() {
            result
                .unhealthy_scopes
                .insert((principal.clone(), workspace.clone()));
        }
    }
    if remove_legacy_when_complete && result.unhealthy_scopes.is_empty() {
        result.legacy_healthy = match layout.metadata_path_sync(legacy_path) {
            Ok(Some(_)) => layout.remove_file_path_sync(legacy_path).is_ok(),
            Ok(None) => true,
            Err(_) => false,
        };
    }
    result
}

fn user_request_is_admitted(request: &UserRequest) -> bool {
    if request.id.len() > 256
        || request.options.len() > MAX_USER_REQUEST_OPTIONS
        || request
            .options
            .iter()
            .any(|option| option.id.len().saturating_add(option.label.len()) > 16 * 1024)
    {
        return false;
    }
    let option_bytes = request.options.iter().fold(0usize, |total, option| {
        total
            .saturating_add(option.id.len())
            .saturating_add(option.label.len())
    });
    let scalar_bytes = request
        .request_type
        .len()
        .saturating_add(request.question.len())
        .saturating_add(request.principal.len())
        .saturating_add(request.workspace.len())
        .saturating_add(request.source.len())
        .saturating_add(request.default_on_timeout.len())
        .saturating_add(request.execution_id.as_ref().map_or(0, String::len))
        .saturating_add(request.task_id.as_ref().map_or(0, String::len))
        .saturating_add(option_bytes);
    if scalar_bytes > MAX_USER_REQUEST_RECORD_BYTES {
        return false;
    }
    let Some(metrics) = inspect_json_bounded(&request.context, MAX_USER_REQUEST_CONTEXT_NODES)
    else {
        return false;
    };
    if metrics.max_depth.saturating_add(2) > MAX_RETAINED_JSON_DEPTH
        || metrics.maximum_scalar_bytes > MAX_USER_REQUEST_RECORD_BYTES
    {
        return false;
    }
    json_encoded_len(&request.context)
        .ok()
        .is_some_and(|context_bytes| {
            scalar_bytes.saturating_add(context_bytes) <= MAX_USER_REQUEST_RECORD_BYTES
                && serde_json::to_vec(request)
                    .ok()
                    .is_some_and(|bytes| bytes.len() <= MAX_USER_REQUEST_RECORD_BYTES)
        })
}

fn user_response_is_admitted(response: &UserResponse) -> bool {
    response
        .request_id
        .len()
        .saturating_add(response.decision.len())
        .saturating_add(response.input.as_ref().map_or(0, String::len))
        .saturating_add(response.channel.len())
        <= MAX_USER_REQUEST_RECORD_BYTES
        && serde_json::to_vec(response)
            .ok()
            .is_some_and(|bytes| bytes.len() <= MAX_USER_REQUEST_RECORD_BYTES)
}

fn pending_scope_accepts(
    pending: &PendingRequests,
    request: &UserRequest,
    app_owner_generation: Option<&str>,
) -> bool {
    let mut rows = pending
        .scope_values(&request.principal, &request.workspace)
        .map(persisted_pending_request)
        .collect::<Vec<_>>();
    if rows.len() >= MAX_USER_REQUEST_SCOPE_RECORDS {
        return false;
    }
    rows.push(PersistedPendingRequest {
        request: request.clone(),
        app_owner_generation: app_owner_generation.map(str::to_owned),
    });
    serde_json::to_vec_pretty(&rows)
        .ok()
        .is_some_and(|bytes| bytes.len() <= MAX_USER_REQUEST_SCOPE_SHARD_BYTES)
}

fn history_scope_is_admitted(records: &[UserRequestRecord]) -> bool {
    records.len() <= MAX_USER_REQUEST_SCOPE_RECORDS
        && records
            .iter()
            .all(|record| user_request_is_admitted(&record.request))
        && serde_json::to_vec_pretty(records)
            .ok()
            .is_some_and(|bytes| bytes.len() <= MAX_USER_REQUEST_SCOPE_SHARD_BYTES)
}

fn persisted_pending_request(entry: &PendingRequest) -> PersistedPendingRequest {
    PersistedPendingRequest {
        request: entry.request.clone(),
        app_owner_generation: entry.app_owner_generation.clone(),
    }
}

fn write_pending_rows_sync(
    workspace_layout: Option<&ArtifactV2Workspace>,
    path: &Path,
    rows: &[PersistedPendingRequest],
) -> io::Result<()> {
    let references = rows.iter().collect::<Vec<_>>();
    try_write_request_shard_sync(workspace_layout, path, &references)
}

fn pending_rows_for_scope(
    pending: &PendingRequests,
    principal: Option<&str>,
    workspace: Option<&str>,
) -> io::Result<Vec<PersistedPendingRequest>> {
    let mut rows = Vec::new();
    let mut scope_counts: HashMap<(String, String), usize> = HashMap::new();
    let mut push_entry = |entry: &PendingRequest| -> io::Result<()> {
        let scoped_count = scope_counts
            .entry((
                entry.request.principal.clone(),
                entry.request.workspace.clone(),
            ))
            .or_default();
        let total_limit = if principal.is_some() {
            MAX_USER_REQUEST_SCOPE_RECORDS
        } else {
            MAX_USER_REQUEST_LEGACY_RECORDS
        };
        if *scoped_count >= MAX_USER_REQUEST_SCOPE_RECORDS
            || rows.len() >= total_limit
            || !user_request_is_admitted(&entry.request)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "pending request scope exceeds admission",
            ));
        }
        *scoped_count += 1;
        rows.push(persisted_pending_request(entry));
        Ok(())
    };
    if let (Some(principal), Some(workspace)) = (principal, workspace) {
        for entry in pending.scope_values(principal, workspace) {
            push_entry(entry)?;
        }
    } else {
        for entry in pending.values() {
            if principal.is_none_or(|principal| entry.request.principal == principal)
                && workspace.is_none_or(|workspace| entry.request.workspace == workspace)
            {
                push_entry(entry)?;
            }
        }
    }
    Ok(rows)
}

/// Synchronous first-acceptance snapshot. It commits the new owner before
/// history publication and event emission without crossing an async
/// cancellation point. Resolution and restore rewrites continue to use the
/// async twin below.
fn try_write_pending_scope_snapshot_sync(
    workspace_layout: Option<&ArtifactV2Workspace>,
    path: Option<&Path>,
    pending: &PendingRequests,
    principal: &str,
    workspace: &str,
) -> io::Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    let Some(layout) = workspace_layout else {
        let snapshot = pending_rows_for_scope(pending, None, None)?;
        return write_pending_rows_sync(None, path, &snapshot);
    };
    let storage_scope = request_storage_scope(Some(layout), principal, workspace);
    if pending.has_storage_alias(principal, workspace) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "distinct request scopes resolve to one storage shard",
        ));
    }
    let scoped = pending_rows_for_scope(pending, Some(principal), Some(workspace))?;
    let shard = layout.scope_requests_path(
        &storage_scope.0,
        &storage_scope.1,
        &request_shard_file_name(path),
    );
    write_pending_rows_sync(Some(layout), &shard, &scoped)
}

/// Replace the removed app notification with a digest-only pending cleanup
/// marker. This is not resolution success: it merely ensures a crash cannot
/// resurrect the body while either shard owner is still being retried.
fn try_write_pending_scope_cleanup_debt_sync(
    workspace_layout: Option<&ArtifactV2Workspace>,
    path: Option<&Path>,
    pending: &PendingRequests,
    redacted_request: &UserRequest,
    app_owner_generation: Option<&str>,
) -> io::Result<()> {
    let Some(path) = path else {
        return Err(io::Error::other("pending persistence owner is unavailable"));
    };
    let Some(app_owner_generation) = app_owner_generation else {
        return Err(io::Error::other(
            "app notification owner generation is unavailable",
        ));
    };
    let Some(layout) = workspace_layout else {
        let mut snapshot = pending_rows_for_scope(pending, None, None)?;
        snapshot.retain(|row| row.request.id != redacted_request.id);
        snapshot.push(PersistedPendingRequest {
            request: redacted_request.clone(),
            app_owner_generation: Some(app_owner_generation.to_owned()),
        });
        return write_pending_rows_sync(None, path, &snapshot);
    };
    let principal = &redacted_request.principal;
    let workspace = &redacted_request.workspace;
    let storage_scope = request_storage_scope(Some(layout), principal, workspace);
    if pending.has_storage_alias(principal, workspace) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "distinct request scopes resolve to one storage shard",
        ));
    }
    let mut scoped =
        pending_rows_for_scope(pending, Some(principal.as_str()), Some(workspace.as_str()))?;
    scoped.retain(|row| row.request.id != redacted_request.id);
    scoped.push(PersistedPendingRequest {
        request: redacted_request.clone(),
        app_owner_generation: Some(app_owner_generation.to_owned()),
    });
    let shard = layout.scope_requests_path(
        &storage_scope.0,
        &storage_scope.1,
        &request_shard_file_name(path),
    );
    write_pending_rows_sync(Some(layout), &shard, &scoped)
}

fn insert_redacted_app_notification_debt(
    pending: &mut PendingRequests,
    redacted_request: UserRequest,
    app_owner_generation: String,
) {
    let request_id = redacted_request.id.clone();
    let (dummy_tx, dummy_rx) = oneshot::channel::<UserResponse>();
    drop(dummy_rx);
    pending.insert(
        request_id,
        PendingRequest {
            request: redacted_request,
            app_owner_generation: Some(app_owner_generation),
            response_tx: dummy_tx,
            sensitive_tx: None,
            timeout_handle: None,
        },
    );
}

fn enqueue_app_notification_cleanup(
    queue: &Arc<Mutex<AppNotificationCleanupQueue>>,
    request_id: &str,
) {
    queue
        .lock()
        .expect("app notification cleanup queue mutex poisoned")
        .enqueue(request_id.to_owned());
}

fn complete_app_notification_cleanup(
    queue: &Arc<Mutex<AppNotificationCleanupQueue>>,
    request_id: &str,
) {
    queue
        .lock()
        .expect("app notification cleanup queue mutex poisoned")
        .complete(request_id);
}

/// Persist the pending map via async tokio::fs IO so the executor
/// thread isn't blocked while the lock is held. Callers hold the
/// `pending` write guard across this await — that's intentional: serialises
/// writes with the in-memory mutation that produced them. Provider-backed hot
/// mutations use the bounded scope-local synchronous helper; this full-map
/// path remains for bounded legacy fallback and clean startup rewrites.
async fn write_pending_snapshot(
    workspace_layout: Option<&ArtifactV2Workspace>,
    path: Option<&Path>,
    pending: &PendingRequests,
) -> io::Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    let Some(layout) = workspace_layout else {
        // No provider (legacy / in-memory tests): single-file fallback.
        let snapshot = pending_rows_for_scope(pending, None, None)?;
        return write_pending_shard(None, path, &snapshot).await;
    };
    let file_name = request_shard_file_name(path);
    let mut by_scope: std::collections::HashMap<(String, String), Vec<PersistedPendingRequest>> =
        std::collections::HashMap::new();
    let mut logical_owners: HashMap<(String, String), (String, String)> = HashMap::new();
    for entry in pending.values() {
        let storage_scope = request_storage_scope(
            Some(layout),
            &entry.request.principal,
            &entry.request.workspace,
        );
        let logical_scope = (
            entry.request.principal.clone(),
            entry.request.workspace.clone(),
        );
        if logical_owners
            .insert(storage_scope.clone(), logical_scope.clone())
            .is_some_and(|owner| owner != logical_scope)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "distinct request scopes resolve to one storage shard",
            ));
        }
        let scoped = by_scope.entry(storage_scope).or_default();
        if scoped.len() >= MAX_USER_REQUEST_SCOPE_RECORDS
            || !user_request_is_admitted(&entry.request)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "pending request scope exceeds admission",
            ));
        }
        scoped.push(persisted_pending_request(entry));
    }
    let mut scopes: std::collections::HashSet<(String, String)> =
        by_scope.keys().cloned().collect();
    scopes.extend(layout.scopes_with_request_shard(&file_name));
    let empty: Vec<PersistedPendingRequest> = Vec::new();
    for (principal, workspace) in scopes {
        let shard = layout.scope_requests_path(&principal, &workspace, &file_name);
        let group = by_scope.get(&(principal, workspace)).unwrap_or(&empty);
        write_pending_shard(Some(layout), &shard, group).await?;
    }
    // Migrate away the legacy single-file location only after every target
    // shard is durable. Deleting it after a logged-and-ignored target failure
    // loses the sole recovery copy of requests from that scope.
    if layout
        .metadata_path_sync(path)
        .map_err(artifact_v2_error_to_io)?
        .is_some()
    {
        layout
            .remove_file_path_sync(path)
            .map_err(artifact_v2_error_to_io)?;
    }
    Ok(())
}

/// Write a pending-request JSON array shard via async atomic IO.
async fn write_pending_shard(
    workspace_layout: Option<&ArtifactV2Workspace>,
    path: &Path,
    requests: &[PersistedPendingRequest],
) -> io::Result<()> {
    let max_records = if workspace_layout.is_some() {
        MAX_USER_REQUEST_SCOPE_RECORDS
    } else {
        MAX_USER_REQUEST_LEGACY_RECORDS
    };
    if requests.len() > max_records {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "pending request shard exceeds its record ceiling",
        ));
    }
    let bytes = match serde_json::to_vec_pretty(requests) {
        Ok(bytes) => bytes,
        Err(error) => {
            warn!(path = %path.display(), error = %error, "[USER-REQUEST] Failed to serialize pending shard");
            return Err(io::Error::other(error));
        },
    };
    if bytes.len() > MAX_USER_REQUEST_SCOPE_SHARD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "pending request shard exceeds its byte ceiling",
        ));
    }
    write_persisted_bytes_atomic(workspace_layout, path, &bytes).await
}

/// In-memory pending-history upsert. Holds no locks and does no IO — caller is
/// responsible for the lock guard and the eventual bounded persistence.
fn upsert_pending_in_history(history: &mut Vec<UserRequestRecord>, request: &UserRequest) {
    if let Some(record) = history
        .iter_mut()
        .find(|record| record.request.id == request.id)
    {
        record.request = request.clone();
        record.status = UserRequestStatus::Pending;
        record.response = None;
        record.resolved_at = None;
        record.request_publication_pending =
            generic_resolution_requires_publication_debt(request).then_some(true);
        record.resolution_publication_pending = None;
    } else {
        let mut record = UserRequestRecord::pending(request.clone());
        record.request_publication_pending =
            generic_resolution_requires_publication_debt(request).then_some(true);
        history.push(record);
    }
}

/// In-memory variant of `try_commit_resolved_request`'s upsert step. Holds
/// no locks and does no IO — see `upsert_pending_in_history`.
fn upsert_resolved_in_history(
    history: &mut Vec<UserRequestRecord>,
    request: &UserRequest,
    response: &UserResponse,
    resolved_at: i64,
) {
    if let Some(record) = history
        .iter_mut()
        .find(|record| record.request.id == request.id)
    {
        record.request = request.clone();
        record.mark_resolved(response.clone(), resolved_at);
        record.resolution_publication_pending =
            generic_resolution_requires_publication_debt(request).then_some(true);
    } else {
        let mut record = UserRequestRecord::pending(request.clone());
        record.mark_resolved(response.clone(), resolved_at);
        record.resolution_publication_pending =
            generic_resolution_requires_publication_debt(request).then_some(true);
        history.push(record);
    }
}

fn pending_request_matches_history_owner(
    pending_request: &UserRequest,
    history_request: &UserRequest,
) -> bool {
    if pending_request.principal != history_request.principal
        || pending_request.workspace != history_request.workspace
    {
        return false;
    }
    if pending_request == history_request {
        return true;
    }
    if !is_app_owner_notification_replay_tombstone(history_request) {
        return false;
    }
    if is_app_owner_notification_replay_tombstone(pending_request) {
        // A crash between the ordered history and pending writes may leave the
        // owners differing only in this host-local receipt bit. Every value
        // that determines request identity or sealed resolution must be exact.
        let mut pending_marker = pending_request.clone();
        let mut history_marker = history_request.clone();
        if let Some(context) = pending_marker.context.as_object_mut() {
            context.remove("cleanup_owners_committed");
        }
        if let Some(context) = history_marker.context.as_object_mut() {
            context.remove("cleanup_owners_committed");
        }
        return pending_marker == history_marker;
    }
    app_owner_notification_tombstone_digest(history_request).is_some_and(|expected_digest| {
        app_owner_notification_replay_digest(pending_request).as_deref() == Some(expected_digest)
    })
}

/// Cleanup can replace only an absent or exact/digest-bound history owner.
/// The reviewed app TTL authorizes deleting its pending body, never mutating a
/// generic or different-app record that happens to reuse the public id.
fn app_notification_cleanup_may_replace_history_record(
    existing: Option<&UserRequestRecord>,
    pending_request: &UserRequest,
) -> bool {
    existing.is_none_or(|record| {
        pending_request_matches_history_owner(pending_request, &record.request)
    })
}

fn install_app_notification_cleanup_history_markers(
    history: &mut HistoryRecords,
    cleanup_debts: &[(UserRequest, UserResponse)],
    resolved_at: i64,
) {
    for (request, response) in cleanup_debts {
        // Expiry authorizes removal of an app pending body, but it does not
        // authorize replacing a different owner that reused the same public
        // id. Preserve that history row; after expiry the cleanup transaction
        // needs no replay marker to redact/delete pending.
        if !app_notification_cleanup_may_replace_history_record(
            history.record_by_id(&request.id),
            request,
        ) {
            continue;
        }
        let redacted_request = redact_app_owner_notification_request(
            request,
            response,
            app_notification_resolution_timestamp_ms(request, resolved_at),
        );
        history.upsert_resolved(&redacted_request, response, resolved_at);
    }
}

fn replay_or_conflict(
    existing: &UserRequest,
    candidate: &UserRequest,
) -> Result<AcceptedUserRequest, UserRequestSubmissionError> {
    if let Some(expected_digest) = app_owner_notification_tombstone_digest(existing) {
        if app_owner_notification_replay_digest(candidate).as_deref() == Some(expected_digest) {
            let mut replayed = candidate.clone();
            replayed.created_at = existing.created_at;
            return Ok(AcceptedUserRequest {
                request: replayed,
                receipt: UserRequestSubmission::IdempotentReplay {
                    request_id: existing.id.clone(),
                },
                response_rx: None,
            });
        }
        return Err(UserRequestSubmissionError::IdempotencyConflict {
            request_id: candidate.id.clone(),
        });
    }
    let mut normalized_candidate = candidate.clone();
    normalized_candidate.created_at = existing.created_at;
    // The stored row was classified at acceptance; a retrying caller sends
    // the same request unclassified. Classify it the same way before
    // comparing, so an idempotent re-submit replays instead of conflicting.
    if normalized_candidate.sensitive.is_none() {
        normalized_candidate.sensitive =
            classify::classify_sensitive(&normalized_candidate, existing.created_at);
        if normalized_candidate
            .sensitive
            .as_ref()
            .is_some_and(|spec| spec.one_time)
        {
            normalized_candidate.timeout_secs = normalized_candidate
                .timeout_secs
                .clamp(1, classify::ONE_TIME_COLLECTION_MAX_SECS);
        }
    }
    if existing == &normalized_candidate {
        return Ok(AcceptedUserRequest {
            request: existing.clone(),
            receipt: UserRequestSubmission::IdempotentReplay {
                request_id: existing.id.clone(),
            },
            response_rx: None,
        });
    }
    Err(UserRequestSubmissionError::IdempotencyConflict {
        request_id: candidate.id.clone(),
    })
}

fn hitl_requested_event(request: &UserRequest) -> RuntimeTransportEvent {
    let canonical_options: Vec<serde_json::Value> = request
        .options
        .iter()
        .map(|option| {
            serde_json::json!({
                "id": option.id,
                "label": option.label,
                "requires_input": option.requires_input,
            })
        })
        .collect();
    let input_type = request
        .context
        .get("input_type")
        .and_then(|value| value.as_str())
        .unwrap_or("choice")
        .to_string();
    let mut input_schema = serde_json::json!({
        "options": canonical_options,
        "request_type": request.request_type,
        "context": request.context,
        "default_on_timeout": request.default_on_timeout,
    });
    if let Some(extra) = request
        .context
        .get("input_schema")
        .and_then(serde_json::Value::as_object)
    {
        if let Some(schema) = input_schema.as_object_mut() {
            for (key, value) in extra {
                schema.insert(key.clone(), value.clone());
            }
        }
    }
    // The value-free spec decided at accept rides every announcement so a
    // client masks by it, never by the request-type name or the wording. It is
    // written last so a producer's own `input_schema` cannot shadow it.
    if let (Some(spec), Some(schema)) = (&request.sensitive, input_schema.as_object_mut()) {
        if let Ok(spec) = serde_json::to_value(spec) {
            schema.insert("sensitive".to_string(), spec);
        }
    }
    RuntimeTransportEvent::HitlRequested {
        correlation_id: request.id.clone(),
        source: "user_request".to_string(),
        input_type,
        prompt: request.question.clone(),
        hint: request
            .context
            .get("input_schema")
            .and_then(|schema| schema.get("hint"))
            .and_then(|value| value.as_str())
            .map(str::to_string),
        input_schema: Some(input_schema),
        task_id: request.task_id.clone(),
        execution_id: request.execution_id.clone(),
        agent_id: request_owner_agent_id(request),
        principal: Some(request.principal.clone()),
        workspace: Some(request.workspace.clone()),
        timestamp: request.created_at,
    }
}

fn hitl_requested_event_matches(
    actual: &RuntimeTransportEvent,
    expected: &RuntimeTransportEvent,
) -> bool {
    matches!(
        (actual, expected),
        (
            RuntimeTransportEvent::HitlRequested {
                correlation_id: actual_correlation_id,
                source: actual_source,
                input_type: actual_input_type,
                prompt: actual_prompt,
                hint: actual_hint,
                input_schema: actual_input_schema,
                task_id: actual_task_id,
                execution_id: actual_execution_id,
                agent_id: actual_agent_id,
                principal: actual_principal,
                workspace: actual_workspace,
                timestamp: actual_timestamp,
            },
            RuntimeTransportEvent::HitlRequested {
                correlation_id: expected_correlation_id,
                source: expected_source,
                input_type: expected_input_type,
                prompt: expected_prompt,
                hint: expected_hint,
                input_schema: expected_input_schema,
                task_id: expected_task_id,
                execution_id: expected_execution_id,
                agent_id: expected_agent_id,
                principal: expected_principal,
                workspace: expected_workspace,
                timestamp: expected_timestamp,
            },
        ) if actual_correlation_id == expected_correlation_id
            && actual_source == expected_source
            && actual_input_type == expected_input_type
            && actual_prompt == expected_prompt
            && actual_hint == expected_hint
            && actual_input_schema == expected_input_schema
            && actual_task_id == expected_task_id
            && actual_execution_id == expected_execution_id
            && actual_agent_id == expected_agent_id
            && actual_principal == expected_principal
            && actual_workspace == expected_workspace
            && actual_timestamp == expected_timestamp
    )
}

fn emit_hitl_requested(event_broadcaster: &RuntimeTransportBroadcaster, request: &UserRequest) {
    event_broadcaster.emit(hitl_requested_event(request));
}

async fn emit_app_notification_resolution_batch(
    event_broadcaster: Arc<RuntimeTransportBroadcaster>,
    attempts: Vec<(UserRequest, i64, String)>,
) -> Vec<bool> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut accepted = vec![false; attempts.len()];
    let mut live_indexes = Vec::with_capacity(attempts.len());
    let mut lifecycle = Vec::with_capacity(attempts.len());
    for (index, (redacted_request, fallback_timestamp, generation)) in attempts.iter().enumerate() {
        if request_deadline_ms(redacted_request) <= now_ms {
            // The sealed absolute deadline independently authorizes removal of
            // compact publication debt. Never mint a late lifecycle event.
            accepted[index] = true;
            continue;
        }
        let timestamp =
            app_notification_resolution_timestamp_ms(redacted_request, *fallback_timestamp);
        let Some(generation) = AppOwnerNotificationPublicationGeneration::parse(generation) else {
            continue;
        };
        let resolved_event = RuntimeTransportEvent::HitlResolved {
            correlation_id: redacted_request.id.clone(),
            source: "app_owner_notification".to_string(),
            outcome: app_notification_resolution_outcome(redacted_request).to_string(),
            decision: app_notification_resolution_decision(redacted_request),
            task_id: None,
            execution_id: None,
            agent_id: None,
            principal: Some(redacted_request.principal.clone()),
            workspace: Some(redacted_request.workspace.clone()),
            timestamp,
        };
        live_indexes.push(index);
        lifecycle.push((
            hitl_requested_event(redacted_request),
            resolved_event,
            generation,
        ));
    }
    if lifecycle.is_empty() {
        return accepted;
    }

    let batch_accepted =
        match magician_core::blocking_admission::spawn_blocking_admitted(move || {
            event_broadcaster.reconcile_app_owner_notification_lifecycle_batch(lifecycle)
        })
        .await
        {
            Ok(accepted) => accepted,
            Err(error) => {
                warn!(
                    error = %error,
                    debt_count = live_indexes.len(),
                    "[USER-REQUEST] App notification lifecycle batch worker was unavailable"
                );
                vec![false; live_indexes.len()]
            },
        };
    for (live_offset, index) in live_indexes.into_iter().enumerate() {
        let lifecycle_accepted = batch_accepted.get(live_offset).copied().unwrap_or(false);
        // Recheck the absolute boundary after journal work. A failure before
        // then remains content-free cleanup debt; a newly elapsed boundary is
        // independent authority to remove it without a late event.
        let deadline_elapsed =
            request_deadline_ms(&attempts[index].0) <= chrono::Utc::now().timestamp_millis();
        accepted[index] = lifecycle_accepted || deadline_elapsed;
    }
    accepted
}

fn app_notification_resolution_timestamp_ms(request: &UserRequest, fallback: i64) -> i64 {
    let deadline_ms = request_deadline_ms(request);
    request
        .context
        .get("resolution_timestamp_ms")
        .and_then(serde_json::Value::as_i64)
        .filter(|timestamp| *timestamp >= request.created_at && *timestamp < deadline_ms)
        .unwrap_or(fallback)
}

fn request_owner_agent_id(request: &UserRequest) -> Option<String> {
    request
        .context
        .as_object()
        .and_then(|context| context.get("owner_agent_id"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
fn hitl_resolution_source(request: &UserRequest) -> &'static str {
    if is_app_owner_notification(request) {
        "app_owner_notification"
    } else {
        "user_request"
    }
}

fn generic_resolution_requires_publication_debt(request: &UserRequest) -> bool {
    !app_owner_notification_marker_present(request)
        && !request.principal.trim().is_empty()
        && !request.workspace.trim().is_empty()
}

fn generic_request_publication_is_pending(record: &UserRequestRecord) -> bool {
    record.request_publication_pending == Some(true)
        && generic_resolution_requires_publication_debt(&record.request)
        && (record.status == UserRequestStatus::Pending
            || record.resolution_publication_pending == Some(true))
}

fn generic_resolution_publication_is_pending(record: &UserRequestRecord) -> bool {
    record.resolution_publication_pending == Some(true)
        && record.status == UserRequestStatus::Resolved
        && generic_resolution_requires_publication_debt(&record.request)
}

fn generic_resolution_event(record: &UserRequestRecord) -> Option<RuntimeTransportEvent> {
    if !generic_resolution_publication_is_pending(record)
        || generic_request_publication_is_pending(record)
    {
        return None;
    }
    generic_resolution_event_even_if_request_pending(record)
}

fn generic_resolution_event_even_if_request_pending(
    record: &UserRequestRecord,
) -> Option<RuntimeTransportEvent> {
    if !generic_resolution_publication_is_pending(record) {
        return None;
    }
    let response = record.response.as_ref()?;
    let resolved_at = record.resolved_at?;
    Some(RuntimeTransportEvent::HitlResolved {
        correlation_id: record.request.id.clone(),
        source: "user_request".to_string(),
        outcome: if response.channel == "timeout" {
            "expired".to_string()
        } else {
            "responded".to_string()
        },
        decision: Some(response.decision.clone()),
        task_id: record.request.task_id.clone(),
        execution_id: record.request.execution_id.clone(),
        agent_id: request_owner_agent_id(&record.request),
        principal: Some(record.request.principal.clone()),
        workspace: Some(record.request.workspace.clone()),
        timestamp: resolved_at,
    })
}

fn enqueue_generic_resolution_publication(
    queue: &Arc<Mutex<GenericResolutionPublicationQueue>>,
    request_id: &str,
) {
    queue
        .lock()
        .expect("generic resolution publication queue mutex poisoned")
        .enqueue(request_id.to_string());
}

fn complete_generic_resolution_publication(
    queue: &Arc<Mutex<GenericResolutionPublicationQueue>>,
    request_id: &str,
) {
    queue
        .lock()
        .expect("generic resolution publication queue mutex poisoned")
        .complete(request_id);
}

fn enqueue_app_notification_request_publication(
    queue: &Arc<Mutex<GenericResolutionPublicationQueue>>,
    request_id: &str,
) {
    queue
        .lock()
        .expect("app notification request publication queue mutex poisoned")
        .enqueue(request_id.to_string());
}

fn complete_app_notification_request_publication(
    queue: &Arc<Mutex<GenericResolutionPublicationQueue>>,
    request_id: &str,
) {
    queue
        .lock()
        .expect("app notification request publication queue mutex poisoned")
        .complete(request_id);
}

fn enqueue_restored_generic_resolution_publication_debts(
    queue: &Arc<Mutex<GenericResolutionPublicationQueue>>,
    history: &[UserRequestRecord],
) {
    let mut queue = queue
        .lock()
        .expect("generic resolution publication queue mutex poisoned");
    for record in history {
        if generic_resolution_publication_is_pending(record) {
            queue.enqueue(record.request.id.clone());
        }
    }
}

fn enqueue_restored_generic_request_publication_debts(
    queue: &Arc<Mutex<GenericResolutionPublicationQueue>>,
    history: &[UserRequestRecord],
) {
    let mut queue = queue
        .lock()
        .expect("generic request publication queue mutex poisoned");
    for record in history {
        if generic_request_publication_is_pending(record) {
            queue.enqueue(record.request.id.clone());
        }
    }
}

fn try_publish_generic_request_debt(
    history: &Arc<Mutex<HistoryRecords>>,
    event_broadcaster: &RuntimeTransportBroadcaster,
    workspace_layout: Option<&ArtifactV2Workspace>,
    history_persist_path: Option<&Path>,
    history_limit: usize,
    request_id: &str,
) -> bool {
    let (event, principal, workspace) = {
        let history = history.lock().expect("user request history mutex poisoned");
        let Some(record) = history.record_by_id(request_id) else {
            return true;
        };
        if !generic_request_publication_is_pending(record) {
            return true;
        }
        (
            hitl_requested_event(&record.request),
            record.request.principal.clone(),
            record.request.workspace.clone(),
        )
    };

    let accepted = match event_broadcaster.hitl_lifecycle_state(&principal, &workspace, request_id)
    {
        HitlLifecycleState::Pending(actual) => hitl_requested_event_matches(&actual, &event),
        // A standalone resolution historically did not prove which request
        // body/schema preceded it. Never clear requested-event debt from a
        // key-only terminal state.
        HitlLifecycleState::Resolved => false,
        // Keep the submit/restore caller off an unbounded shared-journal
        // reduction. Unknown authority is owned by the admitted blocking batch
        // retry worker; only an exact local pending receipt clears here.
        HitlLifecycleState::Unknown => false,
        HitlLifecycleState::Unavailable => false,
    };
    if !accepted {
        return false;
    }

    let mut history = history.lock().expect("user request history mutex poisoned");
    let Some(current) = history.record_by_id(request_id) else {
        return true;
    };
    if !generic_request_publication_is_pending(current) {
        return true;
    }
    let principal = current.request.principal.clone();
    let workspace = current.request.workspace.clone();
    let mut cleared_history = history.records_for_scope(&principal, &workspace);
    let Some(cleared) = cleared_history
        .iter_mut()
        .find(|record| record.request.id == request_id)
    else {
        return true;
    };
    cleared.request_publication_pending = None;
    trim_history_records(&mut cleared_history, history_limit);
    if let Err(error) = try_persist_user_request_scope_candidate(
        workspace_layout,
        history_persist_path,
        &history,
        &cleared_history,
        &principal,
        &workspace,
    ) {
        warn!(
            request_id,
            error = %error,
            "[USER-REQUEST] Published generic request awaits durable debt clear"
        );
        return false;
    }
    history.replace_scope(&principal, &workspace, cleared_history);
    true
}

fn publish_generic_request_or_enqueue_retry(
    history: &Arc<Mutex<HistoryRecords>>,
    event_broadcaster: &Arc<RuntimeTransportBroadcaster>,
    workspace_layout: Option<&ArtifactV2Workspace>,
    history_persist_path: Option<&Path>,
    history_limit: usize,
    store_writer_lease: Option<&Arc<UserRequestStoreWriterLease>>,
    started: &Arc<AtomicBool>,
    publication_queue: &Arc<Mutex<GenericResolutionPublicationQueue>>,
    request_id: &str,
) {
    if try_publish_generic_request_debt(
        history,
        event_broadcaster,
        workspace_layout,
        history_persist_path,
        history_limit,
        request_id,
    ) {
        complete_generic_resolution_publication(publication_queue, request_id);
        return;
    }
    enqueue_generic_resolution_publication(publication_queue, request_id);
    spawn_generic_request_publication_retry(
        Arc::clone(history),
        Arc::clone(event_broadcaster),
        workspace_layout.cloned(),
        history_persist_path.map(Path::to_path_buf),
        history_limit,
        store_writer_lease.cloned(),
        Arc::clone(started),
        Arc::clone(publication_queue),
    );
}

/// Clear one already-durable generic resolution's content-free history debt
/// only when this process already holds the lifecycle owner's exact receipt. A
/// `true` result means there is no remaining debt for this id; `false` asks the
/// caller to retain/enqueue it for blocking shared-journal reconciliation. The
/// durable clear is scope-local and happens while the history mutex prevents a
/// concurrent first-response mutation.
fn try_publish_generic_resolution_debt(
    history: &Arc<Mutex<HistoryRecords>>,
    event_broadcaster: &RuntimeTransportBroadcaster,
    workspace_layout: Option<&ArtifactV2Workspace>,
    history_persist_path: Option<&Path>,
    history_limit: usize,
    request_id: &str,
) -> bool {
    let event = {
        let history = history.lock().expect("user request history mutex poisoned");
        let Some(record) = history.record_by_id(request_id) else {
            return true;
        };
        if !generic_resolution_publication_is_pending(record) {
            return true;
        }
        if generic_request_publication_is_pending(record) {
            return false;
        }
        let Some(event) = generic_resolution_event(record) else {
            warn!(
                request_id,
                "[USER-REQUEST] Generic resolution publication debt is incomplete"
            );
            return false;
        };
        event
    };

    // Keep the response path strictly local. A missing exact receipt is handed
    // to the bounded retry owner, which performs shared-journal reconciliation
    // on spawn_blocking rather than reducing an unbounded journal here.
    if !event_broadcaster.local_generic_hitl_resolution_matches(&event) {
        return false;
    }

    let mut history = history.lock().expect("user request history mutex poisoned");
    let Some(current) = history.record_by_id(request_id) else {
        return true;
    };
    if !generic_resolution_publication_is_pending(current) {
        return true;
    }
    let principal = current.request.principal.clone();
    let workspace = current.request.workspace.clone();
    let mut cleared_history = history.records_for_scope(&principal, &workspace);
    let Some(cleared) = cleared_history
        .iter_mut()
        .find(|record| record.request.id == request_id)
    else {
        return true;
    };
    cleared.resolution_publication_pending = None;
    trim_history_records(&mut cleared_history, history_limit);
    if let Err(error) = try_persist_user_request_scope_candidate(
        workspace_layout,
        history_persist_path,
        &history,
        &cleared_history,
        &principal,
        &workspace,
    ) {
        warn!(
            request_id,
            error = %error,
            "[USER-REQUEST] Published generic resolution awaits durable debt clear"
        );
        return false;
    }
    history.replace_scope(&principal, &workspace, cleared_history);
    true
}

fn publish_generic_resolution_or_enqueue_retry(
    history: &Arc<Mutex<HistoryRecords>>,
    event_broadcaster: &Arc<RuntimeTransportBroadcaster>,
    workspace_layout: Option<&ArtifactV2Workspace>,
    history_persist_path: Option<&Path>,
    history_limit: usize,
    store_writer_lease: Option<&Arc<UserRequestStoreWriterLease>>,
    started: &Arc<AtomicBool>,
    publication_queue: &Arc<Mutex<GenericResolutionPublicationQueue>>,
    request_id: &str,
) {
    if try_publish_generic_resolution_debt(
        history,
        event_broadcaster,
        workspace_layout,
        history_persist_path,
        history_limit,
        request_id,
    ) {
        complete_generic_resolution_publication(publication_queue, request_id);
        return;
    }
    enqueue_generic_resolution_publication(publication_queue, request_id);
    spawn_generic_resolution_publication_retry(
        Arc::clone(history),
        Arc::clone(event_broadcaster),
        workspace_layout.cloned(),
        history_persist_path.map(Path::to_path_buf),
        history_limit,
        store_writer_lease.cloned(),
        Arc::clone(started),
        Arc::clone(publication_queue),
    );
}

/// Startup-only grouped cleanup. Every member's redacted resolved marker has
/// already been installed in `history`. Persist the complete scope history
/// first, verify that each live notification retained exact replay authority,
/// and only then replace all stale pending bodies in the scope with durable
/// content-free publication markers in one write.
fn try_commit_app_notification_cleanup_scope_with_history(
    history: &[UserRequestRecord],
    workspace_layout: Option<&ArtifactV2Workspace>,
    history_persist_path: Option<&Path>,
    pending_persist_path: Option<&Path>,
    pending: &PendingRequests,
    representative: &UserRequest,
    cleanup_debts: &[(UserRequest, UserResponse)],
) -> bool {
    if history_persist_path.is_none() || pending_persist_path.is_none() {
        return false;
    }
    let history_result = try_persist_user_request_scope_history(
        workspace_layout,
        history_persist_path,
        history,
        &representative.principal,
        &representative.workspace,
    );
    if let Err(error) = history_result {
        warn!(
            principal = %representative.principal,
            workspace = %representative.workspace,
            error = %error,
            "[USER-REQUEST] Grouped app notification history cleanup will retry"
        );
        return false;
    }

    let history_is_authoritative = app_notification_cleanup_history_is_authoritative(
        history,
        cleanup_debts,
        chrono::Utc::now().timestamp_millis(),
    );
    if !history_is_authoritative {
        warn!(
            principal = %representative.principal,
            workspace = %representative.workspace,
            "[USER-REQUEST] Grouped app notification history omitted live replay authority"
        );
        return false;
    }

    if let Err(error) = try_write_pending_scope_snapshot_sync(
        workspace_layout,
        pending_persist_path,
        pending,
        &representative.principal,
        &representative.workspace,
    ) {
        warn!(
            principal = %representative.principal,
            workspace = %representative.workspace,
            error = %error,
            "[USER-REQUEST] Grouped app notification publication markers will retry"
        );
        return false;
    }
    true
}

fn app_notification_cleanup_history_is_authoritative(
    history: &[UserRequestRecord],
    cleanup_debts: &[(UserRequest, UserResponse)],
    now_ms: i64,
) -> bool {
    cleanup_debts.iter().all(|(request, _)| {
        let content_free = !history.iter().any(|record| {
            record.request.id == request.id
                && is_app_owner_notification(&record.request)
                && !is_app_owner_notification_replay_tombstone(&record.request)
        });
        let replay_authority = request_deadline_ms(request) <= now_ms
            || history.iter().any(|record| {
                record.request.id == request.id
                    && record.status == UserRequestStatus::Resolved
                    && is_app_owner_notification_replay_tombstone(&record.request)
                    && app_notification_tombstone_is_resolution_authority(record)
                    && pending_request_matches_history_owner(request, &record.request)
            });
        content_free && replay_authority
    })
}

/// Commit a resolution while the caller owns the pending-map write lock and
/// has already removed `request`. Generic requests retain the historical
/// either-authority compatibility rule. App-owner notifications are stricter:
/// their history owner must first be content-free and durable, then their
/// pending owner must be content-free and durable, before success is reported.
fn try_commit_resolved_request(
    history: &Arc<Mutex<HistoryRecords>>,
    workspace_layout: Option<&ArtifactV2Workspace>,
    history_persist_path: Option<&Path>,
    pending_persist_path: Option<&Path>,
    pending: &PendingRequests,
    limit: usize,
    request: &UserRequest,
    app_owner_generation: Option<&str>,
    response: &UserResponse,
    resolved_at: i64,
) -> ResolvedRequestCommit {
    let mut history = history.lock().expect("user request history mutex poisoned");
    try_commit_resolved_request_with_history(
        &mut history,
        workspace_layout,
        history_persist_path,
        pending_persist_path,
        pending,
        limit,
        request,
        app_owner_generation,
        response,
        resolved_at,
    )
}

fn try_commit_resolved_request_with_history(
    history: &mut HistoryRecords,
    workspace_layout: Option<&ArtifactV2Workspace>,
    history_persist_path: Option<&Path>,
    pending_persist_path: Option<&Path>,
    pending: &PendingRequests,
    limit: usize,
    request: &UserRequest,
    app_owner_generation: Option<&str>,
    response: &UserResponse,
    resolved_at: i64,
) -> ResolvedRequestCommit {
    let app_notification = is_app_owner_notification(request);
    if app_notification
        && !app_owner_generation.is_some_and(app_owner_notification_generation_is_valid)
    {
        return ResolvedRequestCommit::Rejected;
    }
    let generic_request_publication_debt_was_pending = history
        .record_by_id(&request.id)
        .is_some_and(generic_request_publication_is_pending);
    let redacted_request = app_notification
        .then(|| redact_app_owner_notification_request(request, response, resolved_at));
    // Resolution is scope-owned. Cloning/sorting the process-wide history made
    // every response O(all tenants + every live TTL tombstone) while both the
    // global pending and history guards were held. Work on only the affected
    // scope and splice it back after the durable decision.
    let mut resolved_history = history.records_for_scope(&request.principal, &request.workspace);
    upsert_resolved_in_history(
        &mut resolved_history,
        redacted_request.as_ref().unwrap_or(request),
        response,
        resolved_at,
    );
    trim_history_scope_records(&mut resolved_history, limit);
    if !history_scope_is_admitted(&resolved_history) {
        return ResolvedRequestCommit::Rejected;
    }

    let history_result = try_persist_user_request_scope_candidate(
        workspace_layout,
        history_persist_path,
        history,
        &resolved_history,
        &request.principal,
        &request.workspace,
    );
    let resolution_marker_retained = resolved_history.iter().any(|record| {
        record.request.id == request.id && record.status == UserRequestStatus::Resolved
    });
    let generic_publication_debt_required = generic_resolution_requires_publication_debt(request);
    let generic_publication_debt_retained = !generic_publication_debt_required
        || resolved_history.iter().any(|record| {
            record.request.id == request.id && generic_resolution_publication_is_pending(record)
        });
    let generic_request_publication_debt_retained = !generic_request_publication_debt_was_pending
        || resolved_history.iter().any(|record| {
            record.request.id == request.id && generic_request_publication_is_pending(record)
        });
    let history_authoritative = history_persist_path.is_some()
        && history_result.is_ok()
        && resolution_marker_retained
        && generic_publication_debt_retained
        && generic_request_publication_debt_retained;

    if app_notification {
        // A live app notification must retain its digest-only replay marker;
        // after absolute expiry, removing the marker is the intended cleanup.
        // In both cases no full app body may remain in the history owner.
        let history_content_free = !resolved_history.iter().any(|record| {
            record.request.id == request.id
                && is_app_owner_notification(&record.request)
                && !is_app_owner_notification_replay_tombstone(&record.request)
        });
        let history_cleaned = history_persist_path.is_some()
            && history_result.is_ok()
            && history_content_free
            && (request_deadline_ms(request) <= chrono::Utc::now().timestamp_millis()
                || resolution_marker_retained);

        // Process memory must stop retaining the content body even when disk IO
        // failed. The returned redacted debt is the only state allowed to retry.
        history.replace_scope(&request.principal, &request.workspace, resolved_history);
        if !history_cleaned {
            if let Err(error) = &history_result {
                if let Some(path) = history_persist_path {
                    warn!(
                        path = %path.display(),
                        request_id = %request.id,
                        error = %error,
                        "[USER-REQUEST] App notification history cleanup will retry"
                    );
                }
            }
            if let Some(redacted_request) = redacted_request.as_ref() {
                if let Err(error) = try_write_pending_scope_cleanup_debt_sync(
                    workspace_layout,
                    pending_persist_path,
                    pending,
                    redacted_request,
                    app_owner_generation,
                ) {
                    warn!(
                        request_id = %request.id,
                        error = %error,
                        "[USER-REQUEST] Could not replace pending app body with redacted cleanup debt"
                    );
                }
            }
            return ResolvedRequestCommit::RetryAppCleanup {
                redacted_request: redacted_request
                    .expect("app notification cleanup always has redacted debt"),
            };
        }

        // Ordered second owner: replace the content-bearing pending request
        // with a durable digest-only publication marker. The caller publishes
        // HitlResolved only after this succeeds, then deletes the marker. A
        // crash before publication therefore replays the marker; a crash after
        // publication may duplicate the correlation-idempotent resolution but
        // can never resurrect or expose notification content.
        let pending_result = redacted_request
            .as_ref()
            .ok_or_else(|| io::Error::other("app cleanup marker is unavailable"))
            .and_then(|redacted_request| {
                try_write_pending_scope_cleanup_debt_sync(
                    workspace_layout,
                    pending_persist_path,
                    pending,
                    redacted_request,
                    app_owner_generation,
                )
            });
        if let Err(error) = pending_result {
            if let Some(path) = pending_persist_path {
                warn!(
                    path = %path.display(),
                    request_id = %request.id,
                    error = %error,
                    "[USER-REQUEST] App notification pending cleanup will retry"
                );
            }
            return ResolvedRequestCommit::RetryAppCleanup {
                redacted_request: redacted_request
                    .expect("app notification cleanup always has redacted debt"),
            };
        }
        return ResolvedRequestCommit::AppResolutionReady {
            redacted_request: redacted_request
                .expect("app notification cleanup always has redacted publication debt"),
        };
    }

    // In the normal production shape the retained history marker is also the
    // deterministic replay authority. If writing it failed, leave the pending
    // shard untouched and reject the response; deleting pending alone would
    // let an upstream deterministic-ID replay publish a second notification
    // after restart. Expired app notifications intentionally retain no marker,
    // so their only valid authority is successful pending deletion.
    let must_preserve_pending = pending_persist_path.is_some()
        && history_persist_path.is_some()
        && ((generic_publication_debt_required
            && (!generic_publication_debt_retained
                || !generic_request_publication_debt_retained
                || history_result.is_err()))
            || (resolution_marker_retained && history_result.is_err()));
    let pending_result = if must_preserve_pending {
        None
    } else {
        Some(try_write_pending_scope_snapshot_sync(
            workspace_layout,
            pending_persist_path,
            pending,
            &request.principal,
            &request.workspace,
        ))
    };
    let pending_authoritative = pending_persist_path.is_none()
        || pending_result
            .as_ref()
            .map(std::result::Result::is_ok)
            .unwrap_or(false);
    let committed = if generic_publication_debt_required && history_persist_path.is_some() {
        history_authoritative
    } else {
        pending_persist_path.is_none()
            || history_authoritative
            || (!resolution_marker_retained && pending_authoritative)
            || (history_persist_path.is_none() && pending_authoritative)
    };
    if committed {
        history.replace_scope(&request.principal, &request.workspace, resolved_history);
    }

    if let Err(error) = &history_result {
        if let Some(path) = history_persist_path {
            warn!(
                path = %path.display(),
                request_id = %request.id,
                error = %error,
                "[USER-REQUEST] Failed to persist resolved request history"
            );
        }
    }
    if let Some(Err(error)) = &pending_result {
        if let Some(path) = pending_persist_path {
            warn!(
                path = %path.display(),
                request_id = %request.id,
                error = %error,
                "[USER-REQUEST] Failed to persist resolved pending deletion"
            );
        }
    }
    if must_preserve_pending {
        warn!(
            request_id = %request.id,
            "[USER-REQUEST] Skipped pending deletion because deterministic replay history did not commit"
        );
    }
    if !committed {
        warn!(
            request_id = %request.id,
            "[USER-REQUEST] Resolution rejected because no durable authority committed"
        );
    }
    if committed {
        ResolvedRequestCommit::Committed
    } else {
        ResolvedRequestCommit::Rejected
    }
}

struct GenericResolutionPublicationRetryGuard {
    started: Arc<AtomicBool>,
    armed: bool,
}

impl GenericResolutionPublicationRetryGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for GenericResolutionPublicationRetryGuard {
    fn drop(&mut self) {
        if self.armed {
            self.started.store(false, Ordering::Release);
        }
    }
}

/// Publish live app-owner notification requests in fixed pages without ever
/// holding the global pending owner across shared-journal IO. This queue is
/// deliberately separate from resolution cleanup: request publication may
/// retain the reviewed body until its sealed deadline, while cleanup owns only
/// content-free resolution markers.
fn spawn_app_notification_request_publication_retry(
    pending: Arc<RwLock<PendingRequests>>,
    event_broadcaster: Arc<RuntimeTransportBroadcaster>,
    store_writer_lease: Option<Arc<UserRequestStoreWriterLease>>,
    started: Arc<AtomicBool>,
    publication_queue: Arc<Mutex<GenericResolutionPublicationQueue>>,
) {
    if started
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let retry_guard = GenericResolutionPublicationRetryGuard {
        started,
        armed: true,
    };
    let _ = tokio::spawn(async move {
        let _store_writer_lease = store_writer_lease;
        let mut retry_guard = retry_guard;
        let mut retry_delay_secs = 1u64;
        loop {
            // The guard lives only inside this block, so it cannot be held
            // across the yield below -- holding it there is what made this
            // spawned future non-`Send`. The `Empty` arm still clears the
            // ownership bit while the mutex excludes enqueuers, which is the
            // enqueue/start race this queue exists to close.
            let popped = {
                let mut queue_guard = publication_queue
                    .lock()
                    .expect("app notification request publication queue mutex poisoned");
                match queue_guard.pop_batch() {
                    RetryQueuePop::Batch(request_ids) => Some(request_ids),
                    RetryQueuePop::Pending => None,
                    RetryQueuePop::Empty => {
                        retry_guard.started.store(false, Ordering::Release);
                        retry_guard.disarm();
                        return;
                    },
                }
            };
            let Some(request_ids) = popped else {
                tokio::task::yield_now().await;
                continue;
            };

            let now_ms = chrono::Utc::now().timestamp_millis();
            let mut completed = Vec::new();
            let mut metadata = Vec::with_capacity(request_ids.len());
            let mut requests = Vec::with_capacity(request_ids.len());
            {
                let pending_guard = pending.read().await;
                for request_id in request_ids {
                    let Some(entry) = pending_guard.get(&request_id) else {
                        completed.push(request_id);
                        continue;
                    };
                    if is_app_owner_notification_cleanup_marker(&entry.request)
                        || !is_app_owner_notification(&entry.request)
                        || request_deadline_ms(&entry.request) <= now_ms
                    {
                        completed.push(request_id);
                        continue;
                    }
                    let Some(generation) = entry
                        .app_owner_generation
                        .as_deref()
                        .and_then(AppOwnerNotificationPublicationGeneration::parse)
                    else {
                        continue;
                    };
                    metadata.push((
                        request_id,
                        entry.request.clone(),
                        entry
                            .app_owner_generation
                            .clone()
                            .expect("parsed app owner generation remains present"),
                    ));
                    requests.push((hitl_requested_event(&entry.request), generation));
                }
            }

            let mut publication_ticket = if requests.is_empty() {
                None
            } else {
                let reconcile_broadcaster = Arc::clone(&event_broadcaster);
                match magician_core::blocking_admission::spawn_blocking_admitted(move || {
                    reconcile_broadcaster.reconcile_app_owner_notification_request_batch(requests)
                })
                .await
                {
                    Ok(ticket) => Some(ticket),
                    Err(error) => {
                        warn!(
                            error = %error,
                            "[USER-REQUEST] App notification request batch worker was unavailable"
                        );
                        None
                    },
                }
            };

            // The request may resolve, expire, or be replaced by compact
            // cleanup debt while the blocking reconciliation is in flight.
            // Clear only an exact still-live snapshot or a state that no
            // longer permits request publication.
            if let Some(ticket) = publication_ticket.as_mut() {
                // Never suspend while holding cross-process authority. If a
                // response currently owns the pending map, release the ticket
                // and retry the still-queued page after it commits redaction.
                if let Ok(pending_guard) = pending.try_read() {
                    let now_ms = chrono::Utc::now().timestamp_millis();
                    for (index, (request_id, expected, expected_generation)) in
                        metadata.into_iter().enumerate()
                    {
                        match pending_guard.get(&request_id) {
                            None => completed.push(request_id),
                            Some(current)
                                if is_app_owner_notification_cleanup_marker(&current.request)
                                    || !is_app_owner_notification(&current.request)
                                    || request_deadline_ms(&current.request) <= now_ms =>
                            {
                                completed.push(request_id);
                            },
                            Some(current)
                                if ticket.accepted(index)
                                    && current.request == expected
                                    && current.app_owner_generation.as_deref()
                                        == Some(expected_generation.as_str()) =>
                            {
                                if event_broadcaster
                                    .broadcast_reconciled_app_owner_notification_request(
                                        ticket,
                                        index,
                                        hitl_requested_event(&expected),
                                    )
                                {
                                    completed.push(request_id);
                                }
                            },
                            Some(_) => {},
                        }
                    }
                }
            }
            // Release the filesystem lock before touching the retry queue or
            // reaching any async yield/sleep below.
            drop(publication_ticket);

            let made_progress = !completed.is_empty();
            for request_id in completed {
                complete_app_notification_request_publication(&publication_queue, &request_id);
            }
            if made_progress {
                retry_delay_secs = 1;
                tokio::task::yield_now().await;
            } else {
                tokio::time::sleep(tokio::time::Duration::from_secs(retry_delay_secs)).await;
                retry_delay_secs = retry_delay_secs.saturating_mul(2).min(60);
            }
        }
    });
}

fn spawn_generic_request_publication_retry(
    history: Arc<Mutex<HistoryRecords>>,
    event_broadcaster: Arc<RuntimeTransportBroadcaster>,
    workspace_layout: Option<ArtifactV2Workspace>,
    history_persist_path: Option<PathBuf>,
    history_limit: usize,
    store_writer_lease: Option<Arc<UserRequestStoreWriterLease>>,
    started: Arc<AtomicBool>,
    publication_queue: Arc<Mutex<GenericResolutionPublicationQueue>>,
) {
    if started
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let retry_guard = GenericResolutionPublicationRetryGuard {
        started,
        armed: true,
    };
    let _ = tokio::spawn(async move {
        let _store_writer_lease = store_writer_lease;
        let mut retry_guard = retry_guard;
        let mut retry_delay_secs = 1u64;
        loop {
            // The guard lives only inside this block, so it cannot be held
            // across the yield below -- holding it there is what made this
            // spawned future non-`Send`. The `Empty` arm still clears the
            // ownership bit while the mutex excludes enqueuers, which is the
            // enqueue/start race this queue exists to close.
            let popped = {
                let mut queue_guard = publication_queue
                    .lock()
                    .expect("generic request publication queue mutex poisoned");
                match queue_guard.pop_batch() {
                    RetryQueuePop::Batch(request_ids) => Some(request_ids),
                    RetryQueuePop::Pending => None,
                    RetryQueuePop::Empty => {
                        retry_guard.started.store(false, Ordering::Release);
                        retry_guard.disarm();
                        return;
                    },
                }
            };
            let Some(request_ids) = popped else {
                tokio::task::yield_now().await;
                continue;
            };

            let mut attempts = Vec::with_capacity(request_ids.len());
            let mut completed = Vec::new();
            {
                let history_guard = history.lock().expect("user request history mutex poisoned");
                for request_id in &request_ids {
                    let Some(record) = history_guard.record_by_id(request_id) else {
                        completed.push(request_id.clone());
                        continue;
                    };
                    if !generic_request_publication_is_pending(record) {
                        completed.push(record.request.id.clone());
                        continue;
                    }
                    attempts.push((
                        record.request.id.clone(),
                        record.request.principal.clone(),
                        record.request.workspace.clone(),
                        hitl_requested_event(&record.request),
                    ));
                }
            }

            let mut accepted_by_scope: HashMap<(String, String), Vec<String>> = HashMap::new();
            let mut reconciliation_metadata = Vec::with_capacity(attempts.len());
            let mut reconciliation_requests = Vec::with_capacity(attempts.len());
            for (request_id, principal, workspace, event) in attempts {
                match event_broadcaster.hitl_lifecycle_state(&principal, &workspace, &request_id) {
                    HitlLifecycleState::Pending(actual)
                        if hitl_requested_event_matches(&actual, &event) =>
                    {
                        accepted_by_scope
                            .entry((principal, workspace))
                            .or_default()
                            .push(request_id);
                    },
                    HitlLifecycleState::Unknown | HitlLifecycleState::Unavailable => {
                        reconciliation_metadata.push((request_id, principal, workspace));
                        reconciliation_requests.push(event);
                    },
                    HitlLifecycleState::Pending(_) | HitlLifecycleState::Resolved => {},
                }
            }
            if !reconciliation_requests.is_empty() {
                let reconcile_broadcaster = Arc::clone(&event_broadcaster);
                let receipts = match magician_core::blocking_admission::acquire_blocking_admission()
                    .await
                {
                    Ok(permit) => {
                        match magician_core::blocking_admission::spawn_blocking_with_admission(
                            permit,
                            move || {
                                reconcile_broadcaster
                                    .reconcile_generic_hitl_request_batch(reconciliation_requests)
                            },
                        )
                        .await
                        {
                            Ok(receipts) => receipts,
                            Err(error) => {
                                warn!(
                                    error = %error,
                                    "[USER-REQUEST] Generic request lifecycle reconciliation worker failed"
                                );
                                Vec::new()
                            },
                        }
                    },
                    Err(error) => {
                        warn!(
                            error = %error,
                            "[USER-REQUEST] Generic request lifecycle reconciliation could not acquire blocking admission"
                        );
                        Vec::new()
                    },
                };
                for ((request_id, principal, workspace), accepted) in
                    reconciliation_metadata.into_iter().zip(receipts)
                {
                    if accepted {
                        accepted_by_scope
                            .entry((principal, workspace))
                            .or_default()
                            .push(request_id);
                    }
                }
            }

            for ((principal, workspace), accepted_ids) in accepted_by_scope {
                let mut history_guard =
                    history.lock().expect("user request history mutex poisoned");
                let mut cleared_history = history_guard.records_for_scope(&principal, &workspace);
                let mut cleared_ids = Vec::with_capacity(accepted_ids.len());
                let mut already_complete = Vec::new();
                for request_id in &accepted_ids {
                    match cleared_history
                        .iter_mut()
                        .find(|record| record.request.id == *request_id)
                    {
                        None => already_complete.push(request_id.clone()),
                        Some(record) if !generic_request_publication_is_pending(record) => {
                            already_complete.push(request_id.clone());
                        },
                        Some(record)
                            if record.request.principal == principal
                                && record.request.workspace == workspace =>
                        {
                            record.request_publication_pending = None;
                            cleared_ids.push(request_id.clone());
                        },
                        Some(_) => {},
                    }
                }
                if cleared_ids.is_empty() {
                    completed.extend(already_complete);
                    continue;
                }
                trim_history_records(&mut cleared_history, history_limit);
                match try_persist_user_request_scope_candidate(
                    workspace_layout.as_ref(),
                    history_persist_path.as_deref(),
                    &history_guard,
                    &cleared_history,
                    &principal,
                    &workspace,
                ) {
                    Ok(()) => {
                        history_guard.replace_scope(&principal, &workspace, cleared_history);
                        completed.extend(already_complete);
                        completed.extend(cleared_ids);
                    },
                    Err(error) => {
                        warn!(
                            principal,
                            workspace,
                            error = %error,
                            debt_count = cleared_ids.len(),
                            "[USER-REQUEST] Published generic request batch awaits durable debt clear"
                        );
                    },
                }
            }

            let made_progress = !completed.is_empty();
            for request_id in completed {
                complete_generic_resolution_publication(&publication_queue, &request_id);
            }
            if made_progress {
                retry_delay_secs = 1;
                tokio::task::yield_now().await;
            } else {
                tokio::time::sleep(tokio::time::Duration::from_secs(retry_delay_secs)).await;
                retry_delay_secs = retry_delay_secs.saturating_mul(2).min(60);
            }
        }
    });
}

/// Own the complete generic-resolution retry queue with one bounded rotating
/// worker. IDs are coalesced in the queue, each pass touches at most
/// `GENERIC_RESOLUTION_PUBLICATION_BATCH` debts, and failures rotate behind
/// later work so one unhealthy scope cannot starve the rest.
fn spawn_generic_resolution_publication_retry(
    history: Arc<Mutex<HistoryRecords>>,
    event_broadcaster: Arc<RuntimeTransportBroadcaster>,
    workspace_layout: Option<ArtifactV2Workspace>,
    history_persist_path: Option<PathBuf>,
    history_limit: usize,
    store_writer_lease: Option<Arc<UserRequestStoreWriterLease>>,
    started: Arc<AtomicBool>,
    publication_queue: Arc<Mutex<GenericResolutionPublicationQueue>>,
) {
    if started
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let retry_guard = GenericResolutionPublicationRetryGuard {
        started,
        armed: true,
    };
    let _ = tokio::spawn(async move {
        let _store_writer_lease = store_writer_lease;
        let mut retry_guard = retry_guard;
        let mut retry_delay_secs = 1u64;
        loop {
            // The guard lives only inside this block, so it cannot be held
            // across the yield below -- holding it there is what made this
            // spawned future non-`Send`. The `Empty` arm still clears the
            // ownership bit while the mutex excludes enqueuers, which is the
            // enqueue/start race this queue exists to close.
            let popped = {
                let mut queue_guard = publication_queue
                    .lock()
                    .expect("generic resolution publication queue mutex poisoned");
                match queue_guard.pop_batch() {
                    RetryQueuePop::Batch(request_ids) => Some(request_ids),
                    RetryQueuePop::Pending => None,
                    RetryQueuePop::Empty => {
                        // The queue mutex closes the enqueue/start race: a later
                        // enqueuer can observe only the released ownership bit.
                        retry_guard.started.store(false, Ordering::Release);
                        retry_guard.disarm();
                        return;
                    },
                }
            };
            let Some(request_ids) = popped else {
                tokio::task::yield_now().await;
                continue;
            };

            // Materialize the fixed-size page through the exact id index; an
            // unrelated tenant never contributes work to this retry pass.
            let mut attempts = Vec::with_capacity(request_ids.len());
            let mut completed = Vec::new();
            {
                let history_guard = history.lock().expect("user request history mutex poisoned");
                for request_id in &request_ids {
                    let Some(record) = history_guard.record_by_id(request_id) else {
                        completed.push(request_id.clone());
                        continue;
                    };
                    if !generic_resolution_publication_is_pending(record) {
                        completed.push(record.request.id.clone());
                        continue;
                    }
                    if generic_request_publication_is_pending(record) {
                        continue;
                    }
                    if let Some(event) = generic_resolution_event(record) {
                        attempts.push((
                            record.request.id.clone(),
                            record.request.principal.clone(),
                            record.request.workspace.clone(),
                            hitl_requested_event(&record.request),
                            event,
                        ));
                    } else {
                        warn!(
                            request_id = %record.request.id,
                            "[USER-REQUEST] Generic resolution publication debt is incomplete"
                        );
                    }
                }
            }

            // Exact receipts already installed by this process avoid disk work.
            // Every other debt is reconciled on one blocking worker under one
            // shared publication lock and one whole-journal reduction for this
            // fixed-size page. Successful clears remain coalesced to at most
            // one scoped history write per represented scope.
            let mut accepted_by_scope: HashMap<(String, String), Vec<String>> = HashMap::new();
            let mut reconciliation_metadata = Vec::with_capacity(attempts.len());
            let mut reconciliation_lifecycle = Vec::with_capacity(attempts.len());
            for (request_id, principal, workspace, request, resolution) in attempts {
                if event_broadcaster.local_generic_hitl_resolution_matches(&resolution) {
                    accepted_by_scope
                        .entry((principal, workspace))
                        .or_default()
                        .push(request_id);
                } else {
                    reconciliation_metadata.push((request_id, principal, workspace));
                    reconciliation_lifecycle.push((request, resolution));
                }
            }
            if !reconciliation_lifecycle.is_empty() {
                let reconcile_broadcaster = Arc::clone(&event_broadcaster);
                let receipts = match magician_core::blocking_admission::acquire_blocking_admission()
                    .await
                {
                    Ok(permit) => {
                        match magician_core::blocking_admission::spawn_blocking_with_admission(
                            permit,
                            move || {
                                reconcile_broadcaster.reconcile_generic_hitl_resolution_batch(
                                    reconciliation_lifecycle,
                                )
                            },
                        )
                        .await
                        {
                            Ok(receipts) => receipts,
                            Err(error) => {
                                warn!(
                                    error = %error,
                                    "[USER-REQUEST] Generic resolution lifecycle reconciliation worker failed"
                                );
                                Vec::new()
                            },
                        }
                    },
                    Err(error) => {
                        warn!(
                            error = %error,
                            "[USER-REQUEST] Generic resolution lifecycle reconciliation could not acquire blocking admission"
                        );
                        Vec::new()
                    },
                };
                for ((request_id, principal, workspace), accepted) in
                    reconciliation_metadata.into_iter().zip(receipts)
                {
                    if accepted {
                        accepted_by_scope
                            .entry((principal, workspace))
                            .or_default()
                            .push(request_id);
                    }
                }
            }
            for ((principal, workspace), accepted_ids) in accepted_by_scope {
                let mut history_guard =
                    history.lock().expect("user request history mutex poisoned");
                let mut cleared_history = history_guard.records_for_scope(&principal, &workspace);
                let mut cleared_ids = Vec::with_capacity(accepted_ids.len());
                let mut already_complete = Vec::new();
                for request_id in &accepted_ids {
                    match cleared_history
                        .iter_mut()
                        .find(|record| record.request.id == *request_id)
                    {
                        None => already_complete.push(request_id.clone()),
                        Some(record) if !generic_resolution_publication_is_pending(record) => {
                            already_complete.push(request_id.clone());
                        },
                        Some(record)
                            if record.request.principal == principal
                                && record.request.workspace == workspace =>
                        {
                            record.resolution_publication_pending = None;
                            cleared_ids.push(request_id.clone());
                        },
                        Some(_) => {},
                    }
                }
                // Another publisher may already have cleared every member.
                if cleared_ids.is_empty() {
                    completed.extend(already_complete);
                    continue;
                }
                trim_history_records(&mut cleared_history, history_limit);
                match try_persist_user_request_scope_candidate(
                    workspace_layout.as_ref(),
                    history_persist_path.as_deref(),
                    &history_guard,
                    &cleared_history,
                    &principal,
                    &workspace,
                ) {
                    Ok(()) => {
                        history_guard.replace_scope(&principal, &workspace, cleared_history);
                        completed.extend(already_complete);
                        completed.extend(cleared_ids);
                    },
                    Err(error) => {
                        warn!(
                            principal,
                            workspace,
                            error = %error,
                            debt_count = cleared_ids.len(),
                            "[USER-REQUEST] Published generic resolution batch awaits durable debt clear"
                        );
                    },
                }
            }

            let made_progress = !completed.is_empty();
            for request_id in completed {
                complete_generic_resolution_publication(&publication_queue, &request_id);
            }
            if made_progress {
                retry_delay_secs = 1;
                tokio::task::yield_now().await;
            } else {
                tokio::time::sleep(tokio::time::Duration::from_secs(retry_delay_secs)).await;
                retry_delay_secs = retry_delay_secs.saturating_mul(2).min(60);
            }
        }
    });
}

struct AppNotificationCleanupRetryGuard {
    started: Arc<AtomicBool>,
    armed: bool,
}

impl AppNotificationCleanupRetryGuard {
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for AppNotificationCleanupRetryGuard {
    fn drop(&mut self) {
        if self.armed {
            self.started.store(false, Ordering::Release);
        }
    }
}

fn spawn_app_notification_cleanup_retry(
    pending: Arc<RwLock<PendingRequests>>,
    history: Arc<Mutex<HistoryRecords>>,
    event_broadcaster: Arc<RuntimeTransportBroadcaster>,
    workspace_layout: Option<ArtifactV2Workspace>,
    history_persist_path: Option<PathBuf>,
    pending_persist_path: Option<PathBuf>,
    history_limit: usize,
    store_writer_lease: Option<Arc<UserRequestStoreWriterLease>>,
    started: Arc<AtomicBool>,
    cleanup_queue: Arc<Mutex<AppNotificationCleanupQueue>>,
) {
    if started
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let retry_guard = AppNotificationCleanupRetryGuard {
        started,
        armed: true,
    };
    let _ = tokio::spawn(async move {
        let _store_writer_lease = store_writer_lease;
        let mut retry_guard = retry_guard;
        let mut retry_delay_secs = 1u64;
        loop {
            // The guard lives only inside this block, so it cannot be held
            // across the yield below -- holding it there is what made this
            // spawned future non-`Send`. The `Empty` arm still clears the
            // ownership bit while the mutex excludes enqueuers, which is the
            // enqueue/start race this queue exists to close.
            let popped = {
                let mut queue_guard = cleanup_queue
                    .lock()
                    .expect("app notification cleanup queue mutex poisoned");
                match queue_guard.pop_batch() {
                    RetryQueuePop::Batch(request_ids) => Some(request_ids),
                    RetryQueuePop::Pending => None,
                    RetryQueuePop::Empty => {
                        // Clear the ownership bit while the queue mutex still
                        // excludes enqueuers. A caller adding debt after this point
                        // necessarily observes false when it calls the start gate.
                        retry_guard.started.store(false, Ordering::Release);
                        retry_guard.disarm();
                        return;
                    },
                }
            };
            let Some(request_ids) = popped else {
                tokio::task::yield_now().await;
                continue;
            };

            // Discover at most one fixed queue page under a read guard, then
            // process it by scope. Every scope is revalidated under the write
            // guard, and that guard is released before the next scope so one
            // tenant's IO cannot amplify into 32 separate global-lock holds.
            // The flat per-scope shard itself still determines byte work; this
            // page bound coalesces mutations but is not an aggregate byte cap.
            let mut debts_by_scope: HashMap<(String, String), Vec<String>> = HashMap::new();
            let mut completed = Vec::new();
            {
                let pending_guard = pending.read().await;
                for request_id in request_ids {
                    match pending_guard.get(&request_id) {
                        Some(entry) if is_app_owner_notification_cleanup_marker(&entry.request) => {
                            debts_by_scope
                                .entry((
                                    entry.request.principal.clone(),
                                    entry.request.workspace.clone(),
                                ))
                                .or_default()
                                .push(request_id);
                        },
                        _ => completed.push(request_id),
                    }
                }
            }

            let mut retry_needed = false;
            let mut lifecycle_debts = Vec::new();
            for ((principal, workspace), request_ids) in debts_by_scope {
                let mut pending_guard = pending.write().await;
                let resolved_at = chrono::Utc::now().timestamp_millis();
                let mut cleanup_debts = Vec::with_capacity(request_ids.len());
                let mut cleanup_generations = HashMap::with_capacity(request_ids.len());
                let mut newly_committed_markers = Vec::new();
                for request_id in request_ids {
                    let Some(entry) = pending_guard.get_mut(&request_id) else {
                        completed.push(request_id);
                        continue;
                    };
                    if !is_app_owner_notification_cleanup_marker(&entry.request) {
                        completed.push(request_id);
                        continue;
                    }
                    if entry.request.principal != principal || entry.request.workspace != workspace
                    {
                        retry_needed = true;
                        continue;
                    }
                    let Some(generation) = entry.app_owner_generation.clone() else {
                        retry_needed = true;
                        continue;
                    };
                    if !app_owner_notification_generation_is_valid(&generation) {
                        retry_needed = true;
                        continue;
                    }
                    if !is_app_owner_notification_replay_tombstone(&entry.request) {
                        let response = app_notification_cleanup_response(&entry.request);
                        entry.request = redact_app_owner_notification_request(
                            &entry.request,
                            &response,
                            resolved_at,
                        );
                    }
                    if !app_notification_cleanup_owners_committed(&entry.request) {
                        mark_app_notification_cleanup_owners_committed(&mut entry.request, true);
                        newly_committed_markers.push(request_id.clone());
                    }
                    let response = redact_app_notification_cleanup_response(
                        &app_notification_cleanup_response(&entry.request),
                    );
                    cleanup_debts.push((entry.request.clone(), response));
                    cleanup_generations.insert(request_id, generation);
                }
                let Some((representative, _)) = cleanup_debts.first() else {
                    continue;
                };
                let representative = representative.clone();
                let scope_committed = {
                    let mut history_guard =
                        history.lock().expect("user request history mutex poisoned");
                    let current_scope_history =
                        history_guard.records_for_scope(&principal, &workspace);
                    let prior_commit_is_authoritative = newly_committed_markers.is_empty()
                        && app_notification_cleanup_history_is_authoritative(
                            &current_scope_history,
                            &cleanup_debts,
                            resolved_at,
                        );
                    if prior_commit_is_authoritative {
                        true
                    } else {
                        let mut scoped_history = if workspace_layout.is_some() {
                            current_scope_history
                        } else {
                            // Legacy flat-file compatibility: its writer owns
                            // every scope in one file, so preserve unrelated
                            // records even though provider-backed hot paths are
                            // scope-local.
                            history_guard.to_vec()
                        };
                        for (request, response) in &cleanup_debts {
                            if app_notification_cleanup_may_replace_history_record(
                                history_guard.record_by_id(&request.id),
                                request,
                            ) {
                                upsert_resolved_in_history(
                                    &mut scoped_history,
                                    request,
                                    response,
                                    resolved_at,
                                );
                            }
                        }
                        trim_history_records(&mut scoped_history, history_limit);
                        let committed = try_commit_app_notification_cleanup_scope_with_history(
                            &mut scoped_history,
                            workspace_layout.as_ref(),
                            history_persist_path.as_deref(),
                            pending_persist_path.as_deref(),
                            &pending_guard,
                            &representative,
                            &cleanup_debts,
                        );
                        // Even a failed physical rewrite must evict any app body
                        // from process memory; scoped compact debt is the only
                        // state the retry worker may retain.
                        if workspace_layout.is_some() {
                            history_guard.replace_scope(&principal, &workspace, scoped_history);
                        } else {
                            *history_guard = HistoryRecords::from_records(scoped_history);
                        }
                        committed
                    }
                };
                if !scope_committed {
                    for request_id in newly_committed_markers {
                        if let Some(entry) = pending_guard.get_mut(&request_id) {
                            mark_app_notification_cleanup_owners_committed(
                                &mut entry.request,
                                false,
                            );
                        }
                    }
                    retry_needed = true;
                    continue;
                }
                lifecycle_debts.extend(cleanup_debts.into_iter().filter_map(
                    |(request, response)| {
                        cleanup_generations
                            .remove(&request.id)
                            .map(|generation| (request, response, resolved_at, generation))
                    },
                ));
                drop(pending_guard);
            }

            if !lifecycle_debts.is_empty() {
                // A disk-authoritative lifecycle lookup may reduce the entire
                // shared journal. Submit the complete fixed queue page as one
                // admitted blocking job after releasing both UserRequest
                // owners, then exact-value revalidate every marker before
                // removal or waiter wakeup.
                let attempts = lifecycle_debts
                    .iter()
                    .map(|(request, _, resolved_at, generation)| {
                        (request.clone(), *resolved_at, generation.clone())
                    })
                    .collect();
                let lifecycle_accepted = emit_app_notification_resolution_batch(
                    Arc::clone(&event_broadcaster),
                    attempts,
                )
                .await;

                let mut pending_guard = pending.write().await;
                let mut published_by_scope: HashMap<(String, String), Vec<(UserRequest, String)>> =
                    HashMap::new();
                for (index, (request, response, _, generation)) in
                    lifecycle_debts.into_iter().enumerate()
                {
                    if !lifecycle_accepted.get(index).copied().unwrap_or(false) {
                        retry_needed = true;
                        continue;
                    }
                    let exact_marker_still_owned =
                        pending_guard.get(&request.id).is_some_and(|entry| {
                            entry.request == request
                                && entry.app_owner_generation.as_deref()
                                    == Some(generation.as_str())
                        });
                    if exact_marker_still_owned {
                        let entry = pending_guard
                            .remove(&request.id)
                            .expect("exact app cleanup marker was just revalidated");
                        // Lifecycle acceptance is durable; wake the original
                        // waiter before the best-effort marker deletion.
                        if !entry.response_tx.is_closed() {
                            let _ = entry.response_tx.send(response);
                        }
                        published_by_scope
                            .entry((request.principal.clone(), request.workspace.clone()))
                            .or_default()
                            .push((request, generation));
                    } else if !pending_guard.contains_key(&request.id) {
                        // A concurrent responder completed this sealed marker
                        // while the worker was off-thread. A different value is
                        // never consumed by a stale batch result.
                        completed.push(request.id);
                    } else {
                        retry_needed = true;
                    }
                }

                for ((principal, workspace), published) in published_by_scope {
                    if let Err(error) = try_write_pending_scope_snapshot_sync(
                        workspace_layout.as_ref(),
                        pending_persist_path.as_deref(),
                        &pending_guard,
                        &principal,
                        &workspace,
                    ) {
                        warn!(
                            principal,
                            workspace,
                            error = %error,
                            debt_count = published.len(),
                            "[USER-REQUEST] Published app notification batch awaits durable deletion"
                        );
                        for (request, generation) in published {
                            insert_redacted_app_notification_debt(
                                &mut pending_guard,
                                request,
                                generation,
                            );
                        }
                        retry_needed = true;
                        continue;
                    }
                    for (request, _) in published {
                        completed.push(request.id);
                    }
                }
            }

            let made_progress = !completed.is_empty();
            for request_id in completed {
                complete_app_notification_cleanup(&cleanup_queue, &request_id);
            }
            if retry_needed && !made_progress {
                tokio::time::sleep(tokio::time::Duration::from_secs(retry_delay_secs)).await;
                retry_delay_secs = retry_delay_secs.saturating_mul(2).min(60);
            } else {
                retry_delay_secs = 1;
                tokio::task::yield_now().await;
            }
        }
    });
}

fn trim_history_records(records: &mut Vec<UserRequestRecord>, limit: usize) {
    // Persistence and reads are scope-owned, so retention must be as well. A
    // process-wide limit lets a noisy tenant evict another tenant's replay
    // authority and makes hot-path work grow with unrelated scopes.
    let mut by_scope: HashMap<(String, String), Vec<UserRequestRecord>> = HashMap::new();
    for record in records.drain(..) {
        by_scope
            .entry((
                record.request.principal.clone(),
                record.request.workspace.clone(),
            ))
            .or_default()
            .push(record);
    }
    for (_, mut scoped_records) in by_scope {
        trim_history_scope_records(&mut scoped_records, limit);
        records.append(&mut scoped_records);
    }
}

fn trim_history_scope_records(records: &mut Vec<UserRequestRecord>, limit: usize) {
    records.sort_by(|left, right| {
        right
            .latest_activity_at()
            .cmp(&left.latest_activity_at())
            .then_with(|| right.request.created_at.cmp(&left.request.created_at))
    });

    let pending_count = records
        .iter()
        .filter(|record| record.status == UserRequestStatus::Pending)
        .count();
    let max_resolved = limit.saturating_sub(pending_count);
    let mut resolved_kept = 0usize;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut retained = Vec::with_capacity(records.len().min(limit.saturating_add(64)));

    for mut record in records.drain(..) {
        if record.status == UserRequestStatus::Pending {
            retained.push(record);
            continue;
        }
        // Generic lifecycle debts are durable outbox entries, not display
        // history. They survive every limit prune until their ordered
        // requested/resolved facts are accepted and durably cleared.
        if generic_request_publication_is_pending(&record)
            || generic_resolution_publication_is_pending(&record)
        {
            retained.push(record);
            continue;
        }
        if is_app_owner_notification_cleanup_marker(&record.request) {
            if request_deadline_ms(&record.request) <= now_ms {
                continue;
            }
            compact_app_owner_notification_record(&mut record);
            retained.push(record);
            continue;
        }
        if app_owner_notification_marker_present(&record.request) {
            // Resolved app notifications never retain their content body.
            // Their reviewed absolute TTL bounds even the compact exact-replay
            // proof, independently of generic display-history capacity. This
            // marker-first branch also prevents malformed reserved rows from
            // falling back to generic, non-expiring retention.
            if request_deadline_ms(&record.request) <= now_ms {
                continue;
            }
            compact_app_owner_notification_record(&mut record);
            retained.push(record);
            continue;
        }
        if resolved_kept < max_resolved {
            resolved_kept += 1;
            retained.push(record);
        }
    }
    *records = retained;
}

fn history_records_for_scope(
    records: &[UserRequestRecord],
    principal: &str,
    workspace: &str,
) -> Vec<UserRequestRecord> {
    records
        .iter()
        .filter(|record| {
            record.request.principal == principal && record.request.workspace == workspace
        })
        .cloned()
        .collect()
}

fn replace_history_scope(
    records: &mut Vec<UserRequestRecord>,
    principal: &str,
    workspace: &str,
    scoped_records: Vec<UserRequestRecord>,
) {
    records.retain(|record| {
        record.request.principal != principal || record.request.workspace != workspace
    });
    records.extend(scoped_records);
}

fn is_app_owner_notification(request: &UserRequest) -> bool {
    request.source == "app_owner_notification"
        && request
            .context
            .get("app_owner_notification")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
}

/// Marker-first classification for the reserved app-owner notification
/// namespace. Exact valid rows additionally satisfy
/// [`is_app_owner_notification`]; malformed combinations remain reserved so
/// they can be rejected/redacted instead of silently becoming generic HITL.
fn app_owner_notification_marker_present(request: &UserRequest) -> bool {
    request.source == "app_owner_notification"
        || request.request_type == APP_OWNER_NOTIFICATION_REPLAY_TOMBSTONE_TYPE
        || request
            .context
            .get("app_owner_notification")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
}

fn validated_app_owner_notification_deadline_ms(
    request: &UserRequest,
) -> Result<Option<i64>, &'static str> {
    if !app_owner_notification_marker_present(request) {
        return Ok(None);
    }
    if !is_app_owner_notification(request) || is_app_owner_notification_cleanup_marker(request) {
        return Err("app owner notification has an invalid reserved shape");
    }
    let deadline = request
        .context
        .get("absolute_expires_at_ms")
        .and_then(serde_json::Value::as_i64)
        .ok_or("app owner notification is missing its absolute expiry")?;
    if deadline <= request.created_at {
        return Err("app owner notification expiry is not after creation");
    }
    Ok(Some(deadline))
}

/// Reserved marker shapes are always quarantined from user-facing reads and
/// normalized through cleanup, even if a torn/legacy write omitted one of the
/// strict source/context discriminators. The reserved request type is not a
/// valid generic request namespace and is therefore safe to quarantine.
fn is_app_owner_notification_cleanup_marker(request: &UserRequest) -> bool {
    request.request_type == APP_OWNER_NOTIFICATION_REPLAY_TOMBSTONE_TYPE
        || (request
            .context
            .get("replay_tombstone")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
            && (request.source == "app_owner_notification"
                || request
                    .context
                    .get("app_owner_notification")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)))
}

fn is_app_owner_notification_replay_tombstone(request: &UserRequest) -> bool {
    request.request_type == APP_OWNER_NOTIFICATION_REPLAY_TOMBSTONE_TYPE
        && is_app_owner_notification(request)
        && request
            .context
            .get("replay_tombstone")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
}

/// Host-only receipt carried by an already-redacted pending marker. `true`
/// means the matching content-free history authority and this marker were
/// durably written in order, so lifecycle retries need not rewrite either
/// complete scope before every append attempt.
fn app_notification_cleanup_owners_committed(request: &UserRequest) -> bool {
    is_app_owner_notification_replay_tombstone(request)
        && request
            .context
            .get("cleanup_owners_committed")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
}

fn mark_app_notification_cleanup_owners_committed(request: &mut UserRequest, committed: bool) {
    let Some(context) = request.context.as_object_mut() else {
        return;
    };
    if committed {
        context.insert(
            "cleanup_owners_committed".to_string(),
            serde_json::Value::Bool(true),
        );
    } else {
        context.remove("cleanup_owners_committed");
    }
}

fn app_owner_notification_replay_digest(request: &UserRequest) -> Option<String> {
    if !is_app_owner_notification(request) || is_app_owner_notification_replay_tombstone(request) {
        return None;
    }
    let mut normalized = request.clone();
    normalized.created_at = 0;
    let bytes = serde_json::to_vec(&normalized).ok()?;
    Some(blake3::hash(&bytes).to_hex().to_string())
}

fn app_owner_notification_tombstone_digest(request: &UserRequest) -> Option<&str> {
    if !is_app_owner_notification_replay_tombstone(request) {
        return None;
    }
    request
        .context
        .get("request_digest")
        .and_then(serde_json::Value::as_str)
}

fn redact_app_owner_notification_request(
    request: &UserRequest,
    response: &UserResponse,
    resolved_at: i64,
) -> UserRequest {
    // A durable tombstone seals the first resolution. Helping responders and
    // cleanup retries must reproduce that exact lifecycle fact; replacing its
    // timestamp after the worker releases the pending guard could let a stale
    // batch publish one fingerprint while the current marker asks for another,
    // permanently wedging cleanup until expiry.
    let resolved_at = if is_app_owner_notification_replay_tombstone(request) {
        app_notification_resolution_timestamp_ms(request, resolved_at)
    } else {
        resolved_at
    };
    let absolute_expires_at_ms = request_deadline_ms(request);
    let request_digest =
        (absolute_expires_at_ms > chrono::Utc::now().timestamp_millis()).then(|| {
            app_owner_notification_tombstone_digest(request)
                .map(str::to_owned)
                .or_else(|| app_owner_notification_replay_digest(request))
                .unwrap_or_else(|| {
                    // A malformed legacy tombstone has no trustworthy
                    // candidate identity. Retain a deterministic fail-closed
                    // digest only within its reviewed TTL; after expiry even
                    // this compact proof is discarded.
                    let mut normalized = request.clone();
                    normalized.created_at = 0;
                    let bytes = serde_json::to_vec(&normalized)
                        .unwrap_or_else(|_| request.id.as_bytes().to_vec());
                    blake3::hash(&bytes).to_hex().to_string()
                })
        });
    let resolution_outcome = if response.channel == "timeout" {
        "expired"
    } else {
        "responded"
    };
    let resolution_decision = (response.decision == "acknowledge").then_some("acknowledge");
    let mut redacted = request.clone();
    redacted.request_type = APP_OWNER_NOTIFICATION_REPLAY_TOMBSTONE_TYPE.to_owned();
    redacted.source = "app_owner_notification".to_owned();
    redacted.question.clear();
    redacted.options.clear();
    redacted.context = serde_json::json!({
        "app_owner_notification": true,
        "replay_tombstone": true,
        "request_digest": request_digest,
        "absolute_expires_at_ms": absolute_expires_at_ms,
        "resolution_committed": response.channel != "service_restart",
        "resolution_outcome": resolution_outcome,
        "resolution_decision": resolution_decision,
        "resolution_timestamp_ms": resolved_at,
    });
    redacted.execution_id = None;
    redacted.task_id = None;
    redacted.timeout_secs = 0;
    redacted.default_on_timeout.clear();
    redacted
}

fn compact_app_owner_notification_record(record: &mut UserRequestRecord) {
    if is_app_owner_notification_replay_tombstone(&record.request) {
        record.response = None;
        return;
    }
    let response = record.response.clone().unwrap_or_else(|| UserResponse {
        request_id: record.request.id.clone(),
        decision: "acknowledge".to_string(),
        input: None,
        channel: "recovery".to_string(),
        sensitive: Vec::new(),
    });
    let resolved_at = record.resolved_at.unwrap_or(record.request.created_at);
    record.request = redact_app_owner_notification_request(&record.request, &response, resolved_at);
    record.response = None;
}

fn app_notification_cleanup_response(request: &UserRequest) -> UserResponse {
    UserResponse {
        request_id: request.id.clone(),
        decision: app_notification_resolution_decision(request)
            .unwrap_or_else(|| "acknowledge".to_string()),
        input: None,
        channel: if app_notification_resolution_outcome(request) == "expired" {
            "timeout".to_string()
        } else {
            "recovery".to_string()
        },
        sensitive: Vec::new(),
    }
}

fn redact_app_notification_cleanup_response(response: &UserResponse) -> UserResponse {
    UserResponse {
        request_id: response.request_id.clone(),
        decision: (response.decision == "acknowledge")
            .then(|| "acknowledge".to_string())
            .unwrap_or_default(),
        input: None,
        channel: if response.channel == "timeout" {
            "timeout".to_string()
        } else {
            "recovery".to_string()
        },
        sensitive: Vec::new(),
    }
}

fn app_notification_resolution_outcome(request: &UserRequest) -> &str {
    request
        .context
        .get("resolution_outcome")
        .and_then(serde_json::Value::as_str)
        .filter(|outcome| matches!(*outcome, "expired" | "responded"))
        .unwrap_or("responded")
}

fn app_notification_resolution_decision(request: &UserRequest) -> Option<String> {
    request
        .context
        .get("resolution_decision")
        .and_then(serde_json::Value::as_str)
        .filter(|decision| *decision == "acknowledge")
        .map(str::to_owned)
}

fn app_notification_tombstone_is_resolution_authority(record: &UserRequestRecord) -> bool {
    record
        .request
        .context
        .get("resolution_committed")
        .and_then(serde_json::Value::as_bool)
        // Legacy synthetic restart normalization resolved at the acceptance
        // timestamp. Treat that exact shape as pending when the old tombstone
        // predates the explicit discriminator; otherwise preserve prior replay
        // authority.
        .unwrap_or(record.resolved_at != Some(record.request.created_at))
}

fn request_is_visible_pending(request: &UserRequest, now_ms: i64) -> bool {
    !is_app_owner_notification_cleanup_marker(request)
        && !(app_owner_notification_marker_present(request)
            && request_deadline_ms(request) <= now_ms)
}

fn request_deadline_ms(request: &UserRequest) -> i64 {
    if app_owner_notification_marker_present(request) {
        if let Some(deadline) = request
            .context
            .get("absolute_expires_at_ms")
            .and_then(serde_json::Value::as_i64)
        {
            return deadline;
        }
        // Reserved marker-bearing rows never inherit generic timeout
        // semantics. Missing sealed expiry is already invalid at admission; on
        // recovery/read paths an immediately-expired sentinel hides and
        // compacts the body fail closed.
        return i64::MIN;
    }
    request
        .created_at
        .saturating_add((request.timeout_secs as i64).saturating_mul(1000))
}

fn normalize_restored_history_records(records: &mut [UserRequestRecord]) {
    for record in records.iter_mut() {
        if record.status == UserRequestStatus::Pending {
            record.mark_resolved(
                UserResponse {
                    request_id: record.request.id.clone(),
                    decision: record.request.default_on_timeout.clone(),
                    input: Some(
                        "Recovered from persisted history after restart; original pending request is no longer actionable."
                            .to_string(),
                    ),
                    channel: "service_restart".to_string(),
                    sensitive: Vec::new(),
                },
                record.request.created_at,
            );
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn make_broadcaster() -> Arc<RuntimeTransportBroadcaster> {
        Arc::new(RuntimeTransportBroadcaster::new(64))
    }

    fn sample_request() -> UserRequest {
        UserRequest {
            id: String::new(),
            request_type: "tool_authorization".to_string(),
            question: "Allow tool X?".to_string(),
            options: vec![
                RequestOption {
                    id: "allow_once".to_string(),
                    label: "Allow Once".to_string(),
                    requires_input: false,
                },
                RequestOption {
                    id: "deny".to_string(),
                    label: "Deny".to_string(),
                    requires_input: false,
                },
            ],
            principal: "default".to_string(),
            workspace: "default".to_string(),
            context: serde_json::json!({"tool": "my_tool"}),
            source: "executor".to_string(),
            execution_id: Some("exec-1".to_string()),
            task_id: None,
            timeout_secs: 300,
            default_on_timeout: "deny".to_string(),
            created_at: 0,
            sensitive: None,
        }
    }

    fn app_owner_notification_request(id: &str, absolute_expires_at_ms: i64) -> UserRequest {
        let mut request = sample_request();
        request.id = id.to_string();
        request.request_type = "notify_owner.info".to_string();
        request.question = "sensitive notification body".to_string();
        request.source = "app_owner_notification".to_string();
        request.context = serde_json::json!({
            "app_owner_notification": true,
            "one_way": true,
            "absolute_expires_at_ms": absolute_expires_at_ms,
        });
        request.created_at = 0;
        request
    }

    #[test]
    fn app_notification_marker_is_reserved_and_requires_exact_absolute_expiry() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut exact = app_owner_notification_request("owner-notify:exact", now_ms + 60_000);
        exact.created_at = now_ms;
        assert_eq!(
            validated_app_owner_notification_deadline_ms(&exact),
            Ok(Some(now_ms + 60_000))
        );

        let mut wrong_source = exact.clone();
        wrong_source.source = "executor".to_string();
        assert!(app_owner_notification_marker_present(&wrong_source));
        assert!(validated_app_owner_notification_deadline_ms(&wrong_source).is_err());

        let mut missing_expiry = exact;
        missing_expiry
            .context
            .as_object_mut()
            .expect("notification context")
            .remove("absolute_expires_at_ms");
        assert!(validated_app_owner_notification_deadline_ms(&missing_expiry).is_err());
        assert_eq!(request_deadline_ms(&missing_expiry), i64::MIN);
    }

    #[test]
    fn legacy_app_pending_row_migrates_to_private_random_generation() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let request = app_owner_notification_request("owner-notify:migrate", now_ms + 60_000);
        let mut rows = vec![PersistedPendingRequest {
            request: request.clone(),
            app_owner_generation: None,
        }];

        assert!(normalize_restored_pending_generations(None, &mut rows).is_empty());
        let generation = rows[0]
            .app_owner_generation
            .as_deref()
            .expect("legacy app row receives a private owner generation");
        assert!(app_owner_notification_generation_is_valid(generation));
        assert!(request.context.get("__app_owner_generation_v1").is_none());

        let encoded = serde_json::to_vec(&rows).expect("serialize migrated pending row");
        assert!(String::from_utf8_lossy(&encoded).contains("__app_owner_generation_v1"));
        let public_rows: Vec<UserRequest> =
            serde_json::from_slice(&encoded).expect("public legacy reader ignores private sibling");
        assert_eq!(public_rows, vec![request]);
    }

    #[test]
    fn conflicting_private_generations_quarantine_the_persisted_owner_scope() {
        let temp = tempdir().expect("tempdir");
        let pending_path = temp.path().join("user-request-pending.json");
        let now_ms = chrono::Utc::now().timestamp_millis();
        let request =
            app_owner_notification_request("owner-notify:generation-conflict", now_ms + 60_000);
        let rows = vec![
            PersistedPendingRequest {
                request: request.clone(),
                app_owner_generation: Some(new_app_owner_notification_generation()),
            },
            PersistedPendingRequest {
                request: request.clone(),
                app_owner_generation: Some(new_app_owner_notification_generation()),
            },
        ];
        std::fs::write(
            &pending_path,
            serde_json::to_vec(&rows).expect("serialize conflicting owners"),
        )
        .expect("seed conflicting owners");

        let loaded = load_pending_requests(None, &pending_path);

        assert!(loaded.legacy_healthy);
        assert!(loaded.entries.is_empty());
        assert!(loaded
            .unhealthy_scopes
            .contains(&(request.principal, request.workspace)));
    }

    #[test]
    fn pending_and_history_owners_require_exact_or_digest_bound_identity() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let request = app_owner_notification_request("owner-notify:cross-owner", now_ms + 60_000);
        let response = UserResponse {
            request_id: request.id.clone(),
            decision: "acknowledge".to_owned(),
            input: None,
            channel: "web".to_owned(),
            sensitive: Vec::new(),
        };
        let tombstone = redact_app_owner_notification_request(&request, &response, now_ms);
        assert!(pending_request_matches_history_owner(&request, &tombstone));

        let mut substituted = request.clone();
        substituted.question = "substituted private body".to_owned();
        assert!(!pending_request_matches_history_owner(
            &substituted,
            &tombstone,
        ));

        let mut cross_scope = request.clone();
        cross_scope.workspace = "other-workspace".to_owned();
        assert!(!pending_request_matches_history_owner(
            &cross_scope,
            &tombstone,
        ));

        let mut pending_marker = tombstone.clone();
        mark_app_notification_cleanup_owners_committed(&mut pending_marker, true);
        assert!(pending_request_matches_history_owner(
            &pending_marker,
            &tombstone,
        ));
    }

    #[test]
    fn cleanup_debt_rewrite_preserves_private_generation() {
        let temp = tempdir().expect("tempdir");
        let pending_path = temp.path().join("user-request-pending.json");
        let now_ms = chrono::Utc::now().timestamp_millis();
        let request =
            app_owner_notification_request("owner-notify:cleanup-generation", now_ms + 60_000);
        let response = UserResponse {
            request_id: request.id.clone(),
            decision: "acknowledge".to_owned(),
            input: None,
            channel: "web".to_owned(),
            sensitive: Vec::new(),
        };
        let redacted = redact_app_owner_notification_request(&request, &response, now_ms);
        let generation = new_app_owner_notification_generation();

        try_write_pending_scope_cleanup_debt_sync(
            None,
            Some(&pending_path),
            &PendingRequests::default(),
            &redacted,
            Some(&generation),
        )
        .expect("persist cleanup debt");
        let loaded = load_pending_requests(None, &pending_path);

        assert!(loaded.legacy_healthy);
        assert!(loaded.unhealthy_scopes.is_empty());
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(
            loaded.entries[0].app_owner_generation.as_deref(),
            Some(generation.as_str())
        );
        assert!(is_app_owner_notification_cleanup_marker(
            &loaded.entries[0].request
        ));
    }

    #[test]
    fn request_and_scope_admission_apply_hard_byte_and_count_backpressure() {
        let mut oversized = sample_request();
        oversized.question = "x".repeat(MAX_USER_REQUEST_RECORD_BYTES + 1);
        assert!(!user_request_is_admitted(&oversized));

        let mut pending = PendingRequests::default();
        for index in 0..MAX_USER_REQUEST_SCOPE_RECORDS {
            let mut request = sample_request();
            request.id = format!("bounded-{index:04}");
            let (response_tx, response_rx) = oneshot::channel();
            drop(response_rx);
            pending.insert(
                request.id.clone(),
                PendingRequest {
                    request,
                    app_owner_generation: None,
                    response_tx,
                    sensitive_tx: None,
                    timeout_handle: None,
                },
            );
        }
        let mut additional = sample_request();
        additional.id = "bounded-overflow".to_string();
        assert!(!pending_scope_accepts(&pending, &additional, None));
    }

    #[test]
    fn malformed_app_marker_never_survives_as_generic_history_body() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut request = app_owner_notification_request("owner-notify:malformed", now_ms + 60_000);
        request.created_at = now_ms;
        request.source = "executor".to_string();
        let response = UserResponse {
            request_id: request.id.clone(),
            decision: "acknowledge".to_string(),
            input: None,
            channel: "web".to_string(),
            sensitive: Vec::new(),
        };
        let mut record = UserRequestRecord::pending(request);
        record.mark_resolved(response, now_ms);
        let mut records = vec![record];

        trim_history_records(&mut records, 256);

        assert_eq!(records.len(), 1);
        assert!(is_app_owner_notification_replay_tombstone(
            &records[0].request
        ));
        assert!(records[0].request.question.is_empty());
        assert!(records[0].request.options.is_empty());
        assert!(records[0].response.is_none());
    }

    #[test]
    fn restored_expiry_reconstructs_requested_debt_before_resolution_debt() {
        let mut request = sample_request();
        request.id = "restored-expired-ordered".to_string();
        request.created_at = chrono::Utc::now().timestamp_millis() - 60_000;
        let response = UserResponse {
            request_id: request.id.clone(),
            decision: request.default_on_timeout.clone(),
            input: None,
            channel: "timeout".to_string(),
            sensitive: Vec::new(),
        };
        let mut records = Vec::new();

        upsert_pending_in_history(&mut records, &request);
        upsert_resolved_in_history(
            &mut records,
            &request,
            &response,
            chrono::Utc::now().timestamp_millis(),
        );

        assert!(generic_request_publication_is_pending(&records[0]));
        assert!(generic_resolution_publication_is_pending(&records[0]));
        assert!(generic_resolution_event(&records[0]).is_none());
    }

    #[test]
    fn in_memory_service_does_not_acquire_a_store_writer_lease() {
        let temp = tempdir().expect("tempdir");
        let service = UserRequestService::new(make_broadcaster())
            .with_workspace_layout(ArtifactV2Workspace::new(temp.path()));

        assert!(service.store_writer_lease_target.is_none());
        assert!(service.store_writer_lease.is_none());
        assert!(service
            .legacy_persistence_recovery_healthy
            .load(Ordering::SeqCst));
    }

    #[test]
    fn late_workspace_is_fenced_without_rebinding_loaded_store() {
        let legacy_root = tempdir().expect("legacy tempdir");
        let history_path = legacy_root.path().join("user-request-history.json");
        let request = recovered_request("lease-builder-order", "principal-a", "workspace-a");
        std::fs::write(
            &history_path,
            serde_json::to_vec(&vec![UserRequestRecord::pending(request)]).expect("history json"),
        )
        .expect("seed history");

        let service = UserRequestService::new(make_broadcaster())
            .with_history_persist_path(history_path)
            .with_workspace_layout(ArtifactV2Workspace::new(legacy_root.path()));

        assert!(service.workspace_layout.is_none());
        assert!(service.store_writer_lease.is_some());
        assert!(!service
            .legacy_persistence_recovery_healthy
            .load(Ordering::SeqCst));
        assert_eq!(
            service
                .list_history_for_scope("principal-a", "workspace-a", Some(1))
                .len(),
            1
        );
    }

    #[test]
    fn repeated_history_builder_is_fenced_without_reloading_or_rebinding() {
        let temp = tempdir().expect("tempdir");
        let first_path = temp.path().join("user-request-history-a.json");
        let second_path = temp.path().join("user-request-history-b.json");
        let first = recovered_request("history-first-owner", "principal-a", "workspace-a");
        let second = recovered_request("history-second-owner", "principal-a", "workspace-a");
        std::fs::write(
            &first_path,
            serde_json::to_vec(&vec![UserRequestRecord::pending(first)]).expect("first history"),
        )
        .expect("seed first history");
        std::fs::write(
            &second_path,
            serde_json::to_vec(&vec![UserRequestRecord::pending(second)]).expect("second history"),
        )
        .expect("seed second history");

        let service = UserRequestService::new(make_broadcaster())
            .with_history_persist_path(first_path.clone())
            .with_history_persist_path(second_path);

        assert_eq!(
            service.history_persist_path.as_deref(),
            Some(first_path.as_path())
        );
        let history = service.list_history_for_scope("principal-a", "workspace-a", Some(2));
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].request.id, "history-first-owner");
        assert!(!service
            .legacy_persistence_recovery_healthy
            .load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn repeated_pending_builder_is_fenced_before_second_load_or_spawn() {
        let temp = tempdir().expect("tempdir");
        let history_path = temp.path().join("user-request-history.json");
        let first_path = temp.path().join("user-request-pending-a.json");
        let second_path = temp.path().join("user-request-pending-b.json");
        std::fs::write(&history_path, b"[]").expect("seed history");
        std::fs::write(&first_path, b"[]").expect("seed first pending");
        let second_bytes = br#"[{"question":"must-not-load""#;
        std::fs::write(&second_path, second_bytes).expect("seed second pending");

        let service = UserRequestService::new(make_broadcaster())
            .with_history_persist_path(history_path)
            .with_pending_persist_path(first_path.clone())
            .await
            .with_pending_persist_path(second_path.clone())
            .await;

        assert_eq!(
            service.pending_persist_path.as_deref(),
            Some(first_path.as_path())
        );
        assert!(service.list_pending().await.is_empty());
        assert_eq!(
            std::fs::read(second_path).expect("second pending remains"),
            &second_bytes[..]
        );
        assert!(!service
            .legacy_persistence_recovery_healthy
            .load(Ordering::SeqCst));
        assert!(matches!(
            service.submit_nonblocking_durable(sample_request()).await,
            Err(UserRequestSubmissionError::PersistenceUnavailable)
        ));
    }

    #[tokio::test]
    async fn pending_before_history_is_fenced_before_pending_load() {
        let temp = tempdir().expect("tempdir");
        let pending_path = temp.path().join("user-request-pending.json");
        let bytes = br#"[{"question":"must-not-load""#;
        std::fs::write(&pending_path, bytes).expect("seed pending owner");

        let service = UserRequestService::new(make_broadcaster())
            .with_pending_persist_path(pending_path.clone())
            .await;

        assert!(service.store_writer_lease.is_some());
        assert!(service.list_pending().await.is_empty());
        assert!(!service
            .legacy_persistence_recovery_healthy
            .load(Ordering::SeqCst));
        assert_eq!(
            std::fs::read(&pending_path).expect("pending owner remains"),
            &bytes[..]
        );
        assert!(matches!(
            service.submit_nonblocking_durable(sample_request()).await,
            Err(UserRequestSubmissionError::PersistenceUnavailable)
        ));
    }

    #[tokio::test]
    async fn upgraded_writer_is_excluded_while_detached_timeout_retains_store() {
        let temp = tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temp.path());
        let history_path = temp.path().join("user-request-history.json");
        let pending_path = temp.path().join("user-request-pending.json");
        let first = UserRequestService::new(make_broadcaster())
            .with_workspace_layout(layout.clone())
            .with_history_persist_path(history_path.clone())
            .with_pending_persist_path(pending_path.clone())
            .await;
        let mut request = sample_request();
        request.id = "detached-timeout-retains-writer".to_string();
        request.timeout_secs = 600;
        assert!(matches!(
            first.submit_nonblocking_durable(request).await,
            Ok(UserRequestSubmission::Accepted { .. })
        ));

        // Dropping the facade does not end its detached timeout owner. That
        // task can still resolve and rewrite both shards, so it must retain the
        // same reference-counted file-lock owner and exclude a replacement
        // service in this process as well as a competing process.
        drop(first);
        let contender = UserRequestService::new(make_broadcaster())
            .with_workspace_layout(layout)
            .with_history_persist_path(history_path)
            .with_pending_persist_path(pending_path)
            .await;

        assert!(contender.store_writer_lease.is_none());
        assert!(!contender
            .legacy_persistence_recovery_healthy
            .load(Ordering::SeqCst));
        assert!(contender.list_pending().await.is_empty());
        assert!(matches!(
            contender.submit_nonblocking_durable(sample_request()).await,
            Err(UserRequestSubmissionError::PersistenceUnavailable)
        ));
    }

    #[tokio::test]
    async fn corrupt_pending_shard_is_preserved_and_fences_durable_mutation() {
        let temp = tempdir().expect("tempdir");
        let history_path = temp.path().join("user-request-history.json");
        let pending_path = temp.path().join("user-request-pending.json");
        std::fs::write(&pending_path, br#"[{"question":"private app body""#)
            .expect("seed corrupt pending shard");

        let service = UserRequestService::new(make_broadcaster())
            .with_history_persist_path(history_path)
            .with_pending_persist_path(pending_path.clone())
            .await;

        assert!(service.list_pending().await.is_empty());
        assert!(!service
            .legacy_persistence_recovery_healthy
            .load(Ordering::SeqCst));
        assert_eq!(
            std::fs::read(&pending_path).expect("preserved pending shard"),
            br#"[{"question":"private app body""#,
        );
        assert!(matches!(
            service.submit_nonblocking_durable(sample_request()).await,
            Err(UserRequestSubmissionError::PersistenceUnavailable)
        ));
    }

    #[tokio::test]
    async fn corrupt_scoped_pending_shard_quarantines_only_its_physical_owner() {
        let temp = tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temp.path());
        let history_path = temp.path().join("user-request-history.json");
        let pending_path = temp.path().join("user-request-pending.json");
        let file_name = request_shard_file_name(&pending_path);
        let corrupt_path = layout.scope_requests_path("principal-a", "workspace-a", &file_name);
        let healthy_path = layout.scope_requests_path("principal-b", "workspace-b", &file_name);
        layout
            .write_atomic_path_sync(&corrupt_path, br#"[{"question":"truncated""#)
            .expect("seed corrupt scope shard");
        let mut healthy_request = sample_request();
        healthy_request.id = "healthy-restored".to_owned();
        healthy_request.principal = "principal-b".to_owned();
        healthy_request.workspace = "workspace-b".to_owned();
        healthy_request.created_at = chrono::Utc::now().timestamp_millis();
        layout
            .write_atomic_path_sync(
                &healthy_path,
                &serde_json::to_vec(&vec![PersistedPendingRequest {
                    request: healthy_request,
                    app_owner_generation: None,
                }])
                .expect("serialize healthy scope"),
            )
            .expect("seed healthy scope shard");

        let service = UserRequestService::new(make_broadcaster())
            .with_workspace_layout(layout)
            .with_history_persist_path(history_path)
            .with_pending_persist_path(pending_path)
            .await;
        let mut quarantined = sample_request();
        quarantined.id = "new-quarantined".to_owned();
        quarantined.principal = "principal-a".to_owned();
        quarantined.workspace = "workspace-a".to_owned();
        assert!(matches!(
            service.submit_nonblocking_durable(quarantined).await,
            Err(UserRequestSubmissionError::PersistenceUnavailable)
        ));
        let mut independent = sample_request();
        independent.id = "new-independent".to_owned();
        independent.principal = "principal-b".to_owned();
        independent.workspace = "workspace-b".to_owned();
        assert!(matches!(
            service.submit_nonblocking_durable(independent).await,
            Ok(UserRequestSubmission::Accepted { .. })
        ));
    }

    #[tokio::test]
    async fn cross_owner_same_id_mismatch_preserves_and_quarantines_both_scopes() {
        let temp = tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temp.path());
        let history_path = temp.path().join("user-request-history.json");
        let pending_path = temp.path().join("user-request-pending.json");
        let history_shard = layout.scope_requests_path(
            "principal-b",
            "workspace-b",
            &request_shard_file_name(&history_path),
        );
        let pending_shard = layout.scope_requests_path(
            "principal-a",
            "workspace-a",
            &request_shard_file_name(&pending_path),
        );
        let history_request =
            recovered_request("cross-owner-conflict", "principal-b", "workspace-b");
        let pending_request =
            recovered_request("cross-owner-conflict", "principal-a", "workspace-a");
        let history_bytes = serde_json::to_vec(&vec![UserRequestRecord::pending(history_request)])
            .expect("serialize history owner");
        let pending_bytes = serde_json::to_vec(&vec![PersistedPendingRequest {
            request: pending_request,
            app_owner_generation: None,
        }])
        .expect("serialize pending owner");
        layout
            .write_atomic_path_sync(&history_shard, &history_bytes)
            .expect("seed history owner");
        layout
            .write_atomic_path_sync(&pending_shard, &pending_bytes)
            .expect("seed pending owner");

        let service = UserRequestService::new(make_broadcaster())
            .with_workspace_layout(layout)
            .with_history_persist_path(history_path)
            .with_pending_persist_path(pending_path)
            .await;

        let unhealthy = service
            .persistence_recovery_unhealthy_scopes
            .lock()
            .expect("recovery mutex");
        assert!(unhealthy.contains(&("principal-a".to_owned(), "workspace-a".to_owned(),)));
        assert!(unhealthy.contains(&("principal-b".to_owned(), "workspace-b".to_owned(),)));
        drop(unhealthy);
        assert_eq!(
            std::fs::read(&history_shard).expect("history owner remains"),
            history_bytes,
        );
        assert_eq!(
            std::fs::read(&pending_shard).expect("pending owner remains"),
            pending_bytes,
        );
    }

    #[tokio::test]
    async fn content_free_app_marker_same_id_collision_uses_ordinary_quarantine() {
        let temp = tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temp.path());
        let history_path = temp.path().join("user-request-history.json");
        let pending_path = temp.path().join("user-request-pending.json");
        let history_shard = layout.scope_requests_path(
            "principal-b",
            "workspace-b",
            &request_shard_file_name(&history_path),
        );
        let pending_shard = layout.scope_requests_path(
            "principal-a",
            "workspace-a",
            &request_shard_file_name(&pending_path),
        );
        let history_request =
            recovered_request("marker-owner-conflict", "principal-b", "workspace-b");
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut app_request =
            app_owner_notification_request("marker-owner-conflict", now_ms + 60_000);
        app_request.principal = "principal-a".to_owned();
        app_request.workspace = "workspace-a".to_owned();
        let response = UserResponse {
            request_id: app_request.id.clone(),
            decision: "acknowledge".to_owned(),
            input: None,
            channel: "web".to_owned(),
            sensitive: Vec::new(),
        };
        let marker = redact_app_owner_notification_request(&app_request, &response, now_ms);
        let history_bytes = serde_json::to_vec(&vec![UserRequestRecord::pending(history_request)])
            .expect("serialize foreign history owner");
        let pending_bytes = serde_json::to_vec(&vec![PersistedPendingRequest {
            request: marker,
            app_owner_generation: Some(new_app_owner_notification_generation()),
        }])
        .expect("serialize content-free pending marker");
        layout
            .write_atomic_path_sync(&history_shard, &history_bytes)
            .expect("seed foreign history owner");
        layout
            .write_atomic_path_sync(&pending_shard, &pending_bytes)
            .expect("seed content-free pending marker");

        let service = UserRequestService::new(make_broadcaster())
            .with_workspace_layout(layout)
            .with_history_persist_path(history_path)
            .with_pending_persist_path(pending_path)
            .await;

        let unhealthy = service
            .persistence_recovery_unhealthy_scopes
            .lock()
            .expect("recovery mutex");
        assert!(unhealthy.contains(&("principal-a".to_owned(), "workspace-a".to_owned(),)));
        assert!(unhealthy.contains(&("principal-b".to_owned(), "workspace-b".to_owned(),)));
        drop(unhealthy);
        assert_eq!(
            std::fs::read(history_shard).expect("history preserved"),
            history_bytes
        );
        assert_eq!(
            std::fs::read(pending_shard).expect("marker preserved"),
            pending_bytes
        );
    }

    #[tokio::test]
    async fn normalized_cross_store_scope_alias_is_quarantined_without_rewrite() {
        let temp = tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temp.path());
        let history_path = temp.path().join("user-request-history.json");
        let pending_path = temp.path().join("user-request-pending.json");
        let first_scope = ("principal:one", "workspace");
        let alias_scope = ("principal_one", "workspace");
        let storage_scope = ArtifactV2Workspace::scope_dir_segments(first_scope.0, first_scope.1);
        assert_eq!(
            storage_scope,
            ArtifactV2Workspace::scope_dir_segments(alias_scope.0, alias_scope.1),
            "test scopes must collide at the Artifact V2 path boundary",
        );
        let history_shard = layout.scope_requests_path(
            first_scope.0,
            first_scope.1,
            &request_shard_file_name(&history_path),
        );
        let pending_shard = layout.scope_requests_path(
            alias_scope.0,
            alias_scope.1,
            &request_shard_file_name(&pending_path),
        );
        let history_bytes = serde_json::to_vec(&vec![UserRequestRecord::pending(
            recovered_request("history-alias-owner", first_scope.0, first_scope.1),
        )])
        .expect("serialize history alias owner");
        let pending_bytes = serde_json::to_vec(&vec![PersistedPendingRequest {
            request: recovered_request("pending-alias-owner", alias_scope.0, alias_scope.1),
            app_owner_generation: None,
        }])
        .expect("serialize pending alias owner");
        layout
            .write_atomic_path_sync(&history_shard, &history_bytes)
            .expect("seed history alias owner");
        layout
            .write_atomic_path_sync(&pending_shard, &pending_bytes)
            .expect("seed pending alias owner");

        let service = UserRequestService::new(make_broadcaster())
            .with_workspace_layout(layout)
            .with_history_persist_path(history_path)
            .with_pending_persist_path(pending_path)
            .await;

        assert!(service.list_pending().await.is_empty());
        assert!(service
            .list_history_for_scope(first_scope.0, first_scope.1, None)
            .is_empty());
        assert!(service
            .persistence_recovery_unhealthy_scopes
            .lock()
            .expect("recovery mutex")
            .contains(&storage_scope));
        assert_eq!(
            std::fs::read(&history_shard).expect("history alias owner remains"),
            history_bytes,
        );
        assert_eq!(
            std::fs::read(&pending_shard).expect("pending alias owner remains"),
            pending_bytes,
        );
    }

    #[test]
    fn scope_indexes_track_replace_remove_and_physical_aliases() {
        let mut pending = PendingRequests::default();
        for (id, principal) in [("first", "principal:one"), ("second", "principal-two")] {
            let request = recovered_request(id, principal, "workspace");
            let (response_tx, response_rx) = oneshot::channel();
            drop(response_rx);
            pending.insert(
                id.to_owned(),
                PendingRequest {
                    request,
                    app_owner_generation: None,
                    response_tx,
                    sensitive_tx: None,
                    timeout_handle: None,
                },
            );
        }
        assert_eq!(
            pending.scope_values("principal:one", "workspace").count(),
            1
        );
        assert!(pending.has_storage_alias("principal_one", "workspace"));
        pending.remove("first");
        assert!(!pending.has_storage_alias("principal_one", "workspace"));

        let mut history = HistoryRecords::from_records(vec![UserRequestRecord::pending(
            recovered_request("history-first", "principal:one", "workspace"),
        )]);
        assert!(history.has_storage_alias("principal_one", "workspace"));
        assert!(history.record_by_id("history-first").is_some());
        history.replace_scope("principal:one", "workspace", Vec::new());
        assert!(history.record_by_id("history-first").is_none());
        assert!(!history.has_storage_alias("principal_one", "workspace"));
    }

    #[tokio::test]
    async fn failed_startup_scope_eviction_aborts_timeout_and_preserves_neighbor() {
        let temp = tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temp.path());
        let failed_request = recovered_request("failed-timeout", "principal-a", "workspace-a");
        let healthy_request = recovered_request("healthy-live", "principal-b", "workspace-b");
        let timeout_fired = Arc::new(AtomicBool::new(false));
        let timeout_fired_clone = Arc::clone(&timeout_fired);
        let timeout_handle = tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
            timeout_fired_clone.store(true, Ordering::SeqCst);
        });
        let mut pending = PendingRequests::default();
        for (request, handle) in [
            (failed_request.clone(), Some(timeout_handle)),
            (healthy_request.clone(), None),
        ] {
            let (response_tx, response_rx) = oneshot::channel();
            drop(response_rx);
            pending.insert(
                request.id.clone(),
                PendingRequest {
                    request,
                    app_owner_generation: None,
                    response_tx,
                    sensitive_tx: None,
                    timeout_handle: handle,
                },
            );
        }
        let mut history = HistoryRecords::from_records(vec![
            UserRequestRecord::pending(failed_request),
            UserRequestRecord::pending(healthy_request),
        ]);
        let failed = HashSet::from([request_storage_scope(
            Some(&layout),
            "principal-a",
            "workspace-a",
        )]);

        remove_failed_startup_scope_projections(&layout, &failed, &mut history, &mut pending);

        assert!(pending.get("failed-timeout").is_none());
        assert!(history.record_by_id("failed-timeout").is_none());
        assert!(pending.get("healthy-live").is_some());
        assert!(history.record_by_id("healthy-live").is_some());
        tokio::time::sleep(tokio::time::Duration::from_millis(40)).await;
        assert!(!timeout_fired.load(Ordering::SeqCst));
    }

    #[test]
    fn persisted_duplicate_ids_dedupe_exact_rows_and_quarantine_conflicts() {
        let temp = tempdir().expect("tempdir");
        let history_exact_path = temp.path().join("history-exact.json");
        let history_conflict_path = temp.path().join("history-conflict.json");
        let pending_exact_path = temp.path().join("pending-exact.json");
        let pending_conflict_path = temp.path().join("pending-conflict.json");
        let request = recovered_request("duplicate-id", "principal-a", "workspace-a");
        let exact_history = UserRequestRecord::pending(request.clone());
        std::fs::write(
            &history_exact_path,
            serde_json::to_vec(&vec![exact_history.clone(), exact_history.clone()])
                .expect("exact history duplicates"),
        )
        .expect("seed exact history duplicates");
        let mut conflicting_history = exact_history.clone();
        conflicting_history.request.question = "different history owner".to_owned();
        std::fs::write(
            &history_conflict_path,
            serde_json::to_vec(&vec![exact_history, conflicting_history])
                .expect("conflicting history duplicates"),
        )
        .expect("seed conflicting history duplicates");
        let exact_pending = PersistedPendingRequest {
            request: request.clone(),
            app_owner_generation: None,
        };
        std::fs::write(
            &pending_exact_path,
            serde_json::to_vec(&vec![exact_pending.clone(), exact_pending.clone()])
                .expect("exact pending duplicates"),
        )
        .expect("seed exact pending duplicates");
        let mut conflicting_pending = exact_pending.clone();
        conflicting_pending.request.question = "different pending owner".to_owned();
        std::fs::write(
            &pending_conflict_path,
            serde_json::to_vec(&vec![exact_pending, conflicting_pending])
                .expect("conflicting pending duplicates"),
        )
        .expect("seed conflicting pending duplicates");

        let exact_history = load_user_request_history(None, &history_exact_path, 256);
        let conflicting_history = load_user_request_history(None, &history_conflict_path, 256);
        let exact_pending = load_pending_requests(None, &pending_exact_path);
        let conflicting_pending = load_pending_requests(None, &pending_conflict_path);

        assert_eq!(exact_history.entries.len(), 1);
        assert!(exact_history.unhealthy_scopes.is_empty());
        assert!(conflicting_history.entries.is_empty());
        assert!(conflicting_history
            .unhealthy_scopes
            .contains(&("principal-a".to_owned(), "workspace-a".to_owned())));
        assert_eq!(exact_pending.entries.len(), 1);
        assert!(exact_pending.unhealthy_scopes.is_empty());
        assert!(conflicting_pending.entries.is_empty());
        assert!(conflicting_pending
            .unhealthy_scopes
            .contains(&("principal-a".to_owned(), "workspace-a".to_owned())));
    }

    #[test]
    fn same_store_physical_aliases_quarantine_history_and_pending_without_rewrite() {
        let temp = tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temp.path());
        let history_path = temp.path().join("user-request-history.json");
        let pending_path = temp.path().join("user-request-pending.json");
        let storage_scope = ArtifactV2Workspace::scope_dir_segments("principal:one", "workspace");
        assert_eq!(
            storage_scope,
            ArtifactV2Workspace::scope_dir_segments("principal_one", "workspace")
        );
        let history_rows = vec![
            UserRequestRecord::pending(recovered_request(
                "history-first",
                "principal:one",
                "workspace",
            )),
            UserRequestRecord::pending(recovered_request(
                "history-alias",
                "principal_one",
                "workspace",
            )),
        ];
        let pending_rows = vec![
            PersistedPendingRequest {
                request: recovered_request("pending-first", "principal:one", "workspace"),
                app_owner_generation: None,
            },
            PersistedPendingRequest {
                request: recovered_request("pending-alias", "principal_one", "workspace"),
                app_owner_generation: None,
            },
        ];
        let history_bytes = serde_json::to_vec(&history_rows).expect("history aliases");
        let pending_bytes = serde_json::to_vec(&pending_rows).expect("pending aliases");
        let history_shard = layout.scope_requests_path(
            &storage_scope.0,
            &storage_scope.1,
            &request_shard_file_name(&history_path),
        );
        let pending_shard = layout.scope_requests_path(
            &storage_scope.0,
            &storage_scope.1,
            &request_shard_file_name(&pending_path),
        );
        layout
            .write_atomic_path_sync(&history_shard, &history_bytes)
            .expect("seed aliased history");
        layout
            .write_atomic_path_sync(&pending_shard, &pending_bytes)
            .expect("seed aliased pending");

        let history = load_user_request_history(Some(&layout), &history_path, 256);
        let pending = load_pending_requests(Some(&layout), &pending_path);

        assert!(history.entries.is_empty());
        assert!(pending.entries.is_empty());
        assert!(history.unhealthy_scopes.contains(&storage_scope));
        assert!(pending.unhealthy_scopes.contains(&storage_scope));
        assert_eq!(
            std::fs::read(history_shard).expect("history preserved"),
            history_bytes
        );
        assert_eq!(
            std::fs::read(pending_shard).expect("pending preserved"),
            pending_bytes
        );
    }

    #[tokio::test]
    async fn live_admission_rejects_a_second_logical_owner_of_one_physical_scope() {
        let temp = tempdir().expect("tempdir");
        let service = UserRequestService::new(make_broadcaster())
            .with_workspace_layout(ArtifactV2Workspace::new(temp.path()));
        let mut first = recovered_request("live-first", "principal:one", "workspace");
        first.created_at = 0;
        service
            .submit_nonblocking(first)
            .await
            .expect("first physical owner");
        let mut alias = recovered_request("live-alias", "principal_one", "workspace");
        alias.created_at = 0;

        assert!(matches!(
            service.submit_nonblocking(alias).await,
            Err(UserRequestSubmissionError::PersistenceUnavailable)
        ));

        let mut pending = service.pending.write().await;
        if let Some(mut entry) = pending.remove("live-first") {
            if let Some(handle) = entry.timeout_handle.take() {
                handle.abort();
            }
        }
    }

    #[test]
    fn provider_hot_scope_writes_ignore_unrelated_invalid_tenants() {
        let temp = tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temp.path());
        let history_path = temp.path().join("user-request-history.json");
        let pending_path = temp.path().join("user-request-pending.json");
        let target = recovered_request("target", "principal-a", "workspace-a");
        let mut unrelated = recovered_request("unrelated", "principal-b", "workspace-b");
        unrelated.question = "x".repeat(MAX_USER_REQUEST_RECORD_BYTES + 1);
        let foreign_pending_shard = layout.scope_requests_path(
            "principal-b",
            "workspace-b",
            &request_shard_file_name(&pending_path),
        );
        let foreign_history_shard = layout.scope_requests_path(
            "principal-b",
            "workspace-b",
            &request_shard_file_name(&history_path),
        );
        let pending_sentinel = b"unrelated pending sentinel";
        let history_sentinel = b"unrelated history sentinel";
        layout
            .write_atomic_path_sync(&foreign_pending_shard, pending_sentinel)
            .expect("seed foreign pending sentinel");
        layout
            .write_atomic_path_sync(&foreign_history_shard, history_sentinel)
            .expect("seed foreign history sentinel");
        let mut pending = PendingRequests::default();
        for request in [target.clone(), unrelated.clone()] {
            let (response_tx, response_rx) = oneshot::channel();
            drop(response_rx);
            pending.insert(
                request.id.clone(),
                PendingRequest {
                    request,
                    app_owner_generation: None,
                    response_tx,
                    sensitive_tx: None,
                    timeout_handle: None,
                },
            );
        }
        let history = HistoryRecords::from_records(vec![
            UserRequestRecord::pending(target.clone()),
            UserRequestRecord::pending(unrelated),
        ]);
        let target_history = history.records_for_scope("principal-a", "workspace-a");

        try_write_pending_scope_snapshot_sync(
            Some(&layout),
            Some(&pending_path),
            &pending,
            "principal-a",
            "workspace-a",
        )
        .expect("target pending scope write");
        try_persist_user_request_scope_candidate(
            Some(&layout),
            Some(&history_path),
            &history,
            &target_history,
            "principal-a",
            "workspace-a",
        )
        .expect("target history scope write");

        assert_eq!(
            std::fs::read(foreign_pending_shard)
                .expect("foreign pending untouched")
                .as_slice(),
            pending_sentinel
        );
        assert_eq!(
            std::fs::read(foreign_history_shard)
                .expect("foreign history untouched")
                .as_slice(),
            history_sentinel
        );
    }

    #[test]
    fn expired_app_cleanup_never_replaces_a_foreign_same_id_history_owner() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut foreign = recovered_request("shared-id", "principal-a", "workspace-a");
        foreign.question = "foreign generic history".to_owned();
        let foreign_record = UserRequestRecord::pending(foreign.clone());
        let mut history = HistoryRecords::from_records(vec![foreign_record.clone()]);
        let expired = app_owner_notification_request("shared-id", now_ms - 1);
        let response = UserResponse {
            request_id: expired.id.clone(),
            decision: "acknowledge".to_owned(),
            input: None,
            channel: "timeout".to_owned(),
            sensitive: Vec::new(),
        };

        install_app_notification_cleanup_history_markers(
            &mut history,
            &[(expired, response)],
            now_ms,
        );

        let live = app_owner_notification_request("shared-id", now_ms + 60_000);
        let live_response = UserResponse {
            request_id: live.id.clone(),
            decision: "acknowledge".to_owned(),
            input: None,
            channel: "web".to_owned(),
            sensitive: Vec::new(),
        };
        let marker = redact_app_owner_notification_request(&live, &live_response, now_ms);
        install_app_notification_cleanup_history_markers(
            &mut history,
            &[(marker, live_response)],
            now_ms,
        );

        assert_eq!(history.len(), 1);
        assert_eq!(history.record_by_id("shared-id"), Some(&foreign_record));

        let mut expected = app_owner_notification_request("shared-id", now_ms + 60_000);
        expected.question = "expected private body".to_owned();
        let expected_response = UserResponse {
            request_id: expected.id.clone(),
            decision: "acknowledge".to_owned(),
            input: None,
            channel: "web".to_owned(),
            sensitive: Vec::new(),
        };
        let mut foreign_app = expected.clone();
        foreign_app.question = "different private body".to_owned();
        let foreign_app_response = UserResponse {
            request_id: foreign_app.id.clone(),
            decision: "acknowledge".to_owned(),
            input: None,
            channel: "web".to_owned(),
            sensitive: Vec::new(),
        };
        let foreign_tombstone =
            redact_app_owner_notification_request(&foreign_app, &foreign_app_response, now_ms);
        let mut foreign_tombstone_record = UserRequestRecord::pending(foreign_tombstone);
        foreign_tombstone_record.mark_resolved(foreign_app_response, now_ms);
        assert!(!app_notification_cleanup_history_is_authoritative(
            &[foreign_tombstone_record],
            &[(expected, expected_response)],
            now_ms,
        ));
    }

    #[test]
    fn legacy_scope_candidate_write_preserves_unrelated_scope_records() {
        let temp = tempdir().expect("tempdir");
        let history_path = temp.path().join("user-request-history.json");
        let first = recovered_request("first", "principal-a", "workspace-a");
        let second = recovered_request("second", "principal-b", "workspace-b");
        let current = HistoryRecords::from_records(vec![
            UserRequestRecord::pending(first.clone()),
            UserRequestRecord::pending(second),
        ]);
        let response = UserResponse {
            request_id: first.id.clone(),
            decision: "deny".to_string(),
            input: None,
            channel: "web".to_string(),
            sensitive: Vec::new(),
        };
        let mut first_scope = vec![UserRequestRecord::pending(first)];
        first_scope[0].mark_resolved(response, chrono::Utc::now().timestamp_millis());

        try_persist_user_request_scope_candidate(
            None,
            Some(&history_path),
            &current,
            &first_scope,
            "principal-a",
            "workspace-a",
        )
        .expect("legacy candidate persistence");

        let stored = read_request_shard::<UserRequestRecord>(
            None,
            &history_path,
            MAX_USER_REQUEST_LEGACY_RECORDS,
        );
        assert!(stored.healthy);
        assert_eq!(stored.entries.len(), 2);
        assert!(stored.entries.iter().any(|record| {
            record.request.id == "first" && record.status == UserRequestStatus::Resolved
        }));
        assert!(stored.entries.iter().any(|record| {
            record.request.id == "second" && record.status == UserRequestStatus::Pending
        }));
    }

    fn recovered_request(id: &str, principal: &str, workspace: &str) -> UserRequest {
        let mut request = sample_request();
        request.id = id.to_string();
        request.principal = principal.to_string();
        request.workspace = workspace.to_string();
        request.created_at = chrono::Utc::now().timestamp_millis();
        request
    }

    #[test]
    fn history_limit_does_not_rewrite_any_shard_while_one_scope_is_quarantined() {
        let temp = tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temp.path());
        let history_path = temp.path().join("user-request-history.json");
        let file_name = request_shard_file_name(&history_path);
        let corrupt_path = layout.scope_requests_path("principal-a", "workspace-a", &file_name);
        let healthy_path = layout.scope_requests_path("principal-b", "workspace-b", &file_name);
        let corrupt_bytes = br#"[{"request":{"question":"truncated""#;
        layout
            .write_atomic_path_sync(&corrupt_path, corrupt_bytes)
            .expect("seed corrupt history scope");
        let healthy_records = vec![
            UserRequestRecord::pending(recovered_request(
                "healthy-first",
                "principal-b",
                "workspace-b",
            )),
            UserRequestRecord::pending(recovered_request(
                "healthy-second",
                "principal-b",
                "workspace-b",
            )),
        ];
        let healthy_bytes = serde_json::to_vec(&healthy_records).expect("serialize healthy scope");
        layout
            .write_atomic_path_sync(&healthy_path, &healthy_bytes)
            .expect("seed healthy history scope");

        let service = UserRequestService::new(make_broadcaster())
            .with_workspace_layout(layout)
            .with_history_persist_path(history_path)
            .with_history_limit(1);

        assert_eq!(service.history.lock().expect("history mutex").len(), 1);
        assert_eq!(
            std::fs::read(&corrupt_path).expect("corrupt authority remains"),
            corrupt_bytes,
        );
        assert_eq!(
            std::fs::read(&healthy_path).expect("healthy authority remains"),
            healthy_bytes,
        );
    }

    fn seed_pending_history(path: &Path, request: UserRequest) {
        std::fs::write(
            path,
            serde_json::to_vec(&vec![UserRequestRecord::pending(request)]).unwrap(),
        )
        .unwrap();
    }

    /// `load_user_request_history` reads whatever shard files it finds, so a
    /// staging sibling left beside the store is a second file the reader can
    /// trip over. Both persist paths must publish in one step.
    #[test]
    fn persisting_history_leaves_no_staging_sibling_and_the_store_parses() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("requests").join("user_requests.json");

        let first = vec![UserRequestRecord::pending(recovered_request(
            "req-1", "default", "default",
        ))];
        persist_user_request_history(None, Some(&path), &first);
        let second = vec![
            UserRequestRecord::pending(recovered_request("req-1", "default", "default")),
            UserRequestRecord::pending(recovered_request("req-2", "default", "default")),
        ];
        persist_user_request_history(None, Some(&path), &second);

        let stored: Vec<UserRequestRecord> =
            serde_json::from_slice(&std::fs::read(&path).expect("store readable"))
                .expect("published store parses");
        assert_eq!(stored.len(), 2);

        let staging_left = std::fs::read_dir(path.parent().expect("requests dir"))
            .expect("requests listing")
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"));
        assert!(
            !staging_left,
            "history publish must leave no staging sibling beside the store"
        );
    }

    /// Async twin of the check above — the pending store is written from the
    /// async resolve path.
    #[tokio::test]
    async fn async_persisted_write_leaves_no_staging_sibling() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("requests").join("pending_requests.json");

        write_persisted_bytes_atomic(None, &path, br#"[{"a":1}]"#)
            .await
            .expect("first write");
        write_persisted_bytes_atomic(None, &path, br#"[{"a":1},{"b":2}]"#)
            .await
            .expect("rewrite");

        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("store readable"))
                .expect("published store parses");
        assert_eq!(stored.as_array().expect("array").len(), 2);

        let staging_left = std::fs::read_dir(path.parent().expect("requests dir"))
            .expect("requests listing")
            .flatten()
            .any(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"));
        assert!(
            !staging_left,
            "async publish must leave no staging sibling beside the store"
        );
    }

    #[test]
    fn artifact_io_conversion_preserves_not_found_kind() {
        let error = crate::magician_v2::artifact_v2::ArtifactV2Error::Io(io::Error::from(
            io::ErrorKind::NotFound,
        ));
        assert_eq!(
            artifact_v2_error_to_io(error).kind(),
            io::ErrorKind::NotFound
        );
    }

    #[tokio::test]
    async fn ask_and_respond_returns_decision() {
        let broadcaster = make_broadcaster();
        let svc = Arc::new(UserRequestService::new(Arc::clone(&broadcaster)));
        let svc_clone = Arc::clone(&svc);

        // Spawn `ask` in the background
        let ask_handle = tokio::spawn(async move { svc_clone.ask(sample_request()).await });

        // Give the ask task a moment to register
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Grab the pending request id
        let pending = svc.list_pending().await;
        assert_eq!(pending.len(), 1);
        let req_id = pending[0].id.clone();

        // Respond
        let accepted = svc
            .respond(UserResponse {
                request_id: req_id.clone(),
                decision: "allow_once".to_string(),
                input: None,
                channel: "web".to_string(),
                sensitive: Vec::new(),
            })
            .await;
        assert!(accepted);

        let response = ask_handle.await.unwrap();
        assert_eq!(response.decision, "allow_once");
        assert_eq!(response.channel, "web");

        // Pending list should be empty now
        assert!(svc.list_pending().await.is_empty());
    }

    #[tokio::test]
    async fn nonblocking_submission_returns_after_durable_acceptance() {
        let temp = tempdir().expect("tempdir");
        let history_path = temp.path().join("user-request-history.json");
        let pending_path = temp.path().join("user-request-pending.json");
        let broadcaster = make_broadcaster();
        let mut receiver = broadcaster.subscribe();
        let svc = UserRequestService::new(Arc::clone(&broadcaster))
            .with_history_persist_path(history_path.clone())
            .with_pending_persist_path(pending_path.clone())
            .await;

        let receipt = svc
            .submit_nonblocking(sample_request())
            .await
            .expect("submission");
        let request_id = receipt.request_id().to_string();
        assert!(matches!(receipt, UserRequestSubmission::Accepted { .. }));

        let persisted_pending = load_pending_requests(None, &pending_path);
        assert!(persisted_pending.legacy_healthy);
        assert!(persisted_pending.unhealthy_scopes.is_empty());
        assert_eq!(persisted_pending.entries.len(), 1);
        assert_eq!(persisted_pending.entries[0].request.id, request_id);
        let persisted_history = read_request_shard::<UserRequestRecord>(
            None,
            &history_path,
            MAX_USER_REQUEST_LEGACY_RECORDS,
        );
        assert!(persisted_history.healthy);
        assert_eq!(persisted_history.entries.len(), 1);
        assert_eq!(persisted_history.entries[0].request.id, request_id);
        assert_eq!(
            persisted_history.entries[0].status,
            UserRequestStatus::Pending
        );

        let event = recv_until(&mut receiver, |event| {
            matches!(
                event,
                RuntimeTransportEvent::HitlRequested { correlation_id, .. }
                    if correlation_id == &request_id
            )
        })
        .await;
        assert!(matches!(event, RuntimeTransportEvent::HitlRequested { .. }));
    }

    #[tokio::test]
    async fn deterministic_nonblocking_submission_replays_exactly_and_rejects_substitution() {
        let broadcaster = make_broadcaster();
        let svc = UserRequestService::new(broadcaster);
        let mut request = sample_request();
        request.id = "owner-notify:stable-effect-1".to_string();

        let accepted = svc
            .submit_nonblocking(request.clone())
            .await
            .expect("first acceptance");
        assert_eq!(
            accepted,
            UserRequestSubmission::Accepted {
                request_id: request.id.clone(),
            }
        );

        let replay = svc
            .submit_nonblocking(request.clone())
            .await
            .expect("exact replay");
        assert_eq!(
            replay,
            UserRequestSubmission::IdempotentReplay {
                request_id: request.id.clone(),
            }
        );
        assert_eq!(svc.list_pending().await.len(), 1);

        let mut substitution = request.clone();
        substitution.question = "Different payload".to_string();
        assert_eq!(
            svc.submit_nonblocking(substitution).await,
            Err(UserRequestSubmissionError::IdempotencyConflict {
                request_id: request.id.clone(),
            })
        );
        assert_eq!(svc.list_pending().await.len(), 1);

        assert!(
            svc.respond(UserResponse {
                request_id: request.id.clone(),
                decision: "deny".to_string(),
                input: None,
                channel: "web".to_string(),
                sensitive: Vec::new(),
            })
            .await
        );
        assert_eq!(
            svc.submit_nonblocking(request.clone()).await,
            Ok(UserRequestSubmission::IdempotentReplay {
                request_id: request.id,
            })
        );
        assert!(svc.list_pending().await.is_empty());
    }

    #[tokio::test]
    async fn expired_app_notification_is_never_admitted() {
        let svc = UserRequestService::new(make_broadcaster());
        let request = app_owner_notification_request(
            "owner-notify:expired",
            chrono::Utc::now().timestamp_millis() - 60_000,
        );

        assert_eq!(
            svc.submit_nonblocking(request).await,
            Err(UserRequestSubmissionError::PersistenceUnavailable)
        );
        assert!(svc.list_pending().await.is_empty());
    }

    /// The app owner-notification outbox settles a delivery as *submitted*
    /// only when the receipt carries back the exact correlation id it handed
    /// in; any other accepted id is read there as a permanent failure. So a
    /// caller id rewritten here would silently dead-letter every notification
    /// an app ever accepts. The projection is also unrouted by construction —
    /// no task, no execution — which is what keeps it owned by this service
    /// alone rather than by whatever execution posture the app platform is in.
    #[tokio::test]
    async fn app_owner_notification_durable_admission_preserves_the_delivery_correlation_id() {
        let temp = tempdir().expect("tempdir");
        let layout = ArtifactV2Workspace::new(temp.path());
        let history_path = temp.path().join("user-request-history.json");
        let pending_path = temp.path().join("user-request-pending.json");
        let service = UserRequestService::new(make_broadcaster())
            .with_workspace_layout(layout)
            .with_history_persist_path(history_path)
            .with_pending_persist_path(pending_path)
            .await;

        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut request =
            app_owner_notification_request("owner-notify:always-owned", now_ms + 60_000);
        request.task_id = None;
        request.execution_id = None;

        assert_eq!(
            service.submit_nonblocking_durable(request.clone()).await,
            Ok(UserRequestSubmission::Accepted {
                request_id: request.id.clone(),
            })
        );

        let pending = service.list_pending().await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, request.id);
        assert_eq!(pending[0].task_id, None);
        assert_eq!(pending[0].execution_id, None);
    }

    #[test]
    fn resolved_app_notification_is_immediately_compacted_and_ttl_bounded() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let request = app_owner_notification_request("owner-notify:live", now_ms + 60_000);
        let response = UserResponse {
            request_id: request.id.clone(),
            decision: "acknowledge".to_string(),
            input: None,
            channel: "timeout".to_string(),
            sensitive: Vec::new(),
        };
        let mut record = UserRequestRecord::pending(request.clone());
        record.mark_resolved(response, now_ms);
        let mut records = vec![record];

        trim_history_records(&mut records, 256);

        assert_eq!(records.len(), 1);
        let tombstone = &records[0].request;
        assert!(is_app_owner_notification_replay_tombstone(tombstone));
        assert_eq!(tombstone.id, request.id);
        assert_eq!(tombstone.principal, request.principal);
        assert_eq!(tombstone.workspace, request.workspace);
        assert_eq!(tombstone.source, request.source);
        assert!(tombstone.question.is_empty());
        assert!(tombstone.options.is_empty());
        assert_eq!(
            tombstone
                .context
                .get("resolution_timestamp_ms")
                .and_then(serde_json::Value::as_i64),
            Some(now_ms)
        );
        assert!(records[0].response.is_none());
        assert!(matches!(
            replay_or_conflict(tombstone, &request),
            Ok(AcceptedUserRequest {
                receipt: UserRequestSubmission::IdempotentReplay { .. },
                ..
            })
        ));

        let mut substitution = request;
        substitution.question = "substituted body".to_string();
        assert!(matches!(
            replay_or_conflict(tombstone, &substitution),
            Err(UserRequestSubmissionError::IdempotencyConflict { .. })
        ));

        records[0]
            .request
            .context
            .as_object_mut()
            .expect("tombstone context")
            .insert(
                "absolute_expires_at_ms".to_string(),
                serde_json::json!(now_ms - 1),
            );
        trim_history_records(&mut records, 256);
        assert!(records.is_empty());

        let expired_request =
            app_owner_notification_request("owner-notify:expired-history", now_ms - 1);
        let expired_response = UserResponse {
            request_id: expired_request.id.clone(),
            decision: "acknowledge".to_string(),
            input: None,
            channel: "timeout".to_string(),
            sensitive: Vec::new(),
        };
        let mut expired_record = UserRequestRecord::pending(expired_request);
        expired_record.mark_resolved(expired_response, now_ms);
        let mut expired_records = vec![expired_record];
        trim_history_records(&mut expired_records, 256);
        assert!(expired_records.is_empty());
    }

    #[test]
    fn app_resolution_requires_both_cleanup_owners_and_keeps_only_redacted_debt() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let request = app_owner_notification_request("owner-notify:cleanup", now_ms + 60_000);
        let response = UserResponse {
            request_id: request.id.clone(),
            decision: "acknowledge".to_string(),
            input: None,
            channel: "web".to_string(),
            sensitive: Vec::new(),
        };
        let mut history =
            HistoryRecords::from_records(vec![UserRequestRecord::pending(request.clone())]);
        let pending = PendingRequests::default();

        let result = try_commit_resolved_request_with_history(
            &mut history,
            None,
            None,
            None,
            &pending,
            256,
            &request,
            Some("17f1e6e8-f9fb-4c84-89f5-d5193f482ce6"),
            &response,
            now_ms,
        );

        let ResolvedRequestCommit::RetryAppCleanup { redacted_request } = result else {
            panic!("app cleanup without either persistence owner must retry");
        };
        assert!(is_app_owner_notification_replay_tombstone(
            &redacted_request
        ));
        assert!(redacted_request.question.is_empty());
        assert!(redacted_request.options.is_empty());
        assert!(redacted_request.execution_id.is_none());
        assert!(redacted_request.task_id.is_none());
        assert!(history
            .iter()
            .all(|record| record.request.question.is_empty()));
        assert!(!request_is_visible_pending(&redacted_request, now_ms));
        assert_eq!(
            hitl_resolution_source(&redacted_request),
            "app_owner_notification"
        );
    }

    #[test]
    fn app_cleanup_batches_are_bounded_and_advance_past_failed_early_ids() {
        let mut queue = AppNotificationCleanupQueue::default();
        for index in 0..40 {
            queue.enqueue(format!("owner-notify:{index:03}"));
        }

        let RetryQueuePop::Batch(first) = queue.pop_batch() else {
            panic!("live cleanup members must produce a batch");
        };
        assert_eq!(first.len(), APP_OWNER_NOTIFICATION_CLEANUP_BATCH);
        for request_id in first {
            queue.enqueue(request_id);
        }
        let RetryQueuePop::Batch(second) = queue.pop_batch() else {
            panic!("rotated cleanup members must produce a batch");
        };
        assert_eq!(second.len(), APP_OWNER_NOTIFICATION_CLEANUP_BATCH);
        assert!(second
            .iter()
            .any(|request_id| request_id == "owner-notify:039"));
    }

    #[test]
    fn cleanup_queues_bound_stale_scans_without_stranding_live_debt() {
        let stale_count = USER_REQUEST_RETRY_QUEUE_SCAN_LIMIT + 1;

        let mut app_queue = AppNotificationCleanupQueue::default();
        for index in 0..stale_count {
            let request_id = format!("stale-app:{index:03}");
            app_queue.enqueue(request_id.clone());
            app_queue.complete(&request_id);
        }
        app_queue.enqueue("live-app".to_string());
        assert_eq!(app_queue.pop_batch(), RetryQueuePop::Pending);
        assert_eq!(
            app_queue.pop_batch(),
            RetryQueuePop::Batch(vec!["live-app".to_string()])
        );
        app_queue.complete("live-app");
        assert_eq!(app_queue.pop_batch(), RetryQueuePop::Empty);

        let mut generic_queue = GenericResolutionPublicationQueue::default();
        for index in 0..stale_count {
            let request_id = format!("stale-generic:{index:03}");
            generic_queue.enqueue(request_id.clone());
            generic_queue.complete(&request_id);
        }
        generic_queue.enqueue("live-generic".to_string());
        assert_eq!(generic_queue.pop_batch(), RetryQueuePop::Pending);
        assert_eq!(
            generic_queue.pop_batch(),
            RetryQueuePop::Batch(vec!["live-generic".to_string()])
        );
        generic_queue.complete("live-generic");
        assert_eq!(generic_queue.pop_batch(), RetryQueuePop::Empty);
    }

    #[test]
    fn generic_resolution_keeps_legacy_either_authority_semantics() {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut request = sample_request();
        request.id = "generic-resolution".to_string();
        request.created_at = now_ms;
        let response = UserResponse {
            request_id: request.id.clone(),
            decision: "deny".to_string(),
            input: None,
            channel: "web".to_string(),
            sensitive: Vec::new(),
        };
        let mut history =
            HistoryRecords::from_records(vec![UserRequestRecord::pending(request.clone())]);

        assert!(matches!(
            try_commit_resolved_request_with_history(
                &mut history,
                None,
                None,
                None,
                &PendingRequests::default(),
                256,
                &request,
                None,
                &response,
                now_ms,
            ),
            ResolvedRequestCommit::Committed
        ));
    }

    #[tokio::test]
    async fn second_response_is_rejected() {
        let broadcaster = make_broadcaster();
        let svc = Arc::new(UserRequestService::new(Arc::clone(&broadcaster)));
        let svc_clone = Arc::clone(&svc);

        let ask_handle = tokio::spawn(async move { svc_clone.ask(sample_request()).await });
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let pending = svc.list_pending().await;
        let req_id = pending[0].id.clone();

        // First response wins
        assert!(
            svc.respond(UserResponse {
                request_id: req_id.clone(),
                decision: "allow_once".to_string(),
                input: None,
                channel: "web".to_string(),
                sensitive: Vec::new(),
            })
            .await
        );

        // Second response is rejected
        assert!(
            !svc.respond(UserResponse {
                request_id: req_id,
                decision: "deny".to_string(),
                input: None,
                channel: "telegram".to_string(),
                sensitive: Vec::new(),
            })
            .await
        );

        let response = ask_handle.await.unwrap();
        assert_eq!(response.decision, "allow_once");
    }

    #[tokio::test]
    async fn timeout_fires_default_response() {
        let broadcaster = make_broadcaster();
        let svc = Arc::new(UserRequestService::new(Arc::clone(&broadcaster)));

        let mut req = sample_request();
        req.timeout_secs = 1; // short timeout for test

        let response = svc.ask(req).await;
        assert_eq!(response.decision, "deny"); // default_on_timeout
        assert_eq!(response.channel, "timeout");
    }

    #[tokio::test]
    async fn scoped_response_rejects_scope_mismatch() {
        let broadcaster = make_broadcaster();
        let svc = Arc::new(UserRequestService::new(Arc::clone(&broadcaster)));
        let svc_clone = Arc::clone(&svc);

        let ask_handle = tokio::spawn(async move { svc_clone.ask(sample_request()).await });
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let pending = svc.list_pending().await;
        let req_id = pending[0].id.clone();

        let mismatch = svc
            .respond_scoped(
                UserResponse {
                    request_id: req_id.clone(),
                    decision: "allow_once".to_string(),
                    input: None,
                    channel: "web".to_string(),
                    sensitive: Vec::new(),
                },
                Some("other-user"),
                Some("default"),
            )
            .await;
        assert_eq!(mismatch, ScopedResponseResult::ScopeMismatch);
        assert_eq!(svc.list_pending().await.len(), 1);

        let accepted = svc
            .respond_scoped(
                UserResponse {
                    request_id: req_id,
                    decision: "deny".to_string(),
                    input: None,
                    channel: "web".to_string(),
                    sensitive: Vec::new(),
                },
                Some("default"),
                Some("default"),
            )
            .await;
        assert_eq!(accepted, ScopedResponseResult::Accepted);

        let response = ask_handle.await.unwrap();
        assert_eq!(response.decision, "deny");
    }

    #[tokio::test]
    async fn recovered_id_rejects_cross_scope_responses_without_emitting() {
        let broadcaster = make_broadcaster();
        let mut receiver = broadcaster.subscribe();
        let dir = tempdir().expect("tempdir");
        let history_path = dir.path().join("user_request_history.json");
        seed_pending_history(
            &history_path,
            recovered_request("recovered-cross-scope", "principal-a", "workspace-a"),
        );
        let svc = UserRequestService::new(Arc::clone(&broadcaster))
            .with_history_persist_path(history_path);

        for (principal, workspace) in [
            ("principal-b", "workspace-a"),
            ("principal-a", "workspace-b"),
        ] {
            let result = svc
                .respond_scoped(
                    UserResponse {
                        request_id: "recovered-cross-scope".to_string(),
                        decision: "allow_once".to_string(),
                        input: None,
                        channel: "web".to_string(),
                        sensitive: Vec::new(),
                    },
                    Some(principal),
                    Some(workspace),
                )
                .await;
            assert_eq!(result, ScopedResponseResult::ScopeMismatch);
        }

        assert!(svc
            .list_history_for_scope("principal-b", "workspace-a", None)
            .is_empty());
        assert!(svc
            .list_history_for_scope("principal-a", "workspace-b", None)
            .is_empty());
        let original_history = svc.list_history_for_scope("principal-a", "workspace-a", Some(1));
        assert_eq!(original_history.len(), 1);
        assert_eq!(
            original_history[0]
                .response
                .as_ref()
                .map(|response| response.channel.as_str()),
            Some("service_restart")
        );
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn unknown_scoped_id_is_rejected_without_history_or_event() {
        let broadcaster = make_broadcaster();
        let mut receiver = broadcaster.subscribe();
        let svc = UserRequestService::new(Arc::clone(&broadcaster));

        let result = svc
            .respond_scoped(
                UserResponse {
                    request_id: "unknown-scoped".to_string(),
                    decision: "allow_once".to_string(),
                    input: None,
                    channel: "web".to_string(),
                    sensitive: Vec::new(),
                },
                Some("principal-a"),
                Some("workspace-a"),
            )
            .await;

        assert_eq!(result, ScopedResponseResult::ScopeMismatch);
        assert!(svc
            .list_history_for_scope("principal-a", "workspace-a", None)
            .is_empty());
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn same_scope_restart_recovery_resolves_authoritative_history() {
        let broadcaster = make_broadcaster();
        let mut receiver = broadcaster.subscribe();
        let dir = tempdir().expect("tempdir");
        let history_path = dir.path().join("user_request_history.json");
        seed_pending_history(
            &history_path,
            recovered_request("recovered-same-scope", "principal-a", "workspace-a"),
        );
        let svc = UserRequestService::new(Arc::clone(&broadcaster))
            .with_history_persist_path(history_path);

        let result = svc
            .respond_scoped(
                UserResponse {
                    request_id: "recovered-same-scope".to_string(),
                    decision: "allow_once".to_string(),
                    input: None,
                    channel: "web".to_string(),
                    sensitive: Vec::new(),
                },
                Some("principal-a"),
                Some("workspace-a"),
            )
            .await;

        assert_eq!(result, ScopedResponseResult::Accepted);
        let history = svc.list_history_for_scope("principal-a", "workspace-a", Some(1));
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].status, UserRequestStatus::Resolved);
        assert_eq!(
            history[0]
                .response
                .as_ref()
                .map(|response| response.decision.as_str()),
            Some("allow_once")
        );
        let resolved = recv_until(&mut receiver, |event| {
            matches!(event, RuntimeTransportEvent::HitlResolved { .. })
        })
        .await;
        match resolved {
            RuntimeTransportEvent::HitlResolved {
                correlation_id,
                principal,
                workspace,
                ..
            } => {
                assert_eq!(correlation_id, "recovered-same-scope");
                assert_eq!(principal.as_deref(), Some("principal-a"));
                assert_eq!(workspace.as_deref(), Some("workspace-a"));
            },
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn recovered_id_preserves_first_response_wins() {
        let broadcaster = make_broadcaster();
        let mut receiver = broadcaster.subscribe();
        let dir = tempdir().expect("tempdir");
        let history_path = dir.path().join("user_request_history.json");
        seed_pending_history(
            &history_path,
            recovered_request("recovered-first-wins", "principal-a", "workspace-a"),
        );
        let svc = UserRequestService::new(Arc::clone(&broadcaster))
            .with_history_persist_path(history_path);

        let first = svc
            .respond_scoped(
                UserResponse {
                    request_id: "recovered-first-wins".to_string(),
                    decision: "allow_once".to_string(),
                    input: None,
                    channel: "web".to_string(),
                    sensitive: Vec::new(),
                },
                Some("principal-a"),
                Some("workspace-a"),
            )
            .await;
        let second = svc
            .respond_scoped(
                UserResponse {
                    request_id: "recovered-first-wins".to_string(),
                    decision: "deny".to_string(),
                    input: None,
                    channel: "web".to_string(),
                    sensitive: Vec::new(),
                },
                Some("principal-a"),
                Some("workspace-a"),
            )
            .await;

        assert_eq!(first, ScopedResponseResult::Accepted);
        assert_eq!(second, ScopedResponseResult::AlreadyResolved);
        let history = svc.list_history_for_scope("principal-a", "workspace-a", Some(1));
        assert_eq!(
            history[0]
                .response
                .as_ref()
                .map(|response| response.decision.as_str()),
            Some("allow_once")
        );
        let _first_event = recv_until(&mut receiver, |event| {
            matches!(event, RuntimeTransportEvent::HitlResolved { .. })
        })
        .await;
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn unscoped_unknown_orphan_recovery_remains_available_to_internal_callers() {
        let broadcaster = make_broadcaster();
        let svc = UserRequestService::new(Arc::clone(&broadcaster));

        assert!(
            svc.respond(UserResponse {
                request_id: "unknown-internal".to_string(),
                decision: "deny".to_string(),
                input: None,
                channel: "internal".to_string(),
                sensitive: Vec::new(),
            })
            .await
        );
        let history = svc.list_history_for_scope("", "", Some(1));
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].request.id, "unknown-internal");
        assert_eq!(history[0].request.source, "recovered_after_restart");
    }

    #[tokio::test]
    async fn history_tracks_pending_and_resolved_requests() {
        let broadcaster = make_broadcaster();
        let svc = Arc::new(UserRequestService::new(Arc::clone(&broadcaster)));
        let svc_clone = Arc::clone(&svc);

        let ask_handle = tokio::spawn(async move { svc_clone.ask(sample_request()).await });
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let pending = svc.list_pending().await;
        let req_id = pending[0].id.clone();
        let history = svc.list_history_for_scope("default", "default", Some(10));
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].request.id, req_id);
        assert_eq!(history[0].status, UserRequestStatus::Pending);
        assert!(history[0].resolved_at.is_none());

        assert!(
            svc.respond(UserResponse {
                request_id: req_id.clone(),
                decision: "allow_once".to_string(),
                input: None,
                channel: "web".to_string(),
                sensitive: Vec::new(),
            })
            .await
        );
        let _ = ask_handle.await.unwrap();

        let history = svc.list_history_for_scope("default", "default", Some(10));
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].request.id, req_id);
        assert_eq!(history[0].status, UserRequestStatus::Resolved);
        assert_eq!(
            history[0]
                .response
                .as_ref()
                .map(|response| response.decision.as_str()),
            Some("allow_once")
        );
        assert!(history[0].resolved_at.is_some());
    }

    /// Drain `receiver` until an event matching `wanted` arrives.
    /// Phase H2 introduced canonical dual-emit — every legacy
    /// `UserRequest*` event is now mirrored by a `Hitl*` canonical
    /// event from the same `ask()` / `respond()` call. Tests that
    /// validate the legacy variants must skip the canonical mirrors
    /// (and vice-versa) so receiver-order doesn't change which
    /// variant lands on a particular `recv()` slot. 1s timeout.
    async fn recv_until<F>(
        receiver: &mut tokio::sync::broadcast::Receiver<RuntimeTransportEvent>,
        mut wanted: F,
    ) -> RuntimeTransportEvent
    where
        F: FnMut(&RuntimeTransportEvent) -> bool,
    {
        loop {
            let event = tokio::time::timeout(tokio::time::Duration::from_secs(1), receiver.recv())
                .await
                .expect("recv timeout")
                .expect("recv should produce an event");
            if wanted(&event) {
                return event;
            }
        }
    }

    #[tokio::test]
    async fn resolved_events_include_owner_agent_id_for_harness_requests() {
        let broadcaster = make_broadcaster();
        let mut receiver = broadcaster.subscribe();
        let svc = Arc::new(UserRequestService::new(Arc::clone(&broadcaster)));
        let svc_clone = Arc::clone(&svc);

        let mut request = sample_request();
        request.request_type = "harness.notify_owner.briefing".to_string();
        request.context = serde_json::json!({
            "owner_agent_id": "ceo",
            "focus_area": "daily review"
        });

        let ask_handle = tokio::spawn(async move { svc_clone.ask(request).await });

        // H6.1 retired the legacy `UserRequestPending` / `UserRequestResolved`
        // typed emits. The canonical `HitlRequested` / `HitlResolved`
        // pair with `source: "user_request"` is now the sole on-the-wire
        // representation, and `owner_agent_id` rides on the canonical
        // event's top-level `agent_id` field.
        let pending_event = recv_until(&mut receiver, |e| {
            matches!(
                e,
                RuntimeTransportEvent::HitlRequested { source, .. } if source == "user_request"
            )
        })
        .await;
        let request_id = match pending_event {
            RuntimeTransportEvent::HitlRequested { correlation_id, .. } => correlation_id,
            other => panic!("unexpected event: {other:?}"),
        };

        assert!(
            svc.respond(UserResponse {
                request_id: request_id.clone(),
                decision: "allow_once".to_string(),
                input: None,
                channel: "web".to_string(),
                sensitive: Vec::new(),
            })
            .await
        );

        let resolved_event = recv_until(&mut receiver, |e| {
            matches!(
                e,
                RuntimeTransportEvent::HitlResolved { source, .. } if source == "user_request"
            )
        })
        .await;
        match resolved_event {
            RuntimeTransportEvent::HitlResolved {
                correlation_id: resolved_request_id,
                agent_id,
                ..
            } => {
                assert_eq!(resolved_request_id, request_id);
                assert_eq!(agent_id.as_deref(), Some("ceo"));
            },
            other => panic!("unexpected event: {other:?}"),
        }

        let response = ask_handle.await.unwrap();
        assert_eq!(response.decision, "allow_once");
    }

    #[tokio::test]
    async fn history_persists_to_disk() {
        let broadcaster = make_broadcaster();
        let dir = tempdir().expect("tempdir");
        let history_path = dir.path().join("user_request_history.json");
        let svc = Arc::new(
            UserRequestService::new(Arc::clone(&broadcaster))
                .with_history_persist_path(history_path.clone()),
        );
        let svc_clone = Arc::clone(&svc);

        let ask_handle = tokio::spawn(async move { svc_clone.ask(sample_request()).await });
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let pending = svc.list_pending().await;
        let req_id = pending[0].id.clone();
        assert!(
            svc.respond(UserResponse {
                request_id: req_id.clone(),
                decision: "deny".to_string(),
                input: None,
                channel: "web".to_string(),
                sensitive: Vec::new(),
            })
            .await
        );
        let _ = ask_handle.await.unwrap();
        for _ in 0..100 {
            if !svc
                .generic_request_publication_retry_started
                .load(Ordering::Acquire)
                && !svc
                    .generic_resolution_publication_retry_started
                    .load(Ordering::Acquire)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            !svc.generic_request_publication_retry_started
                .load(Ordering::Acquire)
                && !svc
                    .generic_resolution_publication_retry_started
                    .load(Ordering::Acquire),
            "restart fixture must first stop in-process publication owners"
        );
        drop(svc);
        tokio::task::yield_now().await;

        let restored = UserRequestService::new(make_broadcaster())
            .with_history_persist_path(history_path.clone());
        let history = restored.list_history_for_scope("default", "default", Some(10));
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].request.id, req_id);
        assert_eq!(history[0].status, UserRequestStatus::Resolved);
        assert_eq!(
            history[0]
                .response
                .as_ref()
                .map(|response| response.decision.as_str()),
            Some("deny")
        );
    }

    #[tokio::test]
    async fn pending_snapshot_round_trips_across_restart_preserving_context() {
        // Process 1: original service writes a snapshot, then "crashes".
        let broadcaster1 = make_broadcaster();
        let dir = tempdir().expect("tempdir");
        let pending_path = dir.path().join("user_request_pending.json");
        let history_path = dir.path().join("user_request_history.json");

        let svc1 = Arc::new(
            UserRequestService::new(Arc::clone(&broadcaster1))
                .with_history_persist_path(history_path.clone())
                .with_pending_persist_path(pending_path.clone())
                .await,
        );
        let svc1_clone = Arc::clone(&svc1);

        // Long timeout so the entry stays pending across the "restart".
        let mut request = sample_request();
        request.timeout_secs = 600;
        request.request_type = "memory_clarification".to_string();
        request.context = serde_json::json!({
            "memory_question_key": "key-under-test",
            "source": "memory_consolidation_clarification",
        });

        let ask_task = tokio::spawn(async move { svc1_clone.ask(request).await });
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let pending = svc1.list_pending().await;
        assert_eq!(pending.len(), 1, "ask should register one pending entry");
        let req_id = pending[0].id.clone();

        // Simulate restart by dropping svc1 + aborting the ask task.
        // The pending + history JSON files persist via tempdir.
        ask_task.abort();
        let timeout_handles = {
            let mut pending = svc1.pending.write().await;
            pending
                .by_id
                .values_mut()
                .filter_map(|entry| entry.timeout_handle.take())
                .collect::<Vec<_>>()
        };
        for handle in timeout_handles {
            handle.abort();
            let _ = handle.await;
        }
        for _ in 0..100 {
            if !svc1
                .generic_request_publication_retry_started
                .load(Ordering::Acquire)
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            !svc1
                .generic_request_publication_retry_started
                .load(Ordering::Acquire),
            "simulated crash must first stop the in-process publication owner"
        );
        drop(svc1);

        // Process 2: new service rehydrates from disk.
        let broadcaster2 = make_broadcaster();
        let svc2 = Arc::new(
            UserRequestService::new(Arc::clone(&broadcaster2))
                .with_history_persist_path(history_path.clone())
                .with_pending_persist_path(pending_path.clone())
                .await,
        );

        let restored = svc2.list_pending().await;
        assert_eq!(restored.len(), 1, "snapshot should rehydrate the entry");
        assert_eq!(restored[0].id, req_id);
        assert_eq!(
            restored[0]
                .context
                .get("memory_question_key")
                .and_then(serde_json::Value::as_str),
            Some("key-under-test"),
            "restored context must preserve memory_question_key"
        );

        // History was reverted from fake-resolved back to Pending so
        // the consolidator's dedup sees the live state.
        let history = svc2.list_history_for_scope("default", "default", Some(10));
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].status, UserRequestStatus::Pending);

        // Operator answers — resolution lands in history with the
        // ORIGINAL context (carried through the restored pending entry).
        assert_eq!(
            svc2.respond_scoped(
                UserResponse {
                    request_id: req_id.clone(),
                    decision: "answer".to_string(),
                    input: Some("yes please".to_string()),
                    channel: "web".to_string(),
                    sensitive: Vec::new(),
                },
                Some("default"),
                Some("default"),
            )
            .await,
            ScopedResponseResult::Accepted
        );

        let history = svc2.list_history_for_scope("default", "default", Some(10));
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].status, UserRequestStatus::Resolved);
        assert_eq!(
            history[0]
                .request
                .context
                .get("memory_question_key")
                .and_then(serde_json::Value::as_str),
            Some("key-under-test"),
            "resolved history record must carry the original memory_question_key"
        );
        assert_eq!(
            history[0]
                .response
                .as_ref()
                .map(|response| response.decision.as_str()),
            Some("answer")
        );

        // Snapshot was rewritten — restored entry is gone.
        assert!(svc2.list_pending().await.is_empty());
    }

    #[tokio::test]
    async fn restored_pending_entries_with_expired_timeout_fire_synthetic_timeout() {
        let broadcaster = make_broadcaster();
        let dir = tempdir().expect("tempdir");
        let pending_path = dir.path().join("user_request_pending.json");
        let history_path = dir.path().join("user_request_history.json");

        // Pre-seed the pending file with an entry whose timeout has
        // already elapsed at restore time.
        let expired_request = UserRequest {
            id: "expired-1".to_string(),
            request_type: "tool_authorization".to_string(),
            question: "stale?".to_string(),
            options: vec![RequestOption {
                id: "deny".to_string(),
                label: "Deny".to_string(),
                requires_input: false,
            }],
            principal: "default".to_string(),
            workspace: "default".to_string(),
            context: serde_json::json!({"key": "value"}),
            source: "executor".to_string(),
            execution_id: None,
            task_id: None,
            timeout_secs: 1,
            default_on_timeout: "deny".to_string(),
            created_at: chrono::Utc::now().timestamp_millis() - 60_000,
            sensitive: None,
        };
        std::fs::write(
            &pending_path,
            serde_json::to_vec(&vec![expired_request.clone()]).unwrap(),
        )
        .unwrap();

        let svc = Arc::new(
            UserRequestService::new(Arc::clone(&broadcaster))
                .with_history_persist_path(history_path.clone())
                .with_pending_persist_path(pending_path.clone())
                .await,
        );

        // Expired entry was fired synthetically — not present in
        // pending, present in history as Resolved with the default.
        assert!(svc.list_pending().await.is_empty());
        let history = svc.list_history_for_scope("default", "default", Some(10));
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].status, UserRequestStatus::Resolved);
        assert_eq!(
            history[0]
                .response
                .as_ref()
                .map(|response| response.decision.as_str()),
            Some("deny")
        );
        assert_eq!(
            history[0]
                .response
                .as_ref()
                .map(|response| response.channel.as_str()),
            Some("timeout")
        );
    }

    #[test]
    fn restored_pending_history_records_become_non_actionable_resolution_entries() {
        let mut record = UserRequestRecord::pending(sample_request());
        record.request.id = "req-1".to_string();
        record.request.created_at = 1234;
        let mut records = vec![record];

        normalize_restored_history_records(&mut records);

        assert_eq!(records[0].status, UserRequestStatus::Resolved);
        assert_eq!(records[0].resolved_at, Some(1234));
        assert_eq!(
            records[0]
                .response
                .as_ref()
                .map(|response| response.channel.as_str()),
            Some("service_restart")
        );
        assert_eq!(
            records[0]
                .response
                .as_ref()
                .map(|response| response.decision.as_str()),
            Some("deny")
        );
    }
}
