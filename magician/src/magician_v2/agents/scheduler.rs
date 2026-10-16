//! Phase 3 durable scheduler core.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

use chrono::{DateTime, FixedOffset, Utc};
use chrono_tz::Tz;
use cron::Schedule as CronSchedule;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    sync::Mutex,
    task,
    time::{Duration as TokioDuration, Instant},
};
use tracing::warn;

use super::{
    storage::{AgentStorage, AgentStorageError},
    types::AgentDefinition,
    wake_up_queue::scoped_automation_task_id,
};

const SCHEDULER_WRITE_LOCK_RETRY_DELAY_MIN: TokioDuration = TokioDuration::from_millis(10);
const SCHEDULER_WRITE_LOCK_RETRY_DELAY_MAX: TokioDuration = TokioDuration::from_millis(250);
const SCHEDULER_WRITE_LOCK_MAX_WAIT: TokioDuration = TokioDuration::from_secs(30);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "trigger_kind", rename_all = "snake_case")]
pub enum SchedulerTriggerRegistration {
    Cron {
        schedule: String,
        timezone: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_run_at: Option<DateTime<Utc>>,
    },
    Event {
        pattern: String,
        filter: HashMap<String, String>,
    },
    Idle,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SchedulerEntryState {
    pub agent_id: String,
    pub goal_id: String,
    /// Optional task_id for entries managed through the task-centric scheduling path.
    /// Legacy entries (keyed by agent_id:goal_id) will have `None` until migrated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub trigger_seq: u64,
    pub last_triggered: Option<DateTime<Utc>>,
    pub registration: SchedulerTriggerRegistration,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SchedulerState {
    pub entries: HashMap<String, SchedulerEntryState>,
    pub last_tick: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledTrigger {
    pub principal: String,
    pub workspace: String,
    pub agent_id: String,
    pub goal_id: String,
    pub trigger_seq: u64,
}

#[derive(Debug, Clone)]
pub struct AgentScheduler {
    state: Arc<Mutex<SchedulerState>>,
    storage: AgentStorage,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SchedulerError {
    #[error("scheduler entry is not registered for agent `{agent_id}` goal `{goal_id}`")]
    NotRegistered { agent_id: String, goal_id: String },
    #[error("trigger_seq overflow for agent `{agent_id}` goal `{goal_id}`")]
    SequenceOverflow { agent_id: String, goal_id: String },
    #[error("timed out acquiring scheduler write lock `{lock_path}` after {wait_ms}ms")]
    LockTimeout { lock_path: String, wait_ms: u64 },
    #[error("scheduler storage operation `{operation}` failed: {details}")]
    Storage {
        operation: &'static str,
        details: String,
    },
}

#[derive(Debug)]
struct SchedulerWriteLockGuard {
    _file: std::fs::File,
    _lock_path: PathBuf,
}

impl AgentScheduler {
    pub fn with_storage(storage: AgentStorage) -> Self {
        assert!(
            storage.scope_segments().is_some(),
            "AgentScheduler requires scoped storage after the scoped scheduler hard cut"
        );
        Self {
            state: Arc::new(Mutex::new(SchedulerState::default())),
            storage,
        }
    }

    pub async fn snapshot(&self) -> SchedulerState {
        match self.load_current_state().await {
            Ok(state) => state,
            Err(err) => {
                warn!(
                    error = %err,
                    "AgentScheduler: failed to refresh state from storage; using in-memory snapshot"
                );
                self.state.lock().await.clone()
            },
        }
    }

    pub async fn recover_from_disk(&self) -> Result<(), SchedulerError> {
        let _ = self.load_current_state().await?;
        Ok(())
    }

    pub async fn entry_for_agent_goal(
        &self,
        agent_id: &str,
        goal_id: &str,
    ) -> Option<SchedulerEntryState> {
        self.snapshot()
            .await
            .entries
            .get(&self.state_key_for_agent_goal(agent_id, goal_id))
            .cloned()
    }

    pub async fn entry_for_task_id(&self, task_id: &str) -> Option<SchedulerEntryState> {
        self.snapshot()
            .await
            .entries
            .get(&state_key_for_task(task_id))
            .cloned()
    }

    pub async fn last_triggered_for(&self, agent_id: &str, goal_id: &str) -> Option<DateTime<Utc>> {
        self.entry_for_agent_goal(agent_id, goal_id)
            .await
            .and_then(|entry| entry.last_triggered)
    }

    pub async fn record_triggered_at(
        &self,
        agent_id: &str,
        goal_id: &str,
        fired_at: DateTime<Utc>,
    ) -> Result<bool, SchedulerError> {
        let key = self.state_key_for_agent_goal(agent_id, goal_id);
        self.mutate_state(|state| {
            let Some(entry) = state.entries.get_mut(&key) else {
                return Ok(false);
            };
            entry.last_triggered = Some(fired_at);
            state.last_tick = Some(fired_at);
            Ok(true)
        })
        .await
    }

    pub async fn rebuild_from_definitions(
        &self,
        definitions: &[AgentDefinition],
    ) -> Result<(usize, usize), SchedulerError> {
        let now = Utc::now();
        self.mutate_state(|state| {
            // In the unified architecture, triggers/schedules live on Tasks, not AgentDefinitions.
            // This method now preserves existing scheduler entries for known agents and prunes
            // entries for agents that are no longer registered. New schedule entries are created
            // via scoped bootstrap/task scheduling APIs.
            let mut hydrated_agent_ids = HashSet::new();
            for definition in definitions {
                hydrated_agent_ids.insert(definition.agent_id.clone());
            }

            let stale_agent_count = state
                .entries
                .values()
                .filter_map(|entry| {
                    if hydrated_agent_ids.contains(&entry.agent_id) {
                        None
                    } else {
                        Some(entry.agent_id.clone())
                    }
                })
                .collect::<HashSet<_>>()
                .len();

            // Prune entries for agents no longer in the definitions list.
            state
                .entries
                .retain(|_, entry| hydrated_agent_ids.contains(&entry.agent_id));
            state.last_tick = Some(now);
            Ok((hydrated_agent_ids.len(), stale_agent_count))
        })
        .await
    }

    /// Register a schedule entry directly (without reading from AgentDefinition triggers).
    /// In the unified architecture, schedule entries are created from Task schedules
    /// or via direct registration (for system agents like meta-agent).
    pub async fn register_entry(
        &self,
        agent_id: &str,
        goal_id: &str,
        registration: SchedulerTriggerRegistration,
    ) -> Result<(), SchedulerError> {
        let now = Utc::now();
        let key = self.state_key_for_agent_goal(agent_id, goal_id);
        let scoped_task_id = self.scoped_automation_task_id(agent_id, goal_id);
        self.mutate_state(|state| {
            let existing = state.entries.get(&key).cloned();
            let task_id = existing
                .as_ref()
                .and_then(|entry| entry.task_id.clone())
                .or_else(|| Some(scoped_task_id.clone()));
            state.entries.insert(
                key.clone(),
                SchedulerEntryState {
                    agent_id: agent_id.to_string(),
                    goal_id: goal_id.to_string(),
                    task_id,
                    trigger_seq: existing.as_ref().map_or(0, |e| e.trigger_seq),
                    last_triggered: existing.as_ref().and_then(|e| e.last_triggered),
                    registration,
                },
            );
            state.last_tick = Some(now);
            Ok(())
        })
        .await
    }

    pub async fn unregister_agent(&self, agent_id: &str) -> Result<(), SchedulerError> {
        self.mutate_state(|state| {
            state.entries.retain(|_, entry| entry.agent_id != agent_id);
            state.last_tick = Some(Utc::now());
            Ok(())
        })
        .await
    }

    pub async fn unregister_agent_goal(
        &self,
        agent_id: &str,
        goal_id: &str,
    ) -> Result<(), SchedulerError> {
        let key = self.state_key_for_agent_goal(agent_id, goal_id);
        self.mutate_state(|state| {
            state.entries.remove(&key);
            state.last_tick = Some(Utc::now());
            Ok(())
        })
        .await
    }

    pub async fn allocate_trigger_seq(
        &self,
        agent_id: &str,
        goal_id: &str,
    ) -> Result<u64, SchedulerError> {
        let now = Utc::now();
        let key = self.state_key_for_agent_goal(agent_id, goal_id);
        self.mutate_state(|state| {
            let seq = {
                let Some(entry) = state.entries.get_mut(&key) else {
                    return Err(SchedulerError::NotRegistered {
                        agent_id: agent_id.to_string(),
                        goal_id: goal_id.to_string(),
                    });
                };
                entry.trigger_seq = entry.trigger_seq.checked_add(1).ok_or_else(|| {
                    SchedulerError::SequenceOverflow {
                        agent_id: agent_id.to_string(),
                        goal_id: goal_id.to_string(),
                    }
                })?;
                entry.last_triggered = Some(now);
                entry.trigger_seq
            };
            state.last_tick = Some(now);
            Ok(seq)
        })
        .await
    }

    pub async fn collect_due_cron_triggers(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<ScheduledTrigger>, SchedulerError> {
        let (principal, workspace) = self
            .storage
            .scope_segments()
            .expect("AgentScheduler storage must remain scoped");
        self.mutate_state(|state| {
            let rollback = state.clone();
            let mut due = Vec::new();

            for entry in state.entries.values_mut() {
                let SchedulerTriggerRegistration::Cron {
                    schedule,
                    timezone,
                    next_run_at,
                } = &mut entry.registration
                else {
                    continue;
                };
                if compute_next_run_at(schedule, timezone, entry.last_triggered.unwrap_or(now))
                    .is_none()
                {
                    *next_run_at = None;
                    continue;
                }
                let Some(due_at) = *next_run_at else {
                    continue;
                };
                if due_at > now {
                    continue;
                }
                let Some(next_seq) = entry.trigger_seq.checked_add(1) else {
                    let err = SchedulerError::SequenceOverflow {
                        agent_id: entry.agent_id.clone(),
                        goal_id: entry.goal_id.clone(),
                    };
                    *state = rollback;
                    return Err(err);
                };
                entry.trigger_seq = next_seq;
                entry.last_triggered = Some(now);
                *next_run_at = compute_next_run_at(schedule, timezone, now);
                due.push(ScheduledTrigger {
                    principal: principal.clone(),
                    workspace: workspace.clone(),
                    agent_id: entry.agent_id.clone(),
                    goal_id: entry.goal_id.clone(),
                    trigger_seq: entry.trigger_seq,
                });
            }
            due.sort_by(|lhs, rhs| {
                lhs.principal
                    .cmp(&rhs.principal)
                    .then_with(|| lhs.workspace.cmp(&rhs.workspace))
                    .then_with(|| lhs.agent_id.cmp(&rhs.agent_id))
                    .then_with(|| lhs.goal_id.cmp(&rhs.goal_id))
                    .then_with(|| lhs.trigger_seq.cmp(&rhs.trigger_seq))
            });
            if !due.is_empty() {
                state.last_tick = Some(now);
            }
            Ok(due)
        })
        .await
    }

    pub async fn requeue_due_cron_trigger(
        &self,
        scheduled: &ScheduledTrigger,
        retry_at: DateTime<Utc>,
    ) -> Result<bool, SchedulerError> {
        let key = self.state_key_for_agent_goal(&scheduled.agent_id, &scheduled.goal_id);
        self.mutate_state(|state| {
            let Some(entry) = state.entries.get_mut(&key) else {
                return Ok(false);
            };
            let SchedulerTriggerRegistration::Cron { next_run_at, .. } = &mut entry.registration
            else {
                return Ok(false);
            };

            // Idempotency + race guard:
            // - only rollback if this exact scheduled tuple is still the latest consumed tuple.
            // - repeated rollback requests for the same tuple become no-ops.
            if entry.trigger_seq != scheduled.trigger_seq {
                return Ok(false);
            }
            let Some(rolled_back_seq) = entry.trigger_seq.checked_sub(1) else {
                return Ok(false);
            };
            entry.trigger_seq = rolled_back_seq;
            if next_run_at.is_none_or(|next| next > retry_at) {
                *next_run_at = Some(retry_at);
            }
            state.last_tick = Some(retry_at);
            Ok(true)
        })
        .await
    }

    async fn mutate_state<T, F>(&self, mutate: F) -> Result<T, SchedulerError>
    where
        F: FnOnce(&mut SchedulerState) -> Result<T, SchedulerError>,
    {
        self.storage
            .ensure_base_layout()
            .await
            .map_err(|err| Self::storage_error("ensure_layout", err))?;
        let _lock = Self::acquire_scheduler_write_lock(&self.storage).await?;
        let (mut state, _) = Self::load_state_from_storage(&self.storage).await?;
        let baseline = state.clone();
        let result = mutate(&mut state)?;
        if state != baseline {
            Self::persist_state_to_storage(&self.storage, &state).await?;
        }
        *self.state.lock().await = state;
        Ok(result)
    }

    fn state_key_for_agent_goal(&self, agent_id: &str, goal_id: &str) -> String {
        state_key_for_task(&self.scoped_automation_task_id(agent_id, goal_id))
    }

    fn scoped_automation_task_id(&self, agent_id: &str, goal_id: &str) -> String {
        let (principal, workspace) = self
            .storage
            .scope_segments()
            .expect("AgentScheduler storage must remain scoped");
        scoped_automation_task_id(&principal, &workspace, agent_id, goal_id)
    }

    async fn load_state_from_storage(
        storage: &AgentStorage,
    ) -> Result<(SchedulerState, bool), SchedulerError> {
        let path = storage.scheduler_state_path();
        let raw = match storage.read_to_string(&path).await {
            Ok(raw) => raw,
            Err(AgentStorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((SchedulerState::default(), false));
            },
            Err(error) => return Err(Self::storage_error("read", error)),
        };
        let (decoded, discarded_invalid_state) = Self::decode_scheduler_state(&raw);
        let (normalized, repaired) = Self::normalize_state_keys_for_storage(storage, decoded);
        if discarded_invalid_state {
            warn!(
                path = %path.display(),
                "Discarding invalid scheduler state file; falling back to default state"
            );
        }
        Ok((normalized, discarded_invalid_state || repaired))
    }

    async fn persist_state_to_storage(
        storage: &AgentStorage,
        state: &SchedulerState,
    ) -> Result<(), SchedulerError> {
        storage
            .write_json_atomic(storage.scheduler_state_path(), state)
            .await
            .map_err(|err| Self::storage_error("persist", err))
    }

    async fn acquire_scheduler_write_lock(
        storage: &AgentStorage,
    ) -> Result<SchedulerWriteLockGuard, SchedulerError> {
        let lock_path = storage.scheduler_write_lock_path();
        let lock_path_for_open = lock_path.clone();
        let mut file = task::spawn_blocking(move || {
            std::fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .open(&lock_path_for_open)
        })
        .await
        .map_err(|join_err| {
            Self::storage_error(
                "lock_open_join",
                AgentStorageError::from(std::io::Error::other(format!(
                    "failed to join scheduler lock-open task: {join_err}"
                ))),
            )
        })?
        .map_err(|err| Self::storage_error("lock_open", AgentStorageError::from(err)))?;

        let started = Instant::now();
        let mut retry_delay = SCHEDULER_WRITE_LOCK_RETRY_DELAY_MIN;

        loop {
            let (next_file, lock_result) = task::spawn_blocking(move || {
                let lock_result = file.try_lock_exclusive();
                (file, lock_result)
            })
            .await
            .map_err(|join_err| {
                Self::storage_error(
                    "lock_attempt_join",
                    AgentStorageError::from(std::io::Error::other(format!(
                        "failed to join scheduler lock-attempt task: {join_err}"
                    ))),
                )
            })?;

            file = next_file;
            match lock_result {
                Ok(()) => {
                    return Ok(SchedulerWriteLockGuard {
                        _file: file,
                        _lock_path: lock_path,
                    });
                },
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if started.elapsed() >= SCHEDULER_WRITE_LOCK_MAX_WAIT {
                        return Err(SchedulerError::LockTimeout {
                            lock_path: lock_path.display().to_string(),
                            wait_ms: u64::try_from(SCHEDULER_WRITE_LOCK_MAX_WAIT.as_millis())
                                .unwrap_or(u64::MAX),
                        });
                    }
                    tokio::time::sleep(retry_delay).await;
                    retry_delay = retry_delay
                        .saturating_mul(2)
                        .min(SCHEDULER_WRITE_LOCK_RETRY_DELAY_MAX);
                },
                Err(err) => {
                    return Err(Self::storage_error(
                        "lock_attempt",
                        AgentStorageError::from(err),
                    ));
                },
            }
        }
    }

    fn decode_scheduler_state(raw: &str) -> (SchedulerState, bool) {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return (SchedulerState::default(), false);
        }
        if let Ok(state) = serde_json::from_str::<SchedulerState>(trimmed) {
            return (state, false);
        }
        if let Ok(raw_json) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some((state, discarded_invalid_entries)) =
                Self::decode_scheduler_state_with_partial_recovery(&raw_json)
            {
                return (state, discarded_invalid_entries);
            }
        }
        (SchedulerState::default(), true)
    }

    fn decode_scheduler_state_with_partial_recovery(
        raw_json: &serde_json::Value,
    ) -> Option<(SchedulerState, bool)> {
        let raw_state = raw_json.as_object()?;
        let raw_entries = raw_state.get("entries")?.as_object()?;

        let mut discarded_invalid_entries = false;
        let mut entries = HashMap::with_capacity(raw_entries.len());
        for (key, raw_entry) in raw_entries {
            if let Ok(entry) = serde_json::from_value::<SchedulerEntryState>(raw_entry.clone()) {
                entries.insert(key.clone(), entry);
                continue;
            }
            discarded_invalid_entries = true;
        }

        let last_tick = match raw_state.get("last_tick") {
            Some(value) => match serde_json::from_value::<Option<DateTime<Utc>>>(value.clone()) {
                Ok(parsed) => parsed,
                Err(_) => {
                    discarded_invalid_entries = true;
                    None
                },
            },
            None => None,
        };

        Some((
            SchedulerState { entries, last_tick },
            discarded_invalid_entries,
        ))
    }

    fn normalize_state_keys_for_storage(
        storage: &AgentStorage,
        state: SchedulerState,
    ) -> (SchedulerState, bool) {
        let Some((principal, workspace)) = storage.scope_segments() else {
            panic!(
                "scheduler storage must have scoped segments after the scoped scheduler cutover"
            );
        };
        let mut entries = HashMap::with_capacity(state.entries.len());
        let mut repaired = false;
        for mut entry in state.entries.into_values() {
            let scoped_automation_task_id =
                scoped_automation_task_id(&principal, &workspace, &entry.agent_id, &entry.goal_id);
            let task_id = entry.task_id.clone().unwrap_or(scoped_automation_task_id);
            if entry.task_id.as_deref() != Some(task_id.as_str()) {
                repaired = true;
            }
            entry.task_id = Some(task_id.clone());
            if let SchedulerTriggerRegistration::Cron {
                schedule,
                timezone,
                next_run_at,
            } = &mut entry.registration
            {
                let after = entry.last_triggered.unwrap_or_else(Utc::now);
                if compute_next_run_at(schedule, timezone, after).is_none() && next_run_at.is_some()
                {
                    *next_run_at = None;
                    repaired = true;
                }
            }
            entries.insert(state_key_for_task(&task_id), entry);
        }
        (
            SchedulerState {
                entries,
                last_tick: state.last_tick,
            },
            repaired,
        )
    }

    async fn load_current_state(&self) -> Result<SchedulerState, SchedulerError> {
        self.storage
            .ensure_base_layout()
            .await
            .map_err(|err| Self::storage_error("ensure_layout", err))?;
        let _lock = Self::acquire_scheduler_write_lock(&self.storage).await?;
        let (recovered, repaired) = Self::load_state_from_storage(&self.storage).await?;
        if repaired {
            Self::persist_state_to_storage(&self.storage, &recovered).await?;
        }
        *self.state.lock().await = recovered.clone();
        Ok(recovered)
    }

    fn storage_error(operation: &'static str, err: AgentStorageError) -> SchedulerError {
        SchedulerError::Storage {
            operation,
            details: err.to_string(),
        }
    }
}

/// Compute a state key from a scoped task_id.
fn state_key_for_task(task_id: &str) -> String {
    format!("task:{}", hex_component(task_id))
}

fn hex_component(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() * 2);
    for byte in raw.as_bytes() {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

#[cfg(any(test, feature = "test-fixtures"))]
fn normalize_timezone(raw: Option<&str>) -> String {
    let candidate = raw.map(str::trim).unwrap_or("UTC");
    if candidate.is_empty() {
        return "UTC".to_string();
    }
    if candidate.eq_ignore_ascii_case("UTC") {
        return "UTC".to_string();
    }
    if let Some(offset_seconds) = parse_fixed_offset_seconds(candidate) {
        return format_fixed_offset_seconds(offset_seconds);
    }
    if let Ok(tz) = candidate.parse::<Tz>() {
        return tz.name().to_string();
    }
    "UTC".to_string()
}

fn parse_fixed_offset_seconds(raw: &str) -> Option<i32> {
    if raw.len() < 3 {
        return None;
    }
    let (sign, rest) = match raw.as_bytes()[0] {
        b'+' => (1_i32, &raw[1..]),
        b'-' => (-1_i32, &raw[1..]),
        _ => return None,
    };
    let (hours_str, minutes_str) = if let Some((h, m)) = rest.split_once(':') {
        (h, m)
    } else if rest.len() == 4 {
        (&rest[0..2], &rest[2..4])
    } else {
        (rest, "0")
    };
    let hours: i32 = hours_str.parse().ok()?;
    let minutes: i32 = minutes_str.parse().ok()?;
    if !(0..=23).contains(&hours) || !(0..=59).contains(&minutes) {
        return None;
    }
    Some(sign * (hours * 3600 + minutes * 60))
}

#[cfg(any(test, feature = "test-fixtures"))]
fn format_fixed_offset_seconds(offset_seconds: i32) -> String {
    let sign = if offset_seconds < 0 { '-' } else { '+' };
    let abs = offset_seconds.abs();
    let hours = abs / 3600;
    let minutes = (abs % 3600) / 60;
    format!("{sign}{hours:02}:{minutes:02}")
}

#[derive(Clone, Copy)]
enum ResolvedTimezone {
    Utc,
    FixedOffset(i32),
    Iana(Tz),
}

fn resolve_timezone(timezone: &str) -> ResolvedTimezone {
    if timezone.eq_ignore_ascii_case("UTC") {
        return ResolvedTimezone::Utc;
    }
    if let Some(offset_seconds) = parse_fixed_offset_seconds(timezone) {
        return ResolvedTimezone::FixedOffset(offset_seconds);
    }
    if let Ok(tz) = timezone.parse::<Tz>() {
        return ResolvedTimezone::Iana(tz);
    }
    ResolvedTimezone::Utc
}

/// Compute the next fire time for a 5-field cron expression, evaluated in the
/// given timezone, returning a UTC `DateTime`.
///
/// Uses the `cron` crate (via [`normalize_cron`]) for parsing and iteration.
/// Returns `None` if the expression is invalid or no future occurrence exists
/// (for example an impossible schedule like Feb 31, or near `DateTime::MAX`).
pub fn compute_next_run_at(
    schedule: &str,
    timezone: &str,
    after_utc: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    use crate::magician_v2::storage::task_scheduler::{normalize_cron, posix_or_cron_next};
    use std::str::FromStr;

    let normalized = normalize_cron(schedule);
    // Fast-reject: if the normalized expression is syntactically invalid, bail.
    if CronSchedule::from_str(&normalized).is_err() {
        return None;
    }

    // The cron crate can panic on DateTime overflow (e.g. near DateTime::MAX).
    // Use catch_unwind to safely fall back.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let resolved = resolve_timezone(timezone);
        match resolved {
            ResolvedTimezone::Utc => posix_or_cron_next(&normalized, &after_utc),
            ResolvedTimezone::FixedOffset(offset_seconds) => {
                let Some(offset) = FixedOffset::east_opt(offset_seconds) else {
                    return posix_or_cron_next(&normalized, &after_utc);
                };
                let local = after_utc.with_timezone(&offset);
                posix_or_cron_next(&normalized, &local)
            },
            ResolvedTimezone::Iana(tz) => {
                let local = after_utc.with_timezone(&tz);
                posix_or_cron_next(&normalized, &local)
            },
        }
    }));

    match result {
        Ok(Some(dt)) => Some(dt),
        _ => None,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::{collections::HashSet, sync::Arc};

    use tempfile::tempdir;

    use chrono::{Datelike, Duration, Timelike};

    use super::*;
    use crate::magician_v2::agents::types::AgentDefinition;
    use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

    const TEST_PRINCIPAL: &str = "principal-a";
    const TEST_WORKSPACE: &str = "workspace-a";

    // ── Minimal AgentDefinition helper (no goals/triggers) ──────────────

    fn simple_definition(agent_id: &str) -> AgentDefinition {
        let yaml = format!(
            r#"
agent_id: "{agent_id}"
name: "Scheduler Test"
persona: "Test"
tools: []
"#
        );
        AgentDefinition::from_yaml_str(&yaml).unwrap()
    }

    // ── Registration helpers — register entries directly ─────────────────

    async fn register_cron(
        scheduler: &AgentScheduler,
        agent_id: &str,
        goal_id: &str,
        schedule: &str,
        timezone: &str,
    ) {
        let tz = normalize_timezone(Some(timezone));
        let next = compute_next_run_at(schedule, &tz, Utc::now());
        scheduler
            .register_entry(
                agent_id,
                goal_id,
                SchedulerTriggerRegistration::Cron {
                    schedule: schedule.to_string(),
                    timezone: tz,
                    next_run_at: next,
                },
            )
            .await
            .unwrap();
    }

    async fn register_event(
        scheduler: &AgentScheduler,
        agent_id: &str,
        goal_id: &str,
        pattern: &str,
        filter: HashMap<String, String>,
    ) {
        scheduler
            .register_entry(
                agent_id,
                goal_id,
                SchedulerTriggerRegistration::Event {
                    pattern: pattern.to_string(),
                    filter,
                },
            )
            .await
            .unwrap();
    }

    async fn register_idle(scheduler: &AgentScheduler, agent_id: &str, goal_id: &str) {
        scheduler
            .register_entry(agent_id, goal_id, SchedulerTriggerRegistration::Idle)
            .await
            .unwrap();
    }

    fn scoped_storage(tmp: &tempfile::TempDir) -> AgentStorage {
        let workspace = ArtifactV2Workspace::new(tmp.path().join("magician_data_v3"));
        AgentStorage::new(workspace.scoped_agent_runtime_root(TEST_PRINCIPAL, TEST_WORKSPACE))
    }

    fn scoped_scheduler() -> (tempfile::TempDir, AgentScheduler) {
        let tmp = tempdir().unwrap();
        let storage = scoped_storage(&tmp);
        (tmp, AgentScheduler::with_storage(storage))
    }

    async fn overwrite_scheduler_state<F>(scheduler: &AgentScheduler, mutate: F)
    where
        F: FnOnce(&mut SchedulerState),
    {
        let mut state = scheduler.snapshot().await;
        mutate(&mut state);
        AgentScheduler::persist_state_to_storage(&scheduler.storage, &state)
            .await
            .unwrap();
        *scheduler.state.lock().await = state;
    }

    #[tokio::test]
    async fn allocate_trigger_seq_is_monotonic_per_goal() {
        let (_tmp, scheduler) = scoped_scheduler();
        register_cron(&scheduler, "a1", "g1", "0 9 * * *", "UTC").await;
        register_cron(&scheduler, "a1", "g2", "0 9 * * *", "UTC").await;

        assert_eq!(scheduler.allocate_trigger_seq("a1", "g1").await.unwrap(), 1);
        assert_eq!(scheduler.allocate_trigger_seq("a1", "g1").await.unwrap(), 2);
        assert_eq!(scheduler.allocate_trigger_seq("a1", "g2").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn allocate_trigger_seq_uniqueness_under_concurrency() {
        let (_tmp, scheduler) = scoped_scheduler();
        let scheduler = Arc::new(scheduler);
        register_cron(&scheduler, "a1", "g1", "0 9 * * *", "UTC").await;
        const N: usize = 32;
        let mut handles = Vec::new();
        for _ in 0..N {
            let scheduler = Arc::clone(&scheduler);
            handles.push(tokio::spawn(async move {
                scheduler.allocate_trigger_seq("a1", "g1").await.unwrap()
            }));
        }
        let mut seqs = Vec::new();
        for handle in handles {
            seqs.push(handle.await.unwrap());
        }
        let unique = seqs.iter().copied().collect::<HashSet<_>>();
        assert_eq!(unique.len(), N);
        let mut ordered = seqs;
        ordered.sort_unstable();
        assert_eq!(ordered, (1_u64..=N as u64).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn allocate_trigger_seq_keeps_colon_ids_distinct() {
        let (_tmp, scheduler) = scoped_scheduler();
        register_cron(&scheduler, "a:b", "c", "0 9 * * *", "UTC").await;
        register_cron(&scheduler, "a", "b:c", "0 9 * * *", "UTC").await;

        assert_eq!(scheduler.allocate_trigger_seq("a:b", "c").await.unwrap(), 1);
        assert_eq!(scheduler.allocate_trigger_seq("a", "b:c").await.unwrap(), 1);
        assert_eq!(scheduler.allocate_trigger_seq("a:b", "c").await.unwrap(), 2);
        assert_eq!(scheduler.allocate_trigger_seq("a", "b:c").await.unwrap(), 2);

        let state = scheduler.snapshot().await;
        assert_eq!(state.entries.len(), 2);
    }

    #[tokio::test]
    async fn allocate_trigger_seq_requires_registered_agent_goal() {
        let (_tmp, scheduler) = scoped_scheduler();
        let err = scheduler
            .allocate_trigger_seq("missing-agent", "missing-goal")
            .await
            .unwrap_err();
        assert_eq!(
            err,
            SchedulerError::NotRegistered {
                agent_id: "missing-agent".to_string(),
                goal_id: "missing-goal".to_string()
            }
        );
    }

    #[tokio::test]
    async fn register_and_unregister_agent_updates_entries() {
        let (_tmp, scheduler) = scoped_scheduler();
        register_cron(&scheduler, "agent-a", "goal-1", "0 9 * * *", "UTC").await;
        let state = scheduler.snapshot().await;
        assert_eq!(state.entries.len(), 1);

        scheduler.unregister_agent("agent-a").await.unwrap();
        let state = scheduler.snapshot().await;
        assert!(state.entries.is_empty());
    }

    #[tokio::test]
    async fn unregister_and_reregister_replaces_previous_entries() {
        let (_tmp, scheduler) = scoped_scheduler();
        register_cron(&scheduler, "agent-a", "goal-1", "0 9 * * *", "UTC").await;
        assert_eq!(
            scheduler
                .allocate_trigger_seq("agent-a", "goal-1")
                .await
                .unwrap(),
            1
        );

        // Unregister the agent (removing all its entries) then register a new goal.
        scheduler.unregister_agent("agent-a").await.unwrap();
        register_cron(&scheduler, "agent-a", "goal-2", "0 9 * * *", "UTC").await;

        let stale = scheduler
            .allocate_trigger_seq("agent-a", "goal-1")
            .await
            .unwrap_err();
        assert_eq!(
            stale,
            SchedulerError::NotRegistered {
                agent_id: "agent-a".to_string(),
                goal_id: "goal-1".to_string()
            }
        );
        assert_eq!(
            scheduler
                .allocate_trigger_seq("agent-a", "goal-2")
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn register_entry_preserves_trigger_seq_for_retained_goals() {
        let (_tmp, scheduler) = scoped_scheduler();
        register_cron(&scheduler, "agent-a", "g1", "0 9 * * *", "UTC").await;
        register_cron(&scheduler, "agent-a", "g2", "0 9 * * *", "UTC").await;

        // Advance g1 to seq 3
        scheduler
            .allocate_trigger_seq("agent-a", "g1")
            .await
            .unwrap();
        scheduler
            .allocate_trigger_seq("agent-a", "g1")
            .await
            .unwrap();
        scheduler
            .allocate_trigger_seq("agent-a", "g1")
            .await
            .unwrap();

        // Re-register the same entry — trigger_seq must be preserved.
        register_cron(&scheduler, "agent-a", "g1", "0 9 * * *", "UTC").await;

        // g1 should continue from 3, not reset to 0.
        assert_eq!(
            scheduler
                .allocate_trigger_seq("agent-a", "g1")
                .await
                .unwrap(),
            4
        );
        // g2 was never incremented, stays at 0 -> next is 1.
        assert_eq!(
            scheduler
                .allocate_trigger_seq("agent-a", "g2")
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn registers_event_trigger_metadata() {
        let (_tmp, scheduler) = scoped_scheduler();
        let mut filter = HashMap::new();
        filter.insert("severity".to_string(), "high".to_string());
        register_event(&scheduler, "agent-a", "g1", "ticket.created", filter).await;

        let entry = scheduler
            .entry_for_agent_goal("agent-a", "g1")
            .await
            .expect("entry should exist");
        match &entry.registration {
            SchedulerTriggerRegistration::Event { pattern, filter } => {
                assert_eq!(pattern, "ticket.created");
                assert_eq!(filter.get("severity").map(String::as_str), Some("high"));
            },
            SchedulerTriggerRegistration::Cron { .. } => panic!("expected event trigger"),
            SchedulerTriggerRegistration::Idle => panic!("expected event trigger"),
        }
    }

    #[tokio::test]
    async fn registers_idle_trigger_metadata() {
        let (_tmp, scheduler) = scoped_scheduler();
        register_idle(&scheduler, "agent-a", "g1").await;

        let entry = scheduler
            .entry_for_agent_goal("agent-a", "g1")
            .await
            .expect("entry should exist");
        assert_eq!(
            entry.task_id,
            Some(scoped_automation_task_id(
                TEST_PRINCIPAL,
                TEST_WORKSPACE,
                "agent-a",
                "g1"
            ))
        );
        assert!(matches!(
            entry.registration,
            SchedulerTriggerRegistration::Idle
        ));
    }

    #[tokio::test]
    async fn allocate_trigger_seq_returns_overflow_error_at_max() {
        let (_tmp, scheduler) = scoped_scheduler();
        register_cron(&scheduler, "a1", "g1", "0 9 * * *", "UTC").await;

        overwrite_scheduler_state(&scheduler, |state| {
            state
                .entries
                .values_mut()
                .find(|entry| entry.agent_id == "a1" && entry.goal_id == "g1")
                .unwrap()
                .trigger_seq = u64::MAX;
        })
        .await;

        let err = scheduler
            .allocate_trigger_seq("a1", "g1")
            .await
            .unwrap_err();
        assert_eq!(
            err,
            SchedulerError::SequenceOverflow {
                agent_id: "a1".to_string(),
                goal_id: "g1".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn scheduler_persists_trigger_seq_across_restart() {
        let tmp = tempdir().unwrap();
        let storage = scoped_storage(&tmp);

        let scheduler = AgentScheduler::with_storage(storage.clone());
        register_cron(&scheduler, "agent-a", "g1", "0 9 * * *", "UTC").await;
        assert_eq!(
            scheduler
                .allocate_trigger_seq("agent-a", "g1")
                .await
                .unwrap(),
            1
        );

        let recovered = AgentScheduler::with_storage(storage.clone());
        recovered.recover_from_disk().await.unwrap();
        assert_eq!(
            recovered
                .allocate_trigger_seq("agent-a", "g1")
                .await
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn rebuild_from_definitions_preserves_sequences_and_prunes_stale_agents() {
        let tmp = tempdir().unwrap();
        let storage = scoped_storage(&tmp);
        let scheduler = AgentScheduler::with_storage(storage.clone());

        // Register entries for two agents.
        register_cron(&scheduler, "agent-a", "g1", "0 9 * * *", "UTC").await;
        register_cron(&scheduler, "agent-b", "g1", "0 9 * * *", "UTC").await;
        assert_eq!(
            scheduler
                .allocate_trigger_seq("agent-a", "g1")
                .await
                .unwrap(),
            1
        );

        // rebuild_from_definitions now only prunes stale agents (does not create entries).
        let definitions = vec![simple_definition("agent-a")];
        let (hydrated_count, stale_agent_count) = scheduler
            .rebuild_from_definitions(&definitions)
            .await
            .unwrap();
        assert_eq!(hydrated_count, 1);
        assert_eq!(stale_agent_count, 1);

        // agent-a/g1 entry preserved with its trigger_seq.
        assert_eq!(
            scheduler
                .allocate_trigger_seq("agent-a", "g1")
                .await
                .unwrap(),
            2
        );
        // agent-b was pruned.
        let stale_err = scheduler
            .allocate_trigger_seq("agent-b", "g1")
            .await
            .unwrap_err();
        assert_eq!(
            stale_err,
            SchedulerError::NotRegistered {
                agent_id: "agent-b".to_string(),
                goal_id: "g1".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn recover_from_disk_without_state_file_is_noop() {
        let (_tmp, scheduler) = scoped_scheduler();
        register_cron(&scheduler, "agent-a", "g1", "0 9 * * *", "UTC").await;
        scheduler.recover_from_disk().await.unwrap();
        assert_eq!(
            scheduler
                .allocate_trigger_seq("agent-a", "g1")
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn recover_from_disk_waits_for_scheduler_write_lock() {
        let tmp = tempdir().unwrap();
        let storage = scoped_storage(&tmp);
        storage.ensure_base_layout().await.unwrap();
        let lock_path = storage.scheduler_write_lock_path();
        let lock_path_for_open = lock_path.clone();
        let mut lock_file = task::spawn_blocking(move || {
            std::fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .open(lock_path_for_open)
        })
        .await
        .unwrap()
        .unwrap();
        let (held_file, lock_result) = task::spawn_blocking(move || {
            let result = lock_file.try_lock_exclusive();
            (lock_file, result)
        })
        .await
        .unwrap();
        lock_file = held_file;
        lock_result.unwrap();

        let scheduler = AgentScheduler::with_storage(storage);
        let scheduler_for_task = scheduler.clone();
        let recover_task =
            tokio::spawn(async move { scheduler_for_task.recover_from_disk().await });
        tokio::time::sleep(TokioDuration::from_millis(100)).await;
        assert!(
            !recover_task.is_finished(),
            "recover_from_disk should wait while the scheduler write lock is held"
        );

        drop(lock_file);
        recover_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn unregister_agent_persists_to_disk() {
        let tmp = tempdir().unwrap();
        let storage = scoped_storage(&tmp);
        let scheduler = AgentScheduler::with_storage(storage.clone());

        register_cron(&scheduler, "agent-a", "g1", "0 9 * * *", "UTC").await;
        scheduler.unregister_agent("agent-a").await.unwrap();

        let recovered = AgentScheduler::with_storage(storage);
        recovered.recover_from_disk().await.unwrap();
        let err = recovered
            .allocate_trigger_seq("agent-a", "g1")
            .await
            .unwrap_err();
        assert_eq!(
            err,
            SchedulerError::NotRegistered {
                agent_id: "agent-a".to_string(),
                goal_id: "g1".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn allocate_trigger_seq_is_monotonic_across_scheduler_instances() {
        let tmp = tempdir().unwrap();
        let storage = scoped_storage(&tmp);
        let scheduler_a = Arc::new(AgentScheduler::with_storage(storage.clone()));
        let scheduler_b = Arc::new(AgentScheduler::with_storage(storage.clone()));

        register_cron(&scheduler_a, "agent-a", "g1", "0 9 * * *", "UTC").await;

        // Use 32 concurrent tasks instead of 128 — enough to exercise cross-instance
        // monotonicity without starving tasks on the exclusive file lock (the retry
        // backoff caps at 250 ms and the timeout is 30 s, so 128 concurrent tasks
        // can cause late tasks to never acquire the lock in time).
        const N: usize = 32;
        let mut handles = Vec::new();
        for i in 0..N {
            let scheduler = if i % 2 == 0 {
                Arc::clone(&scheduler_a)
            } else {
                Arc::clone(&scheduler_b)
            };
            handles.push(tokio::spawn(async move {
                scheduler
                    .allocate_trigger_seq("agent-a", "g1")
                    .await
                    .unwrap()
            }));
        }

        let mut seqs = Vec::new();
        for handle in handles {
            seqs.push(handle.await.unwrap());
        }
        let unique = seqs.iter().copied().collect::<HashSet<_>>();
        assert_eq!(unique.len(), N);
        let mut ordered = seqs;
        ordered.sort_unstable();
        assert_eq!(ordered, (1..=N as u64).collect::<Vec<_>>());

        let verify = AgentScheduler::with_storage(storage);
        verify.recover_from_disk().await.unwrap();
        assert_eq!(
            verify.allocate_trigger_seq("agent-a", "g1").await.unwrap(),
            N as u64 + 1
        );
    }

    #[test]
    fn invalid_timezone_falls_back_to_utc() {
        // normalize_timezone falls back to UTC for unrecognized timezone strings.
        assert_eq!(normalize_timezone(Some("Mars/Phobos")), "UTC");
        assert_eq!(normalize_timezone(Some("Invalid/Zone")), "UTC");
        assert_eq!(normalize_timezone(Some("")), "UTC");
        assert_eq!(normalize_timezone(None), "UTC");
    }

    #[tokio::test]
    async fn iana_timezone_is_preserved() {
        let (_tmp, scheduler) = scoped_scheduler();
        register_cron(&scheduler, "agent-a", "g1", "0 9 * * *", "America/New_York").await;
        let entry = scheduler
            .entry_for_agent_goal("agent-a", "g1")
            .await
            .expect("entry should exist");
        match &entry.registration {
            SchedulerTriggerRegistration::Cron { timezone, .. } => {
                assert_eq!(timezone, "America/New_York")
            },
            SchedulerTriggerRegistration::Event { .. } => panic!("expected cron trigger"),
            SchedulerTriggerRegistration::Idle => panic!("expected cron trigger"),
        }
    }

    #[tokio::test]
    async fn equivalent_fixed_offset_timezones_are_canonicalized() {
        let (_tmp, scheduler) = scoped_scheduler();
        register_cron(&scheduler, "agent-a", "g1", "0 9 * * *", "+0530").await;
        let first_entry = scheduler
            .entry_for_agent_goal("agent-a", "g1")
            .await
            .expect("entry should exist");
        match &first_entry.registration {
            SchedulerTriggerRegistration::Cron { timezone, .. } => assert_eq!(timezone, "+05:30"),
            SchedulerTriggerRegistration::Event { .. } => panic!("expected cron trigger"),
            SchedulerTriggerRegistration::Idle => panic!("expected cron trigger"),
        }

        register_cron(&scheduler, "agent-a", "g1", "0 9 * * *", "+5:30").await;
        let second_entry = scheduler
            .entry_for_agent_goal("agent-a", "g1")
            .await
            .expect("entry should exist");
        assert_eq!(second_entry, first_entry);
    }

    #[test]
    fn cron_day_of_week_accepts_sunday_as_seven() {
        let after = DateTime::parse_from_rfc3339("2026-02-14T09:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let next = compute_next_run_at("0 9 * * 7", "UTC", after).expect("next Sunday run");
        assert_eq!(next.weekday().num_days_from_sunday(), 0);
        assert_eq!(next.hour(), 9);
        assert_eq!(next.minute(), 0);
    }

    #[test]
    fn leap_day_schedule_searches_beyond_one_year() {
        let after = DateTime::parse_from_rfc3339("2025-03-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let next = compute_next_run_at("0 0 29 2 *", "UTC", after).expect("next leap day run");
        assert_eq!(next.year(), 2028);
        assert_eq!(next.month(), 2);
        assert_eq!(next.day(), 29);
        assert_eq!(next.hour(), 0);
        assert_eq!(next.minute(), 0);
    }

    #[test]
    fn compute_next_run_at_near_datetime_max_does_not_panic() {
        let after = DateTime::<Utc>::MAX_UTC;
        let next = compute_next_run_at("0 9 * * *", "UTC", after);
        assert_eq!(next, None);
    }

    #[test]
    fn compute_next_run_at_near_datetime_max_with_fixed_offset_does_not_panic() {
        let after = DateTime::<Utc>::MAX_UTC;
        let next = compute_next_run_at("0 9 * * *", "+14:00", after);
        assert_eq!(next, None);
    }

    #[test]
    fn compute_next_run_at_accepts_standard_5_field_cron() {
        let after = DateTime::parse_from_rfc3339("2026-03-09T05:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        // "At 06:00 on weekdays" — standard 5-field cron
        let next = compute_next_run_at("0 6 * * 1-5", "UTC", after).expect("weekday schedule");
        assert_eq!(next.hour(), 6);
        assert_eq!(next.minute(), 0);
        assert!(next > after);
    }

    #[test]
    fn compute_next_run_at_returns_none_for_impossible_schedule() {
        let after = DateTime::parse_from_rfc3339("2026-04-14T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let next = compute_next_run_at("0 0 31 2 *", "UTC", after);
        assert_eq!(next, None);
    }

    #[tokio::test]
    async fn collect_due_cron_triggers_rolls_back_on_sequence_overflow() {
        let (_tmp, scheduler) = scoped_scheduler();
        register_cron(&scheduler, "agent-a", "g1", "0 9 * * *", "UTC").await;
        register_cron(&scheduler, "agent-a", "g2", "0 9 * * *", "UTC").await;
        let now = Utc::now();
        overwrite_scheduler_state(&scheduler, |state| {
            for entry in state.entries.values_mut() {
                entry.trigger_seq = if entry.goal_id == "g2" { u64::MAX } else { 7 };
                if let SchedulerTriggerRegistration::Cron { next_run_at, .. } =
                    &mut entry.registration
                {
                    *next_run_at = Some(now - Duration::minutes(1));
                }
            }
        })
        .await;
        let before = scheduler.snapshot().await;
        let err = scheduler.collect_due_cron_triggers(now).await.unwrap_err();
        assert!(matches!(err, SchedulerError::SequenceOverflow { .. }));
        let after = scheduler.snapshot().await;
        assert_eq!(before, after);
    }

    #[tokio::test]
    async fn collect_due_cron_triggers_returns_empty_when_nothing_is_due() {
        let (_tmp, scheduler) = scoped_scheduler();
        register_cron(&scheduler, "agent-a", "g1", "0 9 * * *", "UTC").await;
        let before = scheduler.snapshot().await;

        let due = scheduler
            .collect_due_cron_triggers(Utc::now())
            .await
            .unwrap();
        assert!(due.is_empty());
        let after = scheduler.snapshot().await;
        assert_eq!(before, after);
    }

    #[tokio::test]
    async fn collect_due_cron_triggers_does_not_persist_when_nothing_is_due() {
        let tmp = tempdir().unwrap();
        let storage = scoped_storage(&tmp);
        let scheduler = AgentScheduler::with_storage(storage.clone());
        register_cron(&scheduler, "agent-a", "g1", "0 9 * * *", "UTC").await;

        let path = storage.scheduler_state_path();
        let before_bytes = tokio::fs::read(&path).await.unwrap();
        let due = scheduler
            .collect_due_cron_triggers(Utc::now())
            .await
            .unwrap();
        assert!(due.is_empty());
        let after_bytes = tokio::fs::read(path).await.unwrap();
        assert_eq!(before_bytes, after_bytes);
    }

    #[tokio::test]
    async fn requeue_due_cron_trigger_is_idempotent_and_restores_sequence() {
        let (_tmp, scheduler) = scoped_scheduler();
        register_cron(&scheduler, "agent-a", "g1", "0 9 * * *", "UTC").await;

        let due = scheduler
            .collect_due_cron_triggers(Utc::now() + Duration::days(2))
            .await
            .unwrap();
        let scheduled = due
            .into_iter()
            .next()
            .expect("one trigger should be due for rollback test");
        assert_eq!(scheduled.trigger_seq, 1);

        let rolled_back = scheduler
            .requeue_due_cron_trigger(&scheduled, Utc::now())
            .await
            .unwrap();
        assert!(rolled_back);
        let rolled_back_again = scheduler
            .requeue_due_cron_trigger(&scheduled, Utc::now())
            .await
            .unwrap();
        assert!(!rolled_back_again);

        let retried = scheduler
            .collect_due_cron_triggers(Utc::now() + Duration::days(2))
            .await
            .unwrap();
        assert_eq!(retried.len(), 1);
        assert_eq!(retried[0].trigger_seq, 1);
    }

    #[tokio::test]
    async fn collect_due_cron_triggers_skips_event_entries() {
        let (_tmp, scheduler) = scoped_scheduler();
        register_event(
            &scheduler,
            "agent-a",
            "g1",
            "ticket.created",
            HashMap::new(),
        )
        .await;

        let due = scheduler
            .collect_due_cron_triggers(Utc::now())
            .await
            .unwrap();
        assert!(due.is_empty());
        let entry = scheduler
            .entry_for_agent_goal("agent-a", "g1")
            .await
            .expect("entry should exist");
        assert_eq!(entry.trigger_seq, 0);
    }

    #[tokio::test]
    async fn collect_due_cron_triggers_skips_idle_entries() {
        let (_tmp, scheduler) = scoped_scheduler();
        register_idle(&scheduler, "agent-a", "g1").await;

        let due = scheduler
            .collect_due_cron_triggers(Utc::now())
            .await
            .unwrap();
        assert!(due.is_empty());
        let entry = scheduler
            .entry_for_agent_goal("agent-a", "g1")
            .await
            .expect("entry should exist");
        assert_eq!(entry.trigger_seq, 0);
    }

    #[tokio::test]
    async fn collect_due_cron_triggers_returns_stable_sorted_order() {
        let (_tmp, scheduler) = scoped_scheduler();
        register_cron(&scheduler, "agent-a", "g2", "0 9 * * *", "UTC").await;
        register_cron(&scheduler, "agent-a", "g1", "0 9 * * *", "UTC").await;
        register_cron(&scheduler, "agent-a", "g3", "0 9 * * *", "UTC").await;
        let now = Utc::now();
        overwrite_scheduler_state(&scheduler, |state| {
            for entry in state.entries.values_mut() {
                if let SchedulerTriggerRegistration::Cron { next_run_at, .. } =
                    &mut entry.registration
                {
                    *next_run_at = Some(now - Duration::minutes(1));
                }
            }
        })
        .await;

        let due = scheduler.collect_due_cron_triggers(now).await.unwrap();
        let goals: Vec<_> = due.into_iter().map(|trigger| trigger.goal_id).collect();
        assert_eq!(goals, vec!["g1", "g2", "g3"]);
    }

    #[tokio::test]
    async fn record_triggered_at_persists_last_triggered() {
        let tmp = tempdir().unwrap();
        let storage = scoped_storage(&tmp);
        let scheduler = AgentScheduler::with_storage(storage.clone());
        register_idle(&scheduler, "agent-a", "g1").await;

        let fired_at = Utc::now() - Duration::minutes(2);
        assert!(scheduler
            .record_triggered_at("agent-a", "g1", fired_at)
            .await
            .unwrap());
        assert_eq!(
            scheduler.last_triggered_for("agent-a", "g1").await,
            Some(fired_at)
        );

        let recovered = AgentScheduler::with_storage(storage);
        assert_eq!(
            recovered.last_triggered_for("agent-a", "g1").await,
            Some(fired_at)
        );
    }

    #[tokio::test]
    async fn recover_from_corrupted_state_file_falls_back_to_default() {
        let tmp = tempdir().unwrap();
        let storage = scoped_storage(&tmp);
        storage.ensure_base_layout().await.unwrap();
        tokio::fs::write(storage.scheduler_state_path(), "{ invalid json")
            .await
            .unwrap();

        let scheduler = AgentScheduler::with_storage(storage);
        scheduler.recover_from_disk().await.unwrap();
        assert!(scheduler.snapshot().await.entries.is_empty());
    }

    #[tokio::test]
    async fn recover_from_disk_preserves_known_entries_and_discards_unknown_registration_variants()
    {
        let tmp = tempdir().unwrap();
        let storage = scoped_storage(&tmp);
        storage.ensure_base_layout().await.unwrap();

        let known_key = state_key_for_task(&scoped_automation_task_id(
            TEST_PRINCIPAL,
            TEST_WORKSPACE,
            "agent-a",
            "g1",
        ));
        let unknown_key = state_key_for_task(&scoped_automation_task_id(
            TEST_PRINCIPAL,
            TEST_WORKSPACE,
            "agent-b",
            "g2",
        ));
        let mut entries = serde_json::Map::new();
        entries.insert(
            known_key.clone(),
            serde_json::json!({
                "agent_id": "agent-a",
                "goal_id": "g1",
                "trigger_seq": 3,
                "last_triggered": null,
                "registration": {
                    "trigger_kind": "cron",
                    "schedule": "0 9 * * *",
                    "timezone": "UTC",
                    "next_run_at": Utc::now(),
                },
            }),
        );
        entries.insert(
            unknown_key.clone(),
            serde_json::json!({
                "agent_id": "agent-b",
                "goal_id": "g2",
                "trigger_seq": 7,
                "last_triggered": null,
                "registration": {
                    "trigger_kind": "batch_window",
                    "every_minutes": 5,
                },
            }),
        );
        storage
            .write_json_atomic(
                storage.scheduler_state_path(),
                &serde_json::json!({
                    "entries": entries,
                    "last_tick": null
                }),
            )
            .await
            .unwrap();

        let scheduler = AgentScheduler::with_storage(storage);
        scheduler.recover_from_disk().await.unwrap();

        let state = scheduler.snapshot().await;
        assert_eq!(state.entries.len(), 1);

        let known = state.entries.get(&known_key).expect("known entry missing");
        match &known.registration {
            SchedulerTriggerRegistration::Cron { schedule, .. } => {
                assert_eq!(schedule, "0 9 * * *");
            },
            SchedulerTriggerRegistration::Event { .. } => {
                panic!("expected known entry to keep cron registration")
            },
            SchedulerTriggerRegistration::Idle => {
                panic!("expected known entry to keep cron registration")
            },
        }

        assert!(
            !state.entries.contains_key(&unknown_key),
            "unknown registration variants should be discarded after the hard cut"
        );

        assert_eq!(
            scheduler
                .allocate_trigger_seq("agent-a", "g1")
                .await
                .unwrap(),
            4
        );
        let err = scheduler
            .allocate_trigger_seq("agent-b", "g2")
            .await
            .unwrap_err();
        assert_eq!(
            err,
            SchedulerError::NotRegistered {
                agent_id: "agent-b".to_string(),
                goal_id: "g2".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn recover_from_disk_disables_impossible_cron_entries() {
        let tmp = tempdir().unwrap();
        let storage = scoped_storage(&tmp);
        storage.ensure_base_layout().await.unwrap();

        let key = state_key_for_task(&scoped_automation_task_id(
            TEST_PRINCIPAL,
            TEST_WORKSPACE,
            "personal-assistant",
            "harness:personal-assistant:general",
        ));
        storage
            .write_json_atomic(
                storage.scheduler_state_path(),
                &serde_json::json!({
                    "entries": {
                        key.clone(): {
                            "agent_id": "personal-assistant",
                            "goal_id": "harness:personal-assistant:general",
                            "trigger_seq": 4,
                            "last_triggered": "2026-04-14T17:30:22.731272Z",
                            "registration": {
                                "trigger_kind": "cron",
                                "schedule": "0 0 31 2 *",
                                "timezone": "UTC",
                                "next_run_at": "2026-04-14T18:30:22.731272Z"
                            }
                        }
                    },
                    "last_tick": "2026-04-14T17:30:22.731272Z"
                }),
            )
            .await
            .unwrap();

        let scheduler = AgentScheduler::with_storage(storage);
        scheduler.recover_from_disk().await.unwrap();

        let state = scheduler.snapshot().await;
        let entry = state
            .entries
            .get(&key)
            .expect("entry should remain present");
        match &entry.registration {
            SchedulerTriggerRegistration::Cron { next_run_at, .. } => {
                assert_eq!(*next_run_at, None);
            },
            other => panic!("expected cron registration, got {other:?}"),
        }
    }

    #[test]
    fn cron_normalizer_accepts_5_6_7_field_expressions() {
        use crate::magician_v2::storage::task_scheduler::normalize_cron;
        use std::str::FromStr;
        // 5-field standard cron
        assert!(CronSchedule::from_str(&normalize_cron("0 9 * * *")).is_ok());
        // 6-field (with seconds)
        assert!(CronSchedule::from_str(&normalize_cron("0 0 9 * * *")).is_ok());
        // 7-field (native)
        assert!(CronSchedule::from_str(&normalize_cron("0 0 9 * * * *")).is_ok());
    }

    #[test]
    fn compute_next_run_at_posix_or_dom_and_dow() {
        // "At 12:00 on the 15th OR on Mondays" — standard 5-field
        // After 2025-01-12 23:00 UTC:
        //   DoW=Mon → 2025-01-13 12:00
        //   DoM=15 → 2025-01-15 12:00
        // POSIX OR should pick Mon Jan 13 (earlier).
        let after = chrono::NaiveDate::from_ymd_opt(2025, 1, 12)
            .unwrap()
            .and_hms_opt(23, 0, 0)
            .unwrap()
            .and_utc();
        let next = compute_next_run_at("0 12 15 * 1", "UTC", after);
        let next = next.expect("next POSIX OR fire");
        assert_eq!(
            next.day(),
            13,
            "POSIX OR: DoW=Monday should fire before DoM=15th"
        );
        assert_eq!(next.hour(), 12);
    }

    #[tokio::test]
    async fn misfired_cron_trigger_fires_after_restart_recovery() {
        let tmp = tempdir().unwrap();
        let storage = scoped_storage(&tmp);

        let scheduler = AgentScheduler::with_storage(storage.clone());
        register_cron(&scheduler, "agent-a", "g1", "0 9 * * *", "UTC").await;

        // Force persisted next_run_at into the past to simulate downtime misfire.
        let mut persisted: SchedulerState = storage
            .read_json(storage.scheduler_state_path())
            .await
            .unwrap();
        let key = state_key_for_task(&scoped_automation_task_id(
            TEST_PRINCIPAL,
            TEST_WORKSPACE,
            "agent-a",
            "g1",
        ));
        let entry = persisted.entries.get_mut(&key).unwrap();
        match &mut entry.registration {
            SchedulerTriggerRegistration::Cron { next_run_at, .. } => {
                *next_run_at = Some(Utc::now() - Duration::minutes(5));
            },
            SchedulerTriggerRegistration::Event { .. } => panic!("expected cron trigger"),
            SchedulerTriggerRegistration::Idle => panic!("expected cron trigger"),
        }
        storage
            .write_json_atomic(storage.scheduler_state_path(), &persisted)
            .await
            .unwrap();

        let recovered = AgentScheduler::with_storage(storage.clone());
        recovered.recover_from_disk().await.unwrap();
        let due = recovered
            .collect_due_cron_triggers(Utc::now())
            .await
            .unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].principal, TEST_PRINCIPAL);
        assert_eq!(due[0].workspace, TEST_WORKSPACE);
        assert_eq!(due[0].agent_id, "agent-a");
        assert_eq!(due[0].goal_id, "g1");
        assert_eq!(due[0].trigger_seq, 1);

        // Ensure misfire trigger sequence is durable after firing.
        let verify = AgentScheduler::with_storage(storage);
        verify.recover_from_disk().await.unwrap();
        assert_eq!(
            verify.allocate_trigger_seq("agent-a", "g1").await.unwrap(),
            2
        );
    }
}
