//! Evidence-preserving, owner-scoped attention learning.
//!
//! Slice 1 records one canonical outcome vocabulary for Follow-ups and Worth a
//! look, reuses/version-checks semantic embeddings, and computes Bayesian kNN
//! usefulness/actionability estimates. Candidate lifecycle remains owned by
//! the source stores: this module changes order only and never deletes or
//! transitions a candidate.
//!
//! Plan workstream 3.0 relocated this engine lib-side. The three mail-fed
//! workers (historical bootstrap, semantic extraction backfill, and the
//! rank recompute worker that reads the canonical attention union) stayed
//! in `magician-comms`'s `channel_assist::attention_learning` module; its
//! Phase 5 glob re-export of this tree was removed (batch 5 of the
//! 2026-08-28 removal inventory), so this module is the only import root
//! for the engine types.

pub mod actionability;
pub mod bandit;
pub mod delivery;
pub mod grouping;
mod model;
pub mod rank_recompute;
pub mod routing;
mod store;
pub mod training;

use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
    time::Duration,
};

use anyhow::Result;
use magician_vector_index::OllamaEmbedder;
use serde::{Deserialize, Serialize};
use tokio::time::timeout;

use crate::config::{
    AttentionActionabilityMode, AttentionBanditMode, AttentionGroupingMode,
    AttentionLearningConfig, AttentionRoutingMode,
};
use crate::magician_v2::runtime::ollama_lifecycle;

pub use actionability::{
    actionability_input_digest, deserialize_semantic_envelope, feature_vector, infer_actionability,
    serialize_semantic_envelope, ActionabilityExplanation, ActionabilityFeatureInput,
    ActionabilityFeatureVector, ActionabilityInference, ActionabilityModelSnapshot,
    ActionabilityTrainingManifest, ChannelAttentionSemanticEnvelope,
    ChannelAttentionSemanticFeatures, CommunicationType, RequestedAction, SemanticActionOwner,
    SemanticDeadline, SemanticDeadlineKind, SemanticExtractionStatus, SemanticExtractorIdentity,
    ACTIONABILITY_FEATURE_CONTRACT, ATTENTION_SEMANTIC_EXTRACTOR_CONTRACT,
    ATTENTION_SEMANTIC_SCHEMA_VERSION, ATTENTION_TEMPORAL_FEATURE_CONTRACT,
};
pub use bandit::{
    deterministic_bandit_canary_assignment, extract_bandit_features,
    finite_probability_matching_rank, AttentionBanditAttributionQuality, AttentionBanditCandidate,
    AttentionBanditDecisionMetadata, AttentionBanditHealth, AttentionBanditPolicySnapshot,
    AttentionBanditPosteriorState, AttentionBanditProjection, AttentionBanditRewardSpec,
    AttentionBanditTrainingManifest, AttentionOutcomeAttribution, AttentionPosteriorUpdateReceipt,
    AttentionPosteriorUpdateStatus, ATTENTION_BANDIT_FEATURE_CONTRACT,
    ATTENTION_BANDIT_SCHEMA_VERSION, ATTENTION_BANDIT_SEED_CONTRACT,
};
pub use delivery::{
    AttentionDeliveryCandidate, AttentionDeliveryHealth, AttentionDeliveryLedgerHealth,
    AttentionDeliveryOrderItem, AttentionDeliveryOrderPlan, AttentionDeliveryPage,
    AttentionDeliveryReadError, AttentionDeliveryRefreshReason, AttentionDeliveryRootDecision,
    AttentionDeliveryStatus, CreateAttentionDelivery, FrozenAttentionDelivery,
    FrozenAttentionDeliveryItem, ATTENTION_DELIVERY_SCHEMA_VERSION,
};
pub use grouping::{
    canonical_pair, cluster_candidates, infer_pair, required_pair_evaluations, singleton_grouping,
    singleton_grouping_metadata, AttentionCluster, AttentionGroupingHealth,
    AttentionGroupingMetadata, AttentionGroupingProjection, AttentionPairCandidateRef,
    AttentionPairFeedbackReceipt, AttentionPairInference, AttentionPairLabelKind,
    AttentionPairLabelSource, AttentionPairModelHead, AttentionPairModelSnapshot,
    AttentionPairTrainingManifest, GroupingCandidate, GroupingFeatureInput, PersistedPairEvidence,
    RecordAttentionPairLabel, ATTENTION_PAIR_FEATURE_CONTRACT, ATTENTION_PAIR_LABEL_SCHEMA_VERSION,
};
pub use model::{
    evaluate_top_k, BayesianKnnEstimate, BayesianKnnEvaluator, BayesianKnnLabel,
    RankingReplayMetrics,
};
pub use rank_recompute::{
    rank_recompute_result_semantics, AttentionRankRecomputeEnqueueStatus,
    AttentionRankRecomputeGeneration, AttentionRankRecomputeJob, AttentionRankRecomputePauseReason,
    AttentionRankRecomputeQueueCounts, AttentionRankRecomputeReference,
    AttentionRankRecomputeResult, AttentionRankRecomputeRunReport, AttentionRankRecomputeStatus,
    ScheduleAttentionRankRecompute, ATTENTION_RANK_RECOMPUTE_REASON_MAX_CHARS,
    ATTENTION_RANK_RECOMPUTE_RESULT_SEMANTICS, ATTENTION_RANK_RECOMPUTE_SCHEMA_VERSION,
    ATTENTION_RANK_RECOMPUTE_SERVED_UNIVERSE_SEMANTICS,
    ATTENTION_RANK_RECOMPUTE_WRONGLY_STALED_REASON,
};
pub use routing::{
    deterministic_canary_assignment, evaluate_routing, routing_feature_vector, AttentionDecision,
    AttentionDecisionContext, AttentionDecisionDetail, AttentionDecisionFeatureContracts,
    AttentionDecisionItem, AttentionImpressionError, AttentionImpressionReceipt, AttentionRoute,
    AttentionRoutingCandidate, AttentionRoutingEvaluation, AttentionRoutingHealth,
    AttentionRoutingPolicySnapshot, AttentionRoutingTrainingManifest, AttentionUtilityHead,
    RecordAttentionImpression, RoutingFeatureVector, ATTENTION_CANDIDATE_ID_MAX_CHARS,
    ATTENTION_CLIENT_TYPE_MAX_CHARS, ATTENTION_CLIENT_VERSION_MAX_CHARS,
    ATTENTION_DECISION_ID_MAX_CHARS, ATTENTION_EVENT_ID_MAX_CHARS,
    ATTENTION_IMPRESSION_SCHEMA_VERSION, ATTENTION_MIN_VISIBLE_MS_MAX,
    ATTENTION_ROUTING_DECISION_SCHEMA_VERSION, ATTENTION_ROUTING_FEATURE_CONTRACT,
    ATTENTION_SOURCE_REVISION_MAX_CHARS, ATTENTION_VIEWPORT_CLASS_MAX_CHARS,
    ATTENTION_VISIBILITY_RULE_MAX_CHARS, ATTENTION_VISIBLE_MS_MAX,
};
pub use store::RoutingTrainingRow;
pub use store::{
    ActionabilityScopeInstall, ActionabilityTrainingRow, AttentionConnectionTelemetry,
    AttentionEmbeddingBindQueueCounts, AttentionLearningStore, AttentionOptimizeReport,
    AttentionReclaimReport, AttentionRetentionReport, AttentionScorePosterior,
    AttentionTrainingRunRecord, AttentionWriterBusy, BanditScopeInstall,
    CanonicalProjectionLanePage, HistoricalBootstrapCheckpoint, PersistedActionabilityScore,
    PersistedAttentionOutcome, PersistedAttentionPairLabel, ScheduleSemanticExtraction,
    SemanticExtractionCheckpoint, SemanticExtractionContract, SemanticExtractionQueueCounts,
    SemanticExtractionWorkItem, SemanticExtractionWorkStatus, REQUEST_PATH_WRITER_WAIT,
};
pub use training::{
    train_actionability, train_bandit, train_routing, ActionabilityTrainingConfig,
    AttentionActionabilityTrainingWorker, BanditTrainingConfig, LabelSet, RoutingTrainingConfig,
    TrainingMetrics, TrainingOutcome, MIN_CANARY_POSTERIOR_UPDATES,
};

#[derive(Debug, Clone)]
pub struct ResolvedActionability {
    pub mode: AttentionActionabilityMode,
    pub snapshot: Option<ActionabilityModelSnapshot>,
}

impl ResolvedActionability {
    const fn disabled() -> Self {
        Self {
            mode: AttentionActionabilityMode::Disabled,
            snapshot: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedBandit {
    pub mode: AttentionBanditMode,
    pub snapshot: Option<AttentionBanditPolicySnapshot>,
    pub apply_canary: bool,
}

impl ResolvedBandit {
    const fn disabled() -> Self {
        Self {
            mode: AttentionBanditMode::Disabled,
            snapshot: None,
            apply_canary: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ActionabilityTrainingStatus {
    pub enabled: bool,
    pub auto_install: bool,
    pub interval_secs: u64,
    pub last_status: Option<String>,
    pub last_reason: Option<String>,
    pub usable: Option<usize>,
    pub unlinked: Option<usize>,
    pub label_count: Option<usize>,
    pub positive: Option<usize>,
    pub negative: Option<usize>,
    pub auc: Option<f64>,
    pub ece: Option<f64>,
    pub last_snapshot_id: Option<String>,
    pub installed_snapshot_id: Option<String>,
    pub effective_mode: String,
    pub last_run_at: Option<i64>,
}

pub const ATTENTION_OUTCOME_SCHEMA_VERSION: u32 = 1;
pub const ATTENTION_RANK_POLICY_VERSION: &str = "semantic_bayesian_knn_v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionSurface {
    FollowUp,
    WorthALook,
}

impl AttentionSurface {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FollowUp => "follow_up",
            Self::WorthALook => "worth_a_look",
        }
    }

    /// Canonical decision-ledger identity for a surface-local candidate id.
    ///
    /// Decision items, outcomes, and rank-recompute jobs are all keyed by this
    /// surface-qualified form. A raw surface-local id never matches a ledger
    /// row, so every lookup that starts from a raw id must normalize here.
    pub fn canonical_candidate_id(self, raw_candidate_id: &str) -> String {
        format!("{}:{}", self.as_str(), raw_candidate_id)
    }
}

impl std::str::FromStr for AttentionSurface {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "follow_up" => Ok(Self::FollowUp),
            "worth_a_look" => Ok(Self::WorthALook),
            other => anyhow::bail!("unknown attention surface: {other}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionOutcomeKind {
    Useful,
    ActionCompleted,
    Irrelevant,
    NotActionable,
    Obsolete,
    NotOwner,
    DuplicateOf,
    NeutralSeen,
    TimingNegative,
}

impl AttentionOutcomeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Useful => "useful",
            Self::ActionCompleted => "action_completed",
            Self::Irrelevant => "irrelevant",
            Self::NotActionable => "not_actionable",
            Self::Obsolete => "obsolete",
            Self::NotOwner => "not_owner",
            Self::DuplicateOf => "duplicate_of",
            Self::NeutralSeen => "neutral_seen",
            Self::TimingNegative => "timing_negative",
        }
    }

    /// Training targets remain task-specific. In particular, neutral, timing,
    /// and duplicate identity outcomes never become relevance negatives.
    pub const fn usefulness_target(self) -> Option<bool> {
        match self {
            Self::Useful | Self::ActionCompleted => Some(true),
            Self::Irrelevant => Some(false),
            Self::NotActionable
            | Self::Obsolete
            | Self::NotOwner
            | Self::DuplicateOf
            | Self::NeutralSeen
            | Self::TimingNegative => None,
        }
    }

    pub const fn actionability_target(self) -> Option<bool> {
        match self {
            Self::ActionCompleted => Some(true),
            Self::Irrelevant | Self::NotActionable | Self::Obsolete | Self::NotOwner => Some(false),
            Self::Useful | Self::DuplicateOf | Self::NeutralSeen | Self::TimingNegative => None,
        }
    }

    /// Lane corrections, not ranking. Useful never chooses a lane.
    /// `not_actionable` is "this should not have been For you";
    /// `action_completed` is "this is owner work" (including Worth-a-look
    /// "This needs me").
    pub const fn routing_target(self) -> Option<AttentionRoute> {
        match self {
            Self::NotActionable => Some(AttentionRoute::WorthALook),
            Self::ActionCompleted => Some(AttentionRoute::FollowUp),
            Self::Useful
            | Self::Irrelevant
            | Self::Obsolete
            | Self::NotOwner
            | Self::DuplicateOf
            | Self::NeutralSeen
            | Self::TimingNegative => None,
        }
    }
}

impl std::str::FromStr for AttentionOutcomeKind {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "useful" => Ok(Self::Useful),
            "action_completed" => Ok(Self::ActionCompleted),
            "irrelevant" => Ok(Self::Irrelevant),
            "not_actionable" => Ok(Self::NotActionable),
            "obsolete" => Ok(Self::Obsolete),
            "not_owner" => Ok(Self::NotOwner),
            "duplicate_of" => Ok(Self::DuplicateOf),
            "neutral_seen" => Ok(Self::NeutralSeen),
            "timing_negative" => Ok(Self::TimingNegative),
            other => anyhow::bail!("unknown attention outcome: {other}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionLabelQuality {
    Strong,
    Weak,
    Unknown,
}

impl AttentionLabelQuality {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Strong => "strong",
            Self::Weak => "weak",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SemanticAttentionCandidate {
    pub candidate_id: String,
    pub source_revision: Option<String>,
    /// Sanitized brief/summary fields only. Normally transient; when embedding
    /// is unavailable, a bounded copy is persisted in the durable repair queue
    /// and deleted after the outcome is contract-bound.
    pub semantic_text: String,
    /// Existing contract-qualified vector supplied by a source store.
    pub existing_embedding: Option<SemanticEmbedding>,
    /// Revision-bound structured features from the existing classifier call.
    /// `None`, invalid, or stale extraction always falls back to Slice 1.
    pub actionability_features: Option<ActionabilityFeatureInput>,
    /// Safe structured pair features. Raw bodies and title/token heuristics are
    /// deliberately excluded from Slice-3 grouping.
    pub grouping_features: Option<GroupingFeatureInput>,
}

/// Origin binding for canonical-union ranking. `candidate.candidate_id` is the
/// collision-proof projection identity; evidence/cache reads continue to use
/// the exact source-owned surface and raw lifecycle id so past owner labels
/// remain effective after union materialization.
#[derive(Debug, Clone, Copy)]
pub struct CanonicalAttentionRankCandidate<'a> {
    pub candidate: &'a SemanticAttentionCandidate,
    pub evidence_surface: AttentionSurface,
    pub evidence_candidate_id: &'a str,
}

#[derive(Debug, Clone)]
pub struct SemanticEmbedding {
    pub contract: String,
    pub vector: Vec<f32>,
}

#[derive(Debug, Clone)]
pub struct RecordAttentionOutcome {
    pub event_id: String,
    pub candidate: SemanticAttentionCandidate,
    pub outcome: AttentionOutcomeKind,
    pub reason: Option<String>,
    pub label_quality: AttentionLabelQuality,
    pub occurred_at: i64,
    /// Optional causal attribution. Canonical outcome capture never depends on
    /// this being present or valid; only the posterior update does.
    pub attribution: Option<AttentionOutcomeAttribution>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionRescoreStatus {
    Completed,
    Disabled,
    DegradedNoEmbedding,
    DegradedNoCandidates,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttentionFeedbackReceipt {
    pub outcome_id: String,
    pub outcome: AttentionOutcomeKind,
    pub surface: AttentionSurface,
    pub feedback_recorded: bool,
    pub affected_candidates: usize,
    pub rescore_status: AttentionRescoreStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_contract: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic_href: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub posterior_update: Option<AttentionPosteriorUpdateReceipt>,
    /// Explicitly null only when no canonical outcome was committed. A
    /// recorded outcome returns an enqueued job or a typed fail-soft enqueue
    /// result; rank-after remains null until the job reaches `succeeded`.
    pub rank_recompute: Option<AttentionRankRecomputeReference>,
}

// `PartialEq` (without `Eq`, since scores are floats) so this can travel inside
// the canonical projection diagnostics, which compare structurally.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionRankMetadata {
    pub candidate_id: String,
    pub baseline_rank: usize,
    pub learned_rank: usize,
    pub rank_delta: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learning_score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slice1_actionability_probability: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slice1_actionability_weight: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actionability_probability: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actionability_explanation: Option<ActionabilityExplanation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actionability_model_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actionability_snapshot_id: Option<String>,
    pub semantic_feature_status: SemanticExtractionStatus,
    pub actionability_score_status: ActionabilityScoreStatus,
    pub actionability_mode: AttentionActionabilityMode,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionabilityScoreStatus {
    Scored,
    Fallback,
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttentionLearningHealth {
    pub total_active: usize,
    pub source_family_counts: std::collections::BTreeMap<String, u64>,
    pub embedded_candidates: usize,
    pub embedding_coverage: f64,
    pub learned_rank_changes: usize,
    /// `active_cohort` makes the coverage denominator explicit: Slice 1 embeds
    /// the bounded eligible cohort passed to the learner, not hidden long-tail
    /// inventory outside that cohort.
    pub coverage_scope: String,
    pub rank_policy_version: String,
    pub semantic_ranking_enabled: bool,
    pub actionability_mode: AttentionActionabilityMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actionability_snapshot_id: Option<String>,
    pub semantic_extraction_coverage: f64,
    pub actionability_scored_count: usize,
    pub actionability_fallback_count: usize,
}

#[derive(Debug, Clone)]
pub struct AttentionGroupingResult {
    pub generation: u64,
    pub mode: AttentionGroupingMode,
    pub snapshot_id: Option<String>,
    pub projection: AttentionGroupingProjection,
    pub health: AttentionGroupingHealth,
}

#[derive(Clone)]
pub struct AttentionLearningService {
    store: AttentionLearningStore,
    config: AttentionLearningConfig,
}

impl AttentionLearningService {
    pub fn open(base_root: &Path, config: AttentionLearningConfig) -> Result<Self> {
        validate_config(&config)?;
        Ok(Self {
            store: AttentionLearningStore::open(base_root)?,
            config,
        })
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub fn open_in_temp(config: AttentionLearningConfig) -> Self {
        let directory = tempfile::TempDir::new().expect("creating attention learning temp dir");
        let path = directory.keep();
        Self::open(&path, config).expect("opening attention learning temp store")
    }

    pub const fn semantic_ranking_enabled(&self) -> bool {
        self.config.semantic_ranking_enabled
    }

    pub const fn routing_training_enabled(&self) -> bool {
        self.config.routing.training.enabled
    }

    pub const fn routing_training_auto_install(&self) -> bool {
        self.config.routing.training.auto_install
    }

    pub const fn bandit_training_enabled(&self) -> bool {
        self.config.bandit.training.enabled
    }

    pub const fn bandit_training_auto_install(&self) -> bool {
        self.config.bandit.training.auto_install
    }

    pub fn store(&self) -> AttentionLearningStore {
        self.store.clone()
    }

    pub const fn semantic_backfill_enabled(&self) -> bool {
        self.config.semantic_backfill.enabled
    }

    pub fn semantic_backfill_config(&self) -> &crate::config::AttentionSemanticBackfillConfig {
        &self.config.semantic_backfill
    }

    pub const fn rank_recompute_enabled(&self) -> bool {
        self.config.rank_recompute.enabled
    }

    pub fn rank_recompute_config(&self) -> &crate::config::AttentionRankRecomputeConfig {
        &self.config.rank_recompute
    }

    pub fn historical_bootstrap_config(
        &self,
    ) -> &crate::config::AttentionHistoricalBootstrapConfig {
        &self.config.historical_bootstrap
    }

    pub fn bandit_snapshot_id(&self) -> Option<&str> {
        self.config.bandit.snapshot_id.as_deref()
    }

    pub async fn current_rank_recompute_generation(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<AttentionRankRecomputeGeneration> {
        let follow = self
            .store
            .rank_generation(principal, workspace, AttentionSurface::FollowUp)
            .await?;
        let worth = self
            .store
            .rank_generation(principal, workspace, AttentionSurface::WorthALook)
            .await?;
        Ok(AttentionRankRecomputeGeneration {
            follow_up: follow,
            worth_a_look: worth,
        })
    }

    /// Schedule-only boundary used by source creation/revision hooks and admin
    /// tools. Scheduling never calls a model and remains useful while the
    /// worker is disabled so an operator can preview durable coverage work.
    pub async fn schedule_semantic_extraction(
        &self,
        principal: &str,
        workspace: &str,
        request: &ScheduleSemanticExtraction,
        now: i64,
    ) -> Result<bool> {
        self.store
            .schedule_semantic_extraction(principal, workspace, request, now)
            .await
    }

    pub async fn semantic_extraction_queue_counts(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<SemanticExtractionQueueCounts> {
        self.store
            .semantic_extraction_queue_counts(principal, workspace)
            .await
    }

    pub async fn semantic_extraction_checkpoints(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<SemanticExtractionCheckpoint>> {
        self.store
            .semantic_extraction_checkpoints(principal, workspace)
            .await
    }

    pub async fn semantic_extraction_work_items(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<Vec<SemanticExtractionWorkItem>> {
        self.store
            .semantic_extraction_work_items(principal, workspace)
            .await
    }

    /// Whether a learned ordering may be served now. Slice-1 enforcement is
    /// independent; Slice-2 enforcement activates only after the configured
    /// immutable snapshot can be loaded and validated.
    pub async fn serving_semantic_ranking_enabled(&self) -> bool {
        self.config.semantic_ranking_enabled
            || self.effective_actionability_mode().await == AttentionActionabilityMode::Enforced
            || self.effective_grouping_mode().await == AttentionGroupingMode::Enforced
    }

    pub async fn serving_semantic_ranking_enabled_for(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<bool> {
        if self.config.semantic_ranking_enabled {
            return Ok(true);
        }
        let actionability = self.resolve_actionability(principal, workspace).await?;
        Ok(actionability.mode == AttentionActionabilityMode::Enforced
            || self.effective_grouping_mode().await == AttentionGroupingMode::Enforced)
    }

    pub async fn serving_ranking_without_grouping_enabled(&self) -> bool {
        self.config.semantic_ranking_enabled
            || self.effective_actionability_mode().await == AttentionActionabilityMode::Enforced
    }

    pub async fn serving_ranking_without_grouping_enabled_for(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<bool> {
        if self.config.semantic_ranking_enabled {
            return Ok(true);
        }
        Ok(self.resolve_actionability(principal, workspace).await?.mode
            == AttentionActionabilityMode::Enforced)
    }

    pub const fn actionability_mode(&self) -> AttentionActionabilityMode {
        self.config.actionability.mode
    }

    pub async fn effective_actionability_mode(&self) -> AttentionActionabilityMode {
        let configured = self.config.actionability.mode;
        if configured == AttentionActionabilityMode::Disabled {
            return AttentionActionabilityMode::Disabled;
        }
        if self.configured_actionability_snapshot().await.is_some() {
            configured
        } else {
            AttentionActionabilityMode::Disabled
        }
    }

    /// YAML pin wins when it actually loads. A missing pin falls through to
    /// Magician's own scope install instead of disabling a trained snapshot.
    pub async fn resolve_actionability(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<ResolvedActionability> {
        let configured = self.config.actionability.mode;
        if configured != AttentionActionabilityMode::Disabled {
            if let Some(snapshot) = self.configured_actionability_snapshot().await {
                return Ok(ResolvedActionability {
                    mode: configured,
                    snapshot: Some(snapshot),
                });
            }
        }
        let Some(install) = self
            .store
            .actionability_scope_install(principal, workspace)
            .await?
        else {
            return Ok(ResolvedActionability::disabled());
        };
        if install.effective_mode == AttentionActionabilityMode::Disabled {
            return Ok(ResolvedActionability::disabled());
        }
        match self
            .store
            .get_actionability_snapshot(&install.snapshot_id)
            .await
        {
            Ok(Some(snapshot)) => Ok(ResolvedActionability {
                mode: install.effective_mode,
                snapshot: Some(snapshot),
            }),
            Ok(None) => {
                tracing::warn!(
                    principal,
                    workspace,
                    snapshot_id = install.snapshot_id.as_str(),
                    "scope-installed actionability snapshot is missing; preserving prior ordering"
                );
                Ok(ResolvedActionability::disabled())
            },
            Err(error) => {
                tracing::warn!(
                    principal,
                    workspace,
                    snapshot_id = install.snapshot_id.as_str(),
                    error = %error,
                    "scope-installed actionability snapshot is unavailable; preserving prior ordering"
                );
                Ok(ResolvedActionability::disabled())
            },
        }
    }

    pub async fn actionability_training_status(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<ActionabilityTrainingStatus> {
        let training = &self.config.actionability.training;
        let last = self
            .store
            .latest_training_run(principal, workspace, "actionability")
            .await?;
        let metrics = last
            .as_ref()
            .and_then(|run| run.metrics_json.as_deref())
            .and_then(|raw| serde_json::from_str::<TrainingMetrics>(raw).ok());
        let resolved = self.resolve_actionability(principal, workspace).await?;
        Ok(ActionabilityTrainingStatus {
            enabled: training.enabled,
            auto_install: training.auto_install,
            interval_secs: training.interval_secs,
            last_status: last.as_ref().map(|run| run.status.clone()),
            last_reason: last.as_ref().and_then(|run| run.reason.clone()),
            usable: metrics.as_ref().map(|value| value.usable),
            unlinked: metrics.as_ref().map(|value| value.unlinked),
            label_count: metrics.as_ref().map(|value| value.label_count),
            positive: metrics.as_ref().map(|value| value.positive),
            negative: metrics.as_ref().map(|value| value.negative),
            auc: metrics.as_ref().map(|value| value.auc),
            ece: metrics.as_ref().map(|value| value.ece),
            last_snapshot_id: last.as_ref().and_then(|run| run.snapshot_id.clone()),
            installed_snapshot_id: resolved
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.snapshot_id.clone()),
            effective_mode: resolved.mode.as_str().to_string(),
            last_run_at: last.as_ref().map(|run| run.created_at),
        })
    }

    pub async fn routing_training_status(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<ActionabilityTrainingStatus> {
        let training = &self.config.routing.training;
        let last = self
            .store
            .latest_training_run(principal, workspace, "routing")
            .await?;
        let metrics = last
            .as_ref()
            .and_then(|run| run.metrics_json.as_deref())
            .and_then(|raw| serde_json::from_str::<TrainingMetrics>(raw).ok());
        let (mode, snapshot) = self.resolve_routing(principal, workspace).await?;
        Ok(ActionabilityTrainingStatus {
            enabled: training.enabled,
            auto_install: training.auto_install,
            interval_secs: training.interval_secs,
            last_status: last.as_ref().map(|run| run.status.clone()),
            last_reason: last
                .as_ref()
                .and_then(|run| run.reason.clone())
                .or_else(|| {
                    last.is_none().then(|| {
                        "watching lane corrections from shouldn't-have-been-flagged and this-needs-me"
                            .to_string()
                    })
                }),
            usable: metrics.as_ref().map(|value| value.usable),
            unlinked: metrics.as_ref().map(|value| value.unlinked),
            label_count: metrics.as_ref().map(|value| value.label_count),
            positive: metrics.as_ref().map(|value| value.positive),
            negative: metrics.as_ref().map(|value| value.negative),
            auc: metrics.as_ref().map(|value| value.auc),
            ece: metrics.as_ref().map(|value| value.ece),
            last_snapshot_id: last.as_ref().and_then(|run| run.snapshot_id.clone()),
            installed_snapshot_id: snapshot.map(|value| value.snapshot_id),
            effective_mode: mode.as_str().to_string(),
            last_run_at: last.as_ref().map(|run| run.created_at),
        })
    }

    async fn configured_actionability_snapshot(&self) -> Option<ActionabilityModelSnapshot> {
        let snapshot_id = self.config.actionability.snapshot_id.as_deref()?;
        match self.store.get_actionability_snapshot(snapshot_id).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::warn!(
                    snapshot_id,
                    error = %error,
                    "pinned actionability snapshot is unavailable; preserving prior ordering"
                );
                None
            },
        }
    }

    pub fn actionability_snapshot_id(&self) -> Option<&str> {
        self.config.actionability.snapshot_id.as_deref()
    }

    pub async fn install_actionability_snapshot(
        &self,
        snapshot: &ActionabilityModelSnapshot,
    ) -> Result<()> {
        self.store.install_actionability_snapshot(snapshot).await
    }

    pub async fn install_actionability_snapshot_for_scope(
        &self,
        principal: &str,
        workspace: &str,
        snapshot: &ActionabilityModelSnapshot,
        requested: AttentionActionabilityMode,
    ) -> Result<AttentionActionabilityMode> {
        self.store
            .install_actionability_snapshot_for_scope(principal, workspace, snapshot, requested)
            .await
    }

    pub const fn grouping_mode(&self) -> AttentionGroupingMode {
        self.config.grouping.mode
    }

    pub fn grouping_snapshot_id(&self) -> Option<&str> {
        self.config.grouping.snapshot_id.as_deref()
    }

    async fn configured_pair_model_snapshot(&self) -> Option<AttentionPairModelSnapshot> {
        let snapshot_id = self.config.grouping.snapshot_id.as_deref()?;
        match self.store.get_pair_model_snapshot(snapshot_id).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::warn!(
                    snapshot_id,
                    error = %error,
                    "pinned pair-model snapshot is unavailable; preserving ungrouped surface"
                );
                None
            },
        }
    }

    pub async fn effective_grouping_mode(&self) -> AttentionGroupingMode {
        let configured = self.config.grouping.mode;
        if configured == AttentionGroupingMode::Disabled {
            return AttentionGroupingMode::Disabled;
        }
        if self.configured_pair_model_snapshot().await.is_some() {
            configured
        } else {
            AttentionGroupingMode::Disabled
        }
    }

    pub async fn install_pair_model_snapshot(
        &self,
        snapshot: &AttentionPairModelSnapshot,
    ) -> Result<()> {
        self.store.install_pair_model_snapshot(snapshot).await
    }

    pub const fn routing_mode(&self) -> AttentionRoutingMode {
        self.config.routing.mode
    }

    pub fn routing_snapshot_id(&self) -> Option<&str> {
        self.config.routing.snapshot_id.as_deref()
    }

    pub fn routing_seed_identity(&self) -> &str {
        &self.config.routing.seed_identity
    }

    pub const fn routing_canary_fraction(&self) -> f64 {
        self.config.routing.canary_fraction
    }

    /// Version identity for every lane-local evidence generation consumed by
    /// the canonical projector. This makes a newly recorded owner label break
    /// the projection cache even when no source payload revision changed.
    pub async fn canonical_projection_learning_identity(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<String> {
        let [follow_rank, worth_rank, follow_group, worth_group] = self
            .store
            .canonical_projection_generations(principal, workspace)
            .await?;
        Ok(format!(
            "follow-rank:{follow_rank}:worth-rank:{worth_rank}:follow-group:{follow_group}:worth-group:{worth_group}"
        ))
    }

    pub const fn routing_min_visible_ms(&self) -> u64 {
        self.config.routing.min_visible_ms
    }

    pub fn routing_visibility_rule_version(&self) -> &str {
        &self.config.routing.visibility_rule_version
    }

    pub const fn delivery_default_page_size(&self) -> usize {
        self.config.bandit.delivery_default_page_size
    }

    pub const fn delivery_max_page_size(&self) -> usize {
        self.config.bandit.delivery_max_page_size
    }

    pub const fn delivery_ttl_secs(&self) -> u64 {
        self.config.bandit.delivery_ttl_secs
    }

    pub const fn delivery_retention_days(&self) -> u64 {
        self.config.bandit.delivery_retention_days
    }

    async fn configured_routing_policy_snapshot(&self) -> Option<AttentionRoutingPolicySnapshot> {
        let snapshot_id = self.config.routing.snapshot_id.as_deref()?;
        match self.store.get_routing_policy_snapshot(snapshot_id).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::warn!(
                    snapshot_id,
                    error = %error,
                    "pinned routing snapshot is unavailable; preserving baseline lanes"
                );
                None
            },
        }
    }

    pub async fn effective_routing_mode(&self) -> AttentionRoutingMode {
        if self.config.routing.mode == AttentionRoutingMode::Baseline {
            return AttentionRoutingMode::Baseline;
        }
        if self.configured_routing_policy_snapshot().await.is_some() {
            self.config.routing.mode
        } else {
            AttentionRoutingMode::Baseline
        }
    }

    pub async fn install_routing_policy_snapshot(
        &self,
        snapshot: &AttentionRoutingPolicySnapshot,
    ) -> Result<()> {
        self.store.install_routing_policy_snapshot(snapshot).await
    }

    async fn configured_bandit_policy_snapshot(&self) -> Option<AttentionBanditPolicySnapshot> {
        let snapshot_id = self.config.bandit.snapshot_id.as_deref()?;
        match self.store.get_bandit_policy_snapshot(snapshot_id).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::warn!(
                    snapshot_id,
                    error = %error,
                    "pinned bandit snapshot is unavailable; preserving baseline order"
                );
                None
            },
        }
    }

    pub async fn install_bandit_policy_snapshot(
        &self,
        snapshot: &AttentionBanditPolicySnapshot,
    ) -> Result<()> {
        self.store.install_bandit_policy_snapshot(snapshot).await
    }

    /// YAML pin wins when it loads. Otherwise Magician's own scope install
    /// can shadow (learn, do not reorder) or canary (reorder first page).
    pub async fn resolve_bandit(&self, principal: &str, workspace: &str) -> Result<ResolvedBandit> {
        let configured = self.config.bandit.mode;
        if configured != AttentionBanditMode::Disabled {
            if let Some(snapshot) = self.configured_bandit_policy_snapshot().await {
                let apply_canary = configured == AttentionBanditMode::Canary
                    && deterministic_bandit_canary_assignment(
                        self.config.bandit.canary_fraction,
                        &snapshot,
                        principal,
                        workspace,
                        AttentionSurface::FollowUp,
                    );
                return Ok(ResolvedBandit {
                    mode: configured,
                    snapshot: Some(snapshot),
                    apply_canary,
                });
            }
        }
        let Some(install) = self
            .store
            .bandit_scope_install(principal, workspace)
            .await?
        else {
            return Ok(ResolvedBandit::disabled());
        };
        if install.effective_mode == AttentionBanditMode::Disabled {
            return Ok(ResolvedBandit::disabled());
        }
        match self
            .store
            .get_bandit_policy_snapshot(&install.snapshot_id)
            .await
        {
            Ok(Some(snapshot)) => Ok(ResolvedBandit {
                apply_canary: install.effective_mode == AttentionBanditMode::Canary,
                mode: install.effective_mode,
                snapshot: Some(snapshot),
            }),
            Ok(None) => {
                tracing::warn!(
                    principal,
                    workspace,
                    snapshot_id = install.snapshot_id.as_str(),
                    "scope-installed bandit snapshot is missing; preserving baseline order"
                );
                Ok(ResolvedBandit::disabled())
            },
            Err(error) => {
                tracing::warn!(
                    principal,
                    workspace,
                    snapshot_id = install.snapshot_id.as_str(),
                    error = %error,
                    "scope-installed bandit snapshot is unavailable; preserving baseline order"
                );
                Ok(ResolvedBandit::disabled())
            },
        }
    }

    /// Freeze one lane-wide root order. The complete ordered sequence and its
    /// realized conditional policy propensities are returned once; page
    /// materialization is a separate deterministic store operation.
    pub async fn plan_attention_delivery_order(
        &self,
        principal: &str,
        workspace: &str,
        lane: AttentionSurface,
        detail: &AttentionDecisionDetail,
    ) -> Result<AttentionDeliveryOrderPlan> {
        let route = match lane {
            AttentionSurface::FollowUp => AttentionRoute::FollowUp,
            AttentionSurface::WorthALook => AttentionRoute::WorthALook,
        };
        let mut lane_items: Vec<_> = detail
            .items
            .iter()
            .filter(|item| item.served_route == route && item.served_rank > 0)
            .collect();
        lane_items.sort_by_key(|item| item.served_rank);
        let decision_id = uuid::Uuid::new_v4().to_string();
        let delivery_context = AttentionDecisionContext {
            queue_size: lane_items.len(),
            ..detail.decision.context.clone()
        };
        let baseline_plan = |reason: &str,
                             mode: AttentionBanditMode,
                             policy_snapshot_id: Option<String>,
                             policy_model_version: Option<String>,
                             posterior_version: u64,
                             seed_identity: String,
                             policy_snapshot_json: Option<String>| {
            AttentionDeliveryOrderPlan {
                status: AttentionDeliveryStatus::BaselineFallback,
                fallback_reason: Some(reason.to_string()),
                decision_id: decision_id.clone(),
                policy_snapshot_id,
                policy_model_version,
                posterior_version,
                seed_identity,
                ordered_items: lane_items
                    .iter()
                    .map(|item| AttentionDeliveryOrderItem {
                        candidate_id: item.candidate_id.clone(),
                        source_revision: item.source_revision.clone(),
                        root_policy_propensity: 1.0,
                        attribution_item: Some((**item).clone()),
                    })
                    .collect(),
                health: AttentionDeliveryHealth {
                    bandit_mode: mode,
                    canary_assigned: false,
                    applied: false,
                    baseline_preserved: true,
                    complete_universe_recorded: true,
                    propensity_coverage: if lane_items.is_empty() { 0.0 } else { 1.0 },
                    degradation_reason: Some(reason.to_string()),
                    root_sample_count: 0,
                    delivered_count: 0,
                    remaining_count: lane_items.len(),
                    exact_revision_match: true,
                    replay: false,
                },
                policy_snapshot_json,
                context: delivery_context.clone(),
            }
        };
        let resolved = self.resolve_bandit(principal, workspace).await?;
        if resolved.mode == AttentionBanditMode::Disabled {
            return Ok(baseline_plan(
                "bandit_disabled",
                AttentionBanditMode::Disabled,
                None,
                None,
                0,
                "baseline".to_string(),
                None,
            ));
        }
        let Some(snapshot) = resolved.snapshot.clone() else {
            return Ok(baseline_plan(
                "snapshot_missing_or_invalid",
                resolved.mode,
                None,
                None,
                0,
                "baseline".to_string(),
                None,
            ));
        };
        let snapshot_json = serde_json::to_string(&snapshot)?;
        let posterior = match self
            .store
            .get_bandit_posterior(principal, workspace, lane, &snapshot)
            .await
        {
            Ok(posterior) => posterior,
            Err(error) => {
                tracing::warn!(error = %error, "bandit posterior unavailable; freezing baseline delivery");
                return Ok(baseline_plan(
                    "posterior_unavailable",
                    self.config.bandit.mode,
                    Some(snapshot.snapshot_id.clone()),
                    Some(snapshot.model_version.clone()),
                    0,
                    snapshot.seed_identity.clone(),
                    Some(snapshot_json),
                ));
            },
        };
        let mut candidates = Vec::with_capacity(lane_items.len());
        for item in &lane_items {
            let features = match extract_bandit_features(&snapshot, item, &delivery_context) {
                Ok(features) => features,
                Err(error) => {
                    tracing::warn!(candidate_id = item.candidate_id, error = %error, "delivery feature contract mismatch; freezing baseline delivery");
                    return Ok(baseline_plan(
                        "feature_contract_mismatch",
                        self.config.bandit.mode,
                        Some(snapshot.snapshot_id.clone()),
                        Some(snapshot.model_version.clone()),
                        posterior.version,
                        snapshot.seed_identity.clone(),
                        Some(snapshot_json),
                    ));
                },
            };
            candidates.push(AttentionBanditCandidate {
                candidate_id: item.candidate_id.clone(),
                baseline_position: item.served_rank,
                features,
            });
        }
        let canary_assigned = resolved.apply_canary;
        let projection = finite_probability_matching_rank(
            resolved.mode,
            canary_assigned,
            true,
            &decision_id,
            &snapshot,
            &posterior,
            &candidates,
        )?;
        if !projection.health.support_ok {
            return Ok(baseline_plan(
                "feature_contract_mismatch",
                resolved.mode,
                Some(snapshot.snapshot_id.clone()),
                Some(snapshot.model_version.clone()),
                posterior.version,
                snapshot.seed_identity.clone(),
                Some(snapshot_json),
            ));
        }
        let item_by_id: HashMap<&str, &&AttentionDecisionItem> = lane_items
            .iter()
            .map(|item| (item.candidate_id.as_str(), item))
            .collect();
        let ordered_items = projection
            .served_ids
            .iter()
            .enumerate()
            .filter_map(|(index, candidate_id)| {
                let item = item_by_id.get(candidate_id.as_str())?;
                let propensity = projection
                    .metadata
                    .get(candidate_id)
                    .filter(|_| index < snapshot.slate_size)
                    .map(|metadata| metadata.served_propensity)
                    .unwrap_or(1.0);
                Some(AttentionDeliveryOrderItem {
                    candidate_id: candidate_id.clone(),
                    source_revision: item.source_revision.clone(),
                    root_policy_propensity: propensity,
                    attribution_item: Some((**item).clone()),
                })
            })
            .collect::<Vec<_>>();
        anyhow::ensure!(
            ordered_items.len() == lane_items.len()
                && ordered_items.iter().all(|item| {
                    item.root_policy_propensity.is_finite()
                        && item.root_policy_propensity > 0.0
                        && item.root_policy_propensity <= 1.0
                }),
            "frozen delivery order/propensity did not reconcile"
        );
        let (status, fallback_reason) = if projection.health.mode == AttentionBanditMode::Canary
            && canary_assigned
            && projection
                .metadata
                .values()
                .any(|metadata| metadata.applied)
        {
            (AttentionDeliveryStatus::Succeeded, None)
        } else if projection.health.mode == AttentionBanditMode::Shadow {
            (
                AttentionDeliveryStatus::BaselineFallback,
                Some("policy_shadow_only".to_string()),
            )
        } else {
            (
                AttentionDeliveryStatus::BaselineFallback,
                Some("policy_scope_not_canary".to_string()),
            )
        };
        let applied = status == AttentionDeliveryStatus::Succeeded;
        Ok(AttentionDeliveryOrderPlan {
            status,
            fallback_reason: fallback_reason.clone(),
            decision_id,
            policy_snapshot_id: Some(snapshot.snapshot_id.clone()),
            policy_model_version: Some(snapshot.model_version.clone()),
            posterior_version: posterior.version,
            seed_identity: snapshot.seed_identity.clone(),
            ordered_items,
            health: AttentionDeliveryHealth {
                bandit_mode: resolved.mode,
                canary_assigned,
                applied,
                baseline_preserved: !applied,
                complete_universe_recorded: true,
                propensity_coverage: 1.0,
                degradation_reason: fallback_reason,
                root_sample_count: 1,
                delivered_count: 0,
                remaining_count: lane_items.len(),
                exact_revision_match: true,
                replay: false,
            },
            policy_snapshot_json: Some(snapshot_json),
            context: delivery_context,
        })
    }

    pub async fn create_attention_delivery(
        &self,
        principal: &str,
        workspace: &str,
        request: &CreateAttentionDelivery,
    ) -> Result<FrozenAttentionDelivery> {
        self.store
            .create_attention_delivery(principal, workspace, request)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn read_attention_delivery_page(
        &self,
        principal: &str,
        workspace: &str,
        lane: AttentionSurface,
        cursor: &str,
        current_source_generation_token: Option<&str>,
        legacy_current_projection_id: Option<&str>,
        legacy_current_universe_digest: Option<&str>,
        now: i64,
    ) -> std::result::Result<FrozenAttentionDelivery, AttentionDeliveryReadError> {
        self.store
            .read_attention_delivery_page(
                principal,
                workspace,
                lane,
                cursor,
                current_source_generation_token,
                legacy_current_projection_id,
                legacy_current_universe_digest,
                now,
            )
            .await
    }

    pub async fn attention_delivery_cursor_source_generation_token(
        &self,
        principal: &str,
        workspace: &str,
        lane: AttentionSurface,
        cursor: &str,
        now: i64,
    ) -> std::result::Result<Option<String>, AttentionDeliveryReadError> {
        self.store
            .attention_delivery_cursor_source_generation_token(
                principal, workspace, lane, cursor, now,
            )
            .await
    }

    pub async fn attention_delivery_health(
        &self,
        principal: &str,
        workspace: &str,
        now: i64,
    ) -> Result<AttentionDeliveryLedgerHealth> {
        self.store
            .attention_delivery_health(principal, workspace, now)
            .await
    }

    pub async fn compact_attention_deliveries(
        &self,
        principal: &str,
        workspace: &str,
        now: i64,
        apply: bool,
    ) -> Result<AttentionRetentionReport> {
        let retention_ms = self
            .config
            .bandit
            .delivery_retention_days
            .saturating_mul(86_400_000);
        let cutoff_at = now.saturating_sub(i64::try_from(retention_ms).unwrap_or(i64::MAX));
        self.store
            .compact_attention_deliveries(principal, workspace, cutoff_at.max(1), apply)
            .await
    }

    /// Apply Slice-5 only after hard eligibility, routing, grouping, and the
    /// deterministic served projection are complete. Page two and later are
    /// always baseline, and this method never changes `served_route`.
    pub async fn apply_personal_bandit_ranking(
        &self,
        principal: &str,
        workspace: &str,
        first_page: bool,
        context: &AttentionDecisionContext,
        evaluation: &mut AttentionRoutingEvaluation,
    ) -> Result<Vec<String>> {
        let mut baseline_items: Vec<_> = evaluation
            .items
            .iter()
            .filter(|item| item.served_rank > 0)
            .collect();
        baseline_items.sort_by_key(|item| item.served_rank);
        let baseline_ids: Vec<String> = baseline_items
            .iter()
            .map(|item| item.candidate_id.clone())
            .collect();
        let selected_count = evaluation.items.iter().filter(|item| item.selected).count();
        let resolved = self.resolve_bandit(principal, workspace).await?;
        if resolved.mode == AttentionBanditMode::Disabled {
            evaluation.bandit_health = Some(AttentionBanditHealth {
                mode: AttentionBanditMode::Disabled,
                policy_snapshot_id: None,
                posterior_version: 0,
                posterior_update_count: 0,
                propensity_coverage: if selected_count == 0 { 0.0 } else { 1.0 },
                exploration_rate: 0.0,
                support_ok: false,
                first_page_bounded: true,
                degradation_reason: Some("bandit_disabled".to_string()),
            });
            return Ok(baseline_ids);
        }
        let Some(snapshot) = resolved.snapshot.clone() else {
            evaluation.bandit_health = Some(AttentionBanditHealth {
                mode: AttentionBanditMode::Disabled,
                policy_snapshot_id: None,
                posterior_version: 0,
                posterior_update_count: 0,
                propensity_coverage: if selected_count == 0 { 0.0 } else { 1.0 },
                exploration_rate: 0.0,
                support_ok: false,
                first_page_bounded: true,
                degradation_reason: Some("snapshot_missing_or_invalid".to_string()),
            });
            return Ok(baseline_ids);
        };

        let mut served_ids = Vec::new();
        let mut metadata_by_id = HashMap::new();
        let mut posterior_version = 0_u64;
        let mut posterior_update_count = 0_u64;
        let mut support_ok = true;
        let mut first_page_bounded = true;
        let mut exploration_rate = 0.0;
        let mut degradation_reason = None;
        let mut ranked_lanes = 0_u32;

        for (surface, route) in [
            (AttentionSurface::FollowUp, AttentionRoute::FollowUp),
            (AttentionSurface::WorthALook, AttentionRoute::WorthALook),
        ] {
            let mut lane_items: Vec<_> = evaluation
                .items
                .iter()
                .filter(|item| {
                    item.served_route == route
                        && item.served_rank > 0
                        && item.hard_eligible
                        && item.representative
                })
                .collect();
            if lane_items.is_empty() {
                continue;
            }
            lane_items.sort_by_key(|item| item.served_rank);
            let posterior = self
                .store
                .get_bandit_posterior(principal, workspace, surface, &snapshot)
                .await?;
            posterior_version = posterior_version.max(posterior.version);
            posterior_update_count = posterior_update_count.saturating_add(posterior.update_count);
            let mut candidates = Vec::with_capacity(lane_items.len());
            let mut lane_ok = true;
            for (index, item) in lane_items.iter().enumerate() {
                match extract_bandit_features(&snapshot, item, context) {
                    Ok(features) => candidates.push(AttentionBanditCandidate {
                        candidate_id: item.candidate_id.clone(),
                        baseline_position: index + 1,
                        features,
                    }),
                    Err(error) => {
                        tracing::warn!(
                            candidate_id = item.candidate_id,
                            error = %error,
                            "bandit feature contract mismatch; preserving this lane's order"
                        );
                        lane_ok = false;
                        support_ok = false;
                        degradation_reason = Some("feature_contract_mismatch".to_string());
                        break;
                    },
                }
            }
            if !lane_ok {
                served_ids.extend(lane_items.iter().map(|item| item.candidate_id.clone()));
                continue;
            }
            let projection = finite_probability_matching_rank(
                resolved.mode,
                resolved.apply_canary,
                first_page,
                &evaluation.decision_id,
                &snapshot,
                &posterior,
                &candidates,
            )?;
            support_ok &= projection.health.support_ok;
            first_page_bounded &= projection.health.first_page_bounded;
            if let Some(reason) = projection.health.degradation_reason.clone() {
                degradation_reason.get_or_insert(reason);
            }
            exploration_rate += projection.health.exploration_rate;
            ranked_lanes = ranked_lanes.saturating_add(1);
            served_ids.extend(projection.served_ids.iter().cloned());
            metadata_by_id.extend(projection.metadata);
        }

        if ranked_lanes > 0 {
            exploration_rate /= f64::from(ranked_lanes);
        }
        let lane_positions: HashMap<String, usize> = {
            let mut positions = HashMap::new();
            for route in [AttentionRoute::FollowUp, AttentionRoute::WorthALook] {
                let mut rank = 0_usize;
                for id in &served_ids {
                    if evaluation
                        .items
                        .iter()
                        .any(|item| item.candidate_id == *id && item.served_route == route)
                    {
                        rank += 1;
                        positions.insert(id.clone(), rank);
                    }
                }
            }
            positions
        };
        for item in &mut evaluation.items {
            if let Some(metadata) = metadata_by_id.get(&item.candidate_id) {
                item.served_rank = lane_positions
                    .get(&item.candidate_id)
                    .copied()
                    .unwrap_or(item.served_rank);
                item.selection_probability = if metadata.applied {
                    metadata.served_propensity
                } else {
                    item.selection_probability
                };
                item.exploration = metadata.applied && metadata.exploration;
                item.bandit_decision = Some(metadata.clone());
            }
        }
        evaluation.bandit_health = Some(AttentionBanditHealth {
            mode: resolved.mode,
            policy_snapshot_id: Some(snapshot.snapshot_id),
            posterior_version,
            posterior_update_count,
            propensity_coverage: if selected_count == 0 { 0.0 } else { 1.0 },
            exploration_rate,
            support_ok,
            first_page_bounded,
            degradation_reason,
        });
        if served_ids.is_empty() {
            Ok(baseline_ids)
        } else {
            Ok(served_ids)
        }
    }

    /// Evaluate one complete candidate universe. Cross-lane application is
    /// possible only when the caller proves that the input is the canonical
    /// union of both baseline lane universes; otherwise learned changes remain
    /// ledger-only and baseline serving is retained.
    pub async fn resolve_routing(
        &self,
        principal: &str,
        workspace: &str,
    ) -> Result<(AttentionRoutingMode, Option<AttentionRoutingPolicySnapshot>)> {
        let configured = self.config.routing.mode;
        if configured != AttentionRoutingMode::Baseline {
            if let Some(snapshot) = self.configured_routing_policy_snapshot().await {
                return Ok((configured, Some(snapshot)));
            }
        }
        let Some((snapshot_id, mode, _)) = self
            .store
            .routing_scope_install(principal, workspace)
            .await?
        else {
            return Ok((AttentionRoutingMode::Baseline, None));
        };
        if mode == AttentionRoutingMode::Baseline {
            return Ok((AttentionRoutingMode::Baseline, None));
        }
        match self.store.get_routing_policy_snapshot(&snapshot_id).await? {
            Some(snapshot) => Ok((mode, Some(snapshot))),
            None => Ok((AttentionRoutingMode::Baseline, None)),
        }
    }

    pub async fn evaluate_routing_universe(
        &self,
        principal: &str,
        workspace: &str,
        requested_surface: AttentionSurface,
        complete_cross_lane_universe: bool,
        candidates: &[AttentionRoutingCandidate<'_>],
    ) -> Result<AttentionRoutingEvaluation> {
        let (mode, snapshot) = self.resolve_routing(principal, workspace).await?;
        evaluate_routing(
            uuid::Uuid::new_v4().to_string(),
            chrono::Utc::now().timestamp_millis(),
            principal,
            workspace,
            requested_surface,
            mode,
            self.config.routing.canary_fraction,
            &self.config.routing.seed_identity,
            snapshot.as_ref(),
            complete_cross_lane_universe,
            candidates,
        )
    }

    /// Canonical cross-lane evaluation with a deterministic decision identity.
    /// The caller must supply the complete origin-qualified Follow-up +
    /// Worth-a-look union; this boundary is intentionally unavailable to the
    /// legacy lane-local projectors.
    pub async fn evaluate_canonical_routing_universe(
        &self,
        decision_id: String,
        principal: &str,
        workspace: &str,
        candidates: &[AttentionRoutingCandidate<'_>],
    ) -> Result<AttentionRoutingEvaluation> {
        let (mode, snapshot) = self.resolve_routing(principal, workspace).await?;
        evaluate_routing(
            decision_id,
            chrono::Utc::now().timestamp_millis(),
            principal,
            workspace,
            AttentionSurface::FollowUp,
            mode,
            self.config.routing.canary_fraction,
            &self.config.routing.seed_identity,
            snapshot.as_ref(),
            true,
            candidates,
        )
    }

    pub async fn canonical_projection_json(
        &self,
        principal: &str,
        workspace: &str,
        universe_digest: &str,
        policy_identity: &str,
    ) -> Result<Option<String>> {
        self.store
            .get_canonical_projection_json(principal, workspace, universe_digest, policy_identity)
            .await
    }

    pub async fn canonical_projection_lane_page(
        &self,
        projection_id: &str,
        lane: &str,
        offset: usize,
        limit: usize,
    ) -> Result<CanonicalProjectionLanePage> {
        self.store
            .canonical_projection_lane_page(projection_id, lane, offset, limit)
            .await
    }

    pub async fn canonical_projection_lane_size(
        &self,
        projection_id: &str,
        lane: &str,
    ) -> Result<Option<usize>> {
        self.store
            .canonical_projection_lane_size(projection_id, lane)
            .await
    }

    /// Bounded, resumable repair for pre-normalization attention rows. Runtime
    /// reads remain backward-compatible while an operator drains these batches.
    /// Projection and feature counts include deterministic quarantine progress
    /// so a caller draining until zero cannot stop behind an unreadable head row.
    pub async fn migrate_legacy_storage_batch(&self, limit: usize) -> Result<(u64, u64, u64)> {
        let references = self
            .store
            .migrate_legacy_projection_references(limit)
            .await?;
        let projection_rows_processed = self
            .store
            .migrate_legacy_canonical_projections(limit)
            .await?;
        let feature_rows_processed = self.store.migrate_legacy_feature_snapshots(limit).await?;
        tracing::info!(
            references,
            projection_rows_processed,
            feature_rows_processed,
            "processed bounded legacy attention storage batch"
        );
        Ok((
            references,
            projection_rows_processed,
            feature_rows_processed,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn persist_canonical_projection_json(
        &self,
        principal: &str,
        workspace: &str,
        universe_digest: &str,
        policy_identity: &str,
        projection_id: &str,
        created_at: i64,
        projection_json: &str,
    ) -> Result<String> {
        self.store
            .persist_canonical_projection_json(
                principal,
                workspace,
                universe_digest,
                policy_identity,
                projection_id,
                created_at,
                projection_json,
            )
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn persist_canonical_projection_json_owned(
        &self,
        principal: &str,
        workspace: &str,
        universe_digest: &str,
        policy_identity: &str,
        projection_id: &str,
        created_at: i64,
        projection_json: String,
    ) -> Result<String> {
        self.store
            .persist_canonical_projection_json_owned(
                principal,
                workspace,
                universe_digest,
                policy_identity,
                projection_id,
                created_at,
                projection_json,
            )
            .await
    }

    pub async fn record_routing_decision(
        &self,
        principal: &str,
        workspace: &str,
        candidate_set_digest: &str,
        context: AttentionDecisionContext,
        latency_ms: u64,
        returned_item_count: usize,
        evaluation: &AttentionRoutingEvaluation,
    ) -> Result<AttentionDecision> {
        self.store
            .record_decision(
                principal,
                workspace,
                candidate_set_digest,
                context,
                latency_ms,
                returned_item_count,
                evaluation,
            )
            .await
    }

    pub async fn get_routing_decision(
        &self,
        principal: &str,
        workspace: &str,
        decision_id: &str,
    ) -> Result<Option<AttentionDecisionDetail>> {
        self.store
            .get_decision(principal, workspace, decision_id)
            .await
    }

    /// The projection a decision was served from — see the store method for why
    /// a rank recompute must not resolve against a freshly computed one.
    pub async fn get_decision_projection_json(
        &self,
        principal: &str,
        workspace: &str,
        decision_id: &str,
    ) -> Result<Option<String>> {
        self.store
            .get_decision_projection_json(principal, workspace, decision_id)
            .await
    }

    pub async fn record_verified_impression(
        &self,
        principal: &str,
        workspace: &str,
        request: &RecordAttentionImpression,
    ) -> std::result::Result<AttentionImpressionReceipt, AttentionImpressionError> {
        self.store
            .record_impression(
                principal,
                workspace,
                request,
                self.config.routing.min_visible_ms,
                &self.config.routing.visibility_rule_version,
                chrono::Utc::now().timestamp_millis(),
            )
            .await
    }

    pub async fn routing_health(
        &self,
        principal: &str,
        workspace: &str,
        evaluation: &AttentionRoutingEvaluation,
    ) -> Result<AttentionRoutingHealth> {
        self.store
            .routing_health(principal, workspace, evaluation)
            .await
    }

    pub async fn record_pair_label(
        &self,
        principal: &str,
        workspace: &str,
        request: &RecordAttentionPairLabel,
    ) -> Result<(PersistedAttentionPairLabel, u64)> {
        let persisted = self
            .store
            .record_pair_label(principal, workspace, request)
            .await?;
        let generation = if persisted.inserted {
            self.store
                .advance_grouping_generation(
                    principal,
                    workspace,
                    request.surface,
                    request.occurred_at,
                )
                .await?
        } else {
            self.store
                .grouping_generation(principal, workspace, request.surface)
                .await?
        };
        Ok((persisted, generation))
    }

    /// Compute grouping over the complete eligible universe. Callers paginate
    /// only after this projection, and use `representative_ids` only in
    /// effective enforced mode.
    pub async fn group_eligible_universe(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
        baseline_candidates: &[SemanticAttentionCandidate],
        ranks: &[AttentionRankMetadata],
    ) -> Result<AttentionGroupingResult> {
        let generation = self
            .store
            .grouping_generation(principal, workspace, surface)
            .await?;
        let rank_by_id: HashMap<&str, &AttentionRankMetadata> = ranks
            .iter()
            .map(|rank| (rank.candidate_id.as_str(), rank))
            .collect();
        let configured_mode = self.config.grouping.mode;
        let pair_evaluation_budget = self.config.grouping.max_pair_evaluations;
        let required_pair_evaluations = required_pair_evaluations(baseline_candidates.len());
        let budget_exceeded = required_pair_evaluations > pair_evaluation_budget;
        let snapshot = if configured_mode == AttentionGroupingMode::Disabled || budget_exceeded {
            None
        } else {
            self.configured_pair_model_snapshot().await
        };
        // Disabled or invalidly configured grouping must not perform the
        // embedding lookup used only by pair inference.
        let persisted_embeddings = if snapshot.is_some() {
            self.store
                .list_candidate_embeddings(principal, workspace, surface)
                .await?
        } else {
            std::sync::Arc::new(HashMap::new())
        };
        let candidates: Vec<GroupingCandidate> = baseline_candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| {
                let rank = rank_by_id.get(candidate.candidate_id.as_str()).copied();
                let mut features = candidate.grouping_features.clone().unwrap_or_default();
                if features.embedding.is_none() {
                    features.embedding = candidate.existing_embedding.clone().or_else(|| {
                        persisted_embeddings
                            .get(&candidate.candidate_id)
                            .filter(|stored| stored.source_revision == candidate.source_revision)
                            .map(|stored| stored.embedding.clone())
                    });
                }
                GroupingCandidate {
                    candidate_id: candidate.candidate_id.clone(),
                    source_revision: candidate.source_revision.clone(),
                    baseline_rank: rank.map(|rank| rank.baseline_rank).unwrap_or(index + 1),
                    learned_rank: rank.map(|rank| rank.learned_rank).unwrap_or(index + 1),
                    features,
                }
            })
            .collect();
        let (mut mode, mut projection, mut snapshot_id) = if let Some(snapshot) = snapshot {
            let evidence = self
                .store
                .list_pair_evidence(principal, workspace, surface)
                .await?;
            match cluster_candidates(&snapshot, &candidates, &evidence) {
                Ok(projection) => (
                    configured_mode,
                    projection,
                    Some(snapshot.snapshot_id.clone()),
                ),
                Err(error) => {
                    tracing::warn!(error = %error, "pair clustering failed; returning ungrouped universe");
                    (
                        AttentionGroupingMode::Disabled,
                        singleton_grouping(&candidates),
                        None,
                    )
                },
            }
        } else {
            (
                AttentionGroupingMode::Disabled,
                singleton_grouping(&candidates),
                None,
            )
        };
        let mut fallback_ungrouped_total = if mode == AttentionGroupingMode::Disabled {
            candidates.len()
        } else {
            projection
                .clusters
                .iter()
                .filter(|cluster| cluster.member_ids.len() == 1)
                .count()
        };
        let mut health = AttentionGroupingHealth::from_projection(
            candidates.len(),
            &projection,
            fallback_ungrouped_total,
            pair_evaluation_budget,
            required_pair_evaluations,
            budget_exceeded,
        );
        // A malformed or internally incomplete projection must never make an
        // enforced response hide an original Slice-2 row. Fail closed to the
        // complete ungrouped universe while preserving diagnostics.
        if mode == AttentionGroupingMode::Enforced && !health.totals_reconcile {
            tracing::warn!(
                candidate_total = health.candidate_total,
                member_total = health.member_total,
                cluster_total = health.cluster_total,
                "pair grouping totals did not reconcile; returning the ungrouped universe"
            );
            mode = AttentionGroupingMode::Disabled;
            projection = singleton_grouping(&candidates);
            snapshot_id = None;
            fallback_ungrouped_total = candidates.len();
            health = AttentionGroupingHealth::from_projection(
                candidates.len(),
                &projection,
                fallback_ungrouped_total,
                pair_evaluation_budget,
                required_pair_evaluations,
                budget_exceeded,
            );
        }
        Ok(AttentionGroupingResult {
            generation,
            mode,
            snapshot_id,
            projection,
            health,
        })
    }

    /// Reconcile lane-local embeddings and pair corrections into the
    /// origin-qualified union, then perform one clustering pass. Evidence is
    /// remapped only within its owning surface; an unlabeled cross-origin pair
    /// therefore starts with no borrowed must-link/cannot-link evidence.
    pub async fn group_canonical_eligible_universe(
        &self,
        principal: &str,
        workspace: &str,
        candidates: &[CanonicalAttentionRankCandidate<'_>],
        ranks: &[AttentionRankMetadata],
    ) -> Result<AttentionGroupingResult> {
        let rank_by_id: HashMap<&str, &AttentionRankMetadata> = ranks
            .iter()
            .map(|rank| (rank.candidate_id.as_str(), rank))
            .collect();
        anyhow::ensure!(
            rank_by_id.len() == candidates.len(),
            "canonical grouping ranks do not reconcile"
        );
        let configured_mode = self.config.grouping.mode;
        let pair_evaluation_budget = self.config.grouping.max_pair_evaluations;
        let required_pair_evaluations = required_pair_evaluations(candidates.len());
        let budget_exceeded = required_pair_evaluations > pair_evaluation_budget;
        let snapshot = if configured_mode == AttentionGroupingMode::Disabled || budget_exceeded {
            None
        } else {
            self.configured_pair_model_snapshot().await
        };

        let mut origin_embeddings = HashMap::new();
        if snapshot.is_some() {
            for surface in [AttentionSurface::FollowUp, AttentionSurface::WorthALook] {
                // The snapshot is shared, so copy the entries this scope needs
                // rather than draining it.
                for (candidate_id, embedding) in self
                    .store
                    .list_candidate_embeddings(principal, workspace, surface)
                    .await?
                    .iter()
                {
                    origin_embeddings.insert((surface, candidate_id.clone()), embedding.clone());
                }
            }
        }
        let grouping_candidates: Vec<GroupingCandidate> = candidates
            .iter()
            .enumerate()
            .map(|(index, binding)| {
                let rank = rank_by_id
                    .get(binding.candidate.candidate_id.as_str())
                    .copied();
                let mut features = binding
                    .candidate
                    .grouping_features
                    .clone()
                    .unwrap_or_default();
                if features.embedding.is_none() {
                    features.embedding =
                        binding.candidate.existing_embedding.clone().or_else(|| {
                            origin_embeddings
                                .get(&(
                                    binding.evidence_surface,
                                    binding.evidence_candidate_id.to_string(),
                                ))
                                .filter(|stored| {
                                    stored.source_revision == binding.candidate.source_revision
                                })
                                .map(|stored| stored.embedding.clone())
                        });
                }
                GroupingCandidate {
                    candidate_id: binding.candidate.candidate_id.clone(),
                    source_revision: binding.candidate.source_revision.clone(),
                    baseline_rank: rank.map(|rank| rank.baseline_rank).unwrap_or(index + 1),
                    learned_rank: rank.map(|rank| rank.learned_rank).unwrap_or(index + 1),
                    features,
                }
            })
            .collect();

        let mut generation = 0_u64;
        let mut evidence = Vec::new();
        if snapshot.is_some() {
            for surface in [AttentionSurface::FollowUp, AttentionSurface::WorthALook] {
                generation = generation.wrapping_add(
                    self.store
                        .grouping_generation(principal, workspace, surface)
                        .await?,
                );
                let canonical_by_origin: HashMap<(&str, Option<&str>), &str> = candidates
                    .iter()
                    .filter(|candidate| candidate.evidence_surface == surface)
                    .map(|candidate| {
                        (
                            (
                                candidate.evidence_candidate_id,
                                candidate.candidate.source_revision.as_deref(),
                            ),
                            candidate.candidate.candidate_id.as_str(),
                        )
                    })
                    .collect();
                for row in self
                    .store
                    .list_pair_evidence(principal, workspace, surface)
                    .await?
                {
                    let Some(left_id) = canonical_by_origin
                        .get(&(
                            row.left.candidate_id.as_str(),
                            row.left.source_revision.as_deref(),
                        ))
                        .copied()
                    else {
                        continue;
                    };
                    let Some(right_id) = canonical_by_origin
                        .get(&(
                            row.right.candidate_id.as_str(),
                            row.right.source_revision.as_deref(),
                        ))
                        .copied()
                    else {
                        continue;
                    };
                    evidence.push(PersistedPairEvidence {
                        left: AttentionPairCandidateRef {
                            candidate_id: left_id.to_string(),
                            source_revision: row.left.source_revision,
                        },
                        right: AttentionPairCandidateRef {
                            candidate_id: right_id.to_string(),
                            source_revision: row.right.source_revision,
                        },
                        label: row.label,
                        source: row.source,
                        confidence: row.confidence,
                    });
                }
            }
        } else {
            for surface in [AttentionSurface::FollowUp, AttentionSurface::WorthALook] {
                generation = generation.wrapping_add(
                    self.store
                        .grouping_generation(principal, workspace, surface)
                        .await?,
                );
            }
        }

        let (mut mode, mut projection, mut snapshot_id) = if let Some(snapshot) = snapshot {
            match cluster_candidates(&snapshot, &grouping_candidates, &evidence) {
                Ok(projection) => (
                    configured_mode,
                    projection,
                    Some(snapshot.snapshot_id.clone()),
                ),
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "canonical pair clustering failed; returning exact singleton union"
                    );
                    (
                        AttentionGroupingMode::Disabled,
                        singleton_grouping(&grouping_candidates),
                        None,
                    )
                },
            }
        } else {
            (
                AttentionGroupingMode::Disabled,
                singleton_grouping(&grouping_candidates),
                None,
            )
        };
        let mut fallback_ungrouped_total = if mode == AttentionGroupingMode::Disabled {
            grouping_candidates.len()
        } else {
            projection
                .clusters
                .iter()
                .filter(|cluster| cluster.member_ids.len() == 1)
                .count()
        };
        let mut health = AttentionGroupingHealth::from_projection(
            grouping_candidates.len(),
            &projection,
            fallback_ungrouped_total,
            pair_evaluation_budget,
            required_pair_evaluations,
            budget_exceeded,
        );
        if !health.totals_reconcile {
            mode = AttentionGroupingMode::Disabled;
            projection = singleton_grouping(&grouping_candidates);
            snapshot_id = None;
            fallback_ungrouped_total = grouping_candidates.len();
            health = AttentionGroupingHealth::from_projection(
                grouping_candidates.len(),
                &projection,
                fallback_ungrouped_total,
                pair_evaluation_budget,
                required_pair_evaluations,
                budget_exceeded,
            );
        }
        Ok(AttentionGroupingResult {
            generation,
            mode,
            snapshot_id,
            projection,
            health,
        })
    }

    pub fn rescore_limit(&self) -> usize {
        self.config.rescore_limit.clamp(1, 2_000)
    }

    pub fn active_embedding_contract(&self) -> Option<String> {
        Some(
            ollama_lifecycle::embedder()
                .unwrap_or_else(OllamaEmbedder::from_env)
                .embedding_contract_id(),
        )
    }

    pub async fn record_and_propagate(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
        request: RecordAttentionOutcome,
        active_candidates: Vec<SemanticAttentionCandidate>,
    ) -> Result<AttentionFeedbackReceipt> {
        self.record_and_propagate_inner(
            principal,
            workspace,
            surface,
            request,
            active_candidates,
            true,
            true,
        )
        .await
    }

    /// Historical labels update the same estimator but do not pretend the
    /// retired exemplar is an active canonical card requiring a rank-after
    /// diagnostic job.
    pub async fn record_historical_and_propagate(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
        request: RecordAttentionOutcome,
        active_candidates: Vec<SemanticAttentionCandidate>,
    ) -> Result<AttentionFeedbackReceipt> {
        self.record_and_propagate_inner(
            principal,
            workspace,
            surface,
            request,
            active_candidates,
            false,
            true,
        )
        .await
    }

    /// Durably stage one historical label without repeatedly rescoring the
    /// active cohort. Historical-bootstrap callers stage every label in their
    /// bounded pass through this path, then refresh each active surface once
    /// after the whole pass has been persisted.
    pub async fn stage_historical_outcome(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
        request: RecordAttentionOutcome,
    ) -> Result<AttentionFeedbackReceipt> {
        self.record_and_propagate_inner(
            principal,
            workspace,
            surface,
            request,
            Vec::new(),
            false,
            false,
        )
        .await
    }

    async fn record_and_propagate_inner(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
        request: RecordAttentionOutcome,
        active_candidates: Vec<SemanticAttentionCandidate>,
        schedule_rank_diagnostic: bool,
        propagate_scores: bool,
    ) -> Result<AttentionFeedbackReceipt> {
        if !self.config.enabled {
            return Ok(AttentionFeedbackReceipt {
                outcome_id: request.event_id,
                outcome: request.outcome,
                surface,
                feedback_recorded: false,
                affected_candidates: 0,
                rescore_status: AttentionRescoreStatus::Disabled,
                embedding_contract: None,
                diagnostic_href: None,
                posterior_update: Some(AttentionPosteriorUpdateReceipt {
                    status: AttentionPosteriorUpdateStatus::Disabled,
                    policy_snapshot_id: None,
                    posterior_version_before: None,
                    posterior_version_after: None,
                    attribution_quality: AttentionBanditAttributionQuality::Missing,
                    degradation_reason: Some("attention_learning_disabled".to_string()),
                    uncertainty_before: None,
                    uncertainty_after: None,
                    affected_rank_before: None,
                    affected_rank_after: None,
                    affected_rank_delta: None,
                    rescore_scheduled: false,
                }),
                rank_recompute: None,
            });
        }

        let mut candidates = dedupe_candidates(active_candidates, self.rescore_limit());
        if !candidates
            .iter()
            .any(|candidate| candidate.candidate_id == request.candidate.candidate_id)
        {
            candidates.push(request.candidate.clone());
        }

        // Commit the explicit owner outcome before any optional embedding or
        // propagation work. A timeout or unavailable model may degrade the
        // receipt, but must never erase the label itself.
        let active_contract = self.active_embedding_contract();
        let durable_embedding = request
            .candidate
            .existing_embedding
            .as_ref()
            .filter(|embedding| {
                active_contract
                    .as_deref()
                    .is_none_or(|contract| embedding.contract == contract)
            });
        let persisted = self
            .store
            .record_outcome(principal, workspace, surface, &request, durable_embedding)
            .await?;

        // The canonical outcome is already durable. Posterior attribution and
        // update are a separate fail-soft step and can never roll it back.
        let resolved_bandit = self.resolve_bandit(principal, workspace).await?;
        let mut posterior_update = if resolved_bandit.mode == AttentionBanditMode::Disabled {
            AttentionPosteriorUpdateReceipt {
                status: AttentionPosteriorUpdateStatus::Disabled,
                policy_snapshot_id: None,
                posterior_version_before: None,
                posterior_version_after: None,
                attribution_quality: AttentionBanditAttributionQuality::Missing,
                degradation_reason: Some("bandit_disabled".to_string()),
                uncertainty_before: None,
                uncertainty_after: None,
                affected_rank_before: None,
                affected_rank_after: None,
                affected_rank_delta: None,
                rescore_scheduled: false,
            }
        } else if let Some(snapshot) = resolved_bandit.snapshot {
            match self
                .store
                .apply_bandit_outcome_update(
                    principal, workspace, surface, &persisted, &request, &snapshot,
                )
                .await
            {
                Ok(receipt) => receipt,
                Err(error) => {
                    tracing::warn!(
                        outcome_id = persisted.outcome_id,
                        error = %error,
                        "canonical outcome persisted but posterior update failed"
                    );
                    AttentionPosteriorUpdateReceipt {
                        status: AttentionPosteriorUpdateStatus::Degraded,
                        policy_snapshot_id: Some(snapshot.snapshot_id),
                        posterior_version_before: None,
                        posterior_version_after: None,
                        attribution_quality: AttentionBanditAttributionQuality::Missing,
                        degradation_reason: Some("posterior_update_failed".to_string()),
                        uncertainty_before: None,
                        uncertainty_after: None,
                        affected_rank_before: None,
                        affected_rank_after: None,
                        affected_rank_delta: None,
                        rescore_scheduled: false,
                    }
                },
            }
        } else {
            AttentionPosteriorUpdateReceipt {
                status: AttentionPosteriorUpdateStatus::Degraded,
                policy_snapshot_id: self.config.bandit.snapshot_id.clone(),
                posterior_version_before: None,
                posterior_version_after: None,
                attribution_quality: AttentionBanditAttributionQuality::Missing,
                degradation_reason: Some("bandit_snapshot_missing_or_invalid".to_string()),
                uncertainty_before: None,
                uncertainty_after: None,
                affected_rank_before: None,
                affected_rank_after: None,
                affected_rank_delta: None,
                rescore_scheduled: false,
            }
        };

        let expected_canonical_candidate_id =
            surface.canonical_candidate_id(&request.candidate.candidate_id);
        let canonical_candidate_id = request
            .attribution
            .as_ref()
            .map(|attribution| attribution.candidate_id.clone())
            .filter(|candidate_id| candidate_id == &expected_canonical_candidate_id)
            .unwrap_or_else(|| expected_canonical_candidate_id.clone());
        let schedule = ScheduleAttentionRankRecompute {
            outcome_id: persisted.outcome_id.clone(),
            origin_surface: surface,
            canonical_candidate_id,
            raw_candidate_id: request.candidate.candidate_id.clone(),
            source_revision: request.candidate.source_revision.clone(),
            outcome: request.outcome,
            decision_id: request
                .attribution
                .as_ref()
                .map(|value| value.decision_id.clone()),
            delivery_id: request
                .attribution
                .as_ref()
                .and_then(|value| value.delivery_id.clone()),
            impression_id: request
                .attribution
                .as_ref()
                .and_then(|value| value.impression_id.clone()),
            affected_rank_before: posterior_update.affected_rank_before,
            enqueue_policy_snapshot_id: posterior_update.policy_snapshot_id.clone(),
            enqueue_posterior_version: posterior_update.posterior_version_after,
        };
        let rank_recompute = if schedule_rank_diagnostic {
            Some(
                match self
                    .store
                    .schedule_rank_recompute(
                        principal,
                        workspace,
                        &schedule,
                        chrono::Utc::now().timestamp_millis(),
                    )
                    .await
                {
                    Ok(job) => AttentionRankRecomputeReference::enqueued(&job),
                    Err(error) => {
                        tracing::warn!(outcome_id = persisted.outcome_id, error = %error, "canonical outcome persisted but rank recompute enqueue failed");
                        AttentionRankRecomputeReference::failed(
                            posterior_update.affected_rank_before,
                        )
                    },
                },
            )
        } else {
            None
        };
        posterior_update.rescore_scheduled = rank_recompute.as_ref().is_some_and(|reference| {
            reference.enqueue_status == AttentionRankRecomputeEnqueueStatus::Enqueued
        });

        let embedding_contract = match self
            .resolve_embeddings(
                principal,
                workspace,
                surface,
                &mut candidates,
                self.embedding_budget(false),
            )
            .await
        {
            Ok(contract) => contract,
            Err(error) => {
                tracing::warn!(
                    candidate_id = request.candidate.candidate_id,
                    error = %error,
                    "attention outcome persisted but embedding propagation failed"
                );
                return Ok(receipt(
                    &persisted,
                    surface,
                    0,
                    AttentionRescoreStatus::Failed,
                    None,
                    Some(posterior_update.clone()),
                    rank_recompute.clone(),
                ));
            },
        };

        let Some(contract) = embedding_contract else {
            return Ok(receipt(
                &persisted,
                surface,
                0,
                AttentionRescoreStatus::DegradedNoEmbedding,
                None,
                Some(posterior_update.clone()),
                rank_recompute.clone(),
            ));
        };

        let target_embedding = candidates
            .iter()
            .find(|candidate| candidate.candidate_id == request.candidate.candidate_id)
            .and_then(|candidate| {
                candidate
                    .existing_embedding
                    .as_ref()
                    .filter(|embedding| embedding.contract == contract)
            });
        if target_embedding.is_some() {
            if let Err(error) = self
                .store
                .bind_outcome_embedding(
                    principal,
                    workspace,
                    &persisted.outcome_id,
                    &request.candidate.candidate_id,
                    request.candidate.source_revision.as_deref(),
                    &contract,
                )
                .await
            {
                tracing::warn!(
                    outcome_id = persisted.outcome_id,
                    error = %error,
                    "attention outcome persisted but revision-bound embedding could not be attached"
                );
                return Ok(receipt(
                    &persisted,
                    surface,
                    0,
                    AttentionRescoreStatus::Failed,
                    Some(contract),
                    Some(posterior_update.clone()),
                    rank_recompute.clone(),
                ));
            }
            if let Err(error) = self
                .store
                .complete_embedding_bind(&persisted.outcome_id)
                .await
            {
                tracing::warn!(
                    outcome_id = persisted.outcome_id,
                    error = %error,
                    "attention embedding was bound but its repair marker remains"
                );
            }
        }

        if !propagate_scores {
            return Ok(receipt(
                &persisted,
                surface,
                0,
                AttentionRescoreStatus::Completed,
                Some(contract),
                Some(posterior_update),
                rank_recompute,
            ));
        }

        let labels = match self
            .store
            .list_strong_labels(principal, workspace, &contract)
            .await
        {
            Ok(labels) => labels,
            Err(error) => {
                tracing::warn!(
                    outcome_id = persisted.outcome_id,
                    error = %error,
                    "attention outcome persisted but labels could not be loaded"
                );
                return Ok(receipt(
                    &persisted,
                    surface,
                    0,
                    AttentionRescoreStatus::Failed,
                    Some(contract),
                    Some(posterior_update.clone()),
                    rank_recompute.clone(),
                ));
            },
        };
        if candidates.is_empty() {
            return Ok(receipt(
                &persisted,
                surface,
                0,
                AttentionRescoreStatus::DegradedNoCandidates,
                Some(contract),
                Some(posterior_update.clone()),
                rank_recompute.clone(),
            ));
        }
        let evaluator = BayesianKnnEvaluator::new(&self.config);
        let mut affected = 0_usize;
        let mut pending_scores = Vec::new();
        for candidate in &candidates {
            let Some(embedding) = candidate
                .existing_embedding
                .as_ref()
                .filter(|embedding| embedding.contract == contract)
            else {
                continue;
            };
            let estimate = evaluator.estimate(&embedding.vector, &labels);
            if estimate.surface_score(surface).is_some() {
                affected += 1;
            }
            pending_scores.push((candidate.clone(), estimate));
        }
        let score_changed = match self
            .store
            .upsert_scores(
                principal,
                workspace,
                surface,
                pending_scores,
                &contract,
                request.occurred_at,
            )
            .await
        {
            Ok(changed) => changed,
            Err(error) => {
                tracing::warn!(
                    outcome_id = persisted.outcome_id,
                    error = %error,
                    "attention outcome persisted but its candidate score batch could not be written"
                );
                return Ok(receipt(
                    &persisted,
                    surface,
                    0,
                    AttentionRescoreStatus::Failed,
                    Some(contract),
                    Some(posterior_update.clone()),
                    rank_recompute.clone(),
                ));
            },
        };
        if score_changed {
            if let Err(error) = self
                .store
                .advance_rank_generation(principal, workspace, surface, request.occurred_at)
                .await
            {
                tracing::warn!(
                    outcome_id = persisted.outcome_id,
                    error = %error,
                    "attention scores changed but rank generation could not advance"
                );
                return Ok(receipt(
                    &persisted,
                    surface,
                    affected,
                    AttentionRescoreStatus::Failed,
                    Some(contract),
                    Some(posterior_update.clone()),
                    rank_recompute.clone(),
                ));
            }
        }
        Ok(receipt(
            &persisted,
            surface,
            affected,
            AttentionRescoreStatus::Completed,
            Some(contract),
            Some(posterior_update),
            rank_recompute,
        ))
    }

    /// Latency budget for one embedding pass.
    ///
    /// These are not interchangeable. The request budget bounds what an owner
    /// will wait for; the background budget bounds work nothing is waiting on.
    /// Spending the request budget on a background pass is not conservative —
    /// it is the difference between slow and impossible, because a local
    /// embedding model evicted by a resident chat model needs longer to load
    /// than any request path would ever grant.
    fn embedding_budget(&self, background: bool) -> Duration {
        let millis = if background {
            self.config.background_embedding_timeout_ms
        } else {
            self.config.embedding_timeout_ms
        };
        Duration::from_millis(millis.max(1))
    }

    async fn resolve_embeddings(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
        candidates: &mut [SemanticAttentionCandidate],
        budget: Duration,
    ) -> Result<Option<String>> {
        let persisted = self
            .store
            .list_candidate_embeddings(principal, workspace, surface)
            .await?;
        for candidate in candidates.iter_mut() {
            if candidate.existing_embedding.is_none() {
                candidate.existing_embedding = persisted
                    .get(&candidate.candidate_id)
                    .filter(|stored| {
                        stored.source_revision == candidate.source_revision
                            && stored.content_digest
                                == semantic_content_digest(&candidate.semantic_text)
                    })
                    .map(|stored| stored.embedding.clone());
            }
        }

        let embedder = ollama_lifecycle::embedder();
        // Desired contract identity is derived from configuration even while
        // the daemon is unavailable. Never let an obsolete supplied vector
        // become the temporary cohort authority during an outage.
        let runtime_contract = self.active_embedding_contract();
        let selected_contract = runtime_contract.clone();
        let missing_indices: Vec<usize> = candidates
            .iter()
            .enumerate()
            .filter(|(_, candidate)| {
                !candidate.semantic_text.trim().is_empty()
                    && candidate
                        .existing_embedding
                        .as_ref()
                        .is_none_or(|embedding| {
                            selected_contract
                                .as_ref()
                                .is_some_and(|contract| embedding.contract != *contract)
                        })
            })
            .map(|(index, _)| index)
            .collect();

        if let (Some(embedder), Some(contract)) = (embedder, runtime_contract.as_ref()) {
            if !missing_indices.is_empty() && ollama_lifecycle::is_available() {
                // One candidate is one persistence unit, and the budget is a
                // deadline shared across all of them. Putting the whole batch
                // behind a single outer timeout means one slow model load
                // discards every vector the pass already earned — which is how
                // a queue of repairable outcomes stays stuck at exactly its
                // original size no matter how often the worker runs.
                let deadline = tokio::time::Instant::now() + budget;
                for index in missing_indices.iter().copied() {
                    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    let texts = vec![candidates[index].semantic_text.trim().to_string()];
                    let started = std::time::Instant::now();
                    let result =
                        timeout(remaining, embedder.embed_documents_background(&texts)).await;
                    let latency_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
                    let vector = match result {
                        Ok(Ok(vectors)) => vectors.into_iter().next(),
                        _ => None,
                    };
                    crate::magician_v2::analytics::llm_embeddings_sink::record_embedding_batch(
                        principal,
                        workspace,
                        &embedder.config().model,
                        "attention_learning",
                        texts.len(),
                        crate::magician_v2::analytics::llm_embeddings_sink::estimate_input_tokens(
                            &texts,
                        ),
                        latency_ms,
                        vector.is_some(),
                    );
                    if let Some(vector) = vector {
                        candidates[index].existing_embedding = Some(SemanticEmbedding {
                            contract: contract.clone(),
                            vector,
                        });
                    }
                }
            }
        }

        let contract = selected_contract.filter(|contract| {
            candidates.iter().any(|candidate| {
                candidate
                    .existing_embedding
                    .as_ref()
                    .is_some_and(|embedding| embedding.contract == *contract)
            })
        });
        if let Some(contract) = contract.as_ref() {
            for candidate in candidates.iter() {
                if let Some(embedding) = candidate
                    .existing_embedding
                    .as_ref()
                    .filter(|embedding| embedding.contract == *contract)
                {
                    self.store
                        .upsert_embedding(
                            principal,
                            workspace,
                            surface,
                            candidate,
                            embedding,
                            chrono::Utc::now().timestamp_millis(),
                        )
                        .await?;
                }
            }
        }
        Ok(contract)
    }

    /// Retry outcomes whose semantic embedding was unavailable on the request
    /// path. The bounded private snapshot remains durable until both the vector and
    /// outcome contract binding have committed.
    pub async fn repair_pending_embedding_binds(&self, limit: usize) -> Result<usize> {
        let (repaired, _repaired_scopes) = self
            .repair_pending_embedding_binds_with_scopes(limit)
            .await?;
        Ok(repaired)
    }

    /// Internal repair report used by the historical worker to refresh only
    /// cohorts in scopes whose durable evidence actually changed. A single
    /// repaired bind must not trigger an expensive rescore of every known
    /// principal/workspace.
    pub async fn repair_pending_embedding_binds_with_scopes(
        &self,
        limit: usize,
    ) -> Result<(usize, BTreeSet<(String, String)>)> {
        if !self.config.enabled || limit == 0 {
            return Ok((0, BTreeSet::new()));
        }
        let lease_owner = format!("attention-embedding-bind:{}", uuid::Uuid::new_v4());
        let work = self
            .store
            .claim_pending_embedding_binds(limit, &lease_owner, 300)
            .await?;
        let mut grouped = HashMap::<
            (String, String, AttentionSurface),
            Vec<store::PendingAttentionEmbeddingBind>,
        >::new();
        for item in work {
            grouped
                .entry((item.principal.clone(), item.workspace.clone(), item.surface))
                .or_default()
                .push(item);
        }

        let mut repaired = 0usize;
        let mut repaired_scopes = BTreeSet::new();
        for ((principal, workspace, surface), items) in grouped {
            let mut candidates = items
                .iter()
                .map(|item| SemanticAttentionCandidate {
                    candidate_id: item.candidate_id.clone(),
                    source_revision: item.source_revision.clone(),
                    semantic_text: item.semantic_text.clone(),
                    existing_embedding: None,
                    actionability_features: None,
                    grouping_features: None,
                })
                .collect::<Vec<_>>();
            candidates = dedupe_candidates(candidates, items.len().max(1));
            let contract = match self
                .resolve_embeddings(
                    &principal,
                    &workspace,
                    surface,
                    &mut candidates,
                    self.embedding_budget(true),
                )
                .await
            {
                Ok(Some(contract)) => contract,
                Ok(None) => {
                    for item in &items {
                        self.store
                            .fail_claimed_embedding_bind(
                                &item.outcome_id,
                                &lease_owner,
                                "embedding_unavailable",
                            )
                            .await?;
                    }
                    continue;
                },
                Err(error) => {
                    tracing::warn!(
                        principal = %principal,
                        workspace = %workspace,
                        surface = surface.as_str(),
                        error = %error,
                        "attention embedding bind batch will retry"
                    );
                    for item in &items {
                        self.store
                            .fail_claimed_embedding_bind(
                                &item.outcome_id,
                                &lease_owner,
                                "embedding_resolution_failed",
                            )
                            .await?;
                    }
                    continue;
                },
            };
            for item in items {
                let is_boundable = candidates.iter().any(|candidate| {
                    candidate.candidate_id == item.candidate_id
                        && candidate.source_revision == item.source_revision
                        && candidate
                            .existing_embedding
                            .as_ref()
                            .is_some_and(|embedding| embedding.contract == contract)
                });
                if !is_boundable {
                    self.store
                        .fail_claimed_embedding_bind(
                            &item.outcome_id,
                            &lease_owner,
                            "embedding_missing_from_batch",
                        )
                        .await?;
                    continue;
                }
                if let Err(error) = self
                    .store
                    .bind_outcome_embedding(
                        &principal,
                        &workspace,
                        &item.outcome_id,
                        &item.candidate_id,
                        item.source_revision.as_deref(),
                        &contract,
                    )
                    .await
                {
                    tracing::warn!(
                        outcome_id = %item.outcome_id,
                        error = %error,
                        "attention embedding bind will retry"
                    );
                    self.store
                        .fail_claimed_embedding_bind(
                            &item.outcome_id,
                            &lease_owner,
                            "embedding_bind_failed",
                        )
                        .await?;
                    continue;
                }
                if self
                    .store
                    .complete_claimed_embedding_bind(&item.outcome_id, &lease_owner)
                    .await?
                {
                    repaired += 1;
                    repaired_scopes.insert((principal.clone(), workspace.clone()));
                }
            }
        }
        Ok((repaired, repaired_scopes))
    }

    /// Periodically bring newly active candidates onto the same persisted
    /// Slice-1 posterior without manufacturing an outcome. This is the steady-
    /// state counterpart of feedback-triggered propagation: cached embeddings
    /// are reused, labels remain explicit owner evidence, and no rank job or
    /// bandit update is created.
    pub async fn refresh_active_scores(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
        active_candidates: Vec<SemanticAttentionCandidate>,
    ) -> Result<usize> {
        if !self.config.enabled {
            return Ok(0);
        }
        let mut candidates = dedupe_candidates(active_candidates, self.rescore_limit());
        if candidates.is_empty() {
            return Ok(0);
        }
        let Some(contract) = self
            .resolve_embeddings(
                principal,
                workspace,
                surface,
                &mut candidates,
                self.embedding_budget(true),
            )
            .await?
        else {
            return Ok(0);
        };
        let labels = self
            .store
            .list_strong_labels(principal, workspace, &contract)
            .await?;
        if labels.is_empty() {
            return Ok(0);
        }
        let evaluator = BayesianKnnEvaluator::new(&self.config);
        let now = chrono::Utc::now().timestamp_millis();
        let mut affected = 0usize;
        let mut pending_scores = Vec::new();
        for candidate in &candidates {
            let Some(embedding) = candidate
                .existing_embedding
                .as_ref()
                .filter(|embedding| embedding.contract == contract)
            else {
                continue;
            };
            let estimate = evaluator.estimate(&embedding.vector, &labels);
            if estimate.surface_score(surface).is_some() {
                affected += 1;
            }
            pending_scores.push((candidate.clone(), estimate));
        }
        let score_changed = self
            .store
            .upsert_scores(
                principal,
                workspace,
                surface,
                pending_scores,
                &contract,
                now,
            )
            .await?;
        if score_changed {
            self.store
                .advance_rank_generation(principal, workspace, surface, now)
                .await?;
        }
        Ok(affected)
    }

    /// Rank the complete eligible universe supplied by the caller. Pagination
    /// must happen only after this method; ranking an already-sliced page would
    /// invalidate top-k evaluation.
    pub async fn rank_eligible_universe(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
        baseline_candidates: &[SemanticAttentionCandidate],
    ) -> Result<(u64, Vec<AttentionRankMetadata>)> {
        let baseline_ids: Vec<String> = baseline_candidates
            .iter()
            .map(|candidate| candidate.candidate_id.clone())
            .collect();
        let candidate_revisions: Vec<(String, Option<String>)> = baseline_candidates
            .iter()
            .map(|candidate| {
                (
                    candidate.candidate_id.clone(),
                    candidate.source_revision.clone(),
                )
            })
            .collect();
        let generation = self
            .store
            .rank_generation(principal, workspace, surface)
            .await?;
        let posteriors = self
            .store
            .list_score_posteriors(principal, workspace, surface, &candidate_revisions)
            .await?;
        let scores: HashMap<String, Option<f64>> = posteriors
            .iter()
            .map(|(id, posterior)| (id.clone(), posterior.surface_score))
            .collect();
        // Unscored candidates sit at the registered Bayesian prior. Treating
        // every `Some(score)` as better than `None` would lift negatively
        // labelled candidates above the unembedded baseline cohort.
        let prior = self.config.prior_alpha / (self.config.prior_alpha + self.config.prior_beta);
        let resolved = self.resolve_actionability(principal, workspace).await?;
        let actionability_mode = resolved.mode;
        let actionability_snapshot = resolved.snapshot;
        let mut actionability_scores = HashMap::<String, ActionabilityInference>::new();
        if let Some(snapshot) = actionability_snapshot {
            let snapshot_id = snapshot.snapshot_id.as_str();
            let mut actionability_cache_inputs = Vec::new();
            for candidate in baseline_candidates {
                let Some(input) = candidate.actionability_features.as_ref() else {
                    continue;
                };
                if let Some(input_digest) = actionability_input_digest(
                    &snapshot,
                    candidate.source_revision.as_deref(),
                    input,
                )? {
                    actionability_cache_inputs.push((
                        candidate.candidate_id.clone(),
                        candidate.source_revision.clone(),
                        input_digest,
                    ));
                }
            }
            actionability_scores = self
                .store
                .list_actionability_scores(
                    principal,
                    workspace,
                    surface,
                    snapshot_id,
                    &actionability_cache_inputs,
                )
                .await?
                .into_iter()
                .map(|(candidate_id, score)| (candidate_id, score.inference))
                .collect();
            let mut pending_actionability = Vec::new();
            for candidate in baseline_candidates {
                if actionability_scores.contains_key(&candidate.candidate_id) {
                    continue;
                }
                let Some(input) = candidate.actionability_features.as_ref() else {
                    continue;
                };
                if let Some(inference) =
                    infer_actionability(&snapshot, candidate.source_revision.as_deref(), input)?
                {
                    pending_actionability.push((
                        candidate.candidate_id.clone(),
                        candidate.source_revision.clone(),
                        inference.clone(),
                    ));
                    actionability_scores.insert(candidate.candidate_id.clone(), inference);
                }
            }
            self.store
                .upsert_actionability_scores(
                    principal,
                    workspace,
                    surface,
                    pending_actionability,
                    chrono::Utc::now().timestamp_millis(),
                )
                .await?;
        }
        let mut learned: Vec<(usize, &String, f64)> = baseline_ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let slice1_score = scores.get(id).copied().flatten().unwrap_or(prior);
                let served_score = if actionability_mode == AttentionActionabilityMode::Enforced {
                    actionability_scores
                        .get(id)
                        .map(|score| score.probability)
                        .unwrap_or(slice1_score)
                } else {
                    slice1_score
                };
                (index + 1, id, served_score)
            })
            .collect();
        learned.sort_by(|left, right| {
            right
                .2
                .total_cmp(&left.2)
                .then_with(|| left.0.cmp(&right.0))
        });
        let learned_by_id: HashMap<&str, usize> = learned
            .iter()
            .enumerate()
            .map(|(index, (_, id, _))| (id.as_str(), index + 1))
            .collect();
        let metadata = baseline_ids
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let baseline_rank = index + 1;
                let learned_rank = learned_by_id
                    .get(id.as_str())
                    .copied()
                    .unwrap_or(baseline_rank);
                AttentionRankMetadata {
                    candidate_id: id.clone(),
                    baseline_rank,
                    learned_rank,
                    rank_delta: baseline_rank as i64 - learned_rank as i64,
                    learning_score: scores.get(id).copied().flatten(),
                    slice1_actionability_probability: posteriors
                        .get(id)
                        .and_then(|posterior| posterior.actionability_probability)
                        .filter(|_| {
                            posteriors
                                .get(id)
                                .is_some_and(|posterior| posterior.actionability_weight > 0.0)
                        }),
                    slice1_actionability_weight: posteriors.get(id).and_then(|posterior| {
                        (posterior.actionability_weight > 0.0)
                            .then_some(posterior.actionability_weight)
                    }),
                    actionability_probability: actionability_scores
                        .get(id)
                        .map(|score| score.probability),
                    actionability_explanation: actionability_scores
                        .get(id)
                        .map(|score| score.explanation.clone()),
                    actionability_model_version: actionability_scores
                        .get(id)
                        .map(|score| score.model_version.clone()),
                    actionability_snapshot_id: actionability_scores
                        .get(id)
                        .map(|score| score.snapshot_id.clone()),
                    semantic_feature_status: baseline_candidates
                        .get(index)
                        .and_then(|candidate| candidate.actionability_features.as_ref())
                        .and_then(|input| input.semantic.as_ref())
                        .map(|semantic| semantic.status)
                        .unwrap_or(SemanticExtractionStatus::Missing),
                    actionability_score_status: if actionability_mode
                        == AttentionActionabilityMode::Disabled
                    {
                        ActionabilityScoreStatus::Disabled
                    } else if actionability_scores.contains_key(id) {
                        ActionabilityScoreStatus::Scored
                    } else {
                        ActionabilityScoreStatus::Fallback
                    },
                    actionability_mode,
                }
            })
            .collect();
        Ok((generation, metadata))
    }

    /// Rank and actionability-score the complete cross-lane union once while
    /// resolving historical evidence through each item's source-owned key.
    /// Returned metadata is always keyed by the origin-qualified canonical id.
    pub async fn rank_canonical_eligible_universe(
        &self,
        principal: &str,
        workspace: &str,
        candidates: &[CanonicalAttentionRankCandidate<'_>],
    ) -> Result<(u64, Vec<AttentionRankMetadata>)> {
        let mut seen = std::collections::HashSet::new();
        anyhow::ensure!(
            candidates
                .iter()
                .all(|candidate| seen.insert(candidate.candidate.candidate_id.as_str())),
            "canonical rank universe contains duplicate projection ids"
        );
        let mut revisions_by_surface =
            HashMap::<AttentionSurface, Vec<(String, Option<String>)>>::new();
        for candidate in candidates {
            revisions_by_surface
                .entry(candidate.evidence_surface)
                .or_default()
                .push((
                    candidate.evidence_candidate_id.to_string(),
                    candidate.candidate.source_revision.clone(),
                ));
        }
        let mut score_by_canonical_id = HashMap::<String, Option<f64>>::new();
        let mut knn_by_canonical_id = HashMap::<String, AttentionScorePosterior>::new();
        let mut generation = 0_u64;
        for surface in [AttentionSurface::FollowUp, AttentionSurface::WorthALook] {
            generation = generation.wrapping_add(
                self.store
                    .rank_generation(principal, workspace, surface)
                    .await?,
            );
            let Some(revisions) = revisions_by_surface.get(&surface) else {
                continue;
            };
            let scores = self
                .store
                .list_score_posteriors(principal, workspace, surface, revisions)
                .await?;
            for candidate in candidates
                .iter()
                .filter(|candidate| candidate.evidence_surface == surface)
            {
                if let Some(posterior) = scores.get(candidate.evidence_candidate_id) {
                    knn_by_canonical_id
                        .insert(candidate.candidate.candidate_id.clone(), *posterior);
                }
                score_by_canonical_id.insert(
                    candidate.candidate.candidate_id.clone(),
                    scores
                        .get(candidate.evidence_candidate_id)
                        .and_then(|posterior| posterior.surface_score),
                );
            }
        }

        let resolved = self.resolve_actionability(principal, workspace).await?;
        let actionability_mode = resolved.mode;
        let actionability_snapshot = resolved.snapshot;
        let mut actionability_by_canonical_id = HashMap::<String, ActionabilityInference>::new();
        if let Some(snapshot) = actionability_snapshot.as_ref() {
            let snapshot_id = snapshot.snapshot_id.as_str();
            for surface in [AttentionSurface::FollowUp, AttentionSurface::WorthALook] {
                let scoped: Vec<_> = candidates
                    .iter()
                    .filter(|candidate| candidate.evidence_surface == surface)
                    .collect();
                let mut cache_inputs = Vec::new();
                for candidate in &scoped {
                    let Some(input) = candidate.candidate.actionability_features.as_ref() else {
                        continue;
                    };
                    if let Some(input_digest) = actionability_input_digest(
                        snapshot,
                        candidate.candidate.source_revision.as_deref(),
                        input,
                    )? {
                        cache_inputs.push((
                            candidate.evidence_candidate_id.to_string(),
                            candidate.candidate.source_revision.clone(),
                            input_digest,
                        ));
                    }
                }
                let cached = self
                    .store
                    .list_actionability_scores(
                        principal,
                        workspace,
                        surface,
                        snapshot_id,
                        &cache_inputs,
                    )
                    .await?;
                let mut pending_actionability = Vec::new();
                for candidate in scoped {
                    if let Some(score) = cached.get(candidate.evidence_candidate_id) {
                        actionability_by_canonical_id.insert(
                            candidate.candidate.candidate_id.clone(),
                            score.inference.clone(),
                        );
                        continue;
                    }
                    let Some(input) = candidate.candidate.actionability_features.as_ref() else {
                        continue;
                    };
                    let Some(inference) = infer_actionability(
                        snapshot,
                        candidate.candidate.source_revision.as_deref(),
                        input,
                    )?
                    else {
                        continue;
                    };
                    pending_actionability.push((
                        candidate.evidence_candidate_id.to_string(),
                        candidate.candidate.source_revision.clone(),
                        inference.clone(),
                    ));
                    actionability_by_canonical_id
                        .insert(candidate.candidate.candidate_id.clone(), inference);
                }
                self.store
                    .upsert_actionability_scores(
                        principal,
                        workspace,
                        surface,
                        pending_actionability,
                        chrono::Utc::now().timestamp_millis(),
                    )
                    .await?;
            }
        }

        let prior = self.config.prior_alpha / (self.config.prior_alpha + self.config.prior_beta);
        let mut learned: Vec<(usize, &str, f64)> = candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| {
                let id = candidate.candidate.candidate_id.as_str();
                let slice1_score = score_by_canonical_id
                    .get(id)
                    .copied()
                    .flatten()
                    .unwrap_or(prior);
                let served_score = if actionability_mode == AttentionActionabilityMode::Enforced {
                    actionability_by_canonical_id
                        .get(id)
                        .map(|score| score.probability)
                        .unwrap_or(slice1_score)
                } else {
                    slice1_score
                };
                (index + 1, id, served_score)
            })
            .collect();
        learned.sort_by(|left, right| {
            right
                .2
                .total_cmp(&left.2)
                .then_with(|| left.0.cmp(&right.0))
        });
        let learned_by_id: HashMap<&str, usize> = learned
            .iter()
            .enumerate()
            .map(|(index, (_, id, _))| (*id, index + 1))
            .collect();
        let metadata = candidates
            .iter()
            .enumerate()
            .map(|(index, candidate)| {
                let id = candidate.candidate.candidate_id.as_str();
                let baseline_rank = index + 1;
                let learned_rank = learned_by_id.get(id).copied().unwrap_or(baseline_rank);
                let actionability = actionability_by_canonical_id.get(id);
                AttentionRankMetadata {
                    candidate_id: id.to_string(),
                    baseline_rank,
                    learned_rank,
                    rank_delta: baseline_rank as i64 - learned_rank as i64,
                    learning_score: score_by_canonical_id.get(id).copied().flatten(),
                    slice1_actionability_probability: knn_by_canonical_id.get(id).and_then(
                        |posterior| {
                            (posterior.actionability_weight > 0.0)
                                .then_some(posterior.actionability_probability)
                                .flatten()
                        },
                    ),
                    slice1_actionability_weight: knn_by_canonical_id.get(id).and_then(
                        |posterior| {
                            (posterior.actionability_weight > 0.0)
                                .then_some(posterior.actionability_weight)
                        },
                    ),
                    actionability_probability: actionability.map(|score| score.probability),
                    actionability_explanation: actionability.map(|score| score.explanation.clone()),
                    actionability_model_version: actionability
                        .map(|score| score.model_version.clone()),
                    actionability_snapshot_id: actionability.map(|score| score.snapshot_id.clone()),
                    semantic_feature_status: candidate
                        .candidate
                        .actionability_features
                        .as_ref()
                        .and_then(|input| input.semantic.as_ref())
                        .map(|semantic| semantic.status)
                        .unwrap_or(SemanticExtractionStatus::Missing),
                    actionability_score_status: if actionability_mode
                        == AttentionActionabilityMode::Disabled
                    {
                        ActionabilityScoreStatus::Disabled
                    } else if actionability.is_some() {
                        ActionabilityScoreStatus::Scored
                    } else {
                        ActionabilityScoreStatus::Fallback
                    },
                    actionability_mode,
                }
            })
            .collect();
        Ok((generation, metadata))
    }

    pub async fn health(
        &self,
        principal: &str,
        workspace: &str,
        surface: AttentionSurface,
        total_active: usize,
        active_cohort_ids: &[String],
        source_family_counts: std::collections::BTreeMap<String, u64>,
        ranks: &[AttentionRankMetadata],
    ) -> Result<AttentionLearningHealth> {
        let embedded_candidates = self
            .store
            .count_embedded_candidates(principal, workspace, surface, active_cohort_ids)
            .await?;
        let cohort_size = active_cohort_ids.len();
        let resolved = self.resolve_actionability(principal, workspace).await?;
        Ok(AttentionLearningHealth {
            total_active,
            source_family_counts,
            embedded_candidates,
            embedding_coverage: if cohort_size == 0 {
                0.0
            } else {
                embedded_candidates as f64 / cohort_size as f64
            },
            learned_rank_changes: ranks
                .iter()
                .filter(|rank| rank.baseline_rank != rank.learned_rank)
                .count(),
            coverage_scope: "active_cohort".to_string(),
            rank_policy_version: ATTENTION_RANK_POLICY_VERSION.to_string(),
            semantic_ranking_enabled: self.config.semantic_ranking_enabled
                || ranks.first().is_some_and(|rank| {
                    rank.actionability_mode == AttentionActionabilityMode::Enforced
                }),
            actionability_mode: ranks
                .first()
                .map(|rank| rank.actionability_mode)
                .unwrap_or(resolved.mode),
            actionability_snapshot_id: ranks
                .first()
                .and_then(|rank| rank.actionability_snapshot_id.clone())
                .or(resolved.snapshot.map(|snapshot| snapshot.snapshot_id)),
            semantic_extraction_coverage: if cohort_size == 0 {
                0.0
            } else {
                ranks
                    .iter()
                    .filter(|rank| {
                        rank.semantic_feature_status == SemanticExtractionStatus::Succeeded
                    })
                    .count() as f64
                    / cohort_size as f64
            },
            actionability_scored_count: ranks
                .iter()
                .filter(|rank| rank.actionability_score_status == ActionabilityScoreStatus::Scored)
                .count(),
            actionability_fallback_count: ranks
                .iter()
                .filter(|rank| {
                    rank.actionability_score_status == ActionabilityScoreStatus::Fallback
                })
                .count(),
        })
    }
}

fn receipt(
    outcome: &PersistedAttentionOutcome,
    surface: AttentionSurface,
    affected_candidates: usize,
    rescore_status: AttentionRescoreStatus,
    embedding_contract: Option<String>,
    posterior_update: Option<AttentionPosteriorUpdateReceipt>,
    rank_recompute: Option<AttentionRankRecomputeReference>,
) -> AttentionFeedbackReceipt {
    AttentionFeedbackReceipt {
        outcome_id: outcome.outcome_id.clone(),
        outcome: outcome.outcome,
        surface,
        // `false` would imply data loss to clients. An idempotent replay still
        // points at the same durable outcome and is therefore recorded.
        feedback_recorded: true,
        affected_candidates,
        rescore_status,
        embedding_contract,
        // The existing funnel observability endpoint has no outcome-id filter;
        // do not advertise a link that silently ignores its target.
        diagnostic_href: None,
        posterior_update,
        rank_recompute,
    }
}

/// Identity of a candidate set.
///
/// Order-independent, because a universe is a set and the order it arrives in
/// is an artefact of how it was queried and scored. Hashing the slice order
/// meant re-serving the same candidates in a different sequence minted a new
/// identity — the same class of bug as hashing the rank generation, and it
/// survived that fix: 60 consecutive decisions over 13 real universes still
/// produced 48 distinct digests until this was sorted too.
pub fn rank_universe_digest(candidates: &[SemanticAttentionCandidate]) -> String {
    let mut records: Vec<(&str, &str)> = candidates
        .iter()
        .map(|candidate| {
            (
                candidate.candidate_id.as_str(),
                candidate.source_revision.as_deref().unwrap_or_default(),
            )
        })
        .collect();
    records.sort_unstable();
    let mut digest = blake3::Hasher::new();
    for (candidate_id, source_revision) in records {
        digest.update(candidate_id.as_bytes());
        digest.update(b"\x1e");
        digest.update(source_revision.as_bytes());
        digest.update(b"\x1f");
    }
    digest.finalize().to_hex().to_string()
}

/// Identity of the universe a decision was served from.
///
/// Content only. This deliberately excludes the rank generation, which is a
/// counter incremented on every recomputation: mixing it in made two decisions
/// over a byte-identical universe report different identities, so anything
/// comparing "the universe I was served from" against "the universe now" saw a
/// change that had not happened. Measured on the live store, 3,255 stored
/// digests covered only 1,934 genuinely distinct universes — 41% of all digest
/// changes were pure counter drift, and rank recompute, whose commit guard is
/// exactly that comparison, had never once succeeded in 3,977 attempts.
///
/// Representatives are sorted so the digest states its own order-independence
/// rather than inheriting it from how clusters happen to be built.
pub fn canonical_universe_digest(rank_digest: &str, representative_ids: &[String]) -> String {
    let mut representatives: Vec<&str> = representative_ids.iter().map(String::as_str).collect();
    representatives.sort_unstable();
    blake3::hash(format!("{rank_digest}:{}", representatives.join(",")).as_bytes())
        .to_hex()
        .to_string()
}

fn semantic_content_digest(semantic_text: &str) -> String {
    blake3::hash(semantic_text.trim().as_bytes())
        .to_hex()
        .to_string()
}

fn dedupe_candidates(
    candidates: Vec<SemanticAttentionCandidate>,
    limit: usize,
) -> Vec<SemanticAttentionCandidate> {
    let mut seen = std::collections::HashSet::new();
    candidates
        .into_iter()
        .filter(|candidate| seen.insert(candidate.candidate_id.clone()))
        .take(limit)
        .collect()
}

fn validate_config(config: &AttentionLearningConfig) -> Result<()> {
    anyhow::ensure!(
        !config.historical_bootstrap.enabled || config.enabled,
        "attention_learning.enabled must be true when historical_bootstrap is enabled"
    );
    anyhow::ensure!(
        config.rescore_limit > 0,
        "attention_learning.rescore_limit must be positive"
    );
    anyhow::ensure!(
        config.neighbor_count > 0,
        "attention_learning.neighbor_count must be positive"
    );
    anyhow::ensure!(
        config.prior_alpha.is_finite() && config.prior_alpha > 0.0,
        "attention_learning.prior_alpha must be finite and positive"
    );
    anyhow::ensure!(
        config.prior_beta.is_finite() && config.prior_beta > 0.0,
        "attention_learning.prior_beta must be finite and positive"
    );
    anyhow::ensure!(
        config.kernel_bandwidth.is_finite() && config.kernel_bandwidth > 0.0,
        "attention_learning.kernel_bandwidth must be finite and positive"
    );
    anyhow::ensure!(
        config.min_evidence_weight.is_finite() && config.min_evidence_weight >= 0.0,
        "attention_learning.min_evidence_weight must be finite and non-negative"
    );
    anyhow::ensure!(
        (1..=100).contains(&config.semantic_backfill.batch_size),
        "attention_learning.semantic_backfill.batch_size must be within 1..=100"
    );
    anyhow::ensure!(
        (1..=8).contains(&config.semantic_backfill.concurrency),
        "attention_learning.semantic_backfill.concurrency must be within 1..=8"
    );
    anyhow::ensure!((60..=86_400).contains(&config.semantic_backfill.interval_secs), "attention_learning.semantic_backfill.interval_secs must be within 60..=86400 so calls_per_minute remains a hard bound");
    anyhow::ensure!(
        config.semantic_backfill.max_retries <= 20,
        "attention_learning.semantic_backfill.max_retries must be within 0..=20"
    );
    anyhow::ensure!(
        config.semantic_backfill.retry_base_secs > 0
            && config.semantic_backfill.retry_base_secs <= config.semantic_backfill.retry_max_secs,
        "attention_learning.semantic_backfill retry bounds are invalid"
    );
    anyhow::ensure!(
        (30..=3_600).contains(&config.semantic_backfill.lease_secs),
        "attention_learning.semantic_backfill.lease_secs must be within 30..=3600"
    );
    anyhow::ensure!(
        (1..=600).contains(&config.semantic_backfill.calls_per_minute),
        "attention_learning.semantic_backfill.calls_per_minute must be within 1..=600"
    );
    anyhow::ensure!(
        (1..=100).contains(&config.rank_recompute.batch_size),
        "attention_learning.rank_recompute.batch_size must be within 1..=100"
    );
    anyhow::ensure!(
        (1..=8).contains(&config.rank_recompute.concurrency),
        "attention_learning.rank_recompute.concurrency must be within 1..=8"
    );
    anyhow::ensure!(
        (10..=86_400).contains(&config.rank_recompute.interval_secs),
        "attention_learning.rank_recompute.interval_secs must be within 10..=86400"
    );
    anyhow::ensure!(
        config.rank_recompute.max_retries <= 20,
        "attention_learning.rank_recompute.max_retries must be within 0..=20"
    );
    anyhow::ensure!(
        config.rank_recompute.retry_base_secs > 0
            && config.rank_recompute.retry_base_secs <= config.rank_recompute.retry_max_secs,
        "attention_learning.rank_recompute retry bounds are invalid"
    );
    anyhow::ensure!(
        (30..=3_600).contains(&config.rank_recompute.lease_secs),
        "attention_learning.rank_recompute.lease_secs must be within 30..=3600"
    );
    anyhow::ensure!(
        (1..=3_650).contains(&config.rank_recompute.retention_days),
        "attention_learning.rank_recompute.retention_days must be within 1..=3650"
    );
    anyhow::ensure!(
        (1..=100).contains(&config.historical_bootstrap.batch_size),
        "attention_learning.historical_bootstrap.batch_size must be within 1..=100"
    );
    anyhow::ensure!(
        (10..=86_400).contains(&config.historical_bootstrap.interval_secs),
        "attention_learning.historical_bootstrap.interval_secs must be within 10..=86400"
    );
    anyhow::ensure!(
        config.grouping.max_pair_evaluations > 0,
        "attention_learning.grouping.max_pair_evaluations must be positive"
    );
    anyhow::ensure!(
        config.routing.canary_fraction.is_finite()
            && (0.0..=1.0).contains(&config.routing.canary_fraction),
        "attention_learning.routing.canary_fraction must be within 0..=1"
    );
    anyhow::ensure!(
        !config.routing.seed_identity.trim().is_empty()
            && config.routing.seed_identity.chars().count() <= 200,
        "attention_learning.routing.seed_identity must contain 1..=200 characters"
    );
    anyhow::ensure!(
        (1..=ATTENTION_MIN_VISIBLE_MS_MAX).contains(&config.routing.min_visible_ms),
        "attention_learning.routing.min_visible_ms must be within 1..={} milliseconds",
        ATTENTION_MIN_VISIBLE_MS_MAX
    );
    anyhow::ensure!(
        !config.routing.visibility_rule_version.trim().is_empty()
            && config.routing.visibility_rule_version.chars().count()
                <= ATTENTION_VISIBILITY_RULE_MAX_CHARS,
        "attention_learning.routing.visibility_rule_version must contain 1..={} characters",
        ATTENTION_VISIBILITY_RULE_MAX_CHARS
    );
    if config.actionability.mode != AttentionActionabilityMode::Disabled {
        anyhow::ensure!(
            config
                .actionability
                .snapshot_id
                .as_deref()
                .is_some_and(|snapshot_id| !snapshot_id.trim().is_empty()),
            "attention_learning.actionability.snapshot_id is required for shadow or enforced mode"
        );
    }
    if config.grouping.mode != AttentionGroupingMode::Disabled {
        anyhow::ensure!(
            config
                .grouping
                .snapshot_id
                .as_deref()
                .is_some_and(|snapshot_id| !snapshot_id.trim().is_empty()),
            "attention_learning.grouping.snapshot_id is required for shadow or enforced mode"
        );
    }
    if config.routing.mode != AttentionRoutingMode::Baseline {
        anyhow::ensure!(
            config
                .routing
                .snapshot_id
                .as_deref()
                .is_some_and(|snapshot_id| !snapshot_id.trim().is_empty()),
            "attention_learning.routing.snapshot_id is required for shadow or canary mode"
        );
    }
    if config.routing.mode == AttentionRoutingMode::Canary {
        anyhow::ensure!(
            config.routing.canary_fraction > 0.0,
            "attention_learning.routing.canary_fraction must be positive in canary mode"
        );
    }
    anyhow::ensure!(
        config.bandit.canary_fraction.is_finite()
            && (0.0..=1.0).contains(&config.bandit.canary_fraction),
        "attention_learning.bandit.canary_fraction must be within 0..=1"
    );
    anyhow::ensure!(
        config.bandit.delivery_default_page_size > 0
            && config.bandit.delivery_default_page_size <= config.bandit.delivery_max_page_size
            && config.bandit.delivery_max_page_size <= 200,
        "attention_learning.bandit delivery page sizes must satisfy 1 <= default <= max <= 200"
    );
    anyhow::ensure!(
        (60..=86_400).contains(&config.bandit.delivery_ttl_secs),
        "attention_learning.bandit.delivery_ttl_secs must be within 60..=86400"
    );
    anyhow::ensure!(
        (1..=3650).contains(&config.bandit.delivery_retention_days),
        "attention_learning.bandit.delivery_retention_days must be within 1..=3650"
    );
    let bandit_pin = config
        .bandit
        .snapshot_id
        .as_deref()
        .is_some_and(|snapshot_id| !snapshot_id.trim().is_empty());
    if bandit_pin {
        anyhow::ensure!(
            config.bandit.mode != AttentionBanditMode::Disabled,
            "attention_learning.bandit.snapshot_id cannot be set while mode is disabled"
        );
    }
    if config.bandit.mode == AttentionBanditMode::Canary && bandit_pin {
        anyhow::ensure!(
            config.bandit.canary_fraction > 0.0,
            "attention_learning.bandit.canary_fraction must be positive when YAML pins a canary snapshot"
        );
    }
    Ok(())
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn canonical_outcomes_keep_neutral_and_identity_feedback_censored() {
        for outcome in [
            AttentionOutcomeKind::NeutralSeen,
            AttentionOutcomeKind::DuplicateOf,
            AttentionOutcomeKind::TimingNegative,
        ] {
            assert_eq!(outcome.usefulness_target(), None);
            assert_eq!(outcome.actionability_target(), None);
            assert_eq!(outcome.routing_target(), None);
        }
        assert_eq!(AttentionOutcomeKind::Useful.routing_target(), None);
        assert_eq!(
            AttentionOutcomeKind::ActionCompleted.routing_target(),
            Some(AttentionRoute::FollowUp)
        );
        assert_eq!(
            AttentionOutcomeKind::NotActionable.routing_target(),
            Some(AttentionRoute::WorthALook)
        );
        assert_eq!(AttentionOutcomeKind::Irrelevant.routing_target(), None);
    }

    #[test]
    fn task_specific_targets_do_not_conflate_useful_with_actionable() {
        assert_eq!(AttentionOutcomeKind::Useful.usefulness_target(), Some(true));
        assert_eq!(AttentionOutcomeKind::Useful.actionability_target(), None);
        assert_eq!(
            AttentionOutcomeKind::NotActionable.usefulness_target(),
            None
        );
        assert_eq!(
            AttentionOutcomeKind::NotActionable.actionability_target(),
            Some(false)
        );
    }

    #[test]
    fn a_universe_is_a_set_so_candidate_order_is_not_part_of_its_identity() {
        // The order candidates arrive in is an artefact of how they were
        // queried and scored, and it drifts between servings. Hashing it made
        // the same universe report a new identity each time -- which is what
        // kept 60 consecutive decisions over 13 real universes reporting 48
        // distinct digests even after the generation counter was removed.
        let one = semantic_candidate("a", Some("distill:1"));
        let two = semantic_candidate("b", Some("distill:2"));
        assert_eq!(
            rank_universe_digest(&[one.clone(), two.clone()]),
            rank_universe_digest(&[two.clone(), one.clone()]),
        );
        // A revision change is a genuinely different universe.
        assert_ne!(
            rank_universe_digest(&[one.clone(), two.clone()]),
            rank_universe_digest(&[one, semantic_candidate("b", Some("distill:3"))]),
        );
    }

    fn semantic_candidate(id: &str, revision: Option<&str>) -> SemanticAttentionCandidate {
        SemanticAttentionCandidate {
            candidate_id: id.to_string(),
            source_revision: revision.map(str::to_string),
            semantic_text: String::new(),
            existing_embedding: None,
            actionability_features: None,
            grouping_features: None,
        }
    }

    #[test]
    fn the_universe_digest_is_identity_not_a_cache_buster() {
        let representatives = vec!["b".to_string(), "a".to_string()];

        // Same universe, same identity. This is the property the whole
        // served-vs-current comparison rests on, and it did not hold: the rank
        // generation was hashed in, so re-serving an unchanged universe minted
        // a fresh digest and every commit guard read it as a change.
        assert_eq!(
            canonical_universe_digest("rank-1", &representatives),
            canonical_universe_digest("rank-1", &representatives),
        );

        // Order of representatives is not part of the universe's identity.
        assert_eq!(
            canonical_universe_digest("rank-1", &representatives),
            canonical_universe_digest("rank-1", &["a".to_string(), "b".to_string()]),
        );

        // A different universe must still be a different identity, or the
        // guard this enables would be worthless in the other direction.
        assert_ne!(
            canonical_universe_digest("rank-1", &representatives),
            canonical_universe_digest("rank-2", &representatives),
        );
        assert_ne!(
            canonical_universe_digest("rank-1", &representatives),
            canonical_universe_digest("rank-1", &["a".to_string()]),
        );
    }

    #[test]
    fn background_embedding_passes_do_not_inherit_the_request_latency_budget() {
        // The bind-repair queue once sat at a fixed size for days: every pass
        // spent a 5s request budget on a cold local model that needs longer
        // just to load, so no background pass could ever succeed. A background
        // pass has no owner waiting on it and must not be bounded by what one
        // would wait for.
        let config = AttentionLearningConfig::default();
        let service = AttentionLearningService::open_in_temp(config.clone());
        assert_eq!(
            service.embedding_budget(false),
            Duration::from_millis(config.embedding_timeout_ms)
        );
        assert_eq!(
            service.embedding_budget(true),
            Duration::from_millis(config.background_embedding_timeout_ms)
        );
        assert!(
            service.embedding_budget(true) > service.embedding_budget(false),
            "a background pass must be allowed to outlast a request"
        );
    }

    #[tokio::test]
    async fn historical_label_refreshes_new_candidates_without_rank_jobs() {
        let mut config = AttentionLearningConfig::default();
        config.enabled = true;
        config.semantic_ranking_enabled = true;
        let service = AttentionLearningService::open_in_temp(config);
        let embedding_contract = service.active_embedding_contract().unwrap();
        let exemplar = SemanticAttentionCandidate {
            candidate_id: "legacy-exemplar".to_string(),
            source_revision: None,
            semantic_text: String::new(),
            existing_embedding: Some(SemanticEmbedding {
                contract: embedding_contract.clone(),
                vector: vec![1.0, 0.0],
            }),
            actionability_features: None,
            grouping_features: None,
        };
        let receipt = service
            .record_historical_and_propagate(
                "owner",
                "default",
                AttentionSurface::FollowUp,
                RecordAttentionOutcome {
                    event_id: "legacy-event".to_string(),
                    candidate: exemplar,
                    outcome: AttentionOutcomeKind::ActionCompleted,
                    reason: None,
                    label_quality: AttentionLabelQuality::Strong,
                    occurred_at: 1,
                    attribution: None,
                },
                Vec::new(),
            )
            .await
            .unwrap();
        assert!(receipt.rank_recompute.is_none());
        assert_eq!(
            service
                .store()
                .rank_recompute_queue_counts("owner", "default")
                .await
                .unwrap(),
            AttentionRankRecomputeQueueCounts::default()
        );

        let refreshed = service
            .refresh_active_scores(
                "owner",
                "default",
                AttentionSurface::FollowUp,
                vec![SemanticAttentionCandidate {
                    candidate_id: "new-active".to_string(),
                    source_revision: Some("distill:1".to_string()),
                    semantic_text: String::new(),
                    existing_embedding: Some(SemanticEmbedding {
                        contract: embedding_contract,
                        vector: vec![0.99, 0.01],
                    }),
                    actionability_features: None,
                    grouping_features: None,
                }],
            )
            .await
            .unwrap();
        assert_eq!(refreshed, 1);
    }

    #[tokio::test]
    async fn staged_historical_batch_advances_active_generation_once() {
        let mut config = AttentionLearningConfig::default();
        config.enabled = true;
        config.semantic_ranking_enabled = true;
        let service = AttentionLearningService::open_in_temp(config);
        let contract = service.active_embedding_contract().unwrap();

        for (index, outcome, vector) in [
            (0, AttentionOutcomeKind::Useful, vec![1.0, 0.0]),
            (1, AttentionOutcomeKind::Irrelevant, vec![0.0, 1.0]),
            (2, AttentionOutcomeKind::Useful, vec![0.9, 0.1]),
        ] {
            let receipt = service
                .stage_historical_outcome(
                    "owner",
                    "default",
                    AttentionSurface::FollowUp,
                    RecordAttentionOutcome {
                        event_id: format!("historical-event-{index}"),
                        candidate: SemanticAttentionCandidate {
                            candidate_id: format!("historical-candidate-{index}"),
                            source_revision: None,
                            semantic_text: String::new(),
                            existing_embedding: Some(SemanticEmbedding {
                                contract: contract.clone(),
                                vector,
                            }),
                            actionability_features: None,
                            grouping_features: None,
                        },
                        outcome,
                        reason: None,
                        label_quality: AttentionLabelQuality::Strong,
                        occurred_at: index + 1,
                        attribution: None,
                    },
                )
                .await
                .unwrap();
            assert_eq!(receipt.affected_candidates, 0);
            assert!(receipt.rank_recompute.is_none());
        }

        assert_eq!(
            service
                .store()
                .rank_generation("owner", "default", AttentionSurface::FollowUp)
                .await
                .unwrap(),
            0,
            "staging labels must not invalidate projections once per label"
        );

        let active = vec![
            SemanticAttentionCandidate {
                candidate_id: "active-one".to_string(),
                source_revision: Some("revision-1".to_string()),
                semantic_text: String::new(),
                existing_embedding: Some(SemanticEmbedding {
                    contract: contract.clone(),
                    vector: vec![0.95, 0.05],
                }),
                actionability_features: None,
                grouping_features: None,
            },
            SemanticAttentionCandidate {
                candidate_id: "active-two".to_string(),
                source_revision: Some("revision-1".to_string()),
                semantic_text: String::new(),
                existing_embedding: Some(SemanticEmbedding {
                    contract,
                    vector: vec![0.05, 0.95],
                }),
                actionability_features: None,
                grouping_features: None,
            },
        ];
        let refreshed = service
            .refresh_active_scores(
                "owner",
                "default",
                AttentionSurface::FollowUp,
                active.clone(),
            )
            .await
            .unwrap();
        assert!(
            (1..=active.len()).contains(&refreshed),
            "the batch must publish at least one bounded active score"
        );
        assert_eq!(
            service
                .store()
                .rank_generation("owner", "default", AttentionSurface::FollowUp)
                .await
                .unwrap(),
            1,
            "one completed batch must publish one generation"
        );

        service
            .refresh_active_scores("owner", "default", AttentionSurface::FollowUp, active)
            .await
            .unwrap();
        assert_eq!(
            service
                .store()
                .rank_generation("owner", "default", AttentionSurface::FollowUp)
                .await
                .unwrap(),
            1,
            "an unchanged cohort refresh must not churn the generation"
        );
    }

    #[tokio::test]
    async fn canonical_learning_identity_uses_all_four_generation_counters() {
        let mut config = AttentionLearningConfig::default();
        config.enabled = true;
        let service = AttentionLearningService::open_in_temp(config);
        let store = service.store();
        store
            .advance_rank_generation("owner", "default", AttentionSurface::FollowUp, 1)
            .await
            .unwrap();
        for updated_at in 2..=3 {
            store
                .advance_rank_generation(
                    "owner",
                    "default",
                    AttentionSurface::WorthALook,
                    updated_at,
                )
                .await
                .unwrap();
        }
        for updated_at in 4..=6 {
            store
                .advance_grouping_generation(
                    "owner",
                    "default",
                    AttentionSurface::FollowUp,
                    updated_at,
                )
                .await
                .unwrap();
        }
        for updated_at in 7..=10 {
            store
                .advance_grouping_generation(
                    "owner",
                    "default",
                    AttentionSurface::WorthALook,
                    updated_at,
                )
                .await
                .unwrap();
        }

        assert_eq!(
            service
                .canonical_projection_learning_identity("owner", "default")
                .await
                .unwrap(),
            "follow-rank:1:worth-rank:2:follow-group:3:worth-group:4"
        );
    }

    #[tokio::test]
    async fn obsolete_supplied_embedding_contract_is_queued_for_current_contract_repair() {
        let mut config = AttentionLearningConfig::default();
        config.enabled = true;
        let service = AttentionLearningService::open_in_temp(config);
        let current_contract = service.active_embedding_contract().unwrap();
        let candidate = SemanticAttentionCandidate {
            candidate_id: "stale-candidate".to_string(),
            source_revision: Some("revision-1".to_string()),
            semantic_text: "bounded private brief".to_string(),
            existing_embedding: Some(SemanticEmbedding {
                contract: format!("obsolete:{current_contract}"),
                vector: vec![1.0, 0.0],
            }),
            actionability_features: None,
            grouping_features: None,
        };
        let receipt = service
            .record_historical_and_propagate(
                "owner",
                "default",
                AttentionSurface::WorthALook,
                RecordAttentionOutcome {
                    event_id: "stale-contract-event".to_string(),
                    candidate,
                    outcome: AttentionOutcomeKind::Useful,
                    reason: None,
                    label_quality: AttentionLabelQuality::Strong,
                    occurred_at: 1,
                    attribution: None,
                },
                Vec::new(),
            )
            .await
            .unwrap();

        assert!(!service
            .store()
            .outcome_has_embedding("owner", "default", &receipt.outcome_id)
            .await
            .unwrap());
        let work = service
            .store()
            .list_pending_embedding_binds(10)
            .await
            .unwrap();
        assert_eq!(work.len(), 1);
        assert_eq!(work[0].outcome_id, receipt.outcome_id);
    }

    #[test]
    fn impression_policy_dwell_must_fit_the_documented_window() {
        let mut config = AttentionLearningConfig::default();
        config.routing.min_visible_ms = 0;
        assert!(validate_config(&config).is_err());
        config.routing.min_visible_ms = ATTENTION_MIN_VISIBLE_MS_MAX + 1;
        assert!(validate_config(&config).is_err());
        config.routing.min_visible_ms = ATTENTION_MIN_VISIBLE_MS_MAX;
        assert!(validate_config(&config).is_ok());
    }

    #[tokio::test]
    async fn enforced_mode_without_the_pinned_snapshot_preserves_baseline_order() {
        let mut config = AttentionLearningConfig::default();
        config.actionability.mode = AttentionActionabilityMode::Enforced;
        config.actionability.snapshot_id = Some("missing-snapshot".to_string());
        let service = AttentionLearningService::open_in_temp(config);
        assert!(!service.serving_semantic_ranking_enabled().await);

        let candidates = vec![
            SemanticAttentionCandidate {
                candidate_id: "baseline-first".to_string(),
                source_revision: Some("distill:1".to_string()),
                semantic_text: String::new(),
                existing_embedding: None,
                actionability_features: None,
                grouping_features: None,
            },
            SemanticAttentionCandidate {
                candidate_id: "baseline-second".to_string(),
                source_revision: Some("distill:2".to_string()),
                semantic_text: String::new(),
                existing_embedding: None,
                actionability_features: None,
                grouping_features: None,
            },
        ];
        let (_, ranks) = service
            .rank_eligible_universe("owner", "default", AttentionSurface::FollowUp, &candidates)
            .await
            .unwrap();
        assert_eq!(ranks[0].candidate_id, "baseline-first");
        assert_eq!(ranks[0].learned_rank, 1);
        assert_eq!(ranks[1].candidate_id, "baseline-second");
        assert_eq!(ranks[1].learned_rank, 2);
        assert!(ranks
            .iter()
            .all(|rank| rank.actionability_mode == AttentionActionabilityMode::Disabled));
    }

    #[tokio::test]
    async fn pair_budget_exceeded_returns_complete_singleton_universe() {
        let mut config = AttentionLearningConfig::default();
        config.grouping.max_pair_evaluations = 1;
        let service = AttentionLearningService::open_in_temp(config);
        let candidates = vec![
            SemanticAttentionCandidate {
                candidate_id: "a".to_string(),
                source_revision: Some("r1".to_string()),
                semantic_text: String::new(),
                existing_embedding: None,
                actionability_features: None,
                grouping_features: Some(GroupingFeatureInput {
                    exact_source_identity: Some("same-source".to_string()),
                    ..Default::default()
                }),
            },
            SemanticAttentionCandidate {
                candidate_id: "b".to_string(),
                source_revision: Some("r1".to_string()),
                semantic_text: String::new(),
                existing_embedding: None,
                actionability_features: None,
                grouping_features: Some(GroupingFeatureInput {
                    exact_source_identity: Some("same-source".to_string()),
                    ..Default::default()
                }),
            },
            SemanticAttentionCandidate {
                candidate_id: "c".to_string(),
                source_revision: Some("r1".to_string()),
                semantic_text: String::new(),
                existing_embedding: None,
                actionability_features: None,
                grouping_features: Some(GroupingFeatureInput {
                    exact_source_identity: Some("same-source".to_string()),
                    ..Default::default()
                }),
            },
        ];
        let (_, ranks) = service
            .rank_eligible_universe("owner", "default", AttentionSurface::FollowUp, &candidates)
            .await
            .unwrap();
        let grouping = service
            .group_eligible_universe(
                "owner",
                "default",
                AttentionSurface::FollowUp,
                &candidates,
                &ranks,
            )
            .await
            .unwrap();
        assert_eq!(grouping.mode, AttentionGroupingMode::Disabled);
        assert_eq!(grouping.projection.clusters.len(), 3);
        assert!(grouping
            .projection
            .clusters
            .iter()
            .all(|cluster| cluster.member_ids.len() == 1));
        assert!(grouping.health.totals_reconcile);
        assert!(grouping.health.budget_exceeded);
        assert_eq!(grouping.health.pair_evaluation_budget, 1);
        assert_eq!(grouping.health.required_pair_evaluations, 3);
    }
}
