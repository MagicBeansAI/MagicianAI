//! Source guards for Task 11 Parquet families.

use magician_storage::StorageCatalogId;
use magician_storage_migration::SourceGuard;

use super::family::DatasetFamily;

const ALLOWED: [&str; 3] = [
    "magician/src/magician_v2/dataset_owners/local.rs",
    "magician/src/magician_v2/dataset_owners/remote.rs",
    "magician/src/magician_v2/dataset_owners/migration.rs",
];

pub fn dataset_owner_source_guard(family: DatasetFamily) -> SourceGuard {
    SourceGuard::enabled(
        StorageCatalogId::parse(family.id()).expect("catalog id"),
        ALLOWED,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_readers_use_the_family_glob_helper() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        let mut offenders = Vec::new();
        for rel in [
            "magician_v2/analytics/llm_parquet_sink.rs",
            "magician_v2/analytics/llm_embeddings_sink.rs",
            "magician_v2/analytics/llm_trace_materializer.rs",
            "magician_v2/analytics/memory_index_maintainer.rs",
            "magician_v2/execution/internal_data_provider.rs",
        ] {
            let path = format!("{root}/{rel}");
            let text = std::fs::read_to_string(&path).unwrap();
            if text.contains("dt=*/*.parquet")
                && !text.contains("family_read_glob")
                && !text.contains("family_parquet_glob")
            {
                offenders.push(rel);
            }
        }
        assert!(
            offenders.is_empty(),
            "dataset readers still hard-code parquet globs: {offenders:?}"
        );
    }

    #[test]
    fn enabled_guard_rejects_a_new_bypass_site() {
        let guard = dataset_owner_source_guard(DatasetFamily::Events);
        guard
            .check("magician/src/magician_v2/dataset_owners/local.rs")
            .unwrap();
        assert!(guard.check("magician/src/bypass.rs").is_err());
    }
}
