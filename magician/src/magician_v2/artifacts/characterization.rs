//! Characterization of the current durable-artifact markdown layout.

use super::durable_store::{DurableArtifactStore, DurableFrontmatter};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

fn fm(ns: &str, name: &str) -> DurableFrontmatter {
    DurableFrontmatter {
        namespace: ns.to_string(),
        name: name.to_string(),
        created_by: "test".to_string(),
        last_updated_by: "test".to_string(),
        last_updated: chrono::Utc::now(),
        content_type: Some("text/markdown".to_string()),
        source_execution_id: None,
        source_task_id: None,
        source_workflow_instance_id: None,
        source_run_id: None,
        source_cycle_id: None,
        source_agent_id: None,
        producer_stage: None,
    }
}

#[tokio::test]
async fn restart_reopens_the_same_markdown_layout() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(tmp.path());
    DurableArtifactStore::for_scope(&workspace, "alice", "home")
        .unwrap()
        .write("ns", "a.md", "payload", fm("ns", "a.md"))
        .await
        .unwrap();

    let reopened = DurableArtifactStore::for_scope(&workspace, "alice", "home").unwrap();
    let (_, body) = reopened.read("ns", "a.md").await.unwrap();
    assert_eq!(body, "payload");
    assert!(workspace
        .durable_artifacts_root("alice", "home")
        .join("ns")
        .join("a.md")
        .is_file());
}

#[tokio::test]
async fn scopes_do_not_leak() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(tmp.path());
    let alice = DurableArtifactStore::for_scope(&workspace, "alice", "home").unwrap();
    let bob = DurableArtifactStore::for_scope(&workspace, "bob", "home").unwrap();
    alice
        .write("ns", "a.md", "secret", fm("ns", "a.md"))
        .await
        .unwrap();
    assert!(!bob.exists("ns", "a.md"));
}

#[tokio::test]
async fn corrupt_frontmatter_fails_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let store = DurableArtifactStore::new(tmp.path().join("artifacts")).unwrap();
    store
        .write("ns", "a.md", "body", fm("ns", "a.md"))
        .await
        .unwrap();
    let path = store.resolve_path_safe("ns", "a.md").unwrap();
    std::fs::write(&path, "not-frontmatter").unwrap();
    assert!(store.read("ns", "a.md").await.is_err());
}

#[tokio::test]
async fn overwrite_replaces_body_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    let store = DurableArtifactStore::new(tmp.path().join("artifacts")).unwrap();
    store
        .write("ns", "a.md", "first", fm("ns", "a.md"))
        .await
        .unwrap();
    store
        .write("ns", "a.md", "second", fm("ns", "a.md"))
        .await
        .unwrap();
    let (_, body) = store.read("ns", "a.md").await.unwrap();
    assert_eq!(body, "second");
}
