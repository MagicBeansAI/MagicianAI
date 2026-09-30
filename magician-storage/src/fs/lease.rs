use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fs4::FileExt;
use serde::{Deserialize, Serialize};

use crate::error::StorageError;
use crate::lease::{LeaseObservation, LeaseResource, LeaseStore, LeaseToken, OwnerId};

#[derive(Serialize, Deserialize)]
struct LeaseRecord {
    owner: String,
    generation: u64,
    expires_at: DateTime<Utc>,
}

struct HeldLease {
    /// Exclusive flock is released when this file is dropped.
    _file: File,
}

pub struct LocalLeaseStore {
    root: PathBuf,
    held: Mutex<HashMap<String, HeldLease>>,
}

impl LocalLeaseStore {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            held: Mutex::new(HashMap::new()),
        }
    }

    fn paths(&self, resource: &LeaseResource) -> Result<(PathBuf, PathBuf), StorageError> {
        let lock = super::paths::join_encoded(&self.root, &format!("{}.lock", resource.as_str()))?;
        let rec =
            super::paths::join_encoded(&self.root, &format!("{}.lease.json", resource.as_str()))?;
        Ok((lock, rec))
    }

    fn read_record(path: &PathBuf) -> Result<Option<LeaseRecord>, StorageError> {
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(StorageError::backend(err.to_string())),
        };
        let mut buf = String::new();
        file.read_to_string(&mut buf)
            .map_err(|err| StorageError::backend(err.to_string()))?;
        if buf.trim().is_empty() {
            return Ok(None);
        }
        serde_json::from_str(&buf)
            .map(Some)
            .map_err(|err| StorageError::Corrupt {
                detail: err.to_string(),
            })
    }

    fn write_record(path: &PathBuf, record: &LeaseRecord) -> Result<(), StorageError> {
        let tmp = path.with_extension(format!("json.tmp.{}", uuid::Uuid::new_v4().simple()));
        let mut file = File::create(&tmp).map_err(|err| StorageError::backend(err.to_string()))?;
        let bytes = serde_json::to_vec_pretty(record)
            .map_err(|err| StorageError::backend(err.to_string()))?;
        file.write_all(&bytes)
            .map_err(|err| StorageError::backend(err.to_string()))?;
        file.sync_all()
            .map_err(|err| StorageError::backend(err.to_string()))?;
        drop(file);
        std::fs::rename(&tmp, path).map_err(|err| StorageError::backend(err.to_string()))?;
        if let Some(parent) = path.parent() {
            let dir = File::open(parent).map_err(|err| StorageError::backend(err.to_string()))?;
            dir.sync_all()
                .map_err(|err| StorageError::backend(err.to_string()))?;
        }
        Ok(())
    }
}

#[async_trait]
impl LeaseStore for LocalLeaseStore {
    async fn acquire(
        &self,
        resource: LeaseResource,
        owner: OwnerId,
        ttl: Duration,
    ) -> Result<LeaseToken, StorageError> {
        let (lock_path, rec_path) = self.paths(&resource)?;
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| StorageError::backend(err.to_string()))?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|err| StorageError::backend(err.to_string()))?;
        if file.try_lock_exclusive().is_err() {
            return Err(StorageError::Conflict {
                expected: None,
                actual: Some("lease held".into()),
            });
        }
        let previous = Self::read_record(&rec_path)?;
        let generation = previous
            .map(|r| r.generation.saturating_add(1))
            .unwrap_or(1);
        let token = LeaseToken {
            resource: resource.clone(),
            generation,
            expires_at: Utc::now()
                + chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::seconds(1)),
            owner: owner.clone(),
        };
        Self::write_record(
            &rec_path,
            &LeaseRecord {
                owner: owner.as_str().to_string(),
                generation,
                expires_at: token.expires_at,
            },
        )?;
        self.held
            .lock()
            .expect("lease map")
            .insert(resource.as_str().to_string(), HeldLease { _file: file });
        Ok(token)
    }

    async fn renew(&self, token: &LeaseToken, ttl: Duration) -> Result<LeaseToken, StorageError> {
        let held = self.held.lock().expect("lease map");
        if !held.contains_key(token.resource.as_str()) {
            return Err(StorageError::LeaseLost {
                resource: token.resource.as_str().to_string(),
                generation: token.generation,
            });
        }
        drop(held);
        let (_, rec_path) = self.paths(&token.resource)?;
        let current = Self::read_record(&rec_path)?.ok_or_else(|| StorageError::LeaseLost {
            resource: token.resource.as_str().to_string(),
            generation: token.generation,
        })?;
        if current.generation != token.generation {
            return Err(StorageError::LeaseLost {
                resource: token.resource.as_str().to_string(),
                generation: current.generation,
            });
        }
        let now = Utc::now();
        let requested =
            now + chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::seconds(1));
        // Generation is the fence. Expiry is best-effort and must not jump past
        // the already-granted instant if the clock moved backwards.
        let expires_at = if requested < token.expires_at {
            requested
        } else if now < token.expires_at {
            token.expires_at
        } else {
            requested
        };
        let mut next = token.clone();
        next.expires_at = expires_at;
        Self::write_record(
            &rec_path,
            &LeaseRecord {
                owner: token.owner.as_str().to_string(),
                generation: token.generation,
                expires_at,
            },
        )?;
        Ok(next)
    }

    async fn release(&self, token: LeaseToken) -> Result<(), StorageError> {
        let (_, rec_path) = self.paths(&token.resource)?;
        let current = Self::read_record(&rec_path)?;
        if let Some(current) = current {
            if current.generation != token.generation {
                return Err(StorageError::LeaseLost {
                    resource: token.resource.as_str().to_string(),
                    generation: current.generation,
                });
            }
        }
        self.held
            .lock()
            .expect("lease map")
            .remove(token.resource.as_str());
        Ok(())
    }

    async fn inspect(
        &self,
        resource: &LeaseResource,
    ) -> Result<Option<LeaseObservation>, StorageError> {
        let (_, rec_path) = self.paths(resource)?;
        Ok(
            Self::read_record(&rec_path)?.map(|record| LeaseObservation {
                token: LeaseToken {
                    resource: resource.clone(),
                    generation: record.generation,
                    expires_at: record.expires_at,
                    owner: OwnerId::parse(&record.owner)
                        .unwrap_or_else(|_| OwnerId::parse("unknown").expect("static")),
                },
            }),
        )
    }
}
