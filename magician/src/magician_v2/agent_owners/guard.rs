//! Source guards for Task 14 agent, memory, learning, and index owners.

use magician_storage::StorageCatalogId;
use magician_storage_migration::SourceGuard;

use super::owners::AgentOwner;

const ALLOWED: [&str; 3] = [
    "magician/src/magician_v2/agent_owners/local.rs",
    "magician/src/magician_v2/agent_owners/remote.rs",
    "magician/src/magician_v2/agent_owners/migration.rs",
];

pub fn agent_owner_source_guard(owner: AgentOwner) -> SourceGuard {
    SourceGuard::enabled(
        StorageCatalogId::parse(owner.id()).expect("catalog id"),
        ALLOWED,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_stores_do_not_rebuild_the_kit() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        let mut offenders = Vec::new();
        for rel in [
            "magician_v2/agents/storage.rs",
            "magician_v2/agents/definition_store.rs",
            "magician_v2/agents/memory.rs",
            "magician_v2/learning/store.rs",
        ] {
            let path = format!("{root}/{rel}");
            let text = std::fs::read_to_string(&path).unwrap();
            if text.contains("LocalAgentStore::for_scope_root")
                || text.contains("RemoteAgentStore::new")
            {
                offenders.push(rel);
            }
        }
        assert!(
            offenders.is_empty(),
            "agent/memory callers reconstructed the adapter: {offenders:?}"
        );
    }

    #[test]
    fn agent_byte_writes_go_through_the_kit() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/magician_v2/agents/storage.rs"
        );
        let text = std::fs::read_to_string(path).unwrap();
        assert!(
            text.contains("persist_agent_file") && text.contains("store_for_any_owner"),
            "AgentStorage must publish classified files through persist_agent_file"
        );
    }

    #[test]
    fn learning_writes_go_through_the_kit() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/magician_v2/learning/store.rs"
        );
        let text = std::fs::read_to_string(path).unwrap();
        assert!(
            text.contains("persist_agent_file_sync") && text.contains("store_for_any_owner"),
            "LearningStore must publish classified files through persist_agent_file_sync"
        );
    }

    #[test]
    fn enabled_guard_rejects_a_new_bypass_site() {
        let guard = agent_owner_source_guard(AgentOwner::MemoryCanonical);
        guard
            .check("magician/src/magician_v2/agent_owners/local.rs")
            .unwrap();
        assert!(guard.check("magician/src/bypass.rs").is_err());
    }
}
