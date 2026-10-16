//! Source guards for Task 15 SQLite/DuckDB owners.

use magician_storage::StorageCatalogId;
use magician_storage_migration::SourceGuard;

use super::owners::DatabaseOwner;

const ALLOWED: [&str; 3] = [
    "magician/src/magician_v2/database_owners/local.rs",
    "magician/src/magician_v2/database_owners/remote.rs",
    "magician/src/magician_v2/database_owners/migration.rs",
];

pub fn database_owner_source_guard(owner: DatabaseOwner) -> SourceGuard {
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
            "magician_v2/analytics/pool.rs",
            "magician_v2/ui_threads/store.rs",
            "magician_v2/feed/store.rs",
            "magician_v2/apps/registry.rs",
            "magician_v2/social/store.rs",
            "magician_v2/browser_engine_analytics.rs",
            "magician_v2/api_mining/projection.rs",
            "magician_v2/attention/learning/store.rs",
            "magician_v2/attention/resurfacing/store.rs",
            "magician_v2/realtime_events.rs",
        ] {
            let path = format!("{root}/{rel}");
            let text = std::fs::read_to_string(&path).unwrap();
            if text.contains("LocalDatabaseStore::for_scope_root")
                || text.contains("RemoteDatabaseStore::new")
            {
                offenders.push(rel);
            }
        }
        assert!(
            offenders.is_empty(),
            "database callers reconstructed the adapter: {offenders:?}"
        );
    }

    #[test]
    fn production_paths_go_through_the_kit() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        for rel in [
            "magician_v2/analytics/pool.rs",
            "magician_v2/ui_threads/store.rs",
            "magician_v2/feed/store.rs",
            "magician_v2/apps/registry.rs",
            "magician_v2/social/store.rs",
            "magician_v2/browser_engine_analytics.rs",
            "magician_v2/attention/learning/store.rs",
            "magician_v2/attention/resurfacing/store.rs",
            "magician_v2/realtime_events.rs",
        ] {
            let path = format!("{root}/{rel}");
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(
                text.contains("database_file_path") || text.contains("host_database_path"),
                "{rel} must resolve the Task 15 database path through the kit"
            );
        }
    }

    #[test]
    fn enabled_guard_rejects_a_new_bypass_site() {
        let guard = database_owner_source_guard(DatabaseOwner::AnalyticsDuckdb);
        guard
            .check("magician/src/magician_v2/database_owners/local.rs")
            .unwrap();
        assert!(guard.check("magician/src/bypass.rs").is_err());
    }
}
