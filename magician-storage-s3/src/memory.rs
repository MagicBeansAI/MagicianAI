use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bytes::Bytes;

use magician_storage::object::{ByteRange, DeleteCondition, ObjectVersion, PutCondition};
use magician_storage::StorageError;

use crate::backend::{digest_of, BlobMeta, BlobStore};

struct ObjectRec {
    bytes: Bytes,
    meta: BlobMeta,
}

struct Multipart {
    key: String,
    created: Instant,
    parts: BTreeMap<i32, Bytes>,
}

struct State {
    objects: BTreeMap<String, ObjectRec>,
    tombstones: BTreeMap<String, String>,
    uploads: HashMap<String, Multipart>,
}

/// Hermetic S3-semantic blob store: versions, conditional writes, multipart.
pub struct MemoryBlobStore {
    inner: Mutex<State>,
}

impl MemoryBlobStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(State {
                objects: BTreeMap::new(),
                tombstones: BTreeMap::new(),
                uploads: HashMap::new(),
            }),
        }
    }
}

impl Default for MemoryBlobStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl BlobStore for MemoryBlobStore {
    async fn put(
        &self,
        key: &str,
        bytes: Bytes,
        condition: PutCondition,
    ) -> Result<BlobMeta, StorageError> {
        let mut state = self.inner.lock().expect("memory blob");
        apply_condition(&state, key, &condition)?;
        Ok(insert_object(&mut state, key, bytes))
    }

    async fn get(
        &self,
        key: &str,
        range: Option<ByteRange>,
    ) -> Result<(BlobMeta, Bytes), StorageError> {
        let state = self.inner.lock().expect("memory blob");
        let rec = state.objects.get(key).ok_or(StorageError::NotFound)?;
        let slice = slice_range(&rec.bytes, range)?;
        Ok((rec.meta.clone(), Bytes::from(slice)))
    }

    async fn head(&self, key: &str) -> Result<Option<BlobMeta>, StorageError> {
        let state = self.inner.lock().expect("memory blob");
        Ok(state.objects.get(key).map(|rec| rec.meta.clone()))
    }

    async fn delete(
        &self,
        key: &str,
        condition: DeleteCondition,
    ) -> Result<ObjectVersion, StorageError> {
        let mut state = self.inner.lock().expect("memory blob");
        match condition {
            DeleteCondition::Existing => {},
            DeleteCondition::ExpectedVersion(expected) => {
                let actual = state.objects.get(key).ok_or(StorageError::NotFound)?;
                if actual.meta.version.as_str() != expected.as_str() {
                    return Err(StorageError::Conflict {
                        expected: Some(expected.as_str().to_string()),
                        actual: Some(actual.meta.version.as_str().to_string()),
                    });
                }
            },
        }
        let rec = state.objects.remove(key).ok_or(StorageError::NotFound)?;
        let tombstone = ObjectVersion::new(uuid::Uuid::new_v4().to_string())?;
        state
            .tombstones
            .insert(key.to_string(), tombstone.as_str().to_string());
        drop(rec);
        Ok(tombstone)
    }

    async fn list(
        &self,
        prefix: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Vec<BlobMeta>, StorageError> {
        let state = self.inner.lock().expect("memory blob");
        let mut out = Vec::new();
        for (key, rec) in &state.objects {
            if !key.starts_with(prefix) {
                continue;
            }
            if let Some(cursor) = cursor {
                if key.as_str() <= cursor {
                    continue;
                }
            }
            out.push(rec.meta.clone());
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    async fn create_multipart(&self, key: &str) -> Result<String, StorageError> {
        let mut state = self.inner.lock().expect("memory blob");
        let id = uuid::Uuid::new_v4().to_string();
        state.uploads.insert(
            id.clone(),
            Multipart {
                key: key.to_string(),
                created: Instant::now(),
                parts: BTreeMap::new(),
            },
        );
        Ok(id)
    }

    async fn upload_part(
        &self,
        upload_id: &str,
        part: i32,
        bytes: Bytes,
    ) -> Result<(), StorageError> {
        if part < 1 {
            return Err(StorageError::invalid_key("part number"));
        }
        let mut state = self.inner.lock().expect("memory blob");
        let upload = state
            .uploads
            .get_mut(upload_id)
            .ok_or(StorageError::NotFound)?;
        upload.parts.insert(part, bytes);
        Ok(())
    }

    async fn complete_multipart(
        &self,
        upload_id: &str,
        condition: PutCondition,
    ) -> Result<BlobMeta, StorageError> {
        let mut state = self.inner.lock().expect("memory blob");
        let upload = state
            .uploads
            .remove(upload_id)
            .ok_or(StorageError::NotFound)?;
        apply_condition(&state, &upload.key, &condition)?;
        let mut acc = Vec::new();
        for (_, part) in upload.parts {
            acc.extend_from_slice(&part);
        }
        Ok(insert_object(&mut state, &upload.key, Bytes::from(acc)))
    }

    async fn abort_multipart(&self, upload_id: &str) -> Result<(), StorageError> {
        let mut state = self.inner.lock().expect("memory blob");
        state
            .uploads
            .remove(upload_id)
            .ok_or(StorageError::NotFound)?;
        Ok(())
    }

    async fn cleanup_abandoned(&self, older_than: Duration) -> Result<u64, StorageError> {
        let mut state = self.inner.lock().expect("memory blob");
        let cutoff = Instant::now() - older_than;
        let stale: Vec<String> = state
            .uploads
            .iter()
            .filter(|(_, upload)| upload.created <= cutoff)
            .map(|(id, _)| id.clone())
            .collect();
        let count = stale.len() as u64;
        for id in stale {
            state.uploads.remove(&id);
        }
        Ok(count)
    }
}

fn apply_condition(state: &State, key: &str, condition: &PutCondition) -> Result<(), StorageError> {
    let existing = state.objects.get(key);
    match (condition, existing) {
        (PutCondition::Overwrite, _) => Ok(()),
        (PutCondition::CreateOnly, Some(rec)) => Err(StorageError::Conflict {
            expected: None,
            actual: Some(rec.meta.version.as_str().to_string()),
        }),
        (PutCondition::CreateOnly, None) => Ok(()),
        (PutCondition::ExpectedVersion(expected), Some(rec))
            if rec.meta.version.as_str() == expected.as_str() =>
        {
            Ok(())
        },
        (PutCondition::ExpectedVersion(expected), Some(rec)) => Err(StorageError::Conflict {
            expected: Some(expected.as_str().to_string()),
            actual: Some(rec.meta.version.as_str().to_string()),
        }),
        (PutCondition::ExpectedVersion(expected), None) => Err(StorageError::Conflict {
            expected: Some(expected.as_str().to_string()),
            actual: None,
        }),
    }
}

fn insert_object(state: &mut State, key: &str, bytes: Bytes) -> BlobMeta {
    let digest = digest_of(&bytes);
    let version = ObjectVersion::new(uuid::Uuid::new_v4().to_string()).expect("uuid version");
    let meta = BlobMeta {
        key: key.to_string(),
        len: bytes.len() as u64,
        digest,
        version,
    };
    state.tombstones.remove(key);
    state.objects.insert(
        key.to_string(),
        ObjectRec {
            bytes,
            meta: meta.clone(),
        },
    );
    meta
}

fn slice_range(bytes: &Bytes, range: Option<ByteRange>) -> Result<Vec<u8>, StorageError> {
    let Some(range) = range else {
        return Ok(bytes.to_vec());
    };
    let start = range.start as usize;
    if start > bytes.len() {
        return Err(StorageError::invalid_key("range start"));
    }
    let end = range
        .end_exclusive
        .map(|end| end as usize)
        .unwrap_or(bytes.len())
        .min(bytes.len());
    if end < start {
        return Err(StorageError::invalid_key("range end"));
    }
    Ok(bytes[start..end].to_vec())
}
