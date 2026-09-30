//! Completion-aware scheduling and outcome health for reusable App tasks.
use super::*;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppBehaviorExecutionObservation {
    pub task_id: String,
    pub execution_id: String,
    pub status: String,
    pub terminal: bool,
    pub settled: bool,
    pub completed_at: Option<DateTime<Utc>>,
    pub partial: bool,
    pub published_records: BTreeMap<String, u64>,
    pub error: Option<String>,
    /// Set when the run died on a ceiling that only the calendar clears: the
    /// next fire waits for this instant instead of `completed_at + interval`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppBehaviorRecurringState {
    pub latest: AppBehaviorExecutionObservation,
    pub completed_count: u64,
    pub partial_count: u64,
    pub failed_count: u64,
    pub published_records: BTreeMap<String, u64>,
    pub terminal_counted: bool,
}

pub(super) fn has_settled_current_failure(
    connection: &rusqlite::Connection,
    installation_id: &AppInstallationId,
    behavior_id: &AppName,
) -> Result<bool, AppBehaviorSchedulerError> {
    let bytes: Option<Vec<u8>> = connection
        .query_row(
            "SELECT record_json FROM app_behavior_execution_state
         WHERE installation_id = ?1 AND behavior_id = ?2 AND needs_observation = 0",
            params![installation_id.as_str(), behavior_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    let Some(bytes) = bytes else {
        return Ok(false);
    };
    let state: AppBehaviorRecurringState = serde_json::from_slice(&bytes)?;
    if !state.latest.terminal || !state.latest.settled || state.latest.status != "failed" {
        return Ok(false);
    }
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM app_recurring_task_heads h
         JOIN app_recurring_occurrence_locators l ON l.occurrence_id = h.occurrence_id
         WHERE h.task_id = ?1 AND l.task_id = h.task_id AND l.execution_id = ?2)",
        params![state.latest.task_id, state.latest.execution_id],
        |row| row.get(0),
    )?)
}

impl AppArtifactTaskAcceptanceProbe {
    pub(super) async fn read_recurring_execution(
        &self,
        task_id: &str,
    ) -> Result<Option<AppBehaviorExecutionObservation>, AppBehaviorSchedulerError> {
        let service = self
            .artifact_service
            .as_ref()
            .ok_or(AppBehaviorSchedulerError::TaskStateProbeUnavailable)?;
        let task = match service.get_task(&self.scope, task_id).await {
            Ok(task) => task,
            Err(ArtifactV2Error::TaskNotFound(_)) => return Ok(None),
            Err(_) => return Err(AppBehaviorSchedulerError::TaskStateProbeUnavailable),
        };
        if !task
            .manifest
            .tags
            .iter()
            .any(|tag| tag.id == "app_recurring")
        {
            return Err(AppBehaviorSchedulerError::CorruptState);
        }
        let workflows = service.app_workflow_service();
        let Some(execution_id) = workflows
            .current_recurring_execution_id(&self.scope, task_id)
            .await?
        else {
            return if task.state.latest_root_execution_id.is_none() {
                Ok(None)
            } else {
                Err(AppBehaviorSchedulerError::CorruptState)
            };
        };
        let execution = match service
            .get_execution(&self.scope, task_id, &execution_id)
            .await
        {
            Ok(execution) => execution,
            // A sealed occurrence with no accepted root is retryable. The
            // task may still point to its previous completed occurrence.
            Err(ArtifactV2Error::ExecutionNotFound(_))
                if task.state.active_root_execution_id.is_none() =>
            {
                return Ok(None)
            },
            Err(_) => return Err(AppBehaviorSchedulerError::TaskStateProbeUnavailable),
        };
        if execution.state.parent_execution_id.is_some()
            || execution.state.root_execution_id.as_deref() != Some(execution_id.as_str())
        {
            return Err(AppBehaviorSchedulerError::CorruptState);
        }
        let terminal = matches!(
            execution.state.status.as_str(),
            "completed" | "failed" | "cancelled" | "canceled"
        );
        let (published_records, partial) = if terminal {
            workflows
                .recurring_execution_record_counts(&self.scope, task_id, &execution_id)
                .await?
        } else {
            (BTreeMap::new(), false)
        };
        let cleanup = if terminal {
            Box::pin(workflows.recover_crashed_resource_execution(
                &self.scope,
                task_id,
                &execution_id,
                &execution.state.agent_id,
                &execution.state.status,
                &execution.state.updated_at,
                Utc::now(),
            ))
            .await
        } else {
            Ok(())
        };
        let cleanup_ok = cleanup.is_ok();
        let completed_at = execution
            .state
            .completed_at
            .as_deref()
            .map(DateTime::parse_from_rfc3339)
            .transpose()
            .map_err(|_| AppBehaviorSchedulerError::CorruptState)?
            .map(|date| date.with_timezone(&Utc));
        // A run whose model attempts were refused on a period ceiling is not
        // a run to repeat on the interval: the ceiling clears when the period
        // does. The round completes without the participants — the run's
        // status is `completed`, not `failed` — so the marker the refusal
        // left on the run is read off every terminal run, whatever its
        // status; a missing marker means an ordinary run.
        let period_ceiling = if terminal {
            workflows
                .period_ceiling_breach(
                    &self.scope,
                    task_id,
                    &execution_id,
                    &execution.state.agent_id,
                )
                .await
                .unwrap_or_else(|error| {
                    tracing::warn!(
                        task_id,
                        execution_id,
                        error = %error,
                        "could not read the run's period ceiling marker; scheduling on the interval"
                    );
                    None
                })
        } else {
            None
        };
        let deferred_until = match (&period_ceiling, completed_at) {
            (Some(breach), Some(completed)) => {
                match crate::magician_v2::apps::resource_authority::resource_period_end(
                    completed.max(breach.observed_at),
                ) {
                    Ok(period_end) => Some(period_end),
                    Err(error) => {
                        tracing::warn!(
                            task_id,
                            execution_id,
                            error = %error,
                            "could not derive the resource period end; scheduling on the interval"
                        );
                        None
                    },
                }
            },
            _ => None,
        };
        let error = cleanup
            .err()
            .map(|error| error.to_string())
            .or_else(|| {
                period_ceiling
                    .as_ref()
                    .map(|breach| format!("period_resource_ceiling:{}", breach.ceiling))
            })
            .or_else(|| {
                (execution.state.status == "failed").then(|| "workflow_execution_failed".to_owned())
            });
        Ok(Some(AppBehaviorExecutionObservation {
            task_id: task_id.to_owned(),
            execution_id,
            status: execution.state.status.clone(),
            terminal,
            settled: terminal && cleanup_ok,
            completed_at,
            partial,
            published_records,
            error,
            deferred_until,
        }))
    }
}

fn reduce_observation(
    previous: Option<AppBehaviorRecurringState>,
    observed: AppBehaviorExecutionObservation,
) -> AppBehaviorRecurringState {
    let mut state = previous.unwrap_or_else(|| AppBehaviorRecurringState {
        latest: observed.clone(),
        completed_count: 0,
        partial_count: 0,
        failed_count: 0,
        published_records: BTreeMap::new(),
        terminal_counted: false,
    });
    if state.latest.execution_id != observed.execution_id {
        state.terminal_counted = false;
    }
    if observed.terminal && observed.settled && !state.terminal_counted {
        if observed.status == "completed" {
            state.completed_count += 1;
            if observed.partial {
                state.partial_count += 1;
            }
        } else {
            state.failed_count += 1;
        }
        for (entity, count) in &observed.published_records {
            *state.published_records.entry(entity.clone()).or_default() += count;
        }
        state.terminal_counted = true;
    }
    state.latest = observed;
    state
}

impl AppBehaviorScheduler {
    pub(super) async fn observe_recurring_before_claim(
        &self,
        authenticated: &AuthenticatedAppScope,
        binding: &AppBehaviorBinding,
        probe: &dyn AppBehaviorTaskAcceptanceProbe,
        now: DateTime<Utc>,
    ) -> Result<bool, AppBehaviorSchedulerError> {
        self.observe_recurring(
            authenticated,
            &binding.installation_id,
            &binding.behavior_id,
            binding.effective_interval_seconds,
            probe,
            now,
        )
        .await
    }

    pub async fn observe_dispatch(
        &self,
        authenticated: &AuthenticatedAppScope,
        dispatch: &AppBehaviorDispatch,
        probe: &dyn AppBehaviorTaskAcceptanceProbe,
        now: DateTime<Utc>,
    ) -> Result<(), AppBehaviorSchedulerError> {
        self.observe_recurring(
            authenticated,
            &dispatch.installation_id,
            &dispatch.behavior_id,
            dispatch.lease.effective_interval_seconds,
            probe,
            now,
        )
        .await
        .map(|_| ())
    }

    async fn observe_recurring(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        behavior_id: &AppName,
        interval: u64,
        probe: &dyn AppBehaviorTaskAcceptanceProbe,
        now: DateTime<Utc>,
    ) -> Result<bool, AppBehaviorSchedulerError> {
        let task_id =
            recurring_behavior_task_id(authenticated.scope(), installation_id, behavior_id)?;
        let Some(observed) = probe.recurring_execution(&task_id).await? else {
            return Ok(true);
        };
        let installation_id = installation_id.clone();
        let behavior_id = behavior_id.clone();
        self.registry
            .execute_scoped_typed_background_write(authenticated, &now, move |connection, _| {
                persist_recurring_observation(
                    connection,
                    &installation_id,
                    &behavior_id,
                    interval,
                    observed,
                    now,
                )
            })
            .await
    }

    pub(super) async fn populate_recurring_health(
        &self,
        authenticated: &AuthenticatedAppScope,
        items: &mut [AppBehaviorHealthItem],
        now: DateTime<Utc>,
    ) -> Result<(), AppBehaviorSchedulerError> {
        let keys = items
            .iter()
            .map(|item| {
                (
                    item.installation_id.to_string(),
                    item.behavior_id.to_string(),
                )
            })
            .collect::<Vec<_>>();
        let records = self.registry.execute_scoped_typed_read(authenticated, &now, move |connection, _|
            -> Result<Vec<Option<AppBehaviorRecurringState>>, AppBehaviorSchedulerError> {
            let mut query = connection.prepare("SELECT record_json FROM app_behavior_execution_state WHERE installation_id = ?1 AND behavior_id = ?2")?;
            keys.iter().map(|(installation, behavior)| {
                let bytes: Option<Vec<u8>> = query.query_row(params![installation, behavior], |row| row.get(0)).optional()?;
                bytes.as_deref().map(serde_json::from_slice).transpose().map_err(Into::into)
            }).collect()
        }).await?.unwrap_or_default();
        for (item, recurring) in items.iter_mut().zip(records) {
            if let Some(state) = recurring.as_ref() {
                if !state.latest.terminal {
                    item.state = "running".into();
                } else if !state.latest.settled {
                    item.state = "recovering".into();
                }
                if item.state != "blocked" && state.latest.error.is_some() {
                    item.last_error = state.latest.error.clone();
                }
            }
            item.recurring = recurring;
        }
        Ok(())
    }
}

fn persist_recurring_observation(
    connection: &mut rusqlite::Connection,
    installation_id: &AppInstallationId,
    behavior_id: &AppName,
    interval: u64,
    observed: AppBehaviorExecutionObservation,
    now: DateTime<Utc>,
) -> Result<bool, AppBehaviorSchedulerError> {
    let allowed = observed.terminal && observed.settled;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // A second worker may finish observing the previous round after
    // its successor is already published. Never roll health backwards.
    let current_execution: Option<String> = transaction.query_row(
        "SELECT l.execution_id FROM app_recurring_task_heads h JOIN app_recurring_occurrence_locators l
         ON l.occurrence_id = h.occurrence_id WHERE h.task_id = ?1",
        [&observed.task_id], |row| row.get(0)).optional()?;
    if current_execution
        .as_deref()
        .is_some_and(|id| id != observed.execution_id)
    {
        transaction.commit()?;
        return Ok(false);
    }
    let previous: Option<Vec<u8>> = transaction.query_row(
        "SELECT record_json FROM app_behavior_execution_state WHERE installation_id = ?1 AND behavior_id = ?2",
        params![installation_id.as_str(), behavior_id.as_str()], |row| row.get(0),
    ).optional()?;
    let previous: Option<AppBehaviorRecurringState> = previous
        .as_deref()
        .map(serde_json::from_slice)
        .transpose()?;
    if previous
        .as_ref()
        .is_some_and(|state| state.latest == observed)
    {
        transaction.commit()?;
        return Ok(allowed);
    }
    let terminal_new = observed.terminal
        && observed.settled
        && previous.as_ref().is_none_or(|previous| {
            previous.latest.execution_id != observed.execution_id || !previous.latest.settled
        });
    let state = reduce_observation(previous, observed);
    if terminal_new {
        let completed = state
            .latest
            .completed_at
            .ok_or(AppBehaviorSchedulerError::CorruptState)?;
        let interval_next = completed
            .checked_add_signed(Duration::seconds(
                i64::try_from(interval).map_err(|_| AppBehaviorSchedulerError::CorruptState)?,
            ))
            .ok_or(AppBehaviorSchedulerError::CorruptState)?;
        // A run that died on a period ceiling parks the behavior until the
        // period ends. Relaunching on the interval only reproduces the
        // refusal — measured: 112 launches and 2,993 refused model attempts
        // in one day, each launch a recipe, a round per participant, and
        // registry writes, for a ceiling only the calendar clears.
        let next = match state.latest.deferred_until {
            Some(deferred) if deferred > interval_next => {
                tracing::warn!(
                    installation_id = %installation_id,
                    behavior_id = %behavior_id,
                    execution_id = %state.latest.execution_id,
                    error = state.latest.error.as_deref().unwrap_or(""),
                    next_due_at = %timestamp(deferred),
                    "app background behavior parked until the resource period ends"
                );
                deferred
            },
            _ => interval_next,
        };
        transaction.execute("UPDATE app_behavior_heads SET next_due_at = ?1,
            last_error = CASE WHEN state = 'blocked' THEN last_error ELSE ?2 END,
            consecutive_failures = CASE WHEN ?3 = 'completed' THEN 0 ELSE MIN(consecutive_failures + 1, 64) END,
            updated_at = ?4, revision = revision + 1 WHERE installation_id = ?5 AND behavior_id = ?6",
            params![timestamp(next), state.latest.error, state.latest.status, timestamp(now), installation_id.as_str(), behavior_id.as_str()])?;
    }
    transaction.execute("INSERT INTO app_behavior_execution_state (installation_id, behavior_id, record_json, needs_observation)
        VALUES (?1, ?2, ?3, ?4) ON CONFLICT(installation_id, behavior_id) DO UPDATE SET record_json = excluded.record_json, needs_observation = excluded.needs_observation",
        params![installation_id.as_str(), behavior_id.as_str(), serde_json::to_vec(&state)?, !state.latest.terminal || !state.latest.settled])?;
    transaction.commit()?;
    Ok(allowed)
}

#[cfg(test)]
#[path = "recurring_tests.rs"]
mod tests;
