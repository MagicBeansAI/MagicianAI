//! Independent native progress commits inside the workflow's existing owner.
//! These slots do not publish task completion and never grant mutation rights.
use super::*;

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct AppNativeCommitIdentity {
    node_binding_digest: AppDigest,
    participant_ref: AppReference,
}

/// Runtime-only key constructed by the native recipe owner, not from tool
/// arguments. The retained identity is revalidated with the sealed run state.
pub(super) struct AppNativeCommitKey {
    identity: AppNativeCommitIdentity,
}

impl AppNativeCommitKey {
    pub(super) fn for_participant(
        binding: &super::super::recipe_lowering::AppRecipeLoweredNodeBinding,
        participant_ref: AppReference,
    ) -> Result<Self, AppWorkflowError> {
        if !binding.permits_workflow_mutation() {
            return Err(AppWorkflowError::NativeProgressEffectDenied);
        }
        Ok(Self {
            identity: AppNativeCommitIdentity {
                node_binding_digest: binding.binding_digest().clone(),
                participant_ref,
            },
        })
    }
}

pub(super) struct AppNativeProgressContext<'a> {
    pub run: &'a AppRecipeAttachedRun,
    pub node: &'a AppName,
    pub permit: &'a AppRecipeCanonicalNodePermit,
    pub owner: &'a dyn AppRecipeCanonicalNodeOwner,
}

impl AppNativeProgressContext<'_> {
    pub(super) async fn fence_under_task_guard(
        &self,
        scope: &ScopeRef,
        task_id: &str,
        guard: &AppWorkflowTaskGuard,
    ) -> Result<(), AppWorkflowError> {
        self.owner
            .fence_recipe_node_output_under_task_guard(
                scope,
                task_id,
                self.run,
                self.node,
                self.permit,
                guard,
            )
            .await
    }

    pub(super) async fn fence(
        &self,
        scope: &ScopeRef,
        task_id: &str,
    ) -> Result<(), AppWorkflowError> {
        self.owner
            .fence_recipe_node_output(scope, task_id, self.run, self.node, self.permit)
            .await
    }
}

impl AppWorkflowService {
    /// Native callers supply their canonical node claim as well as their
    /// current App task. This is not registered as a model-callable tool.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn commit_native_progress(
        &self,
        scope: &ScopeRef,
        task: &AppWorkflowTaskBinding,
        context: &AppNativeProgressContext<'_>,
        participant_ref: AppReference,
        parameters: &HashMap<String, Value>,
    ) -> Result<AppActionResult<Value>, AppWorkflowError> {
        context.fence(scope, &task.task_id).await?;
        let binding = context
            .run
            .plan()
            .node(context.node)
            .ok_or(AppWorkflowError::RecipeBindingUnavailable)?;
        let key = AppNativeCommitKey::for_participant(binding, participant_ref)?;
        Box::pin(self.commit_workflow_effect_inner(
            scope,
            &task.task_id,
            context.run.execution_id(),
            task.resolved_agent_id.as_str(),
            parameters,
            Utc::now(),
            false,
            AppWorkflowCommitTarget::native(&key),
            Some(context),
        ))
        .await
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct AppWorkflowNativeCommit {
    identity: AppNativeCommitIdentity,
    output_revision: AppRevision,
    attempt_generation: u32,
    intent: Option<AppWorkflowCommitIntent>,
    result: Option<AppActionResult<Value>>,
}

#[derive(Clone)]
pub(super) enum AppWorkflowCommitTarget {
    Terminal,
    Native(AppNativeCommitIdentity),
}

impl AppWorkflowCommitTarget {
    pub(super) fn native(key: &AppNativeCommitKey) -> Self {
        Self::Native(key.identity.clone())
    }
    pub(super) fn terminal(&self) -> bool {
        matches!(self, Self::Terminal)
    }
    fn id(&self) -> Result<AppReference, AppWorkflowError> {
        let Self::Native(identity) = self else {
            return Err(AppWorkflowError::CorruptBinding);
        };
        let digest = AppDigest::blake3_canonical_json(&serde_json::to_value(identity)?)?;
        Ok(AppReference::parse(format!(
            "native-commit:{}",
            digest.as_str().trim_start_matches("blake3:")
        ))?)
    }
    pub(super) fn prepare(&self, state: &mut AppWorkflowRunState) -> Result<(), AppWorkflowError> {
        let Self::Native(identity) = self else {
            return Ok(());
        };
        let id = self.id()?;
        if let Some(existing) = state.native_commits.get(&id) {
            if &existing.identity != identity {
                return Err(AppWorkflowError::CorruptBinding);
            }
        } else {
            // Revision one remains the workflow's terminal output. Progress
            // slots never disappear, so concurrent members cannot reuse a revision.
            let revision = u64::try_from(state.native_commits.len())
                .ok()
                .and_then(|count| count.checked_add(2))
                .ok_or(AppWorkflowError::TerminalAttemptOverflow)?;
            state.native_commits.insert(
                id,
                AppWorkflowNativeCommit {
                    identity: identity.clone(),
                    output_revision: AppRevision::new(revision)?,
                    attempt_generation: 0,
                    intent: None,
                    result: None,
                },
            );
        }
        Ok(())
    }
    fn slot<'a>(
        &self,
        state: &'a AppWorkflowRunState,
    ) -> Result<&'a AppWorkflowNativeCommit, AppWorkflowError> {
        state
            .native_commits
            .get(&self.id()?)
            .ok_or(AppWorkflowError::CorruptBinding)
    }
    fn slot_mut<'a>(
        &self,
        state: &'a mut AppWorkflowRunState,
    ) -> Result<&'a mut AppWorkflowNativeCommit, AppWorkflowError> {
        state
            .native_commits
            .get_mut(&self.id()?)
            .ok_or(AppWorkflowError::CorruptBinding)
    }
    pub(super) fn intent<'a>(
        &self,
        state: &'a AppWorkflowRunState,
    ) -> Result<Option<&'a AppWorkflowCommitIntent>, AppWorkflowError> {
        Ok(match self {
            Self::Terminal => state.commit_intent.as_ref(),
            Self::Native(_) => self.slot(state)?.intent.as_ref(),
        })
    }
    pub(super) fn result<'a>(
        &self,
        state: &'a AppWorkflowRunState,
    ) -> Result<Option<&'a AppActionResult<Value>>, AppWorkflowError> {
        Ok(match self {
            Self::Terminal => state.result.as_ref(),
            Self::Native(_) => self.slot(state)?.result.as_ref(),
        })
    }
    pub(super) fn retained_result<'a>(
        &self,
        state: &'a AppWorkflowRunState,
    ) -> Result<Option<&'a AppActionResult<Value>>, AppWorkflowError> {
        match self {
            Self::Terminal => Ok(state.result.as_ref()),
            Self::Native(_) => Ok(state
                .native_commits
                .get(&self.id()?)
                .and_then(|slot| slot.result.as_ref())),
        }
    }
    pub(super) fn retained_intent<'a>(
        &self,
        state: &'a AppWorkflowRunState,
    ) -> Result<Option<&'a AppWorkflowCommitIntent>, AppWorkflowError> {
        match self {
            Self::Terminal => Ok(state.commit_intent.as_ref()),
            Self::Native(_) => Ok(state
                .native_commits
                .get(&self.id()?)
                .and_then(|slot| slot.intent.as_ref())),
        }
    }
    pub(super) fn set_intent(
        &self,
        state: &mut AppWorkflowRunState,
        intent: AppWorkflowCommitIntent,
    ) -> Result<(), AppWorkflowError> {
        match self {
            Self::Terminal => state.commit_intent = Some(intent),
            Self::Native(_) => self.slot_mut(state)?.intent = Some(intent),
        }
        Ok(())
    }
    pub(super) fn set_result(
        &self,
        state: &mut AppWorkflowRunState,
        result: AppActionResult<Value>,
    ) -> Result<(), AppWorkflowError> {
        match self {
            Self::Terminal => state.result = Some(result),
            Self::Native(_) => self.slot_mut(state)?.result = Some(result),
        }
        Ok(())
    }
    pub(super) fn output_revision(
        &self,
        state: &AppWorkflowRunState,
    ) -> Result<AppRevision, AppWorkflowError> {
        match self {
            Self::Terminal => Ok(AppRevision::new(1)?),
            Self::Native(_) => Ok(self.slot(state)?.output_revision),
        }
    }
    pub(super) fn generation(&self, state: &AppWorkflowRunState) -> Result<u32, AppWorkflowError> {
        Ok(match self {
            Self::Terminal => state.terminal_attempt_generation,
            Self::Native(_) => self.slot(state)?.attempt_generation,
        })
    }
    pub(super) fn clear_proven_unspent(
        &self,
        state: &mut AppWorkflowRunState,
    ) -> Result<(), AppWorkflowError> {
        if self.result(state)?.is_some() {
            return Err(AppWorkflowError::CommitIntentConflict);
        }
        let next = self
            .generation(state)?
            .checked_add(1)
            .ok_or(AppWorkflowError::TerminalAttemptOverflow)?;
        match self {
            Self::Terminal => {
                state.commit_intent = None;
                state.terminal_attempt_generation = next;
            },
            Self::Native(_) => {
                let slot = self.slot_mut(state)?;
                slot.intent = None;
                slot.attempt_generation = next;
            },
        }
        Ok(())
    }
    pub(super) fn invocation_ref(
        &self,
        state: &AppWorkflowRunState,
        intent_digest: &AppDigest,
    ) -> Result<String, AppWorkflowError> {
        let terminal = terminal_invocation_ref(
            state.execution_id.as_str(),
            self.generation(state)?,
            intent_digest,
        );
        Ok(if self.terminal() {
            terminal
        } else {
            format!("native-progress:{}:{terminal}", self.id()?)
        })
    }
    pub(super) fn mutation_reference(
        &self,
        task: &AppWorkflowTaskBinding,
        revision: AppRevision,
    ) -> Result<AppReference, AppWorkflowError> {
        if self.terminal() {
            return terminal_mutation_reference(task, revision);
        }
        let digest = AppDigest::blake3_canonical_json(&json!({
            "task_id": task.task_id, "invocation_idempotency_key": task.invocation.idempotency_key,
            "native_commit": self.id()?, "output_revision": revision,
        }))?;
        Ok(AppReference::parse(format!(
            "app-mutation:{}",
            digest.as_str().trim_start_matches("blake3:")
        ))?)
    }
}

pub(super) fn pending_native_commits(state: &AppWorkflowRunState) -> Vec<AppWorkflowCommitTarget> {
    state
        .native_commits
        .values()
        .filter(|slot| slot.intent.is_some() && slot.result.is_none())
        .map(|slot| AppWorkflowCommitTarget::Native(slot.identity.clone()))
        .collect()
}

pub(super) fn validate_native_commits(
    task: &AppWorkflowTaskBinding,
    state: &AppWorkflowRunState,
) -> Result<(), AppWorkflowError> {
    if !state.native_commits.is_empty() && task.recipe_binding.is_none() {
        return Err(AppWorkflowError::CorruptBinding);
    }
    let mut revisions = BTreeSet::new();
    for (key, slot) in &state.native_commits {
        let target = AppWorkflowCommitTarget::Native(slot.identity.clone());
        if &target.id()? != key
            || slot.output_revision.get() < 2
            || !revisions.insert(slot.output_revision)
        {
            return Err(AppWorkflowError::CorruptBinding);
        }
        if let Some(intent) = &slot.intent {
            let AppWorkflowTerminalEffect::Mutation { command } = &intent.effect else {
                return Err(AppWorkflowError::CorruptBinding);
            };
            if intent.output_revision != slot.output_revision
                || command.operations.is_empty()
                || command.idempotency_key
                    != target.mutation_reference(task, slot.output_revision)?
                || command.expected_schema_revision != state.run_binding.schema_revision
                || command.atomicity != AppMutationAtomicity::AllOrNothing
            {
                return Err(AppWorkflowError::CorruptBinding);
            }
            command.validate_app_contract(&AppContractLimits::default())?;
        }
        if let Some(result) = &slot.result {
            if slot.intent.is_none()
                || result.mutation_receipt_refs.is_empty()
                || result.run_ref != app_run_handle(task)?.run_ref
                || result.action_id != task.invocation.action_id
                || result.status != AppActionStatus::Completed
            {
                return Err(AppWorkflowError::CorruptBinding);
            }
            result.validate_app_contract(&AppContractLimits::default())?;
            let output = result
                .output
                .as_ref()
                .ok_or(AppWorkflowError::CorruptBinding)?;
            if output.value_schema_ref.as_str() != NATIVE_PROGRESS_RECEIPT_SCHEMA
                || output.source != AppDataSource::AppAction
                || output.installation_id != task.installation_id
                || output.package_revision_ref != task.package_revision_ref
                || output.schema_revision != state.run_binding.schema_revision
                || output.scope_binding_ref != task.invocation.input.scope_binding_ref
                || output.content_digest != AppDigest::blake3_canonical_json(&output.value)?
                || result.mutation_receipt_refs.len() != 1
                || output
                    .value
                    .get("committed_record_revisions")
                    .and_then(Value::as_array)
                    .is_none_or(Vec::is_empty)
                || output.value.get("receipt_ref")
                    != Some(&serde_json::to_value(&result.mutation_receipt_refs[0])?)
            {
                return Err(AppWorkflowError::CorruptBinding);
            }
        }
    }
    // Allocation uses the retained slot count. A forged gap must never allow
    // the next participant to collide with an existing workflow origin.
    if revisions
        .iter()
        .enumerate()
        .any(|(index, revision)| revision.get() != index as u64 + 2)
    {
        return Err(AppWorkflowError::CorruptBinding);
    }
    Ok(())
}

pub(super) const NATIVE_PROGRESS_RECEIPT_SCHEMA: &str = "schema:app-workflow-progress-receipt:v1";

#[cfg(test)]
mod tests {
    use super::super::tests::{fixture_run_state, fixture_task};
    use super::*;

    fn fixture() -> (AppWorkflowTaskBinding, AppWorkflowRunState) {
        let mut task = fixture_task();
        let (_, lock) = super::super::super::package_lock::tests::locked_native_recipe_fixture();
        task.workflow_id = AppName::parse("build").unwrap();
        task.recipe_binding = lock.recipe_for_workflow(&task.workflow_id).cloned();
        assert!(task.recipe_binding.is_some());
        let state = fixture_run_state(&task, "exec_root_1", "personal-assistant");
        (task, state)
    }

    // Test retained identities directly; production obtains the key only from
    // the canonical lowered node and its live claim, never this constructor.
    fn target(participant: &str) -> AppWorkflowCommitTarget {
        AppWorkflowCommitTarget::Native(AppNativeCommitIdentity {
            node_binding_digest: AppDigest::blake3(b"test-node"),
            participant_ref: AppReference::parse(participant).unwrap(),
        })
    }

    fn intent(
        target: &AppWorkflowCommitTarget,
        task: &AppWorkflowTaskBinding,
        state: &AppWorkflowRunState,
    ) -> AppWorkflowCommitIntent {
        let output_revision = target.output_revision(state).unwrap();
        AppWorkflowCommitIntent {
            output_revision,
            effect: AppWorkflowTerminalEffect::Mutation {
                command: AppMutationCommand {
                    protocol_version: AppProtocolVersion::V1,
                    idempotency_key: target.mutation_reference(task, output_revision).unwrap(),
                    atomicity: AppMutationAtomicity::AllOrNothing,
                    expected_schema_revision: state.run_binding.schema_revision,
                    operations: vec![AppMutationOperation::Delete {
                        entity: AppName::parse("note").unwrap(),
                        record_id: super::super::super::models::AppRecordId::parse("record-1")
                            .unwrap(),
                    }],
                    expected_record_revisions: serde_json::from_value(json!([
                        {"entity":"note", "record_id":"record-1", "revision":1}
                    ]))
                    .unwrap(),
                },
            },
            result_produced_at: Some("2026-09-09T12:00:00Z".parse().unwrap()),
            user_visible_summary: None,
            source_artifact_refs: Vec::new(),
        }
    }

    #[test]
    fn member_retry_and_recovery_keep_other_members_and_terminal_identity() {
        let (task, mut state) = fixture();
        assert!(serde_json::to_value(&state)
            .unwrap()
            .get("native_commits")
            .is_none());
        let a = target("member:a");
        let b = target("member:b");
        a.prepare(&mut state).unwrap();
        b.prepare(&mut state).unwrap();
        let a_intent = intent(&a, &task, &state);
        let b_intent = intent(&b, &task, &state);
        a.set_intent(&mut state, a_intent.clone()).unwrap();
        b.set_intent(&mut state, b_intent.clone()).unwrap();
        validate_native_commits(&task, &state).unwrap();
        let digest = AppDigest::blake3(b"request");
        let a_attempt = a.invocation_ref(&state, &digest).unwrap();
        let b_attempt = b.invocation_ref(&state, &digest).unwrap();
        assert_ne!(a_attempt, b_attempt);
        let terminal = AppWorkflowCommitTarget::Terminal;
        assert_eq!(terminal.output_revision(&state).unwrap().get(), 1);
        assert_eq!(
            terminal
                .mutation_reference(&task, AppRevision::new(1).unwrap())
                .unwrap(),
            terminal_mutation_reference(&task, AppRevision::new(1).unwrap()).unwrap()
        );
        assert_eq!(
            terminal.invocation_ref(&state, &digest).unwrap(),
            terminal_invocation_ref(state.execution_id.as_str(), 0, &digest)
        );

        // Reopen the durable state, then acknowledge one proven-unspent attempt.
        let mut recovered: AppWorkflowRunState =
            serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
        a.clear_proven_unspent(&mut recovered).unwrap();
        a.prepare(&mut recovered).unwrap();
        assert_eq!(
            a.output_revision(&recovered).unwrap(),
            a_intent.output_revision
        );
        assert!(a.intent(&recovered).unwrap().is_none());
        assert!(b.intent(&recovered).unwrap() == Some(&b_intent));
        assert_ne!(a.invocation_ref(&recovered, &digest).unwrap(), a_attempt);
        assert_eq!(b.invocation_ref(&recovered, &digest).unwrap(), b_attempt);
        assert_eq!(recovered.terminal_attempt_generation, 0);
        assert!(recovered.commit_intent.is_none() && recovered.result.is_none());
        assert_eq!(pending_native_commits(&recovered).len(), 1);
        validate_native_commits(&task, &recovered).unwrap();
    }

    #[test]
    fn retained_member_slots_reject_revision_and_command_substitution() {
        let (task, mut state) = fixture();
        let a = target("member:a");
        let b = target("member:b");
        a.prepare(&mut state).unwrap();
        b.prepare(&mut state).unwrap();
        let a_intent = intent(&a, &task, &state);
        let b_intent = intent(&b, &task, &state);
        a.set_intent(&mut state, a_intent.clone()).unwrap();
        b.set_intent(&mut state, b_intent.clone()).unwrap();
        let encoded = serde_json::to_value(&state).unwrap();
        let fresh = || serde_json::from_value::<AppWorkflowRunState>(encoded.clone()).unwrap();
        validate_native_commits(&task, &state).unwrap();

        let mut corrupt = fresh();
        b.slot_mut(&mut corrupt).unwrap().output_revision = a_intent.output_revision;
        assert!(validate_native_commits(&task, &corrupt).is_err());
        let mut corrupt = fresh();
        b.slot_mut(&mut corrupt).unwrap().output_revision = AppRevision::new(4).unwrap();
        b.slot_mut(&mut corrupt).unwrap().intent = None;
        assert!(validate_native_commits(&task, &corrupt).is_err());
        let mut corrupt = fresh();
        b.set_intent(&mut corrupt, a_intent).unwrap();
        assert!(validate_native_commits(&task, &corrupt).is_err());
        let mut corrupt = fresh();
        let AppWorkflowTerminalEffect::Mutation { command } = &mut b
            .slot_mut(&mut corrupt)
            .unwrap()
            .intent
            .as_mut()
            .unwrap()
            .effect
        else {
            panic!("mutation fixture");
        };
        command.expected_schema_revision = AppRevision::new(2).unwrap();
        assert!(validate_native_commits(&task, &corrupt).is_err());
        let mut corrupt = fresh();
        b.slot_mut(&mut corrupt).unwrap().identity.participant_ref =
            AppReference::parse("member:substitute").unwrap();
        assert!(validate_native_commits(&task, &corrupt).is_err());
        let mut no_recipe = task.clone();
        no_recipe.recipe_binding = None;
        assert!(validate_native_commits(&no_recipe, &state).is_err());
    }

    #[test]
    fn progress_receipt_has_its_own_schema_and_retains_reviewed_policy_floors() {
        let (mut task, state) = fixture();
        task.result_value_schema = Some(serde_json::from_value(json!({
            "type":"object", "fields": {"summary": {"type":"text", "required":true,
                "data_policy":{"classification_floor":"secret","model_processing":"local_only"}}}
        })).unwrap());
        let receipt_value = json!({"receipt_ref":"receipt:member-a", "committed_record_revisions":[], "change_sequence":null});
        let at = "2026-09-09T12:00:00Z".parse().unwrap();
        assert!(app_result_envelope(&task, &state, receipt_value.clone(), None, &[], at).is_err());
        let output =
            workflow_commit_envelope(&task, &state, receipt_value.clone(), None, &[], at, false)
                .unwrap();
        assert_eq!(
            output.value_schema_ref.as_str(),
            NATIVE_PROGRESS_RECEIPT_SCHEMA
        );
        assert_eq!(output.value, receipt_value);
        assert_eq!(
            output.handling_labels.classification,
            AppDataClassification::Secret
        );
        assert_eq!(
            output.handling_labels.model_processing,
            AppModelProcessing::LocalOnly
        );
        assert_ne!(
            output.value_schema_ref,
            task.invocation.requested_result_schema_ref
        );
    }

    #[test]
    fn committed_member_is_replayable_without_completing_the_workflow() {
        let (task, mut state) = fixture();
        let a = target("member:a");
        let b = target("member:b");
        a.prepare(&mut state).unwrap();
        b.prepare(&mut state).unwrap();
        let a_intent = intent(&a, &task, &state);
        let b_intent = intent(&b, &task, &state);
        a.set_intent(&mut state, a_intent.clone()).unwrap();
        b.set_intent(&mut state, b_intent).unwrap();
        // The real I/O path obtains this structure exclusively from the
        // mutation owner; this fixture exercises retained result semantics.
        let receipt: AppMutationReceipt = serde_json::from_value(json!({
            "receipt_id":"receipt:member-a", "installation_id":task.installation_id,
            "origin":{"kind":"workflow", "execution_id":state.execution_id,
                "output_revision":a_intent.output_revision},
            "mutation_key":AppDigest::blake3(b"mutation"),
            "batch_digest":AppDigest::blake3(b"batch"),
            "committed_record_revisions":[{"entity":"note", "record_id":"record-1", "revision":2}],
            "change_seq_range":{"first":1,"last":1},
            "committed_at":"2026-09-09T12:00:00Z"
        }))
        .unwrap();
        let result = build_workflow_commit_result(
            &task,
            &state,
            &a_intent,
            Some(&receipt),
            false,
            receipt.committed_at,
        )
        .unwrap();
        a.set_result(&mut state, result.clone()).unwrap();
        validate_native_commits(&task, &state).unwrap();
        let mut recovered: AppWorkflowRunState =
            serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
        assert!(a.result(&recovered).unwrap() == Some(&result));
        assert!(recovered.result.is_none());
        assert_eq!(pending_native_commits(&recovered).len(), 1);
        assert!(a.clear_proven_unspent(&mut recovered).is_err());
        validate_native_commits(&task, &recovered).unwrap();
        let mut forged = result;
        forged.mutation_receipt_refs[0] = AppReference::parse("receipt:another").unwrap();
        a.set_result(&mut recovered, forged).unwrap();
        assert!(validate_native_commits(&task, &recovered).is_err());
    }
}

pub(super) fn native_receipt_refs(state: &AppWorkflowRunState) -> Vec<AppReference> {
    state
        .native_commits
        .values()
        .filter_map(|slot| slot.result.as_ref())
        .flat_map(|result| result.mutation_receipt_refs.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
