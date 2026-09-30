//! Durable post-feedback current-universe rank diagnostics — worker side.
//!
//! The queue is content-free and keyed uniquely by canonical outcome id. A
//! worker loads the authoritative complete union only after leasing a job,
//! computes one deterministic diagnostic rank, and compare-and-sets the exact
//! universe/revision/generation binding at completion. It never rewrites the
//! historical rank observed when feedback occurred.
//!
//! Plan workstream 3.0: the durable job/result vocabulary moved lib-side to
//! `magician::magician_v2::attention::learning::rank_recompute`; this module
//! re-exports it and keeps the worker, which resolves jobs against the
//! canonical attention union projection over the comms stores.

use std::sync::Arc;

use anyhow::Result;
use futures_util::{stream, StreamExt};
use tracing::instrument;

use magician::config::AttentionRankRecomputeConfig;
use magician::magician_v2::analytics::runtime_activity_layer::{
    KIND_BACKGROUND, WORKLOAD_SCHEDULED,
};
use magician::magician_v2::transport_log::{SYSTEM_PRINCIPAL, SYSTEM_WORKSPACE};

pub use magician::magician_v2::attention::learning::rank_recompute::{
    rank_recompute_result_semantics, AttentionRankRecomputeEnqueueStatus,
    AttentionRankRecomputeGeneration, AttentionRankRecomputeJob, AttentionRankRecomputePauseReason,
    AttentionRankRecomputeQueueCounts, AttentionRankRecomputeReference,
    AttentionRankRecomputeResult, AttentionRankRecomputeRunReport, AttentionRankRecomputeStatus,
    ScheduleAttentionRankRecompute, ATTENTION_RANK_RECOMPUTE_REASON_MAX_CHARS,
    ATTENTION_RANK_RECOMPUTE_RESULT_SEMANTICS, ATTENTION_RANK_RECOMPUTE_SCHEMA_VERSION,
    ATTENTION_RANK_RECOMPUTE_SERVED_UNIVERSE_SEMANTICS,
    ATTENTION_RANK_RECOMPUTE_WRONGLY_STALED_REASON,
};

use crate::channel_assist::canonical_attention::{
    project_canonical_attention_union, CanonicalAttentionItem, CanonicalAttentionLane,
    CanonicalAttentionOrigin, CanonicalAttentionProjection,
};
use crate::channel_assist::channel::ChannelAssistStore;
use magician::magician_v2::attention::learning::{
    extract_bandit_features, AttentionBanditPosteriorState, AttentionDecisionItem,
    AttentionLearningService, AttentionLearningStore,
};
use magician::magician_v2::attention::resurfacing::store::ResurfacingStore;

#[derive(Clone)]
pub struct AttentionRankRecomputeWorker {
    learning: AttentionLearningService,
    store: AttentionLearningStore,
    mail: ChannelAssistStore,
    resurfacing: ResurfacingStore,
    config: AttentionRankRecomputeConfig,
    lease_owner: Arc<str>,
}

impl AttentionRankRecomputeWorker {
    pub fn new(
        learning: AttentionLearningService,
        mail: ChannelAssistStore,
        resurfacing: ResurfacingStore,
        config: AttentionRankRecomputeConfig,
        lease_owner: impl Into<String>,
    ) -> Self {
        let store = learning.store();
        Self {
            learning,
            store,
            mail,
            resurfacing,
            config,
            lease_owner: Arc::from(lease_owner.into()),
        }
    }

    pub fn config(&self) -> &AttentionRankRecomputeConfig {
        &self.config
    }

    pub fn pause_reason(&self) -> Option<AttentionRankRecomputePauseReason> {
        (!self.config.enabled).then_some(AttentionRankRecomputePauseReason::Disabled)
    }

    /// Spawn the durable worker across every persisted principal/workspace.
    /// Scope is taken from each leased job rather than being hard-coded at
    /// process startup.
    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            if !magician::magician_v2::runtime::startup::wait_for_http().await {
                return;
            }
            let interval = std::time::Duration::from_secs(self.config.interval_secs.max(10));
            loop {
                let now = chrono::Utc::now().timestamp_millis();
                match self.run_all_scopes_once(now).await {
                    Ok(report)
                        if report.paused.is_none()
                            && report.leased >= self.config.batch_size.max(1) as u64 =>
                    {
                        // A full batch means more ready work may remain. The
                        // interval is an idle poll, not a throughput throttle.
                        tokio::task::yield_now().await;
                    },
                    result => {
                        if let Err(error) = result {
                            tracing::warn!(error = %error, "attention rank recompute pass failed");
                        }
                        tokio::time::sleep(interval).await;
                    },
                }
            }
        })
    }

    pub async fn schedule_missing_once(
        &self,
        principal: &str,
        workspace: &str,
        now: i64,
        apply: bool,
    ) -> Result<AttentionRankRecomputeRunReport> {
        let scheduled = self
            .store
            .schedule_missing_rank_recompute_jobs(
                principal,
                workspace,
                self.config.batch_size,
                now,
                apply,
            )
            .await?;
        Ok(AttentionRankRecomputeRunReport {
            scheduled,
            ..Default::default()
        })
    }

    /// Requeue the jobs whose stale verdict came from the cross-provenance
    /// universe comparison, so the corrected guard can decide them properly.
    ///
    /// Reported as `scheduled` because that is what it is: work put back on the
    /// queue, for the ordinary worker to process on its normal cadence under
    /// the same leases and retry bounds. Nothing here recomputes a rank.
    pub async fn requeue_wrongly_staled_once(
        &self,
        principal: &str,
        workspace: &str,
        now: i64,
        apply: bool,
    ) -> Result<AttentionRankRecomputeRunReport> {
        let scheduled = self
            .store
            .requeue_wrongly_staled_rank_recompute_jobs(
                principal,
                workspace,
                self.config.batch_size,
                now,
                apply,
            )
            .await?;
        Ok(AttentionRankRecomputeRunReport {
            scheduled,
            ..Default::default()
        })
    }

    pub async fn run_once(
        &self,
        principal: &str,
        workspace: &str,
        now: i64,
    ) -> Result<AttentionRankRecomputeRunReport> {
        if let Some(paused) = self.pause_reason() {
            return Ok(AttentionRankRecomputeRunReport {
                paused: Some(paused),
                ..Default::default()
            });
        }
        let mut report = AttentionRankRecomputeRunReport::default();
        let concurrency = self.config.concurrency.clamp(1, 8);
        while report.leased < self.config.batch_size as u64 {
            let lease_now = chrono::Utc::now().timestamp_millis().max(now);
            let remaining = self
                .config
                .batch_size
                .saturating_sub(report.leased as usize);
            let wave_size = rank_recompute_lease_wave_size(remaining, concurrency);
            let work = self
                .store
                .lease_rank_recompute_jobs(
                    principal,
                    workspace,
                    &self.lease_owner,
                    lease_now,
                    self.lease_expiry(lease_now),
                    wave_size,
                )
                .await?;
            if work.is_empty() {
                break;
            }
            merge_run_report(&mut report, self.process_work(work).await?);
        }
        Ok(report)
    }

    /// One span per interval pass, not one per leased job: the pass is the unit
    /// an operator wants ("the attention worker ran, and did nothing"), and a
    /// per-job span would put a row on the wire for every recomputed rank.
    ///
    /// Declares `system`/`system` explicitly, because this pass really is
    /// runtime-wide: it leases across every scope, so naming any single
    /// principal would be a lie.
    ///
    /// Stated positively rather than left blank. Undeclared spans now fall
    /// back to the default scope (`anonymous`/`default`) — undeclared work
    /// belongs to whoever forgot to say, not to the runtime — so silence no
    /// longer means "system". A pass that is genuinely cross-scope has to
    /// say so, which is the distinction the old fallback erased.
    #[instrument(
        name = "attention_rank_recompute_pass",
        skip_all,
        fields(
            activity_kind = KIND_BACKGROUND,
            workload_class = WORKLOAD_SCHEDULED,
            principal = SYSTEM_PRINCIPAL,
            workspace = SYSTEM_WORKSPACE,
        )
    )]
    pub async fn run_all_scopes_once(&self, now: i64) -> Result<AttentionRankRecomputeRunReport> {
        if let Some(paused) = self.pause_reason() {
            return Ok(AttentionRankRecomputeRunReport {
                paused: Some(paused),
                ..Default::default()
            });
        }
        let mut scheduled = self
            .store
            .recover_legacy_rank_failures(self.config.batch_size, now)
            .await?;
        let scopes = self
            .store
            .rank_recompute_reconciliation_scopes(self.config.batch_size)
            .await?;
        for (principal, workspace) in scopes {
            let remaining = self.config.batch_size.saturating_sub(scheduled as usize);
            if remaining == 0 {
                break;
            }
            scheduled += self
                .store
                .schedule_missing_rank_recompute_jobs(&principal, &workspace, remaining, now, true)
                .await?;
        }
        let mut report = AttentionRankRecomputeRunReport::default();
        let concurrency = self.config.concurrency.clamp(1, 8);
        while report.leased < self.config.batch_size as u64 {
            let lease_now = chrono::Utc::now().timestamp_millis().max(now);
            let remaining = self
                .config
                .batch_size
                .saturating_sub(report.leased as usize);
            let wave_size = rank_recompute_lease_wave_size(remaining, concurrency);
            let work = self
                .store
                .lease_rank_recompute_jobs_globally(
                    &self.lease_owner,
                    lease_now,
                    self.lease_expiry(lease_now),
                    wave_size,
                )
                .await?;
            if work.is_empty() {
                break;
            }
            merge_run_report(&mut report, self.process_work(work).await?);
        }
        report.scheduled = scheduled;
        Ok(report)
    }

    fn lease_expiry(&self, now: i64) -> i64 {
        now.saturating_add((self.config.lease_secs as i64) * 1_000)
    }

    async fn process_work(
        &self,
        work: Vec<AttentionRankRecomputeJob>,
    ) -> Result<AttentionRankRecomputeRunReport> {
        let mut report = AttentionRankRecomputeRunReport {
            leased: work.len() as u64,
            ..Default::default()
        };
        let results = stream::iter(
            work.into_iter()
                .map(|job| async move { self.process_with_lease_heartbeat(job).await }),
        )
        .buffer_unordered(self.config.concurrency.clamp(1, 8))
        .collect::<Vec<_>>()
        .await;
        for result in results {
            match result? {
                AttentionRankRecomputeStatus::Succeeded => report.succeeded += 1,
                AttentionRankRecomputeStatus::Stale => report.stale += 1,
                AttentionRankRecomputeStatus::Retry => report.retry += 1,
                AttentionRankRecomputeStatus::Dead => report.dead += 1,
                AttentionRankRecomputeStatus::Pending | AttentionRankRecomputeStatus::InFlight => {
                },
            }
        }
        Ok(report)
    }

    async fn process_with_lease_heartbeat(
        &self,
        job: AttentionRankRecomputeJob,
    ) -> Result<AttentionRankRecomputeStatus> {
        let heartbeat_ms = ((self.config.lease_secs as u64).saturating_mul(1_000) / 3).max(250);
        let mut heartbeat = tokio::time::interval(std::time::Duration::from_millis(heartbeat_ms));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Tokio intervals tick immediately. Consume that tick so a freshly
        // leased job does not contend with its own first source read.
        heartbeat.tick().await;

        let process = self.process(job.clone());
        tokio::pin!(process);
        loop {
            tokio::select! {
                biased;
                // If completion and a heartbeat become ready together, accept
                // the terminal result first. The terminal write may already
                // have cleared the lease, in which case renewing first would
                // falsely report a lost lease for work that actually landed.
                result = &mut process => return result,
                _ = heartbeat.tick() => {
                    let now = chrono::Utc::now().timestamp_millis();
                    let renewed = self.store
                        .renew_rank_recompute_job_lease(
                            &job.job_id,
                            &self.lease_owner,
                            now,
                            self.lease_expiry(now),
                        )
                        .await?;
                    anyhow::ensure!(renewed, "rank recompute heartbeat lost lease");
                }
            }
        }
    }

    async fn process(
        &self,
        mut job: AttentionRankRecomputeJob,
    ) -> Result<AttentionRankRecomputeStatus> {
        if job.enqueue_policy_snapshot_id.as_deref() != self.learning.bandit_snapshot_id() {
            return self.stale(&job, "snapshot_incompatible").await;
        }
        if job
            .decision_id
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        {
            match self
                .store
                .reconstruct_and_bind_rank_recompute_decision(&job)
                .await
            {
                Ok(Some(decision_id)) => job.decision_id = Some(decision_id),
                Ok(None) => return self.stale(&job, "no_served_decision").await,
                Err(_) => return self.retry(&job, "served_decision_lookup_failed").await,
            }
        }
        let generation_before = match self
            .learning
            .current_rank_recompute_generation(&job.principal, &job.workspace)
            .await
        {
            Ok(generation) => generation,
            Err(_) => return self.retry(&job, "generation_read_failed").await,
        };
        // Resolve against the projection this outcome was SERVED from, not a
        // freshly computed one. The job exists because the owner acted, and
        // acting removes the candidate from the live projection — so a recompute
        // can never contain it, which is why every job went stale. The decision
        // records the universe it served and projections are retained under that
        // digest, so the state the owner saw is still recoverable.
        let served_projection = match job.decision_id.as_deref() {
            Some(decision_id) if !decision_id.trim().is_empty() => {
                match self
                    .learning
                    .get_decision_projection_json(&job.principal, &job.workspace, decision_id)
                    .await
                {
                    Ok(Some(json)) => {
                        match serde_json::from_str::<CanonicalAttentionProjection>(&json) {
                            Ok(projection) => Some(projection),
                            // A stored projection that no longer parses is a
                            // contract change, not a transient fault; retrying it
                            // forever would never succeed.
                            Err(_) => {
                                return self.stale(&job, "served_projection_unreadable").await;
                            },
                        }
                    },
                    Ok(None) => None,
                    Err(_) => return self.retry(&job, "served_projection_read_failed").await,
                }
            },
            // Reconstruct above already tried the serving ledger. Remaining
            // blanks are genuinely unlinkable, distinct from `candidate_inactive`.
            _ => return self.stale(&job, "no_served_decision").await,
        };
        // Whether the universe under evaluation is a historical record or a
        // live read decides which commit-time guards can mean anything at all.
        let (projection, projection_is_live) = match served_projection {
            Some(projection) => (projection, false),
            None => match project_canonical_attention_union(
                &self.mail,
                &self.resurfacing,
                &self.learning,
                &job.principal,
                &job.workspace,
                None,
            )
            .await
            {
                Ok(projection)
                    if projection.integrity.load_complete && projection.integrity.exact_once =>
                {
                    (projection, true)
                },
                Ok(_) | Err(_) => return self.retry(&job, "canonical_projection_failed").await,
            },
        };
        let mut located = None;
        for (lane, items) in [
            (
                CanonicalAttentionLane::FollowUp,
                &projection.lanes.follow_up,
            ),
            (
                CanonicalAttentionLane::WorthALook,
                &projection.lanes.worth_a_look,
            ),
            (
                CanonicalAttentionLane::NonSurfaced,
                &projection.lanes.non_surfaced,
            ),
        ] {
            if let Some((index, item)) = items
                .iter()
                .enumerate()
                .find(|(_, item)| item_matches_rank_job(item, &job))
            {
                located = Some((lane, index + 1, item));
                break;
            }
        }
        let Some((_lane, _lane_position, item)) = located else {
            return self.stale(&job, "candidate_inactive").await;
        };
        if item.source_revision != job.source_revision {
            return self.stale(&job, "source_revision_changed").await;
        }

        let detail = match self
            .learning
            .get_routing_decision(&job.principal, &job.workspace, &projection.projection_id)
            .await
        {
            Ok(Some(detail)) => detail,
            Ok(None) | Err(_) => return self.retry(&job, "routing_decision_unavailable").await,
        };
        let Some(target_decision_item) = detail.items.iter().find(|candidate| {
            decision_item_matches_rank_job(candidate, &job) && candidate.hard_eligible
        }) else {
            // The source is proven active by the complete canonical projection,
            // so an absent eligible ranking row is operationally retryable.
            return self.retry(&job, "rank_target_missing").await;
        };

        // The diagnostic is independent of which baseline/canary order was
        // served. Without a bandit snapshot, the canonical learned rank is the
        // current model rank over the complete eligible union.
        let mut rank_after = target_decision_item.learned_rank;
        if rank_after == 0 {
            return self.retry(&job, "learned_rank_unavailable").await;
        }
        let mut result_snapshot_id = None;
        let mut result_posterior_version = None;
        if let Some(snapshot_id) = job.enqueue_policy_snapshot_id.as_deref() {
            let snapshot = match self.store.get_bandit_policy_snapshot(snapshot_id).await {
                Ok(Some(snapshot)) => snapshot,
                Ok(None) => return self.stale(&job, "snapshot_incompatible").await,
                Err(_) => return self.retry(&job, "posterior_read_failed").await,
            };
            let surface = job.origin_surface;
            let posterior = match self
                .store
                .get_bandit_posterior(&job.principal, &job.workspace, surface, &snapshot)
                .await
            {
                Ok(posterior) => posterior,
                Err(_) => return self.retry(&job, "posterior_read_failed").await,
            };
            if job
                .enqueue_posterior_version
                .is_some_and(|version| posterior.version < version)
            {
                return self.stale(&job, "snapshot_incompatible").await;
            }
            let active_items = detail
                .items
                .iter()
                .filter(|item| item.hard_eligible)
                .collect::<Vec<_>>();
            let mut scored = Vec::with_capacity(active_items.len());
            for item in &active_items {
                let features =
                    match extract_bandit_features(&snapshot, item, &detail.decision.context) {
                        Ok(features) => features,
                        Err(_) => return self.stale(&job, "snapshot_incompatible").await,
                    };
                let score = match posterior_mean_score(&posterior, &features) {
                    Ok(score) => score,
                    Err(_) => return self.stale(&job, "snapshot_incompatible").await,
                };
                scored.push((item.candidate_id.clone(), score));
            }
            scored.sort_by(|(left_id, left_score), (right_id, right_score)| {
                right_score
                    .total_cmp(left_score)
                    .then_with(|| left_id.cmp(right_id))
            });
            let Some(position) = scored
                .iter()
                .position(|(id, _)| id == &job.canonical_candidate_id)
            else {
                return self.retry(&job, "posterior_rank_target_missing").await;
            };
            rank_after = position + 1;
            result_snapshot_id = Some(snapshot.snapshot_id);
            result_posterior_version = Some(posterior.version);
        }
        // Re-reading the live union and demanding it still match is an
        // optimistic-concurrency check on the read, so it only carries meaning
        // when the projection under evaluation was itself read live. A served
        // projection is a historical record of the universe the owner was
        // shown, and the very act that enqueued this job has since removed the
        // candidate from the live union — so against a served projection these
        // three guards reject every attributed job by construction. That is the
        // same mutual exclusion between trigger and precondition that held the
        // queue at a zero success rate before the served projection was used,
        // reintroduced one guard further down the same path.
        if projection_is_live {
            let commit_projection = match project_canonical_attention_union(
                &self.mail,
                &self.resurfacing,
                &self.learning,
                &job.principal,
                &job.workspace,
                None,
            )
            .await
            {
                Ok(projection)
                    if projection.integrity.load_complete && projection.integrity.exact_once =>
                {
                    projection
                },
                Ok(_) | Err(_) => return self.retry(&job, "canonical_projection_failed").await,
            };
            if commit_projection.universe_digest != projection.universe_digest {
                return self.stale(&job, "universe_changed_during_commit").await;
            }
            let commit_item = commit_projection
                .lanes
                .follow_up
                .iter()
                .chain(commit_projection.lanes.worth_a_look.iter())
                .chain(commit_projection.lanes.non_surfaced.iter())
                .find(|candidate| candidate.canonical_id == job.canonical_candidate_id);
            if commit_item.is_none() {
                return self.stale(&job, "candidate_inactive").await;
            }
            if commit_item.and_then(|candidate| candidate.source_revision.as_ref())
                != job.source_revision.as_ref()
            {
                return self.stale(&job, "source_revision_changed").await;
            }
        }
        let generation_after = match self
            .learning
            .current_rank_recompute_generation(&job.principal, &job.workspace)
            .await
        {
            Ok(generation) => generation,
            Err(_) => return self.retry(&job, "generation_read_failed").await,
        };
        if generation_after != generation_before {
            return self.stale(&job, "generation_changed_during_commit").await;
        }
        if job.enqueue_policy_snapshot_id.as_deref() != self.learning.bandit_snapshot_id() {
            return self.stale(&job, "snapshot_incompatible").await;
        }
        if let (Some(snapshot_id), Some(expected_version)) =
            (result_snapshot_id.as_deref(), result_posterior_version)
        {
            let snapshot = match self.store.get_bandit_policy_snapshot(snapshot_id).await {
                Ok(Some(snapshot)) => snapshot,
                Ok(None) => return self.stale(&job, "snapshot_incompatible").await,
                Err(_) => return self.retry(&job, "posterior_read_failed").await,
            };
            let posterior = match self
                .store
                .get_bandit_posterior(
                    &job.principal,
                    &job.workspace,
                    job.origin_surface,
                    &snapshot,
                )
                .await
            {
                Ok(posterior) => posterior,
                Err(_) => return self.retry(&job, "posterior_read_failed").await,
            };
            if posterior.version != expected_version {
                return self.stale(&job, "generation_changed_during_commit").await;
            }
        }
        let completed_at = chrono::Utc::now().timestamp_millis();
        let result = AttentionRankRecomputeResult {
            // Say which universe the numbers below belong to. `universe_digest`
            // and `affected_rank_after` come from `projection`, which is the
            // served one unless it had to be recomputed.
            semantics: if projection_is_live {
                ATTENTION_RANK_RECOMPUTE_RESULT_SEMANTICS.to_string()
            } else {
                ATTENTION_RANK_RECOMPUTE_SERVED_UNIVERSE_SEMANTICS.to_string()
            },
            affected_rank_after: rank_after,
            affected_rank_delta: job
                .affected_rank_before
                .map(|before| rank_after as i64 - before as i64),
            current_source_revision: item.source_revision.clone(),
            universe_digest: projection.universe_digest,
            recompute_generation: generation_after,
            policy_snapshot_id: result_snapshot_id,
            posterior_version: result_posterior_version,
            completed_at,
        };
        self.store
            .finish_rank_recompute_job(&job.job_id, &self.lease_owner, &result, completed_at)
            .await?;
        Ok(AttentionRankRecomputeStatus::Succeeded)
    }

    async fn stale(
        &self,
        job: &AttentionRankRecomputeJob,
        reason: &str,
    ) -> Result<AttentionRankRecomputeStatus> {
        self.store
            .stale_rank_recompute_job(
                &job.job_id,
                &self.lease_owner,
                reason,
                chrono::Utc::now().timestamp_millis(),
            )
            .await?;
        Ok(AttentionRankRecomputeStatus::Stale)
    }

    async fn retry(
        &self,
        job: &AttentionRankRecomputeJob,
        reason: &str,
    ) -> Result<AttentionRankRecomputeStatus> {
        let exhausted = job.attempts >= self.config.max_retries;
        let now = chrono::Utc::now().timestamp_millis();
        let next_retry_at = if exhausted {
            None
        } else {
            let exponent = job.attempts.saturating_sub(1).min(20);
            let delay = self
                .config
                .retry_base_secs
                .saturating_mul(1u64 << exponent)
                .min(self.config.retry_max_secs);
            Some(now.saturating_add((delay as i64) * 1_000))
        };
        self.store
            .retry_rank_recompute_job(&job.job_id, &self.lease_owner, next_retry_at, reason, now)
            .await?;
        Ok(if exhausted {
            AttentionRankRecomputeStatus::Dead
        } else {
            AttentionRankRecomputeStatus::Retry
        })
    }
}

fn decision_item_matches_rank_job(
    item: &AttentionDecisionItem,
    job: &AttentionRankRecomputeJob,
) -> bool {
    item.candidate_id == job.canonical_candidate_id
        || item.candidate_id == job.raw_candidate_id
        || item.candidate_id == format!("follow_up:{}", job.raw_candidate_id)
        || item.candidate_id == format!("worth_a_look:{}", job.raw_candidate_id)
}

fn item_matches_rank_job(item: &CanonicalAttentionItem, job: &AttentionRankRecomputeJob) -> bool {
    if item.canonical_id == job.canonical_candidate_id {
        return true;
    }
    let raw = match &item.origin {
        CanonicalAttentionOrigin::FollowUp { annotation_id, .. } => annotation_id.as_str(),
        CanonicalAttentionOrigin::WorthALook { candidate_id, .. } => candidate_id.as_str(),
    };
    raw == job.raw_candidate_id
        || raw == job.canonical_candidate_id
        || item.canonical_id == job.raw_candidate_id
        || item.canonical_id == format!("follow_up:{}", job.raw_candidate_id)
        || item.canonical_id == format!("worth_a_look:{}", job.raw_candidate_id)
}

fn merge_run_report(
    aggregate: &mut AttentionRankRecomputeRunReport,
    completed: AttentionRankRecomputeRunReport,
) {
    aggregate.leased = aggregate.leased.saturating_add(completed.leased);
    aggregate.succeeded = aggregate.succeeded.saturating_add(completed.succeeded);
    aggregate.stale = aggregate.stale.saturating_add(completed.stale);
    aggregate.retry = aggregate.retry.saturating_add(completed.retry);
    aggregate.dead = aggregate.dead.saturating_add(completed.dead);
}

fn rank_recompute_lease_wave_size(remaining: usize, concurrency: usize) -> usize {
    remaining.min(concurrency.clamp(1, 8))
}

fn posterior_mean_score(
    posterior: &AttentionBanditPosteriorState,
    features: &[f64],
) -> Result<f64> {
    anyhow::ensure!(
        posterior.mean.len() == features.len()
            && posterior.mean.iter().all(|value| value.is_finite())
            && features.iter().all(|value| value.is_finite()),
        "posterior diagnostic dimensions are incompatible"
    );
    let score = posterior
        .mean
        .iter()
        .zip(features)
        .map(|(weight, value)| weight * value)
        .sum::<f64>();
    anyhow::ensure!(
        score.is_finite(),
        "posterior diagnostic score is non-finite"
    );
    Ok(score)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::rank_recompute_lease_wave_size;

    #[test]
    fn lease_wave_never_parks_more_jobs_than_worker_capacity() {
        assert_eq!(rank_recompute_lease_wave_size(100, 3), 3);
        assert_eq!(rank_recompute_lease_wave_size(2, 3), 2);
        assert_eq!(rank_recompute_lease_wave_size(100, 0), 1);
        assert_eq!(rank_recompute_lease_wave_size(100, 99), 8);
        assert_eq!(rank_recompute_lease_wave_size(0, 4), 0);
    }
}
