//! Characterization of current SQLite/DuckDB layouts.

use super::local::{DatabaseAccess, LocalDatabaseStore};
use super::owners::DatabaseOwner;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

#[tokio::test]
async fn restart_reopens_each_database_owner_layout() {
    for owner in DatabaseOwner::ALL {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let store = super::open_local_database_owner(&workspace, "alice", "home", owner);
        store.put(owner.sample_rel(), b"sqlite").await.unwrap();
        let reopened = super::open_local_database_owner(&workspace, "alice", "home", owner);
        assert_eq!(reopened.get(owner.sample_rel()).await.unwrap(), b"sqlite");
        assert!(owner
            .root(&workspace, "alice", "home")
            .join(owner.sample_rel())
            .is_file());
    }
}

#[tokio::test]
async fn tenant_scopes_do_not_leak() {
    for owner in DatabaseOwner::TENANT {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let alice = super::open_local_database_owner(&workspace, "alice", "home", owner);
        let bob = super::open_local_database_owner(&workspace, "bob", "home", owner);
        alice.put(owner.sample_rel(), b"secret").await.unwrap();
        assert!(!bob.exists(owner.sample_rel()).await.unwrap());
    }
}

#[tokio::test]
async fn host_files_are_shared_at_runtime_root() {
    for owner in DatabaseOwner::HOST {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(tmp.path());
        let alice = super::open_local_database_owner(&workspace, "alice", "home", owner);
        let bob = super::open_local_database_owner(&workspace, "bob", "home", owner);
        alice.put(owner.sample_rel(), b"shared").await.unwrap();
        assert!(bob.exists(owner.sample_rel()).await.unwrap());
        assert_eq!(bob.get(owner.sample_rel()).await.unwrap(), b"shared");
    }
}

#[tokio::test]
async fn missing_feed_does_not_empty_social() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(tmp.path());
    let social =
        super::open_local_database_owner(&workspace, "alice", "home", DatabaseOwner::SocialSqlite);
    social
        .put(DatabaseOwner::SocialSqlite.sample_rel(), b"keep")
        .await
        .unwrap();
    let feed =
        super::open_local_database_owner(&workspace, "alice", "home", DatabaseOwner::FeedDuckdb);
    assert!(!feed
        .exists(DatabaseOwner::FeedDuckdb.sample_rel())
        .await
        .unwrap());
    assert!(social
        .exists(DatabaseOwner::SocialSqlite.sample_rel())
        .await
        .unwrap());
}

#[tokio::test]
async fn missing_analytics_catalog_does_not_empty_usage_sqlite() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(tmp.path());
    let usage = super::open_local_database_owner(
        &workspace,
        "alice",
        "home",
        DatabaseOwner::BrowserEngineUsage,
    );
    usage
        .put(DatabaseOwner::BrowserEngineUsage.sample_rel(), b"keep")
        .await
        .unwrap();
    let analytics = super::open_local_database_owner(
        &workspace,
        "alice",
        "home",
        DatabaseOwner::AnalyticsDuckdb,
    );
    assert!(!analytics
        .exists(DatabaseOwner::AnalyticsDuckdb.sample_rel())
        .await
        .unwrap());
    assert!(usage
        .exists(DatabaseOwner::BrowserEngineUsage.sample_rel())
        .await
        .unwrap());
}

#[tokio::test]
async fn wal_sidecars_belong_to_the_same_owner() {
    assert_eq!(
        DatabaseOwner::claiming("social/social.db-wal"),
        Some(DatabaseOwner::SocialSqlite)
    );
    assert_eq!(
        DatabaseOwner::claiming("analytics/analytics.duckdb.wal"),
        Some(DatabaseOwner::AnalyticsDuckdb)
    );
}

#[tokio::test]
async fn traversal_is_rejected() {
    let store = LocalDatabaseStore::for_scope_root("/tmp", DatabaseOwner::SocialSqlite);
    assert!(store.put("../escape.db", b"nope").await.is_err());
}

#[tokio::test]
async fn each_sample_has_exactly_one_owner() {
    for owner in DatabaseOwner::ALL {
        assert_eq!(DatabaseOwner::claiming(owner.sample_rel()), Some(owner));
    }
}

#[tokio::test]
async fn scratch_device_local_and_already_closed_stores_are_not_this_kit() {
    for id in [
        "delivery_receipts",
        "list_index",
        "device_local_imessage",
        "device_local_whatsapp",
        "scratch_tool_duckdb",
    ] {
        assert!(
            DatabaseOwner::ALL.iter().all(|owner| owner.id() != id),
            "{id} must not be a Task 15 lifecycle file"
        );
    }
}

#[tokio::test]
async fn persist_database_file_stays_on_the_classified_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(tmp.path());
    let path =
        super::database_file_path(&workspace, "alice", "home", DatabaseOwner::AppStoreSqlite);
    super::persist_database_file(&workspace, &path, b"db")
        .await
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"db");
}
