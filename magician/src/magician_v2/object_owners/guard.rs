//! Owner-specific source guards for the Task 10 object kit.

use magician_storage::StorageCatalogId;
use magician_storage_migration::SourceGuard;

use super::owners::ObjectOwner;

const ALLOWED: [&str; 3] = [
    "magician/src/magician_v2/object_owners/blob.rs",
    "magician/src/magician_v2/object_owners/object_backend.rs",
    "magician/src/magician_v2/object_owners/migration.rs",
];

pub fn object_owner_source_guard(owner: ObjectOwner) -> SourceGuard {
    SourceGuard::enabled(
        StorageCatalogId::parse(owner.id()).expect("catalog id"),
        ALLOWED,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_constructors_stay_in_the_kit() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        let mut offenders = Vec::new();
        for rel in [
            "magician_v2/apps/registry.rs",
            "magician_v2/apps/package_staging.rs",
            "magician_v2/execution/screenshot_cache.rs",
            "magician_v2/execution/agentic/executor.rs",
            "magician_v2/tool_result_materialization.rs",
            "magician_v2/analytics/llm_trace_materializer.rs",
        ] {
            let path = format!("{root}/{rel}");
            let text = std::fs::read_to_string(&path).unwrap();
            if text.contains("LocalTreeStore::for_scope_root")
                || text.contains("ObjectTreeStore::new")
            {
                offenders.push(rel);
            }
        }
        assert!(
            offenders.is_empty(),
            "object-owner callers reconstructed the adapter: {offenders:?}"
        );
    }

    #[test]
    fn enabled_guard_rejects_a_new_bypass_site() {
        let guard = object_owner_source_guard(ObjectOwner::AppPackages);
        guard
            .check("magician/src/magician_v2/object_owners/blob.rs")
            .unwrap();
        assert!(guard.check("magician/src/bypass.rs").is_err());
    }
}
