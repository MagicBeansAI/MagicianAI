//! Source guards for Task 12 work-spine owners.

use magician_storage::StorageCatalogId;
use magician_storage_migration::SourceGuard;

use super::owners::WorkOwner;

const ALLOWED: [&str; 3] = [
    "magician/src/magician_v2/work_owners/local.rs",
    "magician/src/magician_v2/work_owners/remote.rs",
    "magician/src/magician_v2/work_owners/migration.rs",
];

pub fn work_owner_source_guard(owner: WorkOwner) -> SourceGuard {
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
            "magician_v2/storage/file.rs",
            "magician_v2/storage/list_index.rs",
            "magician_v2/artifact_v2/task_writes.rs",
            "magician_v2/execution/agentic/executor.rs",
        ] {
            let path = format!("{root}/{rel}");
            let text = std::fs::read_to_string(&path).unwrap();
            if text.contains("LocalWorkStore::for_scope_root")
                || text.contains("RemoteWorkStore::new")
            {
                offenders.push(rel);
            }
        }
        assert!(
            offenders.is_empty(),
            "work-spine callers reconstructed the adapter: {offenders:?}"
        );
    }

    #[test]
    fn execution_documents_go_through_the_kit() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/magician_v2/storage/file.rs"
        );
        let text = std::fs::read_to_string(path).unwrap();
        assert!(
            text.contains("persist_work_file") && text.contains("store_for_any_owner"),
            "FileV2Store must publish execution documents through persist_work_file"
        );
    }

    #[test]
    fn enabled_guard_rejects_a_new_bypass_site() {
        let guard = work_owner_source_guard(WorkOwner::TaskRecords);
        guard
            .check("magician/src/magician_v2/work_owners/local.rs")
            .unwrap();
        assert!(guard.check("magician/src/bypass.rs").is_err());
    }
}
