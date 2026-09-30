//! WakeUpQueue -- dumb timers for scheduled task wakes and execution wakes.
//!
//! Holds a sorted Vec<WakeEntry>. Call drain_due() to claim items whose wake_at
//! is in the past. Scheduled automation and crash-recovery rows remain leased
//! until exact acknowledgement. No cron logic lives here.

use std::path::PathBuf;
use std::sync::{Arc, Weak};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{Notify, RwLock, Semaphore};
use tracing::warn;

use crate::magician_v2::agents::storage::{AgentStorage, FileLockGuard};
use crate::magician_v2::artifact_v2::{workspace::ArtifactV2Workspace, ArtifactV2Error};

/// How long a scheduled or crash-recovery wake remains claimed before another
/// dispatcher may retry it. The claim is persisted before the handler starts,
/// so process death cannot turn drain-before-dispatch into a lost timer.
const DURABLE_WAKE_CLAIM_LEASE: chrono::Duration = chrono::Duration::seconds(30);
/// In-memory backoff after durable claim I/O fails. The persisted row remains
/// due (and therefore boot-recoverable), while the live listener avoids a
/// zero-duration retry loop that would hammer the same failing provider.
const EXECUTION_RETRY_PERSISTENCE_BACKOFF: chrono::Duration = chrono::Duration::seconds(1);
/// Global resource ceilings for the shared durable timer queue. The queue is
/// fed by user-visible schedules as well as crash-retry authority, so neither a
/// hostile persisted file nor a large overdue burst may allocate or spawn work
/// in proportion to all historical rows at once.
const MAX_WAKE_QUEUE_ENTRIES: usize = 4_096;
const MAX_WAKE_QUEUE_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_WAKE_QUEUE_JSON_DEPTH: usize = 16;
const MAX_WAKE_QUEUE_JSON_NODES: usize = MAX_WAKE_QUEUE_ENTRIES * 24 + 1;
const MAX_WAKE_ID_BYTES: usize = 4 * 1024;
const MAX_WAKE_DISPATCH_BATCH: usize = 64;
const MAX_CONCURRENT_WAKE_DISPATCHES: usize = 64;

/// Discriminates how a due [`WakeEntry`] should be dispatched.
///
/// - [`WakeKind::Scheduled`]: Routes through `resume_scheduler_entry`, which resolves
///   the scheduled automation entry and runs the SchedulerAgent to check if the
///   cron/sleep window is due before firing.
/// - [`WakeKind::TaskSchedule`]: Legacy recovery hint for a schedule fire that
///   already has a durably accepted active execution. New fires are accepted
///   and launched directly by the V3 scheduler lifecycle.
/// - [`WakeKind::ChildCompleted`]: Fired when a delegated child execution reaches a
///   terminal state. The dispatcher loads the parent execution by `execution_id` and
///   reconciles `active_delegation_group` against child terminal states.
/// - [`WakeKind::ExecutionRetry`]: Re-adopts one existing Artifact V2 execution
///   after a stateless placement pin expires. Both `task_id` and `execution_id`
///   are required; dispatch must never create a replacement execution.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub enum WakeKind {
    #[default]
    Scheduled,
    /// A legacy schedule-fire recovery hint. `task_id` identifies the task
    /// whose already-accepted active root execution must be relaunched; this
    /// row is not authority to create a replacement execution.
    TaskSchedule,
    /// A deferred task retry. The dispatcher loads the task, checks its retry
    /// status/dependencies, and starts a new execution only through Artifact's
    /// ordinary durable admission.
    TaskRetry,
    /// Retry this exact deferred execution. Unlike [`Self::TaskRetry`], this is
    /// not permission to create a fresh task execution.
    ExecutionRetry,
    /// A delegated child execution completed; reconcile the parent execution by `execution_id`.
    ChildCompleted,
}

/// Selects the wake kinds a dispatcher is allowed to claim.
///
/// The web/API dispatcher is the production owner of the complete durable
/// queue. The runtime-local watcher is retained for legacy embedding, but it
/// must never remove task-scheduler rows that only the web/API dispatcher can
/// execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeConsumer {
    ProductionWeb,
    RuntimeLegacy,
}

impl WakeConsumer {
    fn owns(self, kind: &WakeKind) -> bool {
        match self {
            Self::ProductionWeb => true,
            Self::RuntimeLegacy => !matches!(kind, WakeKind::TaskSchedule | WakeKind::TaskRetry),
        }
    }
}

/// A single pending wake entry.
///
/// Task wakes use `task_id`. Execution wakes use `execution_id`.
/// Legacy fields `agent_id` / `goal_id` are retained as `Option<String>` so
/// persisted JSON from prior releases still deserializes without data loss.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WakeEntry {
    /// Primary key — identifies the scheduled task that owns this wake.
    /// Defaults to empty string for backward compat with pre-task_id JSON;
    /// `restore_from_disk` backfills from legacy agent_id/goal_id fields.
    #[serde(default)]
    pub task_id: String,
    /// Canonical Artifact scope for exact execution retries. Legacy timer kinds
    /// leave these empty; an ExecutionRetry without both fields fails closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Execution-targeted wake owner. Used by execution-scoped V2 runtime wakes such
    /// as `ChildCompleted`.
    #[serde(
        default,
        rename = "execution_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub execution_id: Option<String>,
    /// Exact committed stateless segment authorized by a placement-pin sleep.
    /// Absent for intentional pre-loop SleepUntil outcomes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stateless_source_segment: Option<String>,
    /// Legacy restore-only field for older persisted entries.
    /// New writes should use `task_id` only.
    #[serde(default)]
    pub agent_id: Option<String>,
    /// Legacy restore-only field for older persisted entries.
    /// New writes should use `task_id` only.
    #[serde(default)]
    pub goal_id: Option<String>,
    pub wake_at: DateTime<Utc>,
    /// Immutable due time for exact retry generation validation. `wake_at` is
    /// moved to the claim lease deadline while a handler owns the row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_retry_due_at: Option<DateTime<Utc>>,
    /// Immutable delivery generation for leased non-execution-retry rows. The
    /// legacy field name is retained on the wire. `wake_at` becomes a lease
    /// deadline while a handler owns the row, so timestamp comparison alone
    /// cannot distinguish a replacement that happens to use the same instant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_completed_generation: Option<String>,
    /// Discriminates how this entry should be dispatched.  Defaults to `Scheduled`
    /// for backward compatibility with persisted JSON that pre-dates this field.
    #[serde(default)]
    pub kind: WakeKind,
    /// A leased exact retry has reactivated and dispatched this execution at
    /// least once. Persisting this bit keeps the queue row as crash authority
    /// while allowing a later delivery to distinguish a live resumed owner
    /// from a wake that merely raced the original Sleeping projection.
    #[serde(default, skip_serializing_if = "is_false")]
    pub execution_retry_started: bool,
    /// Number of durable lease admissions for this exact Sleeping generation.
    /// The first delivery may race the runtime/Artifact Sleeping projection;
    /// a later delivery is allowed to repair that bounded crash window.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub execution_retry_claim_attempt: u32,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

fn validate_exact_execution_retry_binding(
    principal: &str,
    workspace: &str,
    task_id: &str,
    execution_id: &str,
    stateless_source_segment: Option<&str>,
) -> Result<(), ArtifactV2Error> {
    if [principal, workspace, task_id, execution_id]
        .iter()
        .any(|value| value.trim().is_empty())
    {
        return Err(ArtifactV2Error::InvalidRequest(
            "an exact execution retry requires non-empty principal, workspace, task_id and execution_id"
                .to_string(),
        ));
    }
    if stateless_source_segment
        .is_some_and(|segment| segment.trim().is_empty() || segment.len() > MAX_WAKE_ID_BYTES)
    {
        return Err(ArtifactV2Error::InvalidRequest(
            "an exact stateless retry source segment is empty or oversized".to_owned(),
        ));
    }
    if [principal, workspace, task_id, execution_id]
        .iter()
        .any(|value| value.len() > MAX_WAKE_ID_BYTES)
    {
        return Err(ArtifactV2Error::InvalidRequest(format!(
            "an exact execution retry binding component exceeds the {MAX_WAKE_ID_BYTES}-byte limit"
        )));
    }
    Ok(())
}

/// Canonical scoped key for the one exact-retry row a claim fence protects.
/// Empty/missing legacy coordinates deliberately have no key: they cannot be
/// made safe by accidentally sharing a lock target with another corrupt row.
fn exact_execution_retry_scope_key(entry: &WakeEntry) -> Option<(String, String, String, String)> {
    if entry.kind != WakeKind::ExecutionRetry {
        return None;
    }
    let principal = entry.principal.as_deref()?;
    let workspace = entry.workspace.as_deref()?;
    let task_id = entry.task_id.as_str();
    let execution_id = entry.execution_id.as_deref()?;
    if [principal, workspace, task_id, execution_id]
        .iter()
        .any(|value| value.trim().is_empty() || value.len() > MAX_WAKE_ID_BYTES)
    {
        return None;
    }
    Some((
        principal.to_owned(),
        workspace.to_owned(),
        task_id.to_owned(),
        execution_id.to_owned(),
    ))
}

/// Thread-safe queue of pending WakeEntry items.
#[derive(Debug)]
pub struct WakeUpQueue {
    entries: RwLock<Vec<WakeEntry>>,
    /// Interrupts a dispatcher's current timer when a mutation publishes an
    /// earlier wake. `notify_one` deliberately retains a permit when the
    /// dispatcher is between reading `next_wake_at` and registering its wait,
    /// so that race cannot strand a newly-due entry behind the old deadline.
    changed: Notify,
    persist_path: std::sync::RwLock<Option<PathBuf>>,
    /// First tier of every mutation lock. Atomic-file replacement is safe
    /// against torn bytes but not against two async writers finishing out of
    /// order; this serializes this process before the stable cross-process
    /// advisory sidecar is acquired and disk is reloaded.
    persistence_gate: tokio::sync::Mutex<()>,
    /// Per-scoped-execution gate shared by timer publication, durable claim
    /// issuance, and the handler's two mutation fences. The dispatcher uses it
    /// only for preflight; Artifact activation and runtime admission each
    /// reacquire/revalidate around their own state transition. ACK is instead a
    /// claimed-timestamp compare-and-delete, so it need not carry the guard.
    exact_retry_gates:
        std::sync::Mutex<std::collections::HashMap<String, Weak<tokio::sync::Mutex<()>>>>,
    /// Shared by every queue consumer. A bounded drain alone is insufficient:
    /// a zero-delay listener could drain the next page before the previous
    /// page's detached handlers complete and still create unbounded work.
    dispatch_limit: Arc<Semaphore>,
}

/// Cross-process ownership of one exact retry generation's current critical
/// section. Both guards are required: the Tokio guard serializes callers in
/// this process, while the stable advisory sidecar excludes a rolling peer.
/// The guard is deliberately reacquired at each transition rather than carried
/// through the whole detached lifecycle, whose runtime admission needs the same
/// non-reentrant lock.
#[derive(Debug)]
pub struct ExecutionRetryClaimGuard {
    _local: tokio::sync::OwnedMutexGuard<()>,
    _file: FileLockGuard,
}

impl Default for WakeUpQueue {
    fn default() -> Self {
        Self {
            entries: RwLock::new(Vec::new()),
            changed: Notify::new(),
            persist_path: std::sync::RwLock::new(None),
            persistence_gate: tokio::sync::Mutex::new(()),
            exact_retry_gates: std::sync::Mutex::new(std::collections::HashMap::new()),
            dispatch_limit: Arc::new(Semaphore::new(MAX_CONCURRENT_WAKE_DISPATCHES)),
        }
    }
}

impl WakeUpQueue {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Optionally configure a JSON file path for persistence.
    pub fn set_persist_path(&self, path: PathBuf) {
        let mut g = self.persist_path.write().expect("persist_path poisoned");
        *g = Some(path);
    }

    fn persistence_path(&self) -> Option<PathBuf> {
        self.persist_path
            .read()
            .expect("persist_path poisoned")
            .clone()
    }

    /// Enter one cross-process queue read/modify/write transaction.
    ///
    /// Callers acquire `persistence_gate` first. The advisory guard is on
    /// `AgentStorage`'s stable sidecar rather than on the JSON inode that atomic
    /// publication replaces. Once both guards are held, disk is reloaded and is
    /// authoritative; using this process's cached Vec would let a rolling peer
    /// overwrite a generation it had never observed.
    async fn begin_persistence_transaction(
        &self,
        persistence_required: bool,
    ) -> Result<(Option<PathBuf>, Option<FileLockGuard>, Vec<WakeEntry>), ArtifactV2Error> {
        let Some(path) = self.persistence_path() else {
            if persistence_required {
                return Err(ArtifactV2Error::Runtime(
                    "WakeUpQueue persistence path is not configured".to_owned(),
                ));
            }
            return Ok((None, None, self.entries.read().await.clone()));
        };
        let file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(|error| {
                ArtifactV2Error::Runtime(format!("WakeUpQueue cross-process lock failed: {error}"))
            })?;
        let entries = match load_persisted_entries(&path).await? {
            Some(entries) => entries,
            // Supports configuring persistence after callers populated the
            // queue in memory. A durable deletion writes `[]`; it does not
            // remove the file, so absence is not mistaken for a peer's empty
            // authoritative snapshot.
            None => self.entries.read().await.clone(),
        };
        Ok((Some(path), Some(file_guard), entries))
    }

    async fn persist_transaction_entries(
        &self,
        path: Option<&std::path::Path>,
        entries: &[WakeEntry],
    ) -> Result<(), ArtifactV2Error> {
        let Some(path) = path else {
            // Legacy/in-memory-only users deliberately have no persistence
            // boundary. Result-returning durable admissions call
            // `begin_persistence_transaction(true)` and can never reach this
            // branch.
            return Ok(());
        };
        persist_entries_at_path(path, entries).await
    }

    /// Reserve one process-wide wake-dispatch slot. Both the runtime-local and
    /// web lifecycle listeners use this same semaphore, so accidentally
    /// starting both cannot multiply detached fan-out.
    pub async fn acquire_dispatch_permit(&self) -> tokio::sync::OwnedSemaphorePermit {
        Arc::clone(&self.dispatch_limit)
            .acquire_owned()
            .await
            .expect("WakeUpQueue dispatch semaphore is never closed")
    }

    /// Reserve up to one bounded page of immediately usable handler slots.
    ///
    /// Durable wake claims must be issued only after capacity is reserved. If a
    /// listener leased a page first and then waited behind an earlier page's
    /// long-running handlers, those unstarted leases could expire and be
    /// dispatched by a rolling peer before their local handlers began.
    pub fn reserve_dispatch_page_permits(&self) -> Vec<tokio::sync::OwnedSemaphorePermit> {
        let mut permits = Vec::with_capacity(MAX_WAKE_DISPATCH_BATCH);
        for _ in 0..MAX_WAKE_DISPATCH_BATCH {
            match Arc::clone(&self.dispatch_limit).try_acquire_owned() {
                Ok(permit) => permits.push(permit),
                Err(_) => break,
            }
        }
        permits
    }

    /// Schedule a wake for a task at `wake_at`.
    /// Pending or expired entries for this `task_id` are replaced. An
    /// immutable, still-live delivery lease is retained beside the successor:
    /// rescheduling must not revoke a handler that already owns its fire.
    /// The entry is dispatched via `resume_scheduler_entry` (cron/sleep path).
    pub async fn schedule(&self, task_id: &str, wake_at: DateTime<Utc>) {
        self.schedule_with_kind(task_id, wake_at, WakeKind::Scheduled)
            .await;
    }

    pub async fn schedule_scoped(
        &self,
        principal: &str,
        workspace: &str,
        agent_id: &str,
        goal_id: &str,
        wake_at: DateTime<Utc>,
    ) {
        let task_id = scoped_automation_task_id(principal, workspace, agent_id, goal_id);
        self.schedule_with_kind(&task_id, wake_at, WakeKind::Scheduled)
            .await;
    }

    /// Persist a legacy schedule-fire recovery hint for a task whose active
    /// execution has already been durably accepted.
    pub async fn schedule_task_schedule_wake(&self, task_id: &str, wake_at: DateTime<Utc>) {
        self.schedule_with_kind(task_id, wake_at, WakeKind::TaskSchedule)
            .await;
    }

    /// Schedule a deferred-task retry wake.
    ///
    /// Uses [`WakeKind::TaskRetry`] and retains the deferred-task retry gate.
    pub async fn schedule_task_retry_wake(&self, task_id: &str, wake_at: DateTime<Utc>) {
        self.schedule_with_kind(task_id, wake_at, WakeKind::TaskRetry)
            .await;
    }

    /// Schedule a child-completion wake for a parent execution.
    ///
    /// If an entry already exists for this `execution_id`, it is replaced.
    pub async fn schedule_child_completed_wake(
        &self,
        execution_id: &str,
        wake_at: DateTime<Utc>,
    ) -> Result<(), ArtifactV2Error> {
        if execution_id.trim().is_empty() || execution_id.len() > MAX_WAKE_ID_BYTES {
            return Err(ArtifactV2Error::InvalidRequest(format!(
                "a child-completed wake requires a non-empty execution id within {MAX_WAKE_ID_BYTES} bytes"
            )));
        }
        let _persistence = self.persistence_gate.lock().await;
        let (path, _file_guard, mut next) = self.begin_persistence_transaction(true).await?;
        next.retain(|entry| {
            !(entry.kind == WakeKind::ChildCompleted
                && entry.execution_id.as_deref() == Some(execution_id))
        });
        if next.len() >= MAX_WAKE_QUEUE_ENTRIES {
            return Err(ArtifactV2Error::InvalidRequest(format!(
                "WakeUpQueue is at its {MAX_WAKE_QUEUE_ENTRIES}-entry capacity"
            )));
        }
        next.push(WakeEntry {
            task_id: String::new(),
            principal: None,
            workspace: None,
            execution_id: Some(execution_id.to_string()),
            stateless_source_segment: None,
            agent_id: None,
            goal_id: None,
            wake_at,
            execution_retry_due_at: None,
            child_completed_generation: Some(uuid::Uuid::new_v4().to_string()),
            kind: WakeKind::ChildCompleted,
            execution_retry_started: false,
            execution_retry_claim_attempt: 0,
        });
        next.sort_by_key(|entry| entry.wake_at);
        // Child reconciliation is also crash-recovery work. Publish only after
        // its row is durable so returning success never creates a memory-only
        // safety net.
        self.persist_transaction_entries(path.as_deref(), &next)
            .await?;
        *self.entries.write().await = next;
        self.changed.notify_one();
        Ok(())
    }

    /// Schedule re-adoption of one existing Artifact V2 execution.
    ///
    /// The task id is carried beside the execution id so the due owner can
    /// validate the durable binding without a global scope scan. This method
    /// deliberately does not share [`Self::schedule_task_retry_wake`]'s
    /// task-only shape: that path starts a new execution and would duplicate
    /// an exact stateless continuation.
    pub async fn schedule_execution_retry_wake(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
        execution_id: &str,
        wake_at: DateTime<Utc>,
    ) -> Result<(), ArtifactV2Error> {
        self.schedule_execution_retry_wake_for_segment(
            principal,
            workspace,
            task_id,
            execution_id,
            wake_at,
            None,
        )
        .await
    }

    pub async fn schedule_execution_retry_wake_for_segment(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
        execution_id: &str,
        wake_at: DateTime<Utc>,
        stateless_source_segment: Option<&str>,
    ) -> Result<(), ArtifactV2Error> {
        validate_exact_execution_retry_binding(
            principal,
            workspace,
            task_id,
            execution_id,
            stateless_source_segment,
        )?;
        let _generation_guard = self
            .lock_execution_retry_claim(principal, workspace, task_id, execution_id)
            .await?;
        let _persistence = self.persistence_gate.lock().await;
        let (path, _file_guard, mut next) = self.begin_persistence_transaction(true).await?;
        next.retain(|entry| {
            !(entry.kind == WakeKind::ExecutionRetry
                && entry.execution_id.as_deref() == Some(execution_id)
                && entry.task_id == task_id
                && entry.principal.as_deref() == Some(principal)
                && entry.workspace.as_deref() == Some(workspace))
        });
        if next.len() >= MAX_WAKE_QUEUE_ENTRIES {
            return Err(ArtifactV2Error::InvalidRequest(format!(
                "WakeUpQueue is at its {MAX_WAKE_QUEUE_ENTRIES}-entry capacity"
            )));
        }
        next.push(WakeEntry {
            task_id: task_id.to_string(),
            principal: Some(principal.to_string()),
            workspace: Some(workspace.to_string()),
            execution_id: Some(execution_id.to_string()),
            stateless_source_segment: stateless_source_segment.map(ToOwned::to_owned),
            agent_id: None,
            goal_id: None,
            wake_at,
            execution_retry_due_at: Some(wake_at),
            child_completed_generation: None,
            kind: WakeKind::ExecutionRetry,
            execution_retry_started: false,
            execution_retry_claim_attempt: 0,
        });
        next.sort_by_key(|entry| entry.wake_at);
        // Publish only after the replacement row is durable. Returning an
        // error must leave both the in-memory and on-disk prior generation
        // intact so the caller can fail closed without a ghost retry.
        self.persist_transaction_entries(path.as_deref(), &next)
            .await?;
        *self.entries.write().await = next;
        self.changed.notify_one();
        Ok(())
    }

    /// Atomically ensure one immutable exact-retry generation exists.
    ///
    /// Unlike peek-then-schedule, the equality check shares the same scoped
    /// claim guard and global durable queue transaction as publication. An
    /// already-started matching row is preserved byte-for-semantics, including
    /// its claim lease, attempt count, and admission marker.
    pub async fn ensure_execution_retry_wake_for_segment(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
        execution_id: &str,
        wake_at: DateTime<Utc>,
        stateless_source_segment: Option<&str>,
    ) -> Result<bool, ArtifactV2Error> {
        validate_exact_execution_retry_binding(
            principal,
            workspace,
            task_id,
            execution_id,
            stateless_source_segment,
        )?;
        let _generation_guard = self
            .lock_execution_retry_claim(principal, workspace, task_id, execution_id)
            .await?;
        let _persistence = self.persistence_gate.lock().await;
        let (path, _file_guard, mut next) = self.begin_persistence_transaction(true).await?;
        let scoped_rows = next
            .iter()
            .filter(|entry| {
                entry.kind == WakeKind::ExecutionRetry
                    && entry.execution_id.as_deref() == Some(execution_id)
                    && entry.task_id == task_id
                    && entry.principal.as_deref() == Some(principal)
                    && entry.workspace.as_deref() == Some(workspace)
            })
            .collect::<Vec<_>>();
        if scoped_rows.len() == 1
            && scoped_rows[0].execution_retry_due_at == Some(wake_at)
            && scoped_rows[0].stateless_source_segment.as_deref() == stateless_source_segment
        {
            *self.entries.write().await = next;
            return Ok(false);
        }
        next.retain(|entry| {
            !(entry.kind == WakeKind::ExecutionRetry
                && entry.execution_id.as_deref() == Some(execution_id)
                && entry.task_id == task_id
                && entry.principal.as_deref() == Some(principal)
                && entry.workspace.as_deref() == Some(workspace))
        });
        if next.len() >= MAX_WAKE_QUEUE_ENTRIES {
            return Err(ArtifactV2Error::InvalidRequest(format!(
                "WakeUpQueue is at its {MAX_WAKE_QUEUE_ENTRIES}-entry capacity"
            )));
        }
        next.push(WakeEntry {
            task_id: task_id.to_string(),
            principal: Some(principal.to_string()),
            workspace: Some(workspace.to_string()),
            execution_id: Some(execution_id.to_string()),
            stateless_source_segment: stateless_source_segment.map(ToOwned::to_owned),
            agent_id: None,
            goal_id: None,
            wake_at,
            execution_retry_due_at: Some(wake_at),
            child_completed_generation: None,
            kind: WakeKind::ExecutionRetry,
            execution_retry_started: false,
            execution_retry_claim_attempt: 0,
        });
        next.sort_by_key(|entry| entry.wake_at);
        self.persist_transaction_entries(path.as_deref(), &next)
            .await?;
        *self.entries.write().await = next;
        self.changed.notify_one();
        Ok(true)
    }

    fn exact_retry_gate(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
        execution_id: &str,
    ) -> Arc<tokio::sync::Mutex<()>> {
        let key = format!("{principal}\0{workspace}\0{task_id}\0{execution_id}");
        let mut gates = self
            .exact_retry_gates
            .lock()
            .expect("exact_retry_gates lock poisoned");
        gates.retain(|_, gate| gate.strong_count() > 0);
        if let Some(gate) = gates.get(&key).and_then(Weak::upgrade) {
            return gate;
        }
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        gates.insert(key, Arc::downgrade(&gate));
        gate
    }

    /// Serialize a claimed handler with publication of the next Sleeping
    /// generation for the same scoped execution.
    pub async fn lock_execution_retry_claim(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
        execution_id: &str,
    ) -> Result<ExecutionRetryClaimGuard, ArtifactV2Error> {
        let local = self
            .exact_retry_gate(principal, workspace, task_id, execution_id)
            .lock_owned()
            .await;
        let queue_path = self.persistence_path().ok_or_else(|| {
            ArtifactV2Error::Runtime(
                "WakeUpQueue persistence path is not configured for exact retry claim".to_owned(),
            )
        })?;
        let mut hasher = Sha256::new();
        for value in [principal, workspace, task_id, execution_id] {
            hasher.update((value.len() as u64).to_be_bytes());
            hasher.update(value.as_bytes());
        }
        // This is a *lock target*, not persisted data. AgentStorage derives a
        // stable `.flock` sibling from it. Keeping it in a separate directory
        // from the queue data lock lets a handler hold this per-execution guard
        // while ACK performs the global queue RMW without self-deadlocking.
        let claim_target = queue_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join(".wake-up-queue-claims")
            .join(format!("{}.claim", hex::encode(hasher.finalize())));
        let file = AgentStorage::acquire_file_lock_exclusive(&claim_target)
            .await
            .map_err(|error| {
                ArtifactV2Error::Runtime(format!(
                    "WakeUpQueue exact retry claim lock failed: {error}"
                ))
            })?;
        Ok(ExecutionRetryClaimGuard {
            _local: local,
            _file: file,
        })
    }

    /// Verify that a leased delivery still names the current queue generation.
    ///
    /// Dispatchers hold [`Self::lock_execution_retry_claim`] for this preflight
    /// read, then release it before entering the Artifact lifecycle: exact
    /// admission reacquires the same non-reentrant lock and re-reads the row
    /// around activation. Acknowledgement is a later compare-and-delete on the
    /// claimed `wake_at`, so it cannot erase a replacement generation.
    pub async fn execution_retry_claim_is_current(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
        execution_id: &str,
        claimed_until: DateTime<Utc>,
        expected_due_at: Option<DateTime<Utc>>,
        expected_stateless_source_segment: Option<&str>,
    ) -> bool {
        let _persistence = self.persistence_gate.lock().await;
        let (_path, _file_guard, current) = match self.begin_persistence_transaction(true).await {
            Ok(transaction) => transaction,
            Err(error) => {
                warn!(%error, "WakeUpQueue: exact retry current-generation read failed closed");
                return false;
            },
        };
        let mut scoped = current.iter().filter(|entry| {
            entry.kind == WakeKind::ExecutionRetry
                && entry.execution_id.as_deref() == Some(execution_id)
                && entry.task_id == task_id
                && entry.principal.as_deref() == Some(principal)
                && entry.workspace.as_deref() == Some(workspace)
        });
        let matches = scoped.next().is_some_and(|entry| {
            entry.wake_at == claimed_until
                && entry.execution_retry_due_at == expected_due_at
                && entry.stateless_source_segment.as_deref() == expected_stateless_source_segment
        }) && scoped.next().is_none();
        *self.entries.write().await = current;
        matches
    }

    /// Read the exact durable retry generation for startup repair. Scope and
    /// task are part of the key; execution ids are not globally unique.
    pub async fn execution_retry_wake(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
        execution_id: &str,
    ) -> Option<WakeEntry> {
        let _persistence = self.persistence_gate.lock().await;
        let (_path, _file_guard, current) = match self.begin_persistence_transaction(true).await {
            Ok(transaction) => transaction,
            Err(error) => {
                warn!(%error, "WakeUpQueue: exact retry generation read failed closed");
                return None;
            },
        };
        let mut scoped = current.iter().filter(|entry| {
            entry.kind == WakeKind::ExecutionRetry
                && entry.execution_id.as_deref() == Some(execution_id)
                && entry.task_id == task_id
                && entry.principal.as_deref() == Some(principal)
                && entry.workspace.as_deref() == Some(workspace)
        });
        let found = scoped.next().cloned();
        let found = if scoped.next().is_none() { found } else { None };
        *self.entries.write().await = current;
        found
    }

    async fn schedule_with_kind(&self, task_id: &str, wake_at: DateTime<Utc>, kind: WakeKind) {
        if task_id.len() > MAX_WAKE_ID_BYTES {
            warn!(
                bytes = task_id.len(),
                limit = MAX_WAKE_ID_BYTES,
                "WakeUpQueue: refusing an oversized task wake identifier"
            );
            return;
        }
        let _persistence = self.persistence_gate.lock().await;
        let (path, _file_guard, mut next) = match self.begin_persistence_transaction(false).await {
            Ok(transaction) => transaction,
            Err(error) => {
                warn!(%error, "WakeUpQueue: failed to lock/reload task wake queue");
                return;
            },
        };
        let now = Utc::now();
        next.retain(|entry| {
            entry.execution_id.is_some()
                || entry.task_id != task_id
                || (entry.child_completed_generation.is_some() && entry.wake_at > now)
        });
        if next.len() >= MAX_WAKE_QUEUE_ENTRIES {
            warn!(
                limit = MAX_WAKE_QUEUE_ENTRIES,
                "WakeUpQueue: refusing a new task wake because the durable queue is full"
            );
            return;
        }
        next.push(WakeEntry {
            task_id: task_id.to_string(),
            principal: None,
            workspace: None,
            execution_id: None,
            stateless_source_segment: None,
            agent_id: None,
            goal_id: None,
            wake_at,
            execution_retry_due_at: None,
            child_completed_generation: None,
            kind,
            execution_retry_started: false,
            execution_retry_claim_attempt: 0,
        });
        next.sort_by_key(|e| e.wake_at);
        if let Err(error) = self
            .persist_transaction_entries(path.as_deref(), &next)
            .await
        {
            warn!(%error, "WakeUpQueue: failed to persist task wake");
            return;
        }
        *self.entries.write().await = next;
        self.changed.notify_one();
    }

    /// Cancel pending wake entries by task id. A handler that already owns an
    /// immutable live generation is not pending and cannot be retroactively
    /// fenced here; it keeps its exact ACK/renew authority until completion or
    /// lease expiry.
    pub async fn cancel(&self, task_id: &str) {
        let _persistence = self.persistence_gate.lock().await;
        let (path, _file_guard, mut next) = match self.begin_persistence_transaction(false).await {
            Ok(transaction) => transaction,
            Err(error) => {
                warn!(%error, "WakeUpQueue: failed to lock/reload task-wake cancellation");
                return;
            },
        };
        let now = Utc::now();
        next.retain(|entry| {
            entry.task_id != task_id
                || (entry.child_completed_generation.is_some() && entry.wake_at > now)
        });
        if let Err(error) = self
            .persist_transaction_entries(path.as_deref(), &next)
            .await
        {
            warn!(%error, "WakeUpQueue: failed to persist task-wake cancellation");
            return;
        }
        *self.entries.write().await = next;
    }

    pub async fn cancel_scoped(
        &self,
        principal: &str,
        workspace: &str,
        agent_id: &str,
        goal_id: &str,
    ) {
        self.cancel(&scoped_automation_task_id(
            principal, workspace, agent_id, goal_id,
        ))
        .await;
    }

    /// Force an existing entry to wake immediately (sets wake_at to 1s in the past).
    pub async fn wake_now(&self, task_id: &str) {
        let _persistence = self.persistence_gate.lock().await;
        let (path, _file_guard, mut next) = match self.begin_persistence_transaction(false).await {
            Ok(transaction) => transaction,
            Err(error) => {
                warn!(%error, "WakeUpQueue: failed to lock/reload immediate task wake");
                return;
            },
        };
        let now = Utc::now();
        for e in next.iter_mut() {
            if e.execution_id.is_none()
                && e.task_id == task_id
                && !(e.child_completed_generation.is_some() && e.wake_at > now)
            {
                e.wake_at = now - chrono::Duration::seconds(1);
            }
        }
        // L-5: Re-sort to maintain the sort invariant after mutation.
        next.sort_by_key(|e| e.wake_at);
        if let Err(error) = self
            .persist_transaction_entries(path.as_deref(), &next)
            .await
        {
            warn!(%error, "WakeUpQueue: failed to persist immediate task wake");
            return;
        }
        *self.entries.write().await = next;
        self.changed.notify_one();
    }

    pub async fn wake_now_scoped(
        &self,
        principal: &str,
        workspace: &str,
        agent_id: &str,
        goal_id: &str,
    ) {
        self.wake_now(&scoped_automation_task_id(
            principal, workspace, agent_id, goal_id,
        ))
        .await;
    }

    /// Drain one bounded page for the production web/API dispatcher.
    ///
    /// Kept as the all-kinds entry point for existing production callers. A
    /// partial consumer must use [`Self::drain_due_for`] so it cannot
    /// destructively claim a kind whose handler it does not own.
    pub async fn drain_due(&self) -> Vec<WakeEntry> {
        self.drain_due_for(WakeConsumer::ProductionWeb).await
    }

    /// Drain one bounded page of entries owned by `consumer`. Returns them
    /// sorted oldest-first, leaves every unowned row byte-for-semantics in the
    /// durable queue, and notifies listeners when more owned due work remains.
    pub async fn drain_due_for(&self, consumer: WakeConsumer) -> Vec<WakeEntry> {
        self.drain_due_for_limit(consumer, MAX_WAKE_DISPATCH_BATCH)
            .await
    }

    /// Claim at most `limit` due rows for a consumer. Production dispatchers
    /// pass the number of handler permits they reserved before entering this
    /// method, so every returned durable lease has an immediate local owner.
    pub async fn drain_due_for_limit(
        &self,
        consumer: WakeConsumer,
        limit: usize,
    ) -> Vec<WakeEntry> {
        let limit = limit.min(MAX_WAKE_DISPATCH_BATCH);
        if limit == 0 {
            return Vec::new();
        }
        let now = Utc::now();

        // Exact execution claims share one scoped generation lock with timer
        // replacement and lifecycle activation. Select a bounded candidate set
        // from the local mirror, acquire those locks in deterministic order,
        // and only then enter the global persistence transaction. The ordering
        // matches `schedule_execution_retry_wake_for_segment` (exact -> global)
        // and therefore cannot deadlock it. A rolling peer may have introduced
        // another due key since this mirror was refreshed; that row is left due
        // and becomes a candidate on the next bounded drain.
        //
        // This lock is not merely an optimisation. Without it, a peer can renew
        // `wake_at` while the old handler holds the same per-execution lock for
        // its current-generation check plus Artifact activation. The old
        // handler then mutates Sleeping -> Running using a claim it no longer
        // owns, even though the later runtime admission correctly refuses it.
        let exact_retry_candidates = {
            let entries = self.entries.read().await;
            entries
                .iter()
                .filter(|entry| {
                    consumer.owns(&entry.kind)
                        && entry.kind == WakeKind::ExecutionRetry
                        && entry.wake_at <= now
                })
                .filter_map(exact_execution_retry_scope_key)
                .take(limit)
                .collect::<std::collections::BTreeSet<_>>()
        };
        let mut exact_retry_guards = Vec::with_capacity(exact_retry_candidates.len());
        let mut fenced_exact_retries = std::collections::BTreeSet::new();
        for key in exact_retry_candidates {
            match self
                .lock_execution_retry_claim(&key.0, &key.1, &key.2, &key.3)
                .await
            {
                Ok(guard) => {
                    fenced_exact_retries.insert(key);
                    exact_retry_guards.push(guard);
                },
                Err(error) => {
                    warn!(
                        principal = %key.0,
                        workspace = %key.1,
                        task_id = %key.2,
                        execution_id = %key.3,
                        %error,
                        "WakeUpQueue: exact retry claim fence unavailable; row left due"
                    );
                },
            }
        }

        let _persistence = self.persistence_gate.lock().await;
        let (path, _file_guard, current) = match self.begin_persistence_transaction(false).await {
            Ok(transaction) => transaction,
            Err(error) => {
                warn!(%error, "WakeUpQueue: failed to lock/reload due queue");
                // Disk remains authoritative and no ownership token is
                // returned. Move only this process's cached durable rows
                // out of the zero-delay loop while the substrate recovers.
                let mut local = self.entries.write().await;
                for entry in local.iter_mut().filter(|entry| {
                    matches!(
                        entry.kind,
                        WakeKind::Scheduled
                            | WakeKind::TaskSchedule
                            | WakeKind::TaskRetry
                            | WakeKind::ExecutionRetry
                            | WakeKind::ChildCompleted
                    ) && entry.wake_at <= now
                }) {
                    entry.wake_at = now + EXECUTION_RETRY_PERSISTENCE_BACKOFF;
                }
                local.sort_by_key(|entry| entry.wake_at);
                return Vec::new();
            },
        };
        let mut exact_retry_row_counts = std::collections::BTreeMap::new();
        for key in current
            .iter()
            .filter(|entry| entry.kind == WakeKind::ExecutionRetry)
            .filter_map(exact_execution_retry_scope_key)
        {
            let count = exact_retry_row_counts.entry(key).or_insert(0usize);
            *count = count.saturating_add(1);
        }
        let claim_until = now + DURABLE_WAKE_CLAIM_LEASE;
        let mut due = Vec::new();
        let mut pending = Vec::with_capacity(current.len());
        let mut deferred_ambiguous_exact_retry = false;
        for mut entry in current.iter().cloned() {
            if !consumer.owns(&entry.kind) || entry.wake_at > now || due.len() >= limit {
                pending.push(entry);
                continue;
            }
            if entry.kind == WakeKind::ExecutionRetry {
                let exact_key = exact_execution_retry_scope_key(&entry);
                let exactly_one_row = exact_key
                    .as_ref()
                    .is_some_and(|key| exact_retry_row_counts.get(key).copied() == Some(1));
                let generation_is_fenced = exact_key
                    .as_ref()
                    .is_some_and(|key| fenced_exact_retries.contains(key));
                if !exactly_one_row || !generation_is_fenced {
                    if !exactly_one_row {
                        // Malformed or duplicate scoped identities cannot be
                        // leased safely. Keep them durable but move them out of
                        // the immediate loop while an operator repairs the
                        // corrupted queue.
                        entry.wake_at = now + EXECUTION_RETRY_PERSISTENCE_BACKOFF;
                        deferred_ambiguous_exact_retry = true;
                        warn!(
                            task_id = %entry.task_id,
                            execution_id = ?entry.execution_id,
                            "WakeUpQueue: refusing an ambiguous exact retry row"
                        );
                    }
                    pending.push(entry);
                    continue;
                }
            }
            if matches!(
                entry.kind,
                WakeKind::Scheduled
                    | WakeKind::TaskSchedule
                    | WakeKind::TaskRetry
                    | WakeKind::ExecutionRetry
                    | WakeKind::ChildCompleted
            ) {
                // Scheduled and crash-recovery wakes are claimed, not removed. The returned
                // lease timestamp is the acknowledgement token. If the process
                // dies before dispatch/ack, this same durable row becomes due
                // again. Only exact execution retries use the attempt counter.
                if matches!(
                    entry.kind,
                    WakeKind::Scheduled
                        | WakeKind::TaskSchedule
                        | WakeKind::TaskRetry
                        | WakeKind::ChildCompleted
                ) && entry.child_completed_generation.is_none()
                {
                    // Upgrade a legacy durable row before handing it to a
                    // dispatcher. The claim write below makes this token
                    // authoritative before a handler can begin.
                    entry.child_completed_generation = Some(uuid::Uuid::new_v4().to_string());
                }
                entry.wake_at = claim_until;
                if entry.kind == WakeKind::ExecutionRetry {
                    entry.execution_retry_claim_attempt =
                        entry.execution_retry_claim_attempt.saturating_add(1);
                }
                due.push(entry.clone());
                pending.push(entry);
            } else {
                due.push(entry);
            }
        }
        pending.sort_by_key(|entry| entry.wake_at);
        if !due.is_empty() || deferred_ambiguous_exact_retry {
            let claims_durable_wake = due.iter().any(|entry| {
                matches!(
                    entry.kind,
                    WakeKind::Scheduled
                        | WakeKind::TaskSchedule
                        | WakeKind::TaskRetry
                        | WakeKind::ExecutionRetry
                        | WakeKind::ChildCompleted
                )
            }) || deferred_ambiguous_exact_retry;
            if claims_durable_wake {
                // Claimed timer/recovery rows must be durable before a caller
                // can spawn handlers. On failure, retain the original queue in
                // memory and return no ownership tokens.
                if let Err(error) = self
                    .persist_transaction_entries(path.as_deref(), &pending)
                    .await
                {
                    warn!(%error, "WakeUpQueue: failed to durably claim due entries");
                    let mut retry = current;
                    for entry in retry.iter_mut().filter(|entry| {
                        matches!(
                            entry.kind,
                            WakeKind::Scheduled
                                | WakeKind::TaskSchedule
                                | WakeKind::TaskRetry
                                | WakeKind::ExecutionRetry
                                | WakeKind::ChildCompleted
                        ) && entry.wake_at <= now
                    }) {
                        entry.wake_at = now + EXECUTION_RETRY_PERSISTENCE_BACKOFF;
                    }
                    retry.sort_by_key(|entry| entry.wake_at);
                    *self.entries.write().await = retry;
                    return Vec::new();
                }
            } else if let Err(error) = self
                .persist_transaction_entries(path.as_deref(), &pending)
                .await
            {
                // A configured durable queue must not dispatch rows whose
                // removal did not land: a restart or rolling peer would see
                // the old snapshot and fire them again. With no configured
                // path the helper succeeds as an intentional in-memory queue.
                warn!(%error, "WakeUpQueue: failed to persist drained entries");
                *self.entries.write().await = current;
                return Vec::new();
            }
            *self.entries.write().await = pending;
            if self
                .entries
                .read()
                .await
                .iter()
                .any(|entry| consumer.owns(&entry.kind) && entry.wake_at <= now)
            {
                // A capped page deliberately left due work behind. Interrupt a
                // listener's timer so it drains the next page once dispatch
                // capacity becomes available.
                self.changed.notify_one();
            }
        } else {
            // Even an empty drain refreshes this process from the authoritative
            // snapshot a rolling peer may have changed.
            *self.entries.write().await = current;
        }
        due
    }

    /// Acknowledge one leased scheduled automation wake. The task id, claim
    /// deadline, and immutable delivery generation form a compare-and-delete;
    /// a next-fire replacement published by the handler is never erased by
    /// this older delivery.
    pub async fn acknowledge_scheduled_wake(
        &self,
        task_id: &str,
        claimed_until: DateTime<Utc>,
        expected_generation: Option<&str>,
    ) -> Result<bool, ArtifactV2Error> {
        let _persistence = self.persistence_gate.lock().await;
        let (path, _file_guard, mut next) = self.begin_persistence_transaction(false).await?;
        let before = next.len();
        next.retain(|entry| {
            !(entry.kind == WakeKind::Scheduled
                && entry.task_id == task_id
                && entry.wake_at == claimed_until
                && entry.child_completed_generation.as_deref() == expected_generation)
        });
        let acknowledged = next.len() != before;
        if acknowledged {
            self.persist_transaction_entries(path.as_deref(), &next)
                .await?;
        }
        *self.entries.write().await = next;
        Ok(acknowledged)
    }

    /// Renew one in-flight task-addressed delivery. A slow handler must not
    /// silently outlive its fixed claim and let a rolling peer dispatch the
    /// same fire concurrently. Returning `None` means the exact generation is
    /// no longer owned; callers must drop/cancel their in-flight work and must
    /// not acknowledge a replacement.
    pub async fn renew_task_addressed_wake(
        &self,
        kind: WakeKind,
        task_id: &str,
        claimed_until: DateTime<Utc>,
        expected_generation: Option<&str>,
    ) -> Result<Option<DateTime<Utc>>, ArtifactV2Error> {
        if !matches!(
            kind,
            WakeKind::Scheduled | WakeKind::TaskSchedule | WakeKind::TaskRetry
        ) {
            return Err(ArtifactV2Error::InvalidRequest(
                "task-addressed wake renewal requires Scheduled, TaskSchedule, or TaskRetry"
                    .to_owned(),
            ));
        }
        let _persistence = self.persistence_gate.lock().await;
        let (path, _file_guard, mut next) = self.begin_persistence_transaction(false).await?;
        let positions = next
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                (entry.kind == kind
                    && entry.task_id == task_id
                    && entry.wake_at == claimed_until
                    && entry.child_completed_generation.as_deref() == expected_generation)
                    .then_some(index)
            })
            .take(2)
            .collect::<Vec<_>>();
        let [position] = positions.as_slice() else {
            *self.entries.write().await = next;
            return Ok(None);
        };
        let renewed_until = std::cmp::max(
            Utc::now() + DURABLE_WAKE_CLAIM_LEASE,
            claimed_until + chrono::Duration::seconds(1),
        );
        next[*position].wake_at = renewed_until;
        next.sort_by_key(|entry| entry.wake_at);
        self.persist_transaction_entries(path.as_deref(), &next)
            .await?;
        *self.entries.write().await = next;
        Ok(Some(renewed_until))
    }

    /// Acknowledge one leased V3 task recovery wake. These rows used to be
    /// removed before dispatch; retaining them through the handler closes the
    /// shutdown window where a bounded supervisor abort could lose the only
    /// retry hint. The immutable generation prevents a stale handler from
    /// deleting a replacement published for the same task and deadline.
    pub async fn acknowledge_task_wake(
        &self,
        kind: WakeKind,
        task_id: &str,
        claimed_until: DateTime<Utc>,
        expected_generation: Option<&str>,
    ) -> Result<bool, ArtifactV2Error> {
        if !matches!(kind, WakeKind::TaskSchedule | WakeKind::TaskRetry) {
            return Err(ArtifactV2Error::InvalidRequest(
                "task wake acknowledgement requires TaskSchedule or TaskRetry".to_owned(),
            ));
        }
        let _persistence = self.persistence_gate.lock().await;
        let (path, _file_guard, mut next) = self.begin_persistence_transaction(false).await?;
        let before = next.len();
        next.retain(|entry| {
            !(entry.kind == kind
                && entry.task_id == task_id
                && entry.wake_at == claimed_until
                && entry.child_completed_generation.as_deref() == expected_generation)
        });
        let acknowledged = next.len() != before;
        if acknowledged {
            self.persist_transaction_entries(path.as_deref(), &next)
                .await?;
        }
        *self.entries.write().await = next;
        Ok(acknowledged)
    }

    /// Acknowledge one leased child-completion reconciliation. This is a
    /// compare-and-delete: a terminal child or depth-bounded continuation may
    /// publish a replacement wake while an old handler is still running, and
    /// the stale handler must not erase that newer generation.
    pub async fn acknowledge_child_completed_wake(
        &self,
        execution_id: &str,
        claimed_until: DateTime<Utc>,
        expected_generation: Option<&str>,
    ) -> Result<bool, ArtifactV2Error> {
        let _persistence = self.persistence_gate.lock().await;
        let (path, _file_guard, mut next) = self.begin_persistence_transaction(true).await?;
        let before = next.len();
        next.retain(|entry| {
            !(entry.kind == WakeKind::ChildCompleted
                && entry.execution_id.as_deref() == Some(execution_id)
                && entry.wake_at == claimed_until
                && entry.child_completed_generation.as_deref() == expected_generation)
        });
        let acknowledged = next.len() != before;
        if acknowledged {
            self.persist_transaction_entries(path.as_deref(), &next)
                .await?;
        }
        *self.entries.write().await = next;
        Ok(acknowledged)
    }

    /// Acknowledge one leased exact-execution wake.
    ///
    /// The lease timestamp plus immutable due/source axis make this an exact
    /// compare-and-delete. A replacement is allowed to use the old claim's
    /// timestamp as its new due time; comparing only mutable `wake_at` would let
    /// the stale handler erase that newer sleeping generation.
    pub async fn acknowledge_execution_retry_wake(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
        execution_id: &str,
        claimed_until: DateTime<Utc>,
        expected_due_at: Option<DateTime<Utc>>,
        expected_stateless_source_segment: Option<&str>,
    ) -> Result<bool, ArtifactV2Error> {
        let _persistence = self.persistence_gate.lock().await;
        let (path, _file_guard, mut next) = self.begin_persistence_transaction(true).await?;
        let before = next.len();
        next.retain(|entry| {
            !(entry.kind == WakeKind::ExecutionRetry
                && entry.execution_id.as_deref() == Some(execution_id)
                && entry.task_id == task_id
                && entry.principal.as_deref() == Some(principal)
                && entry.workspace.as_deref() == Some(workspace)
                && entry.wake_at == claimed_until
                && entry.execution_retry_due_at == expected_due_at
                && entry.stateless_source_segment.as_deref() == expected_stateless_source_segment)
        });
        let acknowledged = next.len() != before;
        if acknowledged {
            // Persist before publishing the deletion. If acknowledgement I/O
            // fails, the live process must continue retrying the retained row
            // instead of waiting for a restart to rediscover it from disk.
            self.persist_transaction_entries(path.as_deref(), &next)
                .await?;
        }
        *self.entries.write().await = next;
        Ok(acknowledged)
    }

    /// Persist that one claimed exact retry has crossed Artifact activation and
    /// canonical dispatch admission. The row remains present until a later
    /// delivery observes a live runtime owner or settlement.
    pub async fn mark_execution_retry_started(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
        execution_id: &str,
        claimed_until: DateTime<Utc>,
        expected_due_at: Option<DateTime<Utc>>,
        expected_stateless_source_segment: Option<&str>,
    ) -> Result<bool, ArtifactV2Error> {
        let _persistence = self.persistence_gate.lock().await;
        let (path, _file_guard, mut next) = self.begin_persistence_transaction(true).await?;
        let Some(entry) = next.iter_mut().find(|entry| {
            entry.kind == WakeKind::ExecutionRetry
                && entry.execution_id.as_deref() == Some(execution_id)
                && entry.task_id == task_id
                && entry.principal.as_deref() == Some(principal)
                && entry.workspace.as_deref() == Some(workspace)
                && entry.wake_at == claimed_until
                && entry.execution_retry_due_at == expected_due_at
                && entry.stateless_source_segment.as_deref() == expected_stateless_source_segment
        }) else {
            *self.entries.write().await = next;
            return Ok(false);
        };
        entry.execution_retry_started = true;
        self.persist_transaction_entries(path.as_deref(), &next)
            .await?;
        *self.entries.write().await = next;
        Ok(true)
    }

    /// Transfer a successful adoption marker to the current claim when the
    /// original handler outlived its lease. Scope plus immutable due/source
    /// identity prevents another workspace or a replacement generation from
    /// inheriting that authority.
    pub async fn mark_current_execution_retry_started(
        &self,
        principal: &str,
        workspace: &str,
        task_id: &str,
        execution_id: &str,
        expected_due_at: Option<DateTime<Utc>>,
        expected_stateless_source_segment: Option<&str>,
    ) -> Result<bool, ArtifactV2Error> {
        let _persistence = self.persistence_gate.lock().await;
        let (path, _file_guard, mut next) = self.begin_persistence_transaction(true).await?;
        let Some(entry) = next.iter_mut().find(|entry| {
            entry.kind == WakeKind::ExecutionRetry
                && entry.execution_id.as_deref() == Some(execution_id)
                && entry.task_id == task_id
                && entry.principal.as_deref() == Some(principal)
                && entry.workspace.as_deref() == Some(workspace)
                && entry.execution_retry_due_at == expected_due_at
                && entry.stateless_source_segment.as_deref() == expected_stateless_source_segment
        }) else {
            *self.entries.write().await = next;
            return Ok(false);
        };
        entry.execution_retry_started = true;
        self.persist_transaction_entries(path.as_deref(), &next)
            .await?;
        *self.entries.write().await = next;
        Ok(true)
    }

    /// Peek at the earliest wake owned by the production web/API dispatcher.
    pub async fn next_wake_at(&self) -> Option<DateTime<Utc>> {
        self.next_wake_at_for(WakeConsumer::ProductionWeb).await
    }

    /// Peek at the earliest scheduled wake owned by `consumer` without
    /// removing it. A partial consumer must use the same selector for its timer
    /// and drain, otherwise an unowned overdue row can create a zero-delay loop.
    pub async fn next_wake_at_for(&self, consumer: WakeConsumer) -> Option<DateTime<Utc>> {
        let _persistence = self.persistence_gate.lock().await;
        let current = match self.begin_persistence_transaction(false).await {
            Ok((_path, _file_guard, current)) => current,
            Err(error) => {
                warn!(%error, "WakeUpQueue: durable next-wake refresh failed; using local snapshot");
                return self
                    .entries
                    .read()
                    .await
                    .iter()
                    .find(|entry| consumer.owns(&entry.kind))
                    .map(|entry| entry.wake_at);
            },
        };
        let next = current
            .iter()
            .find(|entry| consumer.owns(&entry.kind))
            .map(|entry| entry.wake_at);
        *self.entries.write().await = current;
        next
    }

    /// Wait until a mutation asks dispatchers to recompute their timer.
    ///
    /// `notify_one` retains a permit when publication races registration, so a
    /// caller that has just read an obsolete `next_wake_at` cannot miss the
    /// earlier wake. The durable queue row remains delivery authority; callers
    /// always re-read [`Self::next_wake_at`] after this returns.
    pub async fn wait_for_change(&self) {
        self.changed.notified().await;
    }

    /// Load entries from the configured persist_path, replacing current state.
    pub async fn restore_from_disk(&self) {
        if let Err(error) = self.restore_from_disk_result().await {
            warn!(%error, "WakeUpQueue: failed to restore durable queue");
        }
    }

    /// Observable restore used by lifecycle recovery that must not interpret a
    /// storage failure as proof that no exact wake exists.
    pub async fn restore_from_disk_result(&self) -> Result<(), ArtifactV2Error> {
        let Some(path) = self.persistence_path() else {
            return Err(ArtifactV2Error::Runtime(
                "WakeUpQueue persistence path is not configured".to_owned(),
            ));
        };
        let _persistence = self.persistence_gate.lock().await;
        let _file_guard = AgentStorage::acquire_file_lock_exclusive(&path)
            .await
            .map_err(|error| {
                ArtifactV2Error::Runtime(format!(
                    "WakeUpQueue cross-process restore lock failed: {error}"
                ))
            })?;
        if let Some(entries) = load_persisted_entries(&path).await? {
            *self.entries.write().await = entries;
            self.changed.notify_one();
        }
        Ok(())
    }

    async fn persist_result(&self) -> Result<(), ArtifactV2Error> {
        let _persistence = self.persistence_gate.lock().await;
        let (path, _file_guard, current) = self.begin_persistence_transaction(true).await?;
        self.persist_transaction_entries(path.as_deref(), &current)
            .await?;
        *self.entries.write().await = current;
        Ok(())
    }

    pub async fn persist(&self) {
        if let Err(error) = self.persist_result().await {
            warn!("WakeUpQueue: failed to persist: {error}");
        }
    }
}

async fn load_persisted_entries(
    path: &std::path::Path,
) -> Result<Option<Vec<WakeEntry>>, ArtifactV2Error> {
    let workspace_layout = workspace_for_persist_path(path);
    let entries = match workspace_layout
        .read_json_bounded_stream_path::<Vec<WakeEntry>, _>(
            path,
            MAX_WAKE_QUEUE_FILE_BYTES,
            MAX_WAKE_QUEUE_JSON_DEPTH,
            MAX_WAKE_QUEUE_JSON_NODES,
        )
        .await
    {
        Ok(entries) => entries,
        Err(ArtifactV2Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        },
        Err(error) => return Err(error),
    };
    if entries.len() > MAX_WAKE_QUEUE_ENTRIES {
        return Err(ArtifactV2Error::InvalidRequest(format!(
            "WakeUpQueue persisted queue exceeds its {MAX_WAKE_QUEUE_ENTRIES}-entry limit"
        )));
    }
    let original_len = entries.len();
    let mut retained = entries
        .into_iter()
        .filter(valid_persisted_entry)
        .collect::<Vec<_>>();
    retained.sort_by_key(|entry| entry.wake_at);
    if retained.len() < original_len {
        warn!(
            path = %path.display(),
            dropped = original_len - retained.len(),
            "WakeUpQueue: dropping persisted entries with no usable address, incomplete exact-retry binding, or oversized identifier"
        );
    }
    Ok(Some(retained))
}

fn valid_persisted_entry(entry: &WakeEntry) -> bool {
    let has_legacy_address = !entry.task_id.trim().is_empty()
        || entry
            .execution_id
            .as_ref()
            .is_some_and(|id| !id.trim().is_empty());
    let exact_binding_is_complete = entry.kind != WakeKind::ExecutionRetry
        || (!entry.task_id.trim().is_empty()
            && entry
                .execution_id
                .as_ref()
                .is_some_and(|id| !id.trim().is_empty())
            && entry
                .principal
                .as_ref()
                .is_some_and(|value| !value.trim().is_empty())
            && entry
                .workspace
                .as_ref()
                .is_some_and(|value| !value.trim().is_empty())
            && entry.execution_retry_due_at.is_some());
    let identifiers_are_bounded = [
        Some(entry.task_id.as_str()),
        entry.principal.as_deref(),
        entry.workspace.as_deref(),
        entry.execution_id.as_deref(),
        entry.stateless_source_segment.as_deref(),
        entry.child_completed_generation.as_deref(),
        entry.agent_id.as_deref(),
        entry.goal_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    .all(|value| value.len() <= MAX_WAKE_ID_BYTES);
    has_legacy_address && exact_binding_is_complete && identifiers_are_bounded
}

async fn persist_entries_at_path(
    path: &std::path::Path,
    entries: &[WakeEntry],
) -> Result<(), ArtifactV2Error> {
    if entries.len() > MAX_WAKE_QUEUE_ENTRIES {
        return Err(ArtifactV2Error::InvalidRequest(format!(
            "WakeUpQueue exceeds its {MAX_WAKE_QUEUE_ENTRIES}-entry capacity"
        )));
    }
    if entries.iter().any(|entry| !valid_persisted_entry(entry)) {
        return Err(ArtifactV2Error::InvalidRequest(
            "WakeUpQueue contains an incomplete or oversized durable entry".to_owned(),
        ));
    }
    // Serialization can approach the four-MiB queue ceiling. Keep both it and
    // atomic publication off Tokio workers; the streaming writer aborts before
    // crossing the byte ceiling instead of allocating a second full String.
    workspace_for_persist_path(path)
        .write_json_value_atomic_stream_path(
            path,
            entries.to_vec(),
            MAX_WAKE_QUEUE_FILE_BYTES as usize,
        )
        .await
}

fn workspace_for_persist_path(path: &std::path::Path) -> ArtifactV2Workspace {
    let root = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    ArtifactV2Workspace::with_local_file_provider(root)
}

pub fn scoped_automation_task_id(
    principal: &str,
    workspace: &str,
    agent_id: &str,
    goal_id: &str,
) -> String {
    format!(
        "scoped:{}:{}:{}:{}",
        hex::encode(principal.as_bytes()),
        hex::encode(workspace.as_bytes()),
        hex::encode(agent_id.as_bytes()),
        hex::encode(goal_id.as_bytes())
    )
}

pub fn parse_scoped_automation_task_id(task_id: &str) -> Option<(String, String, String, String)> {
    let rest = task_id.strip_prefix("scoped:")?;
    let mut parts = rest.splitn(4, ':');
    let principal = String::from_utf8(hex::decode(parts.next()?).ok()?).ok()?;
    let workspace = String::from_utf8(hex::decode(parts.next()?).ok()?).ok()?;
    let agent_id = String::from_utf8(hex::decode(parts.next()?).ok()?).ok()?;
    let goal_id = String::from_utf8(hex::decode(parts.next()?).ok()?).ok()?;
    if principal.is_empty() || workspace.is_empty() || agent_id.is_empty() || goal_id.is_empty() {
        return None;
    }
    Some((principal, workspace, agent_id, goal_id))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use chrono::Duration;
    use tempfile::tempdir;

    fn scheduled_entry(task_id: impl Into<String>, wake_at: DateTime<Utc>) -> WakeEntry {
        WakeEntry {
            task_id: task_id.into(),
            principal: None,
            workspace: None,
            execution_id: None,
            stateless_source_segment: None,
            agent_id: None,
            goal_id: None,
            wake_at,
            execution_retry_due_at: None,
            child_completed_generation: None,
            kind: WakeKind::Scheduled,
            execution_retry_started: false,
            execution_retry_claim_attempt: 0,
        }
    }

    #[tokio::test]
    async fn test_schedule_and_drain() {
        let q = WakeUpQueue::new();
        let t = Utc::now() - Duration::seconds(5);
        q.schedule("task-1", t).await;
        let due = q.drain_due().await;
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].task_id, "task-1");
        assert!(due[0].execution_id.is_none());
        assert!(due[0].child_completed_generation.is_some());
        assert!(
            q.next_wake_at().await.is_some(),
            "scheduled wakes remain leased until their handler acknowledges"
        );
        let renewed_until = q
            .renew_task_addressed_wake(
                WakeKind::Scheduled,
                "task-1",
                due[0].wake_at,
                due[0].child_completed_generation.as_deref(),
            )
            .await
            .expect("in-memory scheduled renewal")
            .expect("exact scheduled generation remains owned");
        assert!(renewed_until > due[0].wake_at);
        assert!(q
            .acknowledge_scheduled_wake(
                "task-1",
                renewed_until,
                due[0].child_completed_generation.as_deref(),
            )
            .await
            .expect("in-memory scheduled acknowledgement"));
        assert!(q.next_wake_at().await.is_none());
    }

    #[tokio::test]
    async fn reschedule_preserves_an_in_flight_scheduled_generation() {
        let q = WakeUpQueue::new();
        let dir = tempdir().expect("tempdir");
        q.set_persist_path(dir.path().join("wakes.json"));
        q.schedule("scheduled-task", Utc::now() - Duration::seconds(1))
            .await;
        let claimed = q.drain_due().await;
        assert_eq!(claimed.len(), 1);
        let replacement_at = claimed[0].wake_at;
        q.schedule("scheduled-task", replacement_at).await;

        assert!(q
            .acknowledge_scheduled_wake(
                "scheduled-task",
                claimed[0].wake_at,
                claimed[0].child_completed_generation.as_deref(),
            )
            .await
            .expect("in-flight scheduled acknowledgement"));
        assert_eq!(q.next_wake_at().await, Some(replacement_at));
    }

    #[tokio::test]
    async fn in_flight_scheduled_generation_remains_renewable_after_reschedule() {
        let q = WakeUpQueue::new();
        let dir = tempdir().expect("tempdir");
        q.set_persist_path(dir.path().join("wakes.json"));
        q.schedule("scheduled-task", Utc::now() - Duration::seconds(1))
            .await;
        let claimed = q.drain_due().await;
        assert_eq!(claimed.len(), 1);
        let replacement_at = claimed[0].wake_at;
        q.schedule("scheduled-task", replacement_at).await;

        let renewed_until = q
            .renew_task_addressed_wake(
                WakeKind::Scheduled,
                "scheduled-task",
                claimed[0].wake_at,
                claimed[0].child_completed_generation.as_deref(),
            )
            .await
            .expect("in-flight scheduled renewal")
            .expect("rescheduling retains the live delivery generation");
        assert!(renewed_until > claimed[0].wake_at);
        assert_eq!(q.next_wake_at().await, Some(replacement_at));
    }

    #[tokio::test]
    async fn cross_process_scheduled_drain_is_a_single_durable_claim() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("wakes.json");
        let first = WakeUpQueue::new();
        let second = WakeUpQueue::new();
        first.set_persist_path(path.clone());
        second.set_persist_path(path);
        first
            .schedule("scheduled-task", Utc::now() - Duration::seconds(1))
            .await;

        let claimed = first.drain_due().await;
        assert_eq!(claimed.len(), 1);
        assert!(second.drain_due().await.is_empty());
        assert!(second
            .acknowledge_scheduled_wake(
                "scheduled-task",
                claimed[0].wake_at,
                claimed[0].child_completed_generation.as_deref(),
            )
            .await
            .expect("peer exact acknowledgement"));
        assert!(first.next_wake_at().await.is_none());
    }

    #[tokio::test]
    async fn runtime_consumer_never_drains_web_owned_task_wakes() {
        let q = WakeUpQueue::new();
        let now = Utc::now();
        let task_schedule_at = now - Duration::seconds(3);
        let task_retry_at = now - Duration::seconds(2);
        let runtime_at = now - Duration::seconds(1);
        q.schedule_task_schedule_wake("scheduled-task", task_schedule_at)
            .await;
        q.schedule_task_retry_wake("retry-task", task_retry_at)
            .await;
        q.schedule("legacy-runtime-task", runtime_at).await;

        assert_eq!(
            q.next_wake_at_for(WakeConsumer::RuntimeLegacy).await,
            Some(runtime_at),
            "an older web-owned row must not drive the runtime watcher's timer"
        );
        let runtime_due = q.drain_due_for(WakeConsumer::RuntimeLegacy).await;
        assert_eq!(runtime_due.len(), 1);
        assert_eq!(runtime_due[0].kind, WakeKind::Scheduled);

        assert_eq!(q.next_wake_at().await, Some(task_schedule_at));
        let production_due = q.drain_due().await;
        assert_eq!(production_due.len(), 2);
        assert!(production_due
            .iter()
            .all(|entry| matches!(entry.kind, WakeKind::TaskSchedule | WakeKind::TaskRetry)));
    }

    #[tokio::test]
    async fn test_future_entry_not_drained() {
        let q = WakeUpQueue::new();
        let t = Utc::now() + Duration::hours(1);
        q.schedule("task-2", t).await;
        let due = q.drain_due().await;
        assert!(due.is_empty());
    }

    #[tokio::test]
    async fn test_cancel_removes_entry() {
        let q = WakeUpQueue::new();
        let t = Utc::now() - Duration::seconds(1);
        q.schedule("task-3", t).await;
        q.cancel("task-3").await;
        let due = q.drain_due().await;
        assert!(due.is_empty());
    }

    #[tokio::test]
    async fn test_cancel_scoped_removes_entry() {
        let q = WakeUpQueue::new();
        let t = Utc::now() - Duration::seconds(1);
        q.schedule_scoped("principal-a", "workspace-a", "a3", "g3", t)
            .await;
        q.cancel_scoped("principal-a", "workspace-a", "a3", "g3")
            .await;
        let due = q.drain_due().await;
        assert!(due.is_empty());
    }

    #[tokio::test]
    async fn cancel_does_not_revoke_an_in_flight_generation() {
        let q = WakeUpQueue::new();
        q.schedule_task_retry_wake("leased-task", Utc::now() - Duration::seconds(1))
            .await;
        let claimed = q.drain_due().await;
        assert_eq!(claimed.len(), 1);

        q.cancel("leased-task").await;

        assert!(q
            .renew_task_addressed_wake(
                WakeKind::TaskRetry,
                "leased-task",
                claimed[0].wake_at,
                claimed[0].child_completed_generation.as_deref(),
            )
            .await
            .expect("in-flight generation renewal")
            .is_some());
    }

    #[tokio::test]
    async fn test_wake_now_triggers_drain() {
        let q = WakeUpQueue::new();
        let t = Utc::now() + Duration::hours(24);
        q.schedule("task-4", t).await;
        q.wake_now("task-4").await;
        let due = q.drain_due().await;
        assert_eq!(due.len(), 1);
    }

    #[tokio::test]
    async fn wake_now_does_not_mutate_an_in_flight_generation() {
        let q = WakeUpQueue::new();
        q.schedule_task_schedule_wake("leased-task", Utc::now() - Duration::seconds(1))
            .await;
        let claimed = q.drain_due().await;
        assert_eq!(claimed.len(), 1);

        q.wake_now("leased-task").await;

        assert!(q
            .renew_task_addressed_wake(
                WakeKind::TaskSchedule,
                "leased-task",
                claimed[0].wake_at,
                claimed[0].child_completed_generation.as_deref(),
            )
            .await
            .expect("in-flight generation renewal")
            .is_some());
    }

    #[tokio::test]
    async fn test_replace_existing_entry() {
        let q = WakeUpQueue::new();
        let t1 = Utc::now() + Duration::hours(2);
        let t2 = Utc::now() - Duration::seconds(1);
        q.schedule("task-5", t1).await;
        q.schedule("task-5", t2).await;
        let due = q.drain_due().await;
        assert_eq!(due.len(), 1);
    }

    #[tokio::test]
    async fn test_schedule_task_schedule_wake_and_drain() {
        let q = WakeUpQueue::new();
        let t = Utc::now() - Duration::seconds(1);
        q.schedule_task_schedule_wake("cron-task-42", t).await;
        let due = q.drain_due().await;
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].task_id, "cron-task-42");
        assert_eq!(due[0].kind, WakeKind::TaskSchedule);
        assert!(due[0].child_completed_generation.is_some());
        let renewed_until = q
            .renew_task_addressed_wake(
                WakeKind::TaskSchedule,
                "cron-task-42",
                due[0].wake_at,
                due[0].child_completed_generation.as_deref(),
            )
            .await
            .expect("task-schedule renewal")
            .expect("task-schedule generation remains owned");
        assert!(q
            .acknowledge_task_wake(
                WakeKind::TaskSchedule,
                "cron-task-42",
                renewed_until,
                due[0].child_completed_generation.as_deref(),
            )
            .await
            .expect("task-schedule acknowledgement"));
        assert!(q.next_wake_at().await.is_none());
    }

    #[tokio::test]
    async fn task_retry_reschedule_preserves_the_in_flight_generation() {
        let q = WakeUpQueue::new();
        let dir = tempdir().expect("tempdir");
        q.set_persist_path(dir.path().join("wakes.json"));
        q.schedule_task_retry_wake("retry-task", Utc::now() - Duration::seconds(1))
            .await;
        let claimed = q.drain_due().await;
        assert_eq!(claimed.len(), 1);
        let replacement_at = claimed[0].wake_at;
        q.schedule_task_retry_wake("retry-task", replacement_at)
            .await;

        assert!(q
            .acknowledge_task_wake(
                WakeKind::TaskRetry,
                "retry-task",
                claimed[0].wake_at,
                claimed[0].child_completed_generation.as_deref(),
            )
            .await
            .expect("in-flight task retry acknowledgement"));
        assert_eq!(q.next_wake_at().await, Some(replacement_at));
    }

    #[tokio::test]
    async fn test_schedule_child_completed_wake_and_drain() {
        let q = WakeUpQueue::new();
        let dir = tempdir().expect("tempdir");
        q.set_persist_path(dir.path().join("wakes.json"));
        let t = Utc::now() - Duration::seconds(1);
        q.schedule_child_completed_wake("thread-parent-1", t)
            .await
            .expect("durable child wake");
        let due = q.drain_due().await;
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].execution_id.as_deref(), Some("thread-parent-1"));
        assert!(due[0].task_id.is_empty());
        assert_eq!(due[0].kind, WakeKind::ChildCompleted);
        assert!(
            q.next_wake_at().await.is_some(),
            "drain leases the child wake until handler acknowledgement"
        );
        assert!(q
            .acknowledge_child_completed_wake(
                "thread-parent-1",
                due[0].wake_at,
                due[0].child_completed_generation.as_deref(),
            )
            .await
            .expect("durable child acknowledgement"));
        assert!(q.next_wake_at().await.is_none());
    }

    #[tokio::test]
    async fn stale_child_ack_cannot_delete_a_replacement_wake() {
        let q = WakeUpQueue::new();
        let dir = tempdir().expect("tempdir");
        q.set_persist_path(dir.path().join("wakes.json"));
        q.schedule_child_completed_wake("parent", Utc::now() - Duration::seconds(1))
            .await
            .expect("initial child wake");
        let claimed = q.drain_due().await;
        // Reuse the lease timestamp deliberately: timestamp-only compare and
        // delete would erase this newer generation.
        let replacement_at = claimed[0].wake_at;
        q.schedule_child_completed_wake("parent", replacement_at)
            .await
            .expect("replacement child wake");

        assert!(!q
            .acknowledge_child_completed_wake(
                "parent",
                claimed[0].wake_at,
                claimed[0].child_completed_generation.as_deref(),
            )
            .await
            .expect("stale acknowledgement is harmless"));
        assert_eq!(q.next_wake_at().await, Some(replacement_at));
    }

    #[tokio::test]
    async fn execution_retry_wake_preserves_the_exact_task_execution_binding() {
        let q = WakeUpQueue::new();
        let dir = tempdir().expect("tempdir");
        q.set_persist_path(dir.path().join("wakes.json"));
        let t = Utc::now() - Duration::seconds(1);
        q.schedule_execution_retry_wake("principal-a", "workspace-a", "task-pin", "exec-pin", t)
            .await
            .expect("exact wake must be durable before admission succeeds");
        let due = q.drain_due().await;
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].task_id, "task-pin");
        assert_eq!(due[0].principal.as_deref(), Some("principal-a"));
        assert_eq!(due[0].workspace.as_deref(), Some("workspace-a"));
        assert_eq!(due[0].execution_id.as_deref(), Some("exec-pin"));
        assert_eq!(due[0].kind, WakeKind::ExecutionRetry);
        assert!(q.next_wake_at().await.is_some(), "drain leases exact wakes");
        assert!(q
            .acknowledge_execution_retry_wake(
                "principal-a",
                "workspace-a",
                "task-pin",
                "exec-pin",
                due[0].wake_at,
                due[0].execution_retry_due_at.clone(),
                due[0].stateless_source_segment.as_deref(),
            )
            .await
            .expect("durable acknowledgement"));
        assert!(q.next_wake_at().await.is_none());
    }

    #[tokio::test]
    async fn exact_retry_claim_issuance_waits_for_the_activation_generation_fence() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("wakes.json");
        let activation_process = WakeUpQueue::new();
        let claiming_process = WakeUpQueue::new();
        activation_process.set_persist_path(path.clone());
        claiming_process.set_persist_path(path);
        activation_process
            .schedule_execution_retry_wake_for_segment(
                "principal",
                "workspace",
                "task",
                "execution",
                Utc::now() - Duration::seconds(1),
                Some("segment"),
            )
            .await
            .expect("publish exact wake");
        claiming_process
            .restore_from_disk_result()
            .await
            .expect("peer restores exact wake");

        let activation_guard = activation_process
            .lock_execution_retry_claim("principal", "workspace", "task", "execution")
            .await
            .expect("activation owns exact generation fence");
        let mut peer_drain = {
            let claiming_process = Arc::clone(&claiming_process);
            tokio::spawn(async move { claiming_process.drain_due().await })
        };
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(25), &mut peer_drain,)
                .await
                .is_err(),
            "a peer must not renew/claim the row during current-generation Artifact activation"
        );

        drop(activation_guard);
        let claimed = tokio::time::timeout(std::time::Duration::from_millis(250), peer_drain)
            .await
            .expect("peer resumes after activation fence is released")
            .expect("peer drain task");
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].execution_retry_claim_attempt, 1);
    }

    #[tokio::test]
    async fn duplicate_exact_retry_rows_are_never_issued_as_claims() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("wakes.json");
        let due_at = Utc::now() - Duration::seconds(1);
        let duplicate = WakeEntry {
            task_id: "task".to_owned(),
            principal: Some("principal".to_owned()),
            workspace: Some("workspace".to_owned()),
            execution_id: Some("execution".to_owned()),
            stateless_source_segment: Some("segment".to_owned()),
            agent_id: None,
            goal_id: None,
            wake_at: due_at,
            execution_retry_due_at: Some(due_at),
            child_completed_generation: None,
            kind: WakeKind::ExecutionRetry,
            execution_retry_started: false,
            execution_retry_claim_attempt: 0,
        };
        persist_entries_at_path(&path, &[duplicate.clone(), duplicate])
            .await
            .expect("persist corrupt duplicate fixture");
        let q = WakeUpQueue::new();
        q.set_persist_path(path);
        q.restore_from_disk_result()
            .await
            .expect("restore duplicate fixture for fail-closed drain");

        assert!(q.drain_due().await.is_empty());
        let entries = q.entries.read().await;
        assert_eq!(
            entries.len(),
            2,
            "ambiguous rows remain available for repair"
        );
        assert!(entries
            .iter()
            .all(|entry| entry.execution_retry_claim_attempt == 0));
    }

    #[test]
    fn exact_retry_claim_lock_order_precedes_the_global_persistence_transaction() {
        let source = include_str!("wake_up_queue.rs");
        let drain = source
            .split("pub async fn drain_due_for_limit")
            .nth(1)
            .and_then(|tail| {
                tail.split("pub async fn acknowledge_child_completed_wake")
                    .next()
            })
            .expect("drain_due body");
        let exact_lock = drain
            .find(".lock_execution_retry_claim")
            .expect("per-execution generation lock");
        let global_lock = drain
            .find("self.persistence_gate.lock().await")
            .expect("global persistence transaction lock");
        assert!(
            exact_lock < global_lock,
            "claim issuance must preserve exact-generation -> global lock ordering"
        );
    }

    #[tokio::test]
    async fn ensure_exact_retry_preserves_a_matching_started_claim_generation() {
        let q = WakeUpQueue::new();
        let dir = tempdir().expect("tempdir");
        q.set_persist_path(dir.path().join("wakes.json"));
        let due_at = Utc::now() - Duration::seconds(1);
        q.schedule_execution_retry_wake_for_segment(
            "p",
            "w",
            "task",
            "exec",
            due_at,
            Some("segment-1"),
        )
        .await
        .expect("initial exact generation");
        let claimed = q.drain_due().await;
        assert_eq!(claimed.len(), 1);
        assert!(q
            .mark_execution_retry_started(
                "p",
                "w",
                "task",
                "exec",
                claimed[0].wake_at,
                claimed[0].execution_retry_due_at.clone(),
                claimed[0].stateless_source_segment.as_deref(),
            )
            .await
            .expect("persist started marker"));

        assert!(!q
            .ensure_execution_retry_wake_for_segment(
                "p",
                "w",
                "task",
                "exec",
                due_at,
                Some("segment-1"),
            )
            .await
            .expect("atomic startup ensure"));
        let preserved = q
            .execution_retry_wake("p", "w", "task", "exec")
            .await
            .expect("matching generation remains");
        assert_eq!(preserved.wake_at, claimed[0].wake_at);
        assert_eq!(preserved.execution_retry_due_at, Some(due_at));
        assert_eq!(
            preserved.stateless_source_segment.as_deref(),
            Some("segment-1")
        );
        assert!(preserved.execution_retry_started);
        assert_eq!(preserved.execution_retry_claim_attempt, 1);
    }

    #[tokio::test]
    async fn duplicate_scoped_exact_rows_never_mint_current_generation_authority() {
        let q = WakeUpQueue::new();
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("wakes.json");
        q.set_persist_path(path.clone());
        let due_at = Utc::now() - Duration::seconds(1);
        let entry = WakeEntry {
            task_id: "task".to_string(),
            principal: Some("p".to_string()),
            workspace: Some("w".to_string()),
            execution_id: Some("exec".to_string()),
            stateless_source_segment: Some("segment".to_string()),
            agent_id: None,
            goal_id: None,
            wake_at: due_at,
            execution_retry_due_at: Some(due_at),
            child_completed_generation: None,
            kind: WakeKind::ExecutionRetry,
            execution_retry_started: false,
            execution_retry_claim_attempt: 1,
        };
        std::fs::write(
            &path,
            serde_json::to_vec(&vec![entry.clone(), entry]).expect("encode duplicate fixture"),
        )
        .expect("write duplicate fixture");

        assert!(q
            .execution_retry_wake("p", "w", "task", "exec")
            .await
            .is_none());
        assert!(
            !q.execution_retry_claim_is_current(
                "p",
                "w",
                "task",
                "exec",
                due_at,
                Some(due_at),
                Some("segment"),
            )
            .await
        );
    }

    #[tokio::test]
    async fn a_renewed_exact_retry_lease_invalidates_the_prior_delivery_token() {
        let q = WakeUpQueue::new();
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("wakes.json");
        q.set_persist_path(path.clone());
        let due_at = Utc::now() - Duration::seconds(1);
        q.schedule_execution_retry_wake_for_segment(
            "p",
            "w",
            "task",
            "exec",
            due_at,
            Some("segment"),
        )
        .await
        .expect("initial exact generation");
        let first = q.drain_due().await.pop().expect("first durable claim");

        // Model lease expiry without waiting: disk is authoritative, so make
        // this exact row due there and let the ordinary claim path renew it.
        let mut durable: Vec<WakeEntry> =
            serde_json::from_slice(&std::fs::read(&path).expect("read durable queue"))
                .expect("decode durable queue");
        durable[0].wake_at = Utc::now() - Duration::milliseconds(1);
        std::fs::write(&path, serde_json::to_vec(&durable).expect("encode queue"))
            .expect("expire durable lease fixture");
        assert!(
            q.drain_due().await.is_empty(),
            "a disk-new due generation is first refreshed into the local mirror; it cannot be \
             claimed by a transaction that did not pre-acquire its exact fence"
        );
        let second = q.drain_due().await.pop().expect("renewed durable claim");
        assert_ne!(first.wake_at, second.wake_at);

        assert!(
            !q.execution_retry_claim_is_current(
                "p",
                "w",
                "task",
                "exec",
                first.wake_at,
                Some(due_at),
                Some("segment"),
            )
            .await
        );
        assert!(
            q.execution_retry_claim_is_current(
                "p",
                "w",
                "task",
                "exec",
                second.wake_at,
                Some(due_at),
                Some("segment"),
            )
            .await
        );
    }

    #[tokio::test]
    async fn exact_retry_identity_includes_scope_and_stale_ack_cannot_delete_replacement() {
        let q = WakeUpQueue::new();
        let dir = tempdir().expect("tempdir");
        q.set_persist_path(dir.path().join("wakes.json"));
        let due_at = Utc::now() - Duration::seconds(1);
        q.schedule_execution_retry_wake("p-a", "w-a", "task", "exec", due_at)
            .await
            .expect("scope a");
        q.schedule_execution_retry_wake("p-b", "w-b", "task", "exec", due_at)
            .await
            .expect("scope b");
        let claimed = q.drain_due().await;
        assert_eq!(claimed.len(), 2, "equal ids in distinct scopes coexist");
        let old_a = claimed
            .iter()
            .find(|entry| entry.principal.as_deref() == Some("p-a"))
            .expect("scope a claim");
        // Deliberately reuse the old claim lease as the replacement due time.
        // A mutable-wake-only acknowledgement would erase this new generation.
        let replacement_at = old_a.wake_at;
        q.schedule_execution_retry_wake("p-a", "w-a", "task", "exec", replacement_at)
            .await
            .expect("replacement");
        assert!(!q
            .mark_execution_retry_started(
                "p-a",
                "w-a",
                "task",
                "exec",
                old_a.wake_at,
                old_a.execution_retry_due_at.clone(),
                old_a.stateless_source_segment.as_deref(),
            )
            .await
            .expect("stale started marker is harmless"));
        assert!(!q
            .mark_current_execution_retry_started(
                "p-a",
                "w-a",
                "task",
                "exec",
                old_a.execution_retry_due_at.clone(),
                old_a.stateless_source_segment.as_deref(),
            )
            .await
            .expect("stale adoption transfer is harmless"));
        assert!(!q
            .acknowledge_execution_retry_wake(
                "p-a",
                "w-a",
                "task",
                "exec",
                old_a.wake_at,
                old_a.execution_retry_due_at.clone(),
                old_a.stateless_source_segment.as_deref(),
            )
            .await
            .expect("stale ack is harmless"));
        assert_eq!(
            q.next_wake_at().await,
            Some(claimed[1].wake_at.min(replacement_at))
        );
    }

    #[tokio::test]
    async fn exact_retry_claim_and_ack_fail_without_publishing_undurable_memory_state() {
        let q = WakeUpQueue::new();
        let dir = tempdir().expect("tempdir");
        let durable_path = dir.path().join("wakes.json");
        q.set_persist_path(durable_path);
        let due_at = Utc::now() - Duration::seconds(1);
        q.schedule_execution_retry_wake("p", "w", "task", "exec", due_at)
            .await
            .expect("initial exact wake");

        // A directory cannot be atomically replaced as the queue file. Claim
        // admission must fail before changing the in-memory due timestamp.
        q.set_persist_path(dir.path().to_path_buf());
        assert!(q.drain_due().await.is_empty());
        assert!(
            q.next_wake_at()
                .await
                .is_some_and(|retry_at| retry_at > due_at),
            "failed durable claim applies only a bounded in-memory backoff"
        );

        q.set_persist_path(dir.path().join("wakes.json"));
        {
            let mut entries = q.entries.write().await;
            entries[0].wake_at = Utc::now() - Duration::seconds(1);
        }
        let claimed = q.drain_due().await;
        assert_eq!(claimed.len(), 1);
        q.set_persist_path(dir.path().to_path_buf());
        assert!(q
            .acknowledge_execution_retry_wake(
                "p",
                "w",
                "task",
                "exec",
                claimed[0].wake_at,
                claimed[0].execution_retry_due_at.clone(),
                claimed[0].stateless_source_segment.as_deref(),
            )
            .await
            .is_err());
        assert_eq!(
            q.next_wake_at().await,
            Some(claimed[0].wake_at),
            "failed ack must retain the live retry row"
        );
    }

    #[tokio::test]
    async fn test_scoped_task_id_deterministic() {
        let id1 = scoped_automation_task_id("principal-a", "workspace-a", "agent-a", "goal-1");
        let id2 = scoped_automation_task_id("principal-a", "workspace-a", "agent-a", "goal-1");
        assert_eq!(id1, id2);
        let id3 = scoped_automation_task_id("principal-a", "workspace-a", "agent-b", "goal-1");
        assert_ne!(id1, id3);
    }

    #[tokio::test]
    async fn test_restore_drops_entries_missing_task_and_execution_ids() {
        let q = WakeUpQueue::new();
        let dir = tempdir().unwrap();
        let path = dir.path().join("wake_up_queue.json");
        q.set_persist_path(path.clone());

        let legacy_json = serde_json::json!([{
            "agent_id": "agent-a",
            "goal_id": "goal-1",
            "wake_at": Utc::now(),
            "kind": "Scheduled"
        }]);
        std::fs::write(&path, serde_json::to_string_pretty(&legacy_json).unwrap()).unwrap();

        q.restore_from_disk().await;
        let due = q.drain_due().await;

        assert!(due.is_empty());
    }

    #[tokio::test]
    async fn restore_drops_an_execution_retry_without_its_exact_scope_binding() {
        let q = WakeUpQueue::new();
        let dir = tempdir().unwrap();
        let path = dir.path().join("wake_up_queue.json");
        q.set_persist_path(path.clone());

        let malformed = serde_json::json!([{
            "task_id": "task-a",
            "execution_id": "exec-a",
            "wake_at": Utc::now(),
            "kind": "ExecutionRetry"
        }]);
        std::fs::write(&path, serde_json::to_vec_pretty(&malformed).unwrap()).unwrap();

        q.restore_from_disk().await;

        assert!(q.drain_due().await.is_empty());
        assert!(q.next_wake_at().await.is_none());
    }

    #[tokio::test]
    async fn restore_drops_an_oversized_exact_stateless_segment() {
        let q = WakeUpQueue::new();
        let dir = tempdir().unwrap();
        let path = dir.path().join("wake_up_queue.json");
        q.set_persist_path(path.clone());
        let due_at = Utc::now();
        let malformed = serde_json::json!([{
            "task_id": "task-a",
            "principal": "principal-a",
            "workspace": "workspace-a",
            "execution_id": "exec-a",
            "stateless_source_segment": "x".repeat(MAX_WAKE_ID_BYTES + 1),
            "wake_at": due_at,
            "execution_retry_due_at": due_at,
            "kind": "ExecutionRetry"
        }]);
        std::fs::write(&path, serde_json::to_vec(&malformed).unwrap()).unwrap();

        q.restore_from_disk().await;

        assert!(q.next_wake_at().await.is_none());
    }

    #[tokio::test]
    async fn an_exact_retry_cannot_be_published_with_an_empty_binding_component() {
        let q = WakeUpQueue::new();
        let dir = tempdir().unwrap();
        q.set_persist_path(dir.path().join("wake_up_queue.json"));

        let result = q
            .schedule_execution_retry_wake("principal", "", "task", "exec", Utc::now())
            .await;

        assert!(result.is_err());
        assert!(q.next_wake_at().await.is_none());
    }

    #[tokio::test]
    async fn test_persist_writes_execution_id_for_child_completed_wake() {
        let q = WakeUpQueue::new();
        let dir = tempdir().unwrap();
        let path = dir.path().join("wake_up_queue.json");
        q.set_persist_path(path.clone());

        q.schedule_child_completed_wake("exec-parent-1", Utc::now())
            .await
            .expect("durable child wake");

        let persisted = std::fs::read_to_string(&path).unwrap();
        assert!(persisted.contains("\"execution_id\""));
        assert!(!persisted.contains("\"thread_id\""));
    }

    #[tokio::test]
    async fn restore_sorts_before_next_wake_at_reads_the_first_row() {
        let q = WakeUpQueue::new();
        let dir = tempdir().unwrap();
        let path = dir.path().join("wake_up_queue.json");
        q.set_persist_path(path.clone());
        let early = Utc::now() + Duration::minutes(1);
        let late = Utc::now() + Duration::hours(1);
        let rows = serde_json::json!([
            { "task_id": "late", "wake_at": late, "kind": "Scheduled" },
            { "task_id": "early", "wake_at": early, "kind": "Scheduled" }
        ]);
        std::fs::write(&path, serde_json::to_vec_pretty(&rows).unwrap()).unwrap();

        q.restore_from_disk().await;

        assert_eq!(q.next_wake_at().await, Some(early));
    }

    #[tokio::test]
    async fn scheduling_an_earlier_wake_interrupts_a_registered_dispatcher() {
        let q = WakeUpQueue::new();
        let dir = tempdir().unwrap();
        q.set_persist_path(dir.path().join("wake_up_queue.json"));
        let late = Utc::now() + Duration::hours(12);
        q.schedule("late", late).await;
        // Consume the publication permit for the initial row. The next wait is
        // the dispatcher's long sleep after it observed `late`.
        q.wait_for_change().await;

        let waiter = {
            let q = Arc::clone(&q);
            tokio::spawn(async move {
                q.wait_for_change().await;
                q.next_wake_at().await
            })
        };
        tokio::task::yield_now().await;
        let earlier = Utc::now() - Duration::seconds(1);
        q.schedule("earlier", earlier).await;

        let observed = tokio::time::timeout(std::time::Duration::from_millis(250), waiter)
            .await
            .expect("the old twelve-hour timer must be interrupted")
            .expect("dispatcher task");
        assert_eq!(observed, Some(earlier));
    }

    #[tokio::test]
    async fn schedule_before_wait_registration_retains_a_change_permit() {
        let q = WakeUpQueue::new();
        let dir = tempdir().unwrap();
        q.set_persist_path(dir.path().join("wake_up_queue.json"));
        q.schedule("already-published", Utc::now()).await;

        tokio::time::timeout(std::time::Duration::from_millis(250), q.wait_for_change())
            .await
            .expect("notify_one must close publication-before-registration race");
    }

    #[tokio::test]
    async fn due_drain_is_page_bounded_and_retains_the_remainder() {
        let q = WakeUpQueue::new();
        let due_at = Utc::now() - Duration::seconds(1);
        {
            let mut entries = q.entries.write().await;
            entries.extend(
                (0..=MAX_WAKE_DISPATCH_BATCH)
                    .map(|index| scheduled_entry(format!("task-{index}"), due_at)),
            );
        }

        let first = q.drain_due().await;
        assert_eq!(first.len(), MAX_WAKE_DISPATCH_BATCH);
        assert_eq!(
            q.entries.read().await.len(),
            MAX_WAKE_DISPATCH_BATCH + 1,
            "the claimed page remains leased beside the one unclaimed remainder"
        );
        assert_eq!(q.drain_due().await.len(), 1);
    }

    #[tokio::test]
    async fn shared_dispatch_permits_bound_detached_wake_fanout() {
        let q = WakeUpQueue::new();
        let mut permits = Vec::new();
        for _ in 0..MAX_CONCURRENT_WAKE_DISPATCHES {
            permits.push(q.acquire_dispatch_permit().await);
        }

        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(25),
            q.acquire_dispatch_permit(),
        )
        .await
        .is_err());
        permits.pop();
        drop(
            tokio::time::timeout(
                std::time::Duration::from_millis(250),
                q.acquire_dispatch_permit(),
            )
            .await
            .expect("releasing one handler must admit one waiting dispatch"),
        );
    }

    #[tokio::test]
    async fn due_claim_count_never_exceeds_reserved_handler_capacity() {
        let q = WakeUpQueue::new();
        let mut occupied = Vec::new();
        for _ in 0..(MAX_CONCURRENT_WAKE_DISPATCHES - 2) {
            occupied.push(q.acquire_dispatch_permit().await);
        }
        let reserved = q.reserve_dispatch_page_permits();
        assert_eq!(reserved.len(), 2);

        let due_at = Utc::now() - Duration::seconds(1);
        {
            let mut entries = q.entries.write().await;
            entries
                .extend((0..3).map(|index| scheduled_entry(format!("reserved-{index}"), due_at)));
        }
        let claimed = q
            .drain_due_for_limit(WakeConsumer::ProductionWeb, reserved.len())
            .await;
        assert_eq!(claimed.len(), reserved.len());
        assert_eq!(
            q.entries
                .read()
                .await
                .iter()
                .filter(|entry| entry.wake_at <= Utc::now())
                .count(),
            1,
            "a row without a reserved handler slot must remain unclaimed"
        );
    }

    #[tokio::test]
    async fn cross_process_schedulers_reload_before_writing_and_do_not_lose_rows() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("wake_up_queue.json");
        let first = WakeUpQueue::new();
        let second = WakeUpQueue::new();
        first.set_persist_path(path.clone());
        second.set_persist_path(path.clone());

        first
            .schedule_child_completed_wake("parent-a", Utc::now())
            .await
            .expect("first process publishes");
        second
            .schedule_child_completed_wake("parent-b", Utc::now())
            .await
            .expect("second process reloads and publishes");

        let cold = WakeUpQueue::new();
        cold.set_persist_path(path);
        cold.restore_from_disk_result().await.expect("cold restore");
        let entries = cold.entries.read().await;
        assert_eq!(
            entries.len(),
            2,
            "a stale process must not overwrite its peer"
        );
        assert!(entries
            .iter()
            .any(|entry| entry.execution_id.as_deref() == Some("parent-a")));
        assert!(entries
            .iter()
            .any(|entry| entry.execution_id.as_deref() == Some("parent-b")));
    }

    #[tokio::test]
    async fn cross_process_drain_cannot_claim_one_exact_generation_twice() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("wake_up_queue.json");
        let first = WakeUpQueue::new();
        let second = WakeUpQueue::new();
        first.set_persist_path(path.clone());
        second.set_persist_path(path);
        let due_at = Utc::now() - Duration::seconds(1);
        first
            .schedule_execution_retry_wake(
                "principal-a",
                "workspace-a",
                "task-a",
                "execution-a",
                due_at,
            )
            .await
            .expect("publish exact retry");

        let claimed = first.drain_due().await;
        assert_eq!(claimed.len(), 1);
        assert!(
            second.drain_due().await.is_empty(),
            "the second process must reload the first process's durable claim lease"
        );
    }

    #[tokio::test]
    async fn restore_refuses_a_persisted_queue_above_the_entry_ceiling() {
        let q = WakeUpQueue::new();
        let dir = tempdir().unwrap();
        let path = dir.path().join("wake_up_queue.json");
        q.set_persist_path(path.clone());
        q.entries
            .write()
            .await
            .push(scheduled_entry("live-sentinel", Utc::now()));
        let rows = (0..=MAX_WAKE_QUEUE_ENTRIES)
            .map(|index| scheduled_entry(format!("persisted-{index}"), Utc::now()))
            .collect::<Vec<_>>();
        std::fs::write(&path, serde_json::to_vec(&rows).unwrap()).unwrap();

        q.restore_from_disk().await;

        let entries = q.entries.read().await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].task_id, "live-sentinel");
    }

    #[tokio::test]
    async fn exact_retry_admission_fails_closed_when_the_queue_is_full() {
        let q = WakeUpQueue::new();
        let dir = tempdir().unwrap();
        q.set_persist_path(dir.path().join("wake_up_queue.json"));
        let wake_at = Utc::now() + Duration::hours(1);
        q.entries.write().await.extend(
            (0..MAX_WAKE_QUEUE_ENTRIES)
                .map(|index| scheduled_entry(format!("task-{index}"), wake_at)),
        );

        let result = q
            .schedule_execution_retry_wake("p", "w", "task", "exec", wake_at)
            .await;

        assert!(result.is_err());
        assert_eq!(q.entries.read().await.len(), MAX_WAKE_QUEUE_ENTRIES);
    }
}
