use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use magician_storage::{MigrationPhase, StorageError, StorageMigrationRecord};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

const REDACT_MARKERS: &[&str] = &[
    "password",
    "secret",
    "token",
    "credential",
    "authorization",
    "dsn",
    "api_key",
    "private_key",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseEvidence {
    pub complete: bool,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub record: StorageMigrationRecord,
    pub fence_generation: u64,
    pub evidence: BTreeMap<String, PhaseEvidence>,
    pub export_digest: Option<String>,
}

impl LedgerEntry {
    pub fn phase_complete(&self, phase: MigrationPhase) -> bool {
        self.evidence
            .get(phase.as_str())
            .map(|item| item.complete)
            .unwrap_or(false)
    }
}

pub struct FileLedger {
    root: PathBuf,
}

impl FileLedger {
    pub async fn open(root: impl AsRef<Path>) -> Result<Self, StorageError> {
        let root = root.as_ref().join("migrations");
        tokio::fs::create_dir_all(&root).await.map_err(io_err)?;
        Ok(Self { root })
    }

    pub async fn get(&self, id: Uuid) -> Result<LedgerEntry, StorageError> {
        let path = self.record_path(id);
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|_| StorageError::NotFound)?;
        serde_json::from_slice(&bytes).map_err(|err| StorageError::Corrupt {
            detail: err.to_string(),
        })
    }

    pub async fn put(&self, entry: &LedgerEntry) -> Result<(), StorageError> {
        let path = self.record_path(entry.record.migration_id);
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(entry)
            .map_err(|err| StorageError::backend(err.to_string()))?;
        tokio::fs::write(&tmp, bytes).await.map_err(io_err)?;
        tokio::fs::rename(&tmp, &path).await.map_err(io_err)
    }

    pub async fn put_export(&self, id: Uuid, bytes: &[u8]) -> Result<(), StorageError> {
        let path = self.export_path(id);
        let tmp = path.with_extension("export.tmp");
        tokio::fs::write(&tmp, bytes).await.map_err(io_err)?;
        tokio::fs::rename(&tmp, &path).await.map_err(io_err)
    }

    pub async fn get_export(&self, id: Uuid) -> Result<Vec<u8>, StorageError> {
        tokio::fs::read(self.export_path(id))
            .await
            .map_err(|_| StorageError::NotFound)
    }

    pub async fn find_open(
        &self,
        store_id: &str,
        scope: &magician_storage::StorageScope,
        source_profile: &str,
        target_profile: &str,
    ) -> Result<Option<LedgerEntry>, StorageError> {
        let mut dir = tokio::fs::read_dir(&self.root).await.map_err(io_err)?;
        while let Some(entry) = dir.next_entry().await.map_err(io_err)? {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.ends_with(".json") {
                continue;
            }
            let bytes = tokio::fs::read(entry.path()).await.map_err(io_err)?;
            let item: LedgerEntry =
                serde_json::from_slice(&bytes).map_err(|err| StorageError::Corrupt {
                    detail: err.to_string(),
                })?;
            if item.record.store_id.as_str() == store_id
                && item.record.scope == *scope
                && item.record.source_profile == source_profile
                && item.record.target_profile == target_profile
                && !item.record.phase.is_terminal()
                && !matches!(
                    item.record.phase,
                    magician_storage::MigrationPhase::RollbackPending
                        | magician_storage::MigrationPhase::RolledBack
                )
            {
                return Ok(Some(item));
            }
        }
        Ok(None)
    }

    fn record_path(&self, id: Uuid) -> PathBuf {
        self.root.join(format!("{id}.json"))
    }

    fn export_path(&self, id: Uuid) -> PathBuf {
        self.root.join(format!("{id}.export"))
    }
}

pub fn sanitize_json(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, child) in map {
                if redact_key(&key) {
                    out.insert(key, Value::String("[redacted]".into()));
                } else {
                    out.insert(key, sanitize_json(child));
                }
            }
            Value::Object(out)
        },
        Value::Array(items) => Value::Array(items.into_iter().map(sanitize_json).collect()),
        other => other,
    }
}

fn redact_key(key: &str) -> bool {
    let lowered = key.to_ascii_lowercase();
    REDACT_MARKERS.iter().any(|marker| lowered.contains(marker))
}

fn io_err(err: std::io::Error) -> StorageError {
    StorageError::backend(err.to_string())
}
