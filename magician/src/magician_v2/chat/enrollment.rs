//! Enrollment Store — file-based identity store mapping (channel_type, address, workspace)
//! to principal.
//!
//! Storage: scoped JSON file
//! `magician_data_v3/scopes/<principal>/<workspace>/chat/enrollments.json`.
//! Writes go through the shared durable writer in `artifact_v2::io`: unique
//! temp name, `sync_all`, rename, parent-directory sync.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::magician_v2::artifact_v2::io::write_bytes_durably;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// On-disk representation of the enrollment store.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EnrollmentData {
    /// Confirmed enrollments: "channel_type:address" -> record.
    #[serde(default)]
    pub enrollments: HashMap<String, EnrollmentRecord>,
    /// Pending (unapproved) enrollments: code -> pending record.
    #[serde(default)]
    pub pending: HashMap<String, PendingEnrollment>,
}

/// A confirmed enrollment record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrollmentRecord {
    pub principal: String,
    pub workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub enrolled_at: i64,
}

/// A pending enrollment awaiting admin approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingEnrollment {
    pub channel_type: String,
    pub channel_address: String,
    pub workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub created_at: i64,
}

/// Result of an enroll attempt.
#[derive(Debug, Clone)]
pub enum EnrollResult {
    /// Already enrolled — returns existing principal.
    AlreadyEnrolled { principal: String },
    /// Auto-approved — enrolled now with the given principal.
    AutoApproved { principal: String },
    /// Pending approval — returns the approval code.
    Pending { code: String },
}

/// Result of an approval, including the channel info from the pending enrollment.
#[derive(Debug, Clone)]
pub struct ApprovalResult {
    pub record: EnrollmentRecord,
    pub channel_type: String,
    pub channel_address: String,
    pub workspace: String,
}

/// Status of an identity in the enrollment store.
#[derive(Debug, Clone)]
pub enum EnrollStatus {
    Enrolled { principal: String },
    Pending { code: String },
    Unknown,
}

// ---------------------------------------------------------------------------
// EnrollmentStore
// ---------------------------------------------------------------------------

/// File-based enrollment store. Thread-safe via `Arc<RwLock<...>>`.
#[derive(Clone)]
pub struct EnrollmentStore {
    file_path: PathBuf,
    data: Arc<RwLock<EnrollmentData>>,
}

impl EnrollmentStore {
    /// Create a new `EnrollmentStore`, loading from `file_path` if it exists.
    pub async fn new<P: AsRef<Path>>(file_path: P) -> Result<Self> {
        let file_path = file_path.as_ref().to_path_buf();
        let data = if file_path.exists() {
            let content = fs::read_to_string(&file_path)
                .await
                .with_context(|| format!("Failed to read enrollment file: {:?}", file_path))?;
            let parsed: EnrollmentData = serde_json::from_str(&content)
                .with_context(|| format!("Failed to parse enrollment file: {:?}", file_path))?;
            info!(
                "[ENROLLMENT] Loaded {} enrollments, {} pending from {:?}",
                parsed.enrollments.len(),
                parsed.pending.len(),
                file_path
            );
            parsed
        } else {
            debug!(
                "[ENROLLMENT] No enrollment file found at {:?}, starting fresh",
                file_path
            );
            EnrollmentData::default()
        };

        Ok(Self {
            file_path,
            data: Arc::new(RwLock::new(data)),
        })
    }

    /// Build the map key from channel_type, address, and workspace.
    fn key(channel_type: &str, address: &str, workspace: &str) -> String {
        format!("{}:{}:{}", channel_type, address, workspace)
    }

    /// Look up whether a (channel_type, address, workspace) tuple is enrolled.
    /// Returns the principal if enrolled, `None` otherwise.
    pub async fn resolve(
        &self,
        channel_type: &str,
        address: &str,
        workspace: &str,
    ) -> Option<String> {
        let data = self.data.read().await;
        data.enrollments
            .get(&Self::key(channel_type, address, workspace))
            .map(|r| r.principal.clone())
    }

    /// Attempt to enroll a (channel_type, address) pair.
    ///
    /// - If already enrolled, returns `AlreadyEnrolled`.
    /// - If `auto_approve` is true, creates the enrollment immediately with `default_principal`.
    /// - Otherwise, creates a pending enrollment and returns the approval code.
    pub async fn enroll(
        &self,
        channel_type: &str,
        address: &str,
        workspace: &str,
        display_name: Option<String>,
        auto_approve: bool,
        default_principal: &str,
    ) -> Result<EnrollResult> {
        let key = Self::key(channel_type, address, workspace);

        let mut data = self.data.write().await;

        // Already enrolled?
        if let Some(record) = data.enrollments.get(&key) {
            return Ok(EnrollResult::AlreadyEnrolled {
                principal: record.principal.clone(),
            });
        }

        if auto_approve {
            let now = Utc::now().timestamp_millis();
            let record = EnrollmentRecord {
                principal: default_principal.to_string(),
                workspace: workspace.to_string(),
                display_name,
                enrolled_at: now,
            };
            data.enrollments.insert(key, record);
            self.save_locked(&data).await?;
            return Ok(EnrollResult::AutoApproved {
                principal: default_principal.to_string(),
            });
        }

        // Check if there's already a pending enrollment for this identity.
        for (code, pending) in &data.pending {
            if pending.channel_type == channel_type
                && pending.channel_address == address
                && pending.workspace == workspace
            {
                return Ok(EnrollResult::Pending { code: code.clone() });
            }
        }

        // Create a new pending enrollment. The code is a CAPABILITY —
        // full-UUID entropy (~122 bits): it authorizes admin approval and,
        // since 2026-08-30, holder cancellation, so the earlier 8-hex-char
        // mint (~32 bits) was brute-forceable at network speed. It is shown
        // exactly once, at creation; length costs nothing.
        let code = Uuid::new_v4().to_string().to_uppercase();
        let pending = PendingEnrollment {
            channel_type: channel_type.to_string(),
            channel_address: address.to_string(),
            workspace: workspace.to_string(),
            display_name,
            created_at: Utc::now().timestamp_millis(),
        };
        data.pending.insert(code.clone(), pending);
        self.save_locked(&data).await?;

        Ok(EnrollResult::Pending { code })
    }

    /// Approve a pending enrollment by code, assigning the given principal.
    /// Returns the enrollment record along with the channel info from the pending entry.
    pub async fn approve(&self, code: &str, principal: &str) -> Result<ApprovalResult> {
        let pending = self.take_pending(code).await?;
        self.insert_approved(pending, principal).await
    }

    async fn take_pending(&self, code: &str) -> Result<PendingEnrollment> {
        let mut data = self.data.write().await;
        let pending = data
            .pending
            .remove(code)
            .ok_or_else(|| anyhow::anyhow!("Pending enrollment not found for code: {}", code))?;
        self.save_locked(&data).await?;
        Ok(pending)
    }

    async fn restore_pending(&self, code: &str, pending: PendingEnrollment) -> Result<()> {
        let mut data = self.data.write().await;
        data.pending.insert(code.to_string(), pending);
        self.save_locked(&data).await
    }

    async fn insert_approved(
        &self,
        pending: PendingEnrollment,
        principal: &str,
    ) -> Result<ApprovalResult> {
        let mut data = self.data.write().await;
        let channel_type = pending.channel_type.clone();
        let channel_address = pending.channel_address.clone();
        let workspace = pending.workspace.clone();
        let key = Self::key(&channel_type, &channel_address, &workspace);
        let now = Utc::now().timestamp_millis();
        let record = EnrollmentRecord {
            principal: principal.to_string(),
            workspace: workspace.clone(),
            display_name: pending.display_name,
            enrolled_at: now,
        };
        data.enrollments.insert(key, record.clone());
        self.save_locked(&data).await?;

        info!(
            "[ENROLLMENT] Approved enrollment for {}:{} as principal={}",
            channel_type, channel_address, principal
        );

        Ok(ApprovalResult {
            record,
            channel_type,
            channel_address,
            workspace,
        })
    }

    /// Remove an approved enrollment for the tuple. `true` when a record
    /// was removed. The channel is unknown again afterwards — the inbound
    /// resolver stops routing it, and a fresh enroll (by anyone) may claim
    /// it. This is the unenrollment/reassignment primitive; there is
    /// deliberately no in-place reassignment: revoke, then re-enroll, with
    /// the duplicate pre-scan keeping the gap race-free.
    pub async fn remove(&self, channel_type: &str, address: &str, workspace: &str) -> Result<bool> {
        let mut data = self.data.write().await;
        let key = Self::key(channel_type, address, workspace);
        let removed = data.enrollments.remove(&key).is_some();
        if removed {
            self.save_locked(&data).await?;
        }
        Ok(removed)
    }

    /// Cancel a pending enrollment by its code — the code is the
    /// capability, shown once at creation, so presenting it IS the
    /// authority to cancel. `None` when no such pending exists (unknown,
    /// already approved, already cancelled, or expired-and-swept are
    /// indistinguishable by design).
    pub async fn cancel_pending(&self, code: &str) -> Result<Option<PendingEnrollment>> {
        let mut data = self.data.write().await;
        match data.pending.remove(code) {
            Some(pending) => {
                self.save_locked(&data).await?;
                Ok(Some(pending))
            },
            None => Ok(None),
        }
    }

    /// Cancel a pending by channel identity rather than code — the owner's
    /// administrative arm (the owner may not hold codes). Pendings live
    /// only in this store's tree, so no cross-tree scan is involved.
    /// Returns the cancelled code, or `None` when no pending matches.
    pub async fn cancel_pending_by_channel(
        &self,
        channel_type: &str,
        address: &str,
        workspace: &str,
    ) -> Result<Option<String>> {
        let mut data = self.data.write().await;
        let matching = data
            .pending
            .iter()
            .find(|(_, pending)| {
                pending.channel_type == channel_type
                    && pending.channel_address == address
                    && pending.workspace == workspace
            })
            .map(|(code, _)| code.clone());
        match matching {
            Some(code) => {
                data.pending.remove(&code);
                self.save_locked(&data).await?;
                Ok(Some(code))
            },
            None => Ok(None),
        }
    }

    /// Check the enrollment status for a (channel_type, address, workspace) tuple.
    pub async fn status(&self, channel_type: &str, address: &str, workspace: &str) -> EnrollStatus {
        let data = self.data.read().await;
        let key = Self::key(channel_type, address, workspace);

        if let Some(record) = data.enrollments.get(&key) {
            return EnrollStatus::Enrolled {
                principal: record.principal.clone(),
            };
        }

        // Check pending
        for (code, pending) in &data.pending {
            if pending.channel_type == channel_type
                && pending.channel_address == address
                && pending.workspace == workspace
            {
                return EnrollStatus::Pending { code: code.clone() };
            }
        }

        EnrollStatus::Unknown
    }

    /// Remove expired pending enrollments older than `ttl_hours`.
    pub async fn cleanup_expired(&self, ttl_hours: u64) {
        let cutoff = Utc::now().timestamp_millis() - (ttl_hours as i64 * 3600 * 1000);
        let mut data = self.data.write().await;
        let before = data.pending.len();
        data.pending.retain(|_, p| p.created_at > cutoff);
        let removed = before - data.pending.len();
        if removed > 0 {
            debug!(
                "[ENROLLMENT] Cleaned up {} expired pending enrollments",
                removed
            );
            if let Err(e) = self.save_locked(&data).await {
                warn!("[ENROLLMENT] Failed to save after cleanup: {}", e);
            }
        }
    }

    /// Persist the enrollment data to disk through the shared durable writer.
    ///
    /// The previous hand-rolled write used a fixed `enrollments.json.tmp`,
    /// shared by every concurrent writer of this path, and never synced the
    /// parent directory — so the rename itself could be lost to a power cut.
    /// The helper creates the parent directory, stages under a unique temp
    /// name, fsyncs the file, renames, then fsyncs the directory.
    /// Takes the already-held borrow rather than re-locking.
    ///
    /// It used to acquire its own read lock, which forced every caller to
    /// `drop` the write guard first — there is a comment about the deadlock
    /// that follows if you do not. Dropping it also opened a window: two
    /// writers could each mutate the shared map, then serialize and write
    /// outside the lock, and the slower writer's older snapshot could land
    /// last. The file would then be behind memory, and a restart would read
    /// back the losing state.
    ///
    /// Serializing under the caller's guard closes the window and removes the
    /// deadlock at the same time, because there is no second acquisition to
    /// deadlock on.
    async fn save_locked(&self, data: &EnrollmentData) -> Result<()> {
        let content = serde_json::to_vec_pretty(data)?;
        write_bytes_durably(&self.file_path, &content).await?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct EnrollmentStoreResolver {
    workspace_layout: ArtifactV2Workspace,
    stores: Arc<RwLock<HashMap<(String, String), EnrollmentStore>>>,
}

impl EnrollmentStoreResolver {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self {
            workspace_layout,
            stores: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn resolve_for_scope(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<EnrollmentStore> {
        let key = (principal.to_string(), workspace.to_string());
        if let Some(store) = self.stores.read().await.get(&key).cloned() {
            return Ok(store);
        }

        let path = self
            .workspace_layout
            .chat_enrollments_path(principal, workspace);
        let store = EnrollmentStore::new(path).await?;

        let mut guard = self.stores.write().await;
        Ok(guard.entry(key).or_insert_with(|| store.clone()).clone())
    }

    async fn known_principals_for_workspace(&self, workspace: &str) -> Result<Vec<String>> {
        let mut principals = BTreeSet::new();
        for (principal, scoped_workspace) in self.workspace_layout.list_scope_segments().await? {
            if scoped_workspace == workspace {
                principals.insert(principal);
            }
        }
        Ok(principals.into_iter().collect())
    }

    pub async fn resolve_enrolled_principal(
        &self,
        workspace: &str,
        channel_type: &str,
        address: &str,
    ) -> Result<Option<String>> {
        for principal in self.known_principals_for_workspace(workspace).await? {
            let store = self.resolve_for_scope(&principal, workspace).await?;
            if let Some(resolved) = store.resolve(channel_type, address, workspace).await {
                return Ok(Some(resolved));
            }
        }
        Ok(None)
    }

    /// Remove the mapping for a channel wherever it lives, returning the
    /// principal that held it (`None` when nobody did). The caller owns the
    /// authorization decision — this only performs the removal, so handlers
    /// can check ownership BEFORE removing (under their mutation lock).
    pub async fn revoke_enrollment(
        &self,
        workspace: &str,
        channel_type: &str,
        address: &str,
    ) -> Result<Option<String>> {
        for principal in self.known_principals_for_workspace(workspace).await? {
            let store = self.resolve_for_scope(&principal, workspace).await?;
            if let Some(holder) = store.resolve(channel_type, address, workspace).await {
                // The record's stored principal names the holder even when
                // the tree it sits in is someone else's (legacy
                // default-registry rows); remove from the tree that answered.
                store.remove(channel_type, address, workspace).await?;
                return Ok(Some(holder));
            }
        }
        Ok(None)
    }

    pub async fn status(
        &self,
        default_principal: &str,
        workspace: &str,
        channel_type: &str,
        address: &str,
    ) -> Result<EnrollStatus> {
        let default_store = self.resolve_for_scope(default_principal, workspace).await?;
        let status = default_store.status(channel_type, address, workspace).await;
        if !matches!(status, EnrollStatus::Unknown) {
            return Ok(status);
        }

        for principal in self.known_principals_for_workspace(workspace).await? {
            if principal == default_principal {
                continue;
            }
            let store = self.resolve_for_scope(&principal, workspace).await?;
            if let Some(resolved) = store.resolve(channel_type, address, workspace).await {
                return Ok(EnrollStatus::Enrolled {
                    principal: resolved,
                });
            }
        }

        Ok(EnrollStatus::Unknown)
    }

    pub async fn approve(
        &self,
        default_principal: &str,
        workspace: &str,
        code: &str,
        principal: &str,
    ) -> Result<ApprovalResult> {
        let pending_store = self.resolve_for_scope(default_principal, workspace).await?;
        if principal == default_principal {
            return pending_store.approve(code, principal).await;
        }

        let pending = pending_store.take_pending(code).await?;
        let target_store = self.resolve_for_scope(principal, workspace).await?;
        match target_store
            .insert_approved(pending.clone(), principal)
            .await
        {
            Ok(result) => Ok(result),
            Err(error) => {
                let _ = pending_store.restore_pending(code, pending).await;
                Err(error)
            },
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_auto_approve_enrollment() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("enrollments.json");
        let store = EnrollmentStore::new(&file_path).await.unwrap();

        let result = store
            .enroll(
                "web",
                "abc-123",
                "default",
                Some("Test".to_string()),
                true,
                "default",
            )
            .await
            .unwrap();

        match result {
            EnrollResult::AutoApproved { principal } => {
                assert_eq!(principal, "default");
            },
            other => panic!("Expected AutoApproved, got {:?}", other),
        }

        // Verify resolve works
        let resolved = store.resolve("web", "abc-123", "default").await;
        assert_eq!(resolved, Some("default".to_string()));

        // Calling again returns AlreadyEnrolled
        let result2 = store
            .enroll("web", "abc-123", "default", None, true, "default")
            .await
            .unwrap();
        match result2 {
            EnrollResult::AlreadyEnrolled { principal } => {
                assert_eq!(principal, "default");
            },
            other => panic!("Expected AlreadyEnrolled, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_pending_enrollment_and_approval() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("enrollments.json");
        let store = EnrollmentStore::new(&file_path).await.unwrap();

        // Enroll without auto-approve
        let result = store
            .enroll(
                "telegram",
                "987654321",
                "default",
                Some("Alex".to_string()),
                false,
                "default",
            )
            .await
            .unwrap();

        let code = match result {
            EnrollResult::Pending { code } => code,
            other => panic!("Expected Pending, got {:?}", other),
        };

        // Status should be pending
        let status = store.status("telegram", "987654321", "default").await;
        match status {
            EnrollStatus::Pending { code: c } => assert_eq!(c, code),
            other => panic!("Expected Pending status, got {:?}", other),
        }

        // Approve
        let result = store.approve(&code, "owner").await.unwrap();
        assert_eq!(result.record.principal, "owner");
        assert_eq!(result.record.workspace, "default");
        assert_eq!(result.channel_type, "telegram");
        assert_eq!(result.channel_address, "987654321");
        assert_eq!(result.workspace, "default");

        // Now should be enrolled
        let resolved = store.resolve("telegram", "987654321", "default").await;
        assert_eq!(resolved, Some("owner".to_string()));
    }

    #[tokio::test]
    async fn test_status_unknown() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("enrollments.json");
        let store = EnrollmentStore::new(&file_path).await.unwrap();

        let status = store.status("discord", "unknown", "default").await;
        match status {
            EnrollStatus::Unknown => {},
            other => panic!("Expected Unknown, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_persistence_across_reload() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("enrollments.json");

        // Create and enroll
        {
            let store = EnrollmentStore::new(&file_path).await.unwrap();
            store
                .enroll("web", "uuid-1", "default", None, true, "default")
                .await
                .unwrap();
        }

        // Reload from disk
        {
            let store = EnrollmentStore::new(&file_path).await.unwrap();
            let resolved = store.resolve("web", "uuid-1", "default").await;
            assert_eq!(resolved, Some("default".to_string()));
        }
    }

    #[tokio::test]
    async fn test_cleanup_expired() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("enrollments.json");
        let store = EnrollmentStore::new(&file_path).await.unwrap();

        // Insert a pending enrollment with an old timestamp
        {
            let mut data = store.data.write().await;
            data.pending.insert(
                "OLD123".to_string(),
                PendingEnrollment {
                    channel_type: "telegram".to_string(),
                    channel_address: "old_user".to_string(),
                    workspace: "default".to_string(),
                    display_name: None,
                    created_at: 0, // epoch — very old
                },
            );
        }

        store.cleanup_expired(24).await;

        let status = store.status("telegram", "old_user", "default").await;
        match status {
            EnrollStatus::Unknown => {},
            other => panic!("Expected Unknown after cleanup, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn resolver_scopes_enrollment_storage_by_scope() {
        let dir = tempdir().unwrap();
        let layout = ArtifactV2Workspace::new(dir.path());
        let resolver = EnrollmentStoreResolver::new(layout.clone());

        let store_a = resolver
            .resolve_for_scope("principal-a", "workspace-a")
            .await
            .unwrap();
        store_a
            .enroll("web", "abc", "workspace-a", None, true, "principal-a")
            .await
            .unwrap();

        let store_b = resolver
            .resolve_for_scope("principal-b", "workspace-b")
            .await
            .unwrap();
        assert_eq!(store_b.resolve("web", "abc", "workspace-b").await, None);

        let expected_path = layout.chat_enrollments_path("principal-a", "workspace-a");
        assert!(expected_path.exists());
    }

    #[tokio::test]
    async fn resolver_moves_approved_enrollment_into_target_scope() {
        let dir = tempdir().unwrap();
        let layout = ArtifactV2Workspace::new(dir.path());
        let resolver = EnrollmentStoreResolver::new(layout.clone());

        let pending_store = resolver
            .resolve_for_scope("default", "workspace-a")
            .await
            .unwrap();
        let EnrollResult::Pending { code } = pending_store
            .enroll("telegram", "42", "workspace-a", None, false, "default")
            .await
            .unwrap()
        else {
            panic!("expected pending enrollment");
        };

        let result = resolver
            .approve("default", "workspace-a", &code, "owner")
            .await
            .unwrap();
        assert_eq!(result.record.principal, "owner");

        let target_store = resolver
            .resolve_for_scope("owner", "workspace-a")
            .await
            .unwrap();
        assert_eq!(
            target_store.resolve("telegram", "42", "workspace-a").await,
            Some("owner".to_string())
        );
        assert_eq!(
            pending_store.resolve("telegram", "42", "workspace-a").await,
            None
        );
    }

    /// Two writers of the same enrollment file must leave a parseable store and
    /// no staging file.
    ///
    /// Each store owns its own in-memory copy, so one of the two enrollments is
    /// expected to be lost — that read-modify-write race is Phase 3's problem
    /// and is deliberately not asserted here. What must hold is that the file a
    /// reader finds is whole.
    #[tokio::test]
    async fn concurrent_saves_leave_a_parseable_file_and_no_staging_file() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("enrollments.json");

        let first = EnrollmentStore::new(&file_path).await.unwrap();
        let second = EnrollmentStore::new(&file_path).await.unwrap();

        let (first_result, second_result) = tokio::join!(
            first.enroll("web", "writer-a", "default", None, true, "default"),
            second.enroll("web", "writer-b", "default", None, true, "default"),
        );
        first_result.expect("first enrollment should persist");
        second_result.expect("second enrollment should persist");

        let staging: Vec<String> = std::fs::read_dir(dir.path())
            .expect("scope directory listing")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(
            staging.is_empty(),
            "durable writes must leave no staging file, found {staging:?}"
        );

        let published = fs::read_to_string(&file_path)
            .await
            .expect("enrollment file should be readable");
        let parsed: EnrollmentData =
            serde_json::from_str(&published).expect("published enrollments should parse");
        assert!(
            !parsed.enrollments.is_empty(),
            "a completed enrollment must survive the concurrent write"
        );
    }
}
