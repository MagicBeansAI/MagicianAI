//! Characterization of current agent, memory, learning, and index layouts.

use super::local::{AgentAccess, LocalAgentStore};
use super::owners::AgentOwner;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

const APPROVAL: &str = "agent_runtime/agents/approvals/apr-1.json";
const LEGACY_MEMORY: &str = "agent_runtime/agents/agent-1/memory/tiers/core.json";
const LANCEDB: &str = "memory/index/lancedb/data.lance";
const PROCEDURE_DECISION: &str = "learning/procedures/decisions/proc-1.jsonl";
const CANDIDATE: &str = "learning/candidates/cand-1.json";

#[tokio::test]
async fn restart_reopens_each_agent_owner_layout() {
    for owner in AgentOwner::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let store = super::open_local_agent_owner(&workspace, "alice", "home", owner);
        store
            .put(owner.sample_rel(), b"{\"id\":\"1\"}")
            .await
            .unwrap();
        let reopened = super::open_local_agent_owner(&workspace, "alice", "home", owner);
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
    for owner in AgentOwner::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let alice = super::open_local_agent_owner(&workspace, "alice", "home", owner);
        let bob = super::open_local_agent_owner(&workspace, "bob", "home", owner);
        alice.put(owner.sample_rel(), b"secret").await.unwrap();
        assert!(!bob.exists(owner.sample_rel()).await.unwrap());
    }
}

#[tokio::test]
async fn missing_memory_index_does_not_empty_canonical() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(tmp.path());
    let canonical =
        super::open_local_agent_owner(&workspace, "alice", "home", AgentOwner::MemoryCanonical);
    canonical
        .put(AgentOwner::MemoryCanonical.sample_rel(), b"keep")
        .await
        .unwrap();
    let index = super::open_local_agent_owner(&workspace, "alice", "home", AgentOwner::MemoryIndex);
    assert!(!index
        .exists(AgentOwner::MemoryIndex.sample_rel())
        .await
        .unwrap());
    assert!(!index.exists(LANCEDB).await.unwrap());
    assert!(canonical
        .exists(AgentOwner::MemoryCanonical.sample_rel())
        .await
        .unwrap());
}

#[tokio::test]
async fn missing_procedure_index_does_not_empty_procedures() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(tmp.path());
    let procedures =
        super::open_local_agent_owner(&workspace, "alice", "home", AgentOwner::LearningProcedures);
    procedures
        .put(AgentOwner::LearningProcedures.sample_rel(), b"keep")
        .await
        .unwrap();
    let index = super::open_local_agent_owner(
        &workspace,
        "alice",
        "home",
        AgentOwner::LearningProcedureIndex,
    );
    assert!(!index
        .exists(AgentOwner::LearningProcedureIndex.sample_rel())
        .await
        .unwrap());
    assert!(procedures
        .exists(AgentOwner::LearningProcedures.sample_rel())
        .await
        .unwrap());
}

#[tokio::test]
async fn definitions_do_not_claim_runtime_or_memory() {
    let store = LocalAgentStore::for_scope_root("/tmp", AgentOwner::AgentDefinitions);
    assert!(store.put(APPROVAL, b"nope").await.is_err());
    assert!(store
        .put(AgentOwner::AgentRuntime.sample_rel(), b"nope")
        .await
        .is_err());
    assert!(store.put(LEGACY_MEMORY, b"nope").await.is_err());
}

#[tokio::test]
async fn traversal_is_rejected() {
    let store = LocalAgentStore::for_scope_root("/tmp", AgentOwner::MemoryCanonical);
    assert!(store.put("../escape.json", b"nope").await.is_err());
}

#[tokio::test]
async fn overwrite_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let store = LocalAgentStore::for_scope_root(tmp.path(), AgentOwner::MemoryCanonical);
    let rel = AgentOwner::MemoryCanonical.sample_rel();
    store.put(rel, b"first").await.unwrap();
    store.put(rel, b"second").await.unwrap();
    assert_eq!(store.get(rel).await.unwrap(), b"second");
}

#[tokio::test]
async fn each_sample_has_exactly_one_owner() {
    for owner in AgentOwner::ALL {
        assert_eq!(AgentOwner::claiming(owner.sample_rel()), Some(owner));
    }
    assert_eq!(
        AgentOwner::claiming(APPROVAL),
        Some(AgentOwner::AgentRuntime)
    );
    assert_eq!(
        AgentOwner::claiming(LEGACY_MEMORY),
        Some(AgentOwner::MemoryCanonical)
    );
    assert_eq!(AgentOwner::claiming(LANCEDB), Some(AgentOwner::MemoryIndex));
    assert_eq!(
        AgentOwner::claiming(PROCEDURE_DECISION),
        Some(AgentOwner::LearningProcedures)
    );
    assert_eq!(
        AgentOwner::claiming(CANDIDATE),
        Some(AgentOwner::LearningHistories)
    );
    assert_eq!(
        AgentOwner::claiming("capability_evolution/proposals/p-1.json"),
        Some(AgentOwner::SkillEvolution)
    );
    assert!(AgentOwner::claiming("capability_evolution/capability_packs.json").is_none());
}

#[tokio::test]
async fn persist_agent_file_stays_on_the_classified_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(tmp.path());
    let path = workspace
        .scope_root("alice", "home")
        .join(AgentOwner::AgentDefinitions.sample_rel());
    super::persist_agent_file(&workspace, &path, b"kind: agent")
        .await
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"kind: agent");
    assert!(
        super::persist_agent_file(&workspace, &tmp.path().join("outside.json"), b"nope")
            .await
            .is_err()
    );
}
