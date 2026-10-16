//! Source guards for Task 16 system, device, and secret owners.

use magician_storage::StorageCatalogId;
use magician_storage_migration::SourceGuard;

use super::owners::SystemOwner;

const ALLOWED: [&str; 3] = [
    "magician/src/magician_v2/system_owners/local.rs",
    "magician/src/magician_v2/system_owners/remote.rs",
    "magician/src/magician_v2/system_owners/migration.rs",
];

pub fn system_owner_source_guard(owner: SystemOwner) -> SourceGuard {
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
            "magician_v2/secrets/store.rs",
            "magician_v2/device_pairing.rs",
            "magician_v2/resource_authority/scoped_authority.rs",
            "magician_v2/storage_governance/compaction_metrics.rs",
        ] {
            let path = format!("{root}/{rel}");
            let text = std::fs::read_to_string(&path).unwrap();
            if text.contains("LocalSystemStore::for_scope_root")
                || text.contains("RemoteSystemStore::new")
            {
                offenders.push(rel);
            }
        }
        assert!(
            offenders.is_empty(),
            "system callers reconstructed the adapter: {offenders:?}"
        );
    }

    #[test]
    fn production_paths_go_through_the_kit() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        for rel in [
            "magician_v2/device_pairing.rs",
            "magician_v2/resource_authority/scoped_authority.rs",
            "magician_v2/storage_governance/compaction_metrics.rs",
        ] {
            let path = format!("{root}/{rel}");
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(
                text.contains("persist_system_file_sync")
                    || text.contains("host_system_path")
                    || text.contains("system_file_path"),
                "{rel} must resolve Task 16 paths through the kit"
            );
        }
    }

    #[test]
    fn enabled_guard_rejects_a_new_bypass_site() {
        let guard = system_owner_source_guard(SystemOwner::SecretVault);
        guard
            .check("magician/src/magician_v2/system_owners/local.rs")
            .unwrap();
        assert!(guard.check("magician/src/bypass.rs").is_err());
    }
}
