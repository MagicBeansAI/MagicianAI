//! Durable proposal storage and resolution primitives for Phase 5.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::warn;
use uuid::Uuid;

use super::{
    definition_store::DefinitionStoreError,
    storage::{validate_agent_identifier, validate_identifier, AgentStorage, AgentStorageError},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProposalStatus {
    Pending,
    Approved,
    Rejected,
    Deferred,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProposalDecision {
    Approve,
    Reject,
    Defer,
}

impl ProposalDecision {
    pub fn target_status(&self) -> ProposalStatus {
        match self {
            Self::Approve => ProposalStatus::Approved,
            Self::Reject => ProposalStatus::Rejected,
            Self::Defer => ProposalStatus::Deferred,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DefinitionProposal {
    pub proposal_id: String,
    pub agent_id: String,
    pub source: String,
    pub status: ProposalStatus,
    pub payload: Value,
    pub yaml_before: String,
    pub yaml_after: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub resolved_by: Option<String>,
    #[serde(default)]
    pub resolved_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub applied_version: Option<u32>,
    #[serde(default)]
    pub applied_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct NewDefinitionProposal {
    pub agent_id: String,
    pub source: String,
    pub payload: Value,
    pub yaml_before: String,
    pub yaml_after: String,
}

#[derive(Debug, Clone, Default)]
pub struct ProposalFilter {
    pub agent_id: Option<String>,
    pub status: Option<ProposalStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalResolveOutcome {
    Resolved,
    Noop,
}

#[derive(Debug, Error)]
pub enum ProposalStoreError {
    #[error("invalid proposal id `{0}`")]
    InvalidProposalId(String),
    #[error("proposal `{0}` not found")]
    NotFound(String),
    #[error("stale proposal transition for `{proposal_id}` from `{current_status:?}` to `{attempted_status:?}`")]
    StaleTransition {
        proposal_id: String,
        current_status: ProposalStatus,
        attempted_status: ProposalStatus,
    },
    #[error("invalid proposal request: {0}")]
    InvalidRequest(String),
    #[error("proposal storage operation `{operation}` failed: {details}")]
    Storage {
        operation: &'static str,
        details: String,
    },
}

#[derive(Debug, Error)]
pub enum ProposalApplicationError {
    #[error("proposal `{0}` is not approved")]
    NotApproved(String),
    #[error("agent `{0}` not found in definition store")]
    AgentNotFound(String),
    #[error("yaml_after failed validation: {0}")]
    InvalidYaml(String),
    #[error("trust policy validation failed: {0}")]
    TrustPolicy(String),
    #[error("definition store error: {0}")]
    DefinitionStore(#[from] DefinitionStoreError),
    #[error("proposal store error: {0}")]
    ProposalStore(#[from] ProposalStoreError),
}

impl ProposalStoreError {
    fn storage(operation: &'static str, err: AgentStorageError) -> Self {
        Self::Storage {
            operation,
            details: err.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProposalStore {
    storage: AgentStorage,
    write_gate: Arc<Mutex<()>>,
}

impl ProposalStore {
    pub fn new(storage: AgentStorage) -> Self {
        Self {
            storage,
            write_gate: Arc::new(Mutex::new(())),
        }
    }

    pub fn storage(&self) -> &AgentStorage {
        &self.storage
    }

    pub fn validate_proposal_id(proposal_id: &str) -> Result<String, ProposalStoreError> {
        if proposal_id.trim().is_empty() {
            return Err(ProposalStoreError::InvalidProposalId(
                proposal_id.to_string(),
            ));
        }
        validate_identifier(proposal_id)
            .map_err(|_| ProposalStoreError::InvalidProposalId(proposal_id.to_string()))?;
        let canonical = Uuid::try_parse(proposal_id)
            .map_err(|_| ProposalStoreError::InvalidProposalId(proposal_id.to_string()))?
            .hyphenated()
            .to_string();
        Ok(canonical)
    }

    pub async fn create_proposal(
        &self,
        proposal: NewDefinitionProposal,
    ) -> Result<DefinitionProposal, ProposalStoreError> {
        validate_agent_identifier(&proposal.agent_id).map_err(|_| {
            ProposalStoreError::InvalidRequest(
                "agent_id must be a valid agent identifier".to_string(),
            )
        })?;
        let source = proposal.source.trim();
        if source.is_empty() {
            return Err(ProposalStoreError::InvalidRequest(
                "source must not be empty".to_string(),
            ));
        }
        if proposal.yaml_before.trim().is_empty() {
            return Err(ProposalStoreError::InvalidRequest(
                "yaml_before must not be empty".to_string(),
            ));
        }
        if proposal.yaml_after.trim().is_empty() {
            return Err(ProposalStoreError::InvalidRequest(
                "yaml_after must not be empty".to_string(),
            ));
        }
        if !proposal.payload.is_object() {
            return Err(ProposalStoreError::InvalidRequest(
                "payload must be a JSON object".to_string(),
            ));
        }

        let created_at = Utc::now();
        let record = DefinitionProposal {
            proposal_id: Uuid::new_v4().to_string(),
            agent_id: proposal.agent_id,
            source: source.to_string(),
            status: ProposalStatus::Pending,
            payload: proposal.payload,
            yaml_before: proposal.yaml_before,
            yaml_after: proposal.yaml_after,
            created_at,
            updated_at: created_at,
            resolved_by: None,
            resolved_at: None,
            applied_version: None,
            applied_at: None,
        };

        let _guard = self.write_gate.lock().await;
        self.storage
            .ensure_base_layout()
            .await
            .map_err(|err| ProposalStoreError::storage("ensure_layout", err))?;
        self.storage
            .write_json_atomic(self.proposal_path(&record.proposal_id), &record)
            .await
            .map_err(|err| ProposalStoreError::storage("create_proposal", err))?;
        Ok(record)
    }

    pub async fn get_proposal(
        &self,
        proposal_id: &str,
    ) -> Result<Option<DefinitionProposal>, ProposalStoreError> {
        let proposal_id = Self::validate_proposal_id(proposal_id)?;
        self.storage
            .ensure_base_layout()
            .await
            .map_err(|err| ProposalStoreError::storage("ensure_layout", err))?;
        let path = self.proposal_path(&proposal_id);
        if !self
            .storage
            .exists(&path)
            .await
            .map_err(|err| ProposalStoreError::storage("proposal_exists", err))?
        {
            return Ok(None);
        }
        self.storage
            .read_json(path)
            .await
            .map(Some)
            .map_err(|err| ProposalStoreError::storage("read_proposal", err))
    }

    /// List proposals matching the given filter.
    ///
    /// Note: reads all proposal files from disk and filters in memory.
    /// Acceptable at current scale; consider indexed storage if proposal
    /// counts exceed ~1000.
    pub async fn list_proposals(
        &self,
        filter: ProposalFilter,
    ) -> Result<Vec<DefinitionProposal>, ProposalStoreError> {
        self.storage
            .ensure_base_layout()
            .await
            .map_err(|err| ProposalStoreError::storage("ensure_layout", err))?;
        let files = self
            .storage
            .list_files_with_extension(self.storage.proposals_dir(), "json")
            .await
            .map_err(|err| ProposalStoreError::storage("list_proposals", err))?;

        let mut proposals = Vec::new();
        for file in files {
            match self.storage.read_json::<DefinitionProposal>(&file).await {
                Ok(proposal) => proposals.push(proposal),
                Err(err) => {
                    warn!(
                        path = %file.display(),
                        error = %err,
                        "Unreadable proposal file encountered; refusing partial proposal list"
                    );
                    return Err(ProposalStoreError::Storage {
                        operation: "list_proposals_read",
                        details: format!(
                            "failed to read proposal file `{}`: {}",
                            file.display(),
                            err
                        ),
                    });
                },
            }
        }

        proposals = proposals
            .into_iter()
            .filter(|proposal| {
                filter
                    .agent_id
                    .as_deref()
                    .map(|agent_id| proposal.agent_id == agent_id)
                    .unwrap_or(true)
            })
            .filter(|proposal| {
                filter
                    .status
                    .as_ref()
                    .map(|status| proposal.status == *status)
                    .unwrap_or(true)
            })
            .collect();

        proposals.sort_by(|lhs, rhs| {
            lhs.created_at
                .cmp(&rhs.created_at)
                .then_with(|| lhs.proposal_id.cmp(&rhs.proposal_id))
        });
        Ok(proposals)
    }

    pub async fn resolve_proposal(
        &self,
        proposal_id: &str,
        decision: ProposalDecision,
        resolved_by: &str,
    ) -> Result<(DefinitionProposal, ProposalResolveOutcome), ProposalStoreError> {
        let proposal_id = Self::validate_proposal_id(proposal_id)?;
        let resolved_by = resolved_by.trim();
        if resolved_by.is_empty() {
            return Err(ProposalStoreError::InvalidRequest(
                "resolved_by must not be empty".to_string(),
            ));
        }

        let next_status = decision.target_status();
        let _guard = self.write_gate.lock().await;
        self.storage
            .ensure_base_layout()
            .await
            .map_err(|err| ProposalStoreError::storage("ensure_layout", err))?;
        let mut proposal = self
            .read_existing_unlocked(&proposal_id)
            .await?
            .ok_or_else(|| ProposalStoreError::NotFound(proposal_id.clone()))?;

        if proposal.status == next_status {
            return Ok((proposal, ProposalResolveOutcome::Noop));
        }

        if !transition_allowed(&proposal.status, &next_status) {
            return Err(ProposalStoreError::StaleTransition {
                proposal_id: proposal.proposal_id.clone(),
                current_status: proposal.status,
                attempted_status: next_status,
            });
        }

        proposal.status = next_status;
        proposal.updated_at = Utc::now();
        proposal.resolved_by = Some(resolved_by.to_string());
        proposal.resolved_at = Some(proposal.updated_at);

        self.storage
            .write_json_atomic(self.proposal_path(&proposal.proposal_id), &proposal)
            .await
            .map_err(|err| ProposalStoreError::storage("resolve_proposal", err))?;
        Ok((proposal, ProposalResolveOutcome::Resolved))
    }

    pub async fn mark_applied(
        &self,
        proposal_id: &str,
        applied_version: u32,
    ) -> Result<DefinitionProposal, ProposalStoreError> {
        if applied_version == 0 {
            return Err(ProposalStoreError::InvalidRequest(
                "applied_version must be >= 1".to_string(),
            ));
        }
        let proposal_id = Self::validate_proposal_id(proposal_id)?;
        let _guard = self.write_gate.lock().await;
        self.storage
            .ensure_base_layout()
            .await
            .map_err(|err| ProposalStoreError::storage("ensure_layout", err))?;
        let mut proposal = self
            .read_existing_unlocked(&proposal_id)
            .await?
            .ok_or_else(|| ProposalStoreError::NotFound(proposal_id.clone()))?;

        if proposal.status != ProposalStatus::Approved {
            return Err(ProposalStoreError::InvalidRequest(format!(
                "proposal `{}` must be approved to mark as applied, current status: {:?}",
                proposal.proposal_id, proposal.status
            )));
        }

        // Idempotent: if already applied with the same version, return as-is.
        if let Some(existing) = proposal.applied_version {
            if existing != applied_version {
                return Err(ProposalStoreError::InvalidRequest(format!(
                    "proposal `{}` already applied at version {}, cannot re-apply at version {}",
                    proposal.proposal_id, existing, applied_version
                )));
            }
            return Ok(proposal);
        }

        let now = Utc::now();
        proposal.applied_version = Some(applied_version);
        proposal.applied_at = Some(now);
        proposal.updated_at = now;

        self.storage
            .write_json_atomic(self.proposal_path(&proposal.proposal_id), &proposal)
            .await
            .map_err(|err| ProposalStoreError::storage("mark_applied", err))?;
        Ok(proposal)
    }

    async fn read_existing_unlocked(
        &self,
        proposal_id: &str,
    ) -> Result<Option<DefinitionProposal>, ProposalStoreError> {
        let path = self.proposal_path(proposal_id);
        if !self
            .storage
            .exists(&path)
            .await
            .map_err(|err| ProposalStoreError::storage("proposal_exists", err))?
        {
            return Ok(None);
        }
        self.storage
            .read_json(path)
            .await
            .map(Some)
            .map_err(|err| ProposalStoreError::storage("read_proposal", err))
    }

    fn proposal_path(&self, proposal_id: &str) -> std::path::PathBuf {
        self.storage
            .proposals_dir()
            .join(format!("{proposal_id}.json"))
    }
}

fn transition_allowed(current: &ProposalStatus, next: &ProposalStatus) -> bool {
    matches!(
        (current, next),
        (ProposalStatus::Pending, ProposalStatus::Approved)
            | (ProposalStatus::Pending, ProposalStatus::Rejected)
            | (ProposalStatus::Pending, ProposalStatus::Deferred)
            | (ProposalStatus::Deferred, ProposalStatus::Approved)
            | (ProposalStatus::Deferred, ProposalStatus::Rejected)
    )
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn new_proposal(agent_id: &str, source: &str) -> NewDefinitionProposal {
        NewDefinitionProposal {
            agent_id: agent_id.to_string(),
            source: source.to_string(),
            payload: serde_json::json!({
                "reasoning": "Adjust workflow retries based on recent failures",
                "episode_refs": ["ep-1", "ep-2"]
            }),
            yaml_before: "agent_id: alpha\nversion: 1\n".to_string(),
            yaml_after: "agent_id: alpha\nversion: 2\n".to_string(),
        }
    }

    #[tokio::test]
    async fn proposal_create_list_get_roundtrip_persists_yaml_snapshots() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProposalStore::new(AgentStorage::new(tmp.path()));

        let created = store
            .create_proposal(new_proposal("agent-alpha", "meta_agent"))
            .await
            .unwrap();
        let loaded = store
            .get_proposal(&created.proposal_id)
            .await
            .unwrap()
            .expect("proposal should exist");
        let listed = store
            .list_proposals(ProposalFilter::default())
            .await
            .unwrap();

        assert_eq!(created.proposal_id, loaded.proposal_id);
        assert_eq!(loaded.yaml_before, "agent_id: alpha\nversion: 1\n");
        assert_eq!(loaded.yaml_after, "agent_id: alpha\nversion: 2\n");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].proposal_id, created.proposal_id);
        assert_eq!(listed[0].payload, created.payload);
    }

    #[tokio::test]
    async fn proposal_list_filters_are_deterministic_for_agent_and_status() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProposalStore::new(AgentStorage::new(tmp.path()));

        let alpha_a = store
            .create_proposal(new_proposal("agent-alpha", "meta_agent"))
            .await
            .unwrap();
        let alpha_b = store
            .create_proposal(new_proposal("agent-alpha", "operator"))
            .await
            .unwrap();
        let _beta = store
            .create_proposal(new_proposal("agent-beta", "meta_agent"))
            .await
            .unwrap();

        store
            .resolve_proposal(&alpha_a.proposal_id, ProposalDecision::Approve, "reviewer")
            .await
            .unwrap();
        store
            .resolve_proposal(&alpha_b.proposal_id, ProposalDecision::Defer, "reviewer")
            .await
            .unwrap();

        let alpha_only = store
            .list_proposals(ProposalFilter {
                agent_id: Some("agent-alpha".to_string()),
                status: None,
            })
            .await
            .unwrap();
        assert_eq!(alpha_only.len(), 2);
        assert!(alpha_only
            .iter()
            .all(|proposal| proposal.agent_id == "agent-alpha"));

        let approved_only = store
            .list_proposals(ProposalFilter {
                agent_id: Some("agent-alpha".to_string()),
                status: Some(ProposalStatus::Approved),
            })
            .await
            .unwrap();
        assert_eq!(approved_only.len(), 1);
        assert_eq!(approved_only[0].proposal_id, alpha_a.proposal_id);

        let deferred_only = store
            .list_proposals(ProposalFilter {
                agent_id: None,
                status: Some(ProposalStatus::Deferred),
            })
            .await
            .unwrap();
        assert_eq!(deferred_only.len(), 1);
        assert_eq!(deferred_only[0].proposal_id, alpha_b.proposal_id);
    }

    #[tokio::test]
    async fn proposal_invalid_transition_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProposalStore::new(AgentStorage::new(tmp.path()));
        let created = store
            .create_proposal(new_proposal("agent-alpha", "meta_agent"))
            .await
            .unwrap();

        store
            .resolve_proposal(&created.proposal_id, ProposalDecision::Approve, "reviewer")
            .await
            .unwrap();
        let err = store
            .resolve_proposal(&created.proposal_id, ProposalDecision::Reject, "reviewer")
            .await
            .unwrap_err();

        match err {
            ProposalStoreError::StaleTransition {
                current_status,
                attempted_status,
                ..
            } => {
                assert_eq!(current_status, ProposalStatus::Approved);
                assert_eq!(attempted_status, ProposalStatus::Rejected);
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn proposal_resolve_is_idempotent_for_duplicate_requests() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProposalStore::new(AgentStorage::new(tmp.path()));
        let created = store
            .create_proposal(new_proposal("agent-alpha", "meta_agent"))
            .await
            .unwrap();

        let (_, first) = store
            .resolve_proposal(&created.proposal_id, ProposalDecision::Approve, "reviewer")
            .await
            .unwrap();
        let (second_proposal, second) = store
            .resolve_proposal(&created.proposal_id, ProposalDecision::Approve, "reviewer")
            .await
            .unwrap();

        assert_eq!(first, ProposalResolveOutcome::Resolved);
        assert_eq!(second, ProposalResolveOutcome::Noop);
        assert_eq!(second_proposal.status, ProposalStatus::Approved);
    }

    #[tokio::test]
    async fn proposal_get_accepts_non_canonical_uuid_forms() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProposalStore::new(AgentStorage::new(tmp.path()));
        let created = store
            .create_proposal(new_proposal("agent-alpha", "meta_agent"))
            .await
            .unwrap();

        let uppercase = created.proposal_id.to_ascii_uppercase();
        let loaded = store
            .get_proposal(&uppercase)
            .await
            .unwrap()
            .expect("proposal should be retrievable via non-canonical UUID");
        assert_eq!(loaded.proposal_id, created.proposal_id);
    }

    #[tokio::test]
    async fn proposal_resolve_accepts_non_canonical_uuid_forms() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProposalStore::new(AgentStorage::new(tmp.path()));
        let created = store
            .create_proposal(new_proposal("agent-alpha", "meta_agent"))
            .await
            .unwrap();

        let uppercase = created.proposal_id.to_ascii_uppercase();
        let (resolved, outcome) = store
            .resolve_proposal(&uppercase, ProposalDecision::Approve, "reviewer")
            .await
            .unwrap();
        assert_eq!(outcome, ProposalResolveOutcome::Resolved);
        assert_eq!(resolved.status, ProposalStatus::Approved);
        assert_eq!(resolved.proposal_id, created.proposal_id);
    }

    #[tokio::test]
    async fn proposal_create_rejects_reserved_agent_identifier() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProposalStore::new(AgentStorage::new(tmp.path()));
        let err = store
            .create_proposal(new_proposal("proposals", "meta_agent"))
            .await
            .unwrap_err();
        assert!(matches!(err, ProposalStoreError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn proposal_create_rejects_non_object_payload() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProposalStore::new(AgentStorage::new(tmp.path()));
        let mut proposal = new_proposal("agent-alpha", "meta_agent");
        proposal.payload = serde_json::Value::Null;
        let err = store.create_proposal(proposal).await.unwrap_err();
        assert!(matches!(err, ProposalStoreError::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn proposal_list_fails_when_any_record_is_unreadable() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProposalStore::new(AgentStorage::new(tmp.path()));
        store
            .create_proposal(new_proposal("agent-alpha", "meta_agent"))
            .await
            .unwrap();

        let corrupt_path = store.storage().proposals_dir().join("corrupt.json");
        tokio::fs::write(&corrupt_path, b"{ invalid json")
            .await
            .unwrap();

        let err = store
            .list_proposals(ProposalFilter::default())
            .await
            .expect_err("corrupt proposal file should fail listing");
        match err {
            ProposalStoreError::Storage { operation, details } => {
                assert_eq!(operation, "list_proposals_read");
                assert!(details.contains("corrupt.json"));
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn mark_applied_sets_version_and_timestamp() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProposalStore::new(AgentStorage::new(tmp.path()));
        let created = store
            .create_proposal(new_proposal("agent-alpha", "meta_agent"))
            .await
            .unwrap();

        store
            .resolve_proposal(&created.proposal_id, ProposalDecision::Approve, "reviewer")
            .await
            .unwrap();

        let applied = store.mark_applied(&created.proposal_id, 2).await.unwrap();
        assert_eq!(applied.applied_version, Some(2));
        assert!(applied.applied_at.is_some());
    }

    #[tokio::test]
    async fn mark_applied_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProposalStore::new(AgentStorage::new(tmp.path()));
        let created = store
            .create_proposal(new_proposal("agent-alpha", "meta_agent"))
            .await
            .unwrap();

        store
            .resolve_proposal(&created.proposal_id, ProposalDecision::Approve, "reviewer")
            .await
            .unwrap();

        let first = store.mark_applied(&created.proposal_id, 2).await.unwrap();
        let second = store.mark_applied(&created.proposal_id, 2).await.unwrap();

        assert_eq!(first.applied_version, second.applied_version);
        assert_eq!(first.applied_at, second.applied_at);
    }

    #[tokio::test]
    async fn mark_applied_rejects_version_mismatch_on_reapply() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProposalStore::new(AgentStorage::new(tmp.path()));
        let created = store
            .create_proposal(new_proposal("agent-alpha", "meta_agent"))
            .await
            .unwrap();

        store
            .resolve_proposal(&created.proposal_id, ProposalDecision::Approve, "reviewer")
            .await
            .unwrap();

        store.mark_applied(&created.proposal_id, 2).await.unwrap();

        let err = store
            .mark_applied(&created.proposal_id, 5)
            .await
            .unwrap_err();
        match err {
            ProposalStoreError::InvalidRequest(msg) => {
                assert!(msg.contains("already applied at version 2"));
                assert!(msg.contains("cannot re-apply at version 5"));
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn mark_applied_rejects_unapproved_proposal() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProposalStore::new(AgentStorage::new(tmp.path()));
        let created = store
            .create_proposal(new_proposal("agent-alpha", "meta_agent"))
            .await
            .unwrap();

        let err = store
            .mark_applied(&created.proposal_id, 2)
            .await
            .unwrap_err();
        match err {
            ProposalStoreError::InvalidRequest(msg) => {
                assert!(msg.contains("must be approved"));
                assert!(msg.contains("Pending"));
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn mark_applied_rejects_version_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProposalStore::new(AgentStorage::new(tmp.path()));
        let created = store
            .create_proposal(new_proposal("agent-alpha", "meta_agent"))
            .await
            .unwrap();

        store
            .resolve_proposal(&created.proposal_id, ProposalDecision::Approve, "reviewer")
            .await
            .unwrap();

        let err = store
            .mark_applied(&created.proposal_id, 0)
            .await
            .unwrap_err();
        match err {
            ProposalStoreError::InvalidRequest(msg) => {
                assert!(msg.contains("must be >= 1"));
            },
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn applied_fields_survive_serialization_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let store = ProposalStore::new(AgentStorage::new(tmp.path()));
        let created = store
            .create_proposal(new_proposal("agent-alpha", "meta_agent"))
            .await
            .unwrap();

        store
            .resolve_proposal(&created.proposal_id, ProposalDecision::Approve, "reviewer")
            .await
            .unwrap();
        store.mark_applied(&created.proposal_id, 2).await.unwrap();

        let loaded = store
            .get_proposal(&created.proposal_id)
            .await
            .unwrap()
            .expect("proposal should exist");
        assert_eq!(loaded.applied_version, Some(2));
        assert!(loaded.applied_at.is_some());

        // Verify serde roundtrip preserves fields
        let json = serde_json::to_string(&loaded).unwrap();
        let deserialized: DefinitionProposal = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.applied_version, loaded.applied_version);
        assert_eq!(deserialized.applied_at, loaded.applied_at);

        // Verify #[serde(default)] handles legacy records without applied fields
        let mut legacy_json: serde_json::Value = serde_json::from_str(&json).unwrap();
        legacy_json
            .as_object_mut()
            .unwrap()
            .remove("applied_version");
        legacy_json.as_object_mut().unwrap().remove("applied_at");
        let legacy: DefinitionProposal = serde_json::from_value(legacy_json).unwrap();
        assert_eq!(legacy.applied_version, None);
        assert_eq!(legacy.applied_at, None);
    }
}
