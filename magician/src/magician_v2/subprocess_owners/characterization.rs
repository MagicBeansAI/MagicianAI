//! Characterization of current workdirs and skill-working layouts.

use super::envelope::{
    env_is_closed, envelope_digest, forbidden_child_env_names, install_skill_working_envelope,
    load_envelope, materialize_input, publish_accepted_output, scavenge_lease, InputManifestEntry,
    OutputSlot, SubprocessStorageEnvelope,
};
use super::local::{
    persist_subprocess_file_sync, workdirs_root, LocalSubprocessStore, SubprocessAccess,
};
use super::owners::SubprocessOwner;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

#[tokio::test]
async fn restart_reopens_each_subprocess_owner_layout() {
    for owner in SubprocessOwner::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let store = super::open_local_subprocess_owner(&workspace, "alice", "home", owner);
        store
            .put(owner.sample_rel(), b"{\"id\":\"1\"}")
            .await
            .unwrap();
        let reopened = super::open_local_subprocess_owner(&workspace, "alice", "home", owner);
        assert_eq!(
            reopened.get(owner.sample_rel()).await.unwrap(),
            b"{\"id\":\"1\"}"
        );
    }
}

#[tokio::test]
async fn tenant_scopes_do_not_leak() {
    for owner in SubprocessOwner::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let alice = super::open_local_subprocess_owner(&workspace, "alice", "home", owner);
        let bob = super::open_local_subprocess_owner(&workspace, "bob", "home", owner);
        alice.put(owner.sample_rel(), b"secret").await.unwrap();
        assert!(!bob.exists(owner.sample_rel()).await.unwrap());
    }
}

#[tokio::test]
async fn each_sample_has_exactly_one_owner() {
    for owner in SubprocessOwner::ALL {
        assert_eq!(SubprocessOwner::claiming(owner.sample_rel()), Some(owner));
    }
}

#[tokio::test]
async fn skill_working_does_not_claim_tool_workdirs() {
    assert_eq!(
        SubprocessOwner::claiming("workdirs/home/.wu/config.yaml"),
        Some(SubprocessOwner::WorkdirsScratch)
    );
    assert_eq!(
        SubprocessOwner::claiming("workdirs/skill-working/call/outputs/slot.bin"),
        Some(SubprocessOwner::SkillWorking)
    );
}

#[tokio::test]
async fn traversal_is_rejected() {
    let store = LocalSubprocessStore::for_scope_root("/tmp", SubprocessOwner::WorkdirsScratch);
    assert!(store.put("../escape.json", b"nope").await.is_err());
}

#[tokio::test]
async fn task16b_is_not_this_kit() {
    assert!(SubprocessOwner::ALL
        .iter()
        .all(|owner| owner.id() != "desktop_engine_roots"));
}

#[tokio::test]
async fn workdirs_root_stays_under_scope() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(tmp.path());
    let path = workdirs_root(&workspace, "alice", "home");
    assert!(path.ends_with("workdirs"));
    assert!(path.starts_with(workspace.scope_root("alice", "home")));
}

#[tokio::test]
async fn persist_classified_workdir_file() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(tmp.path());
    let path = workdirs_root(&workspace, "alice", "home")
        .join("meeting_capture_markers")
        .join("session.json");
    persist_subprocess_file_sync(&workspace, &path, b"{\"ok\":true}").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"{\"ok\":true}");
}

#[tokio::test]
async fn closed_child_rejects_engine_root_and_store_credentials() {
    for name in forbidden_child_env_names() {
        assert!(!env_is_closed([*name]));
    }
    assert!(env_is_closed(["PATH", "HOME", "LANG"]));
}

#[tokio::test]
async fn envelope_materializes_immutable_input_and_scavenges_unaccepted() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(tmp.path());
    let lease = install_skill_working_envelope(&workspace, "alice", "home", "call-1").unwrap();
    let loaded = load_envelope(&lease).unwrap();
    assert_eq!(loaded.version, super::ENVELOPE_VERSION);
    assert!(loaded.closed);
    assert!(!loaded.compat);
    let payload = b"model-bytes";
    let entry = InputManifestEntry {
        logical_id: "model".into(),
        rel: "inputs/model.bin".into(),
        digest: envelope_digest(payload),
        size_bytes: payload.len() as u64,
        source_owner: Some("skills_scope".into()),
    };
    let dest = materialize_input(&lease, &entry, payload).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), payload);
    assert!(materialize_input(&lease, &entry, b"tampered-xx").is_err());
    assert!(materialize_input(
        &lease,
        &InputManifestEntry {
            logical_id: "escape".into(),
            rel: "inputs/../escape.bin".into(),
            digest: envelope_digest(payload),
            size_bytes: payload.len() as u64,
            source_owner: None,
        },
        payload
    )
    .is_err());
    scavenge_lease(&lease).unwrap();
    assert!(!lease.exists());
}

#[tokio::test]
async fn accepted_output_publishes_through_object_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(tmp.path());
    let dest = workspace
        .scope_root("alice", "home")
        .join("tasks/task-1/executions/ex-1/outputs/tool.bin");
    let slot = OutputSlot {
        name: "accepted".into(),
        rel: "outputs/accepted.bin".into(),
        media_type: "application/octet-stream".into(),
        max_bytes: 1024,
        intended_owner: "execution_outputs".into(),
    };
    let receipt = publish_accepted_output(&workspace, &dest, &slot, b"accepted-bytes")
        .await
        .unwrap();
    assert_eq!(receipt.owner_id, "execution_outputs");
    assert_eq!(std::fs::read(&dest).unwrap(), b"accepted-bytes");
}

#[tokio::test]
async fn cancelled_output_is_absent_after_scavenge() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(tmp.path());
    let lease = install_skill_working_envelope(&workspace, "alice", "home", "cancel-1").unwrap();
    std::fs::write(lease.join("outputs/partial.bin"), b"partial").unwrap();
    scavenge_lease(&lease).unwrap();
    assert!(!lease.join("outputs/partial.bin").exists());
}

#[tokio::test]
async fn envelope_file_is_not_a_durable_result() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(tmp.path());
    let lease = install_skill_working_envelope(&workspace, "alice", "home", "disk-1").unwrap();
    assert!(SubprocessStorageEnvelope::path_in(&lease).is_file());
    assert!(load_envelope(&lease).unwrap().result.is_empty());
}
