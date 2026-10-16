//! Durable post-feedback current-universe rank diagnostics.
//!
//! The queue is content-free and keyed uniquely by canonical outcome id. A
//! worker loads the authoritative complete union only after leasing a job,
//! computes one deterministic diagnostic rank, and compare-and-sets the exact
//! universe/revision/generation binding at completion. It never rewrites the
//! historical rank observed when feedback occurred.
//!
//! This module holds the durable job/result vocabulary shared by the
//! attention-learning store. The worker itself stayed in `magician-comms`'s
//! `channel_assist::attention_learning::rank_recompute` module: it resolves
//! jobs against the canonical attention union projection, which reads the
//! comms stores (plan workstream 3.0).

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::{AttentionOutcomeKind, AttentionSurface};

pub const ATTENTION_RANK_RECOMPUTE_SCHEMA_VERSION: u32 = 1;
/// The after-rank was resolved against a freshly computed union — the universe
/// as it stands now.
pub const ATTENTION_RANK_RECOMPUTE_RESULT_SEMANTICS: &str = "current_universe_diagnostic";
/// The after-rank was resolved against the projection the decision was *served*
/// from, so it is bound to the universe the owner was shown rather than the
/// current one.
///
/// Both values existed implicitly the moment a job began resolving against a
/// served projection; only the label did not. A result that says `current` while
/// carrying a historical `universe_digest` is a plausible, unfalsifiable lie to
/// whatever trains on it, so the two are named apart.
pub const ATTENTION_RANK_RECOMPUTE_SERVED_UNIVERSE_SEMANTICS: &str = "served_universe_diagnostic";

/// The semantics a job's eventual result will carry, from the one thing known
/// at enqueue: whether the outcome recorded the decision that served it.
///
/// A promise rather than a guarantee. A job whose decision is recorded but
/// whose stored projection has gone falls back to a recomputed one and will
/// complete as `current`, disagreeing with the receipt that preceded it.
/// Retention keeps a projection alive for as long as a job references it, so
/// this needs the projection to be missing for some other reason, and nothing
/// compares the two values — but they are not the same claim, and a consumer
/// should read the completed result rather than trust the receipt.
pub fn rank_recompute_result_semantics(decision_id: Option<&str>) -> &'static str {
    match decision_id {
        Some(decision_id) if !decision_id.trim().is_empty() => {
            ATTENTION_RANK_RECOMPUTE_SERVED_UNIVERSE_SEMANTICS
        },
        _ => ATTENTION_RANK_RECOMPUTE_RESULT_SEMANTICS,
    }
}
pub const ATTENTION_RANK_RECOMPUTE_REASON_MAX_CHARS: usize = 120;
/// The one stale reason a job can carry without that verdict saying anything
/// about the job: it was decided by comparing a served projection's historical
/// universe against the live one. Jobs holding it are the requeue cohort.
pub const ATTENTION_RANK_RECOMPUTE_WRONGLY_STALED_REASON: &str = "universe_changed_during_commit";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttentionRankRecomputeStatus {
    Pending,
    InFlight,
    Retry,
    Succeeded,
    Stale,
    Dead,
}

impl AttentionRankRecomputeStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InFlight => "in_flight",
            Self::Retry => "retry",
            Self::Succeeded => "succeeded",
            Self::Stale => "stale",
            Self::Dead => "dead",
        }
    }
}

impl FromStr for AttentionRankRecomputeStatus {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> anyhow::Result<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "in_flight" => Ok(Self::InFlight),
            "retry" => Ok(Self::Retry),
            "succeeded" => Ok(Self::Succeeded),
            "stale" => Ok(Self::Stale),
            "dead" => Ok(Self::Dead),
            other => anyhow::bail!("unknown attention rank recompute status: {other}"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttentionRankRecomputeEnqueueStatus {
    Enqueued,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttentionRankRecomputeReference {
    pub enqueue_status: AttentionRankRecomputeEnqueueStatus,
    pub job_id: Option<String>,
    pub job_status: Option<AttentionRankRecomputeStatus>,
    pub status_href: Option<String>,
    pub affected_rank_before: Option<usize>,
    pub affected_rank_after: Option<usize>,
    pub affected_rank_delta: Option<i64>,
    pub result_semantics: String,
    pub reason: Option<String>,
}

impl AttentionRankRecomputeReference {
    pub fn enqueued(job: &AttentionRankRecomputeJob) -> Self {
        Self {
            enqueue_status: AttentionRankRecomputeEnqueueStatus::Enqueued,
            job_id: Some(job.job_id.clone()),
            job_status: Some(job.status),
            status_href: Some(format!(
                "/api/magician/v2/channel-assist/attention-learning/rank-recompute/jobs/{}",
                job.job_id
            )),
            affected_rank_before: job.affected_rank_before,
            affected_rank_after: None,
            affected_rank_delta: None,
            result_semantics: rank_recompute_result_semantics(job.decision_id.as_deref())
                .to_string(),
            reason: None,
        }
    }

    /// No job was enqueued, so no result will ever carry these semantics. The
    /// field is required by the wire and describes nothing here; it keeps the
    /// value it has always had rather than inventing a provenance for work that
    /// will not run.
    pub fn failed(affected_rank_before: Option<usize>) -> Self {
        Self {
            enqueue_status: AttentionRankRecomputeEnqueueStatus::Failed,
            job_id: None,
            job_status: None,
            status_href: None,
            affected_rank_before,
            affected_rank_after: None,
            affected_rank_delta: None,
            result_semantics: ATTENTION_RANK_RECOMPUTE_RESULT_SEMANTICS.to_string(),
            reason: Some("enqueue_failed".to_string()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScheduleAttentionRankRecompute {
    pub outcome_id: String,
    pub origin_surface: AttentionSurface,
    pub canonical_candidate_id: String,
    pub raw_candidate_id: String,
    pub source_revision: Option<String>,
    pub outcome: AttentionOutcomeKind,
    pub decision_id: Option<String>,
    pub delivery_id: Option<String>,
    pub impression_id: Option<String>,
    pub affected_rank_before: Option<usize>,
    pub enqueue_policy_snapshot_id: Option<String>,
    pub enqueue_posterior_version: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttentionRankRecomputeResult {
    pub semantics: String,
    pub affected_rank_after: usize,
    pub affected_rank_delta: Option<i64>,
    pub current_source_revision: Option<String>,
    pub universe_digest: String,
    pub recompute_generation: AttentionRankRecomputeGeneration,
    pub policy_snapshot_id: Option<String>,
    pub posterior_version: Option<u64>,
    pub completed_at: i64,
}

/// Exact cross-origin generation binding used by the current-universe CAS.
/// A summed scalar is insufficient because offsetting origin changes can
/// produce the same sum.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttentionRankRecomputeGeneration {
    pub follow_up: u64,
    pub worth_a_look: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttentionRankRecomputeJob {
    pub job_id: String,
    pub outcome_id: String,
    pub status: AttentionRankRecomputeStatus,
    pub origin_surface: AttentionSurface,
    pub canonical_candidate_id: String,
    pub raw_candidate_id: String,
    pub source_revision: Option<String>,
    pub outcome: AttentionOutcomeKind,
    pub decision_id: Option<String>,
    pub delivery_id: Option<String>,
    pub impression_id: Option<String>,
    pub affected_rank_before: Option<usize>,
    pub enqueue_policy_snapshot_id: Option<String>,
    pub enqueue_posterior_version: Option<u64>,
    pub attempts: u32,
    pub next_retry_at: Option<i64>,
    pub lease_expires_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    pub completed_at: Option<i64>,
    pub reason: Option<String>,
    pub result: Option<AttentionRankRecomputeResult>,
    #[serde(skip)]
    pub principal: String,
    #[serde(skip)]
    pub workspace: String,
    #[serde(skip)]
    pub lease_owner: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttentionRankRecomputeQueueCounts {
    pub pending: u64,
    pub in_flight: u64,
    pub retry: u64,
    pub succeeded: u64,
    pub stale: u64,
    pub dead: u64,
    pub next_retry_at: Option<i64>,
    pub oldest_pending_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AttentionRankRecomputePauseReason {
    #[serde(rename = "rank_recompute_disabled")]
    Disabled,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttentionRankRecomputeRunReport {
    pub scheduled: u64,
    pub leased: u64,
    pub succeeded: u64,
    pub stale: u64,
    pub retry: u64,
    pub dead: u64,
    pub paused: Option<AttentionRankRecomputePauseReason>,
}
