use std::sync::Arc;
use std::time::Instant;

use magician_storage::object::{bytes_body, ObjectStore, PutCondition, PutObjectRequest};
use magician_storage::{
    assert_budget, assert_latency_budget, percentile_ms, BudgetLine, StorageKey,
};
use magician_storage_s3::S3ObjectStore;

#[tokio::test]
async fn hermetic_s3_object_latency_meets_gate3() {
    let store = Arc::new(S3ObjectStore::hermetic("gate3"));
    let key = StorageKey::tenant("alice", "home", "tasks", "blob").unwrap();
    let mut first = Vec::new();
    let mut complete = Vec::new();
    for _ in 0..8 {
        let started = Instant::now();
        store
            .put(PutObjectRequest {
                key: key.clone(),
                body: bytes_body(vec![b's'; 64 * 1024]),
                content_type: None,
                condition: PutCondition::Overwrite,
            })
            .await
            .unwrap();
        complete.push(started.elapsed().as_nanos());
        let started = Instant::now();
        let _ = store.get(&key, None).await.unwrap();
        first.push(started.elapsed().as_nanos());
    }
    assert_latency_budget(
        BudgetLine::ObjectSmallFirstByteP95Ms,
        percentile_ms(&first, 95),
    );
    assert_latency_budget(
        BudgetLine::ObjectSmallCompleteP95Ms,
        percentile_ms(&complete, 95),
    );
    assert_budget(BudgetLine::MaxInMemoryTransferBytes, 64 * 1024);
}
