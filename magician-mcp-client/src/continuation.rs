//! Bounded process-local ownership for MCP requests that have already been dispatched.
//!
//! The official SDK continues to own MRTR and Tasks wire types. This module only keeps
//! their validated continuation authority attached to the exact client and tool that
//! dispatched the request. It deliberately exposes no provider payload, task id, request
//! state, endpoint, or credential material.

use std::{
    collections::HashMap,
    fmt,
    num::NonZeroU64,
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rmcp::model::{
    CallToolRequestParams, CreateTaskResult, GetTaskResult, InputRequests, InputRequiredResult,
    InputResponses, TaskPayload,
};

use crate::{
    mrtr::{
        drop_input_responses_iterative, prepare_input_responses, project_input_slots,
        project_input_slots_from_requests, McpMrtrInputSlot, McpMrtrResponse,
        McpPreparedMrtrResponses,
    },
    recovery::McpTaskRecoveryRecord,
    task::{McpPreparedTaskResponses, McpTaskNotificationHints, McpTaskProgress, McpTaskState},
    validation::measure_serialized_request,
    McpClientError, McpClientLimits, McpToolId,
};

pub const MCP_CONTINUATION_CONTRACT_V1: &str = "magician.mcp-continuation.v1";
pub const MCP_MRTR_CLAIM_CONTRACT_V1: &str = "magician.mcp-mrtr-claim.v1";
pub const MAX_PROCESS_CONTINUATION_BYTES: usize = 512 * 1024 * 1024;
pub const MAX_PROCESS_CONTINUATION_ENTRIES: usize = 512;

#[derive(Default)]
struct ProcessContinuationBudgetState {
    reserved_bytes: usize,
    reserved_entries: usize,
}

struct ProcessContinuationBudget {
    state: Mutex<ProcessContinuationBudgetState>,
}

impl ProcessContinuationBudget {
    fn shared() -> Arc<Self> {
        static BUDGET: OnceLock<Arc<ProcessContinuationBudget>> = OnceLock::new();
        Arc::clone(BUDGET.get_or_init(|| {
            Arc::new(Self {
                state: Mutex::new(ProcessContinuationBudgetState::default()),
            })
        }))
    }

    fn reserve(
        self: &Arc<Self>,
        bytes: usize,
        entries: usize,
    ) -> Result<ProcessContinuationLease, McpClientError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| McpClientError::ContinuationStateUnavailable)?;
        let next_bytes = state
            .reserved_bytes
            .checked_add(bytes)
            .ok_or(McpClientError::ContinuationCapacityExceeded)?;
        let next_entries = state
            .reserved_entries
            .checked_add(entries)
            .ok_or(McpClientError::ContinuationCapacityExceeded)?;
        if next_bytes > MAX_PROCESS_CONTINUATION_BYTES
            || next_entries > MAX_PROCESS_CONTINUATION_ENTRIES
        {
            return Err(McpClientError::ContinuationCapacityExceeded);
        }
        state.reserved_bytes = next_bytes;
        state.reserved_entries = next_entries;
        Ok(ProcessContinuationLease {
            budget: Arc::clone(self),
            bytes,
            entries,
        })
    }
}

struct ProcessContinuationLease {
    budget: Arc<ProcessContinuationBudget>,
    bytes: usize,
    entries: usize,
}

impl Drop for ProcessContinuationLease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.budget.state.lock() {
            state.reserved_bytes = state.reserved_bytes.saturating_sub(self.bytes);
            state.reserved_entries = state.reserved_entries.saturating_sub(self.entries);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum McpContinuationKind {
    AdditionalInput,
    RemoteTask,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct McpContinuationId {
    pub(crate) client_instance_id: u64,
    pub(crate) sequence: NonZeroU64,
}

impl fmt::Debug for McpContinuationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("McpContinuationId([REDACTED])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct McpContinuationRevision(pub(crate) NonZeroU64);

impl McpContinuationRevision {
    pub fn get(self) -> u64 {
        self.0.get()
    }
}

/// Copyable, payload-free reference to one retained continuation.
///
/// This is not authorization and intentionally implements neither `Serialize` nor a
/// value-bearing `Debug`. Every state transition must still be checked against the
/// owning [`McpClient`](crate::McpClient), exact revision, and monotonic deadline.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct McpPendingCall {
    pub(crate) id: McpContinuationId,
    pub(crate) revision: McpContinuationRevision,
    pub(crate) kind: McpContinuationKind,
    pub(crate) expires_at: Instant,
}

impl McpPendingCall {
    pub fn kind(self) -> McpContinuationKind {
        self.kind
    }

    pub fn revision(self) -> McpContinuationRevision {
        self.revision
    }

    pub fn remaining(self) -> Duration {
        self.expires_at.saturating_duration_since(Instant::now())
    }

    pub fn is_expired(self) -> bool {
        self.remaining().is_zero()
    }
}

impl fmt::Debug for McpPendingCall {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpPendingCall")
            .field("contract", &MCP_CONTINUATION_CONTRACT_V1)
            .field("kind", &self.kind)
            .field("revision", &self.revision)
            .field("expired", &self.is_expired())
            .finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct McpMrtrClaimId(NonZeroU64);

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct McpMrtrRoundCounts {
    state_only: usize,
    interactive: usize,
}

/// Move-only lease over one exact, atomically claimed MRTR continuation revision.
///
/// The SDK request state, original tool parameters, server input keys, and prepared
/// values remain inside the continuation owner. Dropping this lease before a retry
/// restores the exact input-required revision, making pre-dispatch cancellation safe.
pub struct McpClaimedMrtrCall {
    owner_state: Weak<Mutex<ContinuationState>>,
    client_instance_id: u64,
    pending: McpPendingCall,
    claim_id: McpMrtrClaimId,
    response_count: usize,
    active: bool,
}

impl McpClaimedMrtrCall {
    pub fn pending(&self) -> McpPendingCall {
        self.pending
    }

    pub fn response_count(&self) -> usize {
        self.response_count
    }
}

impl fmt::Debug for McpClaimedMrtrCall {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpClaimedMrtrCall")
            .field("contract", &MCP_MRTR_CLAIM_CONTRACT_V1)
            .field("kind", &self.pending.kind())
            .field("revision", &self.pending.revision())
            .field("response_count", &self.response_count)
            .finish()
    }
}

impl Drop for McpClaimedMrtrCall {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let Some(state) = self.owner_state.upgrade() else {
            return;
        };
        release_input_claim(&state, self.client_instance_id, self.pending, self.claim_id);
    }
}

impl Drop for McpClaimedTaskOperation {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let Some(state) = self.owner_state.upgrade() else {
            return;
        };
        release_task_operation(
            &state,
            self.client_instance_id,
            self.pending,
            self.claim_id,
            self.kind,
            self.restore_hint,
            self.dispatched,
        );
    }
}

pub(crate) struct McpContinuationOwner {
    client_instance_id: u64,
    max_entries: usize,
    max_accounted_bytes: usize,
    max_pending_payload_bytes: usize,
    continuation_timeout: Duration,
    task_poll_floor: Duration,
    limits: McpClientLimits,
    state: Arc<Mutex<ContinuationState>>,
    _process_budget: Option<ProcessContinuationLease>,
}

struct ContinuationState {
    next_sequence: u64,
    next_claim_sequence: u64,
    accounted_bytes: usize,
    entries: HashMap<NonZeroU64, ContinuationEntry>,
    task_hints: Arc<McpTaskNotificationHints>,
}

struct ContinuationEntry {
    accounted_bytes: usize,
    state: ContinuationEntryState,
}

enum ContinuationEntryState {
    InFlight {
        tool: McpToolId,
        params: CallToolRequestParams,
        request_bytes: usize,
        reserved_at: Instant,
    },
    InputRequired {
        tool: McpToolId,
        params: CallToolRequestParams,
        result: Arc<InputRequiredResult>,
        rounds: McpMrtrRoundCounts,
        revision: McpContinuationRevision,
        expires_at: Instant,
    },
    ClaimedInput {
        tool: McpToolId,
        params: CallToolRequestParams,
        result: Arc<InputRequiredResult>,
        rounds: McpMrtrRoundCounts,
        revision: McpContinuationRevision,
        expires_at: Instant,
        prepared: McpPreparedMrtrResponses,
        claim_id: McpMrtrClaimId,
    },
    RetryingInput {
        tool: McpToolId,
        params: CallToolRequestParams,
        request_bytes: usize,
        rounds: McpMrtrRoundCounts,
        previous_revision: McpContinuationRevision,
        reserved_at: Instant,
    },
    Task {
        task: RetainedTask,
        revision: McpContinuationRevision,
    },
    TaskOperation {
        task: RetainedTask,
        revision: McpContinuationRevision,
        claim_id: McpMrtrClaimId,
        kind: McpTaskOperationKind,
    },
}

struct RetainedTask {
    tool: McpToolId,
    task_id: String,
    created_at: String,
    started_at: Option<Instant>,
    started_at_epoch_millis: u64,
    lifetime_expires_at: Instant,
    expires_at: Instant,
    poll_interval: Duration,
    next_poll_at: Instant,
    input_requests: Option<Arc<InputRequests>>,
    operation_count: usize,
    cancel_requested: bool,
}

pub(crate) struct McpTaskRecoverySnapshot {
    pub(crate) tool_name: String,
    pub(crate) task_id: String,
    pub(crate) created_at: String,
    pub(crate) started_at_epoch_millis: u64,
    pub(crate) lifetime_expires_at_epoch_millis: u64,
    pub(crate) expires_at_epoch_millis: u64,
    pub(crate) operation_count: usize,
    pub(crate) cancel_requested: bool,
    pub(crate) revision: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum McpTaskOperationKind {
    Poll,
    Update,
    Cancel,
}

pub(crate) struct McpClaimedTaskOperation {
    owner_state: Weak<Mutex<ContinuationState>>,
    client_instance_id: u64,
    pending: McpPendingCall,
    claim_id: McpMrtrClaimId,
    kind: McpTaskOperationKind,
    restore_hint: bool,
    dispatched: bool,
    active: bool,
}

impl McpClaimedTaskOperation {
    pub(crate) fn mark_dispatched(&mut self) {
        self.dispatched = true;
    }
}

pub(crate) enum McpTaskPollCommit {
    Pending(McpPendingCall),
    Terminal(TaskPayload),
}

impl ContinuationEntry {
    fn expired(&self, now: Instant) -> bool {
        match &self.state {
            ContinuationEntryState::InFlight { .. }
            | ContinuationEntryState::RetryingInput { .. } => false,
            ContinuationEntryState::InputRequired { expires_at, .. }
            | ContinuationEntryState::ClaimedInput { expires_at, .. }
            | ContinuationEntryState::Task {
                task: RetainedTask { expires_at, .. },
                ..
            }
            | ContinuationEntryState::TaskOperation {
                task: RetainedTask { expires_at, .. },
                ..
            } => *expires_at <= now,
        }
    }

    fn matches(&self, pending: McpPendingCall) -> bool {
        match &self.state {
            ContinuationEntryState::InFlight { .. }
            | ContinuationEntryState::RetryingInput { .. } => false,
            ContinuationEntryState::InputRequired {
                revision,
                expires_at,
                ..
            } => {
                pending.kind == McpContinuationKind::AdditionalInput
                    && pending.revision == *revision
                    && pending.expires_at == *expires_at
            },
            ContinuationEntryState::Task { task, revision, .. } => {
                pending.kind == McpContinuationKind::RemoteTask
                    && pending.revision == *revision
                    && pending.expires_at == task.expires_at
            },
            ContinuationEntryState::ClaimedInput { .. }
            | ContinuationEntryState::TaskOperation { .. } => false,
        }
    }

    fn has_task_id(&self, task_id: &str) -> bool {
        match &self.state {
            ContinuationEntryState::Task { task, .. }
            | ContinuationEntryState::TaskOperation { task, .. } => task.task_id == task_id,
            _ => false,
        }
    }
}

impl McpContinuationOwner {
    #[cfg(test)]
    pub(crate) fn new(
        client_instance_id: u64,
        limits: &McpClientLimits,
        continuation_timeout: Duration,
    ) -> Self {
        Self::new_unbudgeted(
            client_instance_id,
            limits,
            continuation_timeout,
            Duration::from_millis(250),
            Arc::new(McpTaskNotificationHints::new(
                limits.max_pending_continuations,
            )),
        )
    }

    fn new_unbudgeted(
        client_instance_id: u64,
        limits: &McpClientLimits,
        continuation_timeout: Duration,
        task_poll_floor: Duration,
        task_hints: Arc<McpTaskNotificationHints>,
    ) -> Self {
        Self {
            client_instance_id,
            max_entries: limits.max_pending_continuations,
            max_accounted_bytes: limits.max_continuation_bytes,
            max_pending_payload_bytes: limits.max_result_bytes,
            continuation_timeout,
            task_poll_floor,
            limits: limits.clone(),
            state: Arc::new(Mutex::new(ContinuationState {
                next_sequence: 1,
                next_claim_sequence: 1,
                accounted_bytes: 0,
                entries: HashMap::new(),
                task_hints,
            })),
            _process_budget: None,
        }
    }

    pub(crate) fn try_new_with_task_support(
        client_instance_id: u64,
        limits: &McpClientLimits,
        continuation_timeout: Duration,
        task_poll_floor: Duration,
        task_hints: Arc<McpTaskNotificationHints>,
    ) -> Result<Self, McpClientError> {
        let process_budget = ProcessContinuationBudget::shared().reserve(
            limits.max_continuation_bytes,
            limits.max_pending_continuations,
        )?;
        let mut owner = Self::new_unbudgeted(
            client_instance_id,
            limits,
            continuation_timeout,
            task_poll_floor,
            task_hints,
        );
        owner._process_budget = Some(process_budget);
        Ok(owner)
    }

    #[cfg(test)]
    pub(crate) fn new_unbudgeted_with_task_support(
        client_instance_id: u64,
        limits: &McpClientLimits,
        continuation_timeout: Duration,
        task_poll_floor: Duration,
        task_hints: Arc<McpTaskNotificationHints>,
    ) -> Self {
        Self::new_unbudgeted(
            client_instance_id,
            limits,
            continuation_timeout,
            task_poll_floor,
            task_hints,
        )
    }

    pub(crate) fn reserve(
        &self,
        tool: McpToolId,
        params: CallToolRequestParams,
        request_bytes: usize,
    ) -> Result<McpContinuationReservation<'_>, McpClientError> {
        let reserved_bytes = request_bytes
            .checked_add(self.max_pending_payload_bytes)
            .ok_or(McpClientError::ContinuationCapacityExceeded)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| McpClientError::ContinuationStateUnavailable)?;
        let now = Instant::now();
        prune_expired(&mut state, now);
        let next_bytes = state
            .accounted_bytes
            .checked_add(reserved_bytes)
            .ok_or(McpClientError::ContinuationCapacityExceeded)?;
        if state.entries.len() >= self.max_entries || next_bytes > self.max_accounted_bytes {
            return Err(McpClientError::ContinuationCapacityExceeded);
        }
        let sequence = NonZeroU64::new(state.next_sequence)
            .ok_or(McpClientError::ContinuationIdentityExhausted)?;
        state.next_sequence = state.next_sequence.checked_add(1).unwrap_or(0);
        state.accounted_bytes = next_bytes;
        state.entries.insert(
            sequence,
            ContinuationEntry {
                accounted_bytes: reserved_bytes,
                state: ContinuationEntryState::InFlight {
                    tool,
                    params,
                    request_bytes,
                    reserved_at: now,
                },
            },
        );
        drop(state);
        Ok(McpContinuationReservation {
            owner: self,
            sequence,
            active: true,
        })
    }

    pub(crate) fn is_active(&self, pending: McpPendingCall) -> Result<bool, McpClientError> {
        if pending.id.client_instance_id != self.client_instance_id {
            return Ok(false);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| McpClientError::ContinuationStateUnavailable)?;
        let now = Instant::now();
        prune_expired(&mut state, now);
        Ok(state
            .entries
            .get(&pending.id.sequence)
            .is_some_and(|entry| entry.matches(pending)))
    }

    pub(crate) fn pending_count(&self) -> Result<usize, McpClientError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| McpClientError::ContinuationStateUnavailable)?;
        let now = Instant::now();
        prune_expired(&mut state, now);
        Ok(state
            .entries
            .values()
            .filter(|entry| !matches!(entry.state, ContinuationEntryState::InFlight { .. }))
            .count())
    }

    pub(crate) fn recovery_disposition(
        &self,
        pending: McpPendingCall,
    ) -> Result<crate::McpContinuationRecoveryDisposition, McpClientError> {
        if !self.is_active(pending)? {
            return Err(McpClientError::ContinuationNotActive);
        }
        Ok(match pending.kind {
            McpContinuationKind::RemoteTask => {
                crate::McpContinuationRecoveryDisposition::RecoverableRemoteTask
            },
            McpContinuationKind::AdditionalInput => {
                crate::McpContinuationRecoveryDisposition::SessionBound
            },
        })
    }

    pub(crate) fn task_recovery_snapshot(
        &self,
        pending: McpPendingCall,
    ) -> Result<McpTaskRecoverySnapshot, McpClientError> {
        if pending.id.client_instance_id != self.client_instance_id {
            return Err(McpClientError::ContinuationNotActive);
        }
        if pending.kind != McpContinuationKind::RemoteTask {
            return Err(McpClientError::ContinuationRecoverySessionBound);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| McpClientError::ContinuationStateUnavailable)?;
        let now = Instant::now();
        prune_expired(&mut state, now);
        let entry = state
            .entries
            .get(&pending.id.sequence)
            .ok_or(McpClientError::ContinuationNotActive)?;
        if !entry.matches(pending) {
            return Err(McpClientError::ContinuationNotActive);
        }
        let ContinuationEntryState::Task { task, revision } = &entry.state else {
            return Err(McpClientError::ContinuationStateUnavailable);
        };
        let now_epoch_millis = now_epoch_millis()?;
        Ok(McpTaskRecoverySnapshot {
            tool_name: task.tool.remote_name().to_owned(),
            task_id: task.task_id.clone(),
            created_at: task.created_at.clone(),
            started_at_epoch_millis: task.started_at_epoch_millis,
            lifetime_expires_at_epoch_millis: epoch_deadline(
                now_epoch_millis,
                task.lifetime_expires_at.saturating_duration_since(now),
            ),
            expires_at_epoch_millis: epoch_deadline(
                now_epoch_millis,
                task.expires_at.saturating_duration_since(now),
            ),
            operation_count: task.operation_count,
            cancel_requested: task.cancel_requested,
            revision: revision.get(),
        })
    }

    pub(crate) fn recover_task(
        &self,
        record: McpTaskRecoveryRecord,
        tool: McpToolId,
        result: GetTaskResult,
        payload_bytes: usize,
    ) -> Result<McpTaskPollCommit, McpClientError> {
        let now = Instant::now();
        let now_epoch_millis = now_epoch_millis()?;
        let remote_task = result.task.task;
        let payload = result.task.payload;
        if remote_task.task_id != record.task_id || remote_task.created_at != record.created_at {
            return Err(McpClientError::ResponseRejected(
                "task recovery response changed immutable task identity".to_owned(),
            ));
        }
        if remote_task.status.is_terminal() {
            return Ok(McpTaskPollCommit::Terminal(payload));
        }
        let input_requests = match payload {
            TaskPayload::Working => None,
            TaskPayload::InputRequired { input_requests } => Some(Arc::new(input_requests)),
            TaskPayload::Completed { .. } | TaskPayload::Failed { .. } | TaskPayload::Cancelled => {
                return Err(McpClientError::ResponseRejected(
                    "task recovery status and payload disagreed".to_owned(),
                ));
            },
            _ => {
                return Err(McpClientError::ResponseRejected(
                    "task recovery status is unsupported".to_owned(),
                ));
            },
        };
        let operation_count = usize::try_from(record.operation_count)
            .map_err(|_| McpClientError::TaskRecoveryRecordRejected)?;
        if operation_count > self.limits.max_task_operations {
            return Err(McpClientError::TaskOperationLimitExceeded);
        }
        let lifetime_expires_at = instant_from_epoch_deadline(
            now,
            now_epoch_millis,
            record.lifetime_expires_at_epoch_millis,
        )?;
        let persisted_expires_at =
            instant_from_epoch_deadline(now, now_epoch_millis, record.expires_at_epoch_millis)?;
        let remote_ttl_expires_at = remote_task
            .ttl_ms
            .map(Duration::from_millis)
            .map(|ttl| {
                epoch_task_deadline(now, now_epoch_millis, record.started_at_epoch_millis, ttl)
            })
            .transpose()?
            .unwrap_or(lifetime_expires_at);
        let expires_at = persisted_expires_at
            .min(remote_ttl_expires_at)
            .min(lifetime_expires_at);
        if expires_at <= now {
            return Err(McpClientError::ContinuationNotActive);
        }
        let poll_interval = remote_task
            .poll_interval_ms
            .map(Duration::from_millis)
            .unwrap_or(self.task_poll_floor)
            .max(self.task_poll_floor);
        let revision = NonZeroU64::new(record.revision)
            .map(McpContinuationRevision)
            .ok_or(McpClientError::TaskRecoveryRecordRejected)?;

        let mut state = self
            .state
            .lock()
            .map_err(|_| McpClientError::ContinuationStateUnavailable)?;
        prune_expired(&mut state, now);
        if state
            .entries
            .values()
            .any(|entry| entry.has_task_id(&record.task_id))
        {
            return Err(McpClientError::TaskRecoveryAlreadyActive);
        }
        if state.entries.len() >= self.max_entries
            || state.accounted_bytes.saturating_add(payload_bytes) > self.max_accounted_bytes
        {
            return Err(McpClientError::ContinuationCapacityExceeded);
        }
        let sequence = NonZeroU64::new(state.next_sequence)
            .ok_or(McpClientError::ContinuationIdentityExhausted)?;
        state.next_sequence = state.next_sequence.checked_add(1).unwrap_or(0);
        state.task_hints.register(&record.task_id)?;
        state.accounted_bytes += payload_bytes;
        state.entries.insert(
            sequence,
            ContinuationEntry {
                accounted_bytes: payload_bytes,
                state: ContinuationEntryState::Task {
                    task: RetainedTask {
                        tool,
                        task_id: record.task_id,
                        created_at: record.created_at,
                        started_at: None,
                        started_at_epoch_millis: record.started_at_epoch_millis,
                        lifetime_expires_at,
                        expires_at,
                        poll_interval,
                        next_poll_at: deadline_after(now, poll_interval).min(expires_at),
                        input_requests,
                        operation_count,
                        cancel_requested: record.cancel_requested,
                    },
                    revision,
                },
            },
        );
        Ok(McpTaskPollCommit::Pending(McpPendingCall {
            id: McpContinuationId {
                client_instance_id: self.client_instance_id,
                sequence,
            },
            revision,
            kind: McpContinuationKind::RemoteTask,
            expires_at,
        }))
    }

    pub(crate) fn input_slots(
        &self,
        pending: McpPendingCall,
    ) -> Result<Vec<McpMrtrInputSlot>, McpClientError> {
        let result = self.input_required_for(pending)?;
        project_input_slots(pending, &result, self.limits.max_mrtr_inputs)
    }

    pub(crate) fn prepare_input_responses(
        &self,
        pending: McpPendingCall,
        responses: Vec<McpMrtrResponse>,
    ) -> Result<McpPreparedMrtrResponses, McpClientError> {
        let result = self.input_required_for(pending)?;
        prepare_input_responses(pending, &result, responses, &self.limits)
    }

    pub(crate) fn task_progress(
        &self,
        pending: McpPendingCall,
    ) -> Result<McpTaskProgress, McpClientError> {
        if pending.id.client_instance_id != self.client_instance_id
            || pending.kind != McpContinuationKind::RemoteTask
        {
            return Err(McpClientError::ContinuationKindMismatch);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| McpClientError::ContinuationStateUnavailable)?;
        let now = Instant::now();
        prune_expired(&mut state, now);
        let entry = state
            .entries
            .get(&pending.id.sequence)
            .ok_or(McpClientError::ContinuationNotActive)?;
        if !entry.matches(pending) {
            return Err(McpClientError::ContinuationNotActive);
        }
        let ContinuationEntryState::Task { task, .. } = &entry.state else {
            return Err(McpClientError::ContinuationNotActive);
        };
        let hinted = state.task_hints.contains_hint(&task.task_id)?;
        let poll_after = if hinted {
            Duration::ZERO
        } else {
            task.next_poll_at.saturating_duration_since(now)
        };
        let task_state = if task.cancel_requested {
            McpTaskState::CancellationRequested
        } else if task.input_requests.is_some() {
            McpTaskState::InputRequired
        } else {
            McpTaskState::Working
        };
        Ok(McpTaskProgress::new(pending, task_state, poll_after))
    }

    pub(crate) fn task_input_slots(
        &self,
        pending: McpPendingCall,
    ) -> Result<Vec<McpMrtrInputSlot>, McpClientError> {
        let requests = self.task_input_requests_for(pending)?;
        project_input_slots_from_requests(pending, Some(&requests), self.limits.max_mrtr_inputs)
    }

    pub(crate) fn prepare_task_responses(
        &self,
        pending: McpPendingCall,
        responses: Vec<McpMrtrResponse>,
    ) -> Result<McpPreparedTaskResponses, McpClientError> {
        let requests = self.task_input_requests_for(pending)?;
        McpPreparedTaskResponses::prepare(pending, &requests, responses, &self.limits)
    }

    pub(crate) fn begin_task_poll(
        &self,
        pending: McpPendingCall,
    ) -> Result<(McpClaimedTaskOperation, String), McpClientError> {
        self.begin_task_operation(pending, McpTaskOperationKind::Poll, 0)
    }

    pub(crate) fn begin_task_cancel(
        &self,
        pending: McpPendingCall,
    ) -> Result<(McpClaimedTaskOperation, String), McpClientError> {
        self.begin_task_operation(pending, McpTaskOperationKind::Cancel, 0)
    }

    pub(crate) fn begin_task_update(
        &self,
        prepared: McpPreparedTaskResponses,
    ) -> Result<(McpClaimedTaskOperation, String, InputResponses), McpClientError> {
        let pending = prepared.pending();
        let prepared_bytes = prepared.accounted_bytes();
        let (claim, task_id) =
            self.begin_task_operation(pending, McpTaskOperationKind::Update, prepared_bytes)?;
        let Some(responses) = prepared.into_responses() else {
            drop(claim);
            return Err(McpClientError::ContinuationStateUnavailable);
        };
        Ok((claim, task_id, responses))
    }

    fn begin_task_operation(
        &self,
        pending: McpPendingCall,
        kind: McpTaskOperationKind,
        additional_bytes: usize,
    ) -> Result<(McpClaimedTaskOperation, String), McpClientError> {
        if pending.id.client_instance_id != self.client_instance_id
            || pending.kind != McpContinuationKind::RemoteTask
        {
            return Err(McpClientError::ContinuationKindMismatch);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| McpClientError::ContinuationStateUnavailable)?;
        let now = Instant::now();
        prune_expired(&mut state, now);
        let entry = state
            .entries
            .get(&pending.id.sequence)
            .ok_or(McpClientError::ContinuationNotActive)?;
        if !entry.matches(pending) {
            return Err(McpClientError::ContinuationNotActive);
        }
        let ContinuationEntryState::Task { task, .. } = &entry.state else {
            return Err(McpClientError::ContinuationNotActive);
        };
        let task_id = task.task_id.clone();
        let operation_count = task.operation_count;
        let input_required = task.input_requests.is_some();
        let cancel_requested = task.cancel_requested;
        let next_poll_at = task.next_poll_at;
        if operation_count >= self.limits.max_task_operations {
            return Err(McpClientError::TaskOperationLimitExceeded);
        }
        if kind == McpTaskOperationKind::Update && !input_required {
            return Err(McpClientError::TaskInputNotRequired);
        }
        if kind == McpTaskOperationKind::Cancel && cancel_requested {
            return Err(McpClientError::TaskCancellationAlreadyRequested);
        }
        let hinted = if kind == McpTaskOperationKind::Poll {
            state.task_hints.take(&task_id)?
        } else {
            false
        };
        if kind == McpTaskOperationKind::Poll && now < next_poll_at && !hinted {
            return Err(McpClientError::TaskPollNotReady {
                retry_after: next_poll_at.saturating_duration_since(now),
            });
        }
        let next_bytes = state
            .accounted_bytes
            .checked_add(additional_bytes)
            .ok_or(McpClientError::ContinuationCapacityExceeded)?;
        if next_bytes > self.max_accounted_bytes {
            if hinted {
                state.task_hints.mark(&task_id);
            }
            return Err(McpClientError::ContinuationCapacityExceeded);
        }
        let claim_sequence = NonZeroU64::new(state.next_claim_sequence)
            .ok_or(McpClientError::ContinuationIdentityExhausted)?;
        state.next_claim_sequence = state.next_claim_sequence.checked_add(1).unwrap_or(0);
        let claim_id = McpMrtrClaimId(claim_sequence);

        let entry = state
            .entries
            .remove(&pending.id.sequence)
            .ok_or(McpClientError::ContinuationStateUnavailable)?;
        let ContinuationEntry {
            accounted_bytes,
            state: entry_state,
        } = entry;
        let (mut task, revision) = match entry_state {
            ContinuationEntryState::Task { task, revision } => (task, revision),
            other => {
                state.entries.insert(
                    pending.id.sequence,
                    ContinuationEntry {
                        accounted_bytes,
                        state: other,
                    },
                );
                return Err(McpClientError::ContinuationStateUnavailable);
            },
        };
        task.operation_count = task.operation_count.saturating_add(1);
        let task_id = task.task_id.clone();
        state.entries.insert(
            pending.id.sequence,
            ContinuationEntry {
                accounted_bytes,
                state: ContinuationEntryState::TaskOperation {
                    task,
                    revision,
                    claim_id,
                    kind,
                },
            },
        );
        drop(state);

        Ok((
            McpClaimedTaskOperation {
                owner_state: Arc::downgrade(&self.state),
                client_instance_id: self.client_instance_id,
                pending,
                claim_id,
                kind,
                restore_hint: hinted,
                dispatched: false,
                active: true,
            },
            task_id,
        ))
    }

    fn task_input_requests_for(
        &self,
        pending: McpPendingCall,
    ) -> Result<Arc<InputRequests>, McpClientError> {
        if pending.id.client_instance_id != self.client_instance_id
            || pending.kind != McpContinuationKind::RemoteTask
        {
            return Err(McpClientError::ContinuationKindMismatch);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| McpClientError::ContinuationStateUnavailable)?;
        let now = Instant::now();
        prune_expired(&mut state, now);
        let entry = state
            .entries
            .get(&pending.id.sequence)
            .ok_or(McpClientError::ContinuationNotActive)?;
        if !entry.matches(pending) {
            return Err(McpClientError::ContinuationNotActive);
        }
        let ContinuationEntryState::Task { task, .. } = &entry.state else {
            return Err(McpClientError::ContinuationNotActive);
        };
        task.input_requests
            .as_ref()
            .map(Arc::clone)
            .ok_or(McpClientError::TaskInputNotRequired)
    }

    pub(crate) fn commit_task_poll(
        &self,
        mut claim: McpClaimedTaskOperation,
        result: GetTaskResult,
        payload_bytes: usize,
    ) -> Result<McpTaskPollCommit, McpClientError> {
        if !claim.active
            || claim.client_instance_id != self.client_instance_id
            || claim.kind != McpTaskOperationKind::Poll
        {
            return Err(McpClientError::ContinuationNotActive);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| McpClientError::ContinuationStateUnavailable)?;
        let now = Instant::now();
        prune_expired(&mut state, now);
        let entry = state
            .entries
            .get(&claim.pending.id.sequence)
            .ok_or(McpClientError::ContinuationNotActive)?;
        let exact_claim = matches!(
            &entry.state,
            ContinuationEntryState::TaskOperation {
                revision,
                claim_id,
                kind,
                ..
            } if *revision == claim.pending.revision
                && *claim_id == claim.claim_id
                && *kind == McpTaskOperationKind::Poll
        );
        if !exact_claim {
            return Err(McpClientError::ContinuationNotActive);
        }
        let entry = state
            .entries
            .remove(&claim.pending.id.sequence)
            .ok_or(McpClientError::ContinuationStateUnavailable)?;
        claim.active = false;
        state.accounted_bytes = state.accounted_bytes.saturating_sub(entry.accounted_bytes);
        let ContinuationEntryState::TaskOperation {
            mut task,
            revision,
            claim_id,
            kind,
        } = entry.state
        else {
            return Err(McpClientError::ContinuationStateUnavailable);
        };
        if revision != claim.pending.revision
            || claim_id != claim.claim_id
            || kind != McpTaskOperationKind::Poll
        {
            state.task_hints.unregister(&task.task_id);
            return Err(McpClientError::ContinuationStateUnavailable);
        }

        let remote_task = result.task.task;
        let payload = result.task.payload;
        if remote_task.task_id != task.task_id || remote_task.created_at != task.created_at {
            state.task_hints.unregister(&task.task_id);
            return Err(McpClientError::ResponseRejected(
                "task status response changed immutable task identity".to_owned(),
            ));
        }
        if remote_task.status.is_terminal() {
            state.task_hints.unregister(&task.task_id);
            return Ok(McpTaskPollCommit::Terminal(payload));
        }

        let input_requests = match payload {
            TaskPayload::Working => None,
            TaskPayload::InputRequired { input_requests } => Some(Arc::new(input_requests)),
            TaskPayload::Completed { .. } | TaskPayload::Failed { .. } | TaskPayload::Cancelled => {
                state.task_hints.unregister(&task.task_id);
                return Err(McpClientError::ResponseRejected(
                    "task status and payload disagreed".to_owned(),
                ));
            },
            _ => {
                state.task_hints.unregister(&task.task_id);
                return Err(McpClientError::ResponseRejected(
                    "task status is unsupported".to_owned(),
                ));
            },
        };
        let ttl_expires_at = if let Some(ttl_ms) = remote_task.ttl_ms {
            match task.started_at {
                Some(started_at) => deadline_after(started_at, Duration::from_millis(ttl_ms)),
                None => match now_epoch_millis().and_then(|now_epoch_millis| {
                    epoch_task_deadline(
                        now,
                        now_epoch_millis,
                        task.started_at_epoch_millis,
                        Duration::from_millis(ttl_ms),
                    )
                }) {
                    Ok(deadline) => deadline,
                    Err(error) => {
                        state.task_hints.unregister(&task.task_id);
                        return Err(error);
                    },
                },
            }
        } else {
            task.lifetime_expires_at
        };
        // A later server response may shorten retained authority but never extend the
        // previously validated task deadline. This is especially important after
        // restart, where wall-clock reconstruction must not manufacture extra TTL.
        task.expires_at = task
            .expires_at
            .min(ttl_expires_at)
            .min(task.lifetime_expires_at);
        if task.expires_at <= now {
            state.task_hints.unregister(&task.task_id);
            return Err(McpClientError::ContinuationNotActive);
        }
        task.poll_interval = remote_task
            .poll_interval_ms
            .map(Duration::from_millis)
            .unwrap_or(self.task_poll_floor)
            .max(self.task_poll_floor);
        task.next_poll_at = deadline_after(now, task.poll_interval).min(task.expires_at);
        task.input_requests = input_requests;

        let revision = match next_revision(revision) {
            Ok(revision) => revision,
            Err(error) => {
                state.task_hints.unregister(&task.task_id);
                return Err(error);
            },
        };
        let Some(next_bytes) = state.accounted_bytes.checked_add(payload_bytes) else {
            state.task_hints.unregister(&task.task_id);
            return Err(McpClientError::ContinuationCapacityExceeded);
        };
        if next_bytes > self.max_accounted_bytes {
            state.task_hints.unregister(&task.task_id);
            return Err(McpClientError::ContinuationCapacityExceeded);
        }
        let expires_at = task.expires_at;
        state.accounted_bytes = next_bytes;
        state.entries.insert(
            claim.pending.id.sequence,
            ContinuationEntry {
                accounted_bytes: payload_bytes,
                state: ContinuationEntryState::Task { task, revision },
            },
        );
        Ok(McpTaskPollCommit::Pending(McpPendingCall {
            id: claim.pending.id,
            revision,
            kind: McpContinuationKind::RemoteTask,
            expires_at,
        }))
    }

    pub(crate) fn commit_task_update(
        &self,
        claim: McpClaimedTaskOperation,
    ) -> Result<McpPendingCall, McpClientError> {
        self.commit_task_ack(claim, McpTaskOperationKind::Update)
    }

    pub(crate) fn commit_task_cancel(
        &self,
        claim: McpClaimedTaskOperation,
    ) -> Result<McpPendingCall, McpClientError> {
        self.commit_task_ack(claim, McpTaskOperationKind::Cancel)
    }

    fn commit_task_ack(
        &self,
        mut claim: McpClaimedTaskOperation,
        expected_kind: McpTaskOperationKind,
    ) -> Result<McpPendingCall, McpClientError> {
        if expected_kind == McpTaskOperationKind::Poll
            || !claim.active
            || claim.client_instance_id != self.client_instance_id
            || claim.kind != expected_kind
        {
            return Err(McpClientError::ContinuationNotActive);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| McpClientError::ContinuationStateUnavailable)?;
        let now = Instant::now();
        prune_expired(&mut state, now);
        let entry = state
            .entries
            .get(&claim.pending.id.sequence)
            .ok_or(McpClientError::ContinuationNotActive)?;
        let exact_claim = matches!(
            &entry.state,
            ContinuationEntryState::TaskOperation {
                revision,
                claim_id,
                kind,
                ..
            } if *revision == claim.pending.revision
                && *claim_id == claim.claim_id
                && *kind == expected_kind
        );
        if !exact_claim {
            return Err(McpClientError::ContinuationNotActive);
        }
        let next_revision = next_revision(claim.pending.revision)?;
        let entry = state
            .entries
            .remove(&claim.pending.id.sequence)
            .ok_or(McpClientError::ContinuationStateUnavailable)?;
        claim.active = false;
        let accounted_bytes = entry.accounted_bytes;
        let ContinuationEntryState::TaskOperation {
            mut task,
            revision,
            claim_id,
            kind,
        } = entry.state
        else {
            return Err(McpClientError::ContinuationStateUnavailable);
        };
        if revision != claim.pending.revision || claim_id != claim.claim_id || kind != expected_kind
        {
            state.accounted_bytes = state.accounted_bytes.saturating_sub(accounted_bytes);
            state.task_hints.unregister(&task.task_id);
            return Err(McpClientError::ContinuationStateUnavailable);
        }
        match expected_kind {
            McpTaskOperationKind::Update => task.input_requests = None,
            McpTaskOperationKind::Cancel => task.cancel_requested = true,
            McpTaskOperationKind::Poll => {
                state.accounted_bytes = state.accounted_bytes.saturating_sub(accounted_bytes);
                state.task_hints.unregister(&task.task_id);
                return Err(McpClientError::ContinuationStateUnavailable);
            },
        }
        let revision = next_revision;
        let expires_at = task.expires_at;
        state.entries.insert(
            claim.pending.id.sequence,
            ContinuationEntry {
                accounted_bytes,
                state: ContinuationEntryState::Task { task, revision },
            },
        );
        Ok(McpPendingCall {
            id: claim.pending.id,
            revision,
            kind: McpContinuationKind::RemoteTask,
            expires_at,
        })
    }

    pub(crate) fn claim_input_responses(
        &self,
        prepared: McpPreparedMrtrResponses,
    ) -> Result<McpClaimedMrtrCall, McpClientError> {
        let pending = prepared.pending();
        if pending.id.client_instance_id != self.client_instance_id
            || pending.kind != McpContinuationKind::AdditionalInput
        {
            return Err(McpClientError::ContinuationNotActive);
        }

        let response_count = prepared.response_count();
        let prepared_bytes = prepared.accounted_bytes();
        let mut state = self
            .state
            .lock()
            .map_err(|_| McpClientError::ContinuationStateUnavailable)?;
        let now = Instant::now();
        prune_expired(&mut state, now);
        let entry = state
            .entries
            .get(&pending.id.sequence)
            .ok_or(McpClientError::ContinuationNotActive)?;
        if !entry.matches(pending) {
            return Err(McpClientError::ContinuationNotActive);
        }
        let claimed_accounted_bytes = entry
            .accounted_bytes
            .checked_add(prepared_bytes)
            .ok_or(McpClientError::ContinuationCapacityExceeded)?;
        let next_bytes = state
            .accounted_bytes
            .checked_add(prepared_bytes)
            .ok_or(McpClientError::ContinuationCapacityExceeded)?;
        if next_bytes > self.max_accounted_bytes {
            return Err(McpClientError::ContinuationCapacityExceeded);
        }
        let claim_sequence = NonZeroU64::new(state.next_claim_sequence)
            .ok_or(McpClientError::ContinuationIdentityExhausted)?;
        state.next_claim_sequence = state.next_claim_sequence.checked_add(1).unwrap_or(0);
        let claim_id = McpMrtrClaimId(claim_sequence);

        let entry = state
            .entries
            .remove(&pending.id.sequence)
            .ok_or(McpClientError::ContinuationStateUnavailable)?;
        let ContinuationEntry {
            accounted_bytes,
            state: entry_state,
        } = entry;
        let (tool, params, result, rounds, revision, expires_at) = match entry_state {
            ContinuationEntryState::InputRequired {
                tool,
                params,
                result,
                rounds,
                revision,
                expires_at,
            } => (tool, params, result, rounds, revision, expires_at),
            other => {
                state.entries.insert(
                    pending.id.sequence,
                    ContinuationEntry {
                        accounted_bytes,
                        state: other,
                    },
                );
                return Err(McpClientError::ContinuationStateUnavailable);
            },
        };
        state.accounted_bytes = next_bytes;
        state.entries.insert(
            pending.id.sequence,
            ContinuationEntry {
                accounted_bytes: claimed_accounted_bytes,
                state: ContinuationEntryState::ClaimedInput {
                    tool,
                    params,
                    result,
                    rounds,
                    revision,
                    expires_at,
                    prepared,
                    claim_id,
                },
            },
        );
        drop(state);

        Ok(McpClaimedMrtrCall {
            owner_state: Arc::downgrade(&self.state),
            client_instance_id: self.client_instance_id,
            pending,
            claim_id,
            response_count,
            active: true,
        })
    }

    pub(crate) fn begin_input_retry(
        &self,
        claim: &mut McpClaimedMrtrCall,
    ) -> Result<(McpContinuationRetry<'_>, CallToolRequestParams), McpClientError> {
        let pending = claim.pending;
        let owner_state = Arc::downgrade(&self.state);
        if !claim.active
            || claim.client_instance_id != self.client_instance_id
            || pending.id.client_instance_id != self.client_instance_id
            || pending.kind != McpContinuationKind::AdditionalInput
            || !Weak::ptr_eq(&claim.owner_state, &owner_state)
        {
            return Err(McpClientError::ContinuationNotActive);
        }

        let mut state = self
            .state
            .lock()
            .map_err(|_| McpClientError::ContinuationStateUnavailable)?;
        let now = Instant::now();
        prune_expired(&mut state, now);
        let entry = state
            .entries
            .get(&pending.id.sequence)
            .ok_or(McpClientError::ContinuationNotActive)?;
        let exact_claim = matches!(
            &entry.state,
            ContinuationEntryState::ClaimedInput {
                revision,
                expires_at,
                claim_id,
                ..
            } if *revision == pending.revision
                && *expires_at == pending.expires_at
                && *claim_id == claim.claim_id
        );
        if !exact_claim {
            return Err(McpClientError::ContinuationNotActive);
        }

        let entry = state
            .entries
            .remove(&pending.id.sequence)
            .ok_or(McpClientError::ContinuationStateUnavailable)?;
        let ContinuationEntry {
            accounted_bytes,
            state: entry_state,
        } = entry;
        let (tool, mut params, result, rounds, revision, expires_at, mut prepared, claim_id) =
            match entry_state {
                ContinuationEntryState::ClaimedInput {
                    tool,
                    params,
                    result,
                    rounds,
                    revision,
                    expires_at,
                    prepared,
                    claim_id,
                } => (
                    tool, params, result, rounds, revision, expires_at, prepared, claim_id,
                ),
                other => {
                    state.entries.insert(
                        pending.id.sequence,
                        ContinuationEntry {
                            accounted_bytes,
                            state: other,
                        },
                    );
                    return Err(McpClientError::ContinuationStateUnavailable);
                },
            };

        let Some(responses) = prepared.take_responses() else {
            claim.active = false;
            state.accounted_bytes = state.accounted_bytes.saturating_sub(accounted_bytes);
            return Err(McpClientError::ContinuationStateUnavailable);
        };
        let mut staged_responses = Some(responses);
        let previous_responses = params.input_responses.take();
        if staged_responses
            .as_ref()
            .is_some_and(|responses| !responses.is_empty())
        {
            params.input_responses = staged_responses.take();
        }
        let previous_request_state =
            std::mem::replace(&mut params.request_state, result.request_state.clone());

        let preflight = (|| {
            if params.name.as_ref() != tool.remote_name() {
                return Err(McpClientError::ContinuationStateUnavailable);
            }
            let request_bytes = measure_serialized_request(&params, self.limits.max_request_bytes)
                .map_err(|_| McpClientError::ContinuationResponseRejected)?;
            let reserved_bytes = request_bytes
                .checked_add(self.max_pending_payload_bytes)
                .ok_or(McpClientError::ContinuationCapacityExceeded)?;
            let base_accounted_bytes = state
                .accounted_bytes
                .checked_sub(accounted_bytes)
                .ok_or(McpClientError::ContinuationStateUnavailable)?;
            let next_accounted_bytes = base_accounted_bytes
                .checked_add(reserved_bytes)
                .ok_or(McpClientError::ContinuationCapacityExceeded)?;
            if next_accounted_bytes > self.max_accounted_bytes {
                return Err(McpClientError::ContinuationCapacityExceeded);
            }
            Ok((request_bytes, reserved_bytes, next_accounted_bytes))
        })();

        let (request_bytes, reserved_bytes, next_accounted_bytes) = match preflight {
            Ok(preflight) => preflight,
            Err(error) => {
                let Some(responses) = params
                    .input_responses
                    .take()
                    .or_else(|| staged_responses.take())
                else {
                    // This can only occur after internal ownership corruption. Do not
                    // panic or make the consumed claim replayable: release its retained
                    // accounting and fail closed.
                    claim.active = false;
                    state.accounted_bytes = state.accounted_bytes.saturating_sub(accounted_bytes);
                    return Err(McpClientError::ContinuationStateUnavailable);
                };
                prepared.restore_responses(responses);
                params.input_responses = previous_responses;
                params.request_state = previous_request_state;
                state.entries.insert(
                    pending.id.sequence,
                    ContinuationEntry {
                        accounted_bytes,
                        state: ContinuationEntryState::ClaimedInput {
                            tool,
                            params,
                            result,
                            rounds,
                            revision,
                            expires_at,
                            prepared,
                            claim_id,
                        },
                    },
                );
                return Err(error);
            },
        };

        claim.active = false;
        state.accounted_bytes = next_accounted_bytes;
        state.entries.insert(
            pending.id.sequence,
            ContinuationEntry {
                accounted_bytes: reserved_bytes,
                state: ContinuationEntryState::RetryingInput {
                    tool,
                    params: params.clone(),
                    request_bytes,
                    rounds,
                    previous_revision: revision,
                    reserved_at: now,
                },
            },
        );
        drop(state);
        if let Some(previous_responses) = previous_responses {
            drop_input_responses_iterative(previous_responses);
        }
        drop(previous_request_state);
        drop(staged_responses);
        drop(result);
        drop(prepared);

        Ok((
            McpContinuationRetry {
                owner: self,
                sequence: pending.id.sequence,
                previous_revision: revision,
                active: true,
            },
            params,
        ))
    }

    fn input_required_for(
        &self,
        pending: McpPendingCall,
    ) -> Result<Arc<InputRequiredResult>, McpClientError> {
        if pending.id.client_instance_id != self.client_instance_id {
            return Err(McpClientError::ContinuationNotActive);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| McpClientError::ContinuationStateUnavailable)?;
        let now = Instant::now();
        prune_expired(&mut state, now);
        let entry = state
            .entries
            .get(&pending.id.sequence)
            .ok_or(McpClientError::ContinuationNotActive)?;
        if !entry.matches(pending) {
            return Err(McpClientError::ContinuationNotActive);
        }
        match &entry.state {
            ContinuationEntryState::InputRequired { result, .. } => Ok(Arc::clone(result)),
            ContinuationEntryState::Task { .. } => Err(McpClientError::ContinuationKindMismatch),
            ContinuationEntryState::InFlight { .. }
            | ContinuationEntryState::ClaimedInput { .. }
            | ContinuationEntryState::RetryingInput { .. }
            | ContinuationEntryState::TaskOperation { .. } => {
                Err(McpClientError::ContinuationNotActive)
            },
        }
    }

    fn finish_in_flight(&self, sequence: NonZeroU64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        remove_entry(&mut state, sequence);
    }

    fn commit(
        &self,
        sequence: NonZeroU64,
        pending: ValidatedPending,
    ) -> Result<McpPendingCall, McpClientError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| McpClientError::ContinuationStateUnavailable)?;
        let entry = state
            .entries
            .remove(&sequence)
            .ok_or(McpClientError::ContinuationStateUnavailable)?;
        state.accounted_bytes = state.accounted_bytes.saturating_sub(entry.accounted_bytes);
        let ContinuationEntryState::InFlight {
            tool,
            params,
            request_bytes,
            reserved_at,
        } = entry.state
        else {
            return Err(McpClientError::ContinuationStateUnavailable);
        };

        let revision = McpContinuationRevision(NonZeroU64::MIN);
        let now = Instant::now();
        let (kind, expires_at, accounted_bytes, state_value) = match pending {
            ValidatedPending::InputRequired {
                result,
                payload_bytes,
            } => {
                let rounds =
                    advance_mrtr_rounds(McpMrtrRoundCounts::default(), &result, &self.limits)?;
                let accounted_bytes = request_bytes
                    .checked_add(payload_bytes)
                    .ok_or(McpClientError::ContinuationCapacityExceeded)?;
                let expires_at = deadline_after(now, self.continuation_timeout);
                (
                    McpContinuationKind::AdditionalInput,
                    expires_at,
                    accounted_bytes,
                    ContinuationEntryState::InputRequired {
                        tool,
                        params,
                        result: Arc::new(result),
                        rounds,
                        revision,
                        expires_at,
                    },
                )
            },
            ValidatedPending::Task {
                result,
                payload_bytes,
            } => {
                let task = retained_task_from_seed(
                    tool,
                    result,
                    reserved_at,
                    self.continuation_timeout,
                    self.task_poll_floor,
                )?;
                let expires_at = task.expires_at;
                (
                    McpContinuationKind::RemoteTask,
                    expires_at,
                    payload_bytes,
                    ContinuationEntryState::Task { task, revision },
                )
            },
        };
        let next_bytes = state
            .accounted_bytes
            .checked_add(accounted_bytes)
            .ok_or(McpClientError::ContinuationCapacityExceeded)?;
        if next_bytes > self.max_accounted_bytes {
            return Err(McpClientError::ContinuationCapacityExceeded);
        }
        if let ContinuationEntryState::Task { task, .. } = &state_value {
            state.task_hints.register(&task.task_id)?;
        }
        state.accounted_bytes = next_bytes;
        state.entries.insert(
            sequence,
            ContinuationEntry {
                accounted_bytes,
                state: state_value,
            },
        );
        Ok(McpPendingCall {
            id: McpContinuationId {
                client_instance_id: self.client_instance_id,
                sequence,
            },
            revision,
            kind,
            expires_at,
        })
    }

    fn finish_retry(&self, sequence: NonZeroU64, previous_revision: McpContinuationRevision) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let is_exact_retry = state.entries.get(&sequence).is_some_and(|entry| {
            matches!(
                &entry.state,
                ContinuationEntryState::RetryingInput {
                    previous_revision: active_revision,
                    ..
                } if *active_revision == previous_revision
            )
        });
        if is_exact_retry {
            remove_entry(&mut state, sequence);
        }
    }

    fn commit_retry(
        &self,
        sequence: NonZeroU64,
        previous_revision: McpContinuationRevision,
        pending: ValidatedPending,
    ) -> Result<McpPendingCall, McpClientError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| McpClientError::ContinuationStateUnavailable)?;
        let exact_retry = state.entries.get(&sequence).is_some_and(|entry| {
            matches!(
                &entry.state,
                ContinuationEntryState::RetryingInput {
                    previous_revision: active_revision,
                    ..
                } if *active_revision == previous_revision
            )
        });
        if !exact_retry {
            return Err(McpClientError::ContinuationStateUnavailable);
        }
        let entry = state
            .entries
            .remove(&sequence)
            .ok_or(McpClientError::ContinuationStateUnavailable)?;
        let Some(base_accounted_bytes) = state.accounted_bytes.checked_sub(entry.accounted_bytes)
        else {
            state.entries.insert(sequence, entry);
            return Err(McpClientError::ContinuationStateUnavailable);
        };
        let ContinuationEntryState::RetryingInput {
            tool,
            params,
            request_bytes,
            rounds,
            previous_revision: active_revision,
            reserved_at,
        } = entry.state
        else {
            return Err(McpClientError::ContinuationStateUnavailable);
        };
        if active_revision != previous_revision {
            return Err(McpClientError::ContinuationStateUnavailable);
        }
        state.accounted_bytes = base_accounted_bytes;

        let revision_value = previous_revision
            .0
            .get()
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .ok_or(McpClientError::ContinuationIdentityExhausted)?;
        let revision = McpContinuationRevision(revision_value);
        let now = Instant::now();
        let (kind, expires_at, accounted_bytes, state_value) = match pending {
            ValidatedPending::InputRequired {
                result,
                payload_bytes,
            } => {
                let rounds = advance_mrtr_rounds(rounds, &result, &self.limits)?;
                let accounted_bytes = request_bytes
                    .checked_add(payload_bytes)
                    .ok_or(McpClientError::ContinuationCapacityExceeded)?;
                let expires_at = deadline_after(now, self.continuation_timeout);
                (
                    McpContinuationKind::AdditionalInput,
                    expires_at,
                    accounted_bytes,
                    ContinuationEntryState::InputRequired {
                        tool,
                        params,
                        result: Arc::new(result),
                        rounds,
                        revision,
                        expires_at,
                    },
                )
            },
            ValidatedPending::Task {
                result,
                payload_bytes,
            } => {
                let task = retained_task_from_seed(
                    tool,
                    result,
                    reserved_at,
                    self.continuation_timeout,
                    self.task_poll_floor,
                )?;
                let expires_at = task.expires_at;
                (
                    McpContinuationKind::RemoteTask,
                    expires_at,
                    payload_bytes,
                    ContinuationEntryState::Task { task, revision },
                )
            },
        };
        let next_accounted_bytes = state
            .accounted_bytes
            .checked_add(accounted_bytes)
            .ok_or(McpClientError::ContinuationCapacityExceeded)?;
        if next_accounted_bytes > self.max_accounted_bytes {
            return Err(McpClientError::ContinuationCapacityExceeded);
        }
        if let ContinuationEntryState::Task { task, .. } = &state_value {
            state.task_hints.register(&task.task_id)?;
        }
        state.accounted_bytes = next_accounted_bytes;
        state.entries.insert(
            sequence,
            ContinuationEntry {
                accounted_bytes,
                state: state_value,
            },
        );
        Ok(McpPendingCall {
            id: McpContinuationId {
                client_instance_id: self.client_instance_id,
                sequence,
            },
            revision,
            kind,
            expires_at,
        })
    }
}

pub(crate) enum ValidatedPending {
    InputRequired {
        result: InputRequiredResult,
        payload_bytes: usize,
    },
    Task {
        result: CreateTaskResult,
        payload_bytes: usize,
    },
}

impl ValidatedPending {
    pub(crate) fn kind(&self) -> McpContinuationKind {
        match self {
            Self::InputRequired { .. } => McpContinuationKind::AdditionalInput,
            Self::Task { .. } => McpContinuationKind::RemoteTask,
        }
    }
}

pub(crate) struct McpContinuationReservation<'a> {
    owner: &'a McpContinuationOwner,
    sequence: NonZeroU64,
    active: bool,
}

impl McpContinuationReservation<'_> {
    pub(crate) fn complete(mut self) {
        self.owner.finish_in_flight(self.sequence);
        self.active = false;
    }

    pub(crate) fn commit(
        mut self,
        pending: ValidatedPending,
    ) -> Result<McpPendingCall, McpClientError> {
        let result = self.owner.commit(self.sequence, pending);
        if result.is_ok() {
            self.active = false;
        }
        result
    }
}

impl Drop for McpContinuationReservation<'_> {
    fn drop(&mut self) {
        if self.active {
            self.owner.finish_in_flight(self.sequence);
        }
    }
}

pub(crate) struct McpContinuationRetry<'a> {
    owner: &'a McpContinuationOwner,
    sequence: NonZeroU64,
    previous_revision: McpContinuationRevision,
    active: bool,
}

impl McpContinuationRetry<'_> {
    pub(crate) fn complete(mut self) {
        self.owner
            .finish_retry(self.sequence, self.previous_revision);
        self.active = false;
    }

    pub(crate) fn commit(
        mut self,
        pending: ValidatedPending,
    ) -> Result<McpPendingCall, McpClientError> {
        let result = self
            .owner
            .commit_retry(self.sequence, self.previous_revision, pending);
        if result.is_ok() {
            self.active = false;
        }
        result
    }
}

impl Drop for McpContinuationRetry<'_> {
    fn drop(&mut self) {
        if self.active {
            self.owner
                .finish_retry(self.sequence, self.previous_revision);
        }
    }
}

fn release_input_claim(
    owner_state: &Mutex<ContinuationState>,
    client_instance_id: u64,
    pending: McpPendingCall,
    claim_id: McpMrtrClaimId,
) {
    if pending.id.client_instance_id != client_instance_id {
        return;
    }
    let Ok(mut state) = owner_state.lock() else {
        return;
    };
    let now = Instant::now();
    let Some(entry) = state.entries.remove(&pending.id.sequence) else {
        return;
    };
    let ContinuationEntry {
        accounted_bytes,
        state: entry_state,
    } = entry;
    let (tool, params, result, rounds, revision, expires_at, prepared, active_claim_id) =
        match entry_state {
            ContinuationEntryState::ClaimedInput {
                tool,
                params,
                result,
                rounds,
                revision,
                expires_at,
                prepared,
                claim_id: active_claim_id,
            } => (
                tool,
                params,
                result,
                rounds,
                revision,
                expires_at,
                prepared,
                active_claim_id,
            ),
            other => {
                state.entries.insert(
                    pending.id.sequence,
                    ContinuationEntry {
                        accounted_bytes,
                        state: other,
                    },
                );
                return;
            },
        };
    if active_claim_id != claim_id
        || revision != pending.revision
        || expires_at != pending.expires_at
    {
        state.entries.insert(
            pending.id.sequence,
            ContinuationEntry {
                accounted_bytes,
                state: ContinuationEntryState::ClaimedInput {
                    tool,
                    params,
                    result,
                    rounds,
                    revision,
                    expires_at,
                    prepared,
                    claim_id: active_claim_id,
                },
            },
        );
        return;
    }

    let prepared_bytes = prepared.accounted_bytes();
    let Some(restored_bytes) = accounted_bytes.checked_sub(prepared_bytes) else {
        state.entries.insert(
            pending.id.sequence,
            ContinuationEntry {
                accounted_bytes,
                state: ContinuationEntryState::ClaimedInput {
                    tool,
                    params,
                    result,
                    rounds,
                    revision,
                    expires_at,
                    prepared,
                    claim_id: active_claim_id,
                },
            },
        );
        return;
    };
    let Some(base_accounted_bytes) = state.accounted_bytes.checked_sub(accounted_bytes) else {
        state.entries.insert(
            pending.id.sequence,
            ContinuationEntry {
                accounted_bytes,
                state: ContinuationEntryState::ClaimedInput {
                    tool,
                    params,
                    result,
                    rounds,
                    revision,
                    expires_at,
                    prepared,
                    claim_id: active_claim_id,
                },
            },
        );
        return;
    };
    state.accounted_bytes = base_accounted_bytes;
    if expires_at <= now {
        drop(state);
        drop(prepared);
        return;
    }

    state.accounted_bytes = base_accounted_bytes + restored_bytes;
    state.entries.insert(
        pending.id.sequence,
        ContinuationEntry {
            accounted_bytes: restored_bytes,
            state: ContinuationEntryState::InputRequired {
                tool,
                params,
                result,
                rounds,
                revision,
                expires_at,
            },
        },
    );
    drop(state);
    drop(prepared);
}

fn release_task_operation(
    owner_state: &Mutex<ContinuationState>,
    client_instance_id: u64,
    pending: McpPendingCall,
    claim_id: McpMrtrClaimId,
    kind: McpTaskOperationKind,
    restore_hint: bool,
    dispatched: bool,
) {
    if pending.id.client_instance_id != client_instance_id {
        return;
    }
    let Ok(mut state) = owner_state.lock() else {
        return;
    };
    let now = Instant::now();
    let Some(entry) = state.entries.remove(&pending.id.sequence) else {
        return;
    };
    let ContinuationEntry {
        accounted_bytes,
        state: entry_state,
    } = entry;
    let (mut task, revision, active_claim_id, active_kind) = match entry_state {
        ContinuationEntryState::TaskOperation {
            task,
            revision,
            claim_id,
            kind,
        } => (task, revision, claim_id, kind),
        other => {
            state.entries.insert(
                pending.id.sequence,
                ContinuationEntry {
                    accounted_bytes,
                    state: other,
                },
            );
            return;
        },
    };
    if revision != pending.revision || active_claim_id != claim_id || active_kind != kind {
        state.entries.insert(
            pending.id.sequence,
            ContinuationEntry {
                accounted_bytes,
                state: ContinuationEntryState::TaskOperation {
                    task,
                    revision,
                    claim_id: active_claim_id,
                    kind: active_kind,
                },
            },
        );
        return;
    }
    if task.expires_at <= now {
        state.accounted_bytes = state.accounted_bytes.saturating_sub(accounted_bytes);
        state.task_hints.unregister(&task.task_id);
        return;
    }
    if restore_hint {
        state.task_hints.mark(&task.task_id);
    }
    if dispatched {
        match kind {
            McpTaskOperationKind::Update => task.input_requests = None,
            McpTaskOperationKind::Cancel => task.cancel_requested = true,
            McpTaskOperationKind::Poll => {},
        }
    }
    state.entries.insert(
        pending.id.sequence,
        ContinuationEntry {
            accounted_bytes,
            state: ContinuationEntryState::Task { task, revision },
        },
    );
}

fn retained_task_from_seed(
    tool: McpToolId,
    result: CreateTaskResult,
    started_at: Instant,
    continuation_timeout: Duration,
    task_poll_floor: Duration,
) -> Result<RetainedTask, McpClientError> {
    let now = Instant::now();
    let started_at_epoch_millis = now_epoch_millis()?.saturating_sub(
        u64::try_from(now.saturating_duration_since(started_at).as_millis()).unwrap_or(u64::MAX),
    );
    let lifetime_expires_at = deadline_after(started_at, continuation_timeout);
    let ttl_expires_at = result
        .task
        .ttl_ms
        .map(Duration::from_millis)
        .map(|ttl| deadline_after(started_at, ttl))
        .unwrap_or(lifetime_expires_at);
    let expires_at = ttl_expires_at.min(lifetime_expires_at);
    if expires_at <= now {
        return Err(McpClientError::ContinuationNotActive);
    }
    let poll_interval = result
        .task
        .poll_interval_ms
        .map(Duration::from_millis)
        .unwrap_or(task_poll_floor)
        .max(task_poll_floor);
    Ok(RetainedTask {
        tool,
        task_id: result.task.task_id,
        created_at: result.task.created_at,
        started_at: Some(started_at),
        started_at_epoch_millis,
        lifetime_expires_at,
        expires_at,
        poll_interval,
        next_poll_at: deadline_after(now, poll_interval).min(expires_at),
        input_requests: None,
        operation_count: 0,
        cancel_requested: false,
    })
}

fn now_epoch_millis() -> Result<u64, McpClientError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| McpClientError::TaskRecoveryRecordRejected)
        .and_then(|duration| {
            u64::try_from(duration.as_millis())
                .map_err(|_| McpClientError::TaskRecoveryRecordRejected)
        })
}

fn epoch_deadline(now_epoch_millis: u64, remaining: Duration) -> u64 {
    now_epoch_millis.saturating_add(u64::try_from(remaining.as_millis()).unwrap_or(u64::MAX))
}

fn instant_from_epoch_deadline(
    now: Instant,
    now_epoch_millis: u64,
    deadline_epoch_millis: u64,
) -> Result<Instant, McpClientError> {
    if deadline_epoch_millis <= now_epoch_millis {
        return Err(McpClientError::ContinuationNotActive);
    }
    Ok(deadline_after(
        now,
        Duration::from_millis(deadline_epoch_millis - now_epoch_millis),
    ))
}

fn epoch_task_deadline(
    now: Instant,
    now_epoch_millis: u64,
    started_at_epoch_millis: u64,
    ttl: Duration,
) -> Result<Instant, McpClientError> {
    let ttl_millis =
        u64::try_from(ttl.as_millis()).map_err(|_| McpClientError::TaskRecoveryRecordRejected)?;
    let deadline = started_at_epoch_millis.saturating_add(ttl_millis);
    instant_from_epoch_deadline(now, now_epoch_millis, deadline)
}

fn next_revision(
    revision: McpContinuationRevision,
) -> Result<McpContinuationRevision, McpClientError> {
    revision
        .0
        .get()
        .checked_add(1)
        .and_then(NonZeroU64::new)
        .map(McpContinuationRevision)
        .ok_or(McpClientError::ContinuationIdentityExhausted)
}

fn deadline_after(now: Instant, timeout: Duration) -> Instant {
    now.checked_add(timeout).unwrap_or(now)
}

fn advance_mrtr_rounds(
    current: McpMrtrRoundCounts,
    result: &InputRequiredResult,
    limits: &McpClientLimits,
) -> Result<McpMrtrRoundCounts, McpClientError> {
    let interactive = result
        .input_requests
        .as_ref()
        .is_some_and(|requests| !requests.is_empty());
    let next = if interactive {
        McpMrtrRoundCounts {
            interactive: current
                .interactive
                .checked_add(1)
                .ok_or(McpClientError::ContinuationRoundLimitExceeded)?,
            ..current
        }
    } else {
        McpMrtrRoundCounts {
            state_only: current
                .state_only
                .checked_add(1)
                .ok_or(McpClientError::ContinuationRoundLimitExceeded)?,
            ..current
        }
    };
    if next.state_only > limits.max_mrtr_state_only_rounds
        || next.interactive > limits.max_mrtr_interactive_rounds
    {
        return Err(McpClientError::ContinuationRoundLimitExceeded);
    }
    Ok(next)
}

fn prune_expired(state: &mut ContinuationState, now: Instant) {
    let expired = state
        .entries
        .iter()
        .filter_map(|(sequence, entry)| entry.expired(now).then_some(*sequence))
        .collect::<Vec<_>>();
    for sequence in expired {
        remove_entry(state, sequence);
    }
}

fn remove_entry(state: &mut ContinuationState, sequence: NonZeroU64) {
    if let Some(entry) = state.entries.remove(&sequence) {
        state.accounted_bytes = state.accounted_bytes.saturating_sub(entry.accounted_bytes);
        match &entry.state {
            ContinuationEntryState::Task { task, .. }
            | ContinuationEntryState::TaskOperation { task, .. } => {
                state.task_hints.unregister(&task.task_id);
            },
            _ => {},
        }
    }
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{Arc, Barrier},
    };

    use rmcp::model::{
        DetailedTask, GetTaskResult, InputRequest, ListRootsRequest, ListRootsResult, Root, Task,
        TaskPayload, TaskStatus,
    };
    use serde_json::Map;
    use static_assertions::assert_not_impl_any;

    use super::*;

    assert_not_impl_any!(McpPendingCall: serde::Serialize, serde::de::DeserializeOwned);
    assert_not_impl_any!(
        McpClaimedMrtrCall: Clone,
        serde::Serialize,
        serde::de::DeserializeOwned
    );

    fn limits(max_pending_continuations: usize) -> McpClientLimits {
        McpClientLimits {
            max_pending_continuations,
            max_continuation_bytes: 4_096,
            max_result_bytes: 1_024,
            ..McpClientLimits::default()
        }
    }

    #[test]
    fn process_continuation_budget_is_shared_bounded_and_released() {
        let budget = Arc::new(ProcessContinuationBudget {
            state: Mutex::new(ProcessContinuationBudgetState::default()),
        });
        let first = budget
            .reserve(MAX_PROCESS_CONTINUATION_BYTES - 1, 1)
            .expect("first reservation");
        assert!(matches!(
            budget.reserve(2, 1),
            Err(McpClientError::ContinuationCapacityExceeded)
        ));
        drop(first);
        let full = budget
            .reserve(
                MAX_PROCESS_CONTINUATION_BYTES,
                MAX_PROCESS_CONTINUATION_ENTRIES,
            )
            .expect("released capacity");
        assert!(matches!(
            budget.reserve(1, 1),
            Err(McpClientError::ContinuationCapacityExceeded)
        ));
        drop(full);
    }

    fn tool(client: u64, generation: u64) -> McpToolId {
        McpToolId::new("canary-tool".to_owned(), client, generation)
    }

    fn params() -> CallToolRequestParams {
        CallToolRequestParams::new("canary-tool").with_arguments(Map::new())
    }

    fn input_required(value: &str) -> ValidatedPending {
        ValidatedPending::InputRequired {
            result: InputRequiredResult::from_request_state(value),
            payload_bytes: value.len(),
        }
    }

    fn prepared_state_only(
        owner: &McpContinuationOwner,
        pending: McpPendingCall,
    ) -> McpPreparedMrtrResponses {
        owner.prepare_input_responses(pending, Vec::new()).unwrap()
    }

    fn retained_task(owner: &McpContinuationOwner, client: u64) -> McpPendingCall {
        owner
            .reserve(tool(client, 1), params(), 16)
            .unwrap()
            .commit(ValidatedPending::Task {
                result: CreateTaskResult::new(
                    Task::new(
                        "private-task",
                        TaskStatus::Working,
                        "2026-08-07T00:00:00Z",
                        "2026-08-07T00:00:01Z",
                    )
                    .with_ttl_ms(60_000)
                    .with_poll_interval_ms(1),
                ),
                payload_bytes: 64,
            })
            .unwrap()
    }

    fn hint_retained_task(owner: &McpContinuationOwner) {
        let hints = {
            let state = owner.state.lock().unwrap();
            Arc::clone(&state.task_hints)
        };
        hints.mark("private-task");
    }

    #[test]
    fn dropped_in_flight_reservations_release_capacity() {
        let owner = McpContinuationOwner::new(7, &limits(1), Duration::from_secs(60));
        let reservation = owner.reserve(tool(7, 1), params(), 16).unwrap();
        assert!(matches!(
            owner.reserve(tool(7, 1), params(), 16),
            Err(McpClientError::ContinuationCapacityExceeded)
        ));
        drop(reservation);
        assert!(owner.reserve(tool(7, 1), params(), 16).is_ok());
    }

    #[test]
    fn committed_input_is_payload_free_and_exact_client_bound() {
        let owner = McpContinuationOwner::new(7, &limits(2), Duration::from_secs(60));
        let pending = owner
            .reserve(tool(7, 3), params(), 16)
            .unwrap()
            .commit(input_required("canary-secret-state"))
            .unwrap();
        assert_eq!(pending.kind(), McpContinuationKind::AdditionalInput);
        assert_eq!(pending.revision().get(), 1);
        assert!(owner.is_active(pending).unwrap());
        assert_eq!(owner.pending_count().unwrap(), 1);
        let rendered = format!("{pending:?}");
        assert!(!rendered.contains("canary-secret-state"));
        assert!(!rendered.contains("canary-tool"));

        let other = McpContinuationOwner::new(8, &limits(2), Duration::from_secs(60));
        assert!(!other.is_active(pending).unwrap());
    }

    #[test]
    fn task_deadline_cannot_outlive_server_ttl() {
        let owner = McpContinuationOwner::new(9, &limits(2), Duration::from_secs(60));
        let task = Task::new("canary-task", TaskStatus::Working, "now", "now").with_ttl_ms(5);
        let pending = owner
            .reserve(tool(9, 1), params(), 16)
            .unwrap()
            .commit(ValidatedPending::Task {
                result: CreateTaskResult::new(task),
                payload_bytes: 64,
            })
            .unwrap();
        assert_eq!(pending.kind(), McpContinuationKind::RemoteTask);
        assert!(pending.remaining() <= Duration::from_millis(5));
        assert!(matches!(
            owner.input_slots(pending),
            Err(McpClientError::ContinuationKindMismatch)
        ));
    }

    #[test]
    fn recovered_task_ttl_remains_anchored_to_original_start() {
        let limits = limits(2);
        let owner = McpContinuationOwner::new_unbudgeted(
            91,
            &limits,
            Duration::from_secs(10),
            Duration::from_millis(1),
            Arc::new(McpTaskNotificationHints::new(2)),
        );
        let now = now_epoch_millis().unwrap();
        let record = McpTaskRecoveryRecord {
            schema_version: 1,
            binding_digest: [0; 32],
            transport: "streamable_http".to_owned(),
            protocol_version: "2026-07-28".to_owned(),
            server_name: Some("fixture".to_owned()),
            server_version: Some("1".to_owned()),
            tool_name: "canary-tool".to_owned(),
            task_id: "recovered-private-task".to_owned(),
            created_at: "2026-08-07T00:00:00Z".to_owned(),
            started_at_epoch_millis: now.saturating_sub(500),
            lifetime_expires_at_epoch_millis: now.saturating_add(10_000),
            expires_at_epoch_millis: now.saturating_add(500),
            operation_count: 1,
            cancel_requested: false,
            revision: 2,
        };
        let status = || {
            GetTaskResult::new(DetailedTask::new(
                Task::new(
                    "recovered-private-task",
                    TaskStatus::Working,
                    "2026-08-07T00:00:00Z",
                    "2026-08-07T00:00:01Z",
                )
                .with_ttl_ms(1_000)
                .with_poll_interval_ms(1),
                TaskPayload::Working,
            ))
        };
        let pending = match owner
            .recover_task(record, tool(91, 1), status(), 64)
            .unwrap()
        {
            McpTaskPollCommit::Pending(pending) => pending,
            McpTaskPollCommit::Terminal(_) => panic!("expected pending recovered task"),
        };
        std::thread::sleep(Duration::from_millis(2));
        let (claim, _) = owner.begin_task_poll(pending).unwrap();
        let pending = match owner.commit_task_poll(claim, status(), 64).unwrap() {
            McpTaskPollCommit::Pending(pending) => pending,
            McpTaskPollCommit::Terminal(_) => panic!("expected pending recovered task"),
        };
        assert!(pending.remaining() <= Duration::from_millis(500));
    }

    #[test]
    fn task_poll_rejects_changed_remote_identity_and_releases_authority() {
        let owner = McpContinuationOwner::new(34, &limits(2), Duration::from_secs(60));
        let pending = retained_task(&owner, 34);
        hint_retained_task(&owner);
        let (claim, task_id) = owner.begin_task_poll(pending).unwrap();
        assert_eq!(task_id, "private-task");
        let result = GetTaskResult::new(DetailedTask::new(
            Task::new(
                "replacement-task",
                TaskStatus::Working,
                "2026-08-07T00:00:00Z",
                "2026-08-07T00:00:02Z",
            ),
            TaskPayload::Working,
        ));
        assert!(matches!(
            owner.commit_task_poll(claim, result, 64),
            Err(McpClientError::ResponseRejected(_))
        ));
        assert!(!owner.is_active(pending).unwrap());
        assert_eq!(owner.pending_count().unwrap(), 0);
    }

    #[test]
    fn task_poll_accounting_overflow_releases_private_task_hint() {
        let owner = McpContinuationOwner::new(341, &limits(3), Duration::from_secs(60));
        let pending = retained_task(&owner, 341);
        let _other = owner
            .reserve(tool(341, 1), params(), 16)
            .unwrap()
            .commit(input_required("other-retained-state"))
            .unwrap();
        hint_retained_task(&owner);
        let hints = {
            let state = owner.state.lock().unwrap();
            Arc::clone(&state.task_hints)
        };
        let (claim, _) = owner.begin_task_poll(pending).unwrap();
        let result = GetTaskResult::new(DetailedTask::new(
            Task::new(
                "private-task",
                TaskStatus::Working,
                "2026-08-07T00:00:00Z",
                "2026-08-07T00:00:02Z",
            )
            .with_ttl_ms(60_000)
            .with_poll_interval_ms(1),
            TaskPayload::Working,
        ));

        assert!(matches!(
            owner.commit_task_poll(claim, result, usize::MAX),
            Err(McpClientError::ContinuationCapacityExceeded)
        ));
        assert!(matches!(
            hints.contains_hint("private-task"),
            Err(McpClientError::ContinuationStateUnavailable)
        ));
        hints
            .register("private-task")
            .expect("failed poll must release the exact task hint");
    }

    #[test]
    fn dispatched_task_update_failure_cannot_replay_the_same_inputs() {
        let owner = McpContinuationOwner::new(35, &limits(2), Duration::from_secs(60));
        let pending = retained_task(&owner, 35);
        hint_retained_task(&owner);
        let (claim, _) = owner.begin_task_poll(pending).unwrap();
        let requests = BTreeMap::from([(
            "private-root".to_owned(),
            InputRequest::ListRoots(ListRootsRequest::default()),
        )]);
        let result = GetTaskResult::new(DetailedTask::new(
            Task::new(
                "private-task",
                TaskStatus::InputRequired,
                "2026-08-07T00:00:00Z",
                "2026-08-07T00:00:02Z",
            )
            .with_ttl_ms(60_000)
            .with_poll_interval_ms(1),
            TaskPayload::InputRequired {
                input_requests: requests,
            },
        ));
        let pending = match owner.commit_task_poll(claim, result, 128).unwrap() {
            McpTaskPollCommit::Pending(pending) => pending,
            McpTaskPollCommit::Terminal(_) => panic!("expected input-required task"),
        };
        let slots = owner.task_input_slots(pending).unwrap();
        let value = serde_json::to_value(ListRootsResult::new(vec![Root::new(
            "file:///governed/root",
        )]))
        .unwrap();
        let prepared = owner
            .prepare_task_responses(pending, vec![McpMrtrResponse::new(slots[0].id(), value)])
            .unwrap();
        let (mut claim, _, responses) = owner.begin_task_update(prepared).unwrap();
        assert_eq!(responses.len(), 1);
        claim.mark_dispatched();
        drop(responses);
        drop(claim);

        assert!(owner.is_active(pending).unwrap());
        assert_eq!(
            owner.task_progress(pending).unwrap().state(),
            McpTaskState::Working
        );
        assert!(matches!(
            owner.task_input_slots(pending),
            Err(McpClientError::TaskInputNotRequired)
        ));
    }

    #[test]
    fn task_operation_limit_counts_iteratively_without_recursion() {
        let mut constrained = limits(2);
        constrained.max_task_operations = 10_000;
        std::thread::Builder::new()
            .stack_size(64 * 1024)
            .spawn(move || {
                let owner = McpContinuationOwner::new(36, &constrained, Duration::from_secs(60));
                let pending = retained_task(&owner, 36);
                for _ in 0..constrained.max_task_operations {
                    hint_retained_task(&owner);
                    let (claim, _) = owner.begin_task_poll(pending).unwrap();
                    drop(claim);
                }
                hint_retained_task(&owner);
                assert!(matches!(
                    owner.begin_task_poll(pending),
                    Err(McpClientError::TaskOperationLimitExceeded)
                ));
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn expired_entries_are_pruned_before_capacity_is_checked() {
        let owner = McpContinuationOwner::new(10, &limits(1), Duration::from_millis(1));
        let pending = owner
            .reserve(tool(10, 1), params(), 16)
            .unwrap()
            .commit(input_required("state"))
            .unwrap();
        std::thread::sleep(Duration::from_millis(3));
        assert!(!owner.is_active(pending).unwrap());
        assert_eq!(owner.pending_count().unwrap(), 0);
        assert!(owner.reserve(tool(10, 2), params(), 16).is_ok());
    }

    #[test]
    fn pending_handles_are_copyable_but_carry_no_sdk_or_json_payload() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<McpPendingCall>();
        assert!(std::mem::size_of::<McpPendingCall>() <= 64);
    }

    #[test]
    fn poisoned_owner_never_projects_empty_or_inactive_state_as_success() {
        let owner = McpContinuationOwner::new(11, &limits(2), Duration::from_secs(60));
        let pending = owner
            .reserve(tool(11, 1), params(), 16)
            .unwrap()
            .commit(input_required("state"))
            .unwrap();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = owner.state.lock().unwrap();
            panic!("poison continuation fixture");
        }));

        assert!(matches!(
            owner.is_active(pending),
            Err(McpClientError::ContinuationStateUnavailable)
        ));
        assert!(matches!(
            owner.pending_count(),
            Err(McpClientError::ContinuationStateUnavailable)
        ));
        assert!(matches!(
            owner.reserve(tool(11, 1), params(), 16),
            Err(McpClientError::ContinuationStateUnavailable)
        ));
        assert!(matches!(
            owner.input_slots(pending),
            Err(McpClientError::ContinuationStateUnavailable)
        ));
        assert!(matches!(
            owner.prepare_input_responses(pending, Vec::new()),
            Err(McpClientError::ContinuationStateUnavailable)
        ));
    }

    #[test]
    fn state_only_input_round_is_prepared_without_exposing_request_state() {
        let owner = McpContinuationOwner::new(13, &limits(2), Duration::from_secs(60));
        let pending = owner
            .reserve(tool(13, 1), params(), 16)
            .unwrap()
            .commit(input_required("canary-private-state"))
            .unwrap();
        assert!(owner.input_slots(pending).unwrap().is_empty());
        let prepared = owner.prepare_input_responses(pending, Vec::new()).unwrap();
        assert_eq!(prepared.pending(), pending);
        assert_eq!(prepared.response_count(), 0);
        assert!(!format!("{prepared:?}").contains("canary-private-state"));
        assert!(owner.is_active(pending).unwrap());
    }

    #[test]
    fn claim_is_exclusive_payload_free_and_drop_restores_the_exact_revision() {
        let owner = McpContinuationOwner::new(14, &limits(2), Duration::from_secs(60));
        let pending = owner
            .reserve(tool(14, 1), params(), 16)
            .unwrap()
            .commit(input_required("canary-private-state"))
            .unwrap();
        let prepared = prepared_state_only(&owner, pending);
        let claim = owner.claim_input_responses(prepared).unwrap();

        assert_eq!(claim.pending(), pending);
        assert_eq!(claim.response_count(), 0);
        assert!(!owner.is_active(pending).unwrap());
        assert_eq!(owner.pending_count().unwrap(), 1);
        assert!(matches!(
            owner.prepare_input_responses(pending, Vec::new()),
            Err(McpClientError::ContinuationNotActive)
        ));
        let rendered = format!("{claim:?}");
        assert!(!rendered.contains("canary-private-state"));
        assert!(!rendered.contains("canary-tool"));

        drop(claim);
        assert!(owner.is_active(pending).unwrap());
        assert_eq!(owner.input_slots(pending).unwrap().len(), 0);
    }

    #[test]
    fn one_retry_echoes_state_and_commits_one_new_revision_without_network() {
        let owner = McpContinuationOwner::new(26, &limits(2), Duration::from_secs(60));
        let pending = owner
            .reserve(tool(26, 1), params(), 32)
            .unwrap()
            .commit(input_required("canary-exact-state-one"))
            .unwrap();
        let mut claim = owner
            .claim_input_responses(prepared_state_only(&owner, pending))
            .unwrap();
        let (retry, retry_params) = owner.begin_input_retry(&mut claim).unwrap();

        assert_eq!(
            retry_params.request_state.as_deref(),
            Some("canary-exact-state-one")
        );
        assert!(retry_params.input_responses.is_none());
        assert!(!owner.is_active(pending).unwrap());
        assert_eq!(owner.pending_count().unwrap(), 1);

        let next = retry
            .commit(input_required("canary-exact-state-two"))
            .unwrap();
        assert_eq!(next.revision().get(), 2);
        assert!(!owner.is_active(pending).unwrap());
        assert!(owner.is_active(next).unwrap());
    }

    #[test]
    fn one_retry_can_atomically_replace_input_with_a_task_revision() {
        let owner = McpContinuationOwner::new(32, &limits(2), Duration::from_secs(60));
        let pending = owner
            .reserve(tool(32, 1), params(), 32)
            .unwrap()
            .commit(input_required("canary-task-state"))
            .unwrap();
        let mut claim = owner
            .claim_input_responses(prepared_state_only(&owner, pending))
            .unwrap();
        let (retry, _) = owner.begin_input_retry(&mut claim).unwrap();
        let task = Task::new("canary-task", TaskStatus::Working, "now", "now").with_ttl_ms(10_000);
        let next = retry
            .commit(ValidatedPending::Task {
                result: CreateTaskResult::new(task),
                payload_bytes: 64,
            })
            .unwrap();

        assert_eq!(next.kind(), McpContinuationKind::RemoteTask);
        assert_eq!(next.revision().get(), 2);
        assert!(!owner.is_active(pending).unwrap());
        assert!(owner.is_active(next).unwrap());
    }

    #[test]
    fn state_only_and_interactive_round_budgets_are_independent_and_exact() {
        let limits = McpClientLimits {
            max_mrtr_state_only_rounds: 2,
            max_mrtr_interactive_rounds: 1,
            ..McpClientLimits::default()
        };
        let state_only = InputRequiredResult::from_request_state("state-only");
        let interactive = InputRequiredResult::new(
            Some(BTreeMap::from([(
                "private-root-key".to_owned(),
                InputRequest::ListRoots(ListRootsRequest::default()),
            )])),
            Some("interactive".to_owned()),
        );
        let state_one =
            advance_mrtr_rounds(McpMrtrRoundCounts::default(), &state_only, &limits).unwrap();
        let interactive_one = advance_mrtr_rounds(state_one, &interactive, &limits).unwrap();
        let state_two = advance_mrtr_rounds(interactive_one, &state_only, &limits).unwrap();

        assert_eq!(state_two.state_only, 2);
        assert_eq!(state_two.interactive, 1);
        assert!(matches!(
            advance_mrtr_rounds(state_two, &state_only, &limits),
            Err(McpClientError::ContinuationRoundLimitExceeded)
        ));
        assert!(matches!(
            advance_mrtr_rounds(state_two, &interactive, &limits),
            Err(McpClientError::ContinuationRoundLimitExceeded)
        ));
    }

    #[test]
    fn cross_owner_and_preflight_capacity_failures_restore_the_claim() {
        let owner = McpContinuationOwner::new(27, &limits(2), Duration::from_secs(60));
        let pending = owner
            .reserve(tool(27, 1), params(), 16)
            .unwrap()
            .commit(input_required("state"))
            .unwrap();
        let mut claim = owner
            .claim_input_responses(prepared_state_only(&owner, pending))
            .unwrap();
        let other = McpContinuationOwner::new(28, &limits(2), Duration::from_secs(60));
        assert!(matches!(
            other.begin_input_retry(&mut claim),
            Err(McpClientError::ContinuationNotActive)
        ));
        drop(claim);
        assert!(owner.is_active(pending).unwrap());

        let constrained = McpClientLimits {
            max_pending_continuations: 2,
            max_continuation_bytes: 1_040,
            max_result_bytes: 1_024,
            ..McpClientLimits::default()
        };
        let owner = McpContinuationOwner::new(29, &constrained, Duration::from_secs(60));
        let pending = owner
            .reserve(tool(29, 1), params(), 1)
            .unwrap()
            .commit(input_required("state"))
            .unwrap();
        let mut claim = owner
            .claim_input_responses(prepared_state_only(&owner, pending))
            .unwrap();
        assert!(matches!(
            owner.begin_input_retry(&mut claim),
            Err(McpClientError::ContinuationCapacityExceeded)
        ));
        assert!(!owner.is_active(pending).unwrap());
        drop(claim);
        assert!(owner.is_active(pending).unwrap());
    }

    #[test]
    fn revision_exhaustion_after_retry_fails_closed_and_releases_capacity() {
        let owner = McpContinuationOwner::new(30, &limits(2), Duration::from_secs(60));
        let pending = owner
            .reserve(tool(30, 1), params(), 16)
            .unwrap()
            .commit(input_required("state"))
            .unwrap();
        let mut claim = owner
            .claim_input_responses(prepared_state_only(&owner, pending))
            .unwrap();
        claim.pending.revision = McpContinuationRevision(NonZeroU64::new(u64::MAX).unwrap());
        {
            let mut state = owner.state.lock().unwrap();
            let entry = state.entries.get_mut(&pending.id.sequence).unwrap();
            let ContinuationEntryState::ClaimedInput { revision, .. } = &mut entry.state else {
                panic!("expected claimed input fixture")
            };
            *revision = claim.pending.revision;
        }
        let (retry, _) = owner.begin_input_retry(&mut claim).unwrap();
        assert!(matches!(
            retry.commit(input_required("next")),
            Err(McpClientError::ContinuationIdentityExhausted)
        ));
        assert_eq!(owner.pending_count().unwrap(), 0);
    }

    #[test]
    fn concurrent_claimers_cannot_both_lease_one_revision() {
        let owner = Arc::new(McpContinuationOwner::new(
            15,
            &limits(2),
            Duration::from_secs(60),
        ));
        let pending = owner
            .reserve(tool(15, 1), params(), 16)
            .unwrap()
            .commit(input_required("state"))
            .unwrap();
        let first = prepared_state_only(&owner, pending);
        let second = prepared_state_only(&owner, pending);
        let rendezvous = Arc::new(Barrier::new(2));

        let workers = [first, second].map(|prepared| {
            let owner = Arc::clone(&owner);
            let rendezvous = Arc::clone(&rendezvous);
            std::thread::spawn(move || {
                let claim = owner.claim_input_responses(prepared);
                rendezvous.wait();
                claim.is_ok()
            })
        });
        let successes = workers
            .into_iter()
            .map(|worker| usize::from(worker.join().unwrap()))
            .sum::<usize>();

        assert_eq!(successes, 1);
        assert!(owner.is_active(pending).unwrap());
    }

    #[test]
    fn stale_removed_expired_and_cross_owner_claims_fail_without_mutation() {
        let owner = McpContinuationOwner::new(16, &limits(4), Duration::from_secs(60));
        let stale_pending = owner
            .reserve(tool(16, 1), params(), 16)
            .unwrap()
            .commit(input_required("stale"))
            .unwrap();
        let stale = prepared_state_only(&owner, stale_pending);
        {
            let mut state = owner.state.lock().unwrap();
            let entry = state.entries.get_mut(&stale_pending.id.sequence).unwrap();
            let ContinuationEntryState::InputRequired { revision, .. } = &mut entry.state else {
                panic!("expected input-required fixture")
            };
            *revision = McpContinuationRevision(NonZeroU64::new(2).unwrap());
        }
        assert!(matches!(
            owner.claim_input_responses(stale),
            Err(McpClientError::ContinuationNotActive)
        ));

        let removed_pending = owner
            .reserve(tool(16, 2), params(), 16)
            .unwrap()
            .commit(input_required("removed"))
            .unwrap();
        let removed = prepared_state_only(&owner, removed_pending);
        {
            let mut state = owner.state.lock().unwrap();
            remove_entry(&mut state, removed_pending.id.sequence);
        }
        assert!(matches!(
            owner.claim_input_responses(removed),
            Err(McpClientError::ContinuationNotActive)
        ));

        let other = McpContinuationOwner::new(17, &limits(2), Duration::from_secs(60));
        let cross_pending = owner
            .reserve(tool(16, 3), params(), 16)
            .unwrap()
            .commit(input_required("cross"))
            .unwrap();
        let cross = prepared_state_only(&owner, cross_pending);
        assert!(matches!(
            other.claim_input_responses(cross),
            Err(McpClientError::ContinuationNotActive)
        ));
        assert!(owner.is_active(cross_pending).unwrap());

        let expiring = McpContinuationOwner::new(18, &limits(2), Duration::from_millis(1));
        let expired_pending = expiring
            .reserve(tool(18, 1), params(), 16)
            .unwrap()
            .commit(input_required("expired"))
            .unwrap();
        let expired = prepared_state_only(&expiring, expired_pending);
        std::thread::sleep(Duration::from_millis(3));
        assert!(matches!(
            expiring.claim_input_responses(expired),
            Err(McpClientError::ContinuationNotActive)
        ));
        assert_eq!(expiring.pending_count().unwrap(), 0);
    }

    #[test]
    fn claim_capacity_and_identity_failures_leave_revision_active() {
        let constrained = McpClientLimits {
            max_pending_continuations: 2,
            max_continuation_bytes: 1_040,
            max_result_bytes: 1_024,
            ..McpClientLimits::default()
        };
        let owner = McpContinuationOwner::new(19, &constrained, Duration::from_secs(60));
        let pending = owner
            .reserve(tool(19, 1), params(), 16)
            .unwrap()
            .commit(ValidatedPending::InputRequired {
                result: InputRequiredResult::from_request_state("state"),
                payload_bytes: 1_023,
            })
            .unwrap();
        let prepared = prepared_state_only(&owner, pending);
        assert!(matches!(
            owner.claim_input_responses(prepared),
            Err(McpClientError::ContinuationCapacityExceeded)
        ));
        assert!(owner.is_active(pending).unwrap());

        let owner = McpContinuationOwner::new(20, &limits(2), Duration::from_secs(60));
        let pending = owner
            .reserve(tool(20, 1), params(), 16)
            .unwrap()
            .commit(input_required("state"))
            .unwrap();
        let prepared = prepared_state_only(&owner, pending);
        owner.state.lock().unwrap().next_claim_sequence = 0;
        assert!(matches!(
            owner.claim_input_responses(prepared),
            Err(McpClientError::ContinuationIdentityExhausted)
        ));
        assert!(owner.is_active(pending).unwrap());
    }

    #[test]
    fn deadline_is_rechecked_after_lock_wait_for_claim_and_release() {
        let owner = Arc::new(McpContinuationOwner::new(
            24,
            &limits(2),
            Duration::from_millis(20),
        ));
        let pending = owner
            .reserve(tool(24, 1), params(), 16)
            .unwrap()
            .commit(input_required("state"))
            .unwrap();
        let prepared = prepared_state_only(&owner, pending);
        let guard = owner.state.lock().unwrap();
        let claiming_owner = Arc::clone(&owner);
        let claimant = std::thread::spawn(move || claiming_owner.claim_input_responses(prepared));
        std::thread::sleep(Duration::from_millis(30));
        drop(guard);
        assert!(matches!(
            claimant.join().unwrap(),
            Err(McpClientError::ContinuationNotActive)
        ));
        assert_eq!(owner.pending_count().unwrap(), 0);

        let owner = McpContinuationOwner::new(25, &limits(2), Duration::from_millis(20));
        let pending = owner
            .reserve(tool(25, 1), params(), 16)
            .unwrap()
            .commit(input_required("state"))
            .unwrap();
        let claim = owner
            .claim_input_responses(prepared_state_only(&owner, pending))
            .unwrap();
        let guard = owner.state.lock().unwrap();
        let releaser = std::thread::spawn(move || drop(claim));
        std::thread::sleep(Duration::from_millis(30));
        drop(guard);
        releaser.join().unwrap();

        assert!(!owner.is_active(pending).unwrap());
        assert_eq!(owner.pending_count().unwrap(), 0);
    }

    #[test]
    fn poisoned_owner_rejects_claim_and_claim_drop_without_panicking() {
        let owner = McpContinuationOwner::new(21, &limits(2), Duration::from_secs(60));
        let pending = owner
            .reserve(tool(21, 1), params(), 16)
            .unwrap()
            .commit(input_required("state"))
            .unwrap();
        let prepared = prepared_state_only(&owner, pending);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = owner.state.lock().unwrap();
            panic!("poison continuation claim fixture");
        }));
        assert!(matches!(
            owner.claim_input_responses(prepared),
            Err(McpClientError::ContinuationStateUnavailable)
        ));

        let owner = McpContinuationOwner::new(22, &limits(2), Duration::from_secs(60));
        let pending = owner
            .reserve(tool(22, 1), params(), 16)
            .unwrap()
            .commit(input_required("state"))
            .unwrap();
        let claim = owner
            .claim_input_responses(prepared_state_only(&owner, pending))
            .unwrap();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = owner.state.lock().unwrap();
            panic!("poison claimed continuation fixture");
        }));
        drop(claim);
    }

    #[test]
    fn maximum_owner_admission_and_drop_are_iterative_on_a_small_stack() {
        std::thread::Builder::new()
            .stack_size(96 * 1024)
            .spawn(|| {
                let limits = McpClientLimits {
                    max_pending_continuations: 512,
                    max_continuation_bytes: 512 * 256,
                    max_result_bytes: 128,
                    ..McpClientLimits::default()
                };
                let owner = McpContinuationOwner::new(12, &limits, Duration::from_secs(60));
                let mut pending = Vec::with_capacity(512);
                for sequence in 1..=512 {
                    pending.push(
                        owner
                            .reserve(tool(12, sequence), params(), 16)
                            .unwrap()
                            .commit(input_required("state"))
                            .unwrap(),
                    );
                }
                assert_eq!(owner.pending_count().unwrap(), 512);
                assert!(pending
                    .into_iter()
                    .all(|pending| owner.is_active(pending).unwrap()));
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn maximum_claim_and_release_cycle_is_iterative_on_a_small_stack() {
        std::thread::Builder::new()
            .stack_size(96 * 1024)
            .spawn(|| {
                let limits = McpClientLimits {
                    max_pending_continuations: 512,
                    max_continuation_bytes: 512 * 256,
                    max_result_bytes: 128,
                    ..McpClientLimits::default()
                };
                let owner = McpContinuationOwner::new(23, &limits, Duration::from_secs(60));
                let mut pending = Vec::with_capacity(512);
                for sequence in 1..=512 {
                    let call = owner
                        .reserve(tool(23, sequence), params(), 16)
                        .unwrap()
                        .commit(input_required("state"))
                        .unwrap();
                    pending.push(call);
                }
                let claims = pending
                    .iter()
                    .map(|pending| {
                        owner
                            .claim_input_responses(prepared_state_only(&owner, *pending))
                            .unwrap()
                    })
                    .collect::<Vec<_>>();
                assert!(pending
                    .iter()
                    .all(|pending| !owner.is_active(*pending).unwrap()));
                drop(claims);
                assert!(pending
                    .into_iter()
                    .all(|pending| owner.is_active(pending).unwrap()));
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn maximum_one_round_retry_cycle_is_iterative_on_a_small_stack() {
        std::thread::Builder::new()
            .stack_size(96 * 1024)
            .spawn(|| {
                let limits = McpClientLimits {
                    max_pending_continuations: 512,
                    max_continuation_bytes: 512 * 512,
                    max_result_bytes: 128,
                    ..McpClientLimits::default()
                };
                let owner = McpContinuationOwner::new(31, &limits, Duration::from_secs(60));
                let mut pending = Vec::with_capacity(512);
                for sequence in 1..=512 {
                    pending.push(
                        owner
                            .reserve(tool(31, sequence), params(), 32)
                            .unwrap()
                            .commit(input_required("state"))
                            .unwrap(),
                    );
                }

                let retries = pending
                    .into_iter()
                    .map(|pending| {
                        let mut claim = owner
                            .claim_input_responses(prepared_state_only(&owner, pending))
                            .unwrap();
                        owner.begin_input_retry(&mut claim).unwrap().0
                    })
                    .collect::<Vec<_>>();
                let next = retries
                    .into_iter()
                    .map(|retry| retry.commit(input_required("next")).unwrap())
                    .collect::<Vec<_>>();
                assert!(next.iter().all(|pending| pending.revision().get() == 2));
                assert!(next
                    .into_iter()
                    .all(|pending| owner.is_active(pending).unwrap()));
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn maximum_single_continuation_round_cycle_is_iterative_on_a_small_stack() {
        std::thread::Builder::new()
            .stack_size(96 * 1024)
            .spawn(|| {
                let limits = McpClientLimits {
                    max_mrtr_state_only_rounds: 64,
                    ..limits(1)
                };
                let owner = McpContinuationOwner::new(33, &limits, Duration::from_secs(60));
                let mut pending = owner
                    .reserve(tool(33, 1), params(), 32)
                    .unwrap()
                    .commit(input_required("round-1"))
                    .unwrap();
                for round in 2..=64 {
                    let mut claim = owner
                        .claim_input_responses(prepared_state_only(&owner, pending))
                        .unwrap();
                    let (retry, _) = owner.begin_input_retry(&mut claim).unwrap();
                    pending = retry
                        .commit(input_required(&format!("round-{round}")))
                        .unwrap();
                    assert_eq!(pending.revision().get(), round as u64);
                }

                let mut claim = owner
                    .claim_input_responses(prepared_state_only(&owner, pending))
                    .unwrap();
                let (retry, _) = owner.begin_input_retry(&mut claim).unwrap();
                assert!(matches!(
                    retry.commit(input_required("round-65")),
                    Err(McpClientError::ContinuationRoundLimitExceeded)
                ));
                assert_eq!(owner.pending_count().unwrap(), 0);
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
