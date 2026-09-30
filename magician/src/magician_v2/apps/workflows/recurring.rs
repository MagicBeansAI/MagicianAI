//! Stable scheduled-task identity with immutable occurrence/execution bindings.

use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRecurringOccurrence {
    pub occurrence_id: String,
    pub execution_id: String,
}

#[derive(Serialize, Deserialize)]
struct RecurringOccurrenceLocator {
    task_id: String,
    occurrence_id: String,
    execution_id: String,
}

/// Read-only task presentation. This neither schedules work nor mints App authority.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppRecurringTaskSchedule {
    pub behavior_id: AppName,
    pub interval_seconds: u64,
    pub next_due_at: Option<String>,
    pub latest_status: Option<String>,
    pub waiting_for_settlement: bool,
}

pub(crate) fn recurring_behavior_task_id(
    scope: &AppScope,
    installation_id: &AppInstallationId,
    behavior_id: &AppName,
) -> Result<String, AppWorkflowError> {
    let digest = AppDigest::blake3_canonical_json(&json!({
        "schema": "magician.app-recurring-task.v1",
        "scope": scope,
        "installation_id": installation_id,
        "behavior_id": behavior_id,
    }))?;
    Ok(format!(
        "task_app_{}",
        digest.as_str().trim_start_matches("blake3:")
    ))
}

impl AppWorkflowTaskBinding {
    pub(super) fn occurrence_control_id(&self) -> &str {
        self.recurring
            .as_ref()
            .map_or(self.task_id.as_str(), |run| run.occurrence_id.as_str())
    }

    pub(super) fn result_path(
        &self,
        workspace: &ArtifactV2Workspace,
        scope: &ScopeRef,
    ) -> std::path::PathBuf {
        match &self.recurring {
            Some(run) => workspace
                .execution_dir(
                    &scope.principal(),
                    &scope.workspace(),
                    &self.task_id,
                    &run.execution_id,
                )
                .join("app_workflow_result.json"),
            None => task_result_path(workspace, scope, &self.task_id),
        }
    }
}

impl AppWorkflowRunControl {
    /// Maintenance may retire old per-fire task shells; the live recurring
    /// task and user/event actions are outside this authority.
    pub(crate) fn is_legacy_scheduled_behavior(&self) -> bool {
        self.task.recurring.is_none() && self.task.background_behavior_binding.is_some()
    }

    /// Exact occurrence root, independent of the task's latest execution.
    pub fn recurring_execution_id(&self) -> Option<&str> {
        self.task
            .recurring
            .as_ref()
            .map(|run| run.execution_id.as_str())
    }
}

impl AppWorkflowService {
    #[cfg(test)]
    pub(crate) async fn seed_terminal_recovery_test_state(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
    ) -> Result<std::path::PathBuf, AppWorkflowError> {
        let mut task = super::tests::fixture_task();
        task.task_id = task_id.to_owned();
        // Exercise nested sealed reads with a retained, nontrivial input.
        task.invocation.input.value = json!({"retained": "x".repeat(128 * 1024)});
        let task = self.persist_task_binding(scope, &task, None).await?;
        let mut state = super::tests::fixture_run_state(&task, execution_id, "personal-assistant");
        state.run_binding.execution_id = AppReference::parse(execution_id)?;
        state.terminal_attempt_generation = 1;
        self.persist_run_state_unlocked(scope, task_id, execution_id, &state)
            .await?;
        Ok(run_state_path(
            &self.workspace,
            scope,
            task_id,
            execution_id,
        ))
    }

    pub async fn recurring_task_schedule(
        &self,
        scope: &ScopeRef,
        task_id: &str,
    ) -> Result<Option<AppRecurringTaskSchedule>, AppWorkflowError> {
        use rusqlite::OptionalExtension;
        // A first occurrence's task shell can be visible before its binding
        // is published. History remains readable during that admission window.
        if !is_canonical_app_workflow_task_id(task_id)
            || self
                .registry
                .recurring_locator(app_scope_from_scope_ref(scope)?, task_id.to_owned(), true)
                .await?
                .is_none()
        {
            return Ok(None);
        }
        let Some(binding) = self.read_task_binding(scope, task_id).await? else {
            return Ok(None);
        };
        let Some(run) = &binding.recurring else {
            return Ok(None);
        };
        let behavior = binding
            .background_behavior_binding
            .as_ref()
            .ok_or(AppWorkflowError::CorruptBinding)?
            .grant
            .behavior_id
            .clone();
        let now = Utc::now();
        let authenticated = task_execution_scope(
            scope,
            &binding.accepted_authority.scope_binding_ref,
            &run.execution_id,
            now,
        )?;
        let execution_id = run.execution_id.clone();
        self.registry.execute_scoped_typed_read(&authenticated, &now, move |connection, _|
            -> Result<Option<AppRecurringTaskSchedule>, AppWorkflowError> {
            let row: Option<(u64, String, Option<Vec<u8>>)> = connection.query_row(
                "SELECT h.effective_interval_seconds, h.next_due_at, e.record_json
                 FROM app_behavior_heads h LEFT JOIN app_behavior_execution_state e
                 ON e.installation_id = h.installation_id AND e.behavior_id = h.behavior_id
                 WHERE h.installation_id = ?1 AND h.behavior_id = ?2",
                rusqlite::params![binding.installation_id.as_str(), behavior.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional()
                .map_err(AppRegistryError::from)?;
            row.map(|(interval_seconds, next, bytes)| {
                let observed: Option<super::super::background_behaviors::AppBehaviorRecurringState> =
                    bytes.as_deref().map(serde_json::from_slice).transpose()?;
                let observed = observed.filter(|state| state.latest.execution_id == execution_id);
                let waiting = observed.as_ref().is_none_or(|state|
                    !state.latest.terminal || !state.latest.settled);
                Ok(AppRecurringTaskSchedule { behavior_id: behavior, interval_seconds,
                    next_due_at: (!waiting).then_some(next),
                    latest_status: observed.map(|state| state.latest.status),
                    waiting_for_settlement: waiting })
            }).transpose()
        }).await.map(Option::flatten)
    }

    pub(super) async fn read_occurrence_binding(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        control_id: &str,
    ) -> Result<Option<AppWorkflowTaskBinding>, AppWorkflowError> {
        let Some(record) = self
            .registry
            .current_workflow_control(
                app_scope_from_scope_ref(scope)?,
                task_id.to_owned(),
                control_id.to_owned(),
                AppWorkflowControlKind::TaskBinding,
            )
            .await?
        else {
            return Ok(None);
        };
        if record.lifecycle() != AppWorkflowControlLifecycle::Active {
            return Err(AppWorkflowError::CorruptBinding);
        }
        // Archived task metadata has the same admission bound as its current
        // sidecar. The small-control default rejects ordinary shipped bindings
        // once their sealed agent, prompt and recipe material exceeds 16 KiB.
        // A store operation can reach this reader beneath several recipe poll
        // frames. Decode and authenticate on the bounded blocking lane, where
        // the full agent/recipe metadata has a fresh native stack. Boxing the
        // async reader alone does not separate its synchronous Serde frames.
        let workspace = self.workspace.clone();
        let owned_scope = scope.clone();
        let owned_task_id = task_id.to_owned();
        let binding = magician_core::blocking_admission::spawn_blocking_admitted(move || {
            open_sealed_control_blob_bounded::<Box<AppWorkflowTaskBinding>>(
                &workspace,
                &owned_scope,
                "task-binding",
                &owned_task_id,
                None,
                record.sealed_blob(),
                MAX_TASK_BINDING_BYTES,
            )
        })
        .await
        .map_err(|error| {
            ArtifactV2Error::Runtime(format!("app occurrence decoder failed to join: {error}"))
        })??;
        let run = binding
            .recurring
            .as_ref()
            .ok_or(AppWorkflowError::CorruptBinding)?;
        let behavior = binding
            .background_behavior_binding
            .as_ref()
            .ok_or(AppWorkflowError::CorruptBinding)?;
        if binding.task_id != task_id
            || (run.occurrence_id != control_id && run.execution_id != control_id)
            || !is_canonical_app_workflow_task_id(&run.occurrence_id)
            || recurring_behavior_task_id(
                &app_scope_from_scope_ref(scope)?,
                &binding.installation_id,
                &behavior.grant.behavior_id,
            )? != task_id
        {
            return Err(AppWorkflowError::CorruptBinding);
        }
        validate_execution_id(&run.execution_id)?;
        Ok(Some(*binding))
    }

    pub(super) async fn read_execution_task_binding(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
    ) -> Result<Option<AppWorkflowTaskBinding>, AppWorkflowError> {
        validate_execution_id(execution_id)?;
        // Most execution reads are roots. Resolve the retained binding directly
        // without reopening and repairing the task's unrelated latest cache.
        if is_canonical_app_workflow_task_id(task_id) {
            if let Some(binding) = self
                .read_occurrence_binding(scope, task_id, execution_id)
                .await?
            {
                if !self
                    .has_authoritative_app_task_marker(scope, task_id)
                    .await?
                {
                    return Err(AppWorkflowError::CorruptBinding);
                }
                return Ok(Some(binding));
            }
        }
        let current = self.read_task_binding(scope, task_id).await?;
        if current
            .as_ref()
            .is_none_or(|binding| binding.recurring.is_none())
        {
            return Ok(current);
        }
        // Delegated children inherit their own canonical root's immutable
        // binding, even after another occurrence becomes the task's head.
        let path = self
            .workspace
            .execution_dir(
                &scope.principal(),
                &scope.workspace(),
                task_id,
                execution_id,
            )
            .join("state.json");
        let state: serde_json::Value = self
            .workspace
            .read_json_bounded_stream_path(
                &path,
                MAX_TASK_BINDING_BYTES,
                MAX_JSON_DEPTH,
                MAX_JSON_NODES,
            )
            .await?;
        let root = state
            .get("root_execution_id")
            .and_then(Value::as_str)
            .ok_or(AppWorkflowError::InvalidExecutionLineage)?;
        validate_execution_id(root)?;
        self.read_occurrence_binding(scope, task_id, root)
            .await?
            .map(Some)
            .ok_or(AppWorkflowError::CorruptBinding)
    }

    pub(super) async fn archive_occurrence_binding(
        &self,
        scope: &ScopeRef,
        binding: &AppWorkflowTaskBinding,
        sealed: &[u8],
    ) -> Result<(), AppWorkflowError> {
        let Some(run) = binding.recurring.as_ref() else {
            return Ok(());
        };
        for key in [&run.occurrence_id, &run.execution_id] {
            if let Some(existing) = self
                .registry
                .current_workflow_control(
                    app_scope_from_scope_ref(scope)?,
                    binding.task_id.clone(),
                    key.clone(),
                    AppWorkflowControlKind::TaskBinding,
                )
                .await?
            {
                if existing.lifecycle() != AppWorkflowControlLifecycle::Active
                    || existing.sealed_blob() != sealed
                {
                    return Err(AppWorkflowError::TaskBindingConflict);
                }
            } else {
                let record = self
                    .registry
                    .publish_workflow_control_generation(
                        app_scope_from_scope_ref(scope)?,
                        binding.task_id.clone(),
                        key.clone(),
                        AppWorkflowControlKind::TaskBinding,
                        sealed.to_vec(),
                        Utc::now(),
                    )
                    .await?;
                if record.lifecycle() != AppWorkflowControlLifecycle::Active
                    || record.sealed_blob() != sealed
                {
                    return Err(AppWorkflowError::TaskBindingConflict);
                }
            }
        }
        // The scheduler's deterministic fire locator remains an immutable
        // delivery index. It is a control row, not an Artifact task shell.
        let locator = RecurringOccurrenceLocator {
            task_id: binding.task_id.clone(),
            occurrence_id: run.occurrence_id.clone(),
            execution_id: run.execution_id.clone(),
        };
        let locator_bytes = sealed_sidecar_bytes(
            &self.workspace,
            scope,
            "recurring-occurrence",
            &run.occurrence_id,
            None,
            &locator,
        )?;
        self.registry
            .publish_recurring_locator(
                app_scope_from_scope_ref(scope)?,
                binding.task_id.clone(),
                run.occurrence_id.clone(),
                run.execution_id.clone(),
                locator_bytes,
            )
            .await?;
        Ok(())
    }

    pub(crate) async fn recurring_occurrence_locator(
        &self,
        scope: &ScopeRef,
        occurrence_id: &str,
    ) -> Result<Option<(String, String)>, AppWorkflowError> {
        let Some((task_id, stored_occurrence_id, execution_id, bytes)) = self
            .registry
            .recurring_locator(
                app_scope_from_scope_ref(scope)?,
                occurrence_id.to_owned(),
                false,
            )
            .await?
        else {
            return Ok(None);
        };
        self.open_recurring_locator_record(
            scope,
            occurrence_id,
            (task_id, stored_occurrence_id, execution_id, bytes),
        )
        .await
        .map(Some)
    }

    pub(crate) async fn current_recurring_execution_id(
        &self,
        scope: &ScopeRef,
        task_id: &str,
    ) -> Result<Option<String>, AppWorkflowError> {
        let Some(row) = self
            .registry
            .recurring_locator(app_scope_from_scope_ref(scope)?, task_id.to_owned(), true)
            .await?
        else {
            return Ok(None);
        };
        if row.0 != task_id {
            return Err(AppWorkflowError::CorruptBinding);
        }
        let occurrence_id = row.1.clone();
        self.open_recurring_locator_record(scope, &occurrence_id, row)
            .await
            .map(|(_, execution_id)| Some(execution_id))
    }

    async fn open_recurring_locator_record(
        &self,
        scope: &ScopeRef,
        occurrence_id: &str,
        (task_id, stored_occurrence_id, execution_id, bytes): (String, String, String, Vec<u8>),
    ) -> Result<(String, String), AppWorkflowError> {
        let locator: RecurringOccurrenceLocator = open_sealed_control_blob(
            &self.workspace,
            scope,
            "recurring-occurrence",
            occurrence_id,
            None,
            &bytes,
        )?;
        if locator.occurrence_id != occurrence_id
            || stored_occurrence_id != occurrence_id
            || locator.task_id != task_id
            || locator.execution_id != execution_id
        {
            return Err(AppWorkflowError::CorruptBinding);
        }
        let binding = self
            .read_occurrence_binding(scope, &locator.task_id, occurrence_id)
            .await?
            .ok_or(AppWorkflowError::CorruptBinding)?;
        if binding
            .recurring
            .as_ref()
            .map(|run| run.execution_id.as_str())
            != Some(locator.execution_id.as_str())
        {
            return Err(AppWorkflowError::CorruptBinding);
        }
        Ok((locator.task_id, locator.execution_id))
    }

    pub(crate) async fn recurring_execution_record_counts(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        execution_id: &str,
    ) -> Result<(BTreeMap<String, u64>, bool), AppWorkflowError> {
        let Some(state) = self.read_run_state(scope, task_id, execution_id).await? else {
            return Ok((BTreeMap::new(), false));
        };
        let mut counts = BTreeMap::new();
        let mut partial = false;
        for native in state.native_rounds.values() {
            for participant in native.round.participants() {
                use crate::magician_v2::apps::contextual_round::AppRoundParticipantState;
                match participant.state() {
                    AppRoundParticipantState::Committed { record_ids, .. } => {
                        for record in record_ids {
                            if let Some((entity, _)) = record.split_once(':') {
                                *counts.entry(entity.to_owned()).or_insert(0) += 1;
                            }
                        }
                    },
                    AppRoundParticipantState::Quiet { .. } => {},
                    _ => partial = true,
                }
            }
        }
        Ok((counts, partial))
    }

    pub(super) async fn acquire_recurring_launch_guard(
        &self,
        scope: &ScopeRef,
        task_id: &str,
    ) -> Result<AppWorkflowTaskGuard, AppWorkflowError> {
        ArtifactV2Workspace::validate_task_id(task_id)?;
        let path = self
            .workspace
            .task_lock_path(&scope.principal(), &scope.workspace(), task_id)
            .with_extension("recurring-launch.lock");
        if let Some(parent) = path.parent() {
            self.workspace.create_dir_all_path(parent).await?;
        }
        let lock_path = path.clone();
        let file = tokio::task::spawn_blocking(move || -> std::io::Result<File> {
            let mut options = OpenOptions::new();
            options.create(true).read(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
            }
            let file = options.open(path)?;
            file.lock_exclusive()?;
            Ok(file)
        })
        .await
        .map_err(|e| AppWorkflowError::WorkerTerminated(e.to_string()))?
        .map_err(|e| AppWorkflowError::WorkerTerminated(e.to_string()))?;
        Ok(AppWorkflowTaskGuard { file, lock_path })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::apps::registry::tests::canonical_tempdir;

    fn binding(occurrence: u8) -> AppWorkflowTaskBinding {
        let mut task = super::super::tests::fixture_task();
        let behavior = AppName::parse("daily_digest").unwrap();
        let scope = app_scope_from_scope_ref(&scope()).unwrap();
        task.task_id =
            recurring_behavior_task_id(&scope, &task.installation_id, &behavior).unwrap();
        task.recurring = Some(AppRecurringOccurrence {
            occurrence_id: format!("task_app_{:064x}", occurrence),
            execution_id: format!("exec_app_{:064x}", occurrence),
        });
        task.invocation.idempotency_key =
            AppReference::parse(format!("fire:{occurrence}")).unwrap();
        task.background_behavior_binding = Some(AppWorkflowBehaviorBinding {
            grant: serde_json::from_value(json!({
                "behavior_id": behavior, "purpose": "A recurring test", "action": "capture",
                "input_selector_digest": AppDigest::blake3(b"selector"), "operations": [],
                "min_interval_seconds": 60,
                "resources": {"max_tokens_per_run": 1000, "max_cost_microusd_per_run": 1000,
                    "max_active_seconds_per_run": 600, "max_tokens_per_month": 100000,
                    "max_cost_microusd_per_month": 100000, "max_starts_per_period": 60,
                    "period_seconds": 3600, "max_causation_depth": 1, "max_spend_depth": 1,
                    "max_contribution_proposals_per_run": 0},
                "reviewed_request_digest": AppDigest::blake3(b"behavior")
            }))
            .unwrap(),
            launch_ref: AppReference::parse(format!("behavior-launch:fire:{occurrence}")).unwrap(),
            source_policy: task
                .accepted_authority
                .effective_data_handling_policy
                .clone(),
        });
        task
    }

    fn scope() -> ScopeRef {
        ScopeRef::system_internal_unauthenticated("anonymous", "default")
    }

    #[tokio::test]
    async fn recurring_archived_binding_accepts_normal_large_task_metadata() {
        let root = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(root.path());
        let service = AppWorkflowService::new(workspace.clone());
        let mut task = binding(7);
        task.invocation.input.value = json!({"retained": "x".repeat(128 * 1024)});
        let sealed = sealed_sidecar_bytes(
            &workspace,
            &scope(),
            "task-binding",
            &task.task_id,
            None,
            &task,
        )
        .unwrap();
        assert!(sealed.len() > 16 * 1024 && (sealed.len() as u64) < MAX_TASK_BINDING_BYTES);
        service
            .archive_occurrence_binding(&scope(), &task, &sealed)
            .await
            .unwrap();
        let run = task.recurring.as_ref().unwrap();
        for control_id in [&run.occurrence_id, &run.execution_id] {
            let retained = service
                .read_occurrence_binding(&scope(), &task.task_id, control_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(retained.invocation, task.invocation);
        }
        let restarted = AppWorkflowService::new(workspace);
        assert_eq!(
            restarted
                .current_recurring_execution_id(&scope(), &task.task_id)
                .await
                .unwrap()
                .as_deref(),
            Some(run.execution_id.as_str())
        );
    }

    // Model the native frames retained by the recipe evaluator, contextual
    // round and store-resource reader. These bytes must live across poll, not
    // in an async state machine on the heap. Keep worker stacks at the default.
    #[inline(never)]
    fn poll_under_recipe_frames<F: std::future::Future>(
        mut future: std::pin::Pin<&mut F>,
        cx: &mut std::task::Context<'_>,
        depth: usize,
    ) -> std::task::Poll<F::Output> {
        let mut frame = [0u8; 32 * 1024];
        std::hint::black_box(&mut frame);
        let result = if depth == 0 {
            future.as_mut().poll(cx)
        } else {
            poll_under_recipe_frames(future, cx, depth - 1)
        };
        std::hint::black_box(&mut frame);
        result
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn recurring_archived_binding_decodes_with_recipe_poll_stack_pressure() {
        let root = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(root.path());
        let service = AppWorkflowService::new(workspace.clone());
        let mut task = binding(8);
        task.invocation.input.value = json!({"retained": "x".repeat(128 * 1024)});
        let sealed = sealed_sidecar_bytes(
            &workspace,
            &scope(),
            "task-binding",
            &task.task_id,
            None,
            &task,
        )
        .unwrap();
        service
            .archive_occurrence_binding(&scope(), &task, &sealed)
            .await
            .unwrap();
        let task_id = task.task_id.clone();
        let control_id = task.recurring.as_ref().unwrap().execution_id.clone();
        // Spawn explicitly: a multi-thread test's block_on future itself runs
        // on the harness thread, which would hide execution-worker overflows.
        let input = tokio::spawn(async move {
            let mut read = Box::pin(async move {
                service
                    .read_occurrence_binding(&scope(), &task_id, &control_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .invocation
                    .input
            });
            std::future::poll_fn(|cx| poll_under_recipe_frames(read.as_mut(), cx, 31)).await
        })
        .await
        .unwrap();
        assert_eq!(input, task.invocation.input);
    }

    #[test]
    fn recurring_identity_is_scoped_to_installation_and_behavior_not_occurrence_or_version() {
        let first = binding(1);
        let mut second = binding(2);
        second.installation_generation += 1;
        second.package_revision_ref = AppReference::parse("package:upgraded").unwrap();
        assert_eq!(first.task_id, second.task_id);
        assert_ne!(
            app_run_handle(&first).unwrap(),
            app_run_handle(&second).unwrap()
        );
        assert_ne!(
            first.result_path(&ArtifactV2Workspace::new("/tmp/recurring-test"), &scope()),
            second.result_path(&ArtifactV2Workspace::new("/tmp/recurring-test"), &scope())
        );
        let mut other = app_scope_from_scope_ref(&scope()).unwrap();
        other.workspace = AppReference::parse("another").unwrap();
        assert_ne!(
            first.task_id,
            recurring_behavior_task_id(
                &other,
                &first.installation_id,
                &AppName::parse("daily_digest").unwrap()
            )
            .unwrap()
        );
        assert_ne!(
            first.task_id,
            recurring_behavior_task_id(
                &app_scope_from_scope_ref(&scope()).unwrap(),
                &AppInstallationId::parse("other-install").unwrap(),
                &AppName::parse("daily_digest").unwrap()
            )
            .unwrap()
        );
    }

    #[tokio::test]
    async fn recurring_archives_survive_restart_and_old_replay_never_replaces_latest_binding() {
        let root = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(root.path());
        let service = AppWorkflowService::new(workspace.clone());
        let first = binding(1);
        let second = binding(2);
        for task in [&first, &second, &first] {
            let sealed = sealed_sidecar_bytes(
                &workspace,
                &scope(),
                "task-binding",
                &task.task_id,
                None,
                task,
            )
            .unwrap();
            service
                .archive_occurrence_binding(&scope(), task, &sealed)
                .await
                .unwrap();
        }
        let reopened = AppWorkflowService::new(workspace);
        let latest = reopened
            .registry
            .current_workflow_control(
                app_scope_from_scope_ref(&scope()).unwrap(),
                first.task_id.clone(),
                first.task_id.clone(),
                AppWorkflowControlKind::TaskBinding,
            )
            .await
            .unwrap()
            .unwrap();
        let current: AppWorkflowTaskBinding = open_sealed_control_blob(
            &reopened.workspace,
            &scope(),
            "task-binding",
            &first.task_id,
            None,
            latest.sealed_blob(),
        )
        .unwrap();
        assert_eq!(current.recurring, second.recurring);
        for task in [&first, &second] {
            let run = task.recurring.as_ref().unwrap();
            let retained = reopened
                .read_occurrence_binding(&scope(), &task.task_id, &run.execution_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(retained.invocation, task.invocation);
            assert_eq!(
                reopened
                    .recurring_occurrence_locator(&scope(), &run.occurrence_id)
                    .await
                    .unwrap(),
                Some((task.task_id.clone(), run.execution_id.clone()))
            );
        }
        let mut tampered = first.clone();
        tampered.invocation.input.value = json!({"substituted": true});
        let sealed = sealed_sidecar_bytes(
            &reopened.workspace,
            &scope(),
            "task-binding",
            &first.task_id,
            None,
            &tampered,
        )
        .unwrap();
        assert!(matches!(
            reopened
                .archive_occurrence_binding(&scope(), &tampered, &sealed)
                .await,
            Err(AppWorkflowError::TaskBindingConflict)
        ));
    }

    #[tokio::test]
    async fn recurring_preacceptance_restart_does_not_block_on_the_previous_task_root() {
        use crate::magician_v2::apps::background_behaviors::{
            AppArtifactTaskAcceptanceProbe, AppBehaviorTaskAcceptanceProbe,
        };
        let root = canonical_tempdir();
        let service = crate::magician_v2::test_support::build_test_artifact_v2_service(root.path());
        let workspace = ArtifactV2Workspace::new(root.path().join("magician_data_v3"));
        let task = binding(2);
        let mut record = super::super::tests::pristine_prebinding_task_fixture(
            &task.task_id,
            &scope(),
            &task.installation_id,
        );
        record.manifest.lifecycle = TaskLifecycle::Internal;
        record.manifest.tags.push(TaskTagRecord {
            id: "app_recurring".into(),
            name: "Recurring behavior".into(),
            color: None,
        });
        record.state.status = "completed".into();
        // The prior task pointer is deliberately not a resolvable root: this
        // probe must inspect the newly sealed occurrence, never that pointer.
        record.state.latest_root_execution_id = Some("exec_previous_occurrence".into());
        let state_path =
            workspace.task_state_path(&scope().principal(), &scope().workspace(), &task.task_id);
        std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        std::fs::write(&state_path, serde_json::to_vec(&record.state).unwrap()).unwrap();
        std::fs::write(
            workspace.task_manifest_path(&scope().principal(), &scope().workspace(), &task.task_id),
            serde_json::to_vec(&record.manifest).unwrap(),
        )
        .unwrap();
        std::fs::write(
            workspace.task_refs_path(&scope().principal(), &scope().workspace(), &task.task_id),
            serde_json::to_vec(&record.refs).unwrap(),
        )
        .unwrap();
        let workflows = AppWorkflowService::new(workspace.clone());
        let sealed = sealed_sidecar_bytes(
            &workspace,
            &scope(),
            "task-binding",
            &task.task_id,
            None,
            &task,
        )
        .unwrap();
        workflows
            .archive_occurrence_binding(&scope(), &task, &sealed)
            .await
            .unwrap();
        drop(service);
        let restarted =
            crate::magician_v2::test_support::build_test_artifact_v2_service(root.path());
        let probe = AppArtifactTaskAcceptanceProbe::new(Some(restarted), scope());
        assert!(probe
            .recurring_execution(&task.task_id)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn recurring_historical_cancellation_never_targets_the_latest_occurrence() {
        let root = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(root.path());
        let service = AppWorkflowService::new(workspace.clone());
        let first = binding(1);
        let second = binding(2);
        let record = super::super::tests::pristine_prebinding_task_fixture(
            &first.task_id,
            &scope(),
            &first.installation_id,
        );
        let path = workspace.task_manifest_path(
            &scope().principal(),
            &scope().workspace(),
            &first.task_id,
        );
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec(&record.manifest).unwrap()).unwrap();
        for task in [&first, &second] {
            let sealed = sealed_sidecar_bytes(
                &workspace,
                &scope(),
                "task-binding",
                &task.task_id,
                None,
                task,
            )
            .unwrap();
            service
                .archive_occurrence_binding(&scope(), task, &sealed)
                .await
                .unwrap();
        }
        let control = AppWorkflowRunControl {
            run_handle: app_run_handle(&first).unwrap(),
            task_id: first.task_id.clone(),
            scope: scope(),
            task: first.clone(),
        };
        let request = AppActionCancellationRequest {
            expected_generation: 0,
            idempotency_key: AppReference::parse("cancel:old-occurrence").unwrap(),
        };
        let first_execution = &first.recurring.as_ref().unwrap().execution_id;
        let second_execution = &second.recurring.as_ref().unwrap().execution_id;
        assert!(matches!(
            service
                .prepare_action_cancellation_unlocked(
                    &control,
                    second_execution,
                    &request,
                    Utc::now()
                )
                .await,
            Err(AppWorkflowError::InvalidExecutionLineage)
        ));
        let cancellation = service
            .prepare_action_cancellation_unlocked(&control, first_execution, &request, Utc::now())
            .await
            .unwrap();
        assert_eq!(
            cancellation.run_ref,
            app_run_handle(&first).unwrap().run_ref
        );
        let restarted = AppWorkflowService::new(workspace);
        let replay = restarted
            .prepare_action_cancellation_unlocked(&control, first_execution, &request, Utc::now())
            .await
            .unwrap();
        assert_eq!(replay, cancellation);
        assert!(restarted
            .action_cancellation_for_execution(&scope(), &second.task_id, second_execution)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn recurring_historical_results_remain_bound_to_their_occurrence_after_restart() {
        use super::super::tests::fixture_run_state;
        let root = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(root.path());
        let service = AppWorkflowService::new(workspace.clone());
        let first = binding(1);
        let second = binding(2);
        for task in [&first, &second] {
            let run = task.recurring.as_ref().unwrap();
            let state = fixture_run_state(task, &run.execution_id, "personal-assistant");
            let intent = AppWorkflowCommitIntent {
                output_revision: AppRevision::new(1).unwrap(),
                effect: AppWorkflowTerminalEffect::ReadOnly {
                    output: terminal_no_changes_output(),
                },
                result_produced_at: Some(Utc::now()),
                user_visible_summary: None,
                source_artifact_refs: Vec::new(),
            };
            let result = build_terminal_result(task, &state, &intent, None, Utc::now()).unwrap();
            assert_eq!(result.run_ref, app_run_handle(task).unwrap().run_ref);
            let record = AppWorkflowTaskResultRecord {
                native_round: None,
                schema: APP_TASK_RESULT_SCHEMA.into(),
                task_id: task.task_id.clone(),
                execution_id: state.execution_id,
                output_revision: AppRevision::new(1).unwrap(),
                result,
            };
            std::fs::create_dir_all(task.result_path(&workspace, &scope()).parent().unwrap())
                .unwrap();
            service
                .persist_task_result(&scope(), task, record)
                .await
                .unwrap();
        }
        let restarted = AppWorkflowService::new(workspace.clone());
        for task in [&first, &second] {
            let result = restarted
                .read_task_result(&scope(), task)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(result.result.run_ref, app_run_handle(task).unwrap().run_ref);
        }
        std::fs::copy(
            first.result_path(&workspace, &scope()),
            second.result_path(&workspace, &scope()),
        )
        .unwrap();
        assert!(restarted.read_task_result(&scope(), &second).await.is_err());
    }

    #[tokio::test]
    async fn recurring_cleanup_refuses_live_and_recurring_tasks_and_preserves_unrelated_data() {
        use crate::magician_v2::artifact_v2::models::{
            TaskLifecycle, TaskOutputMode, TaskSyncMode, TaskTagRecord,
        };
        let root = canonical_tempdir();
        let service = crate::magician_v2::test_support::build_test_artifact_v2_service(root.path());
        let workspace = ArtifactV2Workspace::new(root.path().join("magician_data_v3"));
        let mut legacy = binding(1);
        legacy.task_id = format!("task_app_{}", "d".repeat(64));
        legacy.recurring = None;
        let recurring = binding(2);
        for task in [&legacy, &recurring] {
            let mut tags = vec![TaskTagRecord {
                id: "app_workflow".into(),
                name: "App workflow".into(),
                color: None,
            }];
            if task.recurring.is_some() {
                tags.push(TaskTagRecord {
                    id: "app_recurring".into(),
                    name: "Recurring app behavior".into(),
                    color: None,
                });
            }
            service
                .ensure_app_workflow_task_with_id(
                    CreateTaskInput {
                        principal: "anonymous".into(),
                        workspace: "default".into(),
                        title: "Scheduled fixture".into(),
                        description: "Recurring cleanup test".into(),
                        agent_id: "personal-assistant".into(),
                        goal_id: None,
                        ui_thread_id: format!("app:{}", task.installation_id),
                        priority: None,
                        due_date: None,
                        tags,
                        created_by: "app_action".into(),
                        depends_on: Vec::new(),
                        approved: true,
                        schedule: None,
                        output_mode: TaskOutputMode::Overwrite,
                        chat_session_id: None,
                        lifecycle: TaskLifecycle::Internal,
                        sync_mode: TaskSyncMode::Deferred,
                    },
                    task.task_id.clone(),
                )
                .await
                .unwrap();
        }
        let control = |task: &AppWorkflowTaskBinding| AppWorkflowRunControl {
            run_handle: app_run_handle(task).unwrap(),
            task_id: task.task_id.clone(),
            scope: scope(),
            task: task.clone(),
        };
        assert!(service
            .delete_legacy_scheduled_app_task(&control(&recurring))
            .await
            .is_err());
        // The not-yet-terminal legacy shell must also be retained.
        assert!(service
            .delete_legacy_scheduled_app_task(&control(&legacy))
            .await
            .is_err());
        let path =
            workspace.task_state_path(&scope().principal(), &scope().workspace(), &legacy.task_id);
        let mut state: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        state["status"] = json!("completed");
        std::fs::write(path, serde_json::to_vec(&state).unwrap()).unwrap();
        let app_data = root.path().join("retained-app-records.json");
        std::fs::write(&app_data, b"original posts").unwrap();
        service
            .delete_legacy_scheduled_app_task(&control(&legacy))
            .await
            .unwrap();
        assert!(service.get_task(&scope(), &legacy.task_id).await.is_err());
        assert!(service.get_task(&scope(), &recurring.task_id).await.is_ok());
        assert_eq!(std::fs::read(app_data).unwrap(), b"original posts");
    }

    #[tokio::test]
    async fn recurring_preexecution_result_read_does_not_create_or_require_an_execution() {
        let root = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(root.path());
        let task = binding(1);
        let scope = scope();
        let home = workspace.task_dir(&scope.principal(), &scope.workspace(), &task.task_id);
        std::fs::create_dir_all(home).unwrap();
        let result_path = task.result_path(&workspace, &scope);
        let service = AppWorkflowService::new(workspace);
        assert!(service
            .read_task_result(&scope, &task)
            .await
            .unwrap()
            .is_none());
        assert!(!result_path.parent().unwrap().exists());
    }

    #[tokio::test]
    async fn recurring_partial_archive_publication_recovers_exact_original_binding() {
        let root = canonical_tempdir();
        let workspace = ArtifactV2Workspace::new(root.path());
        let task = binding(1);
        let run = task.recurring.as_ref().unwrap();
        let service = AppWorkflowService::new(workspace.clone());
        let sealed = sealed_sidecar_bytes(
            &workspace,
            &scope(),
            "task-binding",
            &task.task_id,
            None,
            &task,
        )
        .unwrap();
        service
            .registry
            .publish_workflow_control_generation(
                app_scope_from_scope_ref(&scope()).unwrap(),
                task.task_id.clone(),
                run.occurrence_id.clone(),
                AppWorkflowControlKind::TaskBinding,
                sealed.clone(),
                Utc::now(),
            )
            .await
            .unwrap();
        let restarted = AppWorkflowService::new(workspace);
        let retained = restarted
            .read_occurrence_binding(&scope(), &task.task_id, &run.occurrence_id)
            .await
            .unwrap()
            .unwrap();
        restarted
            .archive_occurrence_binding(&scope(), &retained, &sealed)
            .await
            .unwrap();
        assert_eq!(
            restarted
                .recurring_occurrence_locator(&scope(), &run.occurrence_id)
                .await
                .unwrap(),
            Some((task.task_id.clone(), run.execution_id.clone()))
        );
    }
}
