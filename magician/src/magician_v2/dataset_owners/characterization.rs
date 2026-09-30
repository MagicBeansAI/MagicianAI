//! Characterization of current Parquet family layouts.

use super::family::DatasetFamily;
use super::local::{DatasetAccess, LocalFamilyStore};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

#[tokio::test]
async fn restart_reopens_each_family_layout() {
    for family in DatasetFamily::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let store = super::open_local_dataset_family(&workspace, "alice", "home", family);
        store.put(family.sample_rel(), b"PAR1").await.unwrap();
        let reopened = super::open_local_dataset_family(&workspace, "alice", "home", family);
        assert_eq!(reopened.get(family.sample_rel()).await.unwrap(), b"PAR1");
        assert!(family
            .root(&workspace, "alice", "home")
            .join(family.sample_rel())
            .is_file());
    }
}

#[tokio::test]
async fn scopes_do_not_leak() {
    for family in DatasetFamily::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let alice = super::open_local_dataset_family(&workspace, "alice", "home", family);
        let bob = super::open_local_dataset_family(&workspace, "bob", "home", family);
        alice.put(family.sample_rel(), b"secret").await.unwrap();
        assert!(!bob.exists(family.sample_rel()).await.unwrap());
    }
}

#[tokio::test]
async fn traversal_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let store = LocalFamilyStore::for_root(tmp.path(), DatasetFamily::Events);
    assert!(store.put("../escape.parquet", b"nope").await.is_err());
}

#[test]
fn workspace_from_absolute_scope_path_keeps_outermost_scopes() {
    let tmp = tempfile::tempdir().unwrap();
    let parquet = tmp
        .path()
        .join("scopes/alice/home/events/dt=2026-08-21/part.parquet");
    std::fs::create_dir_all(parquet.parent().unwrap()).unwrap();
    std::fs::write(&parquet, b"PAR1").unwrap();
    let workspace = super::local::workspace_from_scoped_path_for_test(&parquet)
        .expect("absolute scoped parquet resolves");
    assert_eq!(workspace.base_root(), tmp.path());

    let nested = tmp
        .path()
        .join("scopes/alice/home/scopes/inner/events/part.parquet");
    std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
    std::fs::write(&nested, b"PAR1").unwrap();
    let outer = super::local::workspace_from_scoped_path_for_test(&nested)
        .expect("nested scopes still resolves the outer workspace");
    assert_eq!(outer.base_root(), tmp.path());
}

#[tokio::test]
async fn compatibility_glob_matches_current_layout() {
    assert_eq!(DatasetFamily::Events.glob_suffix(), "dt=*/*.parquet");
    assert_eq!(
        DatasetFamily::ActivityRows.glob_suffix(),
        "dt=*/hour=*/*.parquet"
    );
}

#[test]
fn compacted_prefix_locators_are_allowed() {
    assert!(DatasetFamily::Events.allows("dt=2026-08-31/events.compacted.parquet"));
    assert!(DatasetFamily::LlmCalls.allows("dt=2026-08-31/_compact/canonical.compacted.parquet"));
    assert!(!DatasetFamily::LlmCalls.allows("dt=2026-08-31/other/canonical.compacted.parquet"));
    assert!(
        DatasetFamily::ActivityRows.allows("dt=2026-08-31/hour=00/activity_rows.compacted.parquet")
    );
    assert!(DatasetFamily::ActivityRows.allows("dt=2026-08-31/activity_rows.compacted.parquet"));
    assert!(!DatasetFamily::Events.allows("/dt=2026-08-31/evil.parquet"));
}
