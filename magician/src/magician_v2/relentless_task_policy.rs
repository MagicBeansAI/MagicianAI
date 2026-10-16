//! Explicit owner-approved task policy; the ordinary task scheduler remains
//! the launch owner. No model output can activate this standing authority.
use anyhow::{ensure, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::relentless::store::RunScope;
use super::relentless_runtime::{PreparedTextDraft, TextDraftProposal};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRelentlessPolicy {
    pub id: Uuid,
    pub proposal: TextDraftProposal,
    pub contract_hash: String,
    pub task_context_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_handoff: Option<super::approval_envelopes::campaign::CaptureHandoffTarget>,
    pub max_runs: u32,
    pub expires_at: DateTime<Utc>,
    pub approved_by: String,
    pub approved_at: DateTime<Utc>,
}

impl TaskRelentlessPolicy {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.id.is_nil() && self.max_runs > 0 && self.expires_at > self.approved_at,
            "task policy requires finite runs and expiry"
        );
        ensure!(
            !self.approved_by.trim().is_empty() && self.proposal.native_harness.is_some(),
            "task policy requires an owner and native actor"
        );
        self.maximum_cost_microusd()?;
        if let Some(target) = &self.capture_handoff {
            target.validate()?;
            ensure!(
                self.proposal.handoff_kind == super::relentless::routing::HandoffKind::Capture,
                "campaign handoff requires a capture contract"
            );
        }
        Ok(())
    }

    pub fn maximum_cost_microusd(&self) -> Result<u64> {
        self.proposal
            .budget
            .max_cost_microusd
            .checked_mul(u64::from(self.max_runs))
            .ok_or_else(|| anyhow::anyhow!("task policy aggregate budget overflow"))
    }

    pub fn hash(&self) -> Result<String> {
        Ok(blake3::hash(&serde_json::to_vec(self)?)
            .to_hex()
            .to_string())
    }
}

/// Protected immutable launch snapshot written before the ordinary root is
/// visible. Recovery uses this snapshot, rather than a subsequently edited flag.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskPolicyLaunch {
    pub scope: RunScope,
    pub task_id: String,
    pub execution_id: String,
    pub policy_id: Uuid,
    pub policy_hash: String,
    pub run_id: Uuid,
    pub prepared: PreparedTextDraft,
}

impl TaskPolicyLaunch {
    pub fn owner_id(&self) -> String {
        format!(
            "relentless:{}:{}",
            self.run_id,
            self.prepared.setup.admission_key()
        )
    }
}

pub(crate) fn run_id(scope: &RunScope, task_id: &str, execution_id: &str) -> Result<Uuid> {
    let hash = blake3::hash(&serde_json::to_vec(&(
        "relentless-task-policy-v1",
        scope,
        task_id,
        execution_id,
    ))?);
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    Ok(Uuid::from_bytes(bytes))
}
