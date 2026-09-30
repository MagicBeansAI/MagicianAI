//! # Durable Artifact Store
//!
//! Filesystem-backed store for artifacts that persist across runs.
//! Uses `fs2::FileExt::lock_exclusive()` for concurrent write safety
//! on append operations. Remote qualification uses `ObjectDurableStore`.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Frontmatter metadata embedded in durable artifact files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurableFrontmatter {
    pub namespace: String,
    pub name: String,
    pub created_by: String,
    pub last_updated_by: String,
    pub last_updated: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_workflow_instance_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_cycle_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer_stage: Option<String>,
}

/// Entry returned by `list()`.
#[derive(Debug, Clone)]
pub struct DurableArtifactEntry {
    pub namespace: String,
    pub name: String,
    pub path: PathBuf,
    pub frontmatter: Option<DurableFrontmatter>,
}

/// Typed durable-artifact boundary. Local JSON/markdown files remain canonical.
#[async_trait]
pub trait DurableArtifactAccess: Send + Sync {
    async fn write(
        &self,
        namespace: &str,
        name: &str,
        content: &str,
        frontmatter: DurableFrontmatter,
    ) -> Result<()>;
    async fn append(&self, namespace: &str, name: &str, content: &str) -> Result<()>;
    async fn read(&self, namespace: &str, name: &str) -> Result<(DurableFrontmatter, String)>;
    async fn list_entries(&self, namespace: Option<&str>) -> Result<Vec<DurableArtifactEntry>>;
    async fn artifact_exists(&self, namespace: &str, name: &str) -> Result<bool>;
    async fn delete(&self, namespace: &str, name: &str) -> Result<()>;
    async fn export_all(&self) -> Result<Vec<u8>>;
    async fn import_all(&self, bytes: &[u8]) -> Result<()>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedDurable {
    pub namespace: String,
    pub name: String,
    pub frontmatter: DurableFrontmatter,
    pub body: String,
}

/// Filesystem wrapper for durable artifacts at the configured durable-artifact root.
#[derive(Debug, Clone)]
pub struct DurableArtifactStore {
    pub base_path: PathBuf,
    index: std::sync::Arc<std::sync::RwLock<TagIndex>>,
}

/// Default/canonical local adapter. Production composition uses this.
pub fn open_local_durable_artifacts(
    workspace_layout: &crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace,
    principal: &str,
    workspace_name: &str,
) -> Result<DurableArtifactStore> {
    DurableArtifactStore::for_scope(workspace_layout, principal, workspace_name)
}

impl DurableArtifactStore {
    /// Create a new store, ensuring the base directory exists.
    /// The base_path is canonicalized at construction to avoid symlink
    /// mismatches (e.g., macOS `/var` → `/private/var`).
    /// Builds the in-memory tag index by walking all existing artifacts on disk.
    pub fn new(base_path: impl Into<PathBuf>) -> Result<Self> {
        let base_path = base_path.into();
        std::fs::create_dir_all(&base_path)
            .with_context(|| format!("Failed to create durable artifact base: {:?}", base_path))?;
        let base_path = std::fs::canonicalize(&base_path).unwrap_or(base_path);

        let mut tag_index = TagIndex::default();
        // Walk all namespace directories and index existing artifacts
        if let Ok(read_dir) = std::fs::read_dir(&base_path) {
            for entry in read_dir.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(ns) = path.file_name().and_then(|n| n.to_str()) {
                        Self::index_directory_recursive(&path, ns, &path, &mut tag_index);
                    }
                }
            }
        }

        Ok(Self {
            base_path,
            index: std::sync::Arc::new(std::sync::RwLock::new(tag_index)),
        })
    }

    pub fn for_scope(
        workspace_layout: &crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace,
        principal: &str,
        workspace_name: &str,
    ) -> Result<Self> {
        Self::new(workspace_layout.durable_artifacts_root(principal, workspace_name))
    }

    /// Recursively index all files in a namespace directory.
    /// `ns_root` is the top-level namespace directory, used for stable relative name computation.
    fn index_directory_recursive(
        dir: &Path,
        namespace: &str,
        ns_root: &Path,
        index: &mut TagIndex,
    ) {
        let read_dir = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(_) => return,
        };
        for entry in read_dir.flatten() {
            let path = entry.path();
            // Skip symlinks to prevent path traversal
            if let Ok(meta) = std::fs::symlink_metadata(&path) {
                if meta.file_type().is_symlink() {
                    continue;
                }
            }
            if path.is_file() {
                // Skip hidden/temp files
                if path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with('.'))
                {
                    continue;
                }
                // Compute full relative name within namespace using the stable ns_root
                let relative_name = path
                    .strip_prefix(ns_root)
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| {
                        path.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string()
                    });

                // Try to read frontmatter (best-effort)
                let frontmatter = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|content| split_frontmatter(&content))
                    .and_then(|(fm_str, _)| {
                        serde_yaml::from_str::<DurableFrontmatter>(&fm_str).ok()
                    });

                let (tags, timestamp, content_type, src_task, src_execution, src_agent) =
                    if let Some(ref fm) = frontmatter {
                        (
                            derive_tags(fm),
                            fm.last_updated.timestamp(),
                            fm.content_type.clone(),
                            fm.source_task_id.clone(),
                            fm.source_execution_id.clone(),
                            fm.source_agent_id.clone(),
                        )
                    } else {
                        (derive_tags_from_path(namespace), 0, None, None, None, None)
                    };

                index.insert(
                    &tags,
                    ArtifactRef {
                        namespace: namespace.to_string(),
                        name: relative_name,
                        timestamp,
                        content_type,
                        source_task_id: src_task,
                        source_execution_id: src_execution,
                        source_agent_id: src_agent,
                    },
                );
            } else if path.is_dir() {
                // Recurse into subdirectories
                Self::index_directory_recursive(&path, namespace, ns_root, index);
            }
        }
    }

    /// Resolve path with containment validation. Returns Err on traversal attempts.
    pub fn resolve_path_safe(&self, namespace: &str, name: &str) -> Result<PathBuf> {
        // Reject obvious traversal patterns before joining
        if namespace.contains("..") || name.contains("..") {
            anyhow::bail!(
                "Path traversal detected in namespace={:?} name={:?}",
                namespace,
                name
            );
        }
        if namespace.starts_with('/') || name.starts_with('/') {
            anyhow::bail!(
                "Absolute path rejected in namespace={:?} name={:?}",
                namespace,
                name
            );
        }
        if namespace.contains('\0') || name.contains('\0') {
            anyhow::bail!("Null byte in namespace={:?} name={:?}", namespace, name);
        }
        let path = self.base_path.join(namespace).join(name);
        // Canonicalize both to resolve symlinks, then verify containment.
        // For new files that don't exist yet, verify the parent is contained.
        let canonical_base =
            std::fs::canonicalize(&self.base_path).unwrap_or_else(|_| self.base_path.clone());
        let check_path = if path.exists() {
            std::fs::canonicalize(&path).unwrap_or(path.clone())
        } else if let Some(parent) = path.parent() {
            // For new files, check that the parent dir is inside base
            std::fs::canonicalize(parent)
                .map(|p| p.join(path.file_name().unwrap_or_default()))
                .unwrap_or(path.clone())
        } else {
            path.clone()
        };
        if !check_path.starts_with(&canonical_base) {
            anyhow::bail!("Path {:?} escapes base {:?}", check_path, canonical_base);
        }
        Ok(path)
    }

    /// Crash-safe write via temp file + atomic rename.
    /// Uses fs2::lock_exclusive() on the target to serialize with concurrent readers/writers.
    pub async fn write(
        &self,
        namespace: &str,
        name: &str,
        content: &str,
        frontmatter: DurableFrontmatter,
    ) -> Result<PathBuf> {
        let uid = format!("{}/{}", namespace, name);
        crate::magician_v2::analytics::emit(
            crate::magician_v2::analytics::event_sink::AnalyticsEvent::artifact_registered(
                &uid, namespace, name,
            ),
        );
        let path = self.resolve_path_safe(namespace, name)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Capture tag-relevant fields BEFORE frontmatter is consumed by serialization.
        let index_tags = derive_tags(&frontmatter);
        let index_timestamp = frontmatter.last_updated.timestamp();
        let index_content_type = frontmatter.content_type.clone();
        let index_src_task = frontmatter.source_task_id.clone();
        let index_src_execution = frontmatter.source_execution_id.clone();
        let index_src_agent = frontmatter.source_agent_id.clone();

        let yaml = serde_yaml::to_string(&frontmatter)?;
        let full_content = format!("---\n{}---\n{}", yaml, content);
        let path_clone = path.clone();

        let result = tokio::task::spawn_blocking(move || {
            use std::io::Write;

            // Write to a temp file in the same directory first (same filesystem for rename).
            let parent = path_clone
                .parent()
                .context("Cannot determine parent directory")?;
            let tmp_path = parent.join(format!(".tmp-{}", uuid::Uuid::new_v4()));
            let mut tmp_file = std::fs::File::create(&tmp_path)
                .with_context(|| format!("Cannot create temp file: {:?}", tmp_path))?;
            tmp_file.write_all(full_content.as_bytes())?;
            // Flush to OS before rename to ensure content is on disk.
            tmp_file.sync_all()?;
            drop(tmp_file);

            // Acquire exclusive lock on the target (or a lock sentinel) to serialize
            // with concurrent readers. On Unix, rename is atomic at the directory level
            // but we still need to wait for any shared-lock readers to finish.
            // Open/create the target just for locking, then rename over it.
            let lock_file = std::fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .open(&path_clone)
                .with_context(|| format!("Cannot open for lock: {:?}", path_clone))?;
            lock_file
                .lock_exclusive()
                .context("Failed to acquire exclusive lock for write")?;

            // Atomic rename: replaces the target atomically on POSIX.
            std::fs::rename(&tmp_path, &path_clone)
                .with_context(|| format!("Failed to rename {:?} → {:?}", tmp_path, path_clone))?;

            // Lock released when lock_file is dropped (fd still points to old inode,
            // which is fine — readers that opened before rename finish reading the old
            // content; new readers open the new inode).
            Ok::<PathBuf, anyhow::Error>(path_clone)
        })
        .await??;

        // Update tag index after successful write
        {
            let mut idx = self.index.write().unwrap_or_else(|e| {
                tracing::error!("[TAG-INDEX] RwLock poisoned, recovering: {}", e);
                e.into_inner()
            });
            idx.remove(namespace, name); // remove old entry if overwriting
            idx.insert(
                &index_tags,
                ArtifactRef {
                    namespace: namespace.to_string(),
                    name: name.to_string(),
                    timestamp: index_timestamp,
                    content_type: index_content_type,
                    source_task_id: index_src_task,
                    source_execution_id: index_src_execution,
                    source_agent_id: index_src_agent,
                },
            );
        }

        Ok(result)
    }

    /// Crash-safe append: read under lock, write to temp, atomic rename.
    pub async fn append(&self, namespace: &str, name: &str, content: &str) -> Result<PathBuf> {
        let path = self.resolve_path_safe(namespace, name)?;
        let content = content.to_string();
        let path_clone = path.clone();
        let ns_owned = namespace.to_string();
        let name_owned = name.to_string();

        let result = tokio::task::spawn_blocking(move || {
            use std::io::{Read, Write};

            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path_clone)
                .with_context(|| format!("Cannot open for append: {:?}", path_clone))?;

            file.lock_exclusive()
                .context("Failed to acquire exclusive lock")?;

            let mut existing = String::new();
            file.read_to_string(&mut existing)?;

            // Parse and update frontmatter timestamp
            let updated = if let Some((fm_str, body)) = split_frontmatter(&existing) {
                if let Ok(mut fm) = serde_yaml::from_str::<DurableFrontmatter>(&fm_str) {
                    fm.last_updated = Utc::now();
                    let yaml = serde_yaml::to_string(&fm).unwrap_or(fm_str);
                    format!("---\n{}---\n{}{}", yaml, body, content)
                } else {
                    format!("{}{}", existing, content)
                }
            } else {
                format!("{}{}", existing, content)
            };

            // Write to temp file, then atomic rename (crash-safe).
            let parent = path_clone
                .parent()
                .context("Cannot determine parent directory")?;
            let tmp_path = parent.join(format!(".tmp-{}", uuid::Uuid::new_v4()));
            let mut tmp_file = std::fs::File::create(&tmp_path)
                .with_context(|| format!("Cannot create temp file: {:?}", tmp_path))?;
            tmp_file.write_all(updated.as_bytes())?;
            tmp_file.sync_all()?;
            drop(tmp_file);

            std::fs::rename(&tmp_path, &path_clone)
                .with_context(|| format!("Failed to rename {:?} → {:?}", tmp_path, path_clone))?;

            // Lock released when file (old fd) is dropped.
            Ok::<PathBuf, anyhow::Error>(path_clone)
        })
        .await??;

        // Reindex after append (frontmatter timestamp changed).
        // Use spawn_blocking to avoid blocking the Tokio runtime thread.
        let self_clone = self.clone();
        let _ = tokio::task::spawn_blocking(move || {
            self_clone.reindex_artifact(&ns_owned, &name_owned);
        })
        .await;

        Ok(result)
    }

    /// Read a durable artifact, returning frontmatter and body separately.
    /// Uses lock_shared() to prevent torn reads from concurrent writers.
    pub async fn read(&self, namespace: &str, name: &str) -> Result<(DurableFrontmatter, String)> {
        let path = self.resolve_path_safe(namespace, name)?;
        let path_clone = path.clone();

        let content = tokio::task::spawn_blocking(move || {
            use std::io::Read as _;
            let mut file = std::fs::File::open(&path_clone)
                .with_context(|| format!("Cannot read: {:?}", path_clone))?;
            file.lock_shared()
                .context("Failed to acquire shared lock for read")?;
            let mut content = String::new();
            file.read_to_string(&mut content)?;
            // Lock released when file is dropped
            Ok::<String, anyhow::Error>(content)
        })
        .await
        .context("spawn_blocking panicked")??;

        let (fm_str, body) =
            split_frontmatter(&content).context("No frontmatter found in artifact")?;
        let fm: DurableFrontmatter =
            serde_yaml::from_str(&fm_str).context("Invalid frontmatter YAML")?;
        Ok((fm, body))
    }

    /// List durable artifacts, optionally filtered by namespace.
    pub fn list(&self, namespace: Option<&str>) -> Result<Vec<DurableArtifactEntry>> {
        let mut entries = Vec::new();

        if let Some(ns) = namespace {
            // I2 fix: Validate namespace to prevent path traversal
            if ns.contains("..") || ns.starts_with('/') || ns.contains('\0') {
                anyhow::bail!("Invalid namespace: {:?}", ns);
            }
            let ns_dir = self.base_path.join(ns);
            if ns_dir.is_dir() {
                self.collect_entries_from_dir(&ns_dir, ns, &mut entries)?;
            }
        } else {
            // Iterate all namespace directories
            let read_dir = match std::fs::read_dir(&self.base_path) {
                Ok(rd) => rd,
                Err(_) => return Ok(entries),
            };
            for entry in read_dir.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(ns) = path.file_name().and_then(|n| n.to_str()) {
                        self.collect_entries_from_dir(&path, ns, &mut entries)?;
                    }
                }
            }
        }

        Ok(entries)
    }

    /// Check if a durable artifact exists.
    pub fn exists(&self, namespace: &str, name: &str) -> bool {
        self.resolve_path_safe(namespace, name)
            .map(|p| p.is_file())
            .unwrap_or(false)
    }

    /// Delete a durable artifact. Acquires exclusive lock to serialize with
    /// concurrent readers/writers and prevent silent data loss.
    pub async fn delete(&self, namespace: &str, name: &str) -> Result<()> {
        let path = self.resolve_path_safe(namespace, name)?;
        let base = self.base_path.clone();
        let path_clone = path.clone();

        tokio::task::spawn_blocking(move || {
            if !path_clone.exists() {
                return Ok::<(), anyhow::Error>(());
            }
            // Acquire exclusive lock before removing — waits for any concurrent
            // readers (shared lock) or writers (exclusive lock) to finish.
            let lock_file = std::fs::OpenOptions::new()
                .read(true)
                .open(&path_clone)
                .with_context(|| format!("Cannot open for delete: {:?}", path_clone))?;
            lock_file
                .lock_exclusive()
                .context("Failed to acquire exclusive lock for delete")?;

            std::fs::remove_file(&path_clone)?;
            // Lock released when lock_file is dropped (fd to deleted inode).

            // Clean up empty namespace dir
            if let Some(parent) = path_clone.parent() {
                if parent != base {
                    let _ = std::fs::remove_dir(parent); // ignore if non-empty
                }
            }
            Ok(())
        })
        .await
        .context("spawn_blocking panicked")??;

        // Update tag index after successful delete
        {
            let mut idx = self.index.write().unwrap_or_else(|e| {
                tracing::error!("[TAG-INDEX] RwLock poisoned, recovering: {}", e);
                e.into_inner()
            });
            idx.remove(namespace, name);
        }

        Ok(())
    }

    /// Re-read frontmatter from disk and update the tag index.
    /// Called after out-of-band frontmatter mutation (e.g., executor provenance stamping).
    pub fn reindex_artifact(&self, namespace: &str, name: &str) {
        let path = match self.resolve_path_safe(namespace, name) {
            Ok(p) => p,
            Err(_) => return,
        };
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return,
        };
        let frontmatter = split_frontmatter(&content)
            .and_then(|(fm_str, _)| serde_yaml::from_str::<DurableFrontmatter>(&fm_str).ok());

        {
            let mut idx = self.index.write().unwrap_or_else(|e| {
                tracing::error!("[TAG-INDEX] RwLock poisoned, recovering: {}", e);
                e.into_inner()
            });
            idx.remove(namespace, name);
            let (tags, timestamp, content_type, src_task, src_execution, src_agent) =
                if let Some(ref fm) = frontmatter {
                    (
                        derive_tags(fm),
                        fm.last_updated.timestamp(),
                        fm.content_type.clone(),
                        fm.source_task_id.clone(),
                        fm.source_execution_id.clone(),
                        fm.source_agent_id.clone(),
                    )
                } else {
                    (derive_tags_from_path(namespace), 0, None, None, None, None)
                };
            idx.insert(
                &tags,
                ArtifactRef {
                    namespace: namespace.to_string(),
                    name: name.to_string(),
                    timestamp,
                    content_type,
                    source_task_id: src_task,
                    source_execution_id: src_execution,
                    source_agent_id: src_agent,
                },
            );
        }
    }

    /// List artifacts matching ALL provided tags (intersection).
    pub fn list_by_tags(&self, tags: &[&str]) -> Vec<DurableArtifactEntry> {
        let idx = self.index.read().unwrap_or_else(|e| {
            tracing::error!("[TAG-INDEX] RwLock poisoned on read, recovering: {}", e);
            e.into_inner()
        });
        let refs = idx.get_intersection(tags);
        refs.into_iter()
            .map(|r| {
                let path = self.base_path.join(&r.namespace).join(&r.name);
                // Construct minimal frontmatter from index data
                let frontmatter = Some(DurableFrontmatter {
                    namespace: r.namespace.clone(),
                    name: r.name.clone(),
                    created_by: String::new(),
                    last_updated_by: String::new(),
                    last_updated: chrono::DateTime::from_timestamp(r.timestamp, 0)
                        .unwrap_or_else(chrono::Utc::now),
                    content_type: r.content_type.clone(),
                    source_task_id: r.source_task_id.clone(),
                    source_execution_id: r.source_execution_id.clone(),
                    source_agent_id: r.source_agent_id.clone(),
                    source_workflow_instance_id: None,
                    source_run_id: None,
                    source_cycle_id: None,
                    producer_stage: None,
                });
                DurableArtifactEntry {
                    namespace: r.namespace,
                    name: r.name,
                    path,
                    frontmatter,
                }
            })
            .collect()
    }

    fn collect_entries_from_dir(
        &self,
        dir: &Path,
        namespace: &str,
        entries: &mut Vec<DurableArtifactEntry>,
    ) -> Result<()> {
        let read_dir = std::fs::read_dir(dir)?;
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.is_file() {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();

                // Try to read frontmatter (best-effort)
                let frontmatter = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|content| split_frontmatter(&content))
                    .and_then(|(fm_str, _)| {
                        serde_yaml::from_str::<DurableFrontmatter>(&fm_str).ok()
                    });

                entries.push(DurableArtifactEntry {
                    namespace: namespace.to_string(),
                    name,
                    path: path.clone(),
                    frontmatter,
                });
            }
        }
        Ok(())
    }
}

#[async_trait]
impl DurableArtifactAccess for DurableArtifactStore {
    async fn write(
        &self,
        namespace: &str,
        name: &str,
        content: &str,
        frontmatter: DurableFrontmatter,
    ) -> Result<()> {
        DurableArtifactStore::write(self, namespace, name, content, frontmatter)
            .await
            .map(|_| ())
    }

    async fn append(&self, namespace: &str, name: &str, content: &str) -> Result<()> {
        DurableArtifactStore::append(self, namespace, name, content)
            .await
            .map(|_| ())
    }

    async fn read(&self, namespace: &str, name: &str) -> Result<(DurableFrontmatter, String)> {
        DurableArtifactStore::read(self, namespace, name).await
    }

    async fn list_entries(&self, namespace: Option<&str>) -> Result<Vec<DurableArtifactEntry>> {
        DurableArtifactStore::list(self, namespace)
    }

    async fn artifact_exists(&self, namespace: &str, name: &str) -> Result<bool> {
        Ok(DurableArtifactStore::exists(self, namespace, name))
    }

    async fn delete(&self, namespace: &str, name: &str) -> Result<()> {
        DurableArtifactStore::delete(self, namespace, name).await
    }

    async fn export_all(&self) -> Result<Vec<u8>> {
        let entries = DurableArtifactStore::list(self, None)?;
        let mut records = Vec::new();
        for entry in entries {
            let (frontmatter, body) = self.read(&entry.namespace, &entry.name).await?;
            records.push(ExportedDurable {
                namespace: entry.namespace,
                name: entry.name,
                frontmatter,
                body,
            });
        }
        Ok(serde_json::to_vec(&records)?)
    }

    async fn import_all(&self, bytes: &[u8]) -> Result<()> {
        let records: Vec<ExportedDurable> = serde_json::from_slice(bytes)?;
        for record in records {
            DurableArtifactStore::write(
                self,
                &record.namespace,
                &record.name,
                &record.body,
                record.frontmatter,
            )
            .await?;
        }
        Ok(())
    }
}

/// Split YAML frontmatter delimited by `---` from the body.
/// Returns (frontmatter_yaml, body) or None if no frontmatter found.
pub fn split_frontmatter(content: &str) -> Option<(String, String)> {
    let trimmed = content.strip_prefix("---\n")?;
    let end = trimmed.find("\n---\n")?;
    let fm = trimmed[..end].to_string();
    let body = trimmed[end + 5..].to_string(); // skip "\n---\n"
    Some((fm, body))
}

/// Lightweight reference to a durable artifact for tag-based lookup.
#[derive(Debug, Clone)]
pub struct ArtifactRef {
    pub namespace: String,
    pub name: String,
    pub timestamp: i64,
    pub content_type: Option<String>,
    pub source_task_id: Option<String>,
    pub source_execution_id: Option<String>,
    pub source_agent_id: Option<String>,
}

/// In-memory index mapping tags to artifact references.
/// Tags are derived from frontmatter provenance fields, never persisted.
#[derive(Debug, Default)]
pub struct TagIndex {
    tag_to_artifacts: HashMap<String, Vec<ArtifactRef>>,
}

impl TagIndex {
    /// Insert an artifact reference under all its derived tags.
    pub fn insert(&mut self, tags: &[String], artifact: ArtifactRef) {
        for tag in tags {
            self.tag_to_artifacts
                .entry(tag.clone())
                .or_default()
                .push(artifact.clone());
        }
    }

    /// Remove all entries matching (namespace, name) from all tag buckets.
    pub fn remove(&mut self, namespace: &str, name: &str) {
        for entries in self.tag_to_artifacts.values_mut() {
            entries.retain(|a| !(a.namespace == namespace && a.name == name));
        }
        self.tag_to_artifacts.retain(|_, v| !v.is_empty());
    }

    /// Get all artifacts for a single tag.
    pub fn get(&self, tag: &str) -> Vec<ArtifactRef> {
        self.tag_to_artifacts.get(tag).cloned().unwrap_or_default()
    }

    /// Get all artifacts matching ALL tags (intersection).
    pub fn get_intersection(&self, tags: &[&str]) -> Vec<ArtifactRef> {
        if tags.is_empty() {
            return self.all();
        }
        let mut result: Option<Vec<ArtifactRef>> = None;
        for tag in tags {
            let entries = self.get(tag);
            result = Some(match result {
                None => entries,
                Some(current) => current
                    .into_iter()
                    .filter(|a| {
                        entries
                            .iter()
                            .any(|e| e.namespace == a.namespace && e.name == a.name)
                    })
                    .collect(),
            });
        }
        result.unwrap_or_default()
    }

    /// Get all unique indexed artifacts.
    pub fn all(&self) -> Vec<ArtifactRef> {
        let mut seen = std::collections::HashSet::new();
        let mut result = Vec::new();
        for entries in self.tag_to_artifacts.values() {
            for a in entries {
                let key = format!("{}/{}", a.namespace, a.name);
                if seen.insert(key) {
                    result.push(a.clone());
                }
            }
        }
        result
    }
}

/// Derive tags from frontmatter provenance fields.
pub fn derive_tags(frontmatter: &DurableFrontmatter) -> Vec<String> {
    let mut tags = vec![frontmatter.namespace.clone()];
    if let Some(ref tid) = frontmatter.source_task_id {
        tags.push(format!("task:{}", tid));
    }
    if let Some(ref aid) = frontmatter.source_agent_id {
        tags.push(format!("agent:{}", aid));
    }
    if let Some(ref execution_id) = frontmatter.source_execution_id {
        tags.push(format!("execution:{}", execution_id));
    }
    tags
}

/// Derive minimal tags for files without valid frontmatter.
pub fn derive_tags_from_path(namespace: &str) -> Vec<String> {
    vec![namespace.to_string()]
}

/// Format a human-readable summary of durable artifacts for context injection.
/// Capped at `max_chars` to avoid bloating prompts.
pub fn format_durable_artifact_summary(
    entries: &[DurableArtifactEntry],
    base_path: &Path,
    max_chars: usize,
) -> String {
    // Filter out task_state namespace — it is auto-injected as a dedicated
    // "PERSISTED TASK STATE" prompt section, so listing it here would be redundant.
    let entries: Vec<_> = entries
        .iter()
        .filter(|e| e.namespace != "task_state")
        .collect();

    let abs_base = std::fs::canonicalize(base_path).unwrap_or_else(|_| base_path.to_path_buf());

    let mut summary = format!(
        "\n## Shared Workspace (persistent across runs)\nBase path: {}\n",
        abs_base.display()
    );

    for entry in &entries {
        if summary.len() >= max_chars {
            summary.push_str("  ... (more artifacts omitted)\n");
            break;
        }
        let ct = entry
            .frontmatter
            .as_ref()
            .and_then(|fm| fm.content_type.as_deref())
            .unwrap_or("unknown");
        let updated = entry
            .frontmatter
            .as_ref()
            .map(|fm| {
                let ago = Utc::now().signed_duration_since(fm.last_updated);
                if ago.num_hours() < 1 {
                    format!("{}m ago", ago.num_minutes().max(1))
                } else if ago.num_hours() < 24 {
                    format!("{}h ago", ago.num_hours())
                } else {
                    format!("{}d ago", ago.num_days())
                }
            })
            .unwrap_or_else(|| "unknown".to_string());
        summary.push_str(&format!(
            "- {}/{} ({}, updated {})\n",
            entry.namespace, entry.name, ct, updated
        ));
    }
    summary.push_str("Write/append to files in this directory to persist data across runs.\n");
    summary
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_frontmatter(ns: &str, name: &str) -> DurableFrontmatter {
        DurableFrontmatter {
            namespace: ns.to_string(),
            name: name.to_string(),
            created_by: "test-agent".to_string(),
            last_updated_by: "test-agent".to_string(),
            last_updated: Utc::now(),
            content_type: Some("text/markdown".to_string()),
            source_execution_id: None,
            source_task_id: None,
            source_workflow_instance_id: None,
            source_run_id: None,
            source_cycle_id: None,
            source_agent_id: None,
            producer_stage: None,
        }
    }

    #[tokio::test]
    async fn write_read_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let store = DurableArtifactStore::new(tmp.path().join("artifacts")).unwrap();

        let fm = test_frontmatter("billing", "payments.md");
        store
            .write("billing", "payments.md", "| Month | Amount |\n", fm)
            .await
            .unwrap();

        let (read_fm, body) = store.read("billing", "payments.md").await.unwrap();
        assert_eq!(read_fm.namespace, "billing");
        assert_eq!(read_fm.name, "payments.md");
        assert!(body.contains("| Month | Amount |"));
    }

    #[tokio::test]
    async fn append_with_locking() {
        let tmp = TempDir::new().unwrap();
        let store = DurableArtifactStore::new(tmp.path().join("artifacts")).unwrap();

        let fm = test_frontmatter("test", "data.md");
        store.write("test", "data.md", "line1\n", fm).await.unwrap();

        store.append("test", "data.md", "line2\n").await.unwrap();

        let (_, body) = store.read("test", "data.md").await.unwrap();
        assert!(body.contains("line1\n"));
        assert!(body.contains("line2\n"));
    }

    #[tokio::test]
    async fn list_artifacts() {
        let tmp = TempDir::new().unwrap();
        let store = DurableArtifactStore::new(tmp.path().join("artifacts")).unwrap();

        store
            .write("ns1", "a.md", "content-a", test_frontmatter("ns1", "a.md"))
            .await
            .unwrap();
        store
            .write("ns2", "b.md", "content-b", test_frontmatter("ns2", "b.md"))
            .await
            .unwrap();

        let all = store.list(None).unwrap();
        assert_eq!(all.len(), 2);

        let ns1 = store.list(Some("ns1")).unwrap();
        assert_eq!(ns1.len(), 1);
        assert_eq!(ns1[0].name, "a.md");
    }

    #[tokio::test]
    async fn exists_and_delete() {
        let tmp = TempDir::new().unwrap();
        let store = DurableArtifactStore::new(tmp.path().join("artifacts")).unwrap();

        store
            .write("ns", "file.md", "data", test_frontmatter("ns", "file.md"))
            .await
            .unwrap();

        assert!(store.exists("ns", "file.md"));
        store.delete("ns", "file.md").await.unwrap();
        assert!(!store.exists("ns", "file.md"));
    }

    #[test]
    fn split_frontmatter_works() {
        let content = "---\nnamespace: test\nname: foo\n---\nbody content";
        let (fm, body) = split_frontmatter(content).unwrap();
        assert!(fm.contains("namespace: test"));
        assert_eq!(body, "body content");
    }

    #[tokio::test]
    async fn frontmatter_ownership_and_provenance_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let store = DurableArtifactStore::new(tmp.path().join("artifacts")).unwrap();

        let fm = DurableFrontmatter {
            namespace: "billing".to_string(),
            name: "adjust.csv".to_string(),
            created_by: "test-agent".to_string(),
            last_updated_by: "test-agent".to_string(),
            last_updated: Utc::now(),
            content_type: Some("text/csv".to_string()),
            source_execution_id: Some("exec-1".to_string()),
            source_task_id: Some("task-abc-123".to_string()),
            source_workflow_instance_id: Some("wf-7".to_string()),
            source_run_id: Some("run-9".to_string()),
            source_cycle_id: Some("cycle-4".to_string()),
            source_agent_id: None,
            producer_stage: Some("publish".to_string()),
        };

        store
            .write("billing", "adjust.csv", "Month,Amount\n", fm)
            .await
            .unwrap();

        let (read_fm, _body) = store.read("billing", "adjust.csv").await.unwrap();
        assert_eq!(read_fm.source_execution_id, Some("exec-1".to_string()));
        assert_eq!(read_fm.source_task_id, Some("task-abc-123".to_string()));
        assert_eq!(
            read_fm.source_workflow_instance_id,
            Some("wf-7".to_string())
        );
        assert_eq!(read_fm.source_run_id, Some("run-9".to_string()));
        assert_eq!(read_fm.source_cycle_id, Some("cycle-4".to_string()));
        assert_eq!(read_fm.producer_stage, Some("publish".to_string()));
    }

    #[test]
    fn split_frontmatter_returns_none_without_delimiters() {
        assert!(split_frontmatter("no frontmatter here").is_none());
    }

    #[test]
    fn format_summary_caps_at_max() {
        let entries: Vec<DurableArtifactEntry> = (0..100)
            .map(|i| DurableArtifactEntry {
                namespace: "ns".to_string(),
                name: format!("file-{}.md", i),
                path: PathBuf::from(format!("/tmp/ns/file-{}.md", i)),
                frontmatter: Some(DurableFrontmatter {
                    namespace: "ns".to_string(),
                    name: format!("file-{}.md", i),
                    created_by: "agent".to_string(),
                    last_updated_by: "agent".to_string(),
                    last_updated: Utc::now(),
                    content_type: Some("text/markdown".to_string()),
                    source_execution_id: None,
                    source_task_id: None,
                    source_workflow_instance_id: None,
                    source_run_id: None,
                    source_cycle_id: None,
                    source_agent_id: None,
                    producer_stage: None,
                }),
            })
            .collect();

        let summary = format_durable_artifact_summary(&entries, Path::new("/tmp"), 500);
        // Summary truncates after cap — should contain the omitted message
        assert!(summary.contains("Shared Workspace"));
        assert!(summary.contains("more artifacts omitted"));
    }

    #[test]
    fn format_summary_filters_task_state_namespace() {
        let entries = vec![
            DurableArtifactEntry {
                namespace: "billing".to_string(),
                name: "report.md".to_string(),
                path: PathBuf::from("/tmp/billing/report.md"),
                frontmatter: Some(DurableFrontmatter {
                    namespace: "billing".to_string(),
                    name: "report.md".to_string(),
                    created_by: "agent".to_string(),
                    last_updated_by: "agent".to_string(),
                    last_updated: Utc::now(),
                    content_type: Some("text/markdown".to_string()),
                    source_execution_id: None,
                    source_task_id: None,
                    source_workflow_instance_id: None,
                    source_run_id: None,
                    source_cycle_id: None,
                    source_agent_id: None,
                    producer_stage: None,
                }),
            },
            DurableArtifactEntry {
                namespace: "task_state".to_string(),
                name: "task-abc-123.json".to_string(),
                path: PathBuf::from("/tmp/task_state/task-abc-123.json"),
                frontmatter: Some(DurableFrontmatter {
                    namespace: "task_state".to_string(),
                    name: "task-abc-123.json".to_string(),
                    created_by: "orchestrator".to_string(),
                    last_updated_by: "orchestrator".to_string(),
                    last_updated: Utc::now(),
                    content_type: Some("application/json".to_string()),
                    source_execution_id: None,
                    source_task_id: None,
                    source_workflow_instance_id: None,
                    source_run_id: None,
                    source_cycle_id: None,
                    source_agent_id: None,
                    producer_stage: None,
                }),
            },
        ];

        let summary = format_durable_artifact_summary(&entries, Path::new("/tmp"), 5000);
        // billing artifact should appear; task_state should be filtered out
        assert!(summary.contains("billing/report.md"));
        assert!(
            !summary.contains("task_state"),
            "task_state namespace should be filtered from summary"
        );
    }

    #[test]
    fn list_rejects_traversal_namespace() {
        let tmp = TempDir::new().unwrap();
        let store = DurableArtifactStore::new(tmp.path().join("artifacts")).unwrap();
        assert!(store.list(Some("../../etc")).is_err());
        assert!(store.list(Some("/etc")).is_err());
        // Valid namespace should work (even if empty)
        assert!(store.list(Some("valid-ns")).is_ok());
        assert!(store.list(None).is_ok());
    }

    #[test]
    fn resolve_path_safe_rejects_traversal() {
        let tmp = TempDir::new().unwrap();
        let store = DurableArtifactStore::new(tmp.path().join("artifacts")).unwrap();
        assert!(store.resolve_path_safe("..", "passwd").is_err());
        assert!(store.resolve_path_safe("ns", "../etc/passwd").is_err());
        assert!(store.resolve_path_safe("/etc", "passwd").is_err());
        assert!(store.resolve_path_safe("ns\0evil", "file.md").is_err());
        assert!(store.resolve_path_safe("ns", "file\0.md").is_err());
        assert!(store.resolve_path_safe("valid", "file.md").is_ok());
    }

    #[tokio::test]
    async fn tag_index_populated_on_write() {
        let tmp = TempDir::new().unwrap();
        let store = DurableArtifactStore::new(tmp.path().join("artifacts")).unwrap();

        let mut fm = test_frontmatter("ns1", "a.json");
        fm.source_task_id = Some("task-abc".to_string());
        fm.source_agent_id = Some("my-agent".to_string());
        store.write("ns1", "a.json", "{}", fm).await.unwrap();

        let by_task = store.list_by_tags(&["task:task-abc"]);
        assert_eq!(by_task.len(), 1);
        assert_eq!(by_task[0].name, "a.json");

        let by_agent = store.list_by_tags(&["agent:my-agent"]);
        assert_eq!(by_agent.len(), 1);

        let by_ns = store.list_by_tags(&["ns1"]);
        assert_eq!(by_ns.len(), 1);

        let intersection = store.list_by_tags(&["task:task-abc", "ns1"]);
        assert_eq!(intersection.len(), 1);

        let no_match = store.list_by_tags(&["task:xyz"]);
        assert!(no_match.is_empty());
    }

    #[tokio::test]
    async fn tag_index_survives_delete() {
        let tmp = TempDir::new().unwrap();
        let store = DurableArtifactStore::new(tmp.path().join("artifacts")).unwrap();

        let mut fm = test_frontmatter("ns", "file.json");
        fm.source_task_id = Some("task-1".to_string());
        store.write("ns", "file.json", "{}", fm).await.unwrap();

        assert_eq!(store.list_by_tags(&["task:task-1"]).len(), 1);
        store.delete("ns", "file.json").await.unwrap();
        assert!(store.list_by_tags(&["task:task-1"]).is_empty());
    }

    #[tokio::test]
    async fn tag_index_rebuilt_on_startup() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("artifacts");

        let store1 = DurableArtifactStore::new(&path).unwrap();
        let mut fm = test_frontmatter("ns", "report.json");
        fm.source_task_id = Some("task-persist".to_string());
        store1.write("ns", "report.json", "{}", fm).await.unwrap();

        // New store instance should rebuild index from disk
        let store2 = DurableArtifactStore::new(&path).unwrap();
        let results = store2.list_by_tags(&["task:task-persist"]);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "report.json");
    }

    #[tokio::test]
    async fn reindex_artifact_updates_tags() {
        let tmp = TempDir::new().unwrap();
        let store = DurableArtifactStore::new(tmp.path().join("artifacts")).unwrap();

        // Write without task_id
        let fm = test_frontmatter("ns", "file.json");
        store.write("ns", "file.json", "{}", fm).await.unwrap();
        assert!(store.list_by_tags(&["task:task-new"]).is_empty());

        // Manually update frontmatter on disk (simulating executor hook)
        let path = store.resolve_path_safe("ns", "file.json").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let (fm_str, body) = split_frontmatter(&content).unwrap();
        let mut fm: DurableFrontmatter = serde_yaml::from_str(&fm_str).unwrap();
        fm.source_task_id = Some("task-new".to_string());
        let yaml = serde_yaml::to_string(&fm).unwrap();
        std::fs::write(&path, format!("---\n{}---\n{}", yaml, body)).unwrap();

        // Before reindex: tag not found
        assert!(store.list_by_tags(&["task:task-new"]).is_empty());

        // After reindex: tag found
        store.reindex_artifact("ns", "file.json");
        assert_eq!(store.list_by_tags(&["task:task-new"]).len(), 1);
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tag_index_tests {
    use super::*;

    fn make_ref(ns: &str, name: &str) -> ArtifactRef {
        ArtifactRef {
            namespace: ns.to_string(),
            name: name.to_string(),
            timestamp: 0,
            content_type: None,
            source_task_id: None,
            source_execution_id: None,
            source_agent_id: None,
        }
    }

    #[test]
    fn insert_and_get_by_tag() {
        let mut idx = TagIndex::default();
        let tags = vec!["execution_artifacts".to_string(), "task:abc".to_string()];
        idx.insert(&tags, make_ref("execution_artifacts", "report.json"));
        assert_eq!(idx.get("task:abc").len(), 1);
        assert_eq!(idx.get("execution_artifacts").len(), 1);
        assert_eq!(idx.get("task:xyz").len(), 0);
    }

    #[test]
    fn intersection_of_tags() {
        let mut idx = TagIndex::default();
        idx.insert(
            &["execution_artifacts".to_string(), "task:abc".to_string()],
            make_ref("execution_artifacts", "a.json"),
        );
        idx.insert(
            &["execution_artifacts".to_string(), "task:def".to_string()],
            make_ref("execution_artifacts", "b.json"),
        );
        let result = idx.get_intersection(&["execution_artifacts", "task:abc"]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "a.json");
    }

    #[test]
    fn remove_cleans_all_buckets() {
        let mut idx = TagIndex::default();
        idx.insert(
            &["ns".to_string(), "task:abc".to_string()],
            make_ref("ns", "file.json"),
        );
        assert_eq!(idx.get("task:abc").len(), 1);
        idx.remove("ns", "file.json");
        assert_eq!(idx.get("task:abc").len(), 0);
        assert_eq!(idx.get("ns").len(), 0);
    }

    #[test]
    fn all_deduplicates() {
        let mut idx = TagIndex::default();
        idx.insert(
            &[
                "ns".to_string(),
                "task:abc".to_string(),
                "agent:x".to_string(),
            ],
            make_ref("ns", "file.json"),
        );
        // file.json appears in 3 tag buckets but all() should return it once
        assert_eq!(idx.all().len(), 1);
    }

    #[test]
    fn derive_tags_from_full_frontmatter() {
        let fm = DurableFrontmatter {
            namespace: "execution_artifacts".to_string(),
            name: "report.json".to_string(),
            created_by: "test".to_string(),
            last_updated_by: "test".to_string(),
            last_updated: Utc::now(),
            content_type: None,
            source_task_id: Some("task-123".to_string()),
            source_execution_id: Some("thread-456".to_string()),
            source_agent_id: Some("wealth-manager".to_string()),
            source_workflow_instance_id: None,
            source_run_id: None,
            source_cycle_id: None,
            producer_stage: None,
        };
        let tags = derive_tags(&fm);
        assert!(tags.contains(&"execution_artifacts".to_string()));
        assert!(tags.contains(&"task:task-123".to_string()));
        assert!(tags.contains(&"agent:wealth-manager".to_string()));
        assert!(tags.contains(&"execution:thread-456".to_string()));
        assert_eq!(tags.len(), 4);
    }

    #[test]
    fn derive_tags_with_sparse_provenance() {
        let fm = DurableFrontmatter {
            namespace: "task_state".to_string(),
            name: "state.json".to_string(),
            created_by: "test".to_string(),
            last_updated_by: "test".to_string(),
            last_updated: Utc::now(),
            content_type: None,
            source_task_id: Some("task-123".to_string()),
            source_execution_id: None,
            source_agent_id: None,
            source_workflow_instance_id: None,
            source_run_id: None,
            source_cycle_id: None,
            producer_stage: None,
        };
        let tags = derive_tags(&fm);
        assert_eq!(tags.len(), 2);
        assert!(tags.contains(&"task_state".to_string()));
        assert!(tags.contains(&"task:task-123".to_string()));
    }

    #[test]
    fn derive_tags_from_path_gives_namespace_only() {
        let tags = derive_tags_from_path("my_namespace");
        assert_eq!(tags, vec!["my_namespace".to_string()]);
    }
}
