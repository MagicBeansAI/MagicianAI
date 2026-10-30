// Exact ordinary root-owner receipt recovery regressions.
use super::*;

#[cfg(test)]
async fn root_owner_pause_fixture(
    root: &std::path::Path,
) -> (
    Arc<ArtifactV2Service>,
    Arc<crate::magician_v2::execution::agentic::FullPauseStore>,
    crate::magician_v2::execution::agentic::RootOwnerPauseReceiptBinding,
) {
    root_owner_pause_fixture_with_review_rules(root, Vec::new()).await
}

async fn root_owner_pause_fixture_with_review_rules(
    root: &std::path::Path,
    review_rules: Vec<crate::magician_v2::agents::types::ApprovalRule>,
) -> (
    Arc<ArtifactV2Service>,
    Arc<crate::magician_v2::execution::agentic::FullPauseStore>,
    crate::magician_v2::execution::agentic::RootOwnerPauseReceiptBinding,
) {
    use crate::magician_v2::execution::agentic::{
        run_loop::{
            journal::{JournalAppend, RecordedStep, TerminalKind},
            outcome::Phase,
            state::{
                LoopSegmentBinding, LoopState, RunIdentity, TerminalOutcomeProjection,
                TerminalSettlementDescriptor,
            },
            store::{fs::FsLoopStateStore, ExecutionKey, LoopStateStore, Revision},
        },
        AgenticPauseState, EnvironmentState, ExecutionPauseKind, FullPauseData, FullPauseStore,
        RootOwnerPauseReceiptBinding, UserInputType,
    };
    let (service, orchestrator) =
        crate::magician_v2::test_support::build_test_artifact_v2_harness_with_router(root);
    let runtime_root = service.workspace.base_root().to_path_buf();
    let scope = ScopeRef::system_internal_unauthenticated("anonymous", "default");
    let (mut task, mut execution) = service
        .create_task_with_execution_shell(make_create_input(
            "anonymous",
            "default",
            "Root receipt continuation",
            "test",
        ))
        .await
        .unwrap();
    let task_id = task.manifest.task_id.clone();
    let execution_id = execution.state.execution_id.clone();
    orchestrator
        .create_execution_with_id(
            "anonymous",
            "default",
            None,
            "personal-assistant",
            &execution_id,
            Some(task_id.clone()),
            Some(execution_id.clone()),
        )
        .await
        .unwrap();
    orchestrator
        .conversation_store()
        .update_execution_status(
            &execution_id,
            crate::magician_v2::storage::models::WaitingState::WaitingUser,
        )
        .await
        .unwrap();
    execution.state.status = "waiting_for_user".into();
    service
        .workspace
        .write_json_atomic_path(
            &service.workspace.execution_state_path(
                "anonymous",
                "default",
                &task_id,
                &execution_id,
            ),
            &execution.state,
        )
        .await
        .unwrap();
    task.state.status = "paused".into();
    task.state.active_root_execution_id = Some(execution_id.clone());
    service.persist_task_record(&scope, &task).await.unwrap();

    let mut pause = AgenticPauseState::new(
        1,
        "Ask then continue",
        "Reply recorded",
        EnvironmentState::Uninitialized,
        "",
        10,
        3,
    );
    pause.principal = Some("anonymous".into());
    pause.workspace = Some("default".into());
    pause.task_id = Some(task_id.clone());
    pause.execution_id = Some(execution_id.clone());
    pause.plan_id = Some("root-receipt-plan".into());
    pause.step_id = Some("input".into());
    pause.root_execution_id = Some(execution_id.clone());
    pause.stateless_parked_segment = Some(execution_id.clone());
    pause.agent_id = Some("personal-assistant".into());
    pause.active_owner_agent_id = pause.agent_id.clone();
    pause.approval_rules = review_rules;
    pause.max_spawned_tasks = Some(0);
    let executors = orchestrator.build_action_executors_for_resume(
        None,
        Some(&execution_id),
        Some("anonymous"),
        Some("default"),
    );
    let writer = FullPauseStore::with_scoped_v3_persistence_root(&runtime_root);
    let commit = writer
        .store_durable_with_authority(
            FullPauseData::new(
                pause,
                UserInputType::Text {
                    placeholder: None,
                    multiline: false,
                },
                executors,
                Some("Enter the continuation marker".into()),
                None,
            )
            .with_pause_kind(ExecutionPauseKind::UserInput),
        )
        .unwrap();
    let key = ExecutionKey::new("anonymous", "default", &execution_id).unwrap();
    let loop_store = FsLoopStateStore::new(&runtime_root);
    let seq = loop_store
        .append_journal(
            &key,
            &[JournalAppend::phase_completed(
                1,
                Phase::Prepare,
                RecordedStep::RunEnded {
                    terminal: TerminalKind::WaitingForUser,
                },
            )],
        )
        .await
        .unwrap();
    let mut state = LoopState::new(RunIdentity {
        task_id: Some(task_id.clone()),
        execution_id: Some(execution_id.clone()),
        agent_id: Some("personal-assistant".into()),
        principal: Some("anonymous".into()),
        workspace: Some("default".into()),
        max_spawned_tasks: Some(0),
        ..Default::default()
    });
    state.segment_binding = Some(LoopSegmentBinding {
        base_execution_id: execution_id.clone(),
        exact_segment_id: execution_id.clone(),
        preseed_admission_token: None,
    });
    state.journal_seq = seq;
    let replayed = loop_store
        .replay_committed_journal(&key, seq)
        .await
        .unwrap();
    state.cursor.iteration = replayed.iteration;
    state.cursor.phase = replayed.phase;
    state.terminal_settlement_receipt = Some(
        service
            .seal_terminal_settlement_descriptor(TerminalSettlementDescriptor {
                schema_version: 1,
                principal: "anonymous".into(),
                workspace: "default".into(),
                task_id: task_id.clone(),
                base_execution_id: execution_id.clone(),
                exact_segment_id: execution_id.clone(),
                terminal_kind: "waiting_for_user".into(),
                terminal_seq: seq,
                outcome: TerminalOutcomeProjection {
                    execution_status: "waiting_for_user".into(),
                    task_status: "paused".into(),
                    outcome_type: "waiting_for_user".into(),
                    outcome_summary: "Enter the continuation marker".into(),
                    iterations_used: Some(1),
                    is_terminal: false,
                    completion_kind: None,
                    open_items: vec![],
                },
                waiting_for_user_diff_approval: false,
                pipeline_loop_state_segment: None,
                pipeline_stage_settlement: None,
                pause_key: Some(commit.key.clone()),
                pause_revision: Some(commit.revision.clone()),
                pause_body_sha256: Some(commit.body_sha256.clone()),
                pause_resume_segment: Some(format!("{execution_id}-r1")),
                refinement_successor_segment: None,
            })
            .unwrap(),
    );
    loop_store
        .commit(&key, &state, Revision::INITIAL)
        .await
        .unwrap();
    assert!(writer
        .activate_exact_stateless_terminal_pause("anonymous", "default", &execution_id, &commit,)
        .unwrap());
    let binding = RootOwnerPauseReceiptBinding {
        principal: "anonymous".into(),
        workspace: "default".into(),
        task_id,
        agent_id: "personal-assistant".into(),
        exact_segment_id: execution_id.clone(),
        execution_id,
        pause_key: commit.key,
        revision: commit.revision,
        body_sha256: commit.body_sha256,
        continuation_hash: blake3::hash(b"unused by receipt verification"),
    };
    drop(writer);
    (
        service,
        Arc::new(FullPauseStore::with_scoped_v3_persistence_root(
            runtime_root,
        )),
        binding,
    )
}

#[tokio::test]
async fn root_owner_pause_receipt_recovers_cold_exact_body_without_widening() {
    use crate::magician_v2::execution::{
        agentic::app_pause_serializable_continuation_bytes,
        trusted_store::{
            install_process_authority, AuthorityEntry, StoreKind, TrustAuthority, TrustedRecordKey,
        },
    };
    let authority = install_process_authority(Arc::new(TrustAuthority::new()));
    let tmp = tempfile::TempDir::new().unwrap();
    let (service, cold, binding) = root_owner_pause_fixture(tmp.path()).await;
    let key = TrustedRecordKey {
        kind: StoreKind::PauseState,
        principal: binding.principal.clone(),
        workspace: binding.workspace.clone(),
        id: binding.pause_key.clone(),
    };
    authority.forget(&key);
    assert!(cold
        .recover_root_owner_pause_authority_from_receipt(
            &binding.pause_key,
            "anonymous",
            "default",
            &binding.execution_id,
            None
        )
        .await
        .is_err());
    assert!(cold
        .load_serializable_by_key_from_disk_scoped(&binding.pause_key, "anonymous", "default")
        .unwrap()
        .is_some());
    assert!(cold
        .recover_root_owner_pause_authority_from_receipt(
            &binding.pause_key,
            "anonymous",
            "default",
            "exec_wrong_request",
            Some(Arc::clone(&service))
        )
        .await
        .is_err());
    assert!(authority.get(&key).is_none());
    assert!(cold
        .recover_root_owner_pause_authority_from_receipt(
            &binding.pause_key,
            "anonymous",
            "default",
            &binding.execution_id,
            Some(service)
        )
        .await
        .unwrap());
    let body = cold
        .take_serializable_by_key_from_disk_scoped_on_execution_runtime(
            &binding.pause_key,
            Some(("anonymous".into(), "default".into())),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(body.pause_state.max_spawned_tasks, Some(0));
    assert!(body.pause_state.is_elevation());
    assert_eq!(
        authority.get(&key).unwrap().content_hash,
        blake3::hash(&app_pause_serializable_continuation_bytes(&body).unwrap())
    );
    authority.forget(&key);

    let (service, cold, binding) = root_owner_pause_fixture(tmp.path()).await;
    let key = TrustedRecordKey {
        kind: StoreKind::PauseState,
        principal: binding.principal.clone(),
        workspace: binding.workspace.clone(),
        id: binding.pause_key.clone(),
    };
    let poisoned = blake3::hash(b"same boot mismatch");
    authority.record_stage(
        key.clone(),
        AuthorityEntry {
            content_hash: poisoned,
            apply_root: None,
            target_paths: vec![],
        },
    );
    assert!(!cold
        .recover_root_owner_pause_authority_from_receipt(
            &binding.pause_key,
            "anonymous",
            "default",
            &binding.execution_id,
            Some(service)
        )
        .await
        .unwrap());
    assert_eq!(authority.get(&key).unwrap().content_hash, poisoned);
    authority.forget(&key);
}

#[tokio::test]
async fn root_owner_pause_receipt_rejects_forged_changed_and_adopted_sources() {
    use crate::magician_v2::execution::agentic::run_loop::{
        state::{LoopSegmentBinding, LoopState},
        store::{fs::FsLoopStateStore, ExecutionKey, LoopStateStore, Revision},
    };
    let tmp = tempfile::TempDir::new().unwrap();
    let (service, cold, binding) = root_owner_pause_fixture(tmp.path()).await;
    service
        .verify_root_owner_pause_receipt(&binding)
        .await
        .unwrap();
    let changes: &[fn(
        &mut crate::magician_v2::execution::agentic::RootOwnerPauseReceiptBinding,
    )] = &[
        |b| b.body_sha256 = "ff".repeat(32),
        |b| b.revision.push_str("-stale"),
        |b| b.agent_id = "other-owner".into(),
        |b| b.task_id = "task_other".into(),
        |b| b.principal = "other".into(),
        |b| b.exact_segment_id = "exec_missing".into(),
    ];
    for mutate in changes {
        let mut changed = binding.clone();
        mutate(&mut changed);
        assert!(service
            .verify_root_owner_pause_receipt(&changed)
            .await
            .is_err());
    }
    let store = FsLoopStateStore::new(service.workspace.base_root());
    let key = ExecutionKey::new("anonymous", "default", &binding.exact_segment_id).unwrap();
    let source = store.load(&key).await.unwrap().unwrap();
    let mut changed = source.state.clone();
    changed
        .terminal_settlement_receipt
        .as_mut()
        .unwrap()
        .hmac_sha256 = "00".repeat(32);
    let revision = store.commit(&key, &changed, source.revision).await.unwrap();
    assert!(service
        .verify_root_owner_pause_receipt(&binding)
        .await
        .unwrap_err()
        .to_string()
        .contains("seal_mismatch"));
    changed.terminal_settlement_receipt = None;
    let revision = store.commit(&key, &changed, revision).await.unwrap();
    assert!(service
        .verify_root_owner_pause_receipt(&binding)
        .await
        .is_err());
    store.commit(&key, &source.state, revision).await.unwrap();
    let successor = source
        .state
        .terminal_settlement_receipt
        .as_ref()
        .unwrap()
        .descriptor
        .pause_resume_segment
        .as_ref()
        .unwrap();
    let successor_key = ExecutionKey::new("anonymous", "default", successor).unwrap();
    let mut adopted = LoopState::new(source.state.identity.clone());
    adopted.segment_binding = Some(LoopSegmentBinding {
        base_execution_id: binding.execution_id.clone(),
        exact_segment_id: successor.clone(),
        preseed_admission_token: None,
    });
    store
        .commit(&successor_key, &adopted, Revision::INITIAL)
        .await
        .unwrap();
    assert!(service
        .verify_root_owner_pause_receipt(&binding)
        .await
        .unwrap_err()
        .to_string()
        .contains("already_adopted"));
    assert!(cold
        .load_serializable_by_key_from_disk_scoped(&binding.pause_key, "anonymous", "default")
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn root_owner_pause_receipt_recovers_review_requirements_without_granting_approval() {
    use crate::magician_v2::{
        agents::types::{ActionPattern, ApprovalCondition, ApprovalRule},
        execution::{
            agentic::app_pause_serializable_continuation_bytes,
            trusted_store::{
                install_process_authority, StoreKind, TrustAuthority, TrustedRecordKey,
            },
        },
    };
    let authority = install_process_authority(Arc::new(TrustAuthority::new()));
    let tmp = tempfile::TempDir::new().unwrap();
    let rules = vec![ApprovalRule {
        tool: "shell".into(),
        action: ActionPattern::Single("execute".into()),
        when: Some(ApprovalCondition {
            param_matches: [("command".into(), vec!["review-me".into()])].into(),
            url_contains: Vec::new(),
        }),
        ttl_secs: Some(60),
    }];
    let expected_rules = serde_json::to_value(&rules).unwrap();
    let (service, cold, binding) =
        root_owner_pause_fixture_with_review_rules(tmp.path(), rules).await;
    let key = TrustedRecordKey {
        kind: StoreKind::PauseState,
        principal: binding.principal.clone(),
        workspace: binding.workspace.clone(),
        id: binding.pause_key.clone(),
    };
    authority.forget(&key);
    assert!(cold
        .recover_root_owner_pause_authority_from_receipt(
            &binding.pause_key,
            "anonymous",
            "default",
            &binding.execution_id,
            Some(service)
        )
        .await
        .unwrap());
    let body = cold
        .take_serializable_by_key_from_disk_scoped_on_execution_runtime(
            &binding.pause_key,
            Some(("anonymous".into(), "default".into())),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(&body.pause_state.approval_rules).unwrap(),
        expected_rules
    );
    assert!(body.pause_state.approved_confirmation_actions.is_empty());
    assert!(body.pause_state.owner_stack.is_empty());
    assert_eq!(body.pause_state.max_spawned_tasks, Some(0));
    assert!(body.pause_state.is_elevation());
    assert_eq!(
        authority.get(&key).unwrap().content_hash,
        blake3::hash(&app_pause_serializable_continuation_bytes(&body).unwrap())
    );
    authority.forget(&key);
}

#[tokio::test]
async fn root_owner_pause_receipt_rejects_rehashed_body_and_extra_approvals() {
    use crate::magician_v2::execution::{
        agentic::SerializablePauseData,
        trusted_store::{install_process_authority, StoreKind, TrustAuthority, TrustedRecordKey},
    };
    use sha2::{Digest, Sha256};
    let authority = install_process_authority(Arc::new(TrustAuthority::new()));
    for mutation in ["cap", "grant", "remove_owner", "remove_source"] {
        let tmp = tempfile::TempDir::new().unwrap();
        let (service, cold, binding) = root_owner_pause_fixture(tmp.path()).await;
        let key = TrustedRecordKey {
            kind: StoreKind::PauseState,
            principal: "anonymous".into(),
            workspace: "default".into(),
            id: binding.pause_key.clone(),
        };
        authority.forget(&key);
        let directory = service.workspace.pause_states_dir("anonymous", "default");
        let (path, mut envelope) = std::fs::read_dir(&directory)
            .unwrap()
            .filter_map(|entry| {
                let path = entry.ok()?.path();
                let envelope: Value = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
                (envelope.get("key")?.as_str()? == binding.pause_key).then_some((path, envelope))
            })
            .next()
            .unwrap();
        let body_path = directory.join(envelope["body_file"].as_str().unwrap());
        let mut body: SerializablePauseData =
            serde_json::from_slice(&std::fs::read(&body_path).unwrap()).unwrap();
        if mutation == "grant" {
            body.pause_state
                .approval_rules
                .push(crate::magician_v2::agents::types::ApprovalRule {
                    tool: "shell".into(),
                    action: crate::magician_v2::agents::types::ActionPattern::Single(
                        "execute".into(),
                    ),
                    when: None,
                    ttl_secs: None,
                });
        } else {
            body.pause_state.max_spawned_tasks = Some(100);
            if mutation == "remove_owner" || mutation == "remove_source" {
                body.pause_state.active_owner_agent_id = None;
            }
            if mutation == "remove_source" {
                body.pause_state.root_execution_id = None;
                body.pause_state.stateless_parked_segment = None;
            }
        }
        let raw = serde_json::to_vec(&body).unwrap();
        envelope["body_sha256"] = json!(format!("{:x}", Sha256::digest(&raw)));
        envelope["body_size"] = json!(raw.len());
        envelope["authorization_hash"] =
            json!(body.pause_state.authorization_hash().to_hex().to_string());
        std::fs::write(&body_path, raw).unwrap();
        std::fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();
        let result = cold
            .recover_root_owner_pause_authority_from_receipt(
                &binding.pause_key,
                "anonymous",
                "default",
                &binding.execution_id,
                Some(service),
            )
            .await;
        assert!(
            result.is_err(),
            "rehashed body mutation must not verify: {mutation}"
        );
        assert!(authority.get(&key).is_none());
        assert!(
            body_path.exists() && path.exists(),
            "refusal preserves the sole durable continuation"
        );
    }
}
