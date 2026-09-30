use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bytes::Bytes;
use fs4::FileExt;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::error::StorageError;
use crate::gc::{GcPolicy, GcReport, ObjectReference};
use crate::identifiers::{ContentDigest, DigestAlgorithm, StorageKey};
use crate::object::{
    ByteRange, DeleteCondition, DeleteReceipt, ObjectMetadata, ObjectRead, ObjectStore,
    ObjectVersion, PutCondition, PutObjectReceipt, PutObjectRequest, StoragePrefix,
};

use super::paths::{digest_of, key_path};

const SIDECAR_SCHEMA: u32 = 1;
const SIDECAR_SUFFIX: &str = ".objmeta.json";
const LOCK_SUFFIX: &str = ".objlock";
const QUARANTINE_SUFFIX: &str = ".objquarantine";
const PUBLISH_INFIX: &str = ".objpub.";
const RETAIN_INFIX: &str = ".objretain.";

/// Local object bytes stay at the encoded key path. Adapter-owned siblings
/// (`*.objmeta.json`, `*.objlock`, `*.objpub.*`, `*.objquarantine`,
/// `*.objretain.*`) are not locators and are skipped by diagnostic listing.
pub struct LocalObjectStore {
    root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ObjectSidecar {
    schema: u32,
    state: SidecarState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    len: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    digest: Option<ContentDigest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending: Option<PendingPublish>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tombstoned_unix_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retained_version: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SidecarState {
    Live,
    Publishing,
    Tombstone,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingPublish {
    version: String,
    len: u64,
    digest: ContentDigest,
    file: String,
}

#[derive(Serialize)]
struct QuarantineRecord {
    reason: &'static str,
    detail: String,
    expected: Option<String>,
    actual: Option<String>,
}

struct LiveObject {
    metadata: ObjectMetadata,
    bytes: Vec<u8>,
}

enum Loaded {
    Absent,
    Live(LiveObject),
    Tombstone,
}

impl LocalObjectStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Bytes of a logically deleted object that still sit in the restore window.
    pub fn retained_generation(
        &self,
        key: &StorageKey,
    ) -> Result<Option<(ObjectVersion, Vec<u8>)>, StorageError> {
        let path = key_path(&self.root, key)?;
        let Some(sidecar) = read_sidecar(&path)? else {
            return Ok(None);
        };
        let Some(version) = sidecar.retained_version.as_deref() else {
            return Ok(None);
        };
        let retain = retain_path(&path, version);
        match read_bytes(&retain)? {
            Some(bytes) => Ok(Some((ObjectVersion::new(version)?, bytes))),
            None => Ok(None),
        }
    }

    /// Physically collect unreferenced retain files after the restore window.
    /// Live objects and referenced versions are never collected.
    pub fn collect_unreferenced(
        &self,
        policy: GcPolicy,
        referenced: &[ObjectReference],
        now_unix_ms: i64,
    ) -> Result<GcReport, StorageError> {
        let mut report = GcReport::default();
        let mut retains = Vec::new();
        collect_retain_files(&self.root, &mut retains)?;
        for retain in retains {
            report.scanned += 1;
            let Some(object_path) = object_path_from_retain(&retain) else {
                continue;
            };
            let Ok(rel) = object_path.strip_prefix(&self.root) else {
                continue;
            };
            let encoded = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let Ok(key) = StorageKey::decode(&encoded) else {
                continue;
            };
            let Some(version_raw) = retain_version_from_path(&retain) else {
                continue;
            };
            let Ok(version) = ObjectVersion::new(version_raw) else {
                continue;
            };
            if referenced
                .iter()
                .any(|item| item.key == key && item.version.as_str() == version.as_str())
            {
                report.retained_referenced += 1;
                continue;
            }
            if let Loaded::Live(live) = load_generation(&object_path, &key)? {
                if live.metadata.version.as_str() == version.as_str() {
                    report.retained_live_replacement += 1;
                    continue;
                }
            }
            let sidecar = read_sidecar(&object_path)?;
            let Some(tombstoned) = sidecar.as_ref().and_then(|row| row.tombstoned_unix_ms) else {
                report.retained_fresh += 1;
                continue;
            };
            let age_ms = now_unix_ms.saturating_sub(tombstoned);
            if age_ms < policy.tombstone_retain.as_millis() as i64 {
                report.retained_fresh += 1;
                continue;
            }
            match std::fs::remove_file(&retain) {
                Ok(()) => report.collected += 1,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {},
                Err(err) => return Err(StorageError::backend(err.to_string())),
            }
        }
        Ok(report)
    }
}

#[async_trait]
impl ObjectStore for LocalObjectStore {
    async fn head(&self, key: &StorageKey) -> Result<Option<ObjectMetadata>, StorageError> {
        let path = key_path(&self.root, key)?;
        let _lock = lock_key(&path).await?;
        match load_generation(&path, key)? {
            Loaded::Live(live) => Ok(Some(live.metadata)),
            Loaded::Absent | Loaded::Tombstone => Ok(None),
        }
    }

    async fn get(
        &self,
        key: &StorageKey,
        range: Option<ByteRange>,
    ) -> Result<ObjectRead, StorageError> {
        let path = key_path(&self.root, key)?;
        let _lock = lock_key(&path).await?;
        let Loaded::Live(live) = load_generation(&path, key)? else {
            return Err(StorageError::NotFound);
        };
        let slice = match range {
            None => live.bytes,
            Some(range) => slice_range(&live.bytes, range)?,
        };
        Ok(ObjectRead {
            metadata: live.metadata,
            body: crate::object::bytes_body(Bytes::from(slice)),
        })
    }

    async fn put(&self, request: PutObjectRequest) -> Result<PutObjectReceipt, StorageError> {
        let path = key_path(&self.root, &request.key)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|err| StorageError::backend(err.to_string()))?;
        }
        let _lock = lock_key(&path).await?;
        let loaded = load_generation(&path, &request.key)?;
        match (&request.condition, &loaded) {
            (PutCondition::CreateOnly, Loaded::Live(live)) => {
                return Err(conflict(None, Some(live.metadata.version.as_str())));
            },
            (PutCondition::ExpectedVersion(expected), Loaded::Live(live))
                if live.metadata.version.as_str() != expected.as_str() =>
            {
                return Err(conflict(
                    Some(expected.as_str()),
                    Some(live.metadata.version.as_str()),
                ));
            },
            (PutCondition::ExpectedVersion(expected), Loaded::Absent | Loaded::Tombstone) => {
                return Err(conflict(Some(expected.as_str()), None));
            },
            (PutCondition::Overwrite, _)
            | (PutCondition::CreateOnly, _)
            | (PutCondition::ExpectedVersion(_), Loaded::Live(_)) => {},
        }

        let new_version = new_version()?;
        let pending_name = pending_filename(&path, new_version.as_str());
        let pending_path = path
            .parent()
            .map(|parent| parent.join(&pending_name))
            .ok_or_else(|| StorageError::invalid_key("object path"))?;

        let mut file = tokio::fs::File::create(&pending_path)
            .await
            .map_err(|err| StorageError::backend(err.to_string()))?;
        let mut hasher = blake3::Hasher::new();
        let mut len = 0u64;
        let mut body = request.body;
        while let Some(chunk) = body.next().await {
            let chunk = chunk?;
            hasher.update(&chunk);
            len += chunk.len() as u64;
            file.write_all(&chunk)
                .await
                .map_err(|err| StorageError::backend(err.to_string()))?;
        }
        file.flush()
            .await
            .map_err(|err| StorageError::backend(err.to_string()))?;
        file.sync_all()
            .await
            .map_err(|err| StorageError::backend(err.to_string()))?;
        drop(file);

        let digest = ContentDigest {
            algorithm: DigestAlgorithm::Blake3,
            hex: hasher.finalize().to_hex().to_string(),
        };
        let previous = match &loaded {
            Loaded::Live(live) => Some(&live.metadata),
            Loaded::Absent | Loaded::Tombstone => None,
        };
        let keep_retain = read_sidecar(&path)?;
        write_sidecar(
            &path,
            &ObjectSidecar {
                schema: SIDECAR_SCHEMA,
                state: SidecarState::Publishing,
                version: previous.map(|meta| meta.version.as_str().to_string()),
                len: previous.map(|meta| meta.len),
                digest: previous.map(|meta| meta.digest.clone()),
                pending: Some(PendingPublish {
                    version: new_version.as_str().to_string(),
                    len,
                    digest: digest.clone(),
                    file: pending_name,
                }),
                tombstoned_unix_ms: keep_retain
                    .as_ref()
                    .and_then(|sidecar| sidecar.tombstoned_unix_ms),
                retained_version: keep_retain
                    .as_ref()
                    .and_then(|sidecar| sidecar.retained_version.clone()),
            },
        )?;
        std::fs::rename(&pending_path, &path)
            .map_err(|err| StorageError::backend(err.to_string()))?;
        fsync_parent(&path)?;
        write_sidecar(
            &path,
            &ObjectSidecar {
                schema: SIDECAR_SCHEMA,
                state: SidecarState::Live,
                version: Some(new_version.as_str().to_string()),
                len: Some(len),
                digest: Some(digest.clone()),
                pending: None,
                tombstoned_unix_ms: keep_retain
                    .as_ref()
                    .and_then(|sidecar| sidecar.tombstoned_unix_ms),
                retained_version: keep_retain
                    .as_ref()
                    .and_then(|sidecar| sidecar.retained_version.clone()),
            },
        )?;

        let correlation = match request.condition {
            PutCondition::Overwrite => "local-overwrite",
            PutCondition::CreateOnly => "local-create",
            PutCondition::ExpectedVersion(_) => "local-cas",
        };
        Ok(PutObjectReceipt {
            metadata: ObjectMetadata {
                key: request.key,
                len,
                digest,
                version: new_version,
            },
            correlation: correlation.into(),
        })
    }

    async fn delete(
        &self,
        key: &StorageKey,
        condition: DeleteCondition,
    ) -> Result<DeleteReceipt, StorageError> {
        let path = key_path(&self.root, key)?;
        let _lock = lock_key(&path).await?;
        let loaded = load_generation(&path, key)?;
        match (condition, loaded) {
            (DeleteCondition::Existing, Loaded::Live(_)) => {},
            (DeleteCondition::ExpectedVersion(expected), Loaded::Live(live))
                if live.metadata.version.as_str() == expected.as_str() => {},
            (DeleteCondition::ExpectedVersion(expected), Loaded::Live(live)) => {
                return Err(conflict(
                    Some(expected.as_str()),
                    Some(live.metadata.version.as_str()),
                ));
            },
            (DeleteCondition::Existing | DeleteCondition::ExpectedVersion(_), _) => {
                return Err(StorageError::NotFound);
            },
        }
        let tombstone_version = new_version()?;
        let retained_version = match load_generation(&path, key)? {
            Loaded::Live(live) => {
                let retain = retain_path(&path, live.metadata.version.as_str());
                match std::fs::rename(&path, &retain) {
                    Ok(()) => Some(live.metadata.version.as_str().to_string()),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
                    Err(err) => return Err(StorageError::backend(err.to_string())),
                }
            },
            _ => None,
        };
        if path.exists() {
            match std::fs::remove_file(&path) {
                Ok(()) => {},
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {},
                Err(err) => return Err(StorageError::backend(err.to_string())),
            }
        }
        write_sidecar(
            &path,
            &ObjectSidecar {
                schema: SIDECAR_SCHEMA,
                state: SidecarState::Tombstone,
                version: Some(tombstone_version.as_str().to_string()),
                len: None,
                digest: None,
                pending: None,
                tombstoned_unix_ms: Some(unix_ms_now()),
                retained_version,
            },
        )?;
        Ok(DeleteReceipt {
            key: key.clone(),
            tombstone_version,
        })
    }

    async fn list_diagnostic(
        &self,
        prefix: &StoragePrefix,
        cursor: Option<String>,
        limit: usize,
    ) -> Result<Vec<ObjectMetadata>, StorageError> {
        let limit = limit.min(1000);
        let mut keys = Vec::new();
        collect_encoded(&self.root, &self.root, &mut keys)?;
        keys.sort();
        let mut out = Vec::new();
        for encoded in keys {
            if !encoded.starts_with(&prefix.encoded) {
                continue;
            }
            if let Some(cursor) = &cursor {
                if encoded.as_str() <= cursor.as_str() {
                    continue;
                }
            }
            if let Ok(key) = StorageKey::decode(&encoded) {
                if let Some(meta) = self.head(&key).await? {
                    out.push(meta);
                }
            }
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }
}

fn load_generation(path: &Path, key: &StorageKey) -> Result<Loaded, StorageError> {
    match read_sidecar(path)? {
        None => match read_bytes(path)? {
            Some(bytes) => import_legacy(path, key, bytes),
            None => Ok(Loaded::Absent),
        },
        Some(sidecar) => match sidecar.state {
            SidecarState::Live => open_live(path, key, &sidecar),
            SidecarState::Tombstone => {
                required_version(&sidecar, path)?;
                Ok(Loaded::Tombstone)
            },
            SidecarState::Publishing => recover_publishing(path, key, sidecar),
        },
    }
}

fn import_legacy(path: &Path, key: &StorageKey, bytes: Vec<u8>) -> Result<Loaded, StorageError> {
    let digest = digest_of(&bytes);
    let len = bytes.len() as u64;
    let version = new_version()?;
    write_live_sidecar(path, version.as_str(), len, &digest, None)?;
    Ok(Loaded::Live(LiveObject {
        metadata: ObjectMetadata {
            key: key.clone(),
            len,
            digest,
            version,
        },
        bytes,
    }))
}

fn open_live(
    path: &Path,
    key: &StorageKey,
    sidecar: &ObjectSidecar,
) -> Result<Loaded, StorageError> {
    let version = required_version(sidecar, path)?;
    let (len, digest) = required_content(sidecar, path)?;
    match read_bytes(path)? {
        Some(bytes) if bytes_match(&bytes, &digest, len) => Ok(Loaded::Live(LiveObject {
            metadata: ObjectMetadata {
                key: key.clone(),
                len,
                digest,
                version,
            },
            bytes,
        })),
        Some(bytes) => Err(integrity(
            path,
            "digest_mismatch",
            Some(fingerprint(&digest, len)),
            Some(fingerprint(&digest_of(&bytes), bytes.len() as u64)),
        )),
        None => Err(integrity(
            path,
            "digest_mismatch",
            Some(fingerprint(&digest, len)),
            Some("missing".into()),
        )),
    }
}

fn recover_publishing(
    path: &Path,
    key: &StorageKey,
    sidecar: ObjectSidecar,
) -> Result<Loaded, StorageError> {
    let pending = sidecar
        .pending
        .as_ref()
        .ok_or_else(|| corrupt(path, "publishing sidecar missing pending"))?;
    let pending_path = pending_path(path, &pending.file)?;
    let pending_bytes = read_bytes(&pending_path)?;
    let pending_ok = pending_bytes
        .as_ref()
        .is_some_and(|bytes| bytes_match(bytes, &pending.digest, pending.len));
    let current_bytes = read_bytes(path)?;
    let current_is_pending = current_bytes
        .as_ref()
        .is_some_and(|bytes| bytes_match(bytes, &pending.digest, pending.len));
    let current_is_old = match (&current_bytes, &sidecar.digest, sidecar.len) {
        (Some(bytes), Some(digest), Some(len)) => bytes_match(bytes, digest, len),
        _ => false,
    };

    if pending_ok && !current_is_pending {
        std::fs::rename(&pending_path, path)
            .map_err(|err| StorageError::backend(err.to_string()))?;
        fsync_file_and_parent(path)?;
        return commit_pending(path, key, pending);
    }
    if current_is_pending {
        let _ = std::fs::remove_file(&pending_path);
        return commit_pending(path, key, pending);
    }
    if current_is_old {
        let _ = std::fs::remove_file(&pending_path);
        let version = required_version(&sidecar, path)?;
        let (len, digest) = required_content(&sidecar, path)?;
        write_live_sidecar(path, version.as_str(), len, &digest, Some(&sidecar))?;
        return open_live(
            path,
            key,
            &ObjectSidecar {
                state: SidecarState::Live,
                pending: None,
                ..sidecar
            },
        );
    }
    if current_bytes.is_none() && sidecar.version.is_none() {
        if sidecar.retained_version.is_some() {
            return Ok(Loaded::Tombstone);
        }
        let _ = std::fs::remove_file(sibling(path, SIDECAR_SUFFIX));
        let _ = std::fs::remove_file(&pending_path);
        return Ok(Loaded::Absent);
    }
    Err(integrity(
        path,
        "interrupted_publish",
        sidecar
            .digest
            .as_ref()
            .map(|digest| fingerprint(digest, sidecar.len.unwrap_or(0))),
        Some("unresolved".into()),
    ))
}

fn commit_pending(
    path: &Path,
    key: &StorageKey,
    pending: &PendingPublish,
) -> Result<Loaded, StorageError> {
    let version = ObjectVersion::new(pending.version.clone())
        .map_err(|_| corrupt(path, "pending version"))?;
    let retain = read_sidecar(path)?;
    write_live_sidecar(
        path,
        version.as_str(),
        pending.len,
        &pending.digest,
        retain.as_ref(),
    )?;
    let bytes = read_bytes(path)?.ok_or_else(|| {
        integrity(
            path,
            "digest_mismatch",
            Some(fingerprint(&pending.digest, pending.len)),
            Some("missing".into()),
        )
    })?;
    if !bytes_match(&bytes, &pending.digest, pending.len) {
        return Err(integrity(
            path,
            "digest_mismatch",
            Some(fingerprint(&pending.digest, pending.len)),
            Some(fingerprint(&digest_of(&bytes), bytes.len() as u64)),
        ));
    }
    Ok(Loaded::Live(LiveObject {
        metadata: ObjectMetadata {
            key: key.clone(),
            len: pending.len,
            digest: pending.digest.clone(),
            version,
        },
        bytes,
    }))
}

fn read_sidecar(object_path: &Path) -> Result<Option<ObjectSidecar>, StorageError> {
    let path = sibling(object_path, SIDECAR_SUFFIX);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(StorageError::backend(err.to_string())),
    };
    match serde_json::from_slice::<ObjectSidecar>(&bytes) {
        Ok(sidecar) if sidecar.schema == SIDECAR_SCHEMA => Ok(Some(sidecar)),
        Ok(_) => Err(corrupt(object_path, "unsupported sidecar schema")),
        Err(_) => Err(corrupt(object_path, "sidecar json")),
    }
}

fn write_sidecar(object_path: &Path, sidecar: &ObjectSidecar) -> Result<(), StorageError> {
    let dest = sibling(object_path, SIDECAR_SUFFIX);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|err| StorageError::backend(err.to_string()))?;
    }
    let tmp = sibling(
        object_path,
        &format!("{SIDECAR_SUFFIX}.tmp-{}", uuid::Uuid::new_v4().simple()),
    );
    let bytes =
        serde_json::to_vec_pretty(sidecar).map_err(|err| StorageError::backend(err.to_string()))?;
    let mut file = File::create(&tmp).map_err(|err| StorageError::backend(err.to_string()))?;
    file.write_all(&bytes)
        .map_err(|err| StorageError::backend(err.to_string()))?;
    file.sync_all()
        .map_err(|err| StorageError::backend(err.to_string()))?;
    drop(file);
    std::fs::rename(&tmp, &dest).map_err(|err| StorageError::backend(err.to_string()))?;
    fsync_parent(&dest)
}

fn write_live_sidecar(
    path: &Path,
    version: &str,
    len: u64,
    digest: &ContentDigest,
    retain: Option<&ObjectSidecar>,
) -> Result<(), StorageError> {
    write_sidecar(
        path,
        &ObjectSidecar {
            schema: SIDECAR_SCHEMA,
            state: SidecarState::Live,
            version: Some(version.to_string()),
            len: Some(len),
            digest: Some(digest.clone()),
            pending: None,
            tombstoned_unix_ms: retain.and_then(|sidecar| sidecar.tombstoned_unix_ms),
            retained_version: retain.and_then(|sidecar| sidecar.retained_version.clone()),
        },
    )
}

fn required_version(sidecar: &ObjectSidecar, path: &Path) -> Result<ObjectVersion, StorageError> {
    let raw = sidecar
        .version
        .as_deref()
        .ok_or_else(|| corrupt(path, "sidecar missing version"))?;
    ObjectVersion::new(raw.to_string()).map_err(|_| corrupt(path, "sidecar version"))
}

fn required_content(
    sidecar: &ObjectSidecar,
    path: &Path,
) -> Result<(u64, ContentDigest), StorageError> {
    match (sidecar.len, sidecar.digest.clone()) {
        (Some(len), Some(digest)) => Ok((len, digest)),
        _ => Err(corrupt(path, "sidecar missing digest")),
    }
}

fn pending_path(object_path: &Path, file: &str) -> Result<PathBuf, StorageError> {
    if file.is_empty()
        || file.contains('/')
        || file.contains('\\')
        || file.contains('\0')
        || file == "."
        || file == ".."
    {
        return Err(corrupt(object_path, "pending path"));
    }
    let parent = object_path
        .parent()
        .ok_or_else(|| StorageError::invalid_key("object path"))?;
    Ok(parent.join(file))
}

fn pending_filename(path: &Path, version: &str) -> String {
    format!(
        "{}{PUBLISH_INFIX}{version}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("object")
    )
}

fn read_bytes(path: &Path) -> Result<Option<Vec<u8>>, StorageError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(StorageError::backend(err.to_string())),
    }
}

fn bytes_match(bytes: &[u8], digest: &ContentDigest, len: u64) -> bool {
    bytes.len() as u64 == len && digest_of(bytes) == *digest
}

fn fingerprint(digest: &ContentDigest, len: u64) -> String {
    format!("{}:{len}", digest.hex)
}

fn new_version() -> Result<ObjectVersion, StorageError> {
    ObjectVersion::new(uuid::Uuid::new_v4().to_string())
}

fn conflict(expected: Option<&str>, actual: Option<&str>) -> StorageError {
    StorageError::Conflict {
        expected: expected.map(str::to_string),
        actual: actual.map(str::to_string),
    }
}

fn corrupt(path: &Path, detail: &str) -> StorageError {
    write_quarantine(
        path,
        &QuarantineRecord {
            reason: "corrupt_sidecar",
            detail: detail.to_string(),
            expected: None,
            actual: None,
        },
    );
    StorageError::Corrupt {
        detail: detail.to_string(),
    }
}

fn integrity(
    path: &Path,
    reason: &'static str,
    expected: Option<String>,
    actual: Option<String>,
) -> StorageError {
    write_quarantine(
        path,
        &QuarantineRecord {
            reason,
            detail: "object bytes do not match sidecar".into(),
            expected: expected.clone(),
            actual: actual.clone(),
        },
    );
    StorageError::Integrity {
        expected: expected.unwrap_or_else(|| "-".into()),
        actual: actual.unwrap_or_else(|| "-".into()),
    }
}

fn write_quarantine(path: &Path, record: &QuarantineRecord) {
    if let Ok(bytes) = serde_json::to_vec_pretty(record) {
        let _ = std::fs::write(sibling(path, QUARANTINE_SUFFIX), bytes);
    }
}

async fn lock_key(path: &Path) -> Result<File, StorageError> {
    let lock_path = sibling(path, LOCK_SUFFIX);
    tokio::task::spawn_blocking(move || {
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
        file.lock_exclusive()
            .map_err(|err| StorageError::backend(err.to_string()))?;
        Ok(file)
    })
    .await
    .map_err(|err| StorageError::backend(err.to_string()))?
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut raw = path.as_os_str().to_os_string();
    raw.push(suffix);
    PathBuf::from(raw)
}

/// Make a completed rename durable.
///
/// The renamed file's own contents are already on stable storage: every
/// caller fsyncs the source before renaming it into place, and the recovery
/// path only renames a pending file that a prior process fsynced before
/// recording it in the sidecar. Durability of the rename itself needs the
/// parent directory, so re-opening and flushing the file again would buy
/// nothing and cost a second full-device flush on macOS, where `sync_all`
/// issues `F_FULLFSYNC`.
fn fsync_parent(path: &Path) -> Result<(), StorageError> {
    let parent = path
        .parent()
        .ok_or_else(|| StorageError::backend("missing parent directory"))?;
    let dir = File::open(parent).map_err(|err| StorageError::backend(err.to_string()))?;
    dir.sync_all()
        .map_err(|err| StorageError::backend(err.to_string()))?;
    Ok(())
}

/// Flush a file and then its parent directory.
///
/// Only the crash-recovery path uses this: it republishes bytes written by a
/// process that is no longer around to vouch for them, so it re-flushes the
/// file as well. That runs once per recovery, never on the write path.
fn fsync_file_and_parent(path: &Path) -> Result<(), StorageError> {
    let file = File::open(path).map_err(|err| StorageError::backend(err.to_string()))?;
    file.sync_all()
        .map_err(|err| StorageError::backend(err.to_string()))?;
    fsync_parent(path)
}

fn slice_range(bytes: &[u8], range: ByteRange) -> Result<Vec<u8>, StorageError> {
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

fn is_adapter_owned_name(name: &str) -> bool {
    name.starts_with('.')
        || name.contains(".tmp-")
        || name.ends_with(SIDECAR_SUFFIX)
        || name.ends_with(LOCK_SUFFIX)
        || name.ends_with(QUARANTINE_SUFFIX)
        || name.contains(PUBLISH_INFIX)
        || name.contains(RETAIN_INFIX)
}

fn retain_path(object_path: &Path, version: &str) -> PathBuf {
    sibling(object_path, &format!("{RETAIN_INFIX}{version}"))
}

fn object_path_from_retain(retain: &Path) -> Option<PathBuf> {
    let name = retain.file_name()?.to_string_lossy();
    let idx = name.find(RETAIN_INFIX)?;
    Some(retain.with_file_name(&name[..idx]))
}

fn retain_version_from_path(retain: &Path) -> Option<String> {
    let name = retain.file_name()?.to_string_lossy();
    let idx = name.find(RETAIN_INFIX)?;
    Some(name[idx + RETAIN_INFIX.len()..].to_string())
}

fn unix_ms_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn collect_retain_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), StorageError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(StorageError::backend(err.to_string())),
    };
    for entry in entries {
        let entry = entry.map_err(|err| StorageError::backend(err.to_string()))?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            collect_retain_files(&path, out)?;
        } else if name.contains(RETAIN_INFIX) {
            out.push(path);
        }
    }
    Ok(())
}

fn collect_encoded(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<(), StorageError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(StorageError::backend(err.to_string())),
    };
    for entry in entries {
        let entry = entry.map_err(|err| StorageError::backend(err.to_string()))?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if is_adapter_owned_name(&name) {
            continue;
        }
        if path.is_dir() {
            collect_encoded(root, &path, out)?;
        } else {
            let rel = path
                .strip_prefix(root)
                .map_err(|_| StorageError::invalid_key("escaped root"))?;
            let encoded = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            out.push(encoded);
        }
    }
    Ok(())
}
