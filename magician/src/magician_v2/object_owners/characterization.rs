//! Characterization of current object-owner directory layouts.

use super::blob::{BlobAccess, LocalTreeStore};
use super::owners::ObjectOwner;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

#[tokio::test]
async fn restart_reopens_each_owner_layout() {
    for owner in ObjectOwner::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let store = LocalTreeStore::for_scope_root(workspace.scope_root("alice", "home"), owner);
        store.put(owner.sample_rel(), b"payload").await.unwrap();
        let reopened = LocalTreeStore::for_scope_root(workspace.scope_root("alice", "home"), owner);
        assert_eq!(reopened.get(owner.sample_rel()).await.unwrap(), b"payload");
        assert!(workspace
            .scope_root("alice", "home")
            .join(owner.sample_rel())
            .is_file());
    }
}

#[tokio::test]
async fn scopes_do_not_leak() {
    for owner in ObjectOwner::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let alice = LocalTreeStore::for_scope_root(workspace.scope_root("alice", "home"), owner);
        let bob = LocalTreeStore::for_scope_root(workspace.scope_root("bob", "home"), owner);
        alice.put(owner.sample_rel(), b"secret").await.unwrap();
        assert!(!bob.exists(owner.sample_rel()).await.unwrap());
    }
}

#[tokio::test]
async fn traversal_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let store = LocalTreeStore::for_scope_root(tmp.path(), ObjectOwner::AppPackages);
    assert!(store.put("../escape.bin", b"nope").await.is_err());
}

#[tokio::test]
async fn persist_execution_recording_stays_under_recordings() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(tmp.path());
    let path = super::persist_execution_recording(
        &workspace,
        "alice",
        "home",
        Some("task-1"),
        "ex-1",
        "clip.webm",
        b"webm",
    )
    .await
    .unwrap();
    assert!(path.ends_with("recordings/clip.webm"));
    assert_eq!(std::fs::read(&path).unwrap(), b"webm");
}

#[tokio::test]
async fn overwrite_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let store = LocalTreeStore::for_scope_root(tmp.path(), ObjectOwner::AppAttachments);
    let rel = ObjectOwner::AppAttachments.sample_rel();
    store.put(rel, b"first").await.unwrap();
    store.put(rel, b"second").await.unwrap();
    assert_eq!(store.get(rel).await.unwrap(), b"second");
}
