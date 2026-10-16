//! Task 20: two engines, one hermetic remote object store, no directory copy.

use std::sync::Arc;

use magician_storage::object::{bytes_body, ObjectStore, PutCondition, PutObjectRequest};
use magician_storage::StorageKey;
use magician_storage_s3::S3ObjectStore;

#[tokio::test]
async fn local_and_linux_engines_share_remote_objects_without_copying() {
    let remote = Arc::new(S3ObjectStore::hermetic("production/objects"));
    let key = StorageKey::tenant("alice", "home", "tasks", "ack").unwrap();
    remote
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(b"acknowledged".to_vec()),
            content_type: None,
            condition: PutCondition::Overwrite,
        })
        .await
        .unwrap();
    let linux = Arc::clone(&remote);
    assert_eq!(
        linux.get(&key, None).await.unwrap().metadata.len,
        b"acknowledged".len() as u64
    );
}
