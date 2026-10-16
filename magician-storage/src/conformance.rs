//! Named capability cases. Adapters in later tasks must cover every name.

pub const OBJECT_STORE_CASES: &[&str] = &[
    "head_missing",
    "put_overwrite",
    "put_create_only",
    "put_expected_version",
    "get_range",
    "delete_existing",
    "delete_expected_version",
    "conflict_two_writers",
    "list_is_diagnostic",
];

pub const DATASET_STORE_CASES: &[&str] = &[
    "stage_part_immutable",
    "commit_manifest_cas",
    "corrupt_manifest_fails_closed",
];

pub const LEASE_STORE_CASES: &[&str] = &[
    "acquire_advances_generation",
    "cross_process_exclusion",
    "stale_generation_refused",
];

pub const SCRATCH_STORE_CASES: &[&str] = &["quota", "path_escape_refused", "startup_scavenge"];

/// Domain repository scenarios shared by local and remote adapters.
pub const REPOSITORY_SCENARIO_CASES: &[&str] = &[
    "lifecycle_crud",
    "scope_isolation",
    "revision_conflict",
    "idempotent_create",
    "export_import_equivalent",
];

/// Local/remote pair used by owner packets. Neither side is selected by default
/// startup; tests construct both explicitly.
pub struct ScenarioFactory<L, R> {
    pub owner_id: String,
    local: L,
    remote: R,
}

pub type AdapterScenarioFactory<L, R> = ScenarioFactory<L, R>;
pub type RepositoryScenarioFactory<L, R> = ScenarioFactory<L, R>;

impl<L, R> ScenarioFactory<L, R> {
    pub fn new(owner_id: impl Into<String>, local: L, remote: R) -> Self {
        Self {
            owner_id: owner_id.into(),
            local,
            remote,
        }
    }

    pub fn local(&self) -> &L {
        &self.local
    }

    pub fn remote(&self) -> &R {
        &self.remote
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_names_are_unique() {
        for list in [
            OBJECT_STORE_CASES,
            DATASET_STORE_CASES,
            LEASE_STORE_CASES,
            SCRATCH_STORE_CASES,
            REPOSITORY_SCENARIO_CASES,
        ] {
            let mut seen = std::collections::BTreeSet::new();
            for name in list.iter() {
                assert!(seen.insert(*name), "duplicate case {name}");
            }
        }
    }
}
