//! Owner-specific source guard for durable artifacts.

use magician_storage::StorageCatalogId;
use magician_storage_migration::SourceGuard;

pub const DURABLE_ARTIFACTS_OWNER_ID: &str = "durable_artifacts";

pub fn durable_artifacts_source_guard() -> SourceGuard {
    SourceGuard::enabled(
        StorageCatalogId::parse(DURABLE_ARTIFACTS_OWNER_ID).expect("catalog id"),
        [
            "magician/src/magician_v2/artifacts/durable_store.rs",
            "magician/src/magician_v2/artifacts/object_backend.rs",
            "magician/src/magician_v2/artifacts/migration.rs",
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_constructors_stay_on_the_factory() {
        let magician_src = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        let workspace = concat!(env!("CARGO_MANIFEST_DIR"), "/..");
        let mut offenders = Vec::new();
        let magician_rels = [
            "magician_v2/observe_catchup.rs",
            "magician_v2/observe_connectors.rs",
            "magician_v2/orchestrator/v2_orchestrator.rs",
            "magician_v2/execution/task_state_provider.rs",
            "magician_v2/execution/agentic/executor.rs",
        ];
        for rel in magician_rels {
            let path = format!("{magician_src}/{rel}");
            let text = std::fs::read_to_string(&path).unwrap();
            if text.contains("DurableArtifactStore::for_scope")
                || text.contains("DurableArtifactStore::new(")
            {
                offenders.push(format!("magician/src/{rel}"));
            }
        }
        let sibling_rels = [
            "magician-api/src/ambient_api.rs",
            "magician-api/src/artifact_api.rs",
            "magician-api/src/evidence_api.rs",
            "magician-api/src/observe_connectors_api.rs",
            "magician-api/src/web_api.rs",
            "magician-bin/src/main.rs",
            "magician-comms/src/channel_assist/registry.rs",
            "magician-comms/src/channel_assist/sync.rs",
        ];
        for rel in sibling_rels {
            let path = format!("{workspace}/{rel}");
            let text = std::fs::read_to_string(&path).unwrap();
            if text.contains("DurableArtifactStore::for_scope")
                || text.contains("DurableArtifactStore::new(")
            {
                offenders.push(rel.to_string());
            }
        }
        assert!(
            offenders.is_empty(),
            "durable artifact callers reconstructed the adapter: {offenders:?}"
        );
    }

    #[test]
    fn enabled_guard_rejects_a_new_bypass_site() {
        let guard = durable_artifacts_source_guard();
        guard
            .check("magician/src/magician_v2/artifacts/durable_store.rs")
            .unwrap();
        assert!(guard.check("magician/src/bypass.rs").is_err());
    }
}
