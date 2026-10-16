//! Artifact V3 owner for one sealed `agent_as_tool` child execution.
//!
//! This module is deliberately a sidecar to the canonical child execution,
//! not another executor. It persists the exact reviewed launch before the
//! existing delegation scheduler may start the child, records cancellation as
//! a non-terminal intent, derives terminal state from Artifact V3, and owns the
//! replayable one-shot parent-notification outbox.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::execution_artifacts::FilesystemExecutionArtifactIndexStore;
use super::models::{ExecutionState, PersistedExecutionArtifactRecord};
use super::service::{ArtifactV2Error, ScopeRef};
use super::workspace::ArtifactV2Workspace;
use crate::magician_v2::agents::storage::AgentStorage;
use crate::magician_v2::apps::agent_capability::{
    AppAgentCapabilityError, AppAgentChildControlCarrier, AppAgentChildControlRecord,
    AppAgentChildControlStatus, AppAgentChildRunHandle, AppAgentChildTaskBinding,
};
use crate::magician_v2::apps::models::{
    AppContractError, AppDigest, AppHandlingLabels, AppName, AppReference, AppScopeBindingRef,
};
use crate::magician_v2::apps::resource_contract::{
    AppResourceJournal, AppResourceJournalEvent, AppResourceSettlementOutcome,
};
use crate::magician_v2::json_traversal::{canonical_json_bytes, exact_json_encoded_len};

const LIFECYCLE_SCHEMA: &str = "magician.app-agent-tool-artifact-lifecycle.v3";
const CANCELLATION_SCHEMA: &str = "magician.app-agent-tool-cancellation-intent.v1";
const PARENT_NOTIFICATION_SCHEMA: &str = "magician.app-agent-tool-parent-notification.v1";
const LIFECYCLE_FILE: &str = "lifecycle.json";
const PARENT_NOTIFICATION_FILE: &str = "parent-notification.json";
const PARENT_NOTIFICATION_DIR: &str = "app_agent_tool_notifications";
const MAX_LIFECYCLE_BYTES: usize = 1024 * 1024;
const MAX_LIFECYCLE_DEPTH: usize = 32;
const MAX_LIFECYCLE_NODES: usize = 16_384;
const MAX_EXECUTION_STATE_BYTES: u64 = 512 * 1024;
const MAX_EXECUTION_STATE_DEPTH: usize = 32;
const MAX_EXECUTION_STATE_NODES: usize = 16_384;
const MAX_RESULT_JSON_DEPTH: usize = 16;
const MAX_RESULT_JSON_NODES: usize = 4_096;
const MAX_TASK_AGENT_TOOL_LAUNCHES: usize = 4_096;

fn process_lifecycle_lock(path: &Path) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<std::sync::Mutex<BTreeMap<PathBuf, Weak<tokio::sync::Mutex<()>>>>> =
        OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(|| std::sync::Mutex::new(BTreeMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(path).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(path.to_path_buf(), Arc::downgrade(&lock));
    lock
}

#[derive(Debug, Error)]
pub(crate) enum AppAgentToolLifecycleError {
    #[error(transparent)]
    Artifact(#[from] ArtifactV2Error),
    #[error(transparent)]
    Capability(#[from] AppAgentCapabilityError),
    #[error(transparent)]
    Contract(#[from] AppContractError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("agent_as_tool lifecycle lock failed: {0}")]
    Lock(String),
    #[error("agent_as_tool lifecycle document is corrupt")]
    CorruptLifecycle,
    #[error("agent_as_tool child does not match the canonical Artifact execution")]
    ChildIdentityMismatch,
    #[error("agent_as_tool child was not sealed before canonical execution start")]
    ChildAlreadyStarted,
    #[error(
        "agent_as_tool child launch has not been attached to its canonical Artifact execution"
    )]
    ChildNotAttached,
    #[error("agent_as_tool child has not reached a canonical terminal state")]
    ChildNotTerminal,
    #[error("agent_as_tool completed child has no unique bounded typed result artifact")]
    TypedResultArtifactUnavailable,
    #[error("agent_as_tool cancellation conflicts with the retained request")]
    ConflictingCancellation,
    #[error("agent_as_tool terminal settlement conflicts with the retained settlement")]
    ConflictingTerminalSettlement,
    #[error("agent_as_tool parent notification acknowledgement is invalid")]
    InvalidParentNotificationAck,
    #[error("agent_as_tool result disclosure is not authorized by the current app authority")]
    ResultDisclosureDenied,
    #[error("agent_as_tool common effect settlement is not yet available")]
    EffectSettlementPending,
    #[error("agent_as_tool canonical cancellation settlement is not yet available")]
    CancellationSettlementPending,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum AppAgentToolLaunchPhase {
    Reserved,
    Attached,
    Denied,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AppAgentChildCancellationReason {
    ParentCancelled,
    GrantRevoked,
    OwnerStop,
    Deadline,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AppAgentChildCancellationIntent {
    schema: String,
    launch_digest: AppDigest,
    request_ref: AppReference,
    sequence: u64,
    reason: AppAgentChildCancellationReason,
    requested_at_ms: i64,
    before_child_start: bool,
    request_digest: AppDigest,
}

impl fmt::Debug for AppAgentChildCancellationIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppAgentChildCancellationIntent")
            .field("request_ref", &self.request_ref)
            .field("reason", &self.reason)
            .field("requested_at_ms", &self.requested_at_ms)
            .field("before_child_start", &self.before_child_start)
            .field("request_digest", &self.request_digest)
            .finish()
    }
}

impl AppAgentChildCancellationIntent {
    fn mint(
        binding: &AppAgentChildTaskBinding,
        reason: AppAgentChildCancellationReason,
        requested_at_ms: i64,
        before_child_start: bool,
    ) -> Result<Self, AppAgentToolLifecycleError> {
        if requested_at_ms <= 0 {
            return Err(AppAgentToolLifecycleError::ConflictingCancellation);
        }
        let request_ref = AppReference::parse(format!(
            "agent-child-cancellation:{}",
            binding.launch_digest().as_str()
        ))?;
        let request_digest = cancellation_digest(
            binding.launch_digest(),
            &request_ref,
            reason,
            requested_at_ms,
            before_child_start,
        )?;
        Ok(Self {
            schema: CANCELLATION_SCHEMA.to_owned(),
            launch_digest: binding.launch_digest().clone(),
            request_ref,
            sequence: 1,
            reason,
            requested_at_ms,
            before_child_start,
            request_digest,
        })
    }

    fn validate(
        &self,
        binding: &AppAgentChildTaskBinding,
    ) -> Result<(), AppAgentToolLifecycleError> {
        if self.schema != CANCELLATION_SCHEMA
            || &self.launch_digest != binding.launch_digest()
            || self.sequence != 1
            || self.requested_at_ms <= 0
            || self.request_digest
                != cancellation_digest(
                    binding.launch_digest(),
                    &self.request_ref,
                    self.reason,
                    self.requested_at_ms,
                    self.before_child_start,
                )?
        {
            return Err(AppAgentToolLifecycleError::CorruptLifecycle);
        }
        Ok(())
    }
}

fn cancellation_digest(
    launch_digest: &AppDigest,
    request_ref: &AppReference,
    reason: AppAgentChildCancellationReason,
    requested_at_ms: i64,
    before_child_start: bool,
) -> Result<AppDigest, AppAgentToolLifecycleError> {
    Ok(AppDigest::blake3(&canonical_json_bytes(
        &serde_json::json!({
            "schema": CANCELLATION_SCHEMA,
            "launch_digest": launch_digest,
            "request_ref": request_ref,
            "sequence": 1,
            "reason": reason,
            "requested_at_ms": requested_at_ms,
            "before_child_start": before_child_start,
        }),
    )?))
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct AppAgentToolLifecycleDocument {
    schema: String,
    launch_phase: AppAgentToolLaunchPhase,
    binding: AppAgentChildTaskBinding,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cancellation: Option<AppAgentChildCancellationIntent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    terminal: Option<AppAgentChildControlRecord>,
    document_digest: AppDigest,
}

impl fmt::Debug for AppAgentToolLifecycleDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppAgentToolLifecycleDocument")
            .field("schema", &self.schema)
            .field("launch_phase", &self.launch_phase)
            .field("binding", &self.binding)
            .field("cancellation", &self.cancellation)
            .field("terminal", &self.terminal)
            .field("document_digest", &self.document_digest)
            .finish()
    }
}

impl AppAgentToolLifecycleDocument {
    fn new(binding: AppAgentChildTaskBinding) -> Result<Self, AppAgentToolLifecycleError> {
        binding.validate_integrity()?;
        let mut document = Self {
            schema: LIFECYCLE_SCHEMA.to_owned(),
            launch_phase: AppAgentToolLaunchPhase::Reserved,
            binding,
            cancellation: None,
            terminal: None,
            document_digest: AppDigest::blake3(b"pending-agent-tool-lifecycle"),
        };
        document.refresh_digest()?;
        Ok(document)
    }

    fn validate(&self) -> Result<(), AppAgentToolLifecycleError> {
        self.binding.validate_integrity()?;
        if self.schema != LIFECYCLE_SCHEMA || self.document_digest != self.canonical_digest()? {
            return Err(AppAgentToolLifecycleError::CorruptLifecycle);
        }
        match self.launch_phase {
            AppAgentToolLaunchPhase::Reserved
                if self.cancellation.is_some() || self.terminal.is_some() =>
            {
                return Err(AppAgentToolLifecycleError::CorruptLifecycle);
            },
            AppAgentToolLaunchPhase::Denied
                if self.terminal.is_some()
                    || !self.cancellation.as_ref().is_some_and(|intent| {
                        intent.before_child_start
                            && matches!(
                                intent.reason,
                                AppAgentChildCancellationReason::Deadline
                                    | AppAgentChildCancellationReason::GrantRevoked
                            )
                    }) =>
            {
                return Err(AppAgentToolLifecycleError::CorruptLifecycle);
            },
            _ => {},
        }
        if let Some(cancellation) = &self.cancellation {
            cancellation.validate(&self.binding)?;
        }
        if let Some(terminal) = &self.terminal {
            terminal.validate_integrity(&self.binding)?;
            match terminal.status() {
                AppAgentChildControlStatus::Completed if self.cancellation.is_some() => {
                    return Err(AppAgentToolLifecycleError::CorruptLifecycle)
                },
                AppAgentChildControlStatus::Cancelled
                    if !self
                        .cancellation
                        .as_ref()
                        .is_some_and(|intent| intent.before_child_start) =>
                {
                    return Err(AppAgentToolLifecycleError::CorruptLifecycle)
                },
                _ => {},
            }
        }
        Ok(())
    }

    fn refresh_digest(&mut self) -> Result<(), AppAgentToolLifecycleError> {
        self.document_digest = self.canonical_digest()?;
        Ok(())
    }

    fn canonical_digest(&self) -> Result<AppDigest, AppAgentToolLifecycleError> {
        Ok(AppDigest::blake3(&canonical_json_bytes(
            &serde_json::json!({
                "schema": &self.schema,
                "launch_phase": self.launch_phase,
                "binding": &self.binding,
                "cancellation": &self.cancellation,
                "terminal": &self.terminal,
            }),
        )?))
    }
}

/// Move-only durable launch intent. Its exact sealed binding is persisted at a
/// task-level deterministic path before a canonical child shell or parent link
/// may be created. A crash therefore leaves either this adoptable intent or no
/// child authority at all; a mutable agent catalog is never consulted to
/// reconstruct an orphan.
pub struct AppAgentToolReservedLaunch {
    scope: ScopeRef,
    task_id: String,
    child_execution_id: String,
    binding: AppAgentChildTaskBinding,
}

impl fmt::Debug for AppAgentToolReservedLaunch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppAgentToolReservedLaunch")
            .field("task_id", &self.task_id)
            .field("child_execution_id", &self.child_execution_id)
            .field("launch_digest", self.binding.launch_digest())
            .finish_non_exhaustive()
    }
}

impl AppAgentToolReservedLaunch {
    pub(crate) fn binding(&self) -> &AppAgentChildTaskBinding {
        &self.binding
    }

    pub(crate) fn child_execution_id(&self) -> &str {
        self.child_execution_id.as_str()
    }

    pub(crate) fn scope(&self) -> &ScopeRef {
        &self.scope
    }

    pub(crate) fn task_id(&self) -> &str {
        self.task_id.as_str()
    }
}

pub(crate) enum AppAgentToolRecoveredLaunch {
    Reserved,
    Attached(AppAgentToolPreparedChild),
}

/// Move-only authority to dispatch or cancel one exact canonical child.
pub(crate) struct AppAgentToolPreparedChild {
    scope: ScopeRef,
    task_id: String,
    child_execution_id: String,
    binding: AppAgentChildTaskBinding,
    handle: AppAgentChildRunHandle,
}

impl fmt::Debug for AppAgentToolPreparedChild {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppAgentToolPreparedChild")
            .field("task_id", &self.task_id)
            .field("child_execution_id", &self.child_execution_id)
            .field("launch_digest", self.binding.launch_digest())
            .finish_non_exhaustive()
    }
}

impl AppAgentToolPreparedChild {
    pub(crate) fn binding(&self) -> &AppAgentChildTaskBinding {
        &self.binding
    }

    pub(crate) fn child_execution_id(&self) -> &str {
        self.child_execution_id.as_str()
    }
}

/// Non-terminal, move-only request for the canonical execution owner to stop
/// the exact child. It cannot be serialized into cancellation authority.
pub(crate) struct AppAgentChildCancellationPermit {
    child_execution_ref: AppReference,
}

/// Opaque result of reconciling the child's common effect ledger. A completed
/// execution is not sufficient by itself: the child may have performed an
/// outward action whose durable settlement is still uncertain.
pub(crate) struct AppAgentChildEffectSettlementProof {
    launch_digest: AppDigest,
    child_execution_ref: AppReference,
    settlement_digest: AppDigest,
    all_effects_settled: bool,
    outcome_uncertain: bool,
}

/// Opaque acknowledgement from the canonical scheduler that the retained
/// cancellation won before the child entered execution. The request-time
/// status alone is not enough because start and cancel can race.
pub(crate) struct AppAgentChildCancellationSettlementProof {
    launch_digest: AppDigest,
    child_execution_ref: AppReference,
    request_digest: AppDigest,
    settlement_digest: AppDigest,
    stopped_before_start: bool,
    outcome_uncertain: bool,
}

/// Opaque final/replay fence from the live app authority owner. Artifact state
/// is durable evidence, not perpetual disclosure authority: a revoked grant or
/// changed policy must withhold a previously completed typed result.
pub(crate) struct AppAgentChildCurrentAuthorityProof {
    launch_digest: AppDigest,
    scope_binding_ref: AppScopeBindingRef,
    workflow_authority_digest: AppDigest,
    authority_fence_digest: AppDigest,
    result_disclosure_allowed: bool,
    revoked: bool,
}

/// Result projected from a delivered Artifact-owned control carrier. It has
/// no Serde surface and deliberately omits every task, execution, launch and
/// artifact identifier. Completed bytes still have to pass the current app
/// disclosure owner before entering model history.
pub(crate) enum AppAgentToolDeliveredResult {
    Completed {
        value: Value,
        handling_labels: AppHandlingLabels,
        carrier_digest: AppDigest,
    },
    Cancelled {
        handling_labels: AppHandlingLabels,
        carrier_digest: AppDigest,
    },
    OutcomeUncertain {
        failure_code: AppName,
        handling_labels: AppHandlingLabels,
        carrier_digest: AppDigest,
    },
}

/// Deterministic parent-owned control row. This is the exactly-once durable
/// notification boundary; canonical UI events are not used as authority
/// because their UUID identities cannot make crash replay idempotent.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct AppAgentParentNotificationRecord {
    schema: String,
    notification_ref: AppReference,
    carrier_digest: AppDigest,
    parent_task_ref: AppReference,
    parent_execution_ref: AppReference,
    child_execution_ref: AppReference,
    launch_digest: AppDigest,
    status: AppAgentChildControlStatus,
    record_digest: AppDigest,
}

impl AppAgentParentNotificationRecord {
    fn from_carrier(
        binding: &AppAgentChildTaskBinding,
        carrier: &AppAgentChildControlCarrier,
    ) -> Result<Self, AppAgentToolLifecycleError> {
        binding.validate_integrity()?;
        let mut record = Self {
            schema: PARENT_NOTIFICATION_SCHEMA.to_owned(),
            notification_ref: carrier.notification_ref().clone(),
            carrier_digest: carrier.carrier_digest().clone(),
            parent_task_ref: binding.parent_task_ref().clone(),
            parent_execution_ref: binding.parent_execution_ref().clone(),
            child_execution_ref: binding.child_execution_ref().clone(),
            launch_digest: binding.launch_digest().clone(),
            status: carrier.status(),
            record_digest: AppDigest::blake3(b"pending-agent-tool-parent-notification"),
        };
        record.record_digest = record.canonical_digest()?;
        Ok(record)
    }

    fn validate(
        &self,
        binding: &AppAgentChildTaskBinding,
    ) -> Result<(), AppAgentToolLifecycleError> {
        if self.schema != PARENT_NOTIFICATION_SCHEMA
            || self.parent_task_ref != *binding.parent_task_ref()
            || self.parent_execution_ref != *binding.parent_execution_ref()
            || self.child_execution_ref != *binding.child_execution_ref()
            || self.launch_digest != *binding.launch_digest()
            || self.record_digest != self.canonical_digest()?
        {
            return Err(AppAgentToolLifecycleError::InvalidParentNotificationAck);
        }
        Ok(())
    }

    fn canonical_digest(&self) -> Result<AppDigest, AppAgentToolLifecycleError> {
        Ok(AppDigest::blake3(&canonical_json_bytes(
            &serde_json::json!({
                "schema": &self.schema,
                "notification_ref": &self.notification_ref,
                "carrier_digest": &self.carrier_digest,
                "parent_task_ref": &self.parent_task_ref,
                "parent_execution_ref": &self.parent_execution_ref,
                "child_execution_ref": &self.child_execution_ref,
                "launch_digest": &self.launch_digest,
                "status": self.status,
            }),
        )?))
    }
}

impl fmt::Debug for AppAgentChildCurrentAuthorityProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppAgentChildCurrentAuthorityProof")
            .field("launch_digest", &self.launch_digest)
            .field("scope_binding_ref", &self.scope_binding_ref)
            .field("workflow_authority_digest", &self.workflow_authority_digest)
            .field("authority_fence_digest", &self.authority_fence_digest)
            .field("result_disclosure_allowed", &self.result_disclosure_allowed)
            .field("revoked", &self.revoked)
            .finish()
    }
}

impl fmt::Debug for AppAgentChildCancellationSettlementProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppAgentChildCancellationSettlementProof")
            .field("launch_digest", &self.launch_digest)
            .field("child_execution_ref", &self.child_execution_ref)
            .field("request_digest", &self.request_digest)
            .field("settlement_digest", &self.settlement_digest)
            .field("stopped_before_start", &self.stopped_before_start)
            .field("outcome_uncertain", &self.outcome_uncertain)
            .finish()
    }
}

impl fmt::Debug for AppAgentChildEffectSettlementProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppAgentChildEffectSettlementProof")
            .field("launch_digest", &self.launch_digest)
            .field("child_execution_ref", &self.child_execution_ref)
            .field("settlement_digest", &self.settlement_digest)
            .field("all_effects_settled", &self.all_effects_settled)
            .field("outcome_uncertain", &self.outcome_uncertain)
            .finish()
    }
}

impl AppAgentChildEffectSettlementProof {
    /// Derive terminal effect truth from the canonical resource journal for
    /// this exact child node. A committed settlement or a proven-before-I/O
    /// abort closes a reservation; any other settlement is uncertain, while a
    /// reservation with no terminal event remains pending.
    pub(crate) fn from_resource_owner(
        binding: &AppAgentChildTaskBinding,
        node_id: &AppReference,
        journal: &AppResourceJournal,
    ) -> Result<Self, AppAgentToolLifecycleError> {
        binding.validate_integrity()?;
        let mut opened = 0_u32;
        let mut closed = 0_u32;
        let mut reservations = BTreeMap::<AppReference, (bool, bool, bool)>::new();
        let mut relevant = Vec::new();
        let mut outcome_uncertain = false;
        for event in &journal.events {
            match event {
                AppResourceJournalEvent::NodeOpened {
                    node_id: observed, ..
                } if observed == node_id => {
                    opened = opened.saturating_add(1);
                    relevant.push(event);
                },
                AppResourceJournalEvent::NodeClosed {
                    node_id: observed, ..
                } if observed == node_id => {
                    closed = closed.saturating_add(1);
                    relevant.push(event);
                },
                AppResourceJournalEvent::Reserved {
                    node_id: observed,
                    reservation_id,
                    ..
                } if observed == node_id => {
                    if reservations
                        .insert(reservation_id.clone(), (false, false, false))
                        .is_some()
                    {
                        return Err(AppAgentToolLifecycleError::ConflictingTerminalSettlement);
                    }
                    relevant.push(event);
                },
                AppResourceJournalEvent::EffectDispatchStarted {
                    node_id: observed,
                    reservation_id,
                    ..
                } if observed == node_id => {
                    let state = reservations
                        .get_mut(reservation_id)
                        .ok_or(AppAgentToolLifecycleError::ConflictingTerminalSettlement)?;
                    if state.0 || state.1 || state.2 {
                        return Err(AppAgentToolLifecycleError::ConflictingTerminalSettlement);
                    }
                    state.0 = true;
                    relevant.push(event);
                },
                AppResourceJournalEvent::EffectDispatchAbortedBeforeIo {
                    node_id: observed,
                    reservation_id,
                    ..
                } if observed == node_id => {
                    let state = reservations
                        .get_mut(reservation_id)
                        .ok_or(AppAgentToolLifecycleError::ConflictingTerminalSettlement)?;
                    if !state.0 || state.1 || state.2 {
                        return Err(AppAgentToolLifecycleError::ConflictingTerminalSettlement);
                    }
                    state.1 = true;
                    relevant.push(event);
                },
                AppResourceJournalEvent::Settled {
                    node_id: observed,
                    reservation_id,
                    outcome,
                    ..
                } if observed == node_id => {
                    let state = reservations
                        .get_mut(reservation_id)
                        .ok_or(AppAgentToolLifecycleError::ConflictingTerminalSettlement)?;
                    if state.1 || state.2 {
                        return Err(AppAgentToolLifecycleError::ConflictingTerminalSettlement);
                    }
                    state.2 = true;
                    outcome_uncertain |= *outcome != AppResourceSettlementOutcome::Committed;
                    relevant.push(event);
                },
                _ => {},
            }
        }
        let all_effects_settled = opened == 1
            && closed == 1
            && reservations
                .values()
                .all(|(_, aborted, settled)| *aborted || *settled);
        let settlement_digest = AppDigest::blake3(&canonical_json_bytes(&serde_json::json!({
            "schema": "magician.app-agent-tool-effect-settlement.v1",
            "journal_identity": &journal.identity,
            "journal_revision": journal.events.len(),
            "node_id": node_id,
            "events": relevant,
            "all_effects_settled": all_effects_settled,
            "outcome_uncertain": outcome_uncertain,
        }))?);
        Ok(Self {
            launch_digest: binding.launch_digest().clone(),
            child_execution_ref: binding.child_execution_ref().clone(),
            settlement_digest,
            all_effects_settled,
            outcome_uncertain,
        })
    }
}

impl AppAgentChildCurrentAuthorityProof {
    /// Minted by the workflow owner after reopening the current immutable
    /// package lock, installation/grant/schema authority and exact callable
    /// agent definition. Durable launch bytes are a ceiling, not current
    /// result-disclosure authority.
    pub(crate) fn from_workflow_owner(
        binding: &AppAgentChildTaskBinding,
        scope_binding_ref: AppScopeBindingRef,
        workflow_authority_digest: AppDigest,
        authority_fence_digest: AppDigest,
        result_disclosure_allowed: bool,
        revoked: bool,
    ) -> Result<Self, AppAgentToolLifecycleError> {
        binding.validate_integrity()?;
        Ok(Self {
            launch_digest: binding.launch_digest().clone(),
            scope_binding_ref,
            workflow_authority_digest,
            authority_fence_digest,
            result_disclosure_allowed,
            revoked,
        })
    }
}

impl AppAgentChildCancellationPermit {
    pub(crate) fn child_execution_ref(&self) -> &AppReference {
        &self.child_execution_ref
    }
}

/// Unforgeable terminal observation consumed by the Apps carrier. Its fields
/// are private to this Artifact owner and it has no Serde or Clone surface.
pub(crate) struct AppAgentChildTerminalObservation {
    launch_digest: AppDigest,
    status: AppAgentChildControlStatus,
    value: Option<Value>,
    value_digest: Option<AppDigest>,
    result_artifact_ref: Option<AppReference>,
    result_artifact_digest: Option<AppDigest>,
    failure_code: Option<AppName>,
    terminal_settlement_digest: Option<AppDigest>,
    handling_labels: AppHandlingLabels,
    cancellation_settled: bool,
    effect_uncertain: bool,
}

impl fmt::Debug for AppAgentChildTerminalObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppAgentChildTerminalObservation")
            .field("launch_digest", &self.launch_digest)
            .field("status", &self.status)
            .field("value_digest", &self.value_digest)
            .field("result_artifact_ref", &self.result_artifact_ref)
            .field("result_artifact_digest", &self.result_artifact_digest)
            .field("failure_code", &self.failure_code)
            .field(
                "terminal_settlement_digest",
                &self.terminal_settlement_digest,
            )
            .field("handling_labels", &self.handling_labels)
            .field("cancellation_settled", &self.cancellation_settled)
            .field("effect_uncertain", &self.effect_uncertain)
            .finish_non_exhaustive()
    }
}

impl AppAgentChildTerminalObservation {
    pub(crate) fn launch_digest(&self) -> &AppDigest {
        &self.launch_digest
    }

    #[allow(clippy::type_complexity)]
    pub(crate) fn into_parts(
        self,
    ) -> (
        AppAgentChildControlStatus,
        Option<Value>,
        Option<AppDigest>,
        Option<AppReference>,
        Option<AppDigest>,
        Option<AppName>,
        Option<AppDigest>,
        AppHandlingLabels,
        bool,
        bool,
    ) {
        (
            self.status,
            self.value,
            self.value_digest,
            self.result_artifact_ref,
            self.result_artifact_digest,
            self.failure_code,
            self.terminal_settlement_digest,
            self.handling_labels,
            self.cancellation_settled,
            self.effect_uncertain,
        )
    }
}

/// Opaque acknowledgement minted only after the parent notification has been
/// durably appended by the Artifact event owner.
pub(crate) struct AppAgentParentNotificationAck {
    notification_ref: AppReference,
    carrier_digest: AppDigest,
    parent_event_ref: AppReference,
    parent_event_digest: AppDigest,
}

impl AppAgentParentNotificationAck {
    pub(crate) fn notification_ref(&self) -> &AppReference {
        &self.notification_ref
    }

    pub(crate) fn carrier_digest(&self) -> &AppDigest {
        &self.carrier_digest
    }

    pub(crate) fn parent_event_ref(&self) -> &AppReference {
        &self.parent_event_ref
    }

    pub(crate) fn parent_event_digest(&self) -> &AppDigest {
        &self.parent_event_digest
    }
}

pub(crate) struct AppAgentToolPendingNotification {
    carrier: AppAgentChildControlCarrier,
}

impl AppAgentToolPendingNotification {
    pub(crate) fn carrier(&self) -> &AppAgentChildControlCarrier {
        &self.carrier
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppAgentChildCancellationDisposition {
    Requested,
    AlreadyRequested,
}

pub(crate) struct AppAgentToolLifecycleStore {
    workspace: ArtifactV2Workspace,
    artifacts: FilesystemExecutionArtifactIndexStore,
}

impl AppAgentToolLifecycleStore {
    pub(crate) fn new(workspace: ArtifactV2Workspace) -> Self {
        Self {
            artifacts: FilesystemExecutionArtifactIndexStore::new(workspace.clone()),
            workspace,
        }
    }

    /// Persist the exact launch before a canonical child row, parent link, or
    /// scheduler mutation is allowed. Existing identical bytes are an
    /// idempotent replay; any different binding for the deterministic child
    /// identity fails closed.
    pub(crate) async fn reserve_launch(
        &self,
        scope: ScopeRef,
        task_id: &str,
        child_execution_id: &str,
        binding: AppAgentChildTaskBinding,
    ) -> Result<AppAgentToolReservedLaunch, AppAgentToolLifecycleError> {
        binding.validate_integrity()?;
        validate_runtime_segment(child_execution_id)?;
        super::workspace::ArtifactV2Workspace::validate_task_id(task_id)?;
        let path = self.lifecycle_path(&scope, task_id, child_execution_id);
        if let Some(parent) = path.parent() {
            self.workspace.create_dir_all_path(parent).await?;
        }
        let _process_guard = process_lifecycle_lock(&path).lock_owned().await;
        let _guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(|error| AppAgentToolLifecycleError::Lock(error.to_string()))?;
        let document = match self.read_document(&path).await? {
            Some(existing) => {
                if &existing.binding != &binding {
                    return Err(AppAgentToolLifecycleError::CorruptLifecycle);
                }
                if existing.launch_phase == AppAgentToolLaunchPhase::Denied
                    || existing.cancellation.is_some()
                {
                    return Err(AppAgentToolLifecycleError::ConflictingCancellation);
                }
                existing
            },
            None => {
                let document = AppAgentToolLifecycleDocument::new(binding)?;
                self.write_document(&path, document.clone()).await?;
                document
            },
        };
        Ok(AppAgentToolReservedLaunch {
            scope,
            task_id: task_id.to_owned(),
            child_execution_id: child_execution_id.to_owned(),
            binding: document.binding,
        })
    }

    /// Adopt the already-reserved launch after the canonical child shell has
    /// been durably created and attached, but before the scheduler can run it.
    /// This transition is idempotent and validates the exact Artifact identity
    /// under the same launch lock.
    pub(crate) async fn attach_created_child(
        &self,
        reserved: AppAgentToolReservedLaunch,
    ) -> Result<AppAgentToolPreparedChild, AppAgentToolLifecycleError> {
        let path = self.lifecycle_path(
            &reserved.scope,
            &reserved.task_id,
            &reserved.child_execution_id,
        );
        let _process_guard = process_lifecycle_lock(&path).lock_owned().await;
        let _guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(|error| AppAgentToolLifecycleError::Lock(error.to_string()))?;
        let mut document = self
            .read_document(&path)
            .await?
            .ok_or(AppAgentToolLifecycleError::CorruptLifecycle)?;
        if document.binding != reserved.binding {
            return Err(AppAgentToolLifecycleError::CorruptLifecycle);
        }
        if document.launch_phase == AppAgentToolLaunchPhase::Denied {
            return Err(AppAgentToolLifecycleError::ConflictingCancellation);
        }
        let state = self
            .read_and_validate_execution(
                &reserved.scope,
                &reserved.task_id,
                &reserved.child_execution_id,
                &document.binding,
            )
            .await?;
        if !execution_is_pre_start(state.status.as_str())
            && !execution_is_terminal(state.status.as_str())
        {
            return Err(AppAgentToolLifecycleError::ChildAlreadyStarted);
        }
        if document.launch_phase == AppAgentToolLaunchPhase::Reserved {
            document.launch_phase = AppAgentToolLaunchPhase::Attached;
            document.refresh_digest()?;
            self.write_document(&path, document.clone()).await?;
        }
        let handle = AppAgentChildRunHandle::restore(&document.binding)?;
        Ok(AppAgentToolPreparedChild {
            scope: reserved.scope,
            task_id: reserved.task_id,
            child_execution_id: reserved.child_execution_id,
            binding: document.binding,
            handle,
        })
    }

    /// Reopen only the persisted exact binding. Mutable agent definitions are
    /// never consulted during restart recovery.
    pub(crate) async fn reopen_launch(
        &self,
        scope: ScopeRef,
        task_id: &str,
        child_execution_id: &str,
    ) -> Result<AppAgentToolRecoveredLaunch, AppAgentToolLifecycleError> {
        validate_runtime_segment(child_execution_id)?;
        super::workspace::ArtifactV2Workspace::validate_task_id(task_id)?;
        let path = self.lifecycle_path(&scope, task_id, child_execution_id);
        let document = self
            .read_document(&path)
            .await?
            .ok_or(AppAgentToolLifecycleError::CorruptLifecycle)?;
        match document.launch_phase {
            AppAgentToolLaunchPhase::Reserved => Ok(AppAgentToolRecoveredLaunch::Reserved),
            AppAgentToolLaunchPhase::Attached => {
                self.read_and_validate_execution(
                    &scope,
                    task_id,
                    child_execution_id,
                    &document.binding,
                )
                .await?;
                let handle = AppAgentChildRunHandle::restore(&document.binding)?;
                Ok(AppAgentToolRecoveredLaunch::Attached(
                    AppAgentToolPreparedChild {
                        scope,
                        task_id: task_id.to_owned(),
                        child_execution_id: child_execution_id.to_owned(),
                        binding: document.binding,
                        handle,
                    },
                ))
            },
            AppAgentToolLaunchPhase::Denied => {
                Err(AppAgentToolLifecycleError::ConflictingCancellation)
            },
        }
    }

    /// Tombstone a reservation for which no canonical child shell exists.
    /// The durable deadline or current-authority denial is recorded before the
    /// governed parent is failed, so restart cannot silently retry the same
    /// unauthorized launch or create a different child from mutable catalog.
    pub(crate) async fn deny_reserved_launch(
        &self,
        reserved: AppAgentToolReservedLaunch,
        reason: AppAgentChildCancellationReason,
        now_ms: i64,
    ) -> Result<(), AppAgentToolLifecycleError> {
        if !matches!(
            reason,
            AppAgentChildCancellationReason::Deadline
                | AppAgentChildCancellationReason::GrantRevoked
        ) {
            return Err(AppAgentToolLifecycleError::ConflictingCancellation);
        }
        let path = self.lifecycle_path(
            &reserved.scope,
            &reserved.task_id,
            &reserved.child_execution_id,
        );
        let _process_guard = process_lifecycle_lock(&path).lock_owned().await;
        let _guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(|error| AppAgentToolLifecycleError::Lock(error.to_string()))?;
        let mut document = self
            .read_document(&path)
            .await?
            .ok_or(AppAgentToolLifecycleError::CorruptLifecycle)?;
        if document.binding != reserved.binding
            || document.launch_phase != AppAgentToolLaunchPhase::Reserved
            || document.terminal.is_some()
        {
            return Err(AppAgentToolLifecycleError::ConflictingCancellation);
        }
        let intent = match document.cancellation.as_ref() {
            Some(existing) if existing.reason == reason => existing.clone(),
            Some(_) => return Err(AppAgentToolLifecycleError::ConflictingCancellation),
            None => AppAgentChildCancellationIntent::mint(&document.binding, reason, now_ms, true)?,
        };
        document.cancellation = Some(intent);
        document.launch_phase = AppAgentToolLaunchPhase::Denied;
        document.refresh_digest()?;
        self.write_document(&path, document).await
    }

    /// Recover the sole not-yet-started launch owned by one exact parent.
    /// This includes both a reservation left before canonical child creation
    /// and an Attached child shell left before its scheduler task began. Two
    /// recoverable launches are corruption because an app parent executes one
    /// tool call at a time. The scan is bounded and returns the original sealed
    /// binding, never a reconstruction from the current agent catalog.
    pub(crate) async fn recoverable_launch_for_parent(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        parent_execution_id: &str,
    ) -> Result<Option<AppAgentToolReservedLaunch>, AppAgentToolLifecycleError> {
        super::workspace::ArtifactV2Workspace::validate_task_id(task_id)?;
        validate_runtime_segment(parent_execution_id)?;
        let directory = self
            .workspace
            .task_dir(&scope.principal(), &scope.workspace(), task_id)
            .join("app_agent_tool_launches");
        let entries = self.workspace.read_dir_path_or_empty(&directory).await?;
        if entries.len() > MAX_TASK_AGENT_TOOL_LAUNCHES {
            return Err(AppAgentToolLifecycleError::CorruptLifecycle);
        }
        let expected_parent = format!("execution:{parent_execution_id}");
        let mut recovered = None;
        for entry in entries {
            if !entry.is_dir {
                return Err(AppAgentToolLifecycleError::CorruptLifecycle);
            }
            validate_runtime_segment(&entry.file_name)?;
            let path = directory.join(&entry.file_name).join(LIFECYCLE_FILE);
            let document = self
                .read_document(&path)
                .await?
                .ok_or(AppAgentToolLifecycleError::CorruptLifecycle)?;
            document.binding.validate_integrity()?;
            if document.binding.parent_execution_ref().as_str() != expected_parent {
                continue;
            }
            let recoverable = document.cancellation.is_none()
                && match document.launch_phase {
                    AppAgentToolLaunchPhase::Reserved => true,
                    AppAgentToolLaunchPhase::Attached => {
                        let state = self
                            .read_and_validate_execution(
                                scope,
                                task_id,
                                &entry.file_name,
                                &document.binding,
                            )
                            .await?;
                        execution_is_pre_start(state.status.as_str())
                    },
                    AppAgentToolLaunchPhase::Denied => false,
                };
            if !recoverable {
                continue;
            }
            if recovered.is_some() {
                return Err(AppAgentToolLifecycleError::CorruptLifecycle);
            }
            recovered = Some(AppAgentToolReservedLaunch {
                scope: scope.clone(),
                task_id: task_id.to_owned(),
                child_execution_id: entry.file_name,
                binding: document.binding,
            });
        }
        Ok(recovered)
    }

    /// Detect the durable half of a reservation-denial transition. A crash can
    /// occur after the lifecycle tombstone is synced but before the governed
    /// parent is terminally failed. Denied launches are intentionally not
    /// adoptable, so startup uses this bounded probe to finish that exact
    /// parent transition without recreating a child or consulting catalog.
    pub(crate) async fn denied_reservation_for_parent(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        parent_execution_id: &str,
    ) -> Result<bool, AppAgentToolLifecycleError> {
        super::workspace::ArtifactV2Workspace::validate_task_id(task_id)?;
        validate_runtime_segment(parent_execution_id)?;
        let directory = self
            .workspace
            .task_dir(&scope.principal(), &scope.workspace(), task_id)
            .join("app_agent_tool_launches");
        let entries = self.workspace.read_dir_path_or_empty(&directory).await?;
        if entries.len() > MAX_TASK_AGENT_TOOL_LAUNCHES {
            return Err(AppAgentToolLifecycleError::CorruptLifecycle);
        }
        let expected_parent = format!("execution:{parent_execution_id}");
        let mut denied = false;
        for entry in entries {
            if !entry.is_dir {
                return Err(AppAgentToolLifecycleError::CorruptLifecycle);
            }
            validate_runtime_segment(&entry.file_name)?;
            let document = self
                .read_document(&directory.join(&entry.file_name).join(LIFECYCLE_FILE))
                .await?
                .ok_or(AppAgentToolLifecycleError::CorruptLifecycle)?;
            document.binding.validate_integrity()?;
            if document.binding.parent_execution_ref().as_str() != expected_parent
                || document.launch_phase != AppAgentToolLaunchPhase::Denied
            {
                continue;
            }
            if denied {
                return Err(AppAgentToolLifecycleError::CorruptLifecycle);
            }
            denied = true;
        }
        Ok(denied)
    }

    /// Recover the sole Attached nonterminal child whose cancellation intent
    /// was durable before process loss. It is deliberately excluded from
    /// launch adoption: startup must re-enter canonical leaf-tree cancellation
    /// rather than scheduling work after the owner requested stop.
    pub(crate) async fn cancellation_pending_for_parent(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        parent_execution_id: &str,
    ) -> Result<Option<String>, AppAgentToolLifecycleError> {
        super::workspace::ArtifactV2Workspace::validate_task_id(task_id)?;
        validate_runtime_segment(parent_execution_id)?;
        let directory = self
            .workspace
            .task_dir(&scope.principal(), &scope.workspace(), task_id)
            .join("app_agent_tool_launches");
        let entries = self.workspace.read_dir_path_or_empty(&directory).await?;
        if entries.len() > MAX_TASK_AGENT_TOOL_LAUNCHES {
            return Err(AppAgentToolLifecycleError::CorruptLifecycle);
        }
        let expected_parent = format!("execution:{parent_execution_id}");
        let mut pending = None;
        for entry in entries {
            if !entry.is_dir {
                return Err(AppAgentToolLifecycleError::CorruptLifecycle);
            }
            validate_runtime_segment(&entry.file_name)?;
            let document = self
                .read_document(&directory.join(&entry.file_name).join(LIFECYCLE_FILE))
                .await?
                .ok_or(AppAgentToolLifecycleError::CorruptLifecycle)?;
            document.binding.validate_integrity()?;
            if document.launch_phase != AppAgentToolLaunchPhase::Attached
                || document.cancellation.is_none()
                || document.binding.parent_execution_ref().as_str() != expected_parent
            {
                continue;
            }
            let state = self
                .read_and_validate_execution(scope, task_id, &entry.file_name, &document.binding)
                .await?;
            if execution_is_terminal(state.status.as_str()) {
                continue;
            }
            if pending.is_some() {
                return Err(AppAgentToolLifecycleError::CorruptLifecycle);
            }
            pending = Some(entry.file_name);
        }
        Ok(pending)
    }

    /// Enumerate ACKed carriers owned by an exact parent for startup replay.
    /// Only metadata and sealed bindings leave this method; result/control
    /// bytes remain behind `delivered_result` and its fresh authority proof.
    pub(crate) async fn delivered_children_for_parent(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        parent_execution_id: &str,
    ) -> Result<Vec<(String, AppAgentChildTaskBinding)>, AppAgentToolLifecycleError> {
        super::workspace::ArtifactV2Workspace::validate_task_id(task_id)?;
        validate_runtime_segment(parent_execution_id)?;
        let directory = self
            .workspace
            .task_dir(&scope.principal(), &scope.workspace(), task_id)
            .join("app_agent_tool_launches");
        let entries = self.workspace.read_dir_path_or_empty(&directory).await?;
        if entries.len() > MAX_TASK_AGENT_TOOL_LAUNCHES {
            return Err(AppAgentToolLifecycleError::CorruptLifecycle);
        }
        let expected_parent = format!("execution:{parent_execution_id}");
        let mut delivered = Vec::new();
        for entry in entries {
            if !entry.is_dir {
                return Err(AppAgentToolLifecycleError::CorruptLifecycle);
            }
            validate_runtime_segment(&entry.file_name)?;
            let document = self
                .read_document(&directory.join(&entry.file_name).join(LIFECYCLE_FILE))
                .await?
                .ok_or(AppAgentToolLifecycleError::CorruptLifecycle)?;
            document.binding.validate_integrity()?;
            if document.launch_phase != AppAgentToolLaunchPhase::Attached
                || document.binding.parent_execution_ref().as_str() != expected_parent
            {
                continue;
            }
            if let Some(terminal) = document.terminal.as_ref() {
                terminal.validate_integrity(&document.binding)?;
                if terminal.delivered() {
                    delivered.push((entry.file_name, document.binding));
                }
            }
        }
        Ok(delivered)
    }

    pub(crate) async fn reopen(
        &self,
        scope: ScopeRef,
        task_id: &str,
        child_execution_id: &str,
    ) -> Result<AppAgentToolPreparedChild, AppAgentToolLifecycleError> {
        match self
            .reopen_launch(scope, task_id, child_execution_id)
            .await?
        {
            AppAgentToolRecoveredLaunch::Attached(child) => Ok(child),
            AppAgentToolRecoveredLaunch::Reserved => {
                Err(AppAgentToolLifecycleError::ChildNotAttached)
            },
        }
    }

    /// Read-only execution-owner lookup used by the canonical artifact writer
    /// to tag only the exact named JSON result. No handle or dispatch authority
    /// is returned.
    pub(crate) async fn attached_binding(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        child_execution_id: &str,
    ) -> Result<Option<AppAgentChildTaskBinding>, AppAgentToolLifecycleError> {
        let path = self.lifecycle_path(scope, task_id, child_execution_id);
        let Some(document) = self.read_document(&path).await? else {
            return Ok(None);
        };
        if document.launch_phase != AppAgentToolLaunchPhase::Attached {
            return Ok(None);
        }
        self.read_and_validate_execution(scope, task_id, child_execution_id, &document.binding)
            .await?;
        Ok(Some(document.binding))
    }

    pub(crate) async fn request_cancellation(
        &self,
        child: &AppAgentToolPreparedChild,
        reason: AppAgentChildCancellationReason,
        now_ms: i64,
    ) -> Result<
        (
            AppAgentChildCancellationDisposition,
            AppAgentChildCancellationPermit,
        ),
        AppAgentToolLifecycleError,
    > {
        let path = self.lifecycle_path(&child.scope, &child.task_id, &child.child_execution_id);
        let _process_guard = process_lifecycle_lock(&path).lock_owned().await;
        let _guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(|error| AppAgentToolLifecycleError::Lock(error.to_string()))?;
        let mut document = self
            .read_document(&path)
            .await?
            .ok_or(AppAgentToolLifecycleError::CorruptLifecycle)?;
        if document.launch_phase != AppAgentToolLaunchPhase::Attached
            || &document.binding != &child.binding
            || document.terminal.is_some()
        {
            return Err(AppAgentToolLifecycleError::ConflictingCancellation);
        }
        let state = self
            .read_and_validate_execution(
                &child.scope,
                &child.task_id,
                &child.child_execution_id,
                &child.binding,
            )
            .await?;
        if execution_is_terminal(state.status.as_str()) {
            return Err(AppAgentToolLifecycleError::ConflictingCancellation);
        }
        let (disposition, _intent) = match document.cancellation.as_ref() {
            Some(existing) if existing.reason == reason => (
                AppAgentChildCancellationDisposition::AlreadyRequested,
                existing.clone(),
            ),
            Some(_) => return Err(AppAgentToolLifecycleError::ConflictingCancellation),
            None => {
                let intent = AppAgentChildCancellationIntent::mint(
                    &child.binding,
                    reason,
                    now_ms,
                    execution_is_pre_start(state.status.as_str()),
                )?;
                document.cancellation = Some(intent.clone());
                document.refresh_digest()?;
                self.write_document(&path, document).await?;
                (AppAgentChildCancellationDisposition::Requested, intent)
            },
        };
        Ok((
            disposition,
            AppAgentChildCancellationPermit {
                child_execution_ref: child.binding.child_execution_ref().clone(),
            },
        ))
    }

    /// Reconcile canonical terminal state and persist exactly one labeled
    /// outbox record. Failed or post-start-cancelled children are projected as
    /// uncertain until the shared effect owner can prove clean settlement.
    pub(crate) async fn reconcile_terminal(
        &self,
        child: AppAgentToolPreparedChild,
        effect_settlement: Option<AppAgentChildEffectSettlementProof>,
        cancellation_settlement: Option<AppAgentChildCancellationSettlementProof>,
        current_authority: Option<AppAgentChildCurrentAuthorityProof>,
    ) -> Result<Option<AppAgentToolPendingNotification>, AppAgentToolLifecycleError> {
        let path = self.lifecycle_path(&child.scope, &child.task_id, &child.child_execution_id);
        let _process_guard = process_lifecycle_lock(&path).lock_owned().await;
        let _guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(|error| AppAgentToolLifecycleError::Lock(error.to_string()))?;
        let mut document = self
            .read_document(&path)
            .await?
            .ok_or(AppAgentToolLifecycleError::CorruptLifecycle)?;
        if document.launch_phase != AppAgentToolLaunchPhase::Attached
            || &document.binding != &child.binding
        {
            return Err(AppAgentToolLifecycleError::ConflictingTerminalSettlement);
        }
        let state = self
            .read_and_validate_execution(
                &child.scope,
                &child.task_id,
                &child.child_execution_id,
                &child.binding,
            )
            .await?;
        if !execution_is_terminal(state.status.as_str()) {
            return Err(AppAgentToolLifecycleError::ChildNotTerminal);
        }
        if (state.status != "completed" && effect_settlement.is_some())
            || (!matches!(state.status.as_str(), "cancelled" | "canceled")
                && cancellation_settlement.is_some())
        {
            return Err(AppAgentToolLifecycleError::ConflictingTerminalSettlement);
        }
        if let Some(record) = document.terminal.as_ref() {
            if record.delivered() {
                return Ok(None);
            }
            let observation = observation_from_record(&child.binding, record)?;
            let carrier = AppAgentChildControlCarrier::from_artifact_observation(
                &child.binding,
                child.handle,
                observation,
            )?;
            return Ok(Some(AppAgentToolPendingNotification { carrier }));
        }

        match state.status.as_str() {
            "completed" => {
                if current_authority
                    .as_ref()
                    .is_some_and(|proof| current_authority_matches(&child.binding, proof))
                {
                    let settlement = effect_settlement
                        .as_ref()
                        .ok_or(AppAgentToolLifecycleError::EffectSettlementPending)?;
                    if &settlement.launch_digest != child.binding.launch_digest()
                        || &settlement.child_execution_ref != child.binding.child_execution_ref()
                    {
                        return Err(AppAgentToolLifecycleError::ConflictingTerminalSettlement);
                    }
                    if !settlement.all_effects_settled && !settlement.outcome_uncertain {
                        return Err(AppAgentToolLifecycleError::EffectSettlementPending);
                    }
                }
            },
            "cancelled" | "canceled" => {
                if let Some(intent) = document.cancellation.as_ref() {
                    if let Some(settlement) = cancellation_settlement.as_ref() {
                        if &settlement.launch_digest != child.binding.launch_digest()
                            || &settlement.child_execution_ref
                                != child.binding.child_execution_ref()
                            || &settlement.request_digest != &intent.request_digest
                        {
                            return Err(AppAgentToolLifecycleError::ConflictingTerminalSettlement);
                        }
                        if !settlement.stopped_before_start && !settlement.outcome_uncertain {
                            return Err(AppAgentToolLifecycleError::CancellationSettlementPending);
                        }
                    }
                } else if cancellation_settlement.is_some() {
                    return Err(AppAgentToolLifecycleError::ConflictingTerminalSettlement);
                }
            },
            _ => {},
        }

        let observation = self
            .observe_terminal(
                &child,
                &state,
                document.cancellation.as_ref(),
                effect_settlement.as_ref(),
                cancellation_settlement.as_ref(),
                current_authority.as_ref(),
            )
            .await?;
        let carrier = AppAgentChildControlCarrier::from_artifact_observation(
            &child.binding,
            child.handle,
            observation,
        )?;
        let replay_observation = observation_from_carrier(&child.binding, &carrier)?;
        let replay_handle = AppAgentChildRunHandle::restore(&child.binding)?;
        let replay_carrier = AppAgentChildControlCarrier::from_artifact_observation(
            &child.binding,
            replay_handle,
            replay_observation,
        )?;
        let record = AppAgentChildControlRecord::pending(replay_carrier)?;
        record.validate_integrity(&child.binding)?;
        document.terminal = Some(record);
        document.refresh_digest()?;
        self.write_document(&path, document).await?;
        Ok(Some(AppAgentToolPendingNotification { carrier }))
    }

    /// Append the deterministic, payload-minimal parent control row. The
    /// file path and bytes are derived from the sealed launch, so replay after
    /// any crash window returns the same acknowledgement rather than creating
    /// a second notification. Result JSON remains only in the lifecycle
    /// outbox and is not duplicated into the parent event.
    pub(crate) async fn append_parent_notification(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        child_execution_id: &str,
        binding: &AppAgentChildTaskBinding,
        pending: &AppAgentToolPendingNotification,
    ) -> Result<AppAgentParentNotificationAck, AppAgentToolLifecycleError> {
        super::workspace::ArtifactV2Workspace::validate_task_id(task_id)?;
        validate_runtime_segment(child_execution_id)?;
        binding.validate_integrity()?;
        let lifecycle_path = self.lifecycle_path(scope, task_id, child_execution_id);
        let parent_execution_id = binding
            .parent_execution_ref()
            .as_str()
            .strip_prefix("execution:")
            .ok_or(AppAgentToolLifecycleError::InvalidParentNotificationAck)?;
        validate_runtime_segment(parent_execution_id)?;
        let notification_dir = self
            .workspace
            .execution_dir(
                &scope.principal(),
                &scope.workspace(),
                task_id,
                parent_execution_id,
            )
            .join(PARENT_NOTIFICATION_DIR)
            .join(child_execution_id);
        self.workspace
            .create_dir_all_path(&notification_dir)
            .await?;
        let notification_path = notification_dir.join(PARENT_NOTIFICATION_FILE);
        let _process_guard = process_lifecycle_lock(&lifecycle_path).lock_owned().await;
        let _guard = AgentStorage::acquire_file_lock_exclusive(&lifecycle_path)
            .await
            .map_err(|error| AppAgentToolLifecycleError::Lock(error.to_string()))?;
        let document = self
            .read_document(&lifecycle_path)
            .await?
            .ok_or(AppAgentToolLifecycleError::CorruptLifecycle)?;
        if document.launch_phase != AppAgentToolLaunchPhase::Attached
            || &document.binding != binding
            || document.terminal.as_ref().is_none_or(|terminal| {
                terminal.notification_ref() != pending.carrier().notification_ref()
                    || terminal.carrier_digest() != pending.carrier().carrier_digest()
            })
        {
            return Err(AppAgentToolLifecycleError::InvalidParentNotificationAck);
        }
        let expected =
            AppAgentParentNotificationRecord::from_carrier(&document.binding, pending.carrier())?;
        if self
            .workspace
            .metadata_path(&notification_path)
            .await?
            .is_some()
        {
            let retained = self
                .workspace
                .read_json_bounded_stream_path::<AppAgentParentNotificationRecord, _>(
                    &notification_path,
                    u64::try_from(MAX_LIFECYCLE_BYTES).unwrap_or(u64::MAX),
                    MAX_LIFECYCLE_DEPTH,
                    MAX_LIFECYCLE_NODES,
                )
                .await?;
            retained.validate(&document.binding)?;
            if retained != expected {
                return Err(AppAgentToolLifecycleError::InvalidParentNotificationAck);
            }
        } else {
            self.workspace
                .write_json_value_atomic_stream_path(
                    &notification_path,
                    expected.clone(),
                    MAX_LIFECYCLE_BYTES,
                )
                .await?;
        }
        let parent_event_ref = AppReference::parse(format!(
            "artifact-event:agent-tool:{}",
            expected
                .record_digest
                .as_str()
                .trim_start_matches("blake3:")
        ))?;
        Ok(AppAgentParentNotificationAck {
            notification_ref: expected.notification_ref,
            carrier_digest: expected.carrier_digest,
            parent_event_ref,
            parent_event_digest: expected.record_digest,
        })
    }

    pub(crate) async fn mark_parent_notified(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        child_execution_id: &str,
        acknowledgement: AppAgentParentNotificationAck,
    ) -> Result<(), AppAgentToolLifecycleError> {
        super::workspace::ArtifactV2Workspace::validate_task_id(task_id)?;
        validate_runtime_segment(child_execution_id)?;
        let path = self.lifecycle_path(scope, task_id, child_execution_id);
        let _process_guard = process_lifecycle_lock(&path).lock_owned().await;
        let _guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(|error| AppAgentToolLifecycleError::Lock(error.to_string()))?;
        let mut document = self
            .read_document(&path)
            .await?
            .ok_or(AppAgentToolLifecycleError::CorruptLifecycle)?;
        if document.launch_phase != AppAgentToolLaunchPhase::Attached {
            return Err(AppAgentToolLifecycleError::ChildNotAttached);
        }
        let terminal = document
            .terminal
            .as_mut()
            .ok_or(AppAgentToolLifecycleError::InvalidParentNotificationAck)?;
        terminal.mark_delivered(acknowledgement)?;
        document.refresh_digest()?;
        self.write_document(&path, document).await
    }

    /// Read the already-delivered terminal projection for the waiting parent.
    /// Completed values remain fenced by freshly minted authority on every
    /// read; cancellation and uncertainty carry only reviewed control labels.
    pub(crate) async fn delivered_result(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        child_execution_id: &str,
        current_authority: Option<&AppAgentChildCurrentAuthorityProof>,
    ) -> Result<Option<AppAgentToolDeliveredResult>, AppAgentToolLifecycleError> {
        let path = self.lifecycle_path(scope, task_id, child_execution_id);
        let document = self
            .read_document(&path)
            .await?
            .ok_or(AppAgentToolLifecycleError::CorruptLifecycle)?;
        if document.launch_phase != AppAgentToolLaunchPhase::Attached {
            return Err(AppAgentToolLifecycleError::ChildNotAttached);
        }
        self.read_and_validate_execution(scope, task_id, child_execution_id, &document.binding)
            .await?;
        let Some(record) = document.terminal.as_ref() else {
            return Ok(None);
        };
        record.validate_integrity(&document.binding)?;
        if !record.delivered() {
            return Ok(None);
        }
        if !current_authority
            .is_some_and(|proof| current_authority_matches(&document.binding, proof))
        {
            return Err(AppAgentToolLifecycleError::ResultDisclosureDenied);
        }
        let (
            status,
            value,
            _value_digest,
            _result_artifact_ref,
            _result_artifact_digest,
            failure_code,
            _terminal_settlement_digest,
            handling_labels,
            _cancellation_settled,
            _effect_uncertain,
        ) = record.terminal_material();
        match status {
            AppAgentChildControlStatus::Completed => {
                Ok(Some(AppAgentToolDeliveredResult::Completed {
                    value: value.ok_or(AppAgentToolLifecycleError::CorruptLifecycle)?,
                    handling_labels,
                    carrier_digest: record.carrier_digest().clone(),
                }))
            },
            AppAgentChildControlStatus::Cancelled => {
                Ok(Some(AppAgentToolDeliveredResult::Cancelled {
                    handling_labels,
                    carrier_digest: record.carrier_digest().clone(),
                }))
            },
            AppAgentChildControlStatus::OutcomeUncertain => {
                Ok(Some(AppAgentToolDeliveredResult::OutcomeUncertain {
                    failure_code: failure_code
                        .ok_or(AppAgentToolLifecycleError::CorruptLifecycle)?,
                    handling_labels,
                    carrier_digest: record.carrier_digest().clone(),
                }))
            },
        }
    }

    /// Metadata-only readiness probe for the live waiter and startup owner.
    /// It does not materialize result/control payloads and therefore cannot be
    /// used to bypass the fresh authority proof required by `delivered_result`.
    pub(crate) async fn parent_notification_delivered(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        child_execution_id: &str,
    ) -> Result<bool, AppAgentToolLifecycleError> {
        let path = self.lifecycle_path(scope, task_id, child_execution_id);
        let document = self
            .read_document(&path)
            .await?
            .ok_or(AppAgentToolLifecycleError::CorruptLifecycle)?;
        if document.launch_phase != AppAgentToolLaunchPhase::Attached {
            return Ok(false);
        }
        document.binding.validate_integrity()?;
        Ok(document
            .terminal
            .as_ref()
            .is_some_and(AppAgentChildControlRecord::delivered))
    }

    async fn observe_terminal(
        &self,
        child: &AppAgentToolPreparedChild,
        state: &ExecutionState,
        cancellation: Option<&AppAgentChildCancellationIntent>,
        effect_settlement: Option<&AppAgentChildEffectSettlementProof>,
        cancellation_settlement: Option<&AppAgentChildCancellationSettlementProof>,
        current_authority: Option<&AppAgentChildCurrentAuthorityProof>,
    ) -> Result<AppAgentChildTerminalObservation, AppAgentToolLifecycleError> {
        let state_digest = AppDigest::blake3(&canonical_json_bytes(&serde_json::to_value(state)?)?);
        match state.status.as_str() {
            "completed"
                if cancellation.is_none()
                    && current_authority
                        .is_some_and(|proof| current_authority_matches(&child.binding, proof))
                    && effect_settlement.is_some_and(|settlement| {
                        &settlement.launch_digest == child.binding.launch_digest()
                            && &settlement.child_execution_ref
                                == child.binding.child_execution_ref()
                            && settlement.all_effects_settled
                            && !settlement.outcome_uncertain
                    }) =>
            {
                let settlement = effect_settlement
                    .ok_or(AppAgentToolLifecycleError::ConflictingTerminalSettlement)?;
                let authority =
                    current_authority.ok_or(AppAgentToolLifecycleError::ResultDisclosureDenied)?;
                let (artifact_ref, artifact_digest, value) = match self
                    .read_unique_result_artifact(
                        &child.scope,
                        &child.task_id,
                        &child.child_execution_id,
                        &child.binding,
                    )
                    .await
                {
                    Ok(result) => result,
                    // The canonical child is already durably terminal. Its
                    // declared result bytes are immutable at this point, so a
                    // missing, duplicate, malformed, oversized or schema-
                    // invalid artifact cannot become valid by retrying the
                    // reducer forever. Settle a payload-free conservative
                    // carrier instead; no rejected artifact identity or bytes
                    // cross the owner boundary.
                    Err(_) => {
                        return invalid_completed_result_observation(
                            &child.binding,
                            &state_digest,
                            &settlement.settlement_digest,
                        );
                    },
                };
                let provenance_digest =
                    AppDigest::blake3(&canonical_json_bytes(&serde_json::json!({
                        "schema": "magician.app-agent-tool-result-provenance.v1",
                        "launch_digest": child.binding.launch_digest(),
                        "execution_state_digest": state_digest,
                        "effect_settlement_digest": &settlement.settlement_digest,
                        "authority_fence_digest": &authority.authority_fence_digest,
                        "result_artifact_ref": &artifact_ref,
                        "result_artifact_digest": &artifact_digest,
                    }))?);
                let labels = child
                    .binding
                    .tool_binding()
                    .contract()
                    .result_labels(&value, provenance_digest)?;
                Ok(AppAgentChildTerminalObservation {
                    launch_digest: child.binding.launch_digest().clone(),
                    status: AppAgentChildControlStatus::Completed,
                    value: Some(value),
                    value_digest: Some(artifact_digest.clone()),
                    result_artifact_ref: Some(artifact_ref),
                    result_artifact_digest: Some(artifact_digest),
                    failure_code: None,
                    terminal_settlement_digest: Some(settlement.settlement_digest.clone()),
                    handling_labels: labels,
                    cancellation_settled: false,
                    effect_uncertain: false,
                })
            },
            "cancelled" | "canceled"
                if cancellation.zip(cancellation_settlement).is_some_and(
                    |(intent, settlement)| {
                        intent.before_child_start
                            && &settlement.launch_digest == child.binding.launch_digest()
                            && &settlement.child_execution_ref
                                == child.binding.child_execution_ref()
                            && &settlement.request_digest == &intent.request_digest
                            && settlement.stopped_before_start
                            && !settlement.outcome_uncertain
                    },
                ) =>
            {
                let settlement = cancellation_settlement
                    .ok_or(AppAgentToolLifecycleError::ConflictingTerminalSettlement)?;
                let provenance = terminal_control_provenance(
                    child.binding.launch_digest(),
                    &state_digest,
                    cancellation.map(|intent| &intent.request_digest),
                    "cancelled_before_start",
                    Some(&settlement.settlement_digest),
                )?;
                let labels = child
                    .binding
                    .tool_binding()
                    .contract()
                    .control_labels(provenance)?;
                Ok(AppAgentChildTerminalObservation {
                    launch_digest: child.binding.launch_digest().clone(),
                    status: AppAgentChildControlStatus::Cancelled,
                    value: None,
                    value_digest: None,
                    result_artifact_ref: None,
                    result_artifact_digest: None,
                    failure_code: None,
                    terminal_settlement_digest: Some(settlement.settlement_digest.clone()),
                    handling_labels: labels,
                    cancellation_settled: true,
                    effect_uncertain: false,
                })
            },
            status => {
                let code = match status {
                    "completed" => "child_effect_settlement_unproven",
                    "failed" => "child_execution_failed_uncertain",
                    "cancelled" | "canceled" => "child_cancellation_effect_uncertain",
                    _ => "child_terminal_outcome_uncertain",
                };
                let terminal_settlement_digest = match status {
                    "completed" => {
                        effect_settlement.map(|settlement| settlement.settlement_digest.clone())
                    },
                    "cancelled" | "canceled" => cancellation_settlement
                        .map(|settlement| settlement.settlement_digest.clone()),
                    _ => None,
                };
                let provenance = terminal_control_provenance(
                    child.binding.launch_digest(),
                    &state_digest,
                    cancellation.map(|intent| &intent.request_digest),
                    code,
                    terminal_settlement_digest.as_ref(),
                )?;
                let labels = child
                    .binding
                    .tool_binding()
                    .contract()
                    .control_labels(provenance)?;
                Ok(AppAgentChildTerminalObservation {
                    launch_digest: child.binding.launch_digest().clone(),
                    status: AppAgentChildControlStatus::OutcomeUncertain,
                    value: None,
                    value_digest: None,
                    result_artifact_ref: None,
                    result_artifact_digest: None,
                    failure_code: Some(AppName::parse(code)?),
                    terminal_settlement_digest,
                    handling_labels: labels,
                    cancellation_settled: false,
                    effect_uncertain: true,
                })
            },
        }
    }

    async fn read_unique_result_artifact(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        child_execution_id: &str,
        binding: &AppAgentChildTaskBinding,
    ) -> Result<(AppReference, AppDigest, Value), AppAgentToolLifecycleError> {
        let records = self
            .artifacts
            .list_artifacts(scope, task_id, child_execution_id)
            .await?;
        let mut candidates = records.iter().filter(|record| {
            result_record_is_candidate(record, task_id, child_execution_id, binding)
        });
        let record = candidates
            .next()
            .ok_or(AppAgentToolLifecycleError::TypedResultArtifactUnavailable)?;
        if candidates.next().is_some() {
            return Err(AppAgentToolLifecycleError::TypedResultArtifactUnavailable);
        }
        let relative = record
            .payload
            .get("execution_relative_path")
            .and_then(Value::as_str)
            .ok_or(AppAgentToolLifecycleError::TypedResultArtifactUnavailable)?;
        let file_name = relative
            .strip_prefix("outputs/")
            .filter(|name| !name.is_empty() && !name.contains('/') && !name.contains('\\'))
            .ok_or(AppAgentToolLifecycleError::TypedResultArtifactUnavailable)?;
        let expected_path = self
            .workspace
            .execution_outputs_dir(
                &scope.principal(),
                &scope.workspace(),
                task_id,
                child_execution_id,
            )
            .join(file_name);
        let recorded_path = record
            .payload
            .get("execution_absolute_path")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .ok_or(AppAgentToolLifecycleError::TypedResultArtifactUnavailable)?;
        if recorded_path != expected_path
            || self.workspace.canonicalize_path(&recorded_path).await?
                != self.workspace.canonicalize_path(&expected_path).await?
        {
            return Err(AppAgentToolLifecycleError::TypedResultArtifactUnavailable);
        }
        let max_bytes = binding.tool_binding().contract().max_result_bytes();
        if record
            .payload
            .get("size_bytes")
            .and_then(Value::as_u64)
            .is_none_or(|size| size == 0 || size > max_bytes)
        {
            return Err(AppAgentToolLifecycleError::TypedResultArtifactUnavailable);
        }
        let value = self
            .workspace
            .read_json_bounded_stream_path::<Value, _>(
                &expected_path,
                max_bytes,
                MAX_RESULT_JSON_DEPTH,
                MAX_RESULT_JSON_NODES,
            )
            .await?;
        binding.tool_binding().contract().validate_result(&value)?;
        if exact_json_encoded_len(&value) > usize::try_from(max_bytes).unwrap_or(usize::MAX) {
            return Err(AppAgentToolLifecycleError::TypedResultArtifactUnavailable);
        }
        let digest = AppDigest::blake3(&canonical_json_bytes(&value)?);
        Ok((
            AppReference::parse(record.artifact_id.clone())?,
            digest,
            value,
        ))
    }

    async fn read_and_validate_execution(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        child_execution_id: &str,
        binding: &AppAgentChildTaskBinding,
    ) -> Result<ExecutionState, AppAgentToolLifecycleError> {
        let path = self.workspace.execution_state_path(
            &scope.principal(),
            &scope.workspace(),
            task_id,
            child_execution_id,
        );
        let state = self
            .workspace
            .read_json_bounded_stream_path::<ExecutionState, _>(
                path,
                MAX_EXECUTION_STATE_BYTES,
                MAX_EXECUTION_STATE_DEPTH,
                MAX_EXECUTION_STATE_NODES,
            )
            .await?;
        let parent_execution_id = state
            .parent_execution_id
            .as_deref()
            .ok_or(AppAgentToolLifecycleError::ChildIdentityMismatch)?;
        let expected_task_ref = artifact_task_ref(task_id)?;
        let expected_child_ref = artifact_execution_ref(child_execution_id)?;
        let expected_parent_ref = artifact_execution_ref(parent_execution_id)?;
        let expected_scope_binding_ref = artifact_scope_binding_ref(scope)?;
        if state.task_id != task_id
            || state.execution_id != child_execution_id
            || !matches!(
                state.relationship_type.as_str(),
                "delegate" | "delegated" | "delegated_child"
            )
            || binding.parent_task_ref() != &expected_task_ref
            || binding.child_task_ref() != &expected_task_ref
            || binding.child_execution_ref() != &expected_child_ref
            || binding.parent_execution_ref() != &expected_parent_ref
            || binding.scope_binding_ref() != &expected_scope_binding_ref
            || state.agent_id.as_str() != binding.tool_binding().agent().runtime_agent_id.as_str()
        {
            return Err(AppAgentToolLifecycleError::ChildIdentityMismatch);
        }
        Ok(state)
    }

    fn lifecycle_path(&self, scope: &ScopeRef, task_id: &str, child_execution_id: &str) -> PathBuf {
        self.workspace
            .task_dir(&scope.principal(), &scope.workspace(), task_id)
            .join("app_agent_tool_launches")
            .join(child_execution_id)
            .join(LIFECYCLE_FILE)
    }

    async fn read_document(
        &self,
        path: &Path,
    ) -> Result<Option<AppAgentToolLifecycleDocument>, AppAgentToolLifecycleError> {
        if self.workspace.metadata_path(path).await?.is_none() {
            return Ok(None);
        }
        let document = self
            .workspace
            .read_json_bounded_stream_path::<AppAgentToolLifecycleDocument, _>(
                path,
                u64::try_from(MAX_LIFECYCLE_BYTES).unwrap_or(u64::MAX),
                MAX_LIFECYCLE_DEPTH,
                MAX_LIFECYCLE_NODES,
            )
            .await?;
        document.validate()?;
        Ok(Some(document))
    }

    async fn write_document(
        &self,
        path: &Path,
        document: AppAgentToolLifecycleDocument,
    ) -> Result<(), AppAgentToolLifecycleError> {
        document.validate()?;
        self.workspace
            .write_json_value_atomic_stream_path(path, document, MAX_LIFECYCLE_BYTES)
            .await?;
        Ok(())
    }
}

fn invalid_completed_result_observation(
    binding: &AppAgentChildTaskBinding,
    state_digest: &AppDigest,
    settlement_digest: &AppDigest,
) -> Result<AppAgentChildTerminalObservation, AppAgentToolLifecycleError> {
    let code = "child_result_artifact_invalid";
    let provenance = terminal_control_provenance(
        binding.launch_digest(),
        state_digest,
        None,
        code,
        Some(settlement_digest),
    )?;
    let labels = binding
        .tool_binding()
        .contract()
        .control_labels(provenance)?;
    Ok(AppAgentChildTerminalObservation {
        launch_digest: binding.launch_digest().clone(),
        status: AppAgentChildControlStatus::OutcomeUncertain,
        value: None,
        value_digest: None,
        result_artifact_ref: None,
        result_artifact_digest: None,
        failure_code: Some(AppName::parse(code)?),
        terminal_settlement_digest: Some(settlement_digest.clone()),
        handling_labels: labels,
        cancellation_settled: false,
        effect_uncertain: true,
    })
}

fn result_record_is_candidate(
    record: &PersistedExecutionArtifactRecord,
    task_id: &str,
    child_execution_id: &str,
    binding: &AppAgentChildTaskBinding,
) -> bool {
    record.artifact_type == "tool_output_file"
        && record.content_type == "application/json"
        && record.source_execution_id.is_none()
        && record.source_artifact_id.is_none()
        && record.payload.get("artifact_kind").and_then(Value::as_str) == Some("tool_output_file")
        && record.payload.get("artifact_id").and_then(Value::as_str)
            == Some(record.artifact_id.as_str())
        && record.payload.get("content_type").and_then(Value::as_str) == Some("application/json")
        && record.payload.get("task_id").and_then(Value::as_str) == Some(task_id)
        && record.payload.get("execution_id").and_then(Value::as_str) == Some(child_execution_id)
        && record
            .payload
            .get("app_agent_tool_result_name")
            .and_then(Value::as_str)
            == Some(binding.tool_binding().contract().result_artifact_name())
        && record
            .payload
            .get("app_agent_tool_contract_digest")
            .and_then(Value::as_str)
            == Some(binding.tool_binding().contract().digest().as_str())
        && record
            .payload
            .get("app_agent_tool_launch_digest")
            .and_then(Value::as_str)
            == Some(binding.launch_digest().as_str())
}

fn observation_from_record(
    binding: &AppAgentChildTaskBinding,
    record: &AppAgentChildControlRecord,
) -> Result<AppAgentChildTerminalObservation, AppAgentToolLifecycleError> {
    record.validate_integrity(binding)?;
    let (
        status,
        value,
        value_digest,
        result_artifact_ref,
        result_artifact_digest,
        failure_code,
        terminal_settlement_digest,
        handling_labels,
        cancellation_settled,
        effect_uncertain,
    ) = record.terminal_material();
    Ok(AppAgentChildTerminalObservation {
        launch_digest: binding.launch_digest().clone(),
        status,
        value,
        value_digest,
        result_artifact_ref,
        result_artifact_digest,
        failure_code,
        terminal_settlement_digest,
        handling_labels,
        cancellation_settled,
        effect_uncertain,
    })
}

fn observation_from_carrier(
    binding: &AppAgentChildTaskBinding,
    carrier: &AppAgentChildControlCarrier,
) -> Result<AppAgentChildTerminalObservation, AppAgentToolLifecycleError> {
    let (
        status,
        value,
        value_digest,
        result_artifact_ref,
        result_artifact_digest,
        failure_code,
        terminal_settlement_digest,
        handling_labels,
        cancellation_settled,
        effect_uncertain,
    ) = carrier.terminal_material();
    Ok(AppAgentChildTerminalObservation {
        launch_digest: binding.launch_digest().clone(),
        status,
        value,
        value_digest,
        result_artifact_ref,
        result_artifact_digest,
        failure_code,
        terminal_settlement_digest,
        handling_labels,
        cancellation_settled,
        effect_uncertain,
    })
}

fn terminal_control_provenance(
    launch_digest: &AppDigest,
    execution_state_digest: &AppDigest,
    cancellation_digest: Option<&AppDigest>,
    terminal_code: &str,
    terminal_settlement_digest: Option<&AppDigest>,
) -> Result<AppDigest, AppAgentToolLifecycleError> {
    Ok(AppDigest::blake3(&canonical_json_bytes(
        &serde_json::json!({
            "schema": "magician.app-agent-tool-control-provenance.v2",
            "launch_digest": launch_digest,
            "execution_state_digest": execution_state_digest,
            "cancellation_digest": cancellation_digest,
            "terminal_code": terminal_code,
            "terminal_settlement_digest": terminal_settlement_digest,
        }),
    )?))
}

fn current_authority_matches(
    binding: &AppAgentChildTaskBinding,
    proof: &AppAgentChildCurrentAuthorityProof,
) -> bool {
    &proof.launch_digest == binding.launch_digest()
        && &proof.scope_binding_ref == binding.scope_binding_ref()
        && &proof.workflow_authority_digest == binding.workflow_authority_digest()
        && proof.result_disclosure_allowed
        && !proof.revoked
}

fn execution_is_pre_start(status: &str) -> bool {
    matches!(status, "pending" | "ready" | "queued")
}

fn execution_is_terminal(status: &str) -> bool {
    matches!(
        status,
        "completed" | "failed" | "cancelled" | "canceled" | "uncertain" | "archived"
    )
}

fn validate_runtime_segment(value: &str) -> Result<(), AppAgentToolLifecycleError> {
    if value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(AppAgentToolLifecycleError::ChildIdentityMismatch);
    }
    Ok(())
}

pub(crate) fn artifact_task_ref(task_id: &str) -> Result<AppReference, AppAgentToolLifecycleError> {
    super::workspace::ArtifactV2Workspace::validate_task_id(task_id)?;
    Ok(AppReference::parse(format!("task:{task_id}"))?)
}

pub(crate) fn artifact_execution_ref(
    execution_id: &str,
) -> Result<AppReference, AppAgentToolLifecycleError> {
    validate_runtime_segment(execution_id)?;
    Ok(AppReference::parse(format!("execution:{execution_id}"))?)
}

fn artifact_scope_binding_ref(
    scope: &ScopeRef,
) -> Result<AppScopeBindingRef, AppAgentToolLifecycleError> {
    let principal = AppReference::parse(scope.principal().to_string())?;
    let workspace = AppReference::parse(scope.workspace().to_string())?;
    let digest = AppDigest::blake3(format!("{principal}\0{workspace}").as_bytes());
    Ok(AppScopeBindingRef::parse(format!(
        "scope_{}",
        digest.as_str().trim_start_matches("blake3:")
    ))?)
}

#[cfg(test)]
impl AppAgentChildTerminalObservation {
    pub(crate) fn completed_for_test(
        binding: &AppAgentChildTaskBinding,
        value: Value,
    ) -> Result<Self, AppAgentToolLifecycleError> {
        let value_digest = AppDigest::blake3(&canonical_json_bytes(&value)?);
        let labels = binding.tool_binding().contract().result_labels(
            &value,
            AppDigest::blake3(b"agent-tool-terminal-test-provenance"),
        )?;
        Ok(Self {
            launch_digest: binding.launch_digest().clone(),
            status: AppAgentChildControlStatus::Completed,
            value: Some(value),
            value_digest: Some(value_digest.clone()),
            result_artifact_ref: Some(AppReference::parse("tool_output_file:test-result")?),
            result_artifact_digest: Some(value_digest),
            failure_code: None,
            terminal_settlement_digest: Some(AppDigest::blake3(
                b"agent-tool-terminal-test-settlement",
            )),
            handling_labels: labels,
            cancellation_settled: false,
            effect_uncertain: false,
        })
    }
}

#[cfg(test)]
impl AppAgentParentNotificationAck {
    pub(crate) fn for_test(carrier: &AppAgentChildControlCarrier) -> Self {
        Self {
            notification_ref: carrier.notification_ref().clone(),
            carrier_digest: carrier.carrier_digest().clone(),
            parent_event_ref: AppReference::parse("artifact-event:test-parent-notification")
                .expect("static app reference"),
            parent_event_digest: AppDigest::blake3(b"test-parent-notification"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_observation_and_notification_ack_are_not_wire_authority() {
        static_assertions::assert_not_impl_any!(
            AppAgentChildTerminalObservation: serde::Serialize, serde::de::DeserializeOwned, Clone
        );
        static_assertions::assert_not_impl_any!(
            AppAgentParentNotificationAck: serde::Serialize, serde::de::DeserializeOwned, Clone
        );
        static_assertions::assert_not_impl_any!(
            AppAgentChildCancellationPermit: serde::Serialize, serde::de::DeserializeOwned, Clone
        );
        static_assertions::assert_not_impl_any!(
            AppAgentChildEffectSettlementProof: serde::Serialize, serde::de::DeserializeOwned, Clone
        );
        static_assertions::assert_not_impl_any!(
            AppAgentChildCancellationSettlementProof: serde::Serialize, serde::de::DeserializeOwned, Clone
        );
        static_assertions::assert_not_impl_any!(
            AppAgentChildCurrentAuthorityProof: serde::Serialize, serde::de::DeserializeOwned, Clone
        );
    }

    #[test]
    fn cancellation_request_is_not_a_terminal_status() {
        assert!(!serde_json::to_value(AppAgentChildControlStatus::Completed)
            .expect("serialize terminal status")
            .as_str()
            .is_some_and(|value| value == "cancellation_requested"));
        assert!(execution_is_pre_start("pending"));
        assert!(execution_is_pre_start("ready"));
        assert!(execution_is_pre_start("queued"));
        assert!(!execution_is_pre_start("running"));
        assert!(!execution_is_pre_start("completed"));
    }

    #[test]
    fn denied_reservation_requires_a_prestart_deadline_or_revocation_intent() {
        let binding = crate::magician_v2::apps::agent_capability::tests::child_binding();
        let mut denied = AppAgentToolLifecycleDocument::new(binding.clone()).unwrap();
        denied.cancellation = Some(
            AppAgentChildCancellationIntent::mint(
                &binding,
                AppAgentChildCancellationReason::Deadline,
                1_799_999_500_000,
                true,
            )
            .unwrap(),
        );
        denied.launch_phase = AppAgentToolLaunchPhase::Denied;
        denied.refresh_digest().unwrap();
        denied.validate().unwrap();

        let mut invalid = AppAgentToolLifecycleDocument::new(binding.clone()).unwrap();
        invalid.cancellation = Some(
            AppAgentChildCancellationIntent::mint(
                &binding,
                AppAgentChildCancellationReason::OwnerStop,
                1_799_999_500_000,
                true,
            )
            .unwrap(),
        );
        invalid.launch_phase = AppAgentToolLaunchPhase::Denied;
        invalid.refresh_digest().unwrap();
        assert!(matches!(
            invalid.validate(),
            Err(AppAgentToolLifecycleError::CorruptLifecycle)
        ));
    }

    #[test]
    fn invalid_completed_result_projects_payload_free_uncertainty() {
        let binding = crate::magician_v2::apps::agent_capability::tests::child_binding();
        let observation = invalid_completed_result_observation(
            &binding,
            &AppDigest::blake3(b"terminal-state"),
            &AppDigest::blake3(b"effect-settlement"),
        )
        .unwrap();
        assert_eq!(
            observation.status,
            AppAgentChildControlStatus::OutcomeUncertain
        );
        assert!(observation.value.is_none());
        assert!(observation.result_artifact_ref.is_none());
        assert!(observation.result_artifact_digest.is_none());
        assert_eq!(
            observation.failure_code.as_ref().map(AppName::as_str),
            Some("child_result_artifact_invalid")
        );
        assert!(observation.effect_uncertain);
    }
}
