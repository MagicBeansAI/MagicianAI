use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use magician_storage::lease::{LeaseResource, LeaseStore, OwnerId};
use magician_storage::profile::ResolveOptions;
use magician_storage::{
    CommitLikelihood, IdempotencyKey, PrincipalId, Revision, ScopeId, StorageError, WorkspaceId,
};
use magician_storage_state::{
    open_postgres_config, open_postgres_from_profile, ExampleRepository, PostgresOptions,
    SqliteLeaseStore, SqlitePool, SqlitePoolPolicy, EXAMPLE_STORE_ID,
};

fn scope(principal: &str, workspace: &str) -> ScopeId {
    ScopeId::new(
        PrincipalId::parse(principal).unwrap(),
        WorkspaceId::parse(workspace).unwrap(),
    )
}

async fn sqlite_stack(
    path: &std::path::Path,
) -> (Arc<SqlitePool>, SqliteLeaseStore, ExampleRepository) {
    let pool = Arc::new(
        SqlitePool::open(
            path,
            SqlitePoolPolicy {
                max_connections: 2,
                busy_timeout: Duration::from_millis(50),
                wal: true,
            },
        )
        .unwrap(),
    );
    let leases = SqliteLeaseStore::new(Arc::clone(&pool));
    leases.migrate().await.unwrap();
    let example = ExampleRepository::new(Arc::clone(&pool));
    example.migrate().await.unwrap();
    (pool, leases, example)
}

#[tokio::test]
async fn example_repository_idempotency_cas_and_scope() {
    let dir = tempfile::tempdir().unwrap();
    let (_pool, _leases, repo) = sqlite_stack(&dir.path().join("state.sqlite")).await;
    let alice = scope("alice", "home");
    let key = IdempotencyKey::parse("create-1").unwrap();
    let first = repo.create(&alice, "item-1", &key, "one").await.unwrap();
    let again = repo
        .create(&alice, "item-1", &key, "ignored")
        .await
        .unwrap();
    assert_eq!(first.payload, again.payload);
    assert_eq!(first.revision, 1);
    let updated = repo
        .compare_and_update(&alice, "item-1", Revision::new(1), "two")
        .await
        .unwrap();
    assert_eq!(updated.revision, 2);
    let stale = repo
        .compare_and_update(&alice, "item-1", Revision::new(1), "nope")
        .await
        .unwrap_err();
    assert!(matches!(stale, StorageError::Conflict { .. }));
    let leaked = repo.get(&scope("bob", "work"), "item-1").await.unwrap();
    assert!(leaked.is_none());
}

#[tokio::test]
async fn export_import_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let (_pool, _leases, repo) = sqlite_stack(&dir.path().join("state.sqlite")).await;
    let alice = scope("alice", "home");
    repo.create(&alice, "item-1", &IdempotencyKey::parse("k1").unwrap(), "p")
        .await
        .unwrap();
    let dump = repo.export_scope(&alice).await.unwrap();
    let dir2 = tempfile::tempdir().unwrap();
    let (_pool2, _leases2, repo2) = sqlite_stack(&dir2.path().join("state.sqlite")).await;
    repo2.import_scope(&alice, &dump).await.unwrap();
    let got = repo2.get(&alice, "item-1").await.unwrap().unwrap();
    assert_eq!(got.payload, "p");
}

#[tokio::test]
async fn commit_ambiguity_and_connection_loss() {
    let dir = tempfile::tempdir().unwrap();
    let (_pool, _leases, repo) = sqlite_stack(&dir.path().join("state.sqlite")).await;
    assert!(repo.uncommitted_is_dropped().await.unwrap());
    assert!(repo.committed_survives().await.unwrap());
    assert_eq!(
        StorageError::Timeout.may_have_committed(),
        CommitLikelihood::Maybe
    );
}

#[tokio::test]
async fn pool_saturation_and_health() {
    let dir = tempfile::tempdir().unwrap();
    let pool = SqlitePool::open(
        dir.path().join("state.sqlite"),
        SqlitePoolPolicy {
            max_connections: 1,
            busy_timeout: Duration::from_millis(5),
            wal: true,
        },
    )
    .unwrap();
    let _slot = pool.try_hold_slot().unwrap();
    let err = pool.try_hold_slot().unwrap_err();
    assert!(matches!(err, StorageError::CapacityExceeded));
    assert_eq!(pool.health().capability, "state_sqlite");
}

#[tokio::test]
async fn migration_rollback() {
    let dir = tempfile::tempdir().unwrap();
    let (pool, _, _) = sqlite_stack(&dir.path().join("state.sqlite")).await;
    pool.apply_migration(
        EXAMPLE_STORE_ID,
        2,
        "CREATE TABLE IF NOT EXISTS example_extra (id TEXT PRIMARY KEY);",
    )
    .await
    .unwrap();
    assert_eq!(pool.schema_version(EXAMPLE_STORE_ID).unwrap(), 2);
    pool.rollback_migration(EXAMPLE_STORE_ID, 1, "DROP TABLE IF EXISTS example_extra;")
        .await
        .unwrap();
    assert_eq!(pool.schema_version(EXAMPLE_STORE_ID).unwrap(), 1);
}

#[tokio::test]
async fn sqlite_lease_fencing_monotonic_and_forced_loss() {
    let dir = tempfile::tempdir().unwrap();
    let (_pool, leases, _) = sqlite_stack(&dir.path().join("state.sqlite")).await;
    let resource = LeaseResource::parse("scope-alice-home").unwrap();
    let a = leases
        .acquire(
            resource.clone(),
            OwnerId::parse("proc-a").unwrap(),
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(a.generation, 1);
    let refused = leases
        .acquire(
            resource.clone(),
            OwnerId::parse("proc-b").unwrap(),
            Duration::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(matches!(refused, StorageError::Conflict { .. }));
    leases.release(a).await.unwrap();
    let b = leases
        .acquire(
            resource.clone(),
            OwnerId::parse("proc-b").unwrap(),
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(b.generation, 2);
    leases
        .force_generation(resource.as_str(), 99)
        .await
        .unwrap();
    let lost = leases.renew(&b, Duration::from_secs(30)).await.unwrap_err();
    assert!(matches!(lost, StorageError::LeaseLost { .. }));
}

#[tokio::test]
async fn two_process_sql_lease_contention() {
    let resource = LeaseResource::parse("scope-alice-home").unwrap();
    if let Ok(path) = std::env::var("MAGICIAN_STATE_CHILD_DB") {
        let pool = Arc::new(SqlitePool::open(path, SqlitePoolPolicy::default()).unwrap());
        let leases = SqliteLeaseStore::new(pool);
        let err = leases
            .acquire(
                resource,
                OwnerId::parse("proc-b").unwrap(),
                Duration::from_secs(5),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, StorageError::Conflict { .. }));
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.sqlite");
    let (_pool, leases, _) = sqlite_stack(&path).await;
    let _token = leases
        .acquire(
            resource,
            OwnerId::parse("proc-a").unwrap(),
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .env("MAGICIAN_STATE_CHILD_DB", &path)
        .args(["two_process_sql_lease_contention", "--exact"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr={} stdout={}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn postgres_tls_required_and_dsn_redacted() {
    let err = open_postgres_config(
        "postgres://user:s3cret@localhost/db?sslmode=disable",
        PostgresOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(err, StorageError::PermissionDenied));
    assert!(!format!("{err:?}").contains("s3cret"));
    let pool = open_postgres_config(
        "postgres://user:s3cret@localhost/db?sslmode=require",
        PostgresOptions::default(),
    )
    .unwrap();
    let rendered = format!("{pool:?}");
    assert!(!rendered.contains("s3cret"));
    assert!(rendered.contains("redacted"));
}

#[test]
fn postgres_from_local_profile_is_unsupported() {
    let profile = magician_storage::resolve(ResolveOptions {
        cli_path: None,
        env_path: None,
        allow_ambient: false,
    })
    .unwrap();
    let err = open_postgres_from_profile(&profile, PostgresOptions::default()).unwrap_err();
    assert!(matches!(err, StorageError::UnsupportedCapability));
}

#[test]
fn magician_bin_does_not_depend_on_state_crate() {
    let toml = include_str!("../../magician-bin/Cargo.toml");
    assert!(!toml.contains("magician-storage-state"));
}
