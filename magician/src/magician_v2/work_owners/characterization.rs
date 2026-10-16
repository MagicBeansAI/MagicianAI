//! Characterization of the current work-spine layouts.

use super::local::{LocalWorkStore, WorkAccess};
use super::owners::WorkOwner;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

const INTERNAL_EXECUTION: &str = "internal_tasks/task-1/executions/ex-1/execution.json";
const MONITOR_FEEDBACK: &str = "tasks/task-1/state/monitor_feedback.jsonl";
const PLAN_INDEX: &str = "tasks/task-1/plans/plans_index.json";
const RECOVERY_MARKER: &str = "task_planning_recovery/task-1.json";
const PAUSE_HASH: &str = "runtime/pause_states/k2_abc.json";

#[tokio::test]
async fn restart_reopens_each_work_owner_layout() {
    for owner in WorkOwner::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let store = super::open_local_work_owner(&workspace, "alice", "home", owner);
        store
            .put(owner.sample_rel(), b"{\"id\":\"1\"}")
            .await
            .unwrap();
        let reopened = super::open_local_work_owner(&workspace, "alice", "home", owner);
        assert_eq!(
            reopened.get(owner.sample_rel()).await.unwrap(),
            b"{\"id\":\"1\"}"
        );
        assert!(workspace
            .scope_root("alice", "home")
            .join(owner.sample_rel())
            .is_file());
    }
}

#[tokio::test]
async fn scopes_do_not_leak() {
    for owner in WorkOwner::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let alice = super::open_local_work_owner(&workspace, "alice", "home", owner);
        let bob = super::open_local_work_owner(&workspace, "bob", "home", owner);
        alice.put(owner.sample_rel(), b"secret").await.unwrap();
        assert!(!bob.exists(owner.sample_rel()).await.unwrap());
    }
}

#[tokio::test]
async fn missing_list_index_does_not_empty_task_records() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(tmp.path());
    let tasks = super::open_local_work_owner(&workspace, "alice", "home", WorkOwner::TaskRecords);
    tasks
        .put(WorkOwner::TaskRecords.sample_rel(), b"{\"title\":\"keep\"}")
        .await
        .unwrap();
    let index = super::open_local_work_owner(&workspace, "alice", "home", WorkOwner::ListIndex);
    assert!(!index
        .exists(WorkOwner::ListIndex.sample_rel())
        .await
        .unwrap());
    assert!(tasks
        .exists(WorkOwner::TaskRecords.sample_rel())
        .await
        .unwrap());
}

#[tokio::test]
async fn task_records_do_not_claim_outputs_or_executions() {
    let store = LocalWorkStore::for_scope_root("/tmp", WorkOwner::TaskRecords);
    assert!(store
        .put("tasks/task-1/outputs/a.bin", b"nope")
        .await
        .is_err());
    assert!(store
        .put("tasks/task-1/executions/ex-1/state.json", b"nope")
        .await
        .is_err());
    assert!(store.put(PLAN_INDEX, b"nope").await.is_err());
}

#[tokio::test]
async fn traversal_is_rejected() {
    let store = LocalWorkStore::for_scope_root("/tmp", WorkOwner::TaskRecords);
    assert!(store.put("../escape.json", b"nope").await.is_err());
}

#[tokio::test]
async fn overwrite_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let store = LocalWorkStore::for_scope_root(tmp.path(), WorkOwner::TaskRecords);
    let rel = WorkOwner::TaskRecords.sample_rel();
    store.put(rel, b"first").await.unwrap();
    store.put(rel, b"second").await.unwrap();
    assert_eq!(store.get(rel).await.unwrap(), b"second");
}

#[tokio::test]
async fn each_sample_has_exactly_one_owner() {
    for owner in WorkOwner::ALL {
        assert_eq!(WorkOwner::claiming(owner.sample_rel()), Some(owner));
    }
    assert_eq!(
        WorkOwner::claiming(INTERNAL_EXECUTION),
        Some(WorkOwner::Executions)
    );
    assert_eq!(
        WorkOwner::claiming(MONITOR_FEEDBACK),
        Some(WorkOwner::TaskRecords)
    );
    assert_eq!(WorkOwner::claiming(PLAN_INDEX), Some(WorkOwner::TaskPlans));
    assert_eq!(
        WorkOwner::claiming(RECOVERY_MARKER),
        Some(WorkOwner::TaskPlanningRecovery)
    );
    assert_eq!(
        WorkOwner::claiming(PAUSE_HASH),
        Some(WorkOwner::PauseStates)
    );
    assert_eq!(
        WorkOwner::claiming("restricted/accepted_runtime_launch/entry.json"),
        Some(WorkOwner::RestrictedHmacCatalogs)
    );
    assert!(WorkOwner::claiming("restricted/unknown_catalog/entry.json").is_none());
}

#[tokio::test]
async fn persist_work_file_stays_on_the_classified_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(tmp.path());
    let path = workspace
        .scope_root("alice", "home")
        .join(WorkOwner::Executions.sample_rel());
    super::persist_work_file(&workspace, &path, b"{\"v\":1}")
        .await
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"{\"v\":1}");
    assert!(
        super::persist_work_file(&workspace, &tmp.path().join("outside.json"), b"nope")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn executions_exclude_task_10_object_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let store = LocalWorkStore::for_scope_root(tmp.path(), WorkOwner::Executions);
    for rel in [
        "tasks/task-1/executions/ex-1/outputs/a.bin",
        "tasks/task-1/executions/ex-1/observations/shot.png",
        "tasks/task-1/executions/ex-1/recordings/clip.webm",
        "tasks/task-1/executions/ex-1/artifacts/downloads/file.bin",
        "internal_tasks/task-1/executions/ex-1/outputs/a.bin",
    ] {
        assert!(store.put(rel, b"nope").await.is_err(), "{rel}");
    }
    store.put(INTERNAL_EXECUTION, b"ok").await.unwrap();
}
