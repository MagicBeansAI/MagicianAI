//! Characterization of the current chat and progress layouts.

use super::local::{ChatAccess, LocalChatStore};
use super::owners::ChatOwner;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

const SESSION_OUTPUT: &str = "ui/chat_sessions/sess-1/outputs/a.bin";
const LIFECYCLE: &str = "ui/chat_sessions/.lifecycle/deleted/sess-1.deleted.json";
const TRANSCRIPT_SEGMENT: &str =
    "ui/chat_sessions/sess-1/llm_history/segments/gen-000000000001.json";

#[tokio::test]
async fn restart_reopens_each_chat_owner_layout() {
    for owner in ChatOwner::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let store = super::open_local_chat_owner(&workspace, "alice", "home", owner);
        store
            .put(owner.sample_rel(), b"{\"id\":\"1\"}")
            .await
            .unwrap();
        let reopened = super::open_local_chat_owner(&workspace, "alice", "home", owner);
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
    for owner in ChatOwner::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let alice = super::open_local_chat_owner(&workspace, "alice", "home", owner);
        let bob = super::open_local_chat_owner(&workspace, "bob", "home", owner);
        alice.put(owner.sample_rel(), b"secret").await.unwrap();
        assert!(!bob.exists(owner.sample_rel()).await.unwrap());
    }
}

#[tokio::test]
async fn missing_lineage_does_not_empty_progress_events() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(tmp.path());
    let events =
        super::open_local_chat_owner(&workspace, "alice", "home", ChatOwner::ProgressEvents);
    events
        .put(ChatOwner::ProgressEvents.sample_rel(), b"{\"seq\":1}\n")
        .await
        .unwrap();
    let lineage =
        super::open_local_chat_owner(&workspace, "alice", "home", ChatOwner::ProgressLineage);
    assert!(!lineage
        .exists(ChatOwner::ProgressLineage.sample_rel())
        .await
        .unwrap());
    assert!(events
        .exists(ChatOwner::ProgressEvents.sample_rel())
        .await
        .unwrap());
}

#[tokio::test]
async fn chat_sessions_do_not_claim_messages_or_transcripts() {
    let store = LocalChatStore::for_scope_root("/tmp", ChatOwner::ChatSessions);
    assert!(store
        .put(ChatOwner::ChatMessages.sample_rel(), b"nope")
        .await
        .is_err());
    assert!(store
        .put(ChatOwner::ChatTranscripts.sample_rel(), b"nope")
        .await
        .is_err());
    assert!(store.put(SESSION_OUTPUT, b"nope").await.is_err());
}

#[tokio::test]
async fn traversal_is_rejected() {
    let store = LocalChatStore::for_scope_root("/tmp", ChatOwner::ChatSessions);
    assert!(store.put("../escape.json", b"nope").await.is_err());
}

#[tokio::test]
async fn overwrite_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let store = LocalChatStore::for_scope_root(tmp.path(), ChatOwner::ChatSessions);
    let rel = ChatOwner::ChatSessions.sample_rel();
    store.put(rel, b"first").await.unwrap();
    store.put(rel, b"second").await.unwrap();
    assert_eq!(store.get(rel).await.unwrap(), b"second");
}

#[tokio::test]
async fn each_sample_has_exactly_one_owner() {
    for owner in ChatOwner::ALL {
        assert_eq!(ChatOwner::claiming(owner.sample_rel()), Some(owner));
    }
    assert_eq!(
        ChatOwner::claiming(LIFECYCLE),
        Some(ChatOwner::ChatSessions)
    );
    assert_eq!(
        ChatOwner::claiming(TRANSCRIPT_SEGMENT),
        Some(ChatOwner::ChatTranscripts)
    );
}

#[tokio::test]
async fn persist_chat_file_stays_on_the_classified_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(tmp.path());
    let path = workspace
        .scope_root("alice", "home")
        .join(ChatOwner::ChatSessions.sample_rel());
    super::persist_chat_file(&workspace, &path, b"{\"v\":1}")
        .await
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"{\"v\":1}");
    assert!(
        super::persist_chat_file(&workspace, &tmp.path().join("outside.json"), b"nope")
            .await
            .is_err()
    );
}
