//! Bounded asynchronous semantic extraction for active attention candidates.
//!
//! The durable work store contains identities and revisions only. This worker
//! fetches a safe brief from the source store after leasing work, makes one
//! versioned classifier call, validates the shared Slice-2 envelope, and uses a
//! semantics-only compare-and-set. It never reads a raw body or rendered card.

use std::{collections::BTreeMap, sync::Arc};

use anyhow::Result;
use async_trait::async_trait;
use futures_util::{stream, StreamExt};
use serde::{Deserialize, Serialize};

use crate::channel_assist::channel::ChannelAssistStore;
use magician::config::AttentionSemanticBackfillConfig;
use magician::magician_v2::attention::learning::{
    deserialize_semantic_envelope, serialize_semantic_envelope, AttentionLearningStore,
    AttentionSurface, ChannelAttentionSemanticEnvelope, ScheduleSemanticExtraction,
    SemanticExtractionContract, SemanticExtractionStatus, SemanticExtractionWorkItem,
    SemanticExtractionWorkStatus, SemanticExtractorIdentity, ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT,
    ATTENTION_SEMANTIC_SCHEMA_VERSION,
};
use magician::magician_v2::attention::resurfacing::store::ResurfacingStore;
use magician::magician_v2::attention::resurfacing::types::{CandidateState, SourceKind};

#[derive(Debug, Clone)]
pub struct SemanticExtractionInput {
    pub surface: AttentionSurface,
    pub candidate_id: String,
    pub source_revision: String,
    pub source_revision_number: i64,
    /// Locally distilled summary only. Never a raw message body or rendered
    /// card string.
    pub safe_summary: Option<String>,
    /// Structured local brief/source details only.
    pub safe_brief: Option<serde_json::Value>,
    /// Bounded source metadata whose keys are prompt evidence references.
    pub source_metadata: BTreeMap<String, String>,
}

enum SemanticSourceInput {
    Ready(SemanticExtractionInput),
    AlreadyCovered,
    Unavailable,
}

#[async_trait]
pub trait SemanticFeatureExtractor: Send + Sync {
    /// False when the managed operation is not bound or its model is currently
    /// unavailable. The worker pauses rather than falling through to a default.
    fn available(&self) -> bool;
    fn identity(&self) -> SemanticExtractorIdentity;
    fn prompt_version(&self) -> &'static str;
    /// Authoritative runtime queue pressure. Implementations that are not
    /// backed by the production dispatch queue remain unpressured by default.
    fn foreground_pressure(&self) -> bool {
        false
    }
    async fn extract(
        &self,
        input: &SemanticExtractionInput,
    ) -> Result<ChannelAttentionSemanticEnvelope>;
}

#[derive(Debug, Clone, Default)]
pub struct UnavailableSemanticFeatureExtractor;

#[async_trait]
impl SemanticFeatureExtractor for UnavailableSemanticFeatureExtractor {
    fn available(&self) -> bool {
        false
    }

    fn identity(&self) -> SemanticExtractorIdentity {
        SemanticExtractorIdentity::default()
    }

    fn prompt_version(&self) -> &'static str {
        magician::magician_v2::prompts::versions::CHANNEL_CLASSIFY
    }

    async fn extract(
        &self,
        _input: &SemanticExtractionInput,
    ) -> Result<ChannelAttentionSemanticEnvelope> {
        anyhow::bail!("channel_classify operation is unavailable")
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticExtractionPauseReason {
    Disabled,
    ModelUnavailable,
    ForegroundPressure,
}

impl SemanticExtractionPauseReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::ModelUnavailable => "model_unavailable",
            Self::ForegroundPressure => "foreground_pressure",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticExtractionRunReport {
    pub discovered: u64,
    pub leased: u64,
    pub succeeded: u64,
    pub missing: u64,
    pub invalid: u64,
    pub stale: u64,
    pub retry: u64,
    pub dead: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused: Option<SemanticExtractionPauseReason>,
}

#[derive(Clone)]
pub struct SemanticExtractionWorker {
    learning: AttentionLearningStore,
    mail: ChannelAssistStore,
    resurfacing: ResurfacingStore,
    extractor: Arc<dyn SemanticFeatureExtractor>,
    config: AttentionSemanticBackfillConfig,
    lease_owner: String,
}

impl SemanticExtractionWorker {
    pub fn new(
        learning: AttentionLearningStore,
        mail: ChannelAssistStore,
        resurfacing: ResurfacingStore,
        extractor: Arc<dyn SemanticFeatureExtractor>,
        config: AttentionSemanticBackfillConfig,
        lease_owner: impl Into<String>,
    ) -> Self {
        Self {
            learning,
            mail,
            resurfacing,
            extractor,
            config,
            lease_owner: lease_owner.into(),
        }
    }

    /// Spawn the finite pass on the configured interval for one explicit
    /// scope. Disabled configurations are intentionally not spawned by the
    /// runtime; callers may still retain the worker for health/preview APIs.
    ///
    /// The interval is the period between pass starts, not idle appended after
    /// one. A pass already caps itself at `calls_per_minute` model calls, so
    /// measuring the period from the start of the pass is what makes that cap
    /// mean "per interval" instead of "per interval plus however long local
    /// inference happened to take".
    pub fn spawn(
        self,
        principal: impl Into<String>,
        workspace: impl Into<String>,
    ) -> tokio::task::JoinHandle<()> {
        let principal = principal.into();
        let workspace = workspace.into();
        tokio::spawn(async move {
            if !magician::magician_v2::runtime::startup::wait_for_http().await {
                return;
            }
            let interval = std::time::Duration::from_secs(self.config.interval_secs.max(60));
            loop {
                let started = tokio::time::Instant::now();
                let now = chrono::Utc::now().timestamp_millis();
                if let Err(error) = self.run_once(&principal, &workspace, now).await {
                    tracing::warn!(error = %error, "semantic extraction background pass failed");
                }
                // A pass that overran its period has already spent that budget
                // window on real calls, so the next one starts immediately
                // rather than compounding the overrun with a full extra idle
                // interval. A paused pass returns without calling a model, so
                // it still waits the whole period instead of spinning.
                if let Some(remaining) = interval.checked_sub(started.elapsed()) {
                    tokio::time::sleep(remaining).await;
                }
            }
        })
    }

    pub fn pause_reason(&self) -> Option<SemanticExtractionPauseReason> {
        if !self.config.enabled {
            Some(SemanticExtractionPauseReason::Disabled)
        } else if self.extractor.foreground_pressure() {
            Some(SemanticExtractionPauseReason::ForegroundPressure)
        } else if !self.extractor.available() {
            Some(SemanticExtractionPauseReason::ModelUnavailable)
        } else {
            None
        }
    }

    pub fn contract(&self) -> SemanticExtractionContract {
        let identity = self.extractor.identity();
        SemanticExtractionContract {
            semantic_schema_version: ATTENTION_SEMANTIC_SCHEMA_VERSION,
            extractor_contract: ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT.to_string(),
            prompt_version: self.extractor.prompt_version().to_string(),
            model: identity.model,
            profile: identity.profile,
        }
    }

    /// One finite pass. The caller owns the interval; this method never sleeps
    /// and never calls a model until all three pause gates are clear.
    pub async fn run_once(
        &self,
        principal: &str,
        workspace: &str,
        now: i64,
    ) -> Result<SemanticExtractionRunReport> {
        if let Some(reason) = self.pause_reason() {
            return Ok(SemanticExtractionRunReport {
                paused: Some(reason),
                ..Default::default()
            });
        }

        let mut report = SemanticExtractionRunReport::default();
        report.discovered += self
            .discover_follow_ups(principal, workspace, now, true)
            .await?;
        report.discovered += self.discover_worth(principal, workspace, now, true).await?;

        let lease_limit = self
            .config
            .batch_size
            .min(self.config.calls_per_minute as usize)
            .max(1);
        let concurrency = self.config.concurrency.max(1).min(8);
        let mut remaining = lease_limit;
        while remaining > 0 {
            if let Some(reason) = self.pause_reason() {
                report.paused = Some(reason);
                break;
            }
            // Claim only work that can start now. Giving an entire pass one
            // shared lease clock allowed queued local-model calls to expire
            // before an execution slot became available.
            let claim_now = chrono::Utc::now().timestamp_millis();
            let lease_expires_at =
                claim_now.saturating_add((self.config.lease_secs as i64).saturating_mul(1_000));
            let work = self
                .learning
                .lease_semantic_extraction_work(
                    principal,
                    workspace,
                    &self.lease_owner,
                    claim_now,
                    lease_expires_at,
                    remaining.min(concurrency),
                )
                .await?;
            if work.is_empty() {
                break;
            }
            remaining = remaining.saturating_sub(work.len());
            report.leased += work.len() as u64;
            let results = stream::iter(
                work.into_iter()
                    .map(|item| async move { self.process_item(item).await }),
            )
            .buffer_unordered(concurrency)
            .collect::<Vec<_>>()
            .await;
            for result in results {
                match result? {
                    SemanticExtractionWorkStatus::Succeeded => report.succeeded += 1,
                    SemanticExtractionWorkStatus::Missing => report.missing += 1,
                    SemanticExtractionWorkStatus::Invalid => report.invalid += 1,
                    SemanticExtractionWorkStatus::Retry => report.retry += 1,
                    SemanticExtractionWorkStatus::Dead => report.dead += 1,
                    // A stale source is reported separately by process_item as a
                    // missing terminal row with `stale_source_revision`.
                    SemanticExtractionWorkStatus::Pending
                    | SemanticExtractionWorkStatus::InFlight => report.stale += 1,
                }
            }
        }
        Ok(report)
    }

    /// Preview or enqueue one bounded discovery page without leasing work or
    /// calling a model. This is the admin boundary used while the worker is
    /// disabled and is safe to invoke repeatedly.
    pub async fn schedule_missing_once(
        &self,
        principal: &str,
        workspace: &str,
        now: i64,
        apply: bool,
    ) -> Result<SemanticExtractionRunReport> {
        let mut report = SemanticExtractionRunReport::default();
        report.discovered += self
            .discover_follow_ups(principal, workspace, now, apply)
            .await?;
        report.discovered += self
            .discover_worth(principal, workspace, now, apply)
            .await?;
        Ok(report)
    }

    // Discovery is a cheap, deterministic read. Keep its page size separate
    // from model-call/concurrency budgets so valid rows cannot consume hours
    // of backfill intervals before an uncovered candidate is even scheduled.
    async fn discover_follow_ups(
        &self,
        principal: &str,
        workspace: &str,
        now: i64,
        apply: bool,
    ) -> Result<u64> {
        let checkpoints = self
            .learning
            .semantic_extraction_checkpoints(principal, workspace)
            .await?;
        let cursor = checkpoints
            .iter()
            .find(|checkpoint| checkpoint.surface == AttentionSurface::FollowUp)
            .and_then(|checkpoint| checkpoint.cursor.as_deref());
        let rows = self
            .mail
            .list_active_semantic_annotations(
                principal,
                workspace,
                cursor,
                self.config.batch_size.max(500),
            )
            .await?;
        let identity = self.extractor.identity();
        let mut eligible = 0u64;
        for row in &rows {
            let Some(revision) = row.classification_input_revision else {
                continue;
            };
            let source_revision = format!("distill:{revision}");
            let compatible = deserialize_semantic_envelope(row.semantic_features.as_ref())
                .is_some_and(|envelope| {
                    envelope.is_compatible_with_extractor(
                        Some(&source_revision),
                        self.extractor.prompt_version(),
                        &identity,
                    )
                });
            if !compatible {
                eligible += 1;
                if apply {
                    let _ = self
                        .learning
                        .schedule_semantic_extraction_from_source(
                            principal,
                            workspace,
                            &ScheduleSemanticExtraction {
                                surface: AttentionSurface::FollowUp,
                                candidate_id: row.id.clone(),
                                source_revision,
                                source_revision_number: revision,
                                contract: self.contract(),
                            },
                            now,
                        )
                        .await?;
                }
            }
        }
        let next_cursor = if rows.len() < self.config.batch_size.max(500) {
            None
        } else {
            rows.last().map(|row| row.id.as_str())
        };
        if apply {
            self.learning
                .update_semantic_extraction_checkpoint(
                    principal,
                    workspace,
                    AttentionSurface::FollowUp,
                    next_cursor,
                    now,
                )
                .await?;
        }
        Ok(eligible)
    }

    /// This producer consumes locally distilled communication/web briefs.
    /// Tasks and memories use their own deterministic source features; they
    /// are not failed communication extractions and must not cause model calls.
    pub fn supports_worth_source(source_kind: &SourceKind) -> bool {
        matches!(source_kind, SourceKind::Comm | SourceKind::Web)
    }

    async fn discover_worth(
        &self,
        principal: &str,
        workspace: &str,
        now: i64,
        apply: bool,
    ) -> Result<u64> {
        let checkpoints = self
            .learning
            .semantic_extraction_checkpoints(principal, workspace)
            .await?;
        let cursor = checkpoints
            .iter()
            .find(|checkpoint| checkpoint.surface == AttentionSurface::WorthALook)
            .and_then(|checkpoint| checkpoint.cursor.as_deref());
        let rows = self
            .resurfacing
            .list_active_semantic_candidates(
                principal,
                workspace,
                cursor,
                self.config.batch_size.max(500),
            )
            .await?;
        let identity = self.extractor.identity();
        let mut eligible = 0u64;
        for row in &rows {
            if !Self::supports_worth_source(&row.source_kind) {
                continue;
            }
            let Some(source_revision) = row.content_revision.as_deref() else {
                continue;
            };
            let compatible = deserialize_semantic_envelope(row.semantic_features.as_ref())
                .is_some_and(|envelope| {
                    envelope.is_compatible_with_extractor(
                        Some(source_revision),
                        self.extractor.prompt_version(),
                        &identity,
                    )
                });
            if !compatible {
                eligible += 1;
                if apply {
                    let _ = self
                        .learning
                        .schedule_semantic_extraction_from_source(
                            principal,
                            workspace,
                            &ScheduleSemanticExtraction {
                                surface: AttentionSurface::WorthALook,
                                candidate_id: row.candidate_id.clone(),
                                source_revision: source_revision.to_string(),
                                source_revision_number: source_revision.parse().unwrap_or(0),
                                contract: self.contract(),
                            },
                            now,
                        )
                        .await?;
                }
            }
        }
        let next_cursor = if rows.len() < self.config.batch_size.max(500) {
            None
        } else {
            rows.last().map(|row| row.candidate_id.as_str())
        };
        if apply {
            self.learning
                .update_semantic_extraction_checkpoint(
                    principal,
                    workspace,
                    AttentionSurface::WorthALook,
                    next_cursor,
                    now,
                )
                .await?;
        }
        Ok(eligible)
    }

    async fn process_item(
        &self,
        item: SemanticExtractionWorkItem,
    ) -> Result<SemanticExtractionWorkStatus> {
        let source = self.load_safe_input(&item).await?;
        if matches!(source, SemanticSourceInput::AlreadyCovered) {
            self.learning
                .finish_semantic_extraction_work(
                    &item.work_id,
                    &item.source_revision,
                    &self.lease_owner,
                    SemanticExtractionWorkStatus::Succeeded,
                    None,
                    chrono::Utc::now().timestamp_millis(),
                )
                .await?;
            return Ok(SemanticExtractionWorkStatus::Succeeded);
        }
        let SemanticSourceInput::Ready(input) = source else {
            let commit_now = chrono::Utc::now().timestamp_millis();
            self.learning
                .finish_semantic_extraction_work(
                    &item.work_id,
                    &item.source_revision,
                    &self.lease_owner,
                    SemanticExtractionWorkStatus::Missing,
                    Some("stale_source_revision"),
                    commit_now,
                )
                .await?;
            return Ok(SemanticExtractionWorkStatus::Pending);
        };
        let mut extraction = Box::pin(self.extractor.extract(&input));
        let heartbeat_secs = self.config.lease_secs.saturating_div(3).clamp(1, 30);
        let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(heartbeat_secs));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // `interval` ticks immediately; consume that tick so renewal occurs
        // after a bounded interval rather than as redundant write at start.
        heartbeat.tick().await;
        let extraction_result = loop {
            tokio::select! {
                result = &mut extraction => break result,
                _ = heartbeat.tick() => {
                    let renewal_now = chrono::Utc::now().timestamp_millis();
                    let renewal_expires_at = renewal_now.saturating_add(
                        (self.config.lease_secs as i64).saturating_mul(1_000),
                    );
                    if !self.learning.renew_semantic_extraction_work_lease(
                        &item.work_id,
                        &item.source_revision,
                        &self.lease_owner,
                        renewal_expires_at,
                        renewal_now,
                    ).await? {
                        tracing::warn!(
                            surface = item.surface.as_str(),
                            candidate_id = %item.candidate_id,
                            "semantic extraction lease ownership changed during inference"
                        );
                        return Ok(SemanticExtractionWorkStatus::InFlight);
                    }
                }
            }
        };
        let envelope = match extraction_result {
            Ok(envelope) => envelope,
            Err(error) => {
                tracing::warn!(
                    surface = item.surface.as_str(),
                    candidate_id = %item.candidate_id,
                    error = %error,
                    "semantic extraction call failed"
                );
                return self
                    .schedule_retry(
                        &item,
                        chrono::Utc::now().timestamp_millis(),
                        "model_call_failed",
                    )
                    .await;
            },
        };
        if !envelope_matches_work(&envelope, &item) {
            return self
                .schedule_retry(
                    &item,
                    chrono::Utc::now().timestamp_millis(),
                    "extractor_contract_mismatch",
                )
                .await;
        }
        let commit_now = chrono::Utc::now().timestamp_millis();
        if !self
            .learning
            .semantic_extraction_lease_is_current(
                &item.work_id,
                &item.source_revision,
                &self.lease_owner,
                commit_now,
            )
            .await?
        {
            return Ok(SemanticExtractionWorkStatus::InFlight);
        }
        let encoded = serialize_semantic_envelope(&envelope)?;
        let committed = match item.surface {
            AttentionSurface::FollowUp => {
                self.mail
                    .update_semantic_features_if_revision(
                        &item.principal,
                        &item.workspace,
                        &item.candidate_id,
                        item.source_revision_number,
                        &encoded,
                    )
                    .await?
            },
            AttentionSurface::WorthALook => {
                self.resurfacing
                    .update_semantic_features_if_revision(
                        &item.principal,
                        &item.workspace,
                        &item.candidate_id,
                        &item.source_revision,
                        &encoded,
                    )
                    .await?
            },
        };
        if !committed {
            self.learning
                .finish_semantic_extraction_work(
                    &item.work_id,
                    &item.source_revision,
                    &self.lease_owner,
                    SemanticExtractionWorkStatus::Missing,
                    Some("stale_source_revision"),
                    commit_now,
                )
                .await?;
            return Ok(SemanticExtractionWorkStatus::Pending);
        }
        let status = match envelope.status {
            SemanticExtractionStatus::Succeeded => SemanticExtractionWorkStatus::Succeeded,
            SemanticExtractionStatus::Missing => SemanticExtractionWorkStatus::Missing,
            SemanticExtractionStatus::Invalid => SemanticExtractionWorkStatus::Invalid,
        };
        self.learning
            .finish_semantic_extraction_work(
                &item.work_id,
                &item.source_revision,
                &self.lease_owner,
                status,
                envelope.invalid_reason_code.as_deref(),
                commit_now,
            )
            .await?;
        Ok(status)
    }

    async fn schedule_retry(
        &self,
        item: &SemanticExtractionWorkItem,
        now: i64,
        error_code: &str,
    ) -> Result<SemanticExtractionWorkStatus> {
        let exhausted = item.attempts >= self.config.max_retries;
        let next_retry_at = if exhausted {
            None
        } else {
            let exponent = item.attempts.saturating_sub(1).min(20);
            let delay = self
                .config
                .retry_base_secs
                .saturating_mul(1u64 << exponent)
                .min(self.config.retry_max_secs);
            Some(now.saturating_add((delay as i64) * 1_000))
        };
        self.learning
            .retry_semantic_extraction_work(
                &item.work_id,
                &item.source_revision,
                &self.lease_owner,
                next_retry_at,
                error_code,
                now,
            )
            .await?;
        Ok(if exhausted {
            SemanticExtractionWorkStatus::Dead
        } else {
            SemanticExtractionWorkStatus::Retry
        })
    }

    fn source_is_covered(
        &self,
        raw: Option<&serde_json::Value>,
        item: &SemanticExtractionWorkItem,
    ) -> bool {
        deserialize_semantic_envelope(raw).is_some_and(|envelope| {
            envelope.input_revision == item.source_revision_number
                && envelope.is_compatible_with_extractor(
                    Some(&item.source_revision),
                    &item.contract.prompt_version,
                    &SemanticExtractorIdentity {
                        model: item.contract.model.clone(),
                        profile: item.contract.profile.clone(),
                    },
                )
        })
    }

    async fn load_safe_input(
        &self,
        item: &SemanticExtractionWorkItem,
    ) -> Result<SemanticSourceInput> {
        match item.surface {
            AttentionSurface::FollowUp => {
                let Some(annotation) = self
                    .mail
                    .get_annotation(&item.principal, &item.workspace, &item.candidate_id)
                    .await?
                else {
                    return Ok(SemanticSourceInput::Unavailable);
                };
                if annotation.classification_input_revision != Some(item.source_revision_number) {
                    return Ok(SemanticSourceInput::Unavailable);
                }
                if self.source_is_covered(annotation.semantic_features.as_ref(), item) {
                    return Ok(SemanticSourceInput::AlreadyCovered);
                }
                let Some(message_id) = annotation.evidence_message_id.as_deref() else {
                    return Ok(SemanticSourceInput::Unavailable);
                };
                let Some(message) = self
                    .mail
                    .get_message(
                        &item.principal,
                        &item.workspace,
                        &annotation.provider,
                        &annotation.account_alias,
                        message_id,
                    )
                    .await?
                else {
                    return Ok(SemanticSourceInput::Unavailable);
                };
                if message.sensitive_suppressed
                    || message.distill_revision != Some(item.source_revision_number)
                {
                    return Ok(SemanticSourceInput::Unavailable);
                }
                let mut metadata = BTreeMap::new();
                metadata.insert("channel".to_string(), annotation.provider);
                metadata.insert("lane".to_string(), annotation.lane.as_db_str().to_string());
                metadata.insert("subject".to_string(), message.subject.unwrap_or_default());
                metadata.insert(
                    "sender".to_string(),
                    message.from_address.unwrap_or_default(),
                );
                metadata.insert("label_ids".to_string(), message.label_ids.join(", "));
                metadata.insert(
                    "recipient_domains".to_string(),
                    message
                        .to_domains
                        .iter()
                        .chain(message.cc_domains.iter())
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", "),
                );
                metadata.insert("latest_message_id".to_string(), message.message_id);
                metadata.insert(
                    "latest_direction".to_string(),
                    message
                        .direction
                        .map(|value| value.as_db_str().to_string())
                        .unwrap_or_default(),
                );
                metadata.insert(
                    "latest_intent".to_string(),
                    message.intent.unwrap_or_default(),
                );
                metadata.insert(
                    "needs_reply_hint".to_string(),
                    message.needs_reply_hint.to_string(),
                );
                metadata.insert(
                    "follow_up_hint".to_string(),
                    message
                        .follow_up_hint
                        .as_ref()
                        .map(|value| serde_json::to_string(value))
                        .transpose()?
                        .unwrap_or_else(|| "none".to_string()),
                );
                Ok(SemanticSourceInput::Ready(SemanticExtractionInput {
                    surface: item.surface,
                    candidate_id: item.candidate_id.clone(),
                    source_revision: item.source_revision.clone(),
                    source_revision_number: item.source_revision_number,
                    safe_summary: message.summary,
                    safe_brief: message
                        .distill_brief
                        .map(serde_json::to_value)
                        .transpose()?,
                    source_metadata: metadata,
                }))
            },
            AttentionSurface::WorthALook => {
                let Some(candidate) = self
                    .resurfacing
                    .get_candidate(&item.principal, &item.workspace, &item.candidate_id)
                    .await?
                else {
                    return Ok(SemanticSourceInput::Unavailable);
                };
                if !Self::supports_worth_source(&candidate.source_kind)
                    || candidate.content_revision.as_deref() != Some(&item.source_revision)
                    || !matches!(
                        candidate.state,
                        CandidateState::Candidate | CandidateState::Surfaced
                    )
                {
                    return Ok(SemanticSourceInput::Unavailable);
                }
                if self.source_is_covered(candidate.semantic_features.as_ref(), item) {
                    return Ok(SemanticSourceInput::AlreadyCovered);
                }
                let mut metadata = BTreeMap::new();
                metadata.insert(
                    "channel".to_string(),
                    candidate.source_kind.as_str().to_string(),
                );
                metadata.insert("lane".to_string(), "worth_a_look".to_string());
                metadata.insert(
                    "source_kind".to_string(),
                    candidate.source_kind.as_str().to_string(),
                );
                if let Some(at) = candidate.temporal_anchor_at {
                    metadata.insert("temporal_anchor_at".to_string(), at.to_string());
                }
                Ok(SemanticSourceInput::Ready(SemanticExtractionInput {
                    surface: item.surface,
                    candidate_id: item.candidate_id.clone(),
                    source_revision: item.source_revision.clone(),
                    source_revision_number: item.source_revision_number,
                    safe_summary: None,
                    safe_brief: candidate
                        .content_details
                        .map(serde_json::to_value)
                        .transpose()?,
                    source_metadata: metadata,
                }))
            },
        }
    }
}

fn envelope_matches_work(
    envelope: &ChannelAttentionSemanticEnvelope,
    item: &SemanticExtractionWorkItem,
) -> bool {
    envelope.schema_version == item.contract.semantic_schema_version
        && envelope.extractor_contract == item.contract.extractor_contract
        && envelope.prompt_version == item.contract.prompt_version
        && envelope.model == item.contract.model
        && envelope.profile == item.contract.profile
        && envelope.input_revision == item.source_revision_number
        && envelope.source_revision.as_deref() == Some(item.source_revision.as_str())
}

#[cfg(test)]
mod attention_recovery_tests {
    use super::*;
    #[tokio::test]
    async fn attention_recovery_source_repaired_after_discovery_skips_model_dispatch() {
        use crate::channel_assist::types::{MailAssistActor, MailThreadAnnotation};
        let dir = tempfile::tempdir().unwrap();
        let learning = AttentionLearningStore::open(dir.path()).unwrap();
        let mail = ChannelAssistStore::open(dir.path()).unwrap();
        let resurfacing = ResurfacingStore::open(dir.path()).unwrap();
        let identity = SemanticExtractorIdentity::default();
        let features = serde_json::json!({"communication_type":"direct_request", "requested_action":"reply", "action_owner":"owner",
            "direct_request_probability":0.8,"broadcast_probability":0.1,"personal_obligation_probability":0.8,
            "information_value_probability":0.5,"deadline":{"kind":"none","value":null},
            "campaign_or_event_identity":null,"evidence_refs":["summary"]});
        let envelope = ChannelAttentionSemanticEnvelope::from_optional_value_for_source(
            Some(&features),
            "distill:1",
            1,
            "1.1.0",
            &identity,
        );
        let annotation: MailThreadAnnotation = serde_json::from_value(serde_json::json!({
            "schema_version":1,"id":"covered","provider":"gmail","account_alias":"acct","thread_id":"thread",
            "lane":"user_assist","state":"needs_approval","label":"follow_up","confidence":0.8,
            "reason":"test","evidence_refs":[],"evidence_message_id":null,"evidence_message_at":null,
            "classification_input_revision":1,"semantic_features":serialize_semantic_envelope(&envelope).unwrap(),
            "proposed_action":null,"provenance":null,"created_at":1,"updated_at":1
        })).unwrap();
        mail.create_annotation("p", "w", annotation, MailAssistActor::Worker)
            .await
            .unwrap();
        let request = ScheduleSemanticExtraction {
            surface: AttentionSurface::FollowUp,
            candidate_id: "covered".into(),
            source_revision: "distill:1".into(),
            source_revision_number: 1,
            contract: SemanticExtractionContract {
                semantic_schema_version: 1,
                extractor_contract: ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT.into(),
                prompt_version: "1.1.0".into(),
                model: None,
                profile: None,
            },
        };
        let now = chrono::Utc::now().timestamp_millis();
        learning
            .schedule_semantic_extraction("p", "w", &request, now)
            .await
            .unwrap();
        let item = learning
            .lease_semantic_extraction_work("p", "w", "lease", now, now + 60000, 1)
            .await
            .unwrap()
            .remove(0);
        // This extractor fails every call. Success therefore proves the source
        // recheck completed the lease without depending on model availability.
        let worker = SemanticExtractionWorker::new(
            learning.clone(),
            mail,
            resurfacing,
            Arc::new(UnavailableSemanticFeatureExtractor),
            AttentionSemanticBackfillConfig::default(),
            "lease",
        );
        assert_eq!(
            worker.process_item(item).await.unwrap(),
            SemanticExtractionWorkStatus::Succeeded
        );
        let queue = learning
            .semantic_extraction_queue_counts("p", "w")
            .await
            .unwrap();
        assert_eq!(queue.succeeded, 1);
        assert_eq!(queue.retry, 0);
    }

    #[test]
    fn communication_extractor_does_not_schedule_tasks_or_memories() {
        assert!(SemanticExtractionWorker::supports_worth_source(
            &SourceKind::Comm
        ));
        assert!(SemanticExtractionWorker::supports_worth_source(
            &SourceKind::Web
        ));
        assert!(!SemanticExtractionWorker::supports_worth_source(
            &SourceKind::Task
        ));
        assert!(!SemanticExtractionWorker::supports_worth_source(
            &SourceKind::Memory
        ));
    }
}
