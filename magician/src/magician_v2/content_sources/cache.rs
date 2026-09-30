use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::{io::AsyncReadExt, sync::Mutex as AsyncMutex};

use super::{
    public_http::ConditionalHttpRequest,
    types::{
        ContentDocument, ContentPrivacy, ContentProvenance, ReadDepth, SourceIdentity,
        CONTENT_SOURCE_SCHEMA_VERSION,
    },
};
use crate::magician_v2::artifact_v2::io::write_bytes_atomic;

const CONTENT_CACHE_SCHEMA_VERSION: u32 = 2;
const MAX_URL_CACHE_ENTRY_BYTES: usize = 256 * 1024;
const MAX_BODY_CACHE_ENTRY_BYTES: usize = 32 * 1024 * 1024;
const DEFAULT_MAX_SCOPE_CACHE_BYTES: u64 = 512 * 1024 * 1024;
const DEFAULT_CACHE_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const DEFAULT_SCRATCH_RETENTION: Duration = Duration::from_secs(60 * 60);
const DEFAULT_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(15 * 60);
const MAX_RETAINED_SCOPE_LOCKS: usize = 1024;

#[derive(Debug, Clone)]
struct ContentCacheMaintenancePolicy {
    max_scope_bytes: u64,
    cache_retention: Duration,
    scratch_retention: Duration,
    maintenance_interval: Duration,
}

impl Default for ContentCacheMaintenancePolicy {
    fn default() -> Self {
        Self {
            max_scope_bytes: DEFAULT_MAX_SCOPE_CACHE_BYTES,
            cache_retention: DEFAULT_CACHE_RETENTION,
            scratch_retention: DEFAULT_SCRATCH_RETENTION,
            maintenance_interval: DEFAULT_MAINTENANCE_INTERVAL,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ContentCacheHit {
    pub document: ContentDocument,
    pub fetched_at_ms: i64,
    pub validators: ConditionalHttpRequest,
}

#[derive(Debug, Clone)]
pub struct ContentCacheRevalidation {
    pub document: ContentDocument,
    pub revalidated: bool,
}

impl ContentCacheHit {
    pub fn is_fresh_at(&self, now_ms: i64, ttl_ms: i64) -> bool {
        ttl_ms > 0
            && now_ms >= self.fetched_at_ms
            && now_ms.saturating_sub(self.fetched_at_ms) <= ttl_ms
    }
}

#[derive(Debug, Clone)]
pub struct ScopedContentCache {
    root: PathBuf,
    policy: ContentCacheMaintenancePolicy,
    mutation_locks: Arc<Mutex<HashMap<String, Weak<AsyncMutex<()>>>>>,
    last_maintenance_ms: Arc<Mutex<HashMap<String, i64>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedBody {
    schema_version: u32,
    content_hash: String,
    text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedUrlEntry {
    schema_version: u32,
    principal: String,
    workspace: String,
    reader_id: String,
    capability_revision: String,
    read_depth: ReadDepth,
    request_url: String,
    body_hash: String,
    identity: SourceIdentity,
    title: String,
    canonical_url: Option<String>,
    media_type: Option<String>,
    fetched_at_ms: i64,
    privacy: ContentPrivacy,
    content_hash: String,
    provenance: ContentProvenance,
    metadata: std::collections::BTreeMap<String, serde_json::Value>,
    etag: Option<String>,
    last_modified: Option<String>,
}

impl ScopedContentCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            policy: ContentCacheMaintenancePolicy::default(),
            mutation_locks: Arc::new(Mutex::new(HashMap::new())),
            last_maintenance_ms: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    fn with_policy(root: impl Into<PathBuf>, policy: ContentCacheMaintenancePolicy) -> Self {
        Self {
            root: root.into(),
            policy,
            mutation_locks: Arc::new(Mutex::new(HashMap::new())),
            last_maintenance_ms: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn get(
        &self,
        principal: &str,
        workspace: &str,
        reader_id: &str,
        capability_revision: &str,
        read_depth: ReadDepth,
        request_url: &str,
    ) -> Result<Option<ContentCacheHit>> {
        if let Err(error) = self.maintain_scope_if_due(principal, workspace).await {
            tracing::warn!(
                %error,
                principal,
                workspace,
                "content cache maintenance failed before read"
            );
        }
        self.get_without_maintenance(
            principal,
            workspace,
            reader_id,
            capability_revision,
            read_depth,
            request_url,
        )
        .await
    }

    async fn get_without_maintenance(
        &self,
        principal: &str,
        workspace: &str,
        reader_id: &str,
        capability_revision: &str,
        read_depth: ReadDepth,
        request_url: &str,
    ) -> Result<Option<ContentCacheHit>> {
        let path = self.url_entry_path(
            principal,
            workspace,
            reader_id,
            capability_revision,
            read_depth,
            request_url,
        );
        let Some(bytes) = read_bounded_cache_file(&path, MAX_URL_CACHE_ENTRY_BYTES).await? else {
            return Ok(None);
        };
        let entry: PersistedUrlEntry = match serde_json::from_slice(&bytes) {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    path = %path.display(),
                    "ignoring corrupt content cache URL entry"
                );
                return Ok(None);
            },
        };
        if entry.schema_version != CONTENT_CACHE_SCHEMA_VERSION
            || entry.principal != principal
            || entry.workspace != workspace
            || entry.reader_id != reader_id
            || entry.capability_revision != capability_revision
            || entry.read_depth != read_depth
            || entry.request_url != request_url
            || entry.body_hash != entry.content_hash
            || !is_blake3_hash(&entry.body_hash)
            || entry.fetched_at_ms <= 0
        {
            tracing::warn!(
                path = %path.display(),
                "ignoring mismatched content cache URL entry"
            );
            return Ok(None);
        }

        let body_path = self.body_path(principal, workspace, &entry.body_hash);
        let Some(body_bytes) =
            read_bounded_cache_file(&body_path, MAX_BODY_CACHE_ENTRY_BYTES).await?
        else {
            return Ok(None);
        };
        let body: PersistedBody = match serde_json::from_slice(&body_bytes) {
            Ok(body) => body,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    path = %body_path.display(),
                    "ignoring corrupt content cache body"
                );
                return Ok(None);
            },
        };
        let computed_body_hash = blake3::hash(body.text.as_bytes()).to_hex().to_string();
        if body.schema_version != CONTENT_CACHE_SCHEMA_VERSION
            || body.content_hash != entry.body_hash
            || computed_body_hash != body.content_hash
        {
            tracing::warn!(
                path = %body_path.display(),
                "ignoring content cache body with invalid hash"
            );
            return Ok(None);
        }

        let document = ContentDocument {
            schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
            identity: entry.identity,
            title: entry.title,
            text: body.text,
            canonical_url: entry.canonical_url,
            media_type: entry.media_type,
            fetched_at_ms: entry.fetched_at_ms,
            privacy: entry.privacy,
            content_hash: entry.content_hash,
            provenance: entry.provenance,
            metadata: entry.metadata,
        };
        if let Err(error) = document.validate() {
            tracing::warn!(
                error = %error,
                path = %path.display(),
                "ignoring invalid cached content document"
            );
            return Ok(None);
        }
        Ok(Some(ContentCacheHit {
            fetched_at_ms: document.fetched_at_ms,
            document,
            validators: ConditionalHttpRequest {
                etag: entry.etag,
                last_modified: entry.last_modified,
            },
        }))
    }

    pub async fn put(
        &self,
        principal: &str,
        workspace: &str,
        reader_id: &str,
        capability_revision: &str,
        read_depth: ReadDepth,
        request_url: &str,
        document: &ContentDocument,
        validators: &ConditionalHttpRequest,
    ) -> Result<()> {
        let mutation_lock = self.scope_mutation_lock(principal, workspace);
        let _mutation_guard = mutation_lock.lock().await;
        self.put_locked(
            principal,
            workspace,
            reader_id,
            capability_revision,
            read_depth,
            request_url,
            document,
            validators,
        )
        .await
    }

    async fn put_locked(
        &self,
        principal: &str,
        workspace: &str,
        reader_id: &str,
        capability_revision: &str,
        read_depth: ReadDepth,
        request_url: &str,
        document: &ContentDocument,
        validators: &ConditionalHttpRequest,
    ) -> Result<()> {
        document.validate()?;
        let computed_hash = blake3::hash(document.text.as_bytes()).to_hex().to_string();
        if computed_hash != document.content_hash {
            bail!("content document hash does not match extracted text");
        }
        let body_path = self.body_path(principal, workspace, &document.content_hash);
        if !valid_cached_body(&body_path, &document.content_hash).await? {
            let body = PersistedBody {
                schema_version: CONTENT_CACHE_SCHEMA_VERSION,
                content_hash: document.content_hash.clone(),
                text: document.text.clone(),
            };
            let bytes = serde_json::to_vec(&body).context("serializing content cache body")?;
            write_bytes_atomic(&body_path, &bytes)
                .await
                .with_context(|| format!("writing content cache body `{}`", body_path.display()))?;
        }

        let entry = PersistedUrlEntry {
            schema_version: CONTENT_CACHE_SCHEMA_VERSION,
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            reader_id: reader_id.to_string(),
            capability_revision: capability_revision.to_string(),
            read_depth,
            request_url: request_url.to_string(),
            body_hash: document.content_hash.clone(),
            identity: document.identity.clone(),
            title: document.title.clone(),
            canonical_url: document.canonical_url.clone(),
            media_type: document.media_type.clone(),
            fetched_at_ms: document.fetched_at_ms,
            privacy: document.privacy,
            content_hash: document.content_hash.clone(),
            provenance: document.provenance.clone(),
            metadata: document.metadata.clone(),
            etag: validators.etag.clone(),
            last_modified: validators.last_modified.clone(),
        };
        let entry_path = self.url_entry_path(
            principal,
            workspace,
            reader_id,
            capability_revision,
            read_depth,
            request_url,
        );
        let bytes = serde_json::to_vec(&entry).context("serializing content cache URL entry")?;
        write_bytes_atomic(&entry_path, &bytes)
            .await
            .with_context(|| {
                format!("writing content cache URL entry `{}`", entry_path.display())
            })?;
        if let Err(error) = self
            .maintain_scope_if_due_locked(principal, workspace)
            .await
        {
            tracing::warn!(
                %error,
                principal,
                workspace,
                "content cache maintenance failed after write"
            );
        }
        Ok(())
    }

    pub async fn mark_revalidated(
        &self,
        principal: &str,
        workspace: &str,
        reader_id: &str,
        capability_revision: &str,
        read_depth: ReadDepth,
        request_url: &str,
        expected_content_hash: &str,
        fetched_at_ms: i64,
        validators: &ConditionalHttpRequest,
    ) -> Result<Option<ContentCacheRevalidation>> {
        let mutation_lock = self.scope_mutation_lock(principal, workspace);
        let _mutation_guard = mutation_lock.lock().await;
        let Some(mut hit) = self
            .get_without_maintenance(
                principal,
                workspace,
                reader_id,
                capability_revision,
                read_depth,
                request_url,
            )
            .await?
        else {
            return Ok(None);
        };
        // Another service generation may have refreshed the same cache key
        // while this request was in flight. A 304 applies only to the body
        // whose validators were sent; never attach its validators to a newer
        // body written by that concurrent refresh.
        if hit.document.content_hash != expected_content_hash {
            return Ok(Some(ContentCacheRevalidation {
                document: hit.document,
                revalidated: false,
            }));
        }
        hit.document.fetched_at_ms = fetched_at_ms;
        let merged = ConditionalHttpRequest {
            etag: validators.etag.clone().or(hit.validators.etag),
            last_modified: validators
                .last_modified
                .clone()
                .or(hit.validators.last_modified),
        };
        self.put_locked(
            principal,
            workspace,
            reader_id,
            capability_revision,
            read_depth,
            request_url,
            &hit.document,
            &merged,
        )
        .await?;
        Ok(Some(ContentCacheRevalidation {
            document: hit.document,
            revalidated: true,
        }))
    }

    pub fn scratch_path(&self, principal: &str, workspace: &str) -> PathBuf {
        self.scope_root(principal, workspace)
            .join("scratch")
            .join(format!("{}.html", uuid::Uuid::new_v4().simple()))
    }

    async fn maintain_scope_if_due_locked(&self, principal: &str, workspace: &str) -> Result<()> {
        let now_ms = now_epoch_ms();
        let scope_key = format!("{principal}\0{workspace}");
        let due = {
            let last = self
                .last_maintenance_ms
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let interval_ms = duration_ms(self.policy.maintenance_interval);
            last.get(&scope_key)
                .is_none_or(|previous| now_ms.saturating_sub(*previous) >= interval_ms)
        };
        if !due {
            return Ok(());
        }
        self.maintain_scope_locked(principal, workspace, now_ms)
            .await?;
        self.last_maintenance_ms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(scope_key, now_ms);
        Ok(())
    }

    async fn maintain_scope_if_due(&self, principal: &str, workspace: &str) -> Result<()> {
        let now_ms = now_epoch_ms();
        let scope_key = format!("{principal}\0{workspace}");
        let interval_ms = duration_ms(self.policy.maintenance_interval);
        let due = self
            .last_maintenance_ms
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&scope_key)
            .is_none_or(|previous| now_ms.saturating_sub(*previous) >= interval_ms);
        if !due {
            return Ok(());
        }

        let mutation_lock = self.scope_mutation_lock(principal, workspace);
        let _mutation_guard = mutation_lock.lock().await;
        self.maintain_scope_if_due_locked(principal, workspace)
            .await
    }

    fn scope_mutation_lock(&self, principal: &str, workspace: &str) -> Arc<AsyncMutex<()>> {
        let scope_key = format!("{principal}\0{workspace}");
        let mut locks = self
            .mutation_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(lock) = locks.get(&scope_key).and_then(Weak::upgrade) {
            return lock;
        }
        if locks.len() >= MAX_RETAINED_SCOPE_LOCKS {
            locks.retain(|_, lock| lock.strong_count() > 0);
            self.last_maintenance_ms
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .retain(|scope, _| locks.contains_key(scope));
        }
        let lock = Arc::new(AsyncMutex::new(()));
        locks.insert(scope_key, Arc::downgrade(&lock));
        lock
    }

    async fn maintain_scope_locked(
        &self,
        principal: &str,
        workspace: &str,
        now_ms: i64,
    ) -> Result<()> {
        let scope_root = self.scope_root(principal, workspace);
        remove_expired_files(
            &scope_root.join("scratch"),
            now_ms,
            self.policy.scratch_retention,
        )
        .await?;

        let url_dir = scope_root.join("urls");
        let mut urls = load_url_cache_files(
            &url_dir,
            principal,
            workspace,
            now_ms,
            self.policy.cache_retention,
        )
        .await?;
        let body_dir = scope_root.join("bodies");
        let mut bodies = load_body_cache_files(&body_dir).await?;
        remove_unreferenced_bodies(&urls, &mut bodies).await;

        let mut ref_counts = body_reference_counts(&urls);
        let mut total_bytes = urls
            .iter()
            .map(|entry| entry.bytes)
            .sum::<u64>()
            .saturating_add(bodies.values().map(|entry| entry.bytes).sum::<u64>());
        if total_bytes <= self.policy.max_scope_bytes {
            return Ok(());
        }

        urls.sort_by_key(|entry| (entry.modified_at_ms, entry.path.clone()));
        for entry in urls {
            if total_bytes <= self.policy.max_scope_bytes {
                break;
            }
            if !remove_file_if_present(&entry.path).await {
                continue;
            }
            total_bytes = total_bytes.saturating_sub(entry.bytes);
            let Some(count) = ref_counts.get_mut(&entry.body_hash) else {
                continue;
            };
            *count = count.saturating_sub(1);
            if *count == 0 {
                if let Some(body) = bodies.remove(&entry.body_hash) {
                    if remove_file_if_present(&body.path).await {
                        total_bytes = total_bytes.saturating_sub(body.bytes);
                    }
                }
            }
        }
        Ok(())
    }

    fn scope_root(&self, principal: &str, workspace: &str) -> PathBuf {
        self.root
            .join("scopes")
            .join(blake3::hash(principal.as_bytes()).to_hex().to_string())
            .join(blake3::hash(workspace.as_bytes()).to_hex().to_string())
    }

    fn url_entry_path(
        &self,
        principal: &str,
        workspace: &str,
        reader_id: &str,
        capability_revision: &str,
        read_depth: ReadDepth,
        request_url: &str,
    ) -> PathBuf {
        let mut key = blake3::Hasher::new();
        key.update(reader_id.as_bytes());
        key.update(&[0]);
        key.update(capability_revision.as_bytes());
        key.update(&[0]);
        key.update(match read_depth {
            ReadDepth::Gist => b"gist",
            ReadDepth::FullText => b"full_text",
        });
        key.update(&[0]);
        key.update(request_url.as_bytes());
        self.scope_root(principal, workspace)
            .join("urls")
            .join(format!("{}.json", key.finalize().to_hex()))
    }

    fn body_path(&self, principal: &str, workspace: &str, content_hash: &str) -> PathBuf {
        self.scope_root(principal, workspace)
            .join("bodies")
            .join(format!("{content_hash}.json"))
    }
}

#[derive(Debug)]
struct UrlCacheFile {
    path: PathBuf,
    body_hash: String,
    bytes: u64,
    modified_at_ms: i64,
}

#[derive(Debug)]
struct BodyCacheFile {
    path: PathBuf,
    bytes: u64,
}

async fn load_url_cache_files(
    directory: &Path,
    principal: &str,
    workspace: &str,
    now_ms: i64,
    retention: Duration,
) -> Result<Vec<UrlCacheFile>> {
    let mut entries = match tokio::fs::read_dir(directory).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("reading content cache URL dir `{}`", directory.display())
            })
        },
    };
    let mut files = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let file_type = entry.file_type().await?;
        if !file_type.is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let metadata = entry.metadata().await?;
        let modified_at_ms = modified_epoch_ms(&metadata);
        if now_ms.saturating_sub(modified_at_ms) > duration_ms(retention)
            && remove_file_if_present(&path).await
        {
            continue;
        }
        let Some(bytes) = read_bounded_cache_file(&path, MAX_URL_CACHE_ENTRY_BYTES).await? else {
            remove_file_if_present(&path).await;
            continue;
        };
        let parsed = serde_json::from_slice::<PersistedUrlEntry>(&bytes);
        let Ok(url_entry) = parsed else {
            remove_file_if_present(&path).await;
            continue;
        };
        if url_entry.schema_version != CONTENT_CACHE_SCHEMA_VERSION
            || url_entry.principal != principal
            || url_entry.workspace != workspace
            || url_entry.reader_id.trim().is_empty()
            || url_entry.capability_revision.trim().is_empty()
            || url_entry.body_hash != url_entry.content_hash
            || !is_blake3_hash(&url_entry.body_hash)
        {
            remove_file_if_present(&path).await;
            continue;
        }
        files.push(UrlCacheFile {
            path,
            body_hash: url_entry.body_hash,
            bytes: metadata.len(),
            modified_at_ms,
        });
    }
    Ok(files)
}

async fn load_body_cache_files(directory: &Path) -> Result<HashMap<String, BodyCacheFile>> {
    let mut entries = match tokio::fs::read_dir(directory).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("reading content cache body dir `{}`", directory.display())
            })
        },
    };
    let mut files = HashMap::new();
    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Some(hash) = path
            .file_stem()
            .and_then(|value| value.to_str())
            .filter(|value| is_blake3_hash(value))
            .map(str::to_string)
        else {
            remove_file_if_present(&path).await;
            continue;
        };
        let metadata = entry.metadata().await?;
        files.insert(
            hash,
            BodyCacheFile {
                path,
                bytes: metadata.len(),
            },
        );
    }
    Ok(files)
}

async fn remove_unreferenced_bodies(
    urls: &[UrlCacheFile],
    bodies: &mut HashMap<String, BodyCacheFile>,
) {
    let referenced = urls
        .iter()
        .map(|entry| entry.body_hash.as_str())
        .collect::<HashSet<_>>();
    let unreferenced = bodies
        .keys()
        .filter(|hash| !referenced.contains(hash.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    for hash in unreferenced {
        let Some(path) = bodies.get(&hash).map(|body| body.path.clone()) else {
            continue;
        };
        if remove_file_if_present(&path).await {
            bodies.remove(&hash);
        }
    }
}

fn body_reference_counts(urls: &[UrlCacheFile]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for entry in urls {
        *counts.entry(entry.body_hash.clone()).or_insert(0) += 1;
    }
    counts
}

async fn remove_expired_files(directory: &Path, now_ms: i64, retention: Duration) -> Result<()> {
    let mut entries = match tokio::fs::read_dir(directory).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("reading cache scratch dir `{}`", directory.display()))
        },
    };
    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_file() {
            continue;
        }
        let metadata = entry.metadata().await?;
        if now_ms.saturating_sub(modified_epoch_ms(&metadata)) > duration_ms(retention) {
            remove_file_if_present(&entry.path()).await;
        }
    }
    Ok(())
}

async fn remove_file_if_present(path: &Path) -> bool {
    match tokio::fs::remove_file(path).await {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "failed to remove content cache file");
            false
        },
    }
}

fn modified_epoch_ms(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

fn now_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn duration_ms(duration: Duration) -> i64 {
    duration.as_millis().min(i64::MAX as u128) as i64
}

async fn valid_cached_body(path: &Path, expected_hash: &str) -> Result<bool> {
    let Some(bytes) = read_bounded_cache_file(path, MAX_BODY_CACHE_ENTRY_BYTES).await? else {
        return Ok(false);
    };
    let Ok(body) = serde_json::from_slice::<PersistedBody>(&bytes) else {
        return Ok(false);
    };
    Ok(body.schema_version == CONTENT_CACHE_SCHEMA_VERSION
        && body.content_hash == expected_hash
        && blake3::hash(body.text.as_bytes()).to_hex().to_string() == expected_hash)
}

fn is_blake3_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

async fn read_bounded_cache_file(
    path: &std::path::Path,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>> {
    let file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("opening content cache file `{}`", path.display()))
        },
    };
    let mut bytes = Vec::new();
    file.take((max_bytes as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .await
        .with_context(|| format!("reading content cache file `{}`", path.display()))?;
    if bytes.len() > max_bytes {
        tracing::warn!(
            path = %path.display(),
            max_bytes,
            "ignoring oversized content cache file"
        );
        return Ok(None);
    }
    Ok(Some(bytes))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    const TEST_READER: &str = "static-http";
    const TEST_REVISION: &str = "revision-1";
    const TEST_DEPTH: ReadDepth = ReadDepth::FullText;

    fn document(text: &str) -> ContentDocument {
        ContentDocument {
            schema_version: CONTENT_SOURCE_SCHEMA_VERSION,
            identity: SourceIdentity::new("search", "one").unwrap(),
            title: "One".into(),
            text: text.into(),
            canonical_url: Some("https://example.com/one".into()),
            media_type: Some("text/html".into()),
            fetched_at_ms: 100,
            privacy: ContentPrivacy::Public,
            content_hash: blake3::hash(text.as_bytes()).to_hex().to_string(),
            provenance: ContentProvenance {
                source_label: "Example".into(),
                source_url: Some("https://example.com/one".into()),
                retrieved_by: "static-http".into(),
            },
            metadata: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn cache_is_scope_isolated_and_preserves_validators() {
        let temp = tempfile::tempdir().unwrap();
        let cache = ScopedContentCache::new(temp.path());
        let validators = ConditionalHttpRequest {
            etag: Some("\"v1\"".into()),
            last_modified: Some("Wed, 22 Jul 2026 10:00:00 GMT".into()),
        };
        cache
            .put(
                "alice",
                "main",
                TEST_READER,
                TEST_REVISION,
                TEST_DEPTH,
                "https://example.com/one",
                &document("body"),
                &validators,
            )
            .await
            .unwrap();
        let hit = cache
            .get(
                "alice",
                "main",
                TEST_READER,
                TEST_REVISION,
                TEST_DEPTH,
                "https://example.com/one",
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(hit.document.text, "body");
        assert_eq!(hit.validators, validators);
        assert!(cache
            .get(
                "bob",
                "main",
                TEST_READER,
                TEST_REVISION,
                TEST_DEPTH,
                "https://example.com/one",
            )
            .await
            .unwrap()
            .is_none());
    }

    #[test]
    fn cloned_cache_handles_share_one_mutation_and_maintenance_domain() {
        let temp = tempfile::tempdir().unwrap();
        let cache = ScopedContentCache::new(temp.path());
        let clone = cache.clone();

        assert!(Arc::ptr_eq(&cache.mutation_locks, &clone.mutation_locks));
        assert!(Arc::ptr_eq(
            &cache.last_maintenance_ms,
            &clone.last_maintenance_ms
        ));
        assert!(Arc::ptr_eq(
            &cache.scope_mutation_lock("p", "w"),
            &clone.scope_mutation_lock("p", "w")
        ));
        assert!(!Arc::ptr_eq(
            &cache.scope_mutation_lock("p", "w"),
            &clone.scope_mutation_lock("other", "w")
        ));
    }

    #[test]
    fn inactive_scope_lock_and_maintenance_state_is_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let cache = ScopedContentCache::new(temp.path());

        for index in 0..(MAX_RETAINED_SCOPE_LOCKS + 17) {
            let principal = format!("p-{index}");
            cache
                .last_maintenance_ms
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(format!("{principal}\0w"), 1);
            drop(cache.scope_mutation_lock(&principal, "w"));
        }

        assert!(
            cache
                .mutation_locks
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .len()
                <= MAX_RETAINED_SCOPE_LOCKS
        );
        assert!(
            cache
                .last_maintenance_ms
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .len()
                <= MAX_RETAINED_SCOPE_LOCKS
        );
    }

    #[tokio::test]
    async fn stale_304_cannot_overwrite_newer_body_validators() {
        let temp = tempfile::tempdir().unwrap();
        let cache = ScopedContentCache::new(temp.path());
        let old = document("old article body");
        let mut newer = document("newer article body");
        newer.fetched_at_ms = 200;
        cache
            .put(
                "p",
                "w",
                TEST_READER,
                TEST_REVISION,
                TEST_DEPTH,
                "https://example.com/one",
                &old,
                &ConditionalHttpRequest {
                    etag: Some("\"v1\"".into()),
                    last_modified: None,
                },
            )
            .await
            .unwrap();
        cache
            .clone()
            .put(
                "p",
                "w",
                TEST_READER,
                TEST_REVISION,
                TEST_DEPTH,
                "https://example.com/one",
                &newer,
                &ConditionalHttpRequest {
                    etag: Some("\"v2\"".into()),
                    last_modified: None,
                },
            )
            .await
            .unwrap();

        let returned = cache
            .mark_revalidated(
                "p",
                "w",
                TEST_READER,
                TEST_REVISION,
                TEST_DEPTH,
                "https://example.com/one",
                &old.content_hash,
                300,
                &ConditionalHttpRequest {
                    etag: Some("\"v1\"".into()),
                    last_modified: None,
                },
            )
            .await
            .unwrap()
            .unwrap();
        let hit = cache
            .get(
                "p",
                "w",
                TEST_READER,
                TEST_REVISION,
                TEST_DEPTH,
                "https://example.com/one",
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(returned.document.content_hash, newer.content_hash);
        assert!(!returned.revalidated);
        assert_eq!(hit.document.content_hash, newer.content_hash);
        assert_eq!(hit.validators.etag.as_deref(), Some("\"v2\""));
        assert_eq!(hit.fetched_at_ms, 200);
    }

    #[tokio::test]
    async fn cache_isolated_by_reader_revision_and_requested_depth() {
        let temp = tempfile::tempdir().unwrap();
        let cache = ScopedContentCache::new(temp.path());
        cache
            .put(
                "p",
                "w",
                TEST_READER,
                TEST_REVISION,
                ReadDepth::Gist,
                "https://example.com/one",
                &document("reader revision scoped body"),
                &Default::default(),
            )
            .await
            .unwrap();

        assert!(cache
            .get(
                "p",
                "w",
                TEST_READER,
                TEST_REVISION,
                ReadDepth::Gist,
                "https://example.com/one",
            )
            .await
            .unwrap()
            .is_some());
        for (reader, revision, depth) in [
            ("other-reader", TEST_REVISION, ReadDepth::Gist),
            (TEST_READER, "revision-2", ReadDepth::Gist),
            (TEST_READER, TEST_REVISION, ReadDepth::FullText),
        ] {
            assert!(cache
                .get("p", "w", reader, revision, depth, "https://example.com/one",)
                .await
                .unwrap()
                .is_none());
        }
    }

    #[tokio::test]
    async fn identical_text_is_stored_once_with_multiple_url_entries() {
        let temp = tempfile::tempdir().unwrap();
        let cache = ScopedContentCache::new(temp.path());
        let first = document("shared article body");
        let mut second = first.clone();
        second.identity = SourceIdentity::new("search", "two").unwrap();
        second.canonical_url = Some("https://example.com/two".into());
        cache
            .put(
                "p",
                "w",
                TEST_READER,
                TEST_REVISION,
                TEST_DEPTH,
                "https://example.com/one",
                &first,
                &Default::default(),
            )
            .await
            .unwrap();
        cache
            .put(
                "p",
                "w",
                TEST_READER,
                TEST_REVISION,
                TEST_DEPTH,
                "https://example.com/two",
                &second,
                &Default::default(),
            )
            .await
            .unwrap();
        let body_dir = cache.scope_root("p", "w").join("bodies");
        assert_eq!(std::fs::read_dir(body_dir).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn corrupt_or_tampered_body_is_a_cache_miss() {
        let temp = tempfile::tempdir().unwrap();
        let cache = ScopedContentCache::new(temp.path());
        let doc = document("original");
        cache
            .put(
                "p",
                "w",
                TEST_READER,
                TEST_REVISION,
                TEST_DEPTH,
                "https://example.com/one",
                &doc,
                &Default::default(),
            )
            .await
            .unwrap();
        let body_path = cache.body_path("p", "w", &doc.content_hash);
        std::fs::write(
            body_path,
            r#"{"schema_version":1,"content_hash":"wrong","text":"tampered"}"#,
        )
        .unwrap();
        assert!(cache
            .get(
                "p",
                "w",
                TEST_READER,
                TEST_REVISION,
                TEST_DEPTH,
                "https://example.com/one",
            )
            .await
            .unwrap()
            .is_none());

        cache
            .put(
                "p",
                "w",
                TEST_READER,
                TEST_REVISION,
                TEST_DEPTH,
                "https://example.com/one",
                &doc,
                &Default::default(),
            )
            .await
            .unwrap();
        assert_eq!(
            cache
                .get(
                    "p",
                    "w",
                    TEST_READER,
                    TEST_REVISION,
                    TEST_DEPTH,
                    "https://example.com/one",
                )
                .await
                .unwrap()
                .unwrap()
                .document
                .text,
            "original"
        );
    }

    fn immediate_maintenance_policy(max_scope_bytes: u64) -> ContentCacheMaintenancePolicy {
        ContentCacheMaintenancePolicy {
            max_scope_bytes,
            cache_retention: Duration::from_secs(24 * 60 * 60),
            scratch_retention: Duration::from_secs(60),
            maintenance_interval: Duration::ZERO,
        }
    }

    #[tokio::test]
    async fn maintenance_removes_replaced_unreferenced_bodies() {
        let temp = tempfile::tempdir().unwrap();
        let cache =
            ScopedContentCache::with_policy(temp.path(), immediate_maintenance_policy(1024 * 1024));
        let first = document("first body");
        let first_path = cache.body_path("p", "w", &first.content_hash);
        cache
            .put(
                "p",
                "w",
                TEST_READER,
                TEST_REVISION,
                TEST_DEPTH,
                "https://example.com/one",
                &first,
                &Default::default(),
            )
            .await
            .unwrap();
        assert!(first_path.exists());

        let second = document("replacement body");
        let second_path = cache.body_path("p", "w", &second.content_hash);
        cache
            .put(
                "p",
                "w",
                TEST_READER,
                TEST_REVISION,
                TEST_DEPTH,
                "https://example.com/one",
                &second,
                &Default::default(),
            )
            .await
            .unwrap();

        assert!(!first_path.exists());
        assert!(second_path.exists());
    }

    #[tokio::test]
    async fn maintenance_enforces_scope_byte_ceiling() {
        let temp = tempfile::tempdir().unwrap();
        let cache = ScopedContentCache::with_policy(temp.path(), immediate_maintenance_policy(1));
        cache
            .put(
                "p",
                "w",
                TEST_READER,
                TEST_REVISION,
                TEST_DEPTH,
                "https://example.com/one",
                &document("body larger than one byte"),
                &Default::default(),
            )
            .await
            .unwrap();

        assert!(cache
            .get(
                "p",
                "w",
                TEST_READER,
                TEST_REVISION,
                TEST_DEPTH,
                "https://example.com/one",
            )
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn maintenance_expires_completed_entries_without_touching_atomic_temps() {
        let temp = tempfile::tempdir().unwrap();
        let policy = ContentCacheMaintenancePolicy {
            max_scope_bytes: 1024 * 1024,
            cache_retention: Duration::ZERO,
            scratch_retention: Duration::ZERO,
            maintenance_interval: Duration::ZERO,
        };
        let cache = ScopedContentCache::with_policy(temp.path(), policy);
        cache
            .put(
                "p",
                "w",
                TEST_READER,
                TEST_REVISION,
                TEST_DEPTH,
                "https://example.com/one",
                &document("expiring body"),
                &Default::default(),
            )
            .await
            .unwrap();
        let scope = cache.scope_root("p", "w");
        let scratch = scope.join("scratch").join("abandoned.html");
        std::fs::create_dir_all(scratch.parent().unwrap()).unwrap();
        std::fs::write(&scratch, "temporary page").unwrap();
        let atomic_temp = scope.join("urls").join(".write-in-progress.tmp");
        std::fs::write(&atomic_temp, "partial").unwrap();

        cache
            .maintain_scope_locked("p", "w", now_epoch_ms().saturating_add(1))
            .await
            .unwrap();

        assert!(cache
            .get(
                "p",
                "w",
                TEST_READER,
                TEST_REVISION,
                TEST_DEPTH,
                "https://example.com/one",
            )
            .await
            .unwrap()
            .is_none());
        assert!(!scratch.exists());
        assert!(atomic_temp.exists());
    }
}
