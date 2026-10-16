//! Source guards for Task 16A subprocess and skill storage owners.

use magician_storage::StorageCatalogId;
use magician_storage_migration::SourceGuard;

use super::owners::SubprocessOwner;

const ALLOWED: [&str; 3] = [
    "magician/src/magician_v2/subprocess_owners/local.rs",
    "magician/src/magician_v2/subprocess_owners/remote.rs",
    "magician/src/magician_v2/subprocess_owners/migration.rs",
];

pub fn subprocess_owner_source_guard(owner: SubprocessOwner) -> SourceGuard {
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
            "magician_v2/artifact_v2/capabilities.rs",
            "magician_v2/execution/compiled_providers.rs",
            "magician_v2/execution/primitive_dispatch/governed_runtime.rs",
        ] {
            let path = format!("{root}/{rel}");
            let text = std::fs::read_to_string(&path).unwrap();
            if text.contains("LocalSubprocessStore::for_scope_root")
                || text.contains("RemoteSubprocessStore::new")
            {
                offenders.push(rel);
            }
        }
        assert!(
            offenders.is_empty(),
            "subprocess callers reconstructed the adapter: {offenders:?}"
        );
    }

    #[test]
    fn production_paths_go_through_the_kit() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        let capabilities =
            std::fs::read_to_string(format!("{root}/magician_v2/artifact_v2/capabilities.rs"))
                .unwrap();
        assert!(
            capabilities.contains("subprocess_owners::workdirs_root"),
            "capabilities.rs must resolve workdirs through the Task 16A kit"
        );
        let compiled = std::fs::read_to_string(format!(
            "{root}/magician_v2/execution/compiled_providers.rs"
        ))
        .unwrap();
        assert!(
            compiled.contains("subprocess_owners::workdirs_root"),
            "compiled_providers.rs must resolve workdirs through the Task 16A kit"
        );
        let governed = std::fs::read_to_string(format!(
            "{root}/magician_v2/execution/primitive_dispatch/governed_runtime.rs"
        ))
        .unwrap();
        assert!(
            governed.contains("install_skill_working_envelope_from_exec"),
            "governed_runtime.rs must install the subprocess storage envelope"
        );
    }

    #[test]
    fn enabled_guard_rejects_a_new_bypass_site() {
        let guard = subprocess_owner_source_guard(SubprocessOwner::SkillWorking);
        guard
            .check("magician/src/magician_v2/subprocess_owners/local.rs")
            .unwrap();
        assert!(guard.check("magician/src/bypass.rs").is_err());
    }
}
