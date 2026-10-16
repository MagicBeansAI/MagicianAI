//! Cutoff-bound migration of durable pre-Slice-1 owner labels.
//!
//! The worker imports only typed evidence that predates an immutable first-run
//! cutoff. Live handlers share event IDs with source-owned audit rows; bounded
//! repair tails reconcile partial dual writes after the finite migration.

use std::{
    collections::{BTreeSet, HashMap},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::channel_assist::store::{
    AttentionFeedbackHistoryRow, MailAssistStore, NeedsApprovalRow,
};
use magician::config::AttentionHistoricalBootstrapConfig;
use magician::magician_v2::attention::learning::{
    AttentionLabelQuality, AttentionLearningService, AttentionOutcomeKind, AttentionSurface,
    RecordAttentionOutcome, SemanticAttentionCandidate, SemanticEmbedding,
};
use magician::magician_v2::attention::resurfacing::store::{
    ResurfacingAttentionSignal, ResurfacingFeedbackRepairRow, ResurfacingStore,
};
use magician::magician_v2::attention::resurfacing::types::{DismissReason, FeedbackAction};
use magician::magician_v2::resurfacing_seam::Candidate;

pub const HISTORICAL_MAIL_FEEDBACK_SOURCE: &str = "mail_feedback_v1";
pub const HISTORICAL_WORTH_DISMISSED_SOURCE: &str = "worth_dismissed_embedding_v1";
pub const HISTORICAL_WORTH_AFFINITY_SOURCE: &str = "worth_affinity_embedding_v1";
pub const LIVE_MAIL_FEEDBACK_REPAIR_SOURCE: &str = "mail_feedback_live_repair_v1";
pub const LIVE_WORTH_FEEDBACK_REPAIR_SOURCE: &str = "worth_feedback_live_repair_v1";
pub const HISTORICAL_BOOTSTRAP_SOURCES: [&str; 3] = [
    HISTORICAL_MAIL_FEEDBACK_SOURCE,
    HISTORICAL_WORTH_DISMISSED_SOURCE,
    HISTORICAL_WORTH_AFFINITY_SOURCE,
];
const ATTENTION_CHECKPOINT_SOURCES: [&str; 5] = [
    HISTORICAL_MAIL_FEEDBACK_SOURCE,
    HISTORICAL_WORTH_DISMISSED_SOURCE,
    HISTORICAL_WORTH_AFFINITY_SOURCE,
    LIVE_MAIL_FEEDBACK_REPAIR_SOURCE,
    LIVE_WORTH_FEEDBACK_REPAIR_SOURCE,
];

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttentionHistoricalBootstrapReport {
    pub imported: u64,
    pub skipped: u64,
    pub blocked: u64,
    pub completed_sources: u64,
    pub refreshed_candidates: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HistoricalLabel {
    outcome: AttentionOutcomeKind,
    reason: Option<String>,
    quality: AttentionLabelQuality,
}

#[derive(Clone)]
pub struct AttentionHistoricalBootstrapWorker {
    learning: AttentionLearningService,
    mail: MailAssistStore,
    resurfacing: ResurfacingStore,
    config: AttentionHistoricalBootstrapConfig,
    /// `<runtime root>/scopes`. The authority on which scopes still exist, so
    /// discovery can tell a scope that is live from one whose rows merely
    /// outlived it.
    scopes_root: PathBuf,
}

impl AttentionHistoricalBootstrapWorker {
    pub fn new(
        learning: AttentionLearningService,
        mail: MailAssistStore,
        resurfacing: ResurfacingStore,
        config: AttentionHistoricalBootstrapConfig,
        scopes_root: PathBuf,
    ) -> Self {
        Self {
            learning,
            mail,
            resurfacing,
            config,
            scopes_root,
        }
    }

    /// Whether a scope still exists, by the same authority every pass that
    /// scans `scopes/` uses: its directory.
    fn scope_exists(&self, principal: &str, workspace: &str) -> bool {
        self.scopes_root.join(principal).join(workspace).is_dir()
    }

    /// Must be awaited before live producers are started. INSERT OR IGNORE in
    /// the store makes the first cutoff immutable across later restarts.
    pub async fn initialize(&self, principal: &str, workspace: &str, cutoff_at: i64) -> Result<()> {
        self.learning
            .store()
            .initialize_historical_bootstrap(
                principal,
                workspace,
                &ATTENTION_CHECKPOINT_SOURCES,
                cutoff_at,
            )
            .await
    }

    pub async fn initialize_existing_scopes(
        &self,
        cutoff_at: i64,
    ) -> Result<Vec<(String, String)>> {
        let scopes = self.discover_scopes().await?;
        for (principal, workspace) in &scopes {
            self.initialize(principal, workspace, cutoff_at).await?;
        }
        Ok(scopes)
    }

    /// Drain the bounded, capped Worth signal sources before live producers
    /// can evict an unimported pre-cutoff row. Mail history is uncapped and is
    /// intentionally left to the progressive background pass.
    pub async fn drain_historical_worth_before_live(
        &self,
        scopes: &[(String, String)],
    ) -> Result<()> {
        for (principal, workspace) in scopes {
            for _ in 0..512 {
                let mut report = AttentionHistoricalBootstrapReport::default();
                self.import_worth_signals(
                    principal,
                    workspace,
                    HISTORICAL_WORTH_DISMISSED_SOURCE,
                    AttentionOutcomeKind::Irrelevant,
                    &mut report,
                )
                .await?;
                self.import_worth_signals(
                    principal,
                    workspace,
                    HISTORICAL_WORTH_AFFINITY_SOURCE,
                    AttentionOutcomeKind::Useful,
                    &mut report,
                )
                .await?;
                let dismissed_done = self
                    .learning
                    .store()
                    .historical_bootstrap_checkpoint(
                        principal,
                        workspace,
                        HISTORICAL_WORTH_DISMISSED_SOURCE,
                    )
                    .await?
                    .is_some_and(|checkpoint| checkpoint.completed);
                let affinity_done = self
                    .learning
                    .store()
                    .historical_bootstrap_checkpoint(
                        principal,
                        workspace,
                        HISTORICAL_WORTH_AFFINITY_SOURCE,
                    )
                    .await?
                    .is_some_and(|checkpoint| checkpoint.completed);
                if dismissed_done && affinity_done {
                    break;
                }
            }
            let store = self.learning.store();
            let dismissed_done = store
                .historical_bootstrap_checkpoint(
                    principal,
                    workspace,
                    HISTORICAL_WORTH_DISMISSED_SOURCE,
                )
                .await?
                .is_some_and(|checkpoint| checkpoint.completed);
            let affinity_done = store
                .historical_bootstrap_checkpoint(
                    principal,
                    workspace,
                    HISTORICAL_WORTH_AFFINITY_SOURCE,
                )
                .await?
                .is_some_and(|checkpoint| checkpoint.completed);
            anyhow::ensure!(
                dismissed_done && affinity_done,
                "capped historical Worth-a-look evidence did not drain before producers"
            );
        }
        Ok(())
    }

    async fn initialize_runtime_scopes(&self, cutoff_at: i64) -> Result<Vec<(String, String)>> {
        let scopes = self.discover_scopes().await?;
        for (principal, workspace) in &scopes {
            let existed_at_startup = self
                .learning
                .store()
                .historical_bootstrap_checkpoint(
                    principal,
                    workspace,
                    HISTORICAL_MAIL_FEEDBACK_SOURCE,
                )
                .await?
                .is_some();
            if !existed_at_startup {
                // The scope can already have produced live actions before the
                // next discovery tick. Start repair tails at the beginning;
                // source queries admit only typed feedback/outbox rows.
                self.initialize(principal, workspace, 0).await?;
                // A scope first seen after producers started has no pre-live
                // migration boundary. Its source audit rows are covered by the
                // shared-event repair tails, so mark finite sources complete to
                // avoid importing the same Worth action again via legacy signals.
                for source in HISTORICAL_BOOTSTRAP_SOURCES {
                    self.learning
                        .store()
                        .advance_historical_bootstrap_checkpoint(
                            principal, workspace, source, 0, "", 0, 0, true, cutoff_at,
                        )
                        .await?;
                }
            } else {
                self.initialize(principal, workspace, cutoff_at).await?;
            }
        }
        Ok(scopes)
    }

    async fn discover_scopes(&self) -> Result<Vec<(String, String)>> {
        let mut scopes = self.mail.list_scopes().into_iter().collect::<BTreeSet<_>>();
        scopes.extend(self.resurfacing.list_scopes().await?);
        scopes.extend(self.learning.store().list_scopes().await?);
        // Every source above is a database. A row there records what a scope
        // once did; it is not evidence that the scope still exists. Without
        // this filter a deleted workspace is rediscovered here, `initialize`
        // writes its checkpoints, and the scope directory is materialized
        // again — after which every later pass that scans `scopes/` finds it
        // and treats it as live. Measured 2026-09-15: 44 disposable
        // fixture-eval workspaces came back on the next boot after being
        // removed, because 225 checkpoint rows and 134 resurfacing candidates
        // outlived them.
        scopes.retain(|(principal, workspace)| self.scope_exists(principal, workspace));
        // A reserved sink catches records belonging to no tenant. Attention is
        // about what a person should be shown next, so it means nothing in a
        // bucket with no reader — and `initialize` here is what opened the
        // inbox DuckDB in one, a 3.3 MB file plus a scheduler pool sized to the
        // core count, for a mailbox that cannot receive mail. The sink's own
        // checkpoint rows are not evidence it needs this: they exist only
        // because this pass wrote them, which is the same circularity the
        // directory filter above removes.
        scopes.retain(|(principal, workspace)| {
            magician::magician_v2::artifact_v2::workspace::scope_hosts_user_subsystems(
                principal, workspace,
            )
        });
        // Preserve the package/default single-user scope even on a new install
        // before its first source database has been materialized. Inserted
        // after the filter: on a fresh install its directory does not exist
        // yet, and this pass is what prepares it.
        scopes.insert(("anonymous".to_string(), "default".to_string()));
        Ok(scopes.into_iter().collect())
    }

    pub fn spawn(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            if !magician::magician_v2::runtime::startup::wait_for_http().await {
                return;
            }
            loop {
                let now = chrono::Utc::now().timestamp_millis();
                match self.initialize_runtime_scopes(now).await {
                    Ok(scopes) => {
                        let repaired_embedding_scopes = match self
                            .learning
                            .repair_pending_embedding_binds_with_scopes(self.config.batch_size)
                            .await
                        {
                            Ok((_repaired, scopes)) => scopes,
                            Err(error) => {
                                tracing::warn!(error = %error, "attention embedding repair pass failed; retrying");
                                BTreeSet::new()
                            },
                        };
                        for (principal, workspace) in scopes {
                            let repaired_embedding_bind = scope_has_repaired_embedding_bind(
                                &repaired_embedding_scopes,
                                &principal,
                                &workspace,
                            );
                            match self
                                .run_once_inner(&principal, &workspace, repaired_embedding_bind)
                                .await
                            {
                                Ok(report) => tracing::info!(
                                    principal,
                                    workspace,
                                    imported = report.imported,
                                    skipped = report.skipped,
                                    blocked = report.blocked,
                                    completed_sources = report.completed_sources,
                                    refreshed_candidates = report.refreshed_candidates,
                                    "historical attention bootstrap pass completed"
                                ),
                                Err(error) => tracing::warn!(
                                    principal,
                                    workspace,
                                    error = %error,
                                    "historical attention bootstrap scope pass failed; retrying"
                                ),
                            }
                        }
                    },
                    Err(error) => {
                        tracing::warn!(error = %error, "attention scope discovery failed; retrying")
                    },
                }
                tokio::time::sleep(Duration::from_secs(self.config.interval_secs.max(10))).await;
            }
        })
    }

    pub async fn run_once(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<AttentionHistoricalBootstrapReport> {
        self.run_once_inner(principal, workspace, false).await
    }

    async fn run_once_inner(
        &self,
        principal: &str,
        workspace: &str,
        force_refresh: bool,
    ) -> Result<AttentionHistoricalBootstrapReport> {
        let mut report = AttentionHistoricalBootstrapReport::default();
        self.repair_live_mail(principal, workspace, &mut report)
            .await?;
        self.repair_live_worth(principal, workspace, &mut report)
            .await?;
        self.import_mail(principal, workspace, &mut report).await?;
        self.import_worth_signals(
            principal,
            workspace,
            HISTORICAL_WORTH_DISMISSED_SOURCE,
            AttentionOutcomeKind::Irrelevant,
            &mut report,
        )
        .await?;
        self.import_worth_signals(
            principal,
            workspace,
            HISTORICAL_WORTH_AFFINITY_SOURCE,
            AttentionOutcomeKind::Useful,
            &mut report,
        )
        .await?;
        // Rebuilding both active cohorts is the expensive part of this worker.
        // Run it only when this pass persisted new evidence, or when the
        // preceding repair pass attached an embedding to durable evidence.
        // Polling an unchanged source generation must remain a cheap cursor
        // check rather than a periodic full rescore.
        if should_refresh_current_cohorts(report.imported, force_refresh) {
            report.refreshed_candidates =
                self.refresh_current_cohorts(principal, workspace).await? as u64;
        }
        report.completed_sources = 0;
        for source in HISTORICAL_BOOTSTRAP_SOURCES {
            if self
                .learning
                .store()
                .historical_bootstrap_checkpoint(principal, workspace, source)
                .await?
                .is_some_and(|checkpoint| checkpoint.completed)
            {
                report.completed_sources += 1;
            }
        }
        Ok(report)
    }

    async fn repair_live_worth(
        &self,
        principal: &str,
        workspace: &str,
        report: &mut AttentionHistoricalBootstrapReport,
    ) -> Result<()> {
        let store = self.learning.store();
        let Some(checkpoint) = store
            .historical_bootstrap_checkpoint(
                principal,
                workspace,
                LIVE_WORTH_FEEDBACK_REPAIR_SOURCE,
            )
            .await?
        else {
            anyhow::bail!("live Worth-a-look repair checkpoint is not initialized")
        };
        let initial_at = checkpoint.cutoff_at.div_euclid(1_000).saturating_sub(1);
        let after_at = checkpoint.cursor_at.max(initial_at);
        let after_id = if checkpoint.cursor_at < initial_at {
            0
        } else {
            checkpoint.cursor_id.parse::<i64>().unwrap_or(0)
        };
        let rows = self
            .resurfacing
            .list_feedback_attention_repairs_after(
                principal,
                workspace,
                after_at,
                after_id,
                self.config.batch_size,
            )
            .await?;
        if rows.is_empty() {
            return Ok(());
        }
        let active_contract = self.learning.active_embedding_contract();
        for row in rows {
            if store
                .outcome_exists_for_event(principal, workspace, &row.event_id)
                .await?
            {
                self.advance(
                    principal,
                    workspace,
                    LIVE_WORTH_FEEDBACK_REPAIR_SOURCE,
                    row.occurred_at,
                    &format!("{:020}", row.id),
                    false,
                    false,
                    false,
                )
                .await?;
                continue;
            }
            let Some((outcome, reason)) = live_worth_label(&row) else {
                report.skipped += 1;
                self.advance(
                    principal,
                    workspace,
                    LIVE_WORTH_FEEDBACK_REPAIR_SOURCE,
                    row.occurred_at,
                    &format!("{:020}", row.id),
                    false,
                    true,
                    false,
                )
                .await?;
                continue;
            };
            let existing_embedding = match (row.embedding_contract.as_ref(), row.embedding.as_ref())
            {
                (Some(contract), Some(vector))
                    if active_contract.as_deref() == Some(contract.as_str()) =>
                {
                    Some(SemanticEmbedding {
                        contract: contract.clone(),
                        vector: vector.clone(),
                    })
                },
                _ => None,
            };
            if existing_embedding.is_none() && row.semantic_text.trim().is_empty() {
                report.skipped += 1;
                self.advance(
                    principal,
                    workspace,
                    LIVE_WORTH_FEEDBACK_REPAIR_SOURCE,
                    row.occurred_at,
                    &format!("{:020}", row.id),
                    false,
                    true,
                    false,
                )
                .await?;
                continue;
            }
            self.learning
                .stage_historical_outcome(
                    principal,
                    workspace,
                    AttentionSurface::WorthALook,
                    RecordAttentionOutcome {
                        event_id: row.event_id.clone(),
                        candidate: SemanticAttentionCandidate {
                            candidate_id: row.candidate_id.clone(),
                            source_revision: None,
                            semantic_text: row.semantic_text.clone(),
                            existing_embedding,
                            actionability_features: None,
                            grouping_features: None,
                        },
                        outcome,
                        reason,
                        label_quality: AttentionLabelQuality::Strong,
                        occurred_at: row.occurred_at.saturating_mul(1_000),
                        attribution: None,
                    },
                )
                .await
                .context("repairing live Worth-a-look canonical outcome")?;
            report.imported += 1;
            self.advance(
                principal,
                workspace,
                LIVE_WORTH_FEEDBACK_REPAIR_SOURCE,
                row.occurred_at,
                &format!("{:020}", row.id),
                true,
                false,
                false,
            )
            .await?;
        }
        Ok(())
    }

    async fn repair_live_mail(
        &self,
        principal: &str,
        workspace: &str,
        report: &mut AttentionHistoricalBootstrapReport,
    ) -> Result<()> {
        let store = self.learning.store();
        let Some(checkpoint) = store
            .historical_bootstrap_checkpoint(principal, workspace, LIVE_MAIL_FEEDBACK_REPAIR_SOURCE)
            .await?
        else {
            anyhow::bail!("live mail repair checkpoint is not initialized")
        };
        let cursor_at = checkpoint.cursor_at.max(checkpoint.cutoff_at);
        let cursor_id = if checkpoint.cursor_at < checkpoint.cutoff_at {
            ""
        } else {
            checkpoint.cursor_id.as_str()
        };
        let rows = self
            .mail
            .list_attention_feedback_since(
                principal,
                workspace,
                cursor_at,
                cursor_id,
                i64::MAX,
                self.config.batch_size,
            )
            .await?;
        if rows.is_empty() {
            return Ok(());
        }
        for row in rows {
            if store
                .outcome_exists_for_event(principal, workspace, &row.event_id)
                .await?
            {
                self.advance(
                    principal,
                    workspace,
                    LIVE_MAIL_FEEDBACK_REPAIR_SOURCE,
                    row.created_at,
                    &row.event_id,
                    false,
                    false,
                    false,
                )
                .await?;
                continue;
            }
            let Some(label) = historical_mail_label(&row) else {
                report.skipped += 1;
                self.advance(
                    principal,
                    workspace,
                    LIVE_MAIL_FEEDBACK_REPAIR_SOURCE,
                    row.created_at,
                    &row.event_id,
                    false,
                    true,
                    false,
                )
                .await?;
                continue;
            };
            let semantic_text = row.subject.clone().unwrap_or_default();
            if semantic_text.trim().is_empty() {
                report.skipped += 1;
                self.advance(
                    principal,
                    workspace,
                    LIVE_MAIL_FEEDBACK_REPAIR_SOURCE,
                    row.created_at,
                    &row.event_id,
                    false,
                    true,
                    false,
                )
                .await?;
                continue;
            }
            self.learning
                .stage_historical_outcome(
                    principal,
                    workspace,
                    AttentionSurface::FollowUp,
                    RecordAttentionOutcome {
                        event_id: row.event_id.clone(),
                        candidate: SemanticAttentionCandidate {
                            candidate_id: synthetic_candidate_id(
                                LIVE_MAIL_FEEDBACK_REPAIR_SOURCE,
                                &row.event_id,
                            ),
                            source_revision: None,
                            semantic_text,
                            existing_embedding: None,
                            actionability_features: None,
                            grouping_features: None,
                        },
                        outcome: label.outcome,
                        reason: label.reason,
                        label_quality: label.quality,
                        occurred_at: row.created_at,
                        attribution: None,
                    },
                )
                .await
                .context("repairing live Follow-up canonical outcome")?;
            report.imported += 1;
            self.advance(
                principal,
                workspace,
                LIVE_MAIL_FEEDBACK_REPAIR_SOURCE,
                row.created_at,
                &row.event_id,
                true,
                false,
                false,
            )
            .await?;
        }
        Ok(())
    }

    async fn refresh_current_cohorts(&self, principal: &str, workspace: &str) -> Result<usize> {
        let follow_up = load_follow_up_cohort(
            &self.mail,
            principal,
            workspace,
            self.learning.rescore_limit(),
        )
        .await?;
        let worth = load_worth_cohort(
            &self.resurfacing,
            principal,
            workspace,
            self.learning.rescore_limit(),
        )
        .await?;
        let follow_up_count = self
            .learning
            .refresh_active_scores(principal, workspace, AttentionSurface::FollowUp, follow_up)
            .await?;
        let worth_count = self
            .learning
            .refresh_active_scores(principal, workspace, AttentionSurface::WorthALook, worth)
            .await?;
        Ok(follow_up_count.saturating_add(worth_count))
    }

    async fn import_mail(
        &self,
        principal: &str,
        workspace: &str,
        report: &mut AttentionHistoricalBootstrapReport,
    ) -> Result<()> {
        let store = self.learning.store();
        let Some(checkpoint) = store
            .historical_bootstrap_checkpoint(principal, workspace, HISTORICAL_MAIL_FEEDBACK_SOURCE)
            .await?
        else {
            anyhow::bail!("historical mail feedback checkpoint is not initialized")
        };
        if checkpoint.completed {
            return Ok(());
        }
        let rows = self
            .mail
            .list_attention_feedback_since(
                principal,
                workspace,
                checkpoint.cursor_at,
                &checkpoint.cursor_id,
                checkpoint.cutoff_at,
                self.config.batch_size,
            )
            .await?;
        if rows.is_empty() {
            store
                .advance_historical_bootstrap_checkpoint(
                    principal,
                    workspace,
                    HISTORICAL_MAIL_FEEDBACK_SOURCE,
                    checkpoint.cursor_at,
                    &checkpoint.cursor_id,
                    0,
                    0,
                    true,
                    chrono::Utc::now().timestamp_millis(),
                )
                .await?;
            return Ok(());
        }
        let completes_source = rows.len() < self.config.batch_size;
        for (index, row) in rows.iter().enumerate() {
            let completes_here = completes_source && index + 1 == rows.len();
            if store
                .outcome_exists_for_event(principal, workspace, &row.event_id)
                .await?
            {
                self.advance(
                    principal,
                    workspace,
                    HISTORICAL_MAIL_FEEDBACK_SOURCE,
                    row.created_at,
                    &row.event_id,
                    false,
                    false,
                    completes_here,
                )
                .await?;
                continue;
            }
            let Some(label) = historical_mail_label(row) else {
                report.skipped += 1;
                self.advance(
                    principal,
                    workspace,
                    HISTORICAL_MAIL_FEEDBACK_SOURCE,
                    row.created_at,
                    &row.event_id,
                    false,
                    true,
                    completes_here,
                )
                .await?;
                continue;
            };
            let semantic_text = row.subject.clone().unwrap_or_default();
            if semantic_text.trim().is_empty() {
                report.skipped += 1;
                self.advance(
                    principal,
                    workspace,
                    HISTORICAL_MAIL_FEEDBACK_SOURCE,
                    row.created_at,
                    &row.event_id,
                    false,
                    true,
                    completes_here,
                )
                .await?;
                continue;
            }
            let candidate = SemanticAttentionCandidate {
                candidate_id: synthetic_candidate_id(
                    HISTORICAL_MAIL_FEEDBACK_SOURCE,
                    &row.event_id,
                ),
                source_revision: None,
                semantic_text,
                existing_embedding: None,
                actionability_features: None,
                grouping_features: None,
            };
            let receipt = self
                .learning
                .stage_historical_outcome(
                    principal,
                    workspace,
                    AttentionSurface::FollowUp,
                    RecordAttentionOutcome {
                        event_id: row.event_id.clone(),
                        candidate,
                        outcome: label.outcome,
                        reason: label.reason,
                        label_quality: label.quality,
                        occurred_at: row.created_at,
                        attribution: None,
                    },
                )
                .await
                .context("importing historical Follow-up label")?;
            if !store
                .outcome_has_embedding(principal, workspace, &receipt.outcome_id)
                .await?
            {
                let bind_status = store
                    .embedding_bind_status_for_outcome(&receipt.outcome_id)
                    .await?;
                if matches!(
                    bind_status.as_deref(),
                    Some("pending" | "in_flight" | "retry")
                ) {
                    // The local embedder is unavailable or timed out. The
                    // outcome is durable and replay-safe; leave the cursor on
                    // this row while bounded repair is still active.
                    report.blocked += 1;
                    break;
                }
                // Dead-lettered or independently retired repair content must
                // not hold every later historical label behind this row.
                report.skipped += 1;
                self.advance(
                    principal,
                    workspace,
                    HISTORICAL_MAIL_FEEDBACK_SOURCE,
                    row.created_at,
                    &row.event_id,
                    false,
                    true,
                    completes_here,
                )
                .await?;
                continue;
            }
            report.imported += 1;
            self.advance(
                principal,
                workspace,
                HISTORICAL_MAIL_FEEDBACK_SOURCE,
                row.created_at,
                &row.event_id,
                true,
                false,
                completes_here,
            )
            .await?;
        }
        Ok(())
    }

    async fn import_worth_signals(
        &self,
        principal: &str,
        workspace: &str,
        source: &'static str,
        outcome: AttentionOutcomeKind,
        report: &mut AttentionHistoricalBootstrapReport,
    ) -> Result<()> {
        let store = self.learning.store();
        let Some(checkpoint) = store
            .historical_bootstrap_checkpoint(principal, workspace, source)
            .await?
        else {
            anyhow::bail!("historical Worth-a-look checkpoint is not initialized")
        };
        if checkpoint.completed {
            return Ok(());
        }
        // The source clock is whole seconds. Ceil the millisecond cutoff so
        // every row that existed before startup in the current second is
        // included; this finite source is drained before producers start.
        let cutoff_secs = checkpoint.cutoff_at.saturating_add(999).div_euclid(1_000);
        let rows = if source == HISTORICAL_WORTH_DISMISSED_SOURCE {
            self.resurfacing
                .list_dismissed_attention_signals_after(
                    principal,
                    workspace,
                    checkpoint.cursor_at,
                    cutoff_secs,
                    self.config.batch_size,
                )
                .await?
        } else {
            self.resurfacing
                .list_affinity_attention_signals_after(
                    principal,
                    workspace,
                    checkpoint.cursor_at,
                    cutoff_secs,
                    self.config.batch_size,
                )
                .await?
        };
        if rows.is_empty() {
            store
                .advance_historical_bootstrap_checkpoint(
                    principal,
                    workspace,
                    source,
                    checkpoint.cursor_at,
                    &checkpoint.cursor_id,
                    0,
                    0,
                    true,
                    chrono::Utc::now().timestamp_millis(),
                )
                .await?;
            return Ok(());
        }
        let active = load_worth_cohort(
            &self.resurfacing,
            principal,
            workspace,
            self.learning.rescore_limit(),
        )
        .await?;
        let active_contract = self.learning.active_embedding_contract().or_else(|| {
            active.iter().find_map(|candidate| {
                candidate
                    .existing_embedding
                    .as_ref()
                    .map(|embedding| embedding.contract.clone())
            })
        });
        let completes_source = rows.len() < self.config.batch_size;
        for (index, signal) in rows.iter().enumerate() {
            let completes_here = completes_source && index + 1 == rows.len();
            let key = signal.id.to_string();
            if signal.live_event_id.is_some() {
                // This exact vector is owned by the post-activation feedback
                // outbox and its shared-event repair tail. A second legacy
                // outcome would double-weight one owner action.
                self.advance(
                    principal,
                    workspace,
                    source,
                    signal.id,
                    "",
                    false,
                    false,
                    completes_here,
                )
                .await?;
                continue;
            }
            if active_contract.as_deref() != Some(signal.embedding_contract.as_str())
                || !valid_historical_vector(&signal.embedding)
            {
                report.skipped += 1;
                self.advance(
                    principal,
                    workspace,
                    source,
                    signal.id,
                    "",
                    false,
                    true,
                    completes_here,
                )
                .await?;
                continue;
            }
            let candidate = signal_candidate(source, signal);
            let receipt = self
                .learning
                .stage_historical_outcome(
                    principal,
                    workspace,
                    AttentionSurface::WorthALook,
                    RecordAttentionOutcome {
                        event_id: historical_event_id(source, principal, workspace, &key),
                        candidate,
                        outcome,
                        reason: None,
                        label_quality: AttentionLabelQuality::Strong,
                        occurred_at: signal.occurred_at.saturating_mul(1_000),
                        attribution: None,
                    },
                )
                .await
                .context("importing historical Worth-a-look semantic signal")?;
            let bound = store
                .outcome_has_embedding(principal, workspace, &receipt.outcome_id)
                .await?;
            if bound {
                report.imported += 1;
            } else {
                // A retained vector from an obsolete embedding contract cannot
                // be mixed with the active contract. Record it as skipped and
                // continue instead of poisoning the current estimator.
                report.skipped += 1;
            }
            self.advance(
                principal,
                workspace,
                source,
                signal.id,
                "",
                bound,
                !bound,
                completes_here,
            )
            .await?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn advance(
        &self,
        principal: &str,
        workspace: &str,
        source: &str,
        cursor_at: i64,
        cursor_id: &str,
        imported: bool,
        skipped: bool,
        completed: bool,
    ) -> Result<()> {
        self.learning
            .store()
            .advance_historical_bootstrap_checkpoint(
                principal,
                workspace,
                source,
                cursor_at,
                cursor_id,
                u64::from(imported),
                u64::from(skipped),
                completed,
                chrono::Utc::now().timestamp_millis(),
            )
            .await
    }
}

fn should_refresh_current_cohorts(imported: u64, repaired_embedding_bind: bool) -> bool {
    imported > 0 || repaired_embedding_bind
}

fn scope_has_repaired_embedding_bind(
    repaired_scopes: &BTreeSet<(String, String)>,
    principal: &str,
    workspace: &str,
) -> bool {
    repaired_scopes
        .iter()
        .any(|(candidate_principal, candidate_workspace)| {
            candidate_principal == principal && candidate_workspace == workspace
        })
}

fn historical_mail_label(row: &AttentionFeedbackHistoryRow) -> Option<HistoricalLabel> {
    let verdict = row.verdict.trim().to_ascii_lowercase();
    let reason = row
        .comment
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if verdict == "helpful" && row.paired_to_state.as_deref() == Some("approved") {
        return Some(HistoricalLabel {
            outcome: AttentionOutcomeKind::ActionCompleted,
            reason: None,
            quality: AttentionLabelQuality::Strong,
        });
    }
    match verdict.as_str() {
        "helpful" => Some(HistoricalLabel {
            outcome: AttentionOutcomeKind::Useful,
            reason: None,
            quality: AttentionLabelQuality::Strong,
        }),
        "not_helpful" => Some(HistoricalLabel {
            outcome: dismiss_outcome(reason),
            reason: reason.map(str::to_string),
            // This is an explicit owner verdict even when an older client did
            // not persist a same-timestamp lifecycle transition.
            quality: AttentionLabelQuality::Strong,
        }),
        "wrong_label" => Some(HistoricalLabel {
            outcome: AttentionOutcomeKind::NotActionable,
            reason: Some("wrong_classification".to_string()),
            quality: AttentionLabelQuality::Strong,
        }),
        _ => None,
    }
}

fn live_worth_label(
    row: &ResurfacingFeedbackRepairRow,
) -> Option<(AttentionOutcomeKind, Option<String>)> {
    let outcome = match row.action? {
        FeedbackAction::Open => AttentionOutcomeKind::Useful,
        FeedbackAction::OwnerWork => AttentionOutcomeKind::ActionCompleted,
        FeedbackAction::Acknowledge => AttentionOutcomeKind::NeutralSeen,
        FeedbackAction::Dismiss => match row.reason {
            Some(DismissReason::AlreadyHandled) => AttentionOutcomeKind::Obsolete,
            Some(DismissReason::Duplicate) => AttentionOutcomeKind::DuplicateOf,
            Some(DismissReason::Delegated) => AttentionOutcomeKind::NotOwner,
            Some(DismissReason::Spam | DismissReason::NotRelevant) | None => {
                AttentionOutcomeKind::Irrelevant
            },
        },
    };
    Some((
        outcome,
        row.reason.map(|reason| reason.as_str().to_string()),
    ))
}

fn dismiss_outcome(reason: Option<&str>) -> AttentionOutcomeKind {
    match reason {
        Some("wrong_classification" | "not_actionable") => AttentionOutcomeKind::NotActionable,
        Some("already_handled") => AttentionOutcomeKind::Obsolete,
        Some("delegated" | "wrong_owner") => AttentionOutcomeKind::NotOwner,
        Some("duplicate") => AttentionOutcomeKind::DuplicateOf,
        Some("spam" | "not_relevant") | None | Some(_) => AttentionOutcomeKind::Irrelevant,
    }
}

fn historical_event_id(source: &str, principal: &str, workspace: &str, key: &str) -> String {
    let digest = blake3::hash(
        format!("legacy-attention-v1\x1f{source}\x1f{principal}\x1f{workspace}\x1f{key}")
            .as_bytes(),
    );
    format!("legacy:v1:{}", digest.to_hex())
}

fn synthetic_candidate_id(source: &str, key: &str) -> String {
    let digest = blake3::hash(format!("{source}\x1f{key}").as_bytes());
    format!("legacy:{}", digest.to_hex())
}

fn signal_candidate(
    source: &str,
    signal: &ResurfacingAttentionSignal,
) -> SemanticAttentionCandidate {
    SemanticAttentionCandidate {
        candidate_id: synthetic_candidate_id(source, &signal.id.to_string()),
        source_revision: None,
        semantic_text: String::new(),
        existing_embedding: Some(SemanticEmbedding {
            contract: signal.embedding_contract.clone(),
            vector: signal.embedding.clone(),
        }),
        actionability_features: None,
        grouping_features: None,
    }
}

fn valid_historical_vector(vector: &[f32]) -> bool {
    !vector.is_empty()
        && vector.iter().all(|value| value.is_finite())
        && vector.iter().any(|value| value.abs() > f32::EPSILON)
}

fn follow_up_candidate(row: &NeedsApprovalRow) -> SemanticAttentionCandidate {
    let proposed_action = row
        .proposed_action
        .as_ref()
        .and_then(|value| serde_json::to_string(value).ok())
        .unwrap_or_default();
    SemanticAttentionCandidate {
        candidate_id: row.annotation_id.clone(),
        source_revision: row
            .classification_input_revision
            .map(|revision| format!("distill:{revision}")),
        semantic_text: [
            row.subject.as_deref().unwrap_or_default(),
            row.latest_summary.as_deref().unwrap_or_default(),
            row.label.as_deref().unwrap_or_default(),
            row.reason.as_deref().unwrap_or_default(),
            proposed_action.as_str(),
        ]
        .join("\n"),
        existing_embedding: None,
        actionability_features: None,
        grouping_features: None,
    }
}

async fn load_follow_up_cohort(
    store: &MailAssistStore,
    principal: &str,
    workspace: &str,
    limit: usize,
) -> Result<Vec<SemanticAttentionCandidate>> {
    Ok(store
        .list_needs_approval_attention_lane_page(
            principal,
            workspace,
            AttentionSurface::FollowUp.as_str(),
            limit.max(1),
            0,
            None,
        )
        .await?
        .rows
        .iter()
        .map(follow_up_candidate)
        .collect())
}

fn worth_candidate(
    candidate: &Candidate,
    embedding: Option<SemanticEmbedding>,
) -> SemanticAttentionCandidate {
    let details = candidate
        .content_details
        .as_ref()
        .and_then(|value| serde_json::to_string(value).ok())
        .unwrap_or_default();
    SemanticAttentionCandidate {
        candidate_id: candidate.candidate_id.clone(),
        source_revision: candidate
            .content_revision
            .clone()
            .or_else(|| Some(candidate.content_digest.clone())),
        semantic_text: [
            candidate.title.as_str(),
            candidate.content_digest.as_str(),
            details.as_str(),
        ]
        .join("\n"),
        existing_embedding: embedding,
        actionability_features: None,
        grouping_features: None,
    }
}

async fn load_worth_cohort(
    store: &ResurfacingStore,
    principal: &str,
    workspace: &str,
    limit: usize,
) -> Result<Vec<SemanticAttentionCandidate>> {
    let candidates = store
        .list_surfaced_page(principal, workspace, limit.max(1), 0, None)
        .await?
        .candidates;
    let ids = candidates
        .iter()
        .map(|candidate| candidate.candidate_id.clone())
        .collect::<Vec<_>>();
    let embeddings = store
        .list_candidate_embedding_snapshots_for_ids(principal, workspace, &ids)
        .await?
        .into_iter()
        .map(|snapshot| {
            (
                snapshot.candidate_id,
                SemanticEmbedding {
                    contract: snapshot.embedding_contract,
                    vector: snapshot.embedding,
                },
            )
        })
        .collect::<HashMap<_, _>>();
    Ok(candidates
        .iter()
        .map(|candidate| {
            worth_candidate(candidate, embeddings.get(&candidate.candidate_id).cloned())
        })
        .collect())
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn unchanged_bootstrap_pass_does_not_rescore_active_cohorts() {
        assert!(!should_refresh_current_cohorts(0, false));
        assert!(should_refresh_current_cohorts(1, false));
        assert!(should_refresh_current_cohorts(0, true));
    }

    #[test]
    fn repaired_embedding_refreshes_only_its_own_scope() {
        let repaired = BTreeSet::from([("alpha".to_string(), "prod".to_string())]);

        assert!(scope_has_repaired_embedding_bind(
            &repaired, "alpha", "prod"
        ));
        assert!(!scope_has_repaired_embedding_bind(
            &repaired, "alpha", "other"
        ));
        assert!(!scope_has_repaired_embedding_bind(
            &repaired, "beta", "prod"
        ));
    }

    fn row(
        verdict: &str,
        comment: Option<&str>,
        state: Option<&str>,
    ) -> AttentionFeedbackHistoryRow {
        AttentionFeedbackHistoryRow {
            event_id: "event".to_string(),
            annotation_id: "annotation".to_string(),
            provider: "gmail".to_string(),
            account_alias: "primary".to_string(),
            thread_id: "thread".to_string(),
            created_at: 1,
            verdict: verdict.to_string(),
            comment: comment.map(str::to_string),
            subject: Some("Quarterly close".to_string()),
            paired_to_state: state.map(str::to_string),
            paired_action: None,
        }
    }

    #[test]
    fn typed_historical_mapping_preserves_task_meaning() {
        assert_eq!(
            historical_mail_label(&row("helpful", None, Some("approved")))
                .unwrap()
                .outcome,
            AttentionOutcomeKind::ActionCompleted
        );
        assert_eq!(
            historical_mail_label(&row(
                "not_helpful",
                Some("already_handled"),
                Some("dismissed")
            ))
            .unwrap()
            .outcome,
            AttentionOutcomeKind::Obsolete
        );
        assert!(historical_mail_label(&row("other", None, None)).is_none());
    }

    #[test]
    fn deterministic_import_ids_are_scoped_and_bounded() {
        let first = historical_event_id("mail", "p", "w", "1");
        assert_eq!(first, historical_event_id("mail", "p", "w", "1"));
        assert_ne!(first, historical_event_id("mail", "p", "other", "1"));
        assert!(first.len() <= 200);
    }

    #[tokio::test]
    async fn cutoff_is_immutable_and_checkpoint_never_regresses() {
        let service = AttentionLearningService::open_in_temp(Default::default());
        let store = service.store();
        store
            .initialize_historical_bootstrap("owner", "default", &["mail"], 100)
            .await
            .unwrap();
        store
            .initialize_historical_bootstrap("owner", "default", &["mail"], 200)
            .await
            .unwrap();
        store
            .advance_historical_bootstrap_checkpoint(
                "owner", "default", "mail", 10, "b", 1, 0, false, 110,
            )
            .await
            .unwrap();
        store
            .advance_historical_bootstrap_checkpoint(
                "owner", "default", "mail", 5, "z", 1, 0, false, 120,
            )
            .await
            .unwrap();
        store
            .advance_historical_bootstrap_checkpoint(
                "owner", "default", "mail", 10, "b", 0, 0, true, 130,
            )
            .await
            .unwrap();
        let checkpoint = store
            .historical_bootstrap_checkpoint("owner", "default", "mail")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(checkpoint.cutoff_at, 100);
        assert_eq!(checkpoint.cursor_at, 10);
        assert_eq!(checkpoint.cursor_id, "b");
        assert_eq!(checkpoint.imported_count, 1);
        assert!(checkpoint.completed);
    }

    /// A workspace that was deleted must stay deleted. Its checkpoint rows
    /// outlive it by design — they record what it did — but rediscovering it
    /// here re-initializes the scope and materializes its directory again, so
    /// every later pass that scans `scopes/` sees a live scope and the removal
    /// never sticks.
    #[tokio::test]
    async fn a_scope_whose_directory_is_gone_is_not_rediscovered() {
        let root = tempfile::TempDir::new().unwrap();
        let mail_root = root.path().join("mail");
        let resurfacing_root = root.path().join("resurfacing");
        let learning_root = root.path().join("learning");
        let mut learning_config = magician::config::AttentionLearningConfig::default();
        learning_config.enabled = true;
        learning_config.historical_bootstrap.enabled = true;
        let learning = AttentionLearningService::open(&learning_root, learning_config).unwrap();
        // Two scopes with identical history; only one still has a directory.
        for workspace in ["live-workspace", "deleted-workspace"] {
            learning
                .store()
                .record_outcome(
                    "owner",
                    workspace,
                    AttentionSurface::WorthALook,
                    &RecordAttentionOutcome {
                        event_id: format!("{workspace}-event"),
                        candidate: SemanticAttentionCandidate {
                            candidate_id: format!("{workspace}-candidate"),
                            source_revision: None,
                            semantic_text: "private brief".to_string(),
                            existing_embedding: None,
                            actionability_features: None,
                            grouping_features: None,
                        },
                        outcome: AttentionOutcomeKind::Useful,
                        reason: None,
                        label_quality: AttentionLabelQuality::Strong,
                        occurred_at: 100,
                        attribution: None,
                    },
                    None,
                )
                .await
                .unwrap();
        }
        let scopes_root = root.path().join("scopes");
        std::fs::create_dir_all(scopes_root.join("owner").join("live-workspace")).unwrap();

        let worker = AttentionHistoricalBootstrapWorker::new(
            learning.clone(),
            MailAssistStore::open(&mail_root).unwrap(),
            ResurfacingStore::open(&resurfacing_root).unwrap(),
            learning.historical_bootstrap_config().clone(),
            scopes_root,
        );
        let scopes = worker.initialize_runtime_scopes(1_000).await.unwrap();

        assert!(
            scopes.contains(&("owner".to_string(), "live-workspace".to_string())),
            "a scope that still exists is discovered: {scopes:?}"
        );
        assert!(
            !scopes.contains(&("owner".to_string(), "deleted-workspace".to_string())),
            "a scope whose directory is gone must not be rediscovered: {scopes:?}"
        );
        // The default scope survives the filter even without a directory: on a
        // fresh install this pass is what prepares it.
        assert!(scopes.contains(&("anonymous".to_string(), "default".to_string())));
    }

    #[tokio::test]
    async fn production_worker_routes_late_scopes_only_through_live_repairs() {
        let root = tempfile::TempDir::new().unwrap();
        let mail_root = root.path().join("mail");
        let resurfacing_root = root.path().join("resurfacing");
        let learning_root = root.path().join("learning");
        let mut learning_config = magician::config::AttentionLearningConfig::default();
        learning_config.enabled = true;
        learning_config.historical_bootstrap.enabled = true;
        let learning = AttentionLearningService::open(&learning_root, learning_config).unwrap();
        learning
            .store()
            .record_outcome(
                "late-owner",
                "late-workspace",
                AttentionSurface::WorthALook,
                &RecordAttentionOutcome {
                    event_id: "late-live-event".to_string(),
                    candidate: SemanticAttentionCandidate {
                        candidate_id: "late-candidate".to_string(),
                        source_revision: None,
                        semantic_text: "private brief".to_string(),
                        existing_embedding: None,
                        actionability_features: None,
                        grouping_features: None,
                    },
                    outcome: AttentionOutcomeKind::Useful,
                    reason: None,
                    label_quality: AttentionLabelQuality::Strong,
                    occurred_at: 100,
                    attribution: None,
                },
                None,
            )
            .await
            .unwrap();
        // Discovery admits a scope only while it still exists on disk, so the
        // live scope under test needs its directory.
        let scopes_root = root.path().join("scopes");
        std::fs::create_dir_all(scopes_root.join("late-owner").join("late-workspace")).unwrap();
        let worker = AttentionHistoricalBootstrapWorker::new(
            learning.clone(),
            MailAssistStore::open(&mail_root).unwrap(),
            ResurfacingStore::open(&resurfacing_root).unwrap(),
            learning.historical_bootstrap_config().clone(),
            scopes_root.clone(),
        );

        let scopes = worker.initialize_runtime_scopes(1_000).await.unwrap();
        assert!(scopes.contains(&("late-owner".to_string(), "late-workspace".to_string())));
        for source in HISTORICAL_BOOTSTRAP_SOURCES {
            let checkpoint = learning
                .store()
                .historical_bootstrap_checkpoint("late-owner", "late-workspace", source)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(checkpoint.cutoff_at, 0);
            assert!(checkpoint.completed);
        }
        let live = learning
            .store()
            .historical_bootstrap_checkpoint(
                "late-owner",
                "late-workspace",
                LIVE_WORTH_FEEDBACK_REPAIR_SOURCE,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(live.cutoff_at, 0);
        assert!(!live.completed);
    }
}
