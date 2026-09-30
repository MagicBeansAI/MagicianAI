//! Task 20: two scopes stay isolated on the remote-shaped SQLite repository.

use std::sync::Arc;
use std::time::Duration;

use magician_storage::{IdempotencyKey, PrincipalId, ScopeId, WorkspaceId};
use magician_storage_state::{ExampleRepository, SqlitePool, SqlitePoolPolicy};

#[tokio::test]
async fn concurrent_scopes_do_not_share_rows() {
    let dir = tempfile::tempdir().unwrap();
    let pool = Arc::new(
        SqlitePool::open(
            dir.path().join("state.sqlite"),
            SqlitePoolPolicy {
                max_connections: 2,
                busy_timeout: Duration::from_millis(50),
                wal: true,
            },
        )
        .unwrap(),
    );
    let repo = ExampleRepository::new(Arc::clone(&pool));
    repo.migrate().await.unwrap();
    let alice = ScopeId::new(
        PrincipalId::parse("alice").unwrap(),
        WorkspaceId::parse("home").unwrap(),
    );
    let bob = ScopeId::new(
        PrincipalId::parse("bob").unwrap(),
        WorkspaceId::parse("work").unwrap(),
    );
    repo.create(
        &alice,
        "item-1",
        &IdempotencyKey::parse("a1").unwrap(),
        "alice",
    )
    .await
    .unwrap();
    repo.create(&bob, "item-1", &IdempotencyKey::parse("b1").unwrap(), "bob")
        .await
        .unwrap();
    assert_eq!(
        repo.get(&alice, "item-1").await.unwrap().unwrap().payload,
        "alice"
    );
    assert_eq!(
        repo.get(&bob, "item-1").await.unwrap().unwrap().payload,
        "bob"
    );
}
