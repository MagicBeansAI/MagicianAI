//! Durable approval request storage for Phase 3.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::warn;
use uuid::Uuid;

use super::{
    approval::PendingApproval,
    storage::{AgentStorage, AgentStorageError},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Validating,
    Approved,
    Rejected,
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    Sent,
    Resolved,
    Dismissed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub approval_id: String,
    pub agent_id: String,
    pub goal_id: String,
    pub trigger_seq: u64,
    pub cycle_id: String,
    #[serde(default)]
    pub execution_id: Option<String>,
    pub pending_actions: Vec<PendingApproval>,
    pub plan_hash: String,
    pub status: ApprovalStatus,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    #[serde(default)]
    pub resolved_by: Option<String>,
    #[serde(default)]
    pub resolved_at: Option<DateTime<Utc>>,
    /// Originating scope (the execution's principal/workspace at create time).
    /// Stamped onto the canonical `HitlResolved` event so a resolve from ANY
    /// surface is announced to the SAME scope that received the `HitlRequested` —
    /// otherwise a cross-surface resolve (e.g. from a logged-in web session) is
    /// invisible to the surface that showed the request (e.g. the anonymous/default
    /// desktop overlay), stranding its card + tray badge. `#[serde(default)]`
    /// keeps older persisted approvals (which lack the scope) readable.
    #[serde(default)]
    pub principal: Option<String>,
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalDelivery {
    pub delivery_id: String,
    pub approval_id: String,
    pub channel: String,
    pub status: DeliveryStatus,
    #[serde(default)]
    pub delivered_at: Option<DateTime<Utc>>,
}

impl ApprovalDelivery {
    pub fn sent(approval_id: &str, channel: &str) -> Self {
        Self {
            delivery_id: Uuid::new_v4().to_string(),
            approval_id: approval_id.to_string(),
            channel: channel.to_string(),
            status: DeliveryStatus::Sent,
            delivered_at: Some(Utc::now()),
        }
    }
}

#[derive(Debug, Error)]
pub enum ApprovalStoreError {
    #[error("approval `{0}` not found")]
    NotFound(String),
    #[error("approval storage operation `{operation}` failed: {details}")]
    Storage {
        operation: &'static str,
        details: String,
    },
}

impl ApprovalStoreError {
    fn storage(operation: &'static str, err: AgentStorageError) -> Self {
        Self::Storage {
            operation,
            details: err.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ApprovalStore {
    storage: AgentStorage,
    write_gate: Arc<Mutex<()>>,
}

impl ApprovalStore {
    pub fn new(storage: AgentStorage) -> Self {
        Self {
            storage,
            write_gate: Arc::new(Mutex::new(())),
        }
    }

    pub fn storage(&self) -> &AgentStorage {
        &self.storage
    }

    pub async fn save_request(&self, request: &ApprovalRequest) -> Result<(), ApprovalStoreError> {
        let _guard = self.write_gate.lock().await;
        self.storage
            .ensure_base_layout()
            .await
            .map_err(|err| ApprovalStoreError::storage("ensure_layout", err))?;
        self.storage
            .write_json_atomic(self.request_path(&request.approval_id), request)
            .await
            .map_err(|err| ApprovalStoreError::storage("save_request", err))
    }

    /// Atomically saves `request` only when no live matching pending/validating request exists.
    ///
    /// Matching key: `(agent_id, goal_id, cycle_id, plan_hash)`.
    /// Returns `(request, true)` when newly created, `(existing, false)` when deduped.
    pub async fn save_request_if_no_live_pending_match(
        &self,
        request: &ApprovalRequest,
        now: DateTime<Utc>,
    ) -> Result<(ApprovalRequest, bool), ApprovalStoreError> {
        let _guard = self.write_gate.lock().await;
        self.storage
            .ensure_base_layout()
            .await
            .map_err(|err| ApprovalStoreError::storage("ensure_layout", err))?;

        let files = self
            .storage
            .list_files_with_extension(self.storage.approvals_dir(), "json")
            .await
            .map_err(|err| ApprovalStoreError::storage("list_requests", err))?;
        for file in files {
            let Some(name) = file.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.ends_with(".deliveries.json") {
                continue;
            }
            let existing: ApprovalRequest = match self.storage.read_json(&file).await {
                Ok(value) => value,
                Err(err) => {
                    warn!(
                        path = %file.display(),
                        error = %err,
                        "Skipping unreadable approval request file during atomic dedup"
                    );
                    continue;
                },
            };
            let same_key = existing.agent_id == request.agent_id
                && existing.goal_id == request.goal_id
                && existing.cycle_id == request.cycle_id
                && existing.plan_hash == request.plan_hash;
            if same_key
                && matches!(
                    existing.status,
                    ApprovalStatus::Pending | ApprovalStatus::Validating
                )
                && existing.expires_at > now
            {
                return Ok((existing, false));
            }
        }

        self.storage
            .write_json_atomic(self.request_path(&request.approval_id), request)
            .await
            .map_err(|err| ApprovalStoreError::storage("save_request", err))?;
        Ok((request.clone(), true))
    }

    pub async fn get_request(
        &self,
        approval_id: &str,
    ) -> Result<Option<ApprovalRequest>, ApprovalStoreError> {
        self.storage
            .ensure_base_layout()
            .await
            .map_err(|err| ApprovalStoreError::storage("ensure_layout", err))?;
        let path = self.request_path(approval_id);
        if !self
            .storage
            .exists(&path)
            .await
            .map_err(|err| ApprovalStoreError::storage("request_exists", err))?
        {
            return Ok(None);
        }
        self.storage
            .read_json(path)
            .await
            .map(Some)
            .map_err(|err| ApprovalStoreError::storage("read_request", err))
    }

    pub async fn list_requests(&self) -> Result<Vec<ApprovalRequest>, ApprovalStoreError> {
        self.storage
            .ensure_base_layout()
            .await
            .map_err(|err| ApprovalStoreError::storage("ensure_layout", err))?;
        let mut requests = Vec::new();
        let files = self
            .storage
            .list_files_with_extension(self.storage.approvals_dir(), "json")
            .await
            .map_err(|err| ApprovalStoreError::storage("list_requests", err))?;
        for file in files {
            let Some(name) = file.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.ends_with(".deliveries.json") {
                continue;
            }
            match self.storage.read_json::<ApprovalRequest>(&file).await {
                Ok(request) => requests.push(request),
                Err(err) => {
                    warn!(
                        path = %file.display(),
                        error = %err,
                        "Skipping unreadable approval request file"
                    );
                },
            }
        }
        requests.sort_by_key(|r| r.created_at);
        Ok(requests)
    }

    pub async fn load_deliveries(
        &self,
        approval_id: &str,
    ) -> Result<Vec<ApprovalDelivery>, ApprovalStoreError> {
        self.storage
            .ensure_base_layout()
            .await
            .map_err(|err| ApprovalStoreError::storage("ensure_layout", err))?;
        let path = self.deliveries_path(approval_id);
        if !self
            .storage
            .exists(&path)
            .await
            .map_err(|err| ApprovalStoreError::storage("deliveries_exists", err))?
        {
            return Ok(Vec::new());
        }
        self.storage
            .read_json(path)
            .await
            .map_err(|err| ApprovalStoreError::storage("read_deliveries", err))
    }

    pub async fn save_delivery(
        &self,
        delivery: &ApprovalDelivery,
    ) -> Result<(), ApprovalStoreError> {
        let _guard = self.write_gate.lock().await;
        self.storage
            .ensure_base_layout()
            .await
            .map_err(|err| ApprovalStoreError::storage("ensure_layout", err))?;
        let path = self.deliveries_path(&delivery.approval_id);
        let mut deliveries = self
            .read_deliveries_unlocked(&path)
            .await
            .map_err(|err| ApprovalStoreError::storage("read_deliveries", err))?;
        if let Some(existing) = deliveries
            .iter_mut()
            .find(|existing| existing.delivery_id == delivery.delivery_id)
        {
            *existing = delivery.clone();
        } else {
            deliveries.push(delivery.clone());
        }
        self.storage
            .write_json_atomic(path, &deliveries)
            .await
            .map_err(|err| ApprovalStoreError::storage("save_delivery", err))
    }

    pub async fn compare_and_set_status(
        &self,
        approval_id: &str,
        expected: ApprovalStatus,
        new_status: ApprovalStatus,
        resolved_by: Option<&str>,
        resolved_at: Option<DateTime<Utc>>,
    ) -> Result<bool, ApprovalStoreError> {
        let _guard = self.write_gate.lock().await;
        self.storage
            .ensure_base_layout()
            .await
            .map_err(|err| ApprovalStoreError::storage("ensure_layout", err))?;
        let path = self.request_path(approval_id);
        if !self
            .storage
            .exists(&path)
            .await
            .map_err(|err| ApprovalStoreError::storage("request_exists", err))?
        {
            return Err(ApprovalStoreError::NotFound(approval_id.to_string()));
        }
        let mut request: ApprovalRequest = self
            .storage
            .read_json(&path)
            .await
            .map_err(|err| ApprovalStoreError::storage("read_request", err))?;
        if request.status != expected {
            return Ok(false);
        }

        request.status = new_status;
        if let Some(resolver) = resolved_by {
            let trimmed = resolver.trim();
            if !trimmed.is_empty() {
                request.resolved_by = Some(trimmed.to_string());
            }
        }
        if request.status == ApprovalStatus::Pending {
            // Recovery path (e.g. stale Validating -> Pending) must clear resolver metadata.
            request.resolved_by = None;
            request.resolved_at = None;
        } else {
            request.resolved_at = resolved_at.or_else(|| Some(Utc::now()));
        }

        self.storage
            .write_json_atomic(path, &request)
            .await
            .map_err(|err| ApprovalStoreError::storage("save_request", err))?;
        Ok(true)
    }

    pub async fn mark_other_deliveries_dismissed(
        &self,
        approval_id: &str,
        resolved_channel: &str,
    ) -> Result<(), ApprovalStoreError> {
        let _guard = self.write_gate.lock().await;
        self.storage
            .ensure_base_layout()
            .await
            .map_err(|err| ApprovalStoreError::storage("ensure_layout", err))?;
        let path = self.deliveries_path(approval_id);
        let mut deliveries = self
            .read_deliveries_unlocked(&path)
            .await
            .map_err(|err| ApprovalStoreError::storage("read_deliveries", err))?;
        let mut winner_set = false;
        for delivery in &mut deliveries {
            if delivery.channel == resolved_channel && !winner_set {
                delivery.status = DeliveryStatus::Resolved;
                delivery.delivered_at = Some(Utc::now());
                winner_set = true;
            } else if matches!(
                delivery.status,
                DeliveryStatus::Sent | DeliveryStatus::Resolved
            ) {
                delivery.status = DeliveryStatus::Dismissed;
            }
        }
        if !winner_set {
            deliveries.push(ApprovalDelivery {
                delivery_id: Uuid::new_v4().to_string(),
                approval_id: approval_id.to_string(),
                channel: resolved_channel.to_string(),
                status: DeliveryStatus::Resolved,
                delivered_at: Some(Utc::now()),
            });
        }
        self.storage
            .write_json_atomic(path, &deliveries)
            .await
            .map_err(|err| ApprovalStoreError::storage("save_deliveries", err))
    }

    pub async fn expire_pending_before(
        &self,
        now: DateTime<Utc>,
    ) -> Result<usize, ApprovalStoreError> {
        Ok(self.expire_pending_before_collect(now).await?.len())
    }

    pub async fn expire_pending_before_collect(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<ApprovalRequest>, ApprovalStoreError> {
        let _guard = self.write_gate.lock().await;
        self.storage
            .ensure_base_layout()
            .await
            .map_err(|err| ApprovalStoreError::storage("ensure_layout", err))?;
        let files = self
            .storage
            .list_files_with_extension(self.storage.approvals_dir(), "json")
            .await
            .map_err(|err| ApprovalStoreError::storage("list_requests", err))?;

        let mut expired = Vec::new();
        for file in files {
            let Some(name) = file.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.ends_with(".deliveries.json") {
                continue;
            }

            let mut request: ApprovalRequest = match self.storage.read_json(&file).await {
                Ok(value) => value,
                Err(err) => {
                    warn!(
                        path = %file.display(),
                        error = %err,
                        "Skipping unreadable approval request during expiry sweep"
                    );
                    continue;
                },
            };
            if matches!(
                request.status,
                ApprovalStatus::Pending | ApprovalStatus::Validating
            ) && request.expires_at <= now
            {
                request.status = ApprovalStatus::Expired;
                request.resolved_by = Some("system:expiry".to_string());
                request.resolved_at = Some(now);
                self.storage
                    .write_json_atomic(&file, &request)
                    .await
                    .map_err(|err| ApprovalStoreError::storage("save_request", err))?;
                expired.push(request);
            }
        }
        Ok(expired)
    }

    fn request_path(&self, approval_id: &str) -> std::path::PathBuf {
        self.storage
            .approvals_dir()
            .join(format!("{approval_id}.json"))
    }

    fn deliveries_path(&self, approval_id: &str) -> std::path::PathBuf {
        self.storage
            .approvals_dir()
            .join(format!("{approval_id}.deliveries.json"))
    }

    async fn read_deliveries_unlocked(
        &self,
        path: &std::path::Path,
    ) -> Result<Vec<ApprovalDelivery>, AgentStorageError> {
        if !self.storage.exists(path).await? {
            return Ok(Vec::new());
        }
        self.storage.read_json(path).await
    }
}
