use std::sync::Arc;
use std::time::{Duration, Instant};

use magician_storage::{
    assert_budget, assert_latency_budget, percentile_ms, BudgetLine, IdempotencyKey, PrincipalId,
    Revision, ScopeId, WorkspaceId,
};
use magician_storage_state::{ExampleRepository, SqlitePool, SqlitePoolPolicy};

#[tokio::test]
async fn sqlite_repository_read_mutation_meets_gate3() {
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
    let scope = ScopeId::new(
        PrincipalId::parse("alice").unwrap(),
        WorkspaceId::parse("home").unwrap(),
    );
    let mut mut_ns = Vec::new();
    let mut read_ns = Vec::new();
    for i in 0..16 {
        let key = IdempotencyKey::parse(&format!("k{i}")).unwrap();
        let started = Instant::now();
        repo.create(&scope, &format!("item-{i}"), &key, "one")
            .await
            .unwrap();
        mut_ns.push(started.elapsed().as_nanos());
        let started = Instant::now();
        let _ = repo.get(&scope, &format!("item-{i}")).await.unwrap();
        read_ns.push(started.elapsed().as_nanos());
    }
    assert_latency_budget(BudgetLine::RepoMutP50Ms, percentile_ms(&mut_ns, 50));
    assert_latency_budget(BudgetLine::RepoMutP95Ms, percentile_ms(&mut_ns, 95));
    assert_latency_budget(BudgetLine::RepoMutP99Ms, percentile_ms(&mut_ns, 99));
    assert_latency_budget(BudgetLine::RepoReadP50Ms, percentile_ms(&read_ns, 50));
    assert_latency_budget(BudgetLine::RepoReadP95Ms, percentile_ms(&read_ns, 95));
    assert_latency_budget(BudgetLine::RepoReadP99Ms, percentile_ms(&read_ns, 99));
    let cas_started = Instant::now();
    repo.compare_and_update(&scope, "item-0", Revision::new(1), "two")
        .await
        .unwrap();
    assert_latency_budget(
        BudgetLine::RepoMutP95Ms,
        cas_started.elapsed().as_millis() as u64,
    );
    assert_budget(BudgetLine::OutboxBacklog, 16);
}
